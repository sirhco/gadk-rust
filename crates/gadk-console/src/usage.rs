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

//! Token usage from the agent-analytics BigQuery table: the `/usage` breakdowns.
//!
//! Model calls are the `LLM_RESPONSE` rows in the window; each call's model is the
//! `attributes.model` of the `LLM_REQUEST` row with the same `trace_id` and `span_id`, else the
//! response's `attributes.model_version`, else `(unknown)`. Every query is parameterized, filters
//! `timestamp` to the window and stops at the byte cap. Costs are computed by the pages, so
//! every result is grouped by model.

use chrono::{DateTime, Utc};
use gadk_bigquery::{BigQueryClient, QueryOptions, QueryResult};
use serde_json::{Map, Value};

use crate::prices::{UNKNOWN_MODEL, Usage};
use crate::traces::{BigQueryTable, SourceError, from_bigquery, param, text, ts_param};

/// The most model × agent × prompt groups the breakdown query returns.
pub(crate) const BREAKDOWN_CAP: usize = 5_000;
/// Traces listed on `/usage`.
pub(crate) const TOP_TRACES: usize = 20;
/// The most trace × model rows the top-traces query returns.
const TOP_ROWS_CAP: u32 = 10_000;

/// The calls of one model and their summed tokens.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelCalls {
    /// The model.
    pub(crate) model: String,
    /// How many calls.
    pub(crate) calls: u64,
    /// Their summed tokens.
    pub(crate) usage: Usage,
}

/// One breakdown group: the calls of one model by one agent with one prompt version.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cell {
    /// The agent (empty when the rows have none).
    pub(crate) agent: String,
    /// `name@version`, or `None` for calls without `attributes.prompt`.
    pub(crate) prompt: Option<String>,
    /// The model and its calls.
    pub(crate) calls: ModelCalls,
}

/// The breakdown groups of a window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Breakdown {
    /// The groups, ordered by model, agent, prompt.
    pub(crate) cells: Vec<Cell>,
    /// More than [`BREAKDOWN_CAP`] groups exist.
    pub(crate) truncated: bool,
}

/// One of the traces with the most tokens.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TopTrace {
    /// The trace id.
    pub(crate) trace_id: String,
    /// Input + output + reasoning tokens of the trace's calls.
    pub(crate) total: u64,
    /// The trace's calls per model.
    pub(crate) models: Vec<ModelCalls>,
}

/// Usage queries over the agent-analytics table, run (and billed) in the compute project.
pub(crate) struct UsageSource {
    client: BigQueryClient,
    compute_project: String,
    table: BigQueryTable,
    max_bytes_billed: i64,
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
            client,
            compute_project: compute_project.into(),
            table,
            max_bytes_billed,
        }
    }

    /// Runs `sql` with `params`, the byte cap and at most `max_rows` rows.
    async fn run(
        &self,
        sql: &str,
        params: Vec<Value>,
        max_rows: u32,
    ) -> Result<QueryResult, SourceError> {
        let opts = QueryOptions::new()
            .with_maximum_bytes_billed(self.max_bytes_billed)
            .with_query_parameters(params)
            .with_max_rows(max_rows);
        self.client
            .query(&self.compute_project, sql, &opts)
            .await
            .map_err(from_bigquery)
    }

    /// The model calls of `[start, end)` grouped by model, agent and prompt version.
    pub(crate) async fn breakdown(
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
    pub(crate) async fn top_traces(
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
}

/// The `@start` and `@end` TIMESTAMP parameters.
fn window_params(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<Value> {
    vec![
        param("start", "TIMESTAMP", Some(ts_param(start))),
        param("end", "TIMESTAMP", Some(ts_param(end))),
    ]
}

/// An INT64 cell as a count; 0 when NULL or not a non-negative integer.
fn count(row: &Map<String, Value>, key: &str) -> u64 {
    row.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The `model`, `calls`, `input`, `cached`, `output` and `reasoning` cells of a row.
fn model_calls(row: &Map<String, Value>) -> ModelCalls {
    ModelCalls {
        model: text(row, "model").unwrap_or_else(|| UNKNOWN_MODEL.to_owned()),
        calls: count(row, "calls"),
        usage: Usage {
            input: count(row, "input"),
            cached: count(row, "cached"),
            output: count(row, "output"),
            reasoning: count(row, "reasoning"),
        },
    }
}

/// The `req` and `c` CTEs: `c` has one row per `LLM_RESPONSE` row in `[@start, @end)` with its
/// trace, agent, model, prompt name and version, the four token counts (missing values 0) and
/// `latency_ms.total_ms`.
fn calls_cte(table: &str) -> String {
    format!(
        r"WITH req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM {table}
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.trace_id AS trace_id, res.agent AS agent,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    JSON_VALUE(res.attributes, '$.prompt.name') AS prompt_name,
    SAFE_CAST(JSON_VALUE(res.attributes, '$.prompt.version') AS INT64) AS prompt_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.content, '$.usage.prompt') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.content, '$.usage.completion') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms
  FROM {table} AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
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
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::traces::test_http::{FakeHttp, param_value, reply};

    const QUERIES: &str =
        "https://bigquery.googleapis.com/bigquery/v2/projects/billing-proj/queries";

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
        Usage {
            input,
            cached,
            output,
            reasoning,
        }
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
            "IFNULL(SAFE_CAST(JSON_VALUE(res.content, '$.usage.prompt') AS INT64), 0)",
            "'$.usage_metadata.tool_use_prompt_token_count'",
            "'$.usage_metadata.cached_content_token_count'",
            "IFNULL(SAFE_CAST(JSON_VALUE(res.content, '$.usage.completion') AS INT64), 0) AS output",
            "'$.usage_metadata.thoughts_token_count'",
            "CONCAT(prompt_name, '@', CAST(prompt_version AS STRING))",
            "GROUP BY 1, 2, 3",
            "LIMIT @limit",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
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
            SourceError::BigQuery {
                status: Some(403),
                reason: Some("accessDenied".into()),
                detail: "Access Denied".into()
            }
        );
    }
}
