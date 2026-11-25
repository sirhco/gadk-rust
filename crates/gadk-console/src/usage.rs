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

//! The production metrics of prompt versions from the agent-analytics BigQuery table, and
//! [`UsageSource`]: the console's [`AnalyticsSource`] with the queries that are not in
//! gadk-analytics yet.
//!
//! Model calls are the `LLM_RESPONSE` rows in the window; each call's model is the
//! `attributes.model` of the `LLM_REQUEST` row with the same `trace_id` and `span_id`, else the
//! response's `attributes.model_version`, else `(unknown)`. Every query is parameterized, filters
//! `timestamp` to the window and stops at the byte cap. Costs are computed by the pages, so
//! every result is grouped by model.

use std::ops::Deref;

use chrono::{DateTime, Utc};
use gadk_analytics::AnalyticsSource;
use gadk_analytics::plumbing::{
    calls_cte, count, model_calls, responses_cte, string_array_param, version_of, window_params,
};
use gadk_analytics::row::{param, text};
use gadk_analytics::usage::ModelCalls;
use gadk_bigquery::BigQueryClient;
use serde_json::Value;

use crate::traces::{BigQueryTable, SourceError};

/// The most versions the per-version queries return.
const VERSION_CAP: usize = 1_000;
/// The most version × model rows the cost query returns.
const VERSION_MODEL_CAP: usize = 5_000;
/// The most invocation × version rows the pass-rate mapping returns.
const MAPPING_CAP: usize = 20_000;
/// Calls, mean tokens and latency percentiles of one prompt version.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionStats {
    /// The prompt version.
    pub(crate) version: u32,
    /// LLM calls.
    pub(crate) calls: u64,
    /// Mean input tokens per call.
    pub(crate) mean_input: f64,
    /// Mean output tokens per call.
    pub(crate) mean_output: f64,
    /// Median `latency_ms.total_ms` (approximate).
    pub(crate) p50_ms: Option<f64>,
    /// 95th percentile of `latency_ms.total_ms` (approximate).
    pub(crate) p95_ms: Option<f64>,
}

/// The calls of one prompt version on one model.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionModel {
    /// The prompt version.
    pub(crate) version: u32,
    /// The model and its calls.
    pub(crate) calls: ModelCalls,
}

/// The per-version metrics of one prompt.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionMetrics {
    /// One entry per version, newest first.
    pub(crate) stats: Vec<VersionStats>,
    /// Calls and tokens per version and model, for the cost.
    pub(crate) models: Vec<VersionModel>,
    /// A query hit its row cap: some versions or version × model rows are left out.
    pub(crate) truncated: bool,
}

/// The `(invocation id, version)` pairs of [`UsageSource::versions_for_invocations`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InvocationVersions {
    /// The pairs.
    pub(crate) pairs: Vec<(String, u32)>,
    /// The query hit its row cap: some pairs are left out.
    pub(crate) truncated: bool,
}

/// The console's query source: an [`AnalyticsSource`] (which it derefs to) plus the queries
/// that are still the console's.
pub(crate) struct UsageSource {
    inner: AnalyticsSource,
}

impl UsageSource {
    /// A source reading `table` with Application Default Credentials (built on the first call),
    /// each query capped at `max_bytes_billed` bytes.
    pub(crate) fn new(
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self::with_client(
            BigQueryClient::new(),
            compute_project,
            table,
            max_bytes_billed,
        )
    }

    /// [`new`](Self::new) with an explicit BigQuery client.
    pub(crate) fn with_client(
        client: BigQueryClient,
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self {
            inner: AnalyticsSource::new(client, compute_project, table, max_bytes_billed),
        }
    }
}

impl Deref for UsageSource {
    type Target = AnalyticsSource;

    fn deref(&self) -> &AnalyticsSource {
        &self.inner
    }
}

impl UsageSource {
    /// Per-version metrics of `prompt` in `[start, end)`: one query for counts, means and latency
    /// percentiles and one for tokens per version and model (the cost), run concurrently.
    pub(crate) async fn version_metrics(
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
    pub(crate) async fn versions_for_invocations(
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use gadk_analytics::cost::Usage;

    use super::*;
    use gadk_analytics::testutil::{FakeHttp, param_value, reply};

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Usage {
        Usage::new(input, cached, output, reasoning)
    }

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap()
    }

    fn source(fake: &Arc<FakeHttp>) -> UsageSource {
        UsageSource::with_client(
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

    /// `calls_cte(TABLE)`, pinned byte for byte.
    const CALLS_CTE: &str = r"WITH req AS (
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
}
