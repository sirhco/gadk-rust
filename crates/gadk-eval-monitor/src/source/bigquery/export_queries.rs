// Copyright 2026 Chris Olson
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! [`ExportSource`] for the BigQuery source: the invocations of traces, runs by invocation id,
//! session history, scores and prompt versions, and the invocations whose scores match filters,
//! each read with parameterised queries bounded by `timestamp`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;

use super::{
    BigQueryTranscriptSource, Columns, agent_expr, cell_json, cell_str, cell_timestamp,
    invocation_params, param, row_params, run_headers, string_array_param, timestamp_param,
};
use crate::error::MonitorError;
use crate::export::{ExportContext, ExportSource, RunScore, ScoreFilter, TraceInvocation};
use crate::source::mapping::RunHeader;
use crate::source::{ProductionRun, TranscriptQuery};

/// The most earlier runs of a session a case starts with.
const HISTORY_RUNS: usize = 50;

/// How long before a run its session's earlier runs are read.
const HISTORY_WINDOW: TimeDelta = TimeDelta::days(30);

#[async_trait::async_trait]
impl ExportSource for BigQueryTranscriptSource {
    async fn trace_invocations(
        &self,
        traces: &[String],
    ) -> Result<Vec<TraceInvocation>, MonitorError> {
        if traces.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .query(
                &trace_invocations_sql(&self.table),
                vec![string_array_param("traces", traces)],
            )
            .await?;
        rows.iter()
            .map(|r| {
                Ok(TraceInvocation::new(
                    cell_str(r, "trace_id").unwrap_or_default(),
                    cell_str(r, "invocation_id").unwrap_or_default(),
                    cell_timestamp(r, "started_at")?,
                ))
            })
            .collect()
    }

