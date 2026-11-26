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

//! [`GuardSource`] for the BigQuery source: the guard's calls, feedback and runs, each read with
//! parameterised queries through the transcript source's client, billing project and
//! `maximumBytesBilled`.
//!
//! The guard's calls are the `LLM_REQUEST` rows of `@prompt` / `@label` at `@baseline` or
//! `@rollout` (one version, `@version`, for runs) logged in `[@from, @to)`. `LLM_ERROR` and
//! `LLM_RESPONSE` rows join them on `trace_id` / `span_id` and are read from `@from` to an hour
//! after `@to`; `FEEDBACK` rows join them by `invocation_id` and are read from `@from` on. A
//! `sequential` guard's sample starts at the first such call at `@rollout`.
//!
//! [`AlertSource`] for the same table: every `LLM_REQUEST` row in `[@from, @to)`, grouped by
//! model × agent × prompt, with `LLM_ERROR` and `LLM_RESPONSE` rows read from `@from` on.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_model::HttpClient;
use serde_json::{Map, Value};

use super::{
    BigQueryTranscriptSource, Columns, cell_str, cell_timestamp, param, run_headers,
    string_array_param, timestamp_param,
};
use crate::alert_source::{AlertQuery, AlertSource, UsageGroup};
use crate::error::MonitorError;
use crate::gcp::TokenSource;
use crate::guard_source::{GuardQuery, GuardSource, ModelTokens, VersionCalls, VersionFeedback};
use crate::guard_spec::FeedbackSource;
use crate::prices::TokenUsage;
use crate::source::ProductionRun;

/// Reads a guard's data from an agent-analytics table. Every query, including the transcript
/// queries behind [`GuardSource::version_runs`], is capped at `maximumBytesBilled`
/// ([`BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED`] unless
/// [`BigQueryGuardSource::with_max_bytes_billed`] says otherwise).
pub struct BigQueryGuardSource {
    inner: BigQueryTranscriptSource,
}

impl BigQueryGuardSource {
    /// The default `maximumBytesBilled` of every query: 1 GiB.
    pub const DEFAULT_MAX_BYTES_BILLED: i64 = 1_073_741_824;

    /// The table `project.dataset.table`, read with Application Default Credentials. Must be
    /// called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Source`] for an invalid identifier or when no credentials can be built.
    pub fn new(project: &str, dataset: &str, table: &str) -> Result<Self, MonitorError> {
        Ok(Self::wrap(BigQueryTranscriptSource::new(
            project, dataset, table,
        )?))
    }

    /// Like [`BigQueryGuardSource::new`], with an explicit HTTP client and credentials.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Source`] when `project` is not `[A-Za-z0-9_-]+` or `dataset` / `table`
    /// is not `[A-Za-z0-9_]+`.
    pub fn with_client(
        project: &str,
        dataset: &str,
        table: &str,
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn TokenSource>,
    ) -> Result<Self, MonitorError> {
        Ok(Self::wrap(BigQueryTranscriptSource::with_client(
            project, dataset, table, http, auth,
        )?))
    }

    fn wrap(inner: BigQueryTranscriptSource) -> Self {
        Self {
            inner: inner.with_max_bytes_billed(Self::DEFAULT_MAX_BYTES_BILLED),
        }
    }

    /// Runs the queries as jobs in `project`, which pays for them, instead of the table's
    /// project; the table read is unchanged.
    #[must_use]
    pub fn with_billing_project(self, project: &str) -> Self {
        Self {
            inner: self.inner.with_billing_project(project),
        }
    }

    /// Caps every query at `bytes` billed (`maximumBytesBilled`): a query that would bill more
    /// fails instead of running.
    #[must_use]
    pub fn with_max_bytes_billed(self, bytes: i64) -> Self {
        Self {
            inner: self.inner.with_max_bytes_billed(bytes),
        }
    }

    /// The table's columns: an error naming `command` (`guards` or `alerts`) when it has no
    /// `attributes` column, or no `content` column and `content` is needed.
    async fn columns(&self, command: &str, content: bool) -> Result<Columns, MonitorError> {
        let columns = self.inner.columns().await?;
        for (name, present) in [
            ("attributes", columns.attributes),
            ("content", columns.content || !content),
        ] {
            if !present {
                return Err(MonitorError::Source(format!(
                    "{command} need the `{name}` column, which {} does not have \
                     (payload_column_denylist)",
                    self.inner.table
                )));
            }
        }
        Ok(columns)
    }
}

#[async_trait::async_trait]
impl GuardSource for BigQueryGuardSource {
    async fn version_calls(&self, q: &GuardQuery) -> Result<Vec<VersionCalls>, MonitorError> {
        self.columns("guards", false).await?;
        let rows = self
            .inner
            .query(&version_calls_sql(&self.inner.table), call_params(q))
            .await?;
        let mut by_version: BTreeMap<u32, VersionCalls> = BTreeMap::new();
        for r in &rows {
            let version = cell_version(r)?;
            let calls = cell_count(r, "calls");
            let entry = by_version
                .entry(version)
                .or_insert_with(|| VersionCalls::new(version, 0, 0, None, Vec::new()));
            entry.calls += calls;
            entry.errors += cell_count(r, "errors");
            entry.p95_ms = entry.p95_ms.or_else(|| cell_f64(r, "p95_ms"));
            entry.models.push(ModelTokens::new(
                cell_str(r, "model").unwrap_or_default(),
                calls,
                TokenUsage::new(
                    cell_count(r, "input"),
                    cell_count(r, "cached"),
                    cell_count(r, "output"),
                    cell_count(r, "reasoning"),
                ),
            ));
        }
        Ok(by_version.into_values().collect())
    }

    async fn version_feedback(
        &self,
        q: &GuardQuery,
        name: &str,
        source: FeedbackSource,
    ) -> Result<Vec<VersionFeedback>, MonitorError> {
        let columns = self.columns("guards", true).await?;
        let rows = self
            .inner
            .query(
                &version_feedback_sql(&self.inner.table, &columns),
                feedback_params(q, name, source),
            )
            .await?;
        rows.iter()
            .map(|r| {
                Ok(VersionFeedback::new(
                    cell_version(r)?,
                    cell_str(r, "source").unwrap_or_default(),
                    cell_str(r, "data_type").unwrap_or_default(),
                    cell_count(r, "count"),
                    cell_count(r, "positive"),
                    cell_f64(r, "mean"),
                    cell_f64(r, "stddev"),
                ))
            })
            .collect()
    }

