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

//! The production metrics of prompt versions: per-version calls, tokens and latency
//! ([`AnalyticsSource::version_metrics`]), the versions each invocation used
//! ([`AnalyticsSource::versions_for_invocations`]) and the scores of each version's calls
//! ([`AnalyticsSource::version_feedback`]).
//!
//! A call's prompt version is the `attributes.prompt` that `gadk-agentanalytics` records on its
//! `LLM_REQUEST` and `LLM_RESPONSE` rows. Every query is parameterized, filters `timestamp` to
//! the window and stops at the byte cap. Tokens are grouped by model, so callers can price them.
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
//! // One scripted `jobs.query` reply: two invocations, each on one version of `triage`.
//! let http = FakeHttp::new(vec![reply(
//!     &[("invocation_id", "STRING"), ("version", "INT64")],
//!     vec![vec!["inv-1".into(), "2".into()], vec!["inv-2".into(), "3".into()]],
//!     false,
//! )]);
//! let source = AnalyticsSource::new(
//!     BigQueryClient::with_client(http.clone(), Arc::new(NoAuth)),
//!     "billing-proj",
//!     BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
//!     1_073_741_824,
//! );
//! let end = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
//! let ids = ["inv-1".to_owned(), "inv-2".to_owned()];
//! let v = source
//!     .versions_for_invocations("triage", &ids, end - chrono::TimeDelta::days(7), end)
//!     .await?;
//! assert_eq!(v.pairs, [("inv-1".to_owned(), 2), ("inv-2".to_owned(), 3)]);
//! assert!(!v.truncated);
//! # Ok::<(), gadk_analytics::SourceError>(())
//! # }).unwrap();
//! ```

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::row::{param, text};
use crate::source::{count, string_array_param, version_of, window_params};
use crate::usage::{ModelCalls, calls_cte, model_calls, responses_cte};
use crate::{AnalyticsSource, SourceError};

/// The most versions the per-version queries return.
const VERSION_CAP: usize = 1_000;
/// The most version × model rows the cost query returns.
const VERSION_MODEL_CAP: usize = 5_000;
/// The most invocation × version rows the pass-rate mapping returns.
const MAPPING_CAP: usize = 20_000;
/// The most version × score × value rows the feedback query returns.
pub const FEEDBACK_CAP: usize = 5_000;

/// Calls, mean tokens and latency percentiles of one prompt version.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VersionStats {
    /// The prompt version.
    pub version: u32,
    /// LLM calls.
    pub calls: u64,
    /// Mean input tokens per call.
    pub mean_input: f64,
    /// Mean output tokens per call.
    pub mean_output: f64,
    /// Median `latency_ms.total_ms` (approximate).
    pub p50_ms: Option<f64>,
    /// 95th percentile of `latency_ms.total_ms` (approximate).
    pub p95_ms: Option<f64>,
}

impl VersionStats {
    /// The metrics of `version`: `calls` calls, mean tokens and latency percentiles.
    pub fn new(
        version: u32,
        calls: u64,
        mean_input: f64,
        mean_output: f64,
        p50_ms: Option<f64>,
        p95_ms: Option<f64>,
    ) -> Self {
        Self {
            version,
            calls,
            mean_input,
            mean_output,
            p50_ms,
            p95_ms,
        }
    }
}

/// The calls of one prompt version on one model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VersionModel {
    /// The prompt version.
    pub version: u32,
    /// The model and its calls.
    pub calls: ModelCalls,
}

impl VersionModel {
    /// The calls of `version` on one model.
    pub fn new(version: u32, calls: ModelCalls) -> Self {
        Self { version, calls }
    }
}

/// The per-version metrics of one prompt.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VersionMetrics {
    /// One entry per version, newest first.
    pub stats: Vec<VersionStats>,
    /// Calls and tokens per version and model, for the cost.
    pub models: Vec<VersionModel>,
    /// A query hit its row cap: some versions or version × model rows are left out.
    pub truncated: bool,
}

impl VersionMetrics {
    /// The metrics `stats` and per-model calls `models`; `truncated` when a query hit its row cap.
    pub fn new(stats: Vec<VersionStats>, models: Vec<VersionModel>, truncated: bool) -> Self {
        Self {
            stats,
            models,
            truncated,
        }
    }
}

/// The `(invocation id, version)` pairs of [`AnalyticsSource::versions_for_invocations`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct InvocationVersions {
    /// The pairs.
    pub pairs: Vec<(String, u32)>,
    /// The query hit its row cap: some pairs are left out.
    pub truncated: bool,
}