    async fn runs(
        &self,
        ids: &[String],
        from: DateTime<Utc>,
    ) -> Result<Vec<ProductionRun>, MonitorError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let columns = self.columns().await?;
        let headers = run_headers(
            &self
                .query(
                    &run_headers_sql(&self.table, &columns),
                    row_params(ids, from),
                )
                .await?,
        )?;
        let mut runs = self.build_runs(&columns, headers, from).await?;
        runs.sort_by_key(|r| r.started_at);
        Ok(runs)
    }

    async fn histories(
        &self,
        runs: &[ProductionRun],
    ) -> Result<HashMap<String, Vec<ProductionRun>>, MonitorError> {
        let starts = runs.iter().map(|r| r.started_at);
        let (Some(first), Some(last)) = (starts.clone().min(), starts.max()) else {
            return Ok(HashMap::new());
        };
        let mut sessions: Vec<String> = runs.iter().map(|r| r.session_id.clone()).collect();
        sessions.sort();
        sessions.dedup();
        let columns = self.columns().await?;
        let params = vec![
            string_array_param("sessions", &sessions),
            timestamp_param("from", first - HISTORY_WINDOW),
            timestamp_param("to", last),
        ];
        let headers = run_headers(
            &self
                .query(&session_headers_sql(&self.table, &columns), params)
                .await?,
        )?;
        let mut picks: Vec<(String, Vec<String>)> = Vec::with_capacity(runs.len());
        let mut needed: Vec<RunHeader> = Vec::new();
        let mut seen = HashSet::new();
        for run in runs {
            let mut earlier: Vec<&RunHeader> = headers
                .iter()
                .filter(|h| {
                    !h.session_id.is_empty()
                        && h.session_id == run.session_id
                        && h.invocation_id != run.invocation_id
                        && h.started_at < run.started_at
                        && h.started_at >= run.started_at - HISTORY_WINDOW
                })
                .collect();
            earlier.sort_by_key(|h| h.started_at);
            let earlier = &earlier[earlier.len().saturating_sub(HISTORY_RUNS)..];
            for h in earlier {
                if seen.insert(h.invocation_id.clone()) {
                    needed.push((*h).clone());
                }
            }
            picks.push((
                run.invocation_id.clone(),
                earlier.iter().map(|h| h.invocation_id.clone()).collect(),
            ));
        }
        let from = needed.iter().map(|h| h.started_at).min().unwrap_or(first);
        let built: HashMap<String, ProductionRun> = self
            .build_runs(&columns, needed, from)
            .await?
            .into_iter()
            .map(|r| (r.invocation_id.clone(), r))
            .collect();
        Ok(picks
            .into_iter()
            .map(|(id, ids)| {
                let history = ids.iter().filter_map(|i| built.get(i).cloned()).collect();
                (id, history)
            })
            .collect())
    }

    async fn contexts(
        &self,
        ids: &[String],
        from: DateTime<Utc>,
    ) -> Result<HashMap<String, ExportContext>, MonitorError> {
        let mut out: HashMap<String, ExportContext> = ids
            .iter()
            .map(|id| (id.clone(), ExportContext::default()))
            .collect();
        if ids.is_empty() {
            return Ok(out);
        }
        let columns = self.columns().await?;
        if !columns.attributes {
            // Scores (source, author) and prompt versions are both in `attributes`.
            return Ok(out);
        }
        let scores = if columns.content {
            self.query(&scores_sql(&self.table, &columns), row_params(ids, from))
                .await?
        } else {
            Vec::new()
        };
        for r in scores {
            let mut score = RunScore::new(
                cell_str(&r, "name").unwrap_or_default(),
                cell_json(&r, "value").unwrap_or(Value::Null),
                cell_str(&r, "data_type").unwrap_or_default(),
                cell_str(&r, "source").unwrap_or_default(),
                cell_str(&r, "author").unwrap_or_default(),
                cell_timestamp(&r, "timestamp")?,
            );
            if let Some(comment) = cell_str(&r, "comment").filter(|c| !c.is_empty()) {
                score = score.with_comment(comment);
            }
            let id = cell_str(&r, "invocation_id").unwrap_or_default();
            out.entry(id).or_default().scores.push(score);
        }
        for r in self
            .query(&prompts_sql(&self.table, &columns), row_params(ids, from))
            .await?
        {
            let name = cell_str(&r, "prompt_name");
            let version = cell_str(&r, "prompt_version").and_then(|v| v.parse::<i64>().ok());
            if let (Some(name), Some(version)) = (name, version) {
                let id = cell_str(&r, "invocation_id").unwrap_or_default();
                out.entry(id).or_default().prompt = Some((name, version));
            }
        }
        Ok(out)
    }

    async fn matching(
        &self,
        filters: &[ScoreFilter],
        q: &TranscriptQuery,
    ) -> Result<Vec<(String, DateTime<Utc>)>, MonitorError> {
        if q.limit == 0 {
            return Ok(Vec::new());
        }
        let columns = self.columns().await?;
        for (needed, present, what) in [
            (
                !filters.is_empty(),
                columns.content,
                "score filters need the `content` column",
            ),
            (
                q.agent.is_some(),
                columns.attributes,
                "the `agent` filter needs the `attributes` column",
            ),
        ] {
            if needed && !present {
                return Err(MonitorError::Source(format!(
                    "{what}, which {} does not have (payload_column_denylist)",
                    self.table
                )));
            }
        }
        let mut params = invocation_params(q);
        for (i, f) in filters.iter().enumerate() {
            let (name, value) = match f {
                ScoreFilter::Equals { name, value } => {
                    (name, param(&format!("value_{i}"), "STRING", value.clone()))
                }
                ScoreFilter::Compare { name, value, .. } => (
                    name,
                    param(&format!("value_{i}"), "FLOAT64", value.to_string()),
                ),
            };
            params.push(param(&format!("name_{i}"), "STRING", name.clone()));
            params.push(value);
        }
        self.query(
            &matching_sql(&self.table, filters, q.agent.is_some(), &columns),
            params,
        )
        .await?
        .iter()
        .map(|r| {
            Ok((
                cell_str(r, "invocation_id").unwrap_or_default(),
                cell_timestamp(r, "started_at")?,
            ))
        })
        .collect()
    }
}

/// `, event_id DESC` when the table has `event_id`: the tie-break of rows with equal
/// timestamps, newest first.
fn event_id_desc(columns: &Columns) -> &'static str {
    if columns.event_id {
        ", event_id DESC"
    } else {
        ""
    }
}