    async fn version_runs(
        &self,
        q: &GuardQuery,
        version: u32,
        limit: usize,
    ) -> Result<Vec<ProductionRun>, MonitorError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let columns = self.columns("guards", false).await?;
        let headers = run_headers(
            &self
                .inner
                .query(
                    &version_runs_sql(&self.inner.table),
                    run_params(q, version, limit),
                )
                .await?,
        )?;
        // The rows of a run are read from 10 minutes before its start, so from the earliest.
        let from = headers.iter().map(|h| h.started_at).min().unwrap_or(q.from);
        self.inner.build_runs(&columns, headers, from).await
    }

    async fn rollout_start(&self, q: &GuardQuery) -> Result<Option<DateTime<Utc>>, MonitorError> {
        self.columns("guards", false).await?;
        let rows = self
            .inner
            .query(&rollout_start_sql(&self.inner.table), start_params(q))
            .await?;
        first_call(&rows)
    }
}

/// The most model × agent × prompt groups [`AlertSource::usage`] returns; more is an error.
pub(crate) const MAX_USAGE_GROUPS: usize = 5_000;

#[async_trait::async_trait]
impl AlertSource for BigQueryGuardSource {
    async fn usage(&self, q: &AlertQuery) -> Result<Vec<UsageGroup>, MonitorError> {
        self.columns("alerts", false).await?;
        let rows = self
            .inner
            .query(&usage_sql(&self.inner.table), usage_params(q))
            .await?;
        if rows.len() > MAX_USAGE_GROUPS {
            return Err(MonitorError::Source(format!(
                "alert usage: more than {MAX_USAGE_GROUPS} model × agent × prompt groups"
            )));
        }
        Ok(rows
            .iter()
            .map(|r| {
                UsageGroup::new(
                    cell_str(r, "model").unwrap_or_default(),
                    cell_str(r, "agent").unwrap_or_default(),
                    cell_str(r, "prompt").filter(|p| !p.is_empty()),
                    cell_count(r, "calls"),
                    cell_count(r, "errors"),
                    TokenUsage::new(
                        cell_count(r, "input"),
                        cell_count(r, "cached"),
                        cell_count(r, "output"),
                        cell_count(r, "reasoning"),
                    ),
                )
            })
            .collect())
    }
}

/// The filter of the guard's calls (`LLM_REQUEST` rows of `@prompt` / `@label` in `[@from,
/// @to)`) at the versions `versions` (an SQL list of parameters).
fn calls_filter(versions: &str) -> String {
    format!(
        "event_type = 'LLM_REQUEST'
    AND timestamp >= @from AND timestamp < @to
    AND JSON_VALUE(attributes, '$.prompt.name') = @prompt
    AND JSON_VALUE(attributes, '$.prompt.label') = @label
    AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IN ({versions})"
    )
}

/// The time of the first guard call at `@rollout` (`first`, NULL without one).
pub(crate) fn rollout_start_sql(table: &str) -> String {
    let filter = calls_filter("@rollout");
    format!("SELECT MIN(timestamp) AS first\nFROM {table}\nWHERE {filter}")
}

/// Per version and model of the guard's calls: calls, calls with an `LLM_ERROR` row, the
/// summed tokens of their `LLM_RESPONSE` rows and the version's approximate p95
/// `latency_ms.total_ms`. Tokens are read as the console's usage pages read them (missing
/// values 0); the model is the `LLM_REQUEST` row's, else the response's `model_version`.
pub(crate) fn version_calls_sql(table: &str) -> String {
    let filter = calls_filter("@baseline, @rollout");
    format!(
        r"WITH calls AS (
  SELECT trace_id, span_id,
    SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version,
    JSON_VALUE(attributes, '$.model') AS model
  FROM {table}
  WHERE {filter}),
errors AS (
  SELECT DISTINCT trace_id, span_id
  FROM {table}
  WHERE event_type = 'LLM_ERROR'
    AND timestamp >= @from AND timestamp < TIMESTAMP_ADD(@to, INTERVAL 1 HOUR)),
responses AS (
  SELECT res.trace_id AS trace_id, res.span_id AS span_id,
    JSON_VALUE(res.attributes, '$.model_version') AS model_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms
  FROM {table} AS res
  WHERE res.event_type = 'LLM_RESPONSE'
    AND res.timestamp >= @from AND res.timestamp < TIMESTAMP_ADD(@to, INTERVAL 1 HOUR)
  QUALIFY ROW_NUMBER() OVER (PARTITION BY res.trace_id, res.span_id ORDER BY res.timestamp DESC) = 1),
j AS (
  SELECT calls.version AS version,
    COALESCE(NULLIF(calls.model, ''), NULLIF(responses.model_version, ''), '(unknown)') AS model,
    errors.trace_id IS NOT NULL AS failed,
    responses.input AS input, responses.cached AS cached, responses.output AS output,
    responses.reasoning AS reasoning, responses.total_ms AS total_ms
  FROM calls
  LEFT JOIN errors ON errors.trace_id = calls.trace_id AND errors.span_id = calls.span_id
  LEFT JOIN responses
    ON responses.trace_id = calls.trace_id AND responses.span_id = calls.span_id),
p AS (
  SELECT version, APPROX_QUANTILES(total_ms, 100)[SAFE_OFFSET(95)] AS p95_ms
  FROM j
  GROUP BY version)
SELECT j.version AS version, j.model AS model, COUNT(*) AS calls, COUNTIF(j.failed) AS errors,
  IFNULL(SUM(j.input), 0) AS input, IFNULL(SUM(j.cached), 0) AS cached,
  IFNULL(SUM(j.output), 0) AS output, IFNULL(SUM(j.reasoning), 0) AS reasoning,
  ANY_VALUE(p.p95_ms) AS p95_ms
FROM j JOIN p ON p.version = j.version
GROUP BY 1, 2
ORDER BY 1, 2"
    )
}

