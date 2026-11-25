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

//! Token usage from the agent-analytics table: the model calls of a window grouped by model,
//! agent and prompt version ([`AnalyticsSource::breakdown`]), the same per time bucket
//! ([`AnalyticsSource::trend`]) and the traces with the most tokens
//! ([`AnalyticsSource::top_traces`]).
//!
//! Model calls are the `LLM_RESPONSE` rows in the window; each call's model is the
//! `attributes.model` of the `LLM_REQUEST` row with the same `trace_id` and `span_id`, else the
//! response's `attributes.model_version`, else `(unknown)`. Every query is parameterized, filters
//! `timestamp` to the window and stops at the byte cap. Tokens are grouped by model, so callers
//! can price them with [`crate::cost::cost_of`].
//!
//! ```
//! use std::sync::Arc;
//!
//! use chrono::{TimeZone, Utc};
//! use gadk_analytics::testutil::{FakeHttp, reply};
//! use gadk_analytics::{AnalyticsSource, BigQueryTable};
//! use gadk_bigquery::{BigQueryClient, NoAuth};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! // One scripted `jobs.query` reply: a single breakdown row.
//! let http = FakeHttp::new(vec![reply(
//!     &[
//!         ("model", "STRING"), ("agent", "STRING"), ("prompt", "STRING"), ("calls", "INT64"),
//!         ("input", "INT64"), ("cached", "INT64"), ("output", "INT64"), ("reasoning", "INT64"),
//!     ],
//!     vec![vec![
//!         "gemini-3.8-flash".into(), "triage".into(), "triage@2".into(), "3".into(),
//!         "3000".into(), "1000".into(), "600".into(), "0".into(),
//!     ]],
//!     false,
//! )]);
//! let source = AnalyticsSource::new(
//!     BigQueryClient::with_client(http.clone(), Arc::new(NoAuth)),
//!     "billing-proj",
//!     BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
//!     1_073_741_824,
//! );
//! let end = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
//! let breakdown = source.breakdown(end - chrono::TimeDelta::hours(24), end).await?;
//! let cell = &breakdown.cells[0];
//! assert_eq!(cell.calls.model, "gemini-3.8-flash");
//! assert_eq!((cell.calls.calls, cell.calls.usage.input), (3, 3000));
//! # Ok::<(), gadk_analytics::SourceError>(())
//! # }).unwrap();
//! ```

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::cost::{UNKNOWN_MODEL, Usage};
use crate::row::{param, text};
use crate::source::{count, window_params};
use crate::{AnalyticsSource, SourceError};

/// The most model × agent × prompt groups the breakdown query returns.
pub const BREAKDOWN_CAP: usize = 5_000;
/// Traces listed on `/usage`.
pub const TOP_TRACES: usize = 20;
/// The most trace × model rows the top-traces query returns.
const TOP_ROWS_CAP: u32 = 10_000;
/// The most bucket × model × agent × prompt rows the trend query returns.
pub const TREND_CAP: usize = 20_000;

/// The calls of one model and their summed tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ModelCalls {
    /// The model.
    pub model: String,
    /// How many calls.
    pub calls: u64,
    /// Their summed tokens.
    pub usage: Usage,
}

impl ModelCalls {
    /// The calls of `model`: `calls` calls with `usage` tokens in all.
    pub fn new(model: String, calls: u64, usage: Usage) -> Self {
        Self {
            model,
            calls,
            usage,
        }
    }
}

/// One breakdown group: the calls of one model by one agent with one prompt version.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Cell {
    /// The agent (empty when the rows have none).
    pub agent: String,
    /// `name@version`, or `None` for calls without `attributes.prompt`.
    pub prompt: Option<String>,
    /// The model and its calls.
    pub calls: ModelCalls,
}

impl Cell {
    /// The group of `agent` with `prompt` (`name@version`) and `calls`.
    pub fn new(agent: String, prompt: Option<String>, calls: ModelCalls) -> Self {
        Self {
            agent,
            prompt,
            calls,
        }
    }
}

