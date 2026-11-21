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

//! [`ExportSource`] for the BigQuery source: the invocations of traces, runs by invocation id
//! and session history, each read with parameterised queries bounded by `timestamp`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};

use super::{
    BigQueryTranscriptSource, Columns, agent_expr, cell_str, cell_timestamp, row_params,
    run_headers, string_array_param, timestamp_param,
};
use crate::error::MonitorError;
use crate::export::{ExportSource, TraceInvocation};
use crate::source::ProductionRun;
use crate::source::mapping::RunHeader;

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
}