/// Per version, source and data type: the latest `@name` score per (invocation, name, source)
/// from the sources `@sources` (the console's rule) of the invocations with a guard call,
/// recorded from `@from` on: count, `true` values, and the mean and sample standard deviation
/// of number values.
pub(crate) fn version_feedback_sql(table: &str, columns: &Columns) -> String {
    let filter = calls_filter("@baseline, @rollout");
    let event_id = if columns.event_id {
        ", event_id DESC"
    } else {
        ""
    };
    format!(
        r"WITH calls AS (
  SELECT DISTINCT invocation_id,
    SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version
  FROM {table}
  WHERE {filter}
    AND invocation_id IS NOT NULL AND invocation_id != ''),
s AS (
  SELECT invocation_id, JSON_VALUE(attributes, '$.source') AS source,
    JSON_VALUE(content, '$.data_type') AS data_type,
    JSON_VALUE(content, '$.value') AS value
  FROM {table}
  WHERE event_type = 'FEEDBACK' AND timestamp >= @from
    AND invocation_id IS NOT NULL AND invocation_id != ''
    AND JSON_VALUE(content, '$.name') = @name
    AND JSON_VALUE(attributes, '$.source') IN UNNEST(@sources)
  QUALIFY ROW_NUMBER() OVER (
    PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), JSON_VALUE(attributes, '$.source')
    ORDER BY timestamp DESC{event_id}) = 1)
SELECT calls.version AS version, s.source AS source, IFNULL(s.data_type, '') AS data_type,
  COUNT(*) AS count, COUNTIF(s.value = 'true') AS positive,
  AVG(IF(s.data_type = 'number', SAFE_CAST(s.value AS FLOAT64), NULL)) AS mean,
  STDDEV_SAMP(IF(s.data_type = 'number', SAFE_CAST(s.value AS FLOAT64), NULL)) AS stddev
FROM s JOIN calls ON calls.invocation_id = s.invocation_id
GROUP BY 1, 2, 3
ORDER BY 1, 2, 3"
    )
}

/// The run headers (as the monitor's invocations query reads them) of the newest `@limit`
/// invocations with a guard call at `@version`, newest call first. An invocation's
/// `INVOCATION_STARTING` row is read from a day before `@from`; invocations without one are left
/// out.
pub(crate) fn version_runs_sql(table: &str) -> String {
    let filter = calls_filter("@version");
    format!(
        r"WITH ids AS (
  SELECT invocation_id, MAX(timestamp) AS last_call
  FROM {table}
  WHERE {filter}
    AND invocation_id IS NOT NULL AND invocation_id != ''
  GROUP BY invocation_id
  ORDER BY last_call DESC
  LIMIT @limit)
SELECT inv.invocation_id AS invocation_id, MIN(inv.timestamp) AS started_at,
  ANY_VALUE(inv.session_id) AS session_id,
  ANY_VALUE(JSON_VALUE(inv.attributes, '$.root_agent_name')) AS agent,
  MAX(ids.last_call) AS last_call
FROM {table} AS inv
JOIN ids ON ids.invocation_id = inv.invocation_id
WHERE inv.event_type = 'INVOCATION_STARTING'
  AND inv.timestamp >= TIMESTAMP_SUB(@from, INTERVAL 1 DAY) AND inv.timestamp < @to
GROUP BY inv.invocation_id
ORDER BY last_call DESC, invocation_id"
    )
}

/// Per model × agent × prompt of the `LLM_REQUEST` rows in `[@from, @to)`: calls, calls with
/// an `LLM_ERROR` row and the summed tokens of their latest `LLM_RESPONSE` rows, at most
/// `@limit` groups. Errors and responses are read from `@from` on. The model is the request's,
/// else the response's `model_version`, else `(unknown)`; the prompt is the request's prompt
/// name, an empty one counting as none. Tokens are read as [`version_calls_sql`] reads them.
pub(crate) fn usage_sql(table: &str) -> String {
    format!(
        r"WITH calls AS (
  SELECT trace_id, span_id, IFNULL(agent, '') AS agent,
    JSON_VALUE(attributes, '$.model') AS model,
    NULLIF(JSON_VALUE(attributes, '$.prompt.name'), '') AS prompt
  FROM {table}
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @from AND timestamp < @to),
errors AS (
  SELECT DISTINCT trace_id, span_id
  FROM {table}
  WHERE event_type = 'LLM_ERROR' AND timestamp >= @from),
responses AS (
  SELECT res.trace_id AS trace_id, res.span_id AS span_id,
    JSON_VALUE(res.attributes, '$.model_version') AS model_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning
  FROM {table} AS res
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @from
  QUALIFY ROW_NUMBER() OVER (PARTITION BY res.trace_id, res.span_id ORDER BY res.timestamp DESC) = 1),
j AS (
  SELECT COALESCE(NULLIF(calls.model, ''), NULLIF(responses.model_version, ''), '(unknown)') AS model,
    calls.agent AS agent, calls.prompt AS prompt,
    errors.trace_id IS NOT NULL AS failed,
    responses.input AS input, responses.cached AS cached, responses.output AS output,
    responses.reasoning AS reasoning
  FROM calls
  LEFT JOIN errors ON errors.trace_id = calls.trace_id AND errors.span_id = calls.span_id
  LEFT JOIN responses
    ON responses.trace_id = calls.trace_id AND responses.span_id = calls.span_id)
SELECT model, agent, prompt, COUNT(*) AS calls, COUNTIF(failed) AS errors,
  IFNULL(SUM(input), 0) AS input, IFNULL(SUM(cached), 0) AS cached,
  IFNULL(SUM(output), 0) AS output, IFNULL(SUM(reasoning), 0) AS reasoning
FROM j
GROUP BY 1, 2, 3
ORDER BY 1, 2, 3
LIMIT @limit"
    )
}

/// The parameters of [`usage_sql`]: `@from`, `@to` and `@limit`, one more than
/// [`MAX_USAGE_GROUPS`] so that too many groups are told apart from exactly the cap.
pub(crate) fn usage_params(q: &AlertQuery) -> Vec<Value> {
    vec![
        timestamp_param("from", q.from),
        timestamp_param("to", q.to),
        param("limit", "INT64", (MAX_USAGE_GROUPS + 1).to_string()),
    ]
}

/// `@prompt`, `@label`, `@from` and `@to` of `q`.
fn window_params(q: &GuardQuery) -> Vec<Value> {
    vec![
        param("prompt", "STRING", q.prompt.clone()),
        param("label", "STRING", q.label.clone()),
        timestamp_param("from", q.from),
        timestamp_param("to", q.to),
    ]
}

/// The parameters of [`version_calls_sql`]: the window and `@baseline` / `@rollout`.
pub(crate) fn call_params(q: &GuardQuery) -> Vec<Value> {
    let mut params = window_params(q);
    params.push(param("baseline", "INT64", q.baseline.to_string()));
    params.push(param("rollout", "INT64", q.rollout.to_string()));
    params
}