/// The latest `FEEDBACK` row per (invocation, score name, source) of the invocations
/// `@invocation_ids`, recorded from `@from` on; `value` is the JSON value.
fn scores_sql(table: &str, columns: &Columns) -> String {
    format!(
        "SELECT invocation_id, timestamp, JSON_VALUE(content, '$.name') AS name,\n  \
         JSON_QUERY(content, '$.value') AS value, \
         JSON_VALUE(content, '$.data_type') AS data_type,\n  \
         JSON_VALUE(attributes, '$.source') AS source, \
         JSON_VALUE(attributes, '$.author') AS author,\n  \
         JSON_VALUE(content, '$.comment') AS comment\n\
         FROM {table}\n\
         WHERE event_type = 'FEEDBACK'\n  \
         AND invocation_id IN UNNEST(@invocation_ids)\n  \
         AND timestamp >= @from\n\
         QUALIFY ROW_NUMBER() OVER (\n  \
         PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), \
         JSON_VALUE(attributes, '$.source')\n  \
         ORDER BY timestamp DESC{}) = 1\n\
         ORDER BY invocation_id, name, source",
        event_id_desc(columns)
    )
}

/// The prompt name and version of the first `LLM_REQUEST` row with one, per invocation of
/// `@invocation_ids`, logged from `@from` on.
fn prompts_sql(table: &str, columns: &Columns) -> String {
    format!(
        "SELECT invocation_id, JSON_VALUE(attributes, '$.prompt.name') AS prompt_name,\n  \
         SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS prompt_version\n\
         FROM {table}\n\
         WHERE event_type = 'LLM_REQUEST'\n  \
         AND invocation_id IN UNNEST(@invocation_ids)\n  \
         AND timestamp >= @from\n  \
         AND JSON_VALUE(attributes, '$.prompt.name') IS NOT NULL\n  \
         AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IS NOT NULL\n\
         QUALIFY ROW_NUMBER() OVER (PARTITION BY invocation_id ORDER BY timestamp{}) = 1",
        if columns.event_id { ", event_id" } else { "" }
    )
}

/// The invocations that started in `[@from, @to)` (root agent `@agent` when `with_agent`) whose
/// latest score of each name, across sources and recorded from `@from` on, passes every filter:
/// `@name_<i>` with `@value_<i>`; newest first, at most `@limit`.
fn matching_sql(
    table: &str,
    filters: &[ScoreFilter],
    with_agent: bool,
    columns: &Columns,
) -> String {
    let agent = if with_agent {
        "\n    AND JSON_VALUE(attributes, '$.root_agent_name') = @agent"
    } else {
        ""
    };
    let runs = format!(
        "WITH runs AS (\n  \
         SELECT invocation_id, MIN(timestamp) AS started_at\n  \
         FROM {table}\n  \
         WHERE event_type = 'INVOCATION_STARTING'\n    \
         AND timestamp >= @from AND timestamp < @to{agent}\n  \
         GROUP BY invocation_id)"
    );
    if filters.is_empty() {
        return format!(
            "{runs}\n\
             SELECT invocation_id, started_at\n\
             FROM runs\n\
             ORDER BY started_at DESC\n\
             LIMIT @limit"
        );
    }
    let conditions: Vec<String> = filters
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let test = match f {
                ScoreFilter::Equals { .. } => format!("s.value = @value_{i}"),
                ScoreFilter::Compare { op, .. } => format!(
                    "s.data_type = 'number' AND SAFE_CAST(s.value AS FLOAT64) {} @value_{i}",
                    op.as_sql()
                ),
            };
            format!(
                "EXISTS (SELECT 1 FROM scores s WHERE s.invocation_id = r.invocation_id \
                 AND s.name = @name_{i} AND {test})"
            )
        })
        .collect();
    format!(
        "{runs},\n\
         scores AS (\n  \
         SELECT invocation_id, JSON_VALUE(content, '$.name') AS name,\n    \
         JSON_VALUE(content, '$.data_type') AS data_type, \
         JSON_VALUE(content, '$.value') AS value\n  \
         FROM {table}\n  \
         WHERE event_type = 'FEEDBACK' AND timestamp >= @from\n  \
         QUALIFY ROW_NUMBER() OVER (\n    \
         PARTITION BY invocation_id, JSON_VALUE(content, '$.name')\n    \
         ORDER BY timestamp DESC{}) = 1)\n\
         SELECT r.invocation_id, r.started_at\n\
         FROM runs r\n\
         WHERE {}\n\
         ORDER BY r.started_at DESC\n\
         LIMIT @limit",
        event_id_desc(columns),
        conditions.join("\n  AND ")
    )
}