/// The breakdown groups of a window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Breakdown {
    /// The groups, ordered by model, agent, prompt.
    pub cells: Vec<Cell>,
    /// More than [`BREAKDOWN_CAP`] groups exist.
    pub truncated: bool,
}

impl Breakdown {
    /// The groups `cells`; `truncated` when more groups exist.
    pub fn new(cells: Vec<Cell>, truncated: bool) -> Self {
        Self { cells, truncated }
    }
}

/// One trend group: the calls of one model by one agent with one prompt version in one bucket.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TrendCell {
    /// The bucket index, `0..count` from the window start.
    pub bucket: usize,
    /// The agent, prompt version, model and calls.
    pub cell: Cell,
}

impl TrendCell {
    /// The group `cell` in bucket `bucket`.
    pub fn new(bucket: usize, cell: Cell) -> Self {
        Self { bucket, cell }
    }
}

/// The trend groups of a window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Trend {
    /// The groups, ordered by bucket, model, agent, prompt.
    pub cells: Vec<TrendCell>,
    /// More than [`TREND_CAP`] groups exist.
    pub truncated: bool,
}

impl Trend {
    /// The groups `cells`; `truncated` when more groups exist.
    pub fn new(cells: Vec<TrendCell>, truncated: bool) -> Self {
        Self { cells, truncated }
    }
}

/// The `/usage` time window: the trace-list ranges plus 30 days, each cut into fixed buckets
/// for the trends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum UsageRange {
    /// The last hour.
    Hour1,
    /// The last 6 hours.
    Hours6,
    /// The last 24 hours (the default).
    #[default]
    Hours24,
    /// The last 7 days.
    Days7,
    /// The last 30 days.
    Days30,
}

/// Serializes as the query-string form ([`UsageRange::as_str`]): `"1h"`, `"6h"`, `"24h"`,
/// `"7d"` or `"30d"`.
impl Serialize for UsageRange {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl UsageRange {
    /// Every range, shortest first.
    pub const ALL: &'static [UsageRange] = &[
        UsageRange::Hour1,
        UsageRange::Hours6,
        UsageRange::Hours24,
        UsageRange::Days7,
        UsageRange::Days30,
    ];

    /// "1h" | "6h" | "24h" | "7d" | "30d"; anything else is None.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.as_str() == s)
    }

    /// The query-string form: "1h", "6h", "24h", "7d" or "30d".
    pub fn as_str(self) -> &'static str {
        match self {
            UsageRange::Hour1 => "1h",
            UsageRange::Hours6 => "6h",
            UsageRange::Hours24 => "24h",
            UsageRange::Days7 => "7d",
            UsageRange::Days30 => "30d",
        }
    }

    /// The window length.
    pub fn duration(self) -> TimeDelta {
        match self {
            UsageRange::Hour1 => TimeDelta::hours(1),
            UsageRange::Hours6 => TimeDelta::hours(6),
            UsageRange::Hours24 => TimeDelta::hours(24),
            UsageRange::Days7 => TimeDelta::days(7),
            UsageRange::Days30 => TimeDelta::days(30),
        }
    }

    /// The trend buckets: their width in seconds, how many tile the window, and the width as
    /// words (`cost per <label>`).
    pub fn bucket(self) -> (i64, usize, &'static str) {
        match self {
            UsageRange::Hour1 => (300, 12, "5 minutes"),
            UsageRange::Hours6 => (900, 24, "15 minutes"),
            UsageRange::Hours24 => (3600, 24, "hour"),
            UsageRange::Days7 => (21600, 28, "6 hours"),
            UsageRange::Days30 => (86400, 30, "day"),
        }
    }
}

/// One of the traces with the most tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TopTrace {
    /// The trace id.
    pub trace_id: String,
    /// Input + output + reasoning tokens of the trace's calls.
    pub total: u64,
    /// The trace's calls per model.
    pub models: Vec<ModelCalls>,
}