/// The parameters of [`rollout_start_sql`]: the window and `@rollout`.
pub(crate) fn start_params(q: &GuardQuery) -> Vec<Value> {
    let mut params = window_params(q);
    params.push(param("rollout", "INT64", q.rollout.to_string()));
    params
}

/// The parameters of [`version_feedback_sql`]: [`call_params`], `@name` and `@sources` (both
/// sources for [`FeedbackSource::Any`]).
pub(crate) fn feedback_params(q: &GuardQuery, name: &str, source: FeedbackSource) -> Vec<Value> {
    let sources: Vec<String> = match source {
        FeedbackSource::App => vec!["app".into()],
        FeedbackSource::Console => vec!["console".into()],
        FeedbackSource::Any => vec!["app".into(), "console".into()],
    };
    let mut params = call_params(q);
    params.push(param("name", "STRING", name.to_owned()));
    params.push(string_array_param("sources", &sources));
    params
}

/// The parameters of [`version_runs_sql`]: the window, `@version` and `@limit`.
pub(crate) fn run_params(q: &GuardQuery, version: u32, limit: usize) -> Vec<Value> {
    let mut params = window_params(q);
    params.push(param("version", "INT64", version.to_string()));
    params.push(param("limit", "INT64", limit.to_string()));
    params
}