/// The invocations of the traces `@traces` whose `INVOCATION_STARTING` row is from the last 30
/// days, oldest first.
fn trace_invocations_sql(table: &str) -> String {
    format!(
        "SELECT trace_id, invocation_id, MIN(timestamp) AS started_at\n\
         FROM {table}\n\
         WHERE event_type = 'INVOCATION_STARTING'\n  \
         AND trace_id IN UNNEST(@traces)\n  \
         AND timestamp >= TIMESTAMP_SUB(CURRENT_TIMESTAMP(), INTERVAL 30 DAY)\n\
         GROUP BY trace_id, invocation_id\n\
         ORDER BY started_at, invocation_id"
    )
}

/// The run headers of the invocations `@invocation_ids` that started at or after `@from`.
fn run_headers_sql(table: &str, columns: &Columns) -> String {
    format!(
        "SELECT invocation_id, MIN(timestamp) AS started_at, \
         ANY_VALUE(session_id) AS session_id, {} AS agent\n\
         FROM {table}\n\
         WHERE event_type = 'INVOCATION_STARTING'\n  \
         AND invocation_id IN UNNEST(@invocation_ids)\n  \
         AND timestamp >= @from\n\
         GROUP BY invocation_id\n\
         ORDER BY started_at",
        agent_expr(columns)
    )
}