impl InvocationVersions {
    /// The `(invocation id, version)` `pairs`; `truncated` when the query hit its row cap.
    pub fn new(pairs: Vec<(String, u32)>, truncated: bool) -> Self {
        Self { pairs, truncated }
    }
}

/// The scores of one prompt version with one name, source and data type.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FeedbackRow {
    /// The prompt version.
    pub version: u32,
    /// The score name.
    pub name: String,
    /// `app` or `console` (empty when the rows have none).
    pub source: String,
    /// `bool`, `number` or `category` (empty when the rows have none).
    pub data_type: String,
    /// How many scores (the latest per invocation, name and source).
    pub count: u64,
    /// `bool` scores that are `true`.
    pub positive: u64,
    /// The mean of `number` scores.
    pub mean: Option<f64>,
    /// The most common `category` (ties: the lowest value).
    pub mode: Option<String>,
}

impl FeedbackRow {
    /// A feedback row with every field given.
    #[allow(clippy::too_many_arguments)] // one argument per field
    pub fn new(
        version: u32,
        name: String,
        source: String,
        data_type: String,
        count: u64,
        positive: u64,
        mean: Option<f64>,
        mode: Option<String>,
    ) -> Self {
        Self {
            version,
            name,
            source,
            data_type,
            count,
            positive,
            mean,
            mode,
        }
    }
}

/// The per-version feedback of one prompt.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VersionFeedback {
    /// One row per version, name, source and data type: newest version first, then by name,
    /// source and data type.
    pub rows: Vec<FeedbackRow>,
    /// The query hit its row cap: some scores are left out.
    pub truncated: bool,
}

impl VersionFeedback {
    /// The feedback `rows`; `truncated` when the query hit its row cap.
    pub fn new(rows: Vec<FeedbackRow>, truncated: bool) -> Self {
        Self { rows, truncated }
    }
}