/// An INT64 count cell (NULL or invalid → 0).
fn cell_count(row: &Map<String, Value>, name: &str) -> u64 {
    cell_str(row, name)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// A FLOAT64 cell (NULL or invalid → `None`).
fn cell_f64(row: &Map<String, Value>, name: &str) -> Option<f64> {
    cell_str(row, name).and_then(|s| s.parse().ok())
}

/// The `version` cell.
fn cell_version(row: &Map<String, Value>) -> Result<u32, MonitorError> {
    let raw = cell_str(row, "version").unwrap_or_default();
    raw.parse()
        .map_err(|_| MonitorError::Source(format!("invalid prompt version {raw:?} in `version`")))
}

/// The `first` cell of a [`rollout_start_sql`] reply: `None` without a row or with a NULL (or
/// missing) cell.
fn first_call(rows: &[Map<String, Value>]) -> Result<Option<DateTime<Utc>>, MonitorError> {
    match rows.first() {
        Some(row) if !matches!(row.get("first"), None | Some(Value::Null)) => {
            cell_timestamp(row, "first").map(Some)
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::super::tests::{ALL, HEAD, QUERIES, ROW, reply, row, table_reply};
    use super::super::{query_body, row_params, rows_sql};
    use super::*;
    use crate::gcp::NoAuth;
    use crate::testutil::{Exchange, at, exchange, replay};

    const TBL: &str = "`proj.analytics.agent_events`";

    /// Base time: 2025-11-17T03:26:40Z.
    const T: i64 = 1_763_350_000;

    /// A query whose prompt and label would break the SQL if they were spliced into it.
    fn guard_query() -> GuardQuery {
        GuardQuery::new(
            "tri'age`; DROP TABLE x; --",
            "prod\"uction",
            3,
            4,
            at(T),
            at(T + 86_400),
        )
    }

    fn guard(http: Arc<dyn HttpClient>) -> BigQueryGuardSource {
        BigQueryGuardSource::with_client(
            "proj",
            "analytics",
            "agent_events",
            http,
            Arc::new(NoAuth),
        )
        .unwrap()
    }

    /// A `jobs.query` of `sql` with `params` capped at `bytes`, answered with `response`.
    fn post(sql: &str, params: Vec<Value>, bytes: i64, response: &str) -> Exchange {
        let mut body = query_body(sql, params);
        body["maximumBytesBilled"] = json!(bytes.to_string());
        exchange("POST", QUERIES, &body.to_string(), 200, response)
    }

    /// The `@name` parameters `sql` reads.
    fn sql_params(sql: &str) -> BTreeSet<String> {
        sql.split('@')
            .skip(1)
            .map(|s| {
                s.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect()
            })
            .collect()
    }

    /// The names of `params`.
    fn param_names(params: &[Value]) -> BTreeSet<String> {
        params
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `sql` reads exactly `params`, and no user text of `q` is spliced into it.
    fn assert_parameterised(sql: &str, params: &[Value], q: &GuardQuery) {
        assert_eq!(sql_params(sql), param_names(params), "{sql}");
        for text in [&q.prompt, &q.label, "thumbs_up"] {
            assert!(!sql.contains(text), "{text} in {sql}");
        }
        assert!(!sql.contains("'tri"), "{sql}");
    }

    const CALL_FILTER: &str = "event_type = 'LLM_REQUEST'
    AND timestamp >= @from AND timestamp < @to
    AND JSON_VALUE(attributes, '$.prompt.name') = @prompt
    AND JSON_VALUE(attributes, '$.prompt.label') = @label
    AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IN (";

    #[test]
    fn calls_query_filters_the_guard_calls_and_joins_errors_and_responses() {
        let q = guard_query();
        let sql = version_calls_sql(TBL);
        let params = call_params(&q);
        assert_parameterised(&sql, &params, &q);
        assert_eq!(
            Value::Array(params),
            json!([
                {"name": "prompt", "parameterType": {"type": "STRING"},
                 "parameterValue": {"value": "tri'age`; DROP TABLE x; --"}},
                {"name": "label", "parameterType": {"type": "STRING"},
                 "parameterValue": {"value": "prod\"uction"}},
                {"name": "from", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-17 03:26:40.000000+00:00"}},
                {"name": "to", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-18 03:26:40.000000+00:00"}},
                {"name": "baseline", "parameterType": {"type": "INT64"},
                 "parameterValue": {"value": "3"}},
                {"name": "rollout", "parameterType": {"type": "INT64"},
                 "parameterValue": {"value": "4"}},
            ])
        );
        for part in [
            &format!("FROM {TBL}\n  WHERE {CALL_FILTER}@baseline, @rollout)),"),
            "WHERE event_type = 'LLM_ERROR'\n    \
             AND timestamp >= @from AND timestamp < TIMESTAMP_ADD(@to, INTERVAL 1 HOUR)),",
            "WHERE res.event_type = 'LLM_RESPONSE'\n    \
             AND res.timestamp >= @from AND res.timestamp < TIMESTAMP_ADD(@to, INTERVAL 1 HOUR)",
            "LEFT JOIN errors ON errors.trace_id = calls.trace_id AND errors.span_id = calls.span_id",
            "LEFT JOIN responses\n    \
             ON responses.trace_id = calls.trace_id AND responses.span_id = calls.span_id",
            "JSON_VALUE(attributes, '$.model') AS model",
            "COALESCE(NULLIF(calls.model, ''), NULLIF(responses.model_version, ''), '(unknown)')",
            "'$.usage_metadata.prompt_token_count'",
            "'$.usage_metadata.tool_use_prompt_token_count'",
            "'$.usage_metadata.cached_content_token_count'",
            "'$.usage_metadata.candidates_token_count'",
            "'$.usage_metadata.thoughts_token_count'",
            "APPROX_QUANTILES(total_ms, 100)[SAFE_OFFSET(95)] AS p95_ms",
            "COUNT(*) AS calls, COUNTIF(j.failed) AS errors",
        ] {
            assert!(sql.contains(part), "{part}\nnot in\n{sql}");
        }
        // The joined rows carry no prompt filter.
        assert_eq!(sql.matches("$.prompt.name").count(), 1, "{sql}");
    }

    #[test]
    fn feedback_query_keeps_the_latest_score_per_invocation_and_source() {
        let q = guard_query();
        let sql = version_feedback_sql(TBL, &ALL);
        let params = feedback_params(&q, "thumbs_up", FeedbackSource::Any);
        assert_parameterised(&sql, &params, &q);
        assert_eq!(
            params[6],
            json!({"name": "name", "parameterType": {"type": "STRING"},
                   "parameterValue": {"value": "thumbs_up"}})
        );
        let sources = |s| feedback_params(&q, "thumbs_up", s)[7]["parameterValue"].clone();
        assert_eq!(
            sources(FeedbackSource::Any),
            json!({"arrayValues": [{"value": "app"}, {"value": "console"}]})
        );
        assert_eq!(
            sources(FeedbackSource::App),
            json!({"arrayValues": [{"value": "app"}]})
        );
        assert_eq!(
            sources(FeedbackSource::Console),
            json!({"arrayValues": [{"value": "console"}]})
        );
        for part in [
            &format!(
                "FROM {TBL}\n  WHERE {CALL_FILTER}@baseline, @rollout)\n    \
                      AND invocation_id IS NOT NULL AND invocation_id != ''),"
            ),
            "WHERE event_type = 'FEEDBACK' AND timestamp >= @from\n",
            "AND JSON_VALUE(content, '$.name') = @name\n    \
             AND JSON_VALUE(attributes, '$.source') IN UNNEST(@sources)",
            "PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), \
             JSON_VALUE(attributes, '$.source')\n    \
             ORDER BY timestamp DESC, event_id DESC) = 1)",
            "COUNT(*) AS count, COUNTIF(s.value = 'true') AS positive",
            "AVG(IF(s.data_type = 'number', SAFE_CAST(s.value AS FLOAT64), NULL)) AS mean",
            "STDDEV_SAMP(IF(s.data_type = 'number', SAFE_CAST(s.value AS FLOAT64), NULL)) AS stddev",
            "FROM s JOIN calls ON calls.invocation_id = s.invocation_id\nGROUP BY 1, 2, 3",
        ] {
            assert!(sql.contains(part), "{part}\nnot in\n{sql}");
        }
        // FEEDBACK rows are bounded by `@from` only.
        let scores =
            &sql[sql.find("\ns AS (").unwrap()..sql.find("\nSELECT calls.version").unwrap()];
        assert!(!scores.contains("@to"), "{scores}");
        let old = version_feedback_sql(
            TBL,
            &Columns {
                event_id: false,
                ..ALL
            },
        );
        assert!(old.contains("ORDER BY timestamp DESC) = 1)"), "{old}");
        assert!(!old.contains("event_id"), "{old}");
    }

    #[test]
    fn runs_query_lists_the_newest_invocations_of_one_version() {
        let q = guard_query();
        let sql = version_runs_sql(TBL);
        let params = run_params(&q, 4, 25);
        assert_parameterised(&sql, &params, &q);
        assert_eq!(
            Value::Array(params[4..].to_vec()),
            json!([
                {"name": "version", "parameterType": {"type": "INT64"},
                 "parameterValue": {"value": "4"}},
                {"name": "limit", "parameterType": {"type": "INT64"},
                 "parameterValue": {"value": "25"}},
            ])
        );
        for part in [
            &format!("FROM {TBL}\n  WHERE {CALL_FILTER}@version)\n"),
            "GROUP BY invocation_id\n  ORDER BY last_call DESC\n  LIMIT @limit)",
            "SELECT inv.invocation_id AS invocation_id, MIN(inv.timestamp) AS started_at,\n  \
             ANY_VALUE(inv.session_id) AS session_id,\n  \
             ANY_VALUE(JSON_VALUE(inv.attributes, '$.root_agent_name')) AS agent,",
            "WHERE inv.event_type = 'INVOCATION_STARTING'\n  \
             AND inv.timestamp >= TIMESTAMP_SUB(@from, INTERVAL 1 DAY) AND inv.timestamp < @to",
            "ORDER BY last_call DESC, invocation_id",
        ] {
            assert!(sql.contains(part), "{part}\nnot in\n{sql}");
        }
    }

    const CALLS: [&str; 9] = [
        "version",
        "model",
        "calls",
        "errors",
        "input",
        "cached",
        "output",
        "reasoning",
        "p95_ms",
    ];

    const FEEDBACK: [&str; 7] = [
        "version",
        "source",
        "data_type",
        "count",
        "positive",
        "mean",
        "stddev",
    ];

    /// The exchanges of the three methods (with `version_runs(q, 4, 2)`), every query capped at
    /// `bytes`.
    fn every_query(bytes: i64) -> Vec<Exchange> {
        let q = guard_query();
        let ids = ["inv-b".to_owned(), "inv-a".to_owned()];
        vec![
            table_reply(&[]),
            post(
                &version_calls_sql(TBL),
                call_params(&q),
                bytes,
                &reply(
                    &CALLS,
                    &[
                        vec![
                            json!("3"),
                            json!("gemini-a"),
                            json!("100"),
                            json!("2"),
                            json!("5000"),
                            json!("1000"),
                            json!("700"),
                            json!("50"),
                            json!("812.5"),
                        ],
                        vec![
                            json!("3"),
                            json!("gemini-b"),
                            json!("20"),
                            json!("0"),
                            json!("900"),
                            json!("0"),
                            json!("100"),
                            json!("0"),
                            json!("812.5"),
                        ],
                        vec![
                            json!("4"),
                            json!("gemini-a"),
                            json!("3"),
                            json!("3"),
                            json!("0"),
                            json!("0"),
                            json!("0"),
                            json!("0"),
                            Value::Null,
                        ],
                    ],
                    None,
                ),
            ),
            post(
                &version_feedback_sql(TBL, &ALL),
                feedback_params(&q, "thumbs_up", FeedbackSource::Any),
                bytes,
                &reply(
                    &FEEDBACK,
                    &[
                        vec![
                            json!("3"),
                            json!("app"),
                            json!("bool"),
                            json!("40"),
                            json!("30"),
                            Value::Null,
                            Value::Null,
                        ],
                        vec![
                            json!("4"),
                            json!("console"),
                            json!("number"),
                            json!("5"),
                            json!("0"),
                            json!("4.2"),
                            json!("0.5"),
                        ],
                    ],
                    None,
                ),
            ),
            post(
                &version_runs_sql(TBL),
                run_params(&q, 4, 2),
                bytes,
                &reply(
                    &[
                        "invocation_id",
                        "started_at",
                        "session_id",
                        "agent",
                        "last_call",
                    ],
                    &[
                        vec![
                            json!("inv-b"),
                            json!(((T + 600) * 1_000_000).to_string()),
                            json!("s-2"),
                            json!("support"),
                            json!(((T + 601) * 1_000_000).to_string()),
                        ],
                        vec![
                            json!("inv-a"),
                            json!(((T - 300) * 1_000_000).to_string()),
                            json!("s-1"),
                            json!("support"),
                            json!(((T + 10) * 1_000_000).to_string()),
                        ],
                    ],
                    None,
                ),
            ),
            // The rows are read from 10 minutes before the earliest run's start.
            post(
                &rows_sql(TBL, &ALL),
                row_params(&ids, at(T - 300)),
                bytes,
                &reply(
                    &ROW,
                    &[
                        row(
                            "inv-a",
                            "USER_MESSAGE_RECEIVED",
                            (T - 300) * 1_000_000 + 1,
                            json!({"text_summary": "hi"}),
                            "tr-a",
                        ),
                        row(
                            "inv-b",
                            "USER_MESSAGE_RECEIVED",
                            (T + 600) * 1_000_000 + 1,
                            json!({"text_summary": "Where is order 7?"}),
                            "tr-b",
                        ),
                        row(
                            "inv-b",
                            "AGENT_RESPONSE",
                            (T + 600) * 1_000_000 + 2,
                            json!({"response": "text: 'It ships today.'"}),
                            "tr-b",
                        ),
                    ],
                    None,
                ),
            ),
        ]
    }

    #[tokio::test]
    async fn version_calls_sum_models_per_version() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &every_query(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED),
        );
        let calls = guard(http).version_calls(&guard_query()).await.unwrap();
        assert_eq!(
            calls,
            vec![
                VersionCalls::new(
                    3,
                    120,
                    2,
                    Some(812.5),
                    vec![
                        ModelTokens::new("gemini-a", 100, TokenUsage::new(5000, 1000, 700, 50)),
                        ModelTokens::new("gemini-b", 20, TokenUsage::new(900, 0, 100, 0)),
                    ],
                ),
                VersionCalls::new(
                    4,
                    3,
                    3,
                    None,
                    vec![ModelTokens::new("gemini-a", 3, TokenUsage::default())],
                ),
            ]
        );
    }

    #[tokio::test]
    async fn version_feedback_reads_one_row_per_version_source_and_type() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &every_query(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED),
        );
        let feedback = guard(http)
            .version_feedback(&guard_query(), "thumbs_up", FeedbackSource::Any)
            .await
            .unwrap();
        assert_eq!(
            feedback,
            vec![
                VersionFeedback::new(3, "app", "bool", 40, 30, None, None),
                VersionFeedback::new(4, "console", "number", 5, 0, Some(4.2), Some(0.5)),
            ]
        );
    }

    #[tokio::test]
    async fn version_runs_rebuild_transcripts_newest_call_first() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &every_query(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED),
        );
        let runs = guard(http)
            .version_runs(&guard_query(), 4, 2)
            .await
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.invocation_id.as_str()).collect();
        assert_eq!(ids, ["inv-b", "inv-a"]);
        assert_eq!(runs[0].session_id, "s-2");
        assert_eq!(runs[0].agent, "support");
        assert_eq!(runs[0].started_at, at(T + 600));
        assert_eq!(runs[0].trace_id.as_deref(), Some("tr-b"));
        assert_eq!(runs[0].transcript.turns[0].user, "Where is order 7?");
        assert_eq!(runs[0].transcript.turns[0].reply, "It ships today.");
        assert_eq!(runs[1].transcript.turns[0].user, "hi");
    }

    #[tokio::test]
    async fn every_query_carries_the_byte_cap() {
        for (bytes, configured) in [
            (BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED, None),
            (5000, Some(5000)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            // Replay matches the whole body: a query without this cap finds no answer.
            let http = replay(dir.path(), &every_query(bytes));
            let mut source = guard(http);
            if let Some(b) = configured {
                source = source.with_max_bytes_billed(b);
            }
            let q = guard_query();
            assert_eq!(source.version_calls(&q).await.unwrap().len(), 2, "{bytes}");
            assert_eq!(
                source
                    .version_feedback(&q, "thumbs_up", FeedbackSource::Any)
                    .await
                    .unwrap()
                    .len(),
                2,
                "{bytes}"
            );
            assert_eq!(
                source.version_runs(&q, 4, 2).await.unwrap().len(),
                2,
                "{bytes}"
            );
        }
    }

    #[tokio::test]
    async fn the_billing_project_runs_the_queries() {
        let q = guard_query();
        let mut body = query_body(&version_calls_sql(TBL), call_params(&q));
        body["maximumBytesBilled"] = json!("1073741824");
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange(
                    "POST",
                    "https://bigquery.googleapis.com/bigquery/v2/projects/bill/queries",
                    &body.to_string(),
                    200,
                    &reply(&CALLS, &[], None),
                ),
            ],
        );
        let source = guard(http).with_billing_project("bill");
        assert!(source.version_calls(&q).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn guards_need_the_attributes_column() {
        let q = guard_query();
        let want = "transcript source: guards need the `attributes` column, which \
                    `proj.analytics.agent_events` does not have (payload_column_denylist)";
        let dir = tempfile::tempdir().unwrap();
        let source = guard(replay(dir.path(), &[table_reply(&["attributes"])]));
        assert_eq!(
            source.version_calls(&q).await.unwrap_err().to_string(),
            want
        );
        assert_eq!(
            source
                .version_feedback(&q, "thumbs_up", FeedbackSource::App)
                .await
                .unwrap_err()
                .to_string(),
            want
        );
        assert_eq!(
            source
                .version_runs(&q, 3, 10)
                .await
                .unwrap_err()
                .to_string(),
            want
        );
        let dir = tempfile::tempdir().unwrap();
        let source = guard(replay(dir.path(), &[table_reply(&["content"])]));
        assert_eq!(
            source
                .version_feedback(&q, "thumbs_up", FeedbackSource::App)
                .await
                .unwrap_err()
                .to_string(),
            "transcript source: guards need the `content` column, which \
             `proj.analytics.agent_events` does not have (payload_column_denylist)"
        );
    }

    #[tokio::test]
    async fn no_runs_asked_or_found_send_no_transcript_query() {
        let dir = tempfile::tempdir().unwrap();
        let source = guard(replay(dir.path(), &[]));
        assert!(
            source
                .version_runs(&guard_query(), 4, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let q = guard_query();
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                post(
                    &version_runs_sql(TBL),
                    run_params(&q, 3, 10),
                    BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED,
                    &reply(&HEAD, &[], None),
                ),
            ],
        );
        assert!(
            guard(http)
                .version_runs(&q, 3, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rollout_start_query_takes_the_first_rollout_call() {
        let q = guard_query();
        let sql = rollout_start_sql(TBL);
        let params = start_params(&q);
        assert_parameterised(&sql, &params, &q);
        assert_eq!(
            sql,
            "SELECT MIN(timestamp) AS first
FROM `proj.analytics.agent_events`
WHERE event_type = 'LLM_REQUEST'
    AND timestamp >= @from AND timestamp < @to
    AND JSON_VALUE(attributes, '$.prompt.name') = @prompt
    AND JSON_VALUE(attributes, '$.prompt.label') = @label
    AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IN (@rollout)"
        );
        assert_eq!(
            Value::Array(params),
            json!([
                {"name": "prompt", "parameterType": {"type": "STRING"},
                 "parameterValue": {"value": "tri'age`; DROP TABLE x; --"}},
                {"name": "label", "parameterType": {"type": "STRING"},
                 "parameterValue": {"value": "prod\"uction"}},
                {"name": "from", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-17 03:26:40.000000+00:00"}},
                {"name": "to", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-18 03:26:40.000000+00:00"}},
                {"name": "rollout", "parameterType": {"type": "INT64"},
                 "parameterValue": {"value": "4"}},
            ])
        );
    }

    /// The table lookup and the `rollout_start` query capped at `bytes`, answered with the
    /// `first` cells `rows`.
    fn start_exchanges(bytes: i64, rows: &[Vec<Value>]) -> Vec<Exchange> {
        vec![
            table_reply(&[]),
            post(
                &rollout_start_sql(TBL),
                start_params(&guard_query()),
                bytes,
                &reply(&["first"], rows, None),
            ),
        ]
    }

    #[tokio::test]
    async fn rollout_start_reads_the_first_call_under_the_byte_cap() {
        let first = vec![vec![json!(((T + 120) * 1_000_000).to_string())]];
        for (bytes, configured) in [
            (BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED, None),
            (5000, Some(5000)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            // Replay matches the whole body: a query without this cap finds no answer.
            let mut source = guard(replay(dir.path(), &start_exchanges(bytes, &first)));
            if let Some(b) = configured {
                source = source.with_max_bytes_billed(b);
            }
            assert_eq!(
                source.rollout_start(&guard_query()).await.unwrap(),
                Some(at(T + 120)),
                "{bytes}"
            );
        }
    }

    #[tokio::test]
    async fn rollout_start_without_a_call_is_none() {
        let bytes = BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED;
        for rows in [vec![vec![Value::Null]], Vec::new()] {
            let dir = tempfile::tempdir().unwrap();
            let source = guard(replay(dir.path(), &start_exchanges(bytes, &rows)));
            assert_eq!(
                source.rollout_start(&guard_query()).await.unwrap(),
                None,
                "{rows:?}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let source = guard(replay(dir.path(), &[table_reply(&["attributes"])]));
        assert_eq!(
            source
                .rollout_start(&guard_query())
                .await
                .unwrap_err()
                .to_string(),
            "transcript source: guards need the `attributes` column, which \
             `proj.analytics.agent_events` does not have (payload_column_denylist)"
        );
    }

    #[test]
    fn first_call_is_none_without_a_row_or_a_value() {
        let cell =
            |v: Value| -> Map<String, Value> { [("first".to_owned(), v)].into_iter().collect() };
        assert_eq!(first_call(&[]).unwrap(), None);
        assert_eq!(first_call(&[cell(Value::Null)]).unwrap(), None);
        assert_eq!(first_call(&[Map::new()]).unwrap(), None);
        assert_eq!(
            first_call(&[cell(json!("1763350120000000"))]).unwrap(),
            Some(at(T + 120))
        );
        assert_eq!(
            first_call(&[cell(json!("soon"))]).unwrap_err().to_string(),
            "transcript source: invalid TIMESTAMP \"soon\" in `first`"
        );
    }

    #[test]
    fn an_invalid_version_cell_is_a_source_error() {
        let row: Map<String, Value> = [("version".to_owned(), json!("v3"))].into_iter().collect();
        assert_eq!(
            cell_version(&row).unwrap_err().to_string(),
            "transcript source: invalid prompt version \"v3\" in `version`"
        );
    }

    // ---- alerts ----

    /// The calls of the day from `T`.
    fn alert_query() -> AlertQuery {
        AlertQuery::new(at(T), at(T + 86_400))
    }

    /// `usage_sql(TBL)`, byte for byte.
    const USAGE_SQL: &str = r"WITH calls AS (
  SELECT trace_id, span_id, IFNULL(agent, '') AS agent,
    JSON_VALUE(attributes, '$.model') AS model,
    NULLIF(JSON_VALUE(attributes, '$.prompt.name'), '') AS prompt
  FROM `proj.analytics.agent_events`
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @from AND timestamp < @to),
errors AS (
  SELECT DISTINCT trace_id, span_id
  FROM `proj.analytics.agent_events`
  WHERE event_type = 'LLM_ERROR' AND timestamp >= @from),
responses AS (
  SELECT res.trace_id AS trace_id, res.span_id AS span_id,
    JSON_VALUE(res.attributes, '$.model_version') AS model_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning
  FROM `proj.analytics.agent_events` AS res
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @from
  QUALIFY ROW_NUMBER() OVER (PARTITION BY res.trace_id, res.span_id ORDER BY res.timestamp DESC) = 1),
j AS (
  SELECT COALESCE(NULLIF(calls.model, ''), NULLIF(responses.model_version, ''), '(unknown)') AS model,
    calls.agent AS agent, calls.prompt AS prompt,
    errors.trace_id IS NOT NULL AS failed,
    responses.input AS input, responses.cached AS cached, responses.output AS output,
    responses.reasoning AS reasoning
  FROM calls
  LEFT JOIN errors ON errors.trace_id = calls.trace_id AND errors.span_id = calls.span_id
  LEFT JOIN responses
    ON responses.trace_id = calls.trace_id AND responses.span_id = calls.span_id)
SELECT model, agent, prompt, COUNT(*) AS calls, COUNTIF(failed) AS errors,
  IFNULL(SUM(input), 0) AS input, IFNULL(SUM(cached), 0) AS cached,
  IFNULL(SUM(output), 0) AS output, IFNULL(SUM(reasoning), 0) AS reasoning
FROM j
GROUP BY 1, 2, 3
ORDER BY 1, 2, 3
LIMIT @limit";

    const USAGE: [&str; 9] = [
        "model",
        "agent",
        "prompt",
        "calls",
        "errors",
        "input",
        "cached",
        "output",
        "reasoning",
    ];

    /// A usage row: the model, agent and prompt cells, then `calls`, `errors` and the four
    /// token sums.
    fn usage_row(model: &str, agent: &str, prompt: Value, counts: [u64; 6]) -> Vec<Value> {
        let mut row = vec![json!(model), json!(agent), prompt];
        row.extend(counts.iter().map(|n| json!(n.to_string())));
        row
    }

    /// The exchanges of one `usage` call capped at `bytes` and answered with `rows`.
    fn usage_exchanges(bytes: i64, rows: &[Vec<Value>]) -> Vec<Exchange> {
        vec![
            table_reply(&[]),
            post(
                &usage_sql(TBL),
                usage_params(&alert_query()),
                bytes,
                &reply(&USAGE, rows, None),
            ),
        ]
    }

    #[test]
    fn usage_query_is_pinned_and_parameterised() {
        let sql = usage_sql(TBL);
        assert_eq!(sql, USAGE_SQL);
        let params = usage_params(&alert_query());
        assert_eq!(sql_params(&sql), param_names(&params));
        assert_eq!(
            Value::Array(params),
            json!([
                {"name": "from", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-17 03:26:40.000000+00:00"}},
                {"name": "to", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-18 03:26:40.000000+00:00"}},
                {"name": "limit", "parameterType": {"type": "INT64"},
                 "parameterValue": {"value": "5001"}},
            ])
        );
    }

    #[tokio::test]
    async fn usage_reads_one_group_per_row() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            usage_row("(unknown)", "", Value::Null, [2, 2, 0, 0, 0, 0]),
            usage_row(
                "gemini-a",
                "support",
                json!("support/answer"),
                [100, 3, 5000, 1000, 700, 50],
            ),
            // An empty prompt cell is no prompt, as `NULLIF` makes it.
            usage_row("gemini-b", "triage", json!(""), [7, 0, 900, 0, 100, 0]),
            // NULL token sums count 0.
            vec![
                json!("gemini-b"),
                json!("triage"),
                json!("triage/route"),
                json!("4"),
                json!("1"),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        ];
        let http = replay(
            dir.path(),
            &usage_exchanges(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED, &rows),
        );
        let groups = guard(http).usage(&alert_query()).await.unwrap();
        assert_eq!(
            groups,
            vec![
                UsageGroup::new("(unknown)", "", None, 2, 2, TokenUsage::default()),
                UsageGroup::new(
                    "gemini-a",
                    "support",
                    Some("support/answer".into()),
                    100,
                    3,
                    TokenUsage::new(5000, 1000, 700, 50),
                ),
                UsageGroup::new(
                    "gemini-b",
                    "triage",
                    None,
                    7,
                    0,
                    TokenUsage::new(900, 0, 100, 0),
                ),
                UsageGroup::new(
                    "gemini-b",
                    "triage",
                    Some("triage/route".into()),
                    4,
                    1,
                    TokenUsage::default(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn usage_carries_the_configured_byte_cap() {
        let dir = tempfile::tempdir().unwrap();
        // Replay matches the whole body: a query without this cap finds no answer.
        let http = replay(
            dir.path(),
            &usage_exchanges(
                5000,
                &[usage_row("m", "a", Value::Null, [1, 0, 0, 0, 0, 0])],
            ),
        );
        let source = guard(http).with_max_bytes_billed(5000);
        assert_eq!(source.usage(&alert_query()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn more_than_5000_groups_is_an_error() {
        let rows = |n: usize| -> Vec<Vec<Value>> {
            (0..n)
                .map(|i| usage_row(&format!("m{i:04}"), "a", Value::Null, [1, 0, 0, 0, 0, 0]))
                .collect()
        };
        let bytes = BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED;
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &usage_exchanges(bytes, &rows(MAX_USAGE_GROUPS)));
        assert_eq!(guard(http).usage(&alert_query()).await.unwrap().len(), 5000);
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &usage_exchanges(bytes, &rows(MAX_USAGE_GROUPS + 1)),
        );
        assert_eq!(
            guard(http)
                .usage(&alert_query())
                .await
                .unwrap_err()
                .to_string(),
            "transcript source: alert usage: more than 5000 model × agent × prompt groups"
        );
    }

    #[tokio::test]
    async fn alerts_need_the_attributes_column() {
        let dir = tempfile::tempdir().unwrap();
        let source = guard(replay(dir.path(), &[table_reply(&["attributes"])]));
        assert_eq!(
            source.usage(&alert_query()).await.unwrap_err().to_string(),
            "transcript source: alerts need the `attributes` column, which \
             `proj.analytics.agent_events` does not have (payload_column_denylist)"
        );
        // Without `content`, usage still runs.
        let dir = tempfile::tempdir().unwrap();
        let mut exchanges = usage_exchanges(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED, &[]);
        exchanges[0] = table_reply(&["content"]);
        let source = guard(replay(dir.path(), &exchanges));
        assert!(source.usage(&alert_query()).await.unwrap().is_empty());
    }
}