/// The run headers of the sessions `@sessions` that started in `[@from, @to)`.
fn session_headers_sql(table: &str, columns: &Columns) -> String {
    format!(
        "SELECT invocation_id, MIN(timestamp) AS started_at, \
         ANY_VALUE(session_id) AS session_id, {} AS agent\n\
         FROM {table}\n\
         WHERE event_type = 'INVOCATION_STARTING'\n  \
         AND session_id IN UNNEST(@sessions)\n  \
         AND timestamp >= @from AND timestamp < @to\n\
         GROUP BY invocation_id\n\
         ORDER BY started_at",
        agent_expr(columns)
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::tests::{ALL, HEAD, QUERIES, ROW, reply, row, source, table_reply};
    use super::super::{query_body, rows_sql};
    use super::*;
    use crate::export::CompareOp;
    use crate::testutil::{Exchange, at, exchange, replay, run};

    const TBL: &str = "`proj.analytics.agent_events`";

    /// Base time: 2025-11-17T03:26:40Z.
    const T: i64 = 1_763_350_000;

    fn micros(secs: i64) -> String {
        (secs * 1_000_000).to_string()
    }

    /// A `jobs.query` of `sql` with `params`, answered with `response`.
    fn post(sql: &str, params: Vec<Value>, response: &str) -> Exchange {
        exchange(
            "POST",
            QUERIES,
            &query_body(sql, params).to_string(),
            200,
            response,
        )
    }

    /// A header row: invocation `id` of session `session` started at `secs`.
    fn head(id: &str, session: &str, secs: i64) -> Vec<Value> {
        vec![
            json!(id),
            json!(micros(secs)),
            json!(session),
            json!("support"),
        ]
    }

    /// The user message `user` and the reply `ok` of invocation `id` at `secs`.
    fn turn(id: &str, secs: i64, user: &str) -> [Vec<Value>; 2] {
        let micros = secs * 1_000_000;
        [
            row(
                id,
                "USER_MESSAGE_RECEIVED",
                micros + 1,
                json!({"text_summary": user}),
                "tr",
            ),
            row(
                id,
                "AGENT_RESPONSE",
                micros + 2,
                json!({"response": "text: 'ok'"}),
                "tr",
            ),
        ]
    }

    /// A run of `session` (see `testutil::run`).
    fn session_run(id: &str, session: &str, secs: i64) -> ProductionRun {
        let mut r = run(id, secs, &[("q", "ok", &[])]);
        r.session_id = session.into();
        r
    }

    #[tokio::test]
    async fn trace_invocations_reads_the_last_30_days_oldest_first() {
        let sql = trace_invocations_sql(TBL);
        assert_eq!(
            sql,
            "SELECT trace_id, invocation_id, MIN(timestamp) AS started_at\n\
             FROM `proj.analytics.agent_events`\n\
             WHERE event_type = 'INVOCATION_STARTING'\n  \
             AND trace_id IN UNNEST(@traces)\n  \
             AND timestamp >= TIMESTAMP_SUB(CURRENT_TIMESTAMP(), INTERVAL 30 DAY)\n\
             GROUP BY trace_id, invocation_id\n\
             ORDER BY started_at, invocation_id"
        );
        let trace = "a".repeat(32);
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[post(
                &sql,
                vec![json!({
                    "name": "traces",
                    "parameterType": {"type": "ARRAY", "arrayType": {"type": "STRING"}},
                    "parameterValue": {"arrayValues": [{"value": trace}]},
                })],
                &reply(
                    &["trace_id", "invocation_id", "started_at"],
                    &[
                        vec![json!(trace), json!("e-1"), json!(micros(T))],
                        vec![json!(trace), json!("e-2"), json!(micros(T + 5))],
                    ],
                    None,
                ),
            )],
        );
        let got = source(http)
            .trace_invocations(std::slice::from_ref(&trace))
            .await
            .unwrap();
        assert_eq!(
            got,
            [
                TraceInvocation::new(trace.clone(), "e-1", at(T)),
                TraceInvocation::new(trace.clone(), "e-2", at(T + 5)),
            ]
        );
        let dir = tempfile::tempdir().unwrap();
        let none = source(replay(dir.path(), &[]));
        assert!(none.trace_invocations(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn runs_reads_headers_then_rows_oldest_first() {
        assert_eq!(
            run_headers_sql(TBL, &ALL),
            "SELECT invocation_id, MIN(timestamp) AS started_at, \
             ANY_VALUE(session_id) AS session_id, \
             ANY_VALUE(JSON_VALUE(attributes, '$.root_agent_name')) AS agent\n\
             FROM `proj.analytics.agent_events`\n\
             WHERE event_type = 'INVOCATION_STARTING'\n  \
             AND invocation_id IN UNNEST(@invocation_ids)\n  \
             AND timestamp >= @from\n\
             GROUP BY invocation_id\n\
             ORDER BY started_at"
        );
        let ids = vec!["e-2".to_owned(), "e-1".to_owned()];
        let from = at(T);
        let mut rows = Vec::new();
        rows.extend(turn("e-1", T, "first"));
        rows.extend(turn("e-2", T + 60, "second"));
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                post(
                    &run_headers_sql(TBL, &ALL),
                    row_params(&ids, from),
                    &reply(
                        &HEAD,
                        &[head("e-1", "s-1", T), head("e-2", "s-1", T + 60)],
                        None,
                    ),
                ),
                post(
                    &rows_sql(TBL, &ALL),
                    row_params(&["e-1".to_owned(), "e-2".to_owned()], from),
                    &reply(&ROW, &rows, None),
                ),
            ],
        );
        let runs = source(http).runs(&ids, from).await.unwrap();
        let got: Vec<(&str, &str)> = runs
            .iter()
            .map(|r| {
                (
                    r.invocation_id.as_str(),
                    r.transcript.turns[0].user.as_str(),
                )
            })
            .collect();
        assert_eq!(got, [("e-1", "first"), ("e-2", "second")]);
        assert!(runs.iter().all(|r| r.content_complete));
        let dir = tempfile::tempdir().unwrap();
        let none = source(replay(dir.path(), &[]));
        assert!(none.runs(&[], from).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn histories_keep_the_latest_50_earlier_runs_of_each_session_within_30_days() {
        assert_eq!(
            session_headers_sql(TBL, &ALL),
            "SELECT invocation_id, MIN(timestamp) AS started_at, \
             ANY_VALUE(session_id) AS session_id, \
             ANY_VALUE(JSON_VALUE(attributes, '$.root_agent_name')) AS agent\n\
             FROM `proj.analytics.agent_events`\n\
             WHERE event_type = 'INVOCATION_STARTING'\n  \
             AND session_id IN UNNEST(@sessions)\n  \
             AND timestamp >= @from AND timestamp < @to\n\
             GROUP BY invocation_id\n\
             ORDER BY started_at"
        );
        // Session s: `old` 31 days before r2, then h-00 … h-51 every 10 s, then r2 and r3.
        // Session u: u1 alone.
        let h = |i: i64| format!("h-{i:02}");
        let h_at = |i: i64| T - (52 - i) * 10;
        let mut heads = vec![head("old", "s", T - 31 * 86_400)];
        heads.extend((0..52).map(|i| head(&h(i), "s", h_at(i))));
        heads.push(head("r2", "s", T));
        heads.push(head("u1", "u", T + 50));
        // r2 keeps h-02 … h-51; r3 keeps h-03 … h-51 and r2.
        let needed: Vec<String> = (2..52).map(h).chain(["r2".to_owned()]).collect();
        let mut rows = Vec::new();
        for i in 2..52 {
            rows.extend(turn(&h(i), h_at(i), &h(i)));
        }
        rows.extend(turn("r2", T, "two"));
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                post(
                    &session_headers_sql(TBL, &ALL),
                    vec![
                        string_array_param("sessions", &["s".to_owned(), "u".to_owned()]),
                        timestamp_param("from", at(T) - TimeDelta::days(30)),
                        timestamp_param("to", at(T + 100)),
                    ],
                    &reply(&HEAD, &heads, None),
                ),
                post(
                    &rows_sql(TBL, &ALL),
                    row_params(&needed, at(h_at(2))),
                    &reply(&ROW, &rows, None),
                ),
            ],
        );
        let runs = [
            session_run("r2", "s", T),
            session_run("r3", "s", T + 100),
            session_run("u1", "u", T + 50),
        ];
        let got = source(http).histories(&runs).await.unwrap();
        let users = |id: &str| -> Vec<String> {
            got[id]
                .iter()
                .map(|r| r.transcript.turns[0].user.clone())
                .collect()
        };
        let r2_want: Vec<String> = (2..52).map(h).collect();
        assert_eq!(users("r2"), r2_want);
        let r3_want: Vec<String> = (3..52).map(h).chain(["two".to_owned()]).collect();
        assert_eq!(users("r3"), r3_want);
        assert!(got["u1"].is_empty());
        assert_eq!(got.len(), 3);
        let dir = tempfile::tempdir().unwrap();
        let none = source(replay(dir.path(), &[]));
        assert!(none.histories(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn contexts_read_the_latest_scores_and_the_first_prompt_version() {
        assert_eq!(
            scores_sql(TBL, &ALL),
            "SELECT invocation_id, timestamp, JSON_VALUE(content, '$.name') AS name,\n  \
             JSON_QUERY(content, '$.value') AS value, \
             JSON_VALUE(content, '$.data_type') AS data_type,\n  \
             JSON_VALUE(attributes, '$.source') AS source, \
             JSON_VALUE(attributes, '$.author') AS author,\n  \
             JSON_VALUE(content, '$.comment') AS comment\n\
             FROM `proj.analytics.agent_events`\n\
             WHERE event_type = 'FEEDBACK'\n  \
             AND invocation_id IN UNNEST(@invocation_ids)\n  \
             AND timestamp >= @from\n\
             QUALIFY ROW_NUMBER() OVER (\n  \
             PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), \
             JSON_VALUE(attributes, '$.source')\n  \
             ORDER BY timestamp DESC, event_id DESC) = 1\n\
             ORDER BY invocation_id, name, source"
        );
        assert_eq!(
            prompts_sql(TBL, &ALL),
            "SELECT invocation_id, JSON_VALUE(attributes, '$.prompt.name') AS prompt_name,\n  \
             SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS prompt_version\n\
             FROM `proj.analytics.agent_events`\n\
             WHERE event_type = 'LLM_REQUEST'\n  \
             AND invocation_id IN UNNEST(@invocation_ids)\n  \
             AND timestamp >= @from\n  \
             AND JSON_VALUE(attributes, '$.prompt.name') IS NOT NULL\n  \
             AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IS NOT NULL\n\
             QUALIFY ROW_NUMBER() OVER (PARTITION BY invocation_id ORDER BY timestamp, event_id) = 1"
        );
        let ids = vec!["e-1".to_owned(), "e-2".to_owned(), "e-3".to_owned()];
        let from = at(T);
        let score_cols = [
            "invocation_id",
            "timestamp",
            "name",
            "value",
            "data_type",
            "source",
            "author",
            "comment",
        ];
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                post(
                    &scores_sql(TBL, &ALL),
                    row_params(&ids, from),
                    &reply(
                        &score_cols,
                        &[
                            vec![
                                json!("e-1"),
                                json!(micros(T + 10)),
                                json!("thumbs"),
                                json!("true"),
                                json!("bool"),
                                json!("app"),
                                json!("user-1"),
                                Value::Null,
                            ],
                            vec![
                                json!("e-1"),
                                json!(micros(T + 20)),
                                json!("quality"),
                                json!("\"helpful\""),
                                json!("category"),
                                json!("console"),
                                json!("chris"),
                                json!("clear"),
                            ],
                            vec![
                                json!("e-2"),
                                json!(micros(T + 30)),
                                json!("rating"),
                                json!("4"),
                                json!("number"),
                                json!("app"),
                                json!("user-1"),
                                json!(""),
                            ],
                        ],
                        None,
                    ),
                ),
                post(
                    &prompts_sql(TBL, &ALL),
                    row_params(&ids, from),
                    &reply(
                        &["invocation_id", "prompt_name", "prompt_version"],
                        &[vec![json!("e-2"), json!("e2e-assistant"), json!("1")]],
                        None,
                    ),
                ),
            ],
        );
        let got = source(http).contexts(&ids, from).await.unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(
            got["e-1"].scores,
            [
                RunScore::new("thumbs", json!(true), "bool", "app", "user-1", at(T + 10)),
                RunScore::new(
                    "quality",
                    json!("helpful"),
                    "category",
                    "console",
                    "chris",
                    at(T + 20)
                )
                .with_comment("clear"),
            ]
        );
        assert_eq!(got["e-1"].prompt, None);
        assert_eq!(
            got["e-2"],
            ExportContext::new(vec![RunScore::new(
                "rating",
                json!(4),
                "number",
                "app",
                "user-1",
                at(T + 30)
            )])
            .with_prompt("e2e-assistant", 1)
        );
        assert_eq!(got["e-3"], ExportContext::default());
        let dir = tempfile::tempdir().unwrap();
        let none = source(replay(dir.path(), &[]));
        assert!(none.contexts(&[], from).await.unwrap().is_empty());
    }

    fn window(agent: Option<&str>, limit: usize) -> TranscriptQuery {
        TranscriptQuery {
            from: at(T - 3_600),
            to: at(T),
            agent: agent.map(str::to_owned),
            limit,
        }
    }

    #[tokio::test]
    async fn matching_filters_on_the_latest_score_per_name() {
        let filters = [
            ScoreFilter::parse("thumbs=true").unwrap(),
            ScoreFilter::parse("rating>=4").unwrap(),
        ];
        let sql = matching_sql(TBL, &filters, true, &ALL);
        assert_eq!(
            sql,
            "WITH runs AS (\n  \
             SELECT invocation_id, MIN(timestamp) AS started_at\n  \
             FROM `proj.analytics.agent_events`\n  \
             WHERE event_type = 'INVOCATION_STARTING'\n    \
             AND timestamp >= @from AND timestamp < @to\n    \
             AND JSON_VALUE(attributes, '$.root_agent_name') = @agent\n  \
             GROUP BY invocation_id),\n\
             scores AS (\n  \
             SELECT invocation_id, JSON_VALUE(content, '$.name') AS name,\n    \
             JSON_VALUE(content, '$.data_type') AS data_type, \
             JSON_VALUE(content, '$.value') AS value\n  \
             FROM `proj.analytics.agent_events`\n  \
             WHERE event_type = 'FEEDBACK' AND timestamp >= @from\n  \
             QUALIFY ROW_NUMBER() OVER (\n    \
             PARTITION BY invocation_id, JSON_VALUE(content, '$.name')\n    \
             ORDER BY timestamp DESC, event_id DESC) = 1)\n\
             SELECT r.invocation_id, r.started_at\n\
             FROM runs r\n\
             WHERE EXISTS (SELECT 1 FROM scores s WHERE s.invocation_id = r.invocation_id \
             AND s.name = @name_0 AND s.value = @value_0)\n  \
             AND EXISTS (SELECT 1 FROM scores s WHERE s.invocation_id = r.invocation_id \
             AND s.name = @name_1 AND s.data_type = 'number' \
             AND SAFE_CAST(s.value AS FLOAT64) >= @value_1)\n\
             ORDER BY r.started_at DESC\n\
             LIMIT @limit"
        );
        let q = window(Some("support"), 5);
        let mut params = invocation_params(&q);
        params.extend([
            json!({"name": "name_0", "parameterType": {"type": "STRING"},
                   "parameterValue": {"value": "thumbs"}}),
            json!({"name": "value_0", "parameterType": {"type": "STRING"},
                   "parameterValue": {"value": "true"}}),
            json!({"name": "name_1", "parameterType": {"type": "STRING"},
                   "parameterValue": {"value": "rating"}}),
            json!({"name": "value_1", "parameterType": {"type": "FLOAT64"},
                   "parameterValue": {"value": "4"}}),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                post(
                    &sql,
                    params,
                    &reply(
                        &["invocation_id", "started_at"],
                        &[
                            vec![json!("e-2"), json!(micros(T - 10))],
                            vec![json!("e-1"), json!(micros(T - 20))],
                        ],
                        None,
                    ),
                ),
            ],
        );
        let got = source(http).matching(&filters, &q).await.unwrap();
        assert_eq!(
            got,
            [
                ("e-2".to_owned(), at(T - 10)),
                ("e-1".to_owned(), at(T - 20))
            ]
        );
        assert_eq!(
            [CompareOp::Lt, CompareOp::Le, CompareOp::Gt, CompareOp::Ge].map(CompareOp::as_sql),
            ["<", "<=", ">", ">="]
        );
    }

    #[tokio::test]
    async fn matching_without_filters_lists_the_window() {
        let sql = matching_sql(TBL, &[], false, &ALL);
        assert_eq!(
            sql,
            "WITH runs AS (\n  \
             SELECT invocation_id, MIN(timestamp) AS started_at\n  \
             FROM `proj.analytics.agent_events`\n  \
             WHERE event_type = 'INVOCATION_STARTING'\n    \
             AND timestamp >= @from AND timestamp < @to\n  \
             GROUP BY invocation_id)\n\
             SELECT invocation_id, started_at\n\
             FROM runs\n\
             ORDER BY started_at DESC\n\
             LIMIT @limit"
        );
        let q = window(None, 2);
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                post(
                    &sql,
                    invocation_params(&q),
                    &reply(&["invocation_id", "started_at"], &[], None),
                ),
            ],
        );
        assert!(source(http).matching(&[], &q).await.unwrap().is_empty());
        let dir = tempfile::tempdir().unwrap();
        let none = source(replay(dir.path(), &[]));
        assert!(
            none.matching(&[], &window(None, 0))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn tables_without_event_id_break_no_ties() {
        let old = Columns {
            event_id: false,
            ..ALL
        };
        let filters = [ScoreFilter::parse("rating>=4").unwrap()];
        for sql in [
            scores_sql(TBL, &old),
            prompts_sql(TBL, &old),
            matching_sql(TBL, &filters, false, &old),
        ] {
            assert!(!sql.contains("event_id"), "{sql}");
        }
        assert!(scores_sql(TBL, &old).contains("ORDER BY timestamp DESC) = 1"));
        assert!(prompts_sql(TBL, &old).contains("ORDER BY timestamp) = 1"));
    }

    #[tokio::test]
    async fn contexts_skip_what_the_table_does_not_have() {
        let ids = vec!["e-1".to_owned()];
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[table_reply(&["attributes"])]);
        let got = source(http).contexts(&ids, at(T)).await.unwrap();
        assert_eq!(got["e-1"], ExportContext::default());

        let no_content = Columns {
            content: false,
            ..ALL
        };
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&["content"]),
                post(
                    &prompts_sql(TBL, &no_content),
                    row_params(&ids, at(T)),
                    &reply(
                        &["invocation_id", "prompt_name", "prompt_version"],
                        &[vec![json!("e-1"), json!("p"), json!("2")]],
                        None,
                    ),
                ),
            ],
        );
        let got = source(http).contexts(&ids, at(T)).await.unwrap();
        assert_eq!(got["e-1"], ExportContext::default().with_prompt("p", 2));
    }

    #[tokio::test]
    async fn matching_needs_the_columns_its_filters_read() {
        let filters = [ScoreFilter::parse("thumbs=true").unwrap()];
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[table_reply(&["content"])]);
        let err = source(http)
            .matching(&filters, &window(None, 5))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "transcript source: score filters need the `content` column, which \
             `proj.analytics.agent_events` does not have (payload_column_denylist)"
        );
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[table_reply(&["attributes"])]);
        let err = source(http)
            .matching(&[], &window(Some("support"), 5))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("the `agent` filter needs the `attributes` column"),
            "{err}"
        );
    }
}