impl AnalyticsSource {
    /// Per-version metrics of `prompt` in `[start, end)`: one query for counts, means and latency
    /// percentiles and one for tokens per version and model (the cost), run concurrently.
    pub async fn version_metrics(
        &self,
        prompt: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<VersionMetrics, SourceError> {
        let table = self.table_path()?;
        let params = || {
            let mut p = window_params(start, end);
            p.push(param("prompt", "STRING", Some(prompt.to_owned())));
            p
        };
        let stats_sql = version_stats_sql(&table);
        let models_sql = version_models_sql(&table);
        let ((stat_rows, stats_truncated), (model_rows, models_truncated)) = tokio::try_join!(
            self.run_capped(&stats_sql, params(), VERSION_CAP),
            self.run_capped(&models_sql, params(), VERSION_MODEL_CAP),
        )?;
        let stats = stat_rows
            .iter()
            .filter_map(|row| {
                Some(VersionStats {
                    version: version_of(row)?,
                    calls: count(row, "calls"),
                    mean_input: row.get("mean_input").and_then(Value::as_f64).unwrap_or(0.0),
                    mean_output: row
                        .get("mean_output")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    p50_ms: row.get("p50_ms").and_then(Value::as_f64),
                    p95_ms: row.get("p95_ms").and_then(Value::as_f64),
                })
            })
            .collect();
        let models = model_rows
            .iter()
            .filter_map(|row| {
                Some(VersionModel {
                    version: version_of(row)?,
                    calls: model_calls(row),
                })
            })
            .collect();
        Ok(VersionMetrics {
            stats,
            models,
            truncated: stats_truncated || models_truncated,
        })
    }

    /// The versions of `prompt` each invocation in `ids` used: `(invocation id, version)` pairs
    /// from its `LLM_REQUEST` rows, read in `[start, end)` widened by one day before and one hour
    /// after.
    pub async fn versions_for_invocations(
        &self,
        prompt: &str,
        ids: &[String],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<InvocationVersions, SourceError> {
        let sql = invocation_versions_sql(&self.table_path()?);
        let mut params = window_params(start, end);
        params.push(param("prompt", "STRING", Some(prompt.to_owned())));
        params.push(string_array_param("ids", ids));
        let (rows, truncated) = self.run_capped(&sql, params, MAPPING_CAP).await?;
        Ok(InvocationVersions {
            pairs: rows
                .iter()
                .filter_map(|row| Some((text(row, "invocation_id")?, version_of(row)?)))
                .collect(),
            truncated,
        })
    }

    /// The scores of the calls of prompt `prompt`: `FEEDBACK` rows written since `start` (no
    /// upper bound: scores arrive after the calls), the latest per invocation, name and source,
    /// mapped to versions through the `LLM_REQUEST` rows of `[start, end)` widened by one day
    /// before and one hour after (an invocation that used several versions counts for each).
    pub async fn version_feedback(
        &self,
        prompt: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<VersionFeedback, SourceError> {
        let sql = version_feedback_sql(&self.table_path()?);
        let mut params = window_params(start, end);
        params.push(param("prompt", "STRING", Some(prompt.to_owned())));
        let (rows, truncated) = self.run_capped(&sql, params, FEEDBACK_CAP).await?;
        Ok(VersionFeedback {
            rows: merge_feedback(&rows),
            truncated,
        })
    }
}

/// Calls, mean input and output tokens and approximate p50/p95 latency per version of
/// `@prompt`, newest version first.
fn version_stats_sql(table: &str) -> String {
    let cte = responses_cte(table);
    format!(
        r"{cte}
SELECT prompt_version AS version, COUNT(*) AS calls, AVG(input) AS mean_input,
  AVG(output) AS mean_output,
  APPROX_QUANTILES(total_ms, 100)[SAFE_OFFSET(50)] AS p50_ms,
  APPROX_QUANTILES(total_ms, 100)[SAFE_OFFSET(95)] AS p95_ms
FROM c
WHERE prompt_name = @prompt AND prompt_version IS NOT NULL
GROUP BY 1
ORDER BY 1 DESC
LIMIT @limit"
    )
}

/// Calls and tokens per version of `@prompt` and model.
fn version_models_sql(table: &str) -> String {
    let cte = calls_cte(table);
    format!(
        r"{cte}
SELECT prompt_version AS version, model, COUNT(*) AS calls, SUM(input) AS input,
  SUM(cached) AS cached, SUM(output) AS output, SUM(reasoning) AS reasoning
FROM c
WHERE prompt_name = @prompt AND prompt_version IS NOT NULL
GROUP BY 1, 2
ORDER BY 1 DESC, 2
LIMIT @limit"
    )
}

/// The `@prompt` versions recorded on the `LLM_REQUEST` rows of the invocations in `@ids`.
fn invocation_versions_sql(table: &str) -> String {
    format!(
        r"SELECT DISTINCT invocation_id,
  SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version
FROM {table}
WHERE event_type = 'LLM_REQUEST'
  AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
  AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)
  AND invocation_id IN UNNEST(@ids)
  AND JSON_VALUE(attributes, '$.prompt.name') = @prompt
  AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IS NOT NULL
LIMIT @limit"
    )
}

/// A [`FeedbackRow`] being merged: the sum of its numbers and its category counts.
struct Merging {
    row: FeedbackRow,
    total: f64,
    categories: Vec<(String, u64)>,
}

/// The rows of [`version_feedback_sql`] (one per version, name, source, data type and
/// category) merged into one per version, name, source and data type, in first-seen order.
fn merge_feedback(rows: &[Map<String, Value>]) -> Vec<FeedbackRow> {
    let mut out: Vec<Merging> = Vec::new();
    for row in rows {
        let (Some(version), Some(name)) = (version_of(row), text(row, "name")) else {
            continue;
        };
        let source = text(row, "source").unwrap_or_default();
        let data_type = text(row, "data_type").unwrap_or_default();
        let n = count(row, "count");
        let i = match out.iter().position(|m| {
            m.row.version == version
                && m.row.name == name
                && m.row.source == source
                && m.row.data_type == data_type
        }) {
            Some(i) => i,
            None => {
                out.push(Merging {
                    row: FeedbackRow {
                        version,
                        name,
                        source,
                        data_type,
                        count: 0,
                        positive: 0,
                        mean: None,
                        mode: None,
                    },
                    total: 0.0,
                    categories: Vec::new(),
                });
                out.len() - 1
            }
        };
        let m = &mut out[i];
        m.row.count += n;
        m.row.positive += count(row, "positive");
        m.total += row.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(category) = text(row, "category") {
            m.categories.push((category, n));
        }
    }
    out.into_iter()
        .map(|m| {
            let mut r = m.row;
            if r.data_type == "number" && r.count > 0 {
                r.mean = Some(m.total / r.count as f64);
            }
            r.mode = m
                .categories
                .into_iter()
                .reduce(|best, c| {
                    if c.1 > best.1 || (c.1 == best.1 && c.0 < best.0) {
                        c
                    } else {
                        best
                    }
                })
                .map(|(category, _)| category);
            r
        })
        .collect()
}

/// Scores of the calls of `@prompt`, per version, name, source, data type and category (NULL
/// unless `category`): count, `true` values and the sum of numbers. Only the latest score per
/// (invocation, name, source) counts, a NULL source counting as `''`.
fn version_feedback_sql(table: &str) -> String {
    format!(
        r"WITH s AS (
  SELECT invocation_id, JSON_VALUE(content, '$.name') AS name,
    JSON_VALUE(attributes, '$.source') AS source,
    JSON_VALUE(content, '$.data_type') AS data_type,
    JSON_VALUE(content, '$.value') AS value
  FROM {table}
  WHERE event_type = 'FEEDBACK' AND timestamp >= @start
    AND invocation_id IS NOT NULL AND invocation_id != ''
  QUALIFY ROW_NUMBER() OVER (
    PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), IFNULL(JSON_VALUE(attributes, '$.source'), '')
    ORDER BY timestamp DESC, event_id DESC) = 1),
v AS (
  SELECT DISTINCT invocation_id,
    SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version
  FROM {table}
  WHERE event_type = 'LLM_REQUEST'
    AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
    AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)
    AND JSON_VALUE(attributes, '$.prompt.name') = @prompt
    AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IS NOT NULL)
SELECT v.version AS version, s.name AS name, IFNULL(s.source, '') AS source,
  IFNULL(s.data_type, '') AS data_type,
  IF(s.data_type = 'category', s.value, NULL) AS category,
  COUNT(*) AS count, COUNTIF(s.value = 'true') AS positive,
  SUM(IF(s.data_type = 'number', SAFE_CAST(s.value AS FLOAT64), NULL)) AS total
FROM s JOIN v ON v.invocation_id = s.invocation_id
GROUP BY 1, 2, 3, 4, 5
ORDER BY 1 DESC, 2, 3, 4, 5
LIMIT @limit"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::BigQueryTable;
    use crate::cost::Usage;
    use crate::testutil::{FakeHttp, param_value, reply};
    use crate::usage::tests::CALLS_CTE;

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Usage {
        Usage::new(input, cached, output, reasoning)
    }

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