impl TopTrace {
    /// The trace `trace_id` with `total` tokens and its calls per model, `models`.
    pub fn new(trace_id: String, total: u64, models: Vec<ModelCalls>) -> Self {
        Self {
            trace_id,
            total,
            models,
        }
    }
}

impl AnalyticsSource {
    /// The model calls of `[start, end)` grouped by model, agent and prompt version.
    pub async fn breakdown(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Breakdown, SourceError> {
        let sql = breakdown_sql(&self.table.sql_path()?);
        let mut params = window_params(start, end);
        params.push(param(
            "limit",
            "INT64",
            Some((BREAKDOWN_CAP + 1).to_string()),
        ));
        let result = self.run(&sql, params, (BREAKDOWN_CAP + 1) as u32).await?;
        let truncated = result.rows.len() > BREAKDOWN_CAP || result.has_more;
        let cells = result
            .rows
            .iter()
            .take(BREAKDOWN_CAP)
            .map(|row| Cell {
                agent: text(row, "agent").unwrap_or_default(),
                prompt: text(row, "prompt"),
                calls: model_calls(row),
            })
            .collect();
        Ok(Breakdown { cells, truncated })
    }

    /// The `limit` traces of `[start, end)` with the most tokens, most first, with their calls
    /// per model.
    pub async fn top_traces(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TopTrace>, SourceError> {
        let sql = top_traces_sql(&self.table.sql_path()?);
        let mut params = window_params(start, end);
        params.push(param("limit", "INT64", Some(limit.to_string())));
        let result = self.run(&sql, params, TOP_ROWS_CAP).await?;
        let mut out: Vec<TopTrace> = Vec::new();
        for row in &result.rows {
            let Some(trace_id) = text(row, "trace_id") else {
                continue;
            };
            let calls = model_calls(row);
            match out.last_mut() {
                Some(last) if last.trace_id == trace_id => last.models.push(calls),
                _ => out.push(TopTrace {
                    trace_id,
                    total: count(row, "total"),
                    models: vec![calls],
                }),
            }
        }
        Ok(out)
    }

    /// The model calls of `[start, end)` grouped by `bucket_seconds`-wide bucket (aligned to
    /// `start`), model, agent and prompt version. Rows whose bucket falls outside the window are
    /// dropped.
    pub async fn trend(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        bucket_seconds: i64,
    ) -> Result<Trend, SourceError> {
        debug_assert!(bucket_seconds > 0, "bucket width {bucket_seconds}");
        let sql = trend_sql(&self.table.sql_path()?);
        let mut params = window_params(start, end);
        params.push(param("bucket", "INT64", Some(bucket_seconds.to_string())));
        let (rows, truncated) = self.run_capped(&sql, params, TREND_CAP).await?;
        let window = (end - start).num_seconds();
        let count = (window + bucket_seconds - 1) / bucket_seconds;
        let cells = rows
            .iter()
            .filter_map(|row| {
                let bucket = row.get("bucket").and_then(Value::as_i64)?;
                if !(0..count).contains(&bucket) {
                    return None;
                }
                Some(TrendCell {
                    bucket: usize::try_from(bucket).ok()?,
                    cell: Cell {
                        agent: text(row, "agent").unwrap_or_default(),
                        prompt: text(row, "prompt"),
                        calls: model_calls(row),
                    },
                })
            })
            .collect();
        Ok(Trend { cells, truncated })
    }
}

/// The `model`, `calls`, `input`, `cached`, `output` and `reasoning` cells of a row.
pub(crate) fn model_calls(row: &Map<String, Value>) -> ModelCalls {
    ModelCalls {
        model: text(row, "model").unwrap_or_else(|| UNKNOWN_MODEL.to_owned()),
        calls: count(row, "calls"),
        usage: Usage::new(
            count(row, "input"),
            count(row, "cached"),
            count(row, "output"),
            count(row, "reasoning"),
        ),
    }
}

/// The per-call columns of `c` read from the `res` (`LLM_RESPONSE`) row: prompt name and
/// version, the four token counts (missing values 0, all from `attributes.usage_metadata`, so the
/// `content` column is never read) and `latency_ms.total_ms`.
pub(crate) const CALL_COLUMNS: &str = r"JSON_VALUE(res.attributes, '$.prompt.name') AS prompt_name,
    SAFE_CAST(JSON_VALUE(res.attributes, '$.prompt.version') AS INT64) AS prompt_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms";

/// The `req` and `c` CTEs: `c` has one row per `LLM_RESPONSE` row in `[@start, @end)` with its
/// trace, agent, model and the [`CALL_COLUMNS`].
pub(crate) fn calls_cte(table: &str) -> String {
    calls_cte_builder(table, false)
}

/// [`calls_cte`], with `with_ts` also projecting the response timestamp as `ts`.
fn calls_cte_builder(table: &str, with_ts: bool) -> String {
    let ts = if with_ts {
        ",\n    res.timestamp AS ts"
    } else {
        ""
    };
    format!(
        r"WITH req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM {table}
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.trace_id AS trace_id, res.agent AS agent,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    {CALL_COLUMNS}{ts}
  FROM {table} AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end)"
    )
}

/// The `c` CTE without the model: [`calls_cte`] minus the `LLM_REQUEST` join, for queries that
/// do not group by model.
pub(crate) fn responses_cte(table: &str) -> String {
    format!(
        r"WITH c AS (
  SELECT res.trace_id AS trace_id, res.agent AS agent,
    {CALL_COLUMNS}
  FROM {table} AS res
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end)"
    )
}