    #[tokio::test]
    async fn version_metrics_read_stats_then_cost_cells_for_the_prompt() {
        let stats = reply(
            &[
                ("version", "INT64"),
                ("calls", "INT64"),
                ("mean_input", "FLOAT64"),
                ("mean_output", "FLOAT64"),
                ("p50_ms", "FLOAT64"),
                ("p95_ms", "FLOAT64"),
            ],
            vec![vec![
                json!("2"),
                json!("4"),
                json!("1250"),
                json!("300.5"),
                json!("820"),
                Value::Null,
            ]],
            false,
        );
        let models = reply(
            &[
                ("version", "INT64"),
                ("model", "STRING"),
                ("calls", "INT64"),
                ("input", "INT64"),
                ("cached", "INT64"),
                ("output", "INT64"),
                ("reasoning", "INT64"),
            ],
            vec![vec![
                json!("2"),
                json!("gemini-3.8-flash"),
                json!("4"),
                json!("5000"),
                json!("1000"),
                json!("1202"),
                json!("0"),
            ]],
            false,
        );
        let fake = FakeHttp::new(vec![stats, models]);
        let m = source(&fake)
            .version_metrics("greet", start(), end())
            .await
            .unwrap();
        assert_eq!(
            m.stats,
            vec![VersionStats {
                version: 2,
                calls: 4,
                mean_input: 1250.0,
                mean_output: 300.5,
                p50_ms: Some(820.0),
                p95_ms: None,
            }]
        );
        assert_eq!(
            m.models,
            vec![VersionModel {
                version: 2,
                calls: ModelCalls::new("gemini-3.8-flash".into(), 4, usage(5000, 1000, 1202, 0)),
            }]
        );
        let first = fake.body(0);
        let sql = first["query"].as_str().unwrap();
        for piece in [
            "WHERE prompt_name = @prompt AND prompt_version IS NOT NULL",
            "AVG(input) AS mean_input",
            "APPROX_QUANTILES(total_ms, 100)[SAFE_OFFSET(50)] AS p50_ms",
            "APPROX_QUANTILES(total_ms, 100)[SAFE_OFFSET(95)] AS p95_ms",
            "SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms",
            "GROUP BY 1\nORDER BY 1 DESC",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert_eq!(
            param_value(&first, "prompt")["parameterValue"]["value"],
            json!("greet")
        );
        assert_eq!(first["maximumBytesBilled"], json!("1000"));
        assert!(!m.truncated);
        assert!(
            !sql.contains("LLM_REQUEST") && !sql.contains("req."),
            "the stats query does not join the requests: {sql}"
        );
        assert!(
            !sql.contains("(content") && !sql.contains("res.content"),
            "{sql}"
        );
        assert_eq!(
            param_value(&first, "limit")["parameterValue"]["value"],
            json!("1001")
        );
        let second = fake.body(1);
        let sql = second["query"].as_str().unwrap();
        assert!(sql.contains("GROUP BY 1, 2\nORDER BY 1 DESC, 2"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN req"),
            "the cost query needs the model: {sql}"
        );
        assert_eq!(
            param_value(&second, "limit")["parameterValue"]["value"],
            json!("5001")
        );
        assert_eq!(
            param_value(&second, "start")["parameterValue"]["value"],
            json!("2026-11-19 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn versions_for_invocations_maps_ids_with_an_array_parameter() {
        let fake = FakeHttp::new(vec![reply(
            &[("invocation_id", "STRING"), ("version", "INT64")],
            vec![
                vec![json!("inv-0"), json!("2")],
                vec![json!("inv-2"), json!("1")],
                vec![json!("inv-9"), Value::Null],
            ],
            false,
        )]);
        let ids = vec!["inv-0".to_owned(), "inv-2".to_owned()];
        let mapping = source(&fake)
            .versions_for_invocations("greet", &ids, start(), end())
            .await
            .unwrap();
        assert_eq!(
            mapping,
            InvocationVersions {
                pairs: vec![("inv-0".to_owned(), 2), ("inv-2".to_owned(), 1)],
                truncated: false,
            }
        );
        let body = fake.body(0);
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "WHERE event_type = 'LLM_REQUEST'",
            "AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)",
            "AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)",
            "AND invocation_id IN UNNEST(@ids)",
            "AND JSON_VALUE(attributes, '$.prompt.name') = @prompt",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert!(!sql.contains("inv-0"), "ids are a parameter: {sql}");
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("20001")
        );
        assert_eq!(
            param_value(&body, "ids"),
            json!({
                "name": "ids",
                "parameterType": { "type": "ARRAY", "arrayType": { "type": "STRING" } },
                "parameterValue": { "arrayValues": [{ "value": "inv-0" }, { "value": "inv-2" }] }
            })
        );
    }

    #[tokio::test]
    async fn version_metrics_and_mapping_report_truncation_at_their_caps() {
        let stats = reply(
            &[("version", "INT64"), ("calls", "INT64")],
            vec![vec![json!("2"), json!("4")]],
            true,
        );
        let models = reply(
            &[
                ("version", "INT64"),
                ("model", "STRING"),
                ("calls", "INT64"),
            ],
            vec![vec![json!("2"), json!("m"), json!("4")]],
            false,
        );
        let mapping = reply(
            &[("invocation_id", "STRING"), ("version", "INT64")],
            vec![vec![json!("inv-0"), json!("2")]],
            true,
        );
        let fake = FakeHttp::new(vec![stats, models, mapping]);
        let src = source(&fake);
        let m = src.version_metrics("greet", start(), end()).await.unwrap();
        assert!(m.truncated);
        assert_eq!(m.stats.len(), 1);
        let ids = vec!["inv-0".to_owned()];
        let v = src
            .versions_for_invocations("greet", &ids, start(), end())
            .await
            .unwrap();
        assert!(v.truncated);
        assert_eq!(v.pairs, vec![("inv-0".to_owned(), 2)]);
    }

    /// The table path the byte-identical SQL tests use.
    const TABLE: &str = "`data-proj.agent_analytics.agent_events`";

    #[test]
    fn version_models_sql_is_pinned() {
        assert_eq!(
            version_models_sql(TABLE),
            format!(
                r"{CALLS_CTE}
SELECT prompt_version AS version, model, COUNT(*) AS calls, SUM(input) AS input,
  SUM(cached) AS cached, SUM(output) AS output, SUM(reasoning) AS reasoning
FROM c
WHERE prompt_name = @prompt AND prompt_version IS NOT NULL
GROUP BY 1, 2
ORDER BY 1 DESC, 2
LIMIT @limit"
            )
        );
    }

    const FEEDBACK_FIELDS: [(&str, &str); 8] = [
        ("version", "INT64"),
        ("name", "STRING"),
        ("source", "STRING"),
        ("data_type", "STRING"),
        ("category", "STRING"),
        ("count", "INT64"),
        ("positive", "INT64"),
        ("total", "FLOAT64"),
    ];

    /// A category row of `FEEDBACK_FIELDS` for version 2 of score `quality` from the app.
    fn category(value: &str, count: &str) -> Vec<Value> {
        vec![
            json!("2"),
            json!("quality"),
            json!("app"),
            json!("category"),
            json!(value),
            json!(count),
            json!("0"),
            Value::Null,
        ]
    }

    /// A `bool` row of `FEEDBACK_FIELDS` for score `thumbs`.
    fn thumbs(version: &str, source: &str, count: &str, positive: &str) -> Vec<Value> {
        vec![
            json!(version),
            json!("thumbs"),
            json!(source),
            json!("bool"),
            Value::Null,
            json!(count),
            json!(positive),
            Value::Null,
        ]
    }

    #[tokio::test]
    async fn version_feedback_summarizes_each_version_name_and_source() {
        let rating = vec![
            json!("2"),
            json!("rating"),
            json!("console"),
            json!("number"),
            Value::Null,
            json!("2"),
            json!("0"),
            json!("7.5"),
        ];
        let fake = FakeHttp::new(vec![reply(
            &FEEDBACK_FIELDS,
            vec![
                category("clear", "2"),
                category("helpful", "2"),
                category("vague", "1"),
                rating,
                thumbs("2", "app", "3", "2"),
                thumbs("2", "console", "1", "0"),
                thumbs("1", "app", "1", "1"),
            ],
            false,
        )]);
        let start = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let f = source(&fake)
            .version_feedback("greet", start, end)
            .await
            .unwrap();
        assert!(!f.truncated);
        let row = |version, name: &str, source: &str, data_type: &str| FeedbackRow {
            version,
            name: name.into(),
            source: source.into(),
            data_type: data_type.into(),
            count: 0,
            positive: 0,
            mean: None,
            mode: None,
        };
        assert_eq!(
            f.rows,
            vec![
                FeedbackRow {
                    count: 5,
                    mode: Some("clear".into()),
                    ..row(2, "quality", "app", "category")
                },
                FeedbackRow {
                    count: 2,
                    mean: Some(3.75),
                    ..row(2, "rating", "console", "number")
                },
                FeedbackRow {
                    count: 3,
                    positive: 2,
                    ..row(2, "thumbs", "app", "bool")
                },
                FeedbackRow {
                    count: 1,
                    ..row(2, "thumbs", "console", "bool")
                },
                FeedbackRow {
                    count: 1,
                    positive: 1,
                    ..row(1, "thumbs", "app", "bool")
                },
            ]
        );
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "WHERE event_type = 'FEEDBACK' AND timestamp >= @start\n    AND invocation_id IS NOT NULL",
            "QUALIFY ROW_NUMBER() OVER (\n    PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), IFNULL(JSON_VALUE(attributes, '$.source'), '')\n    ORDER BY timestamp DESC, event_id DESC) = 1",
            "WHERE event_type = 'LLM_REQUEST'",
            "AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)",
            "AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)",
            "AND JSON_VALUE(attributes, '$.prompt.name') = @prompt",
            "FROM s JOIN v ON v.invocation_id = s.invocation_id",
            "GROUP BY 1, 2, 3, 4, 5",
            "LIMIT @limit",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert!(!sql.contains("greet"), "{sql}");
        assert_eq!(
            param_value(&body, "prompt")["parameterValue"]["value"],
            json!("greet")
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-13 12:00:00.000000+00:00")
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

    #[test]
    fn the_mode_breaks_ties_by_the_lowest_value_in_any_row_order() {
        // Decoded rows, as `gadk-bigquery` returns them: INT64 cells are numbers.
        let cells = |category: &str, count: u64| -> Map<String, Value> {
            let row = json!({
                "version": 1, "name": "q", "source": "app", "data_type": "category",
                "category": category, "count": count, "positive": 0, "total": null,
            });
            row.as_object().unwrap().clone()
        };
        let rows = merge_feedback(&[cells("zeta", 2), cells("alpha", 2), cells("mid", 1)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mode.as_deref(), Some("alpha"));
        assert_eq!(rows[0].count, 5);
        assert_eq!(rows[0].mean, None);
    }
}