/// Calls and tokens per model × agent × prompt (`name@version`, NULL without a prompt).
fn breakdown_sql(table: &str) -> String {
    let cte = calls_cte(table);
    format!(
        r"{cte}
SELECT model, IFNULL(agent, '') AS agent,
  IF(prompt_name IS NULL OR prompt_version IS NULL, NULL,
     CONCAT(prompt_name, '@', CAST(prompt_version AS STRING))) AS prompt,
  COUNT(*) AS calls, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
  SUM(reasoning) AS reasoning
FROM c
GROUP BY 1, 2, 3
ORDER BY 1, 2, 3
LIMIT @limit"
    )
}

/// Calls and tokens per `@bucket`-second bucket from `@start` × model × agent × prompt, with
/// the groups of [`breakdown_sql`].
fn trend_sql(table: &str) -> String {
    let cte = calls_cte_builder(table, true);
    format!(
        r"{cte}
SELECT DIV(TIMESTAMP_DIFF(ts, @start, SECOND), @bucket) AS bucket, model, IFNULL(agent, '') AS agent,
  IF(prompt_name IS NULL OR prompt_version IS NULL, NULL,
     CONCAT(prompt_name, '@', CAST(prompt_version AS STRING))) AS prompt,
  COUNT(*) AS calls, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
  SUM(reasoning) AS reasoning
FROM c
GROUP BY 1, 2, 3, 4
ORDER BY 1, 2, 3, 4
LIMIT @limit"
    )
}

/// The `@limit` traces with the most input + output + reasoning tokens, with calls and tokens
/// per model.
fn top_traces_sql(table: &str) -> String {
    let cte = calls_cte(table);
    format!(
        r"{cte},
ranked AS (
  SELECT trace_id, SUM(input + output + reasoning) AS total
  FROM c
  WHERE REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{{32}}$')
  GROUP BY trace_id
  ORDER BY total DESC, trace_id
  LIMIT @limit)
SELECT c.trace_id AS trace_id, ranked.total AS total, c.model AS model, COUNT(*) AS calls,
  SUM(c.input) AS input, SUM(c.cached) AS cached, SUM(c.output) AS output,
  SUM(c.reasoning) AS reasoning
FROM c JOIN ranked ON ranked.trace_id = c.trace_id
GROUP BY 1, 2, 3
ORDER BY 2 DESC, 1, 3"
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::BigQueryTable;
    use crate::testutil::{FakeHttp, TREND_FIELDS, param_value, reply};

    const QUERIES: &str =
        "https://bigquery.googleapis.com/bigquery/v2/projects/billing-proj/queries";

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap()
    }

    fn source(fake: &Arc<FakeHttp>) -> AnalyticsSource {
        AnalyticsSource::new(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )
    }

    const BREAKDOWN_FIELDS: [(&str, &str); 8] = [
        ("model", "STRING"),
        ("agent", "STRING"),
        ("prompt", "STRING"),
        ("calls", "INT64"),
        ("input", "INT64"),
        ("cached", "INT64"),
        ("output", "INT64"),
        ("reasoning", "INT64"),
    ];

    const TOP_FIELDS: [(&str, &str); 8] = [
        ("trace_id", "STRING"),
        ("total", "INT64"),
        ("model", "STRING"),
        ("calls", "INT64"),
        ("input", "INT64"),
        ("cached", "INT64"),
        ("output", "INT64"),
        ("reasoning", "INT64"),
    ];

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Usage {
        Usage::new(input, cached, output, reasoning)
    }

    #[tokio::test]
    async fn breakdown_groups_llm_responses_in_the_window_by_model_agent_and_prompt() {
        let fake = FakeHttp::new(vec![reply(
            &BREAKDOWN_FIELDS,
            vec![
                vec![
                    json!("gemini-3.8-flash"),
                    json!("triage"),
                    json!("triage@2"),
                    json!("3"),
                    json!("3000"),
                    json!("1000"),
                    json!("600"),
                    json!("0"),
                ],
                vec![
                    json!("other-model"),
                    json!(""),
                    Value::Null,
                    json!("1"),
                    json!("10"),
                    json!("0"),
                    json!("5"),
                    json!("0"),
                ],
            ],
            false,
        )]);
        let b = source(&fake).breakdown(start(), end()).await.unwrap();
        assert!(!b.truncated);
        assert_eq!(
            b.cells,
            vec![
                Cell {
                    agent: "triage".into(),
                    prompt: Some("triage@2".into()),
                    calls: ModelCalls {
                        model: "gemini-3.8-flash".into(),
                        calls: 3,
                        usage: usage(3000, 1000, 600, 0),
                    },
                },
                Cell {
                    agent: String::new(),
                    prompt: None,
                    calls: ModelCalls {
                        model: "other-model".into(),
                        calls: 1,
                        usage: usage(10, 0, 5, 0),
                    },
                },
            ]
        );

        assert_eq!(fake.uris(), [QUERIES]);
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["maxResults"], json!(5001));
        assert_eq!(body["parameterMode"], json!("NAMED"));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "FROM `data-proj.agent_analytics.agent_events` AS res",
            "WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end",
            "WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end",
            "LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id",
            "COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model",
            "IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)",
            "'$.usage_metadata.tool_use_prompt_token_count'",
            "'$.usage_metadata.cached_content_token_count'",
            "IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output",
            "'$.usage_metadata.thoughts_token_count'",
            "CONCAT(prompt_name, '@', CAST(prompt_version AS STRING))",
            "GROUP BY 1, 2, 3",
            "LIMIT @limit",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert!(
            !sql.contains("(content") && !sql.contains("res.content"),
            "{sql}"
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-19 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "end")["parameterValue"]["value"],
            json!("2026-11-20 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("5001")
        );
    }

    #[tokio::test]
    async fn breakdown_is_truncated_when_more_rows_exist() {
        let row = vec![
            json!("m"),
            json!("a"),
            Value::Null,
            json!("1"),
            json!("1"),
            json!("0"),
            json!("1"),
            json!("0"),
        ];
        let fake = FakeHttp::new(vec![reply(&BREAKDOWN_FIELDS, vec![row], true)]);
        let b = source(&fake).breakdown(start(), end()).await.unwrap();
        assert!(b.truncated);
        assert_eq!(b.cells.len(), 1);
    }

    #[tokio::test]
    async fn top_traces_group_model_rows_by_trace() {
        let t1 = "0af7651916cd43dd8448eb211c80319c";
        let t2 = "11111111111111111111111111111111";
        let fake = FakeHttp::new(vec![reply(
            &TOP_FIELDS,
            vec![
                vec![
                    json!(t1),
                    json!("5200"),
                    json!("gemini-3.8-flash"),
                    json!("4"),
                    json!("4000"),
                    json!("1000"),
                    json!("1000"),
                    json!("100"),
                ],
                vec![
                    json!(t1),
                    json!("5200"),
                    json!("other-model"),
                    json!("1"),
                    json!("90"),
                    json!("0"),
                    json!("10"),
                    json!("0"),
                ],
                vec![
                    json!(t2),
                    json!("600"),
                    json!("other-model"),
                    json!("2"),
                    json!("500"),
                    json!("0"),
                    json!("100"),
                    json!("0"),
                ],
            ],
            false,
        )]);
        let top = source(&fake).top_traces(start(), end(), 20).await.unwrap();
        assert_eq!(
            top,
            vec![
                TopTrace {
                    trace_id: t1.into(),
                    total: 5200,
                    models: vec![
                        ModelCalls {
                            model: "gemini-3.8-flash".into(),
                            calls: 4,
                            usage: usage(4000, 1000, 1000, 100),
                        },
                        ModelCalls {
                            model: "other-model".into(),
                            calls: 1,
                            usage: usage(90, 0, 10, 0),
                        },
                    ],
                },
                TopTrace {
                    trace_id: t2.into(),
                    total: 600,
                    models: vec![ModelCalls {
                        model: "other-model".into(),
                        calls: 2,
                        usage: usage(500, 0, 100, 0),
                    }],
                },
            ]
        );
        let body = fake.body(0);
        assert_eq!(body["maxResults"], json!(10000));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "ranked AS (",
            "SUM(input + output + reasoning) AS total",
            "WHERE REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{32}$')",
            "ORDER BY total DESC, trace_id",
            "FROM c JOIN ranked ON ranked.trace_id = c.trace_id",
            "ORDER BY 2 DESC, 1, 3",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("20")
        );
    }

    #[tokio::test]
    async fn query_failures_are_source_errors() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let err = source(&fake).breakdown(start(), end()).await.unwrap_err();
        assert_eq!(
            err,
            SourceError::bigquery(Some(403), Some("accessDenied".into()), "Access Denied")
        );
    }

    /// The table path the byte-identical SQL tests use.
    const TABLE: &str = "`data-proj.agent_analytics.agent_events`";

    /// `calls_cte(TABLE)`, pinned byte for byte.
    pub(crate) const CALLS_CTE: &str = r"WITH req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM `data-proj.agent_analytics.agent_events`
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.trace_id AS trace_id, res.agent AS agent,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    JSON_VALUE(res.attributes, '$.prompt.name') AS prompt_name,
    SAFE_CAST(JSON_VALUE(res.attributes, '$.prompt.version') AS INT64) AS prompt_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms
  FROM `data-proj.agent_analytics.agent_events` AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end)";

    #[test]
    fn calls_cte_is_pinned() {
        assert_eq!(calls_cte(TABLE), CALLS_CTE);
    }

    #[test]
    fn breakdown_sql_is_pinned() {
        assert_eq!(
            breakdown_sql(TABLE),
            format!(
                r"{CALLS_CTE}
SELECT model, IFNULL(agent, '') AS agent,
  IF(prompt_name IS NULL OR prompt_version IS NULL, NULL,
     CONCAT(prompt_name, '@', CAST(prompt_version AS STRING))) AS prompt,
  COUNT(*) AS calls, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
  SUM(reasoning) AS reasoning
FROM c
GROUP BY 1, 2, 3
ORDER BY 1, 2, 3
LIMIT @limit"
            )
        );
    }

    #[test]
    fn top_traces_sql_is_pinned() {
        assert_eq!(
            top_traces_sql(TABLE),
            format!(
                r"{CALLS_CTE},
ranked AS (
  SELECT trace_id, SUM(input + output + reasoning) AS total
  FROM c
  WHERE REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{{32}}$')
  GROUP BY trace_id
  ORDER BY total DESC, trace_id
  LIMIT @limit)
SELECT c.trace_id AS trace_id, ranked.total AS total, c.model AS model, COUNT(*) AS calls,
  SUM(c.input) AS input, SUM(c.cached) AS cached, SUM(c.output) AS output,
  SUM(c.reasoning) AS reasoning
FROM c JOIN ranked ON ranked.trace_id = c.trace_id
GROUP BY 1, 2, 3
ORDER BY 2 DESC, 1, 3"
            )
        );
    }

    #[test]
    fn usage_range_parses_its_five_ranges_in_order() {
        assert_eq!(
            UsageRange::ALL
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>(),
            ["1h", "6h", "24h", "7d", "30d"]
        );
        for &r in UsageRange::ALL {
            assert_eq!(UsageRange::parse(r.as_str()), Some(r));
        }
        for bogus in ["", "bogus", "30D", "1d", "2h"] {
            assert_eq!(UsageRange::parse(bogus), None, "{bogus:?}");
        }
        assert_eq!(UsageRange::default(), UsageRange::Hours24);
    }

    #[test]
    fn usage_range_buckets_tile_the_window() {
        assert_eq!(UsageRange::Hour1.bucket(), (300, 12, "5 minutes"));
        assert_eq!(UsageRange::Hours6.bucket(), (900, 24, "15 minutes"));
        assert_eq!(UsageRange::Hours24.bucket(), (3600, 24, "hour"));
        assert_eq!(UsageRange::Days7.bucket(), (21600, 28, "6 hours"));
        assert_eq!(UsageRange::Days30.bucket(), (86400, 30, "day"));
        assert_eq!(UsageRange::Days30.duration(), chrono::TimeDelta::days(30));
        for r in UsageRange::ALL {
            let (seconds, count, _) = r.bucket();
            assert_eq!(r.duration().num_seconds(), seconds * count as i64, "{r:?}");
        }
    }

    /// `trend_sql(TABLE)`, pinned byte for byte.
    const TREND_SQL: &str = r"WITH req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM `data-proj.agent_analytics.agent_events`
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.trace_id AS trace_id, res.agent AS agent,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    JSON_VALUE(res.attributes, '$.prompt.name') AS prompt_name,
    SAFE_CAST(JSON_VALUE(res.attributes, '$.prompt.version') AS INT64) AS prompt_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms,
    res.timestamp AS ts
  FROM `data-proj.agent_analytics.agent_events` AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end)
SELECT DIV(TIMESTAMP_DIFF(ts, @start, SECOND), @bucket) AS bucket, model, IFNULL(agent, '') AS agent,
  IF(prompt_name IS NULL OR prompt_version IS NULL, NULL,
     CONCAT(prompt_name, '@', CAST(prompt_version AS STRING))) AS prompt,
  COUNT(*) AS calls, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
  SUM(reasoning) AS reasoning
FROM c
GROUP BY 1, 2, 3, 4
ORDER BY 1, 2, 3, 4
LIMIT @limit";

    #[test]
    fn trend_sql_is_pinned() {
        assert_eq!(trend_sql(TABLE), TREND_SQL);
    }

    /// A trend row in bucket `bucket` for model `m`, agent `triage`, no prompt, one call.
    fn trend_row(bucket: Value, m: &str) -> Vec<Value> {
        vec![
            bucket,
            json!(m),
            json!("triage"),
            Value::Null,
            json!("1"),
            json!("10"),
            json!("0"),
            json!("5"),
            json!("0"),
        ]
    }

    #[tokio::test]
    async fn trend_reads_buckets_and_drops_rows_outside_the_window() {
        let fake = FakeHttp::new(vec![reply(
            &TREND_FIELDS,
            vec![
                vec![
                    json!("0"),
                    json!("gemini-3.8-flash"),
                    json!("triage"),
                    json!("triage@2"),
                    json!("3"),
                    json!("3000"),
                    json!("1000"),
                    json!("600"),
                    json!("0"),
                ],
                trend_row(json!("23"), "other-model"),
                trend_row(json!("-1"), "before"),
                trend_row(json!("24"), "after"),
                trend_row(Value::Null, "no-bucket"),
            ],
            false,
        )]);
        let t = source(&fake).trend(start(), end(), 3600).await.unwrap();
        assert_eq!(
            t,
            Trend {
                cells: vec![
                    TrendCell {
                        bucket: 0,
                        cell: Cell {
                            agent: "triage".into(),
                            prompt: Some("triage@2".into()),
                            calls: ModelCalls {
                                model: "gemini-3.8-flash".into(),
                                calls: 3,
                                usage: usage(3000, 1000, 600, 0),
                            },
                        },
                    },
                    TrendCell {
                        bucket: 23,
                        cell: Cell {
                            agent: "triage".into(),
                            prompt: None,
                            calls: ModelCalls {
                                model: "other-model".into(),
                                calls: 1,
                                usage: usage(10, 0, 5, 0),
                            },
                        },
                    },
                ],
                truncated: false,
            }
        );

        assert_eq!(fake.uris(), [QUERIES]);
        let body = fake.body(0);
        assert_eq!(
            body["query"].as_str().unwrap(),
            TREND_SQL,
            "the trend query"
        );
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["maxResults"], json!(20001));
        assert_eq!(body["parameterMode"], json!("NAMED"));
        assert_eq!(
            param_value(&body, "start"),
            json!({
                "name": "start",
                "parameterType": { "type": "TIMESTAMP" },
                "parameterValue": { "value": "2026-11-19 12:00:00.000000+00:00" }
            })
        );
        assert_eq!(
            param_value(&body, "end")["parameterValue"]["value"],
            json!("2026-11-20 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "bucket"),
            json!({
                "name": "bucket",
                "parameterType": { "type": "INT64" },
                "parameterValue": { "value": "3600" }
            })
        );
        assert_eq!(
            param_value(&body, "limit"),
            json!({
                "name": "limit",
                "parameterType": { "type": "INT64" },
                "parameterValue": { "value": "20001" }
            })
        );
        assert_eq!(body["queryParameters"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn trend_is_truncated_past_its_cap() {
        let rows = (0..=TREND_CAP)
            .map(|i| trend_row(json!((i % 24).to_string()), &format!("m{i}")))
            .collect();
        let fake = FakeHttp::new(vec![
            reply(&TREND_FIELDS, rows, false),
            reply(&TREND_FIELDS, vec![trend_row(json!("1"), "m")], true),
            reply(&TREND_FIELDS, vec![trend_row(json!("1"), "m")], false),
        ]);
        let src = source(&fake);
        let over = src.trend(start(), end(), 3600).await.unwrap();
        assert!(over.truncated);
        assert_eq!(over.cells.len(), TREND_CAP);
        assert_eq!(over.cells[TREND_CAP - 1].cell.calls.model, "m19999");
        let more = src.trend(start(), end(), 3600).await.unwrap();
        assert!(more.truncated, "a next page means more rows");
        assert_eq!(more.cells.len(), 1);
        let under = src.trend(start(), end(), 3600).await.unwrap();
        assert!(!under.truncated);
    }
}
