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

//! The usage routes: `/usage` and `/usage.csv`.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use chrono::TimeDelta;
use gadk_analytics::cost::CostSum;
use gadk_analytics::usage::{TOP_TRACES, UsageRange};
use http::StatusCode;
use serde::Deserialize;

use super::AppState;
use super::pages::error::{error_page, source_error_in};
use super::pages::layout::{Nav, html};
use super::pages::usage::{self, NEEDS_BIGQUERY, Totals, agent_label, group, prompt_label};
use crate::csv::{Csv, Field, filename, response};
use crate::prices::cost_of;

/// The text of the 400 page for a missing or unknown `/usage.csv` table.
const BAD_TABLE: &str = "table: must be model, agent, prompt or trend";

/// The token and cost columns that end every `/usage.csv` row.
const TOTAL_COLUMNS: [&str; 7] = [
    "llm_calls",
    "input",
    "cached",
    "output",
    "reasoning",
    "cost_usd",
    "cost_partial",
];

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/usage", get(usage_page))
        .route("/usage.csv", get(usage_csv))
}

/// `/usage` query parameters.
#[derive(Debug, Default, Deserialize)]
struct UsageParams {
    range: Option<String>,
}

/// The `range` of `/usage`, `/usage.csv` and the usage API routes: an unknown or missing value
/// is 24h.
pub(crate) fn usage_range(raw: Option<&str>) -> UsageRange {
    raw.and_then(UsageRange::parse).unwrap_or_default()
}

/// An unknown or missing `range` is 24h. Without the BigQuery source the page is a note and
/// nothing is queried; otherwise the breakdown, top traces and trend queries run concurrently,
/// and a failed query is the error page with the source's hint.
async fn usage_page(State(state): State<AppState>, Query(p): Query<UsageParams>) -> Response {
    let range = usage_range(p.range.as_deref());
    let Some(source) = &state.usage else {
        return html(StatusCode::OK, usage::render_unavailable(range));
    };
    let end = (state.now)();
    let start = end - range.duration();
    let (bucket_seconds, _, _) = range.bucket();
    let (breakdown, top, trend) = match tokio::try_join!(
        source.breakdown(start, end),
        source.top_traces(start, end, TOP_TRACES),
        source.trend(start, end, bucket_seconds),
    ) {
        Ok(all) => all,
        Err(e) => return source_error_in(Nav::Usage, &e),
    };
    html(
        StatusCode::OK,
        usage::render(
            range,
            &breakdown,
            &top,
            &trend,
            state.prices.as_deref(),
            start,
        ),
    )
}

/// `/usage.csv` query parameters.
#[derive(Debug, Default, Deserialize)]
struct UsageCsvParams {
    range: Option<String>,
    table: Option<String>,
}

/// The calls, tokens and cost cells of `t`.
fn total_fields(t: &Totals) -> [Field; 6] {
    [
        Field::Int(t.calls),
        Field::Int(t.usage.input),
        Field::Int(t.usage.cached),
        Field::Int(t.usage.output),
        Field::Int(t.usage.reasoning),
        Field::Cost(t.cost),
    ]
}

/// The CSV of one `/usage` table: the header `key` plus [`TOTAL_COLUMNS`], then one row per
/// group in key order.
fn totals_csv(key: &str, rows: impl Iterator<Item = (String, Totals)>) -> String {
    let mut csv = Csv::new();
    let mut header = vec![key];
    header.extend(TOTAL_COLUMNS);
    csv.header(&header);
    for (label, t) in rows {
        let mut row = vec![Field::Text(label)];
        row.extend(total_fields(&t));
        csv.row(&row);
    }
    csv.finish()
}

/// `GET /usage.csv?range=<r>&table=<t>`: one `/usage` table as CSV. `model`, `agent` and
/// `prompt` read the breakdown query and sum it as the page does; `trend` reads the trend
/// query, one row per group and bucket. An unknown or missing `range` is 24h. Without the
/// BigQuery source it is a 404 page, an unknown `table` a 400 page and a failed query a 502
/// page, all under the Usage nav.
async fn usage_csv(State(state): State<AppState>, Query(p): Query<UsageCsvParams>) -> Response {
    let Some(source) = &state.usage else {
        return error_page(StatusCode::NOT_FOUND, Nav::Usage, NEEDS_BIGQUERY, None);
    };
    let table = match p.table.as_deref() {
        Some(t @ ("model" | "agent" | "prompt" | "trend")) => t,
        _ => return error_page(StatusCode::BAD_REQUEST, Nav::Usage, BAD_TABLE, None),
    };
    let range = usage_range(p.range.as_deref());
    let end = (state.now)();
    let start = end - range.duration();
    let prices = state.prices.as_deref();
    let (body, truncated) = if table == "trend" {
        let (bucket_seconds, _, _) = range.bucket();
        let trend = match source.trend(start, end, bucket_seconds).await {
            Ok(trend) => trend,
            Err(e) => return source_error_in(Nav::Usage, &e),
        };
        let mut csv = Csv::new();
        let mut header = vec!["bucket_start", "model", "agent", "prompt"];
        header.extend(TOTAL_COLUMNS);
        csv.header(&header);
        for c in &trend.cells {
            let calls = &c.cell.calls;
            let mut cost = CostSum::default();
            cost.add(cost_of(prices, &calls.model, &calls.usage));
            let t = Totals {
                calls: calls.calls,
                usage: calls.usage,
                cost,
            };
            let bucket_start = start + TimeDelta::seconds(c.bucket as i64 * bucket_seconds);
            let mut row = vec![
                Field::Time(bucket_start),
                Field::Text(calls.model.clone()),
                Field::Text(c.cell.agent.clone()),
                Field::Text(c.cell.prompt.clone().unwrap_or_default()),
            ];
            row.extend(total_fields(&t));
            csv.row(&row);
        }
        (csv.finish(), trend.truncated)
    } else {
        let breakdown = match source.breakdown(start, end).await {
            Ok(b) => b,
            Err(e) => return source_error_in(Nav::Usage, &e),
        };
        let g = group(&breakdown, prices);
        let body = match table {
            "model" => totals_csv("model", g.by_model.into_iter()),
            "agent" => totals_csv(
                "agent",
                g.by_agent.into_iter().map(|(k, t)| (agent_label(&k), t)),
            ),
            _ => totals_csv(
                "prompt",
                g.by_prompt
                    .into_iter()
                    .map(|(k, t)| (prompt_label(k.as_deref()), t)),
            ),
        };
        (body, breakdown.truncated)
    };
    let name = filename(&format!("usage-{table}"), range.as_str(), end, truncated);
    response(body, &name, truncated)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use gadk_bigquery::{BigQueryClient, NoAuth};
    use http::StatusCode;
    use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    use serde_json::{Value, json};

    use crate::prices::{PriceTable, thousands};
    use crate::traces::BigQueryTable;
    use crate::web::AppState;
    use crate::web::testutil::{FakeSource, get, state};
    use gadk_analytics::AnalyticsSource;
    use gadk_analytics::testutil::{FakeHttp, TREND_FIELDS, param_value, reply};

    const PRICES: &str =
        "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n";

    fn bq_state(fake: &Arc<FakeHttp>, prices: bool) -> AppState {
        let mut st = state(Arc::new(FakeSource::new()), None);
        st.usage = Some(Arc::new(AnalyticsSource::new(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )));
        if prices {
            st.prices = Some(Arc::new(PriceTable::from_yaml(PRICES).unwrap()));
        }
        st
    }

    fn breakdown_reply() -> Result<(u16, String), String> {
        let fields = [
            ("model", "STRING"),
            ("agent", "STRING"),
            ("prompt", "STRING"),
            ("calls", "INT64"),
            ("input", "INT64"),
            ("cached", "INT64"),
            ("output", "INT64"),
            ("reasoning", "INT64"),
        ];
        let row = |cells: [Value; 8]| cells.to_vec();
        reply(
            &fields,
            vec![
                row([
                    json!("gemini-3.8-flash"),
                    json!("triage"),
                    json!("triage@2"),
                    json!("3"),
                    json!("3000"),
                    json!("1000"),
                    json!("600"),
                    json!("0"),
                ]),
                row([
                    json!("gemini-3.8-flash"),
                    json!("triage"),
                    Value::Null,
                    json!("1"),
                    json!("1000"),
                    json!("0"),
                    json!("400"),
                    json!("100"),
                ]),
                row([
                    json!("other-model"),
                    json!("writer"),
                    json!("triage@2"),
                    json!("2"),
                    json!("500"),
                    json!("0"),
                    json!("100"),
                    json!("0"),
                ]),
            ],
            false,
        )
    }

    fn top_reply() -> Result<(u16, String), String> {
        let fields = [
            ("trace_id", "STRING"),
            ("total", "INT64"),
            ("model", "STRING"),
            ("calls", "INT64"),
            ("input", "INT64"),
            ("cached", "INT64"),
            ("output", "INT64"),
            ("reasoning", "INT64"),
        ];
        reply(
            &fields,
            vec![
                vec![
                    json!("0af7651916cd43dd8448eb211c80319c"),
                    json!("5100"),
                    json!("gemini-3.8-flash"),
                    json!("4"),
                    json!("4000"),
                    json!("1000"),
                    json!("1000"),
                    json!("100"),
                ],
                vec![
                    json!("11111111111111111111111111111111"),
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
        )
    }

    /// The trend rows of [`breakdown_reply`]'s calls: the first group in bucket 0, the other two
    /// in bucket 23.
    fn trend_reply(page_token: bool) -> Result<(u16, String), String> {
        let row = |cells: [Value; 9]| cells.to_vec();
        reply(
            &TREND_FIELDS,
            vec![
                row([
                    json!("0"),
                    json!("gemini-3.8-flash"),
                    json!("triage"),
                    json!("triage@2"),
                    json!("3"),
                    json!("3000"),
                    json!("1000"),
                    json!("600"),
                    json!("0"),
                ]),
                row([
                    json!("23"),
                    json!("gemini-3.8-flash"),
                    json!("triage"),
                    Value::Null,
                    json!("1"),
                    json!("1000"),
                    json!("0"),
                    json!("400"),
                    json!("100"),
                ]),
                row([
                    json!("23"),
                    json!("other-model"),
                    json!("writer"),
                    json!("triage@2"),
                    json!("2"),
                    json!("500"),
                    json!("0"),
                    json!("100"),
                    json!("0"),
                ]),
            ],
            page_token,
        )
    }

    /// A completed reply with `fields` and no rows.
    fn empty_reply(fields: &[(&str, &str)]) -> Result<(u16, String), String> {
        reply(fields, Vec::new(), false)
    }

    /// The start of a `Trend` cell's sparkline.
    const SPARK: &str = r#"<td><svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="usage trend"><polyline points=""#;

    /// A flat sparkline of 24 buckets.
    const FLAT_24: &str = r#"<td><svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="usage trend"><polyline points="4.0,30.0 8.9,30.0 13.7,30.0 18.6,30.0 23.5,30.0 28.3,30.0 33.2,30.0 38.1,30.0 43.0,30.0 47.8,30.0 52.7,30.0 57.6,30.0 62.4,30.0 67.3,30.0 72.2,30.0 77.0,30.0 81.9,30.0 86.8,30.0 91.7,30.0 96.5,30.0 101.4,30.0 106.3,30.0 111.1,30.0 116.0,30.0" fill="none"></polyline><circle cx="116.0" cy="30.0" r="2"></circle></svg></td></tr>"#;

    #[tokio::test]
    async fn usage_shows_totals_breakdowns_and_top_traces_with_cost() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(false)]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
            "{body}"
        );
        // Totals, with the whole sparkline: $0.002175 in bucket 0, $0.001550 in bucket 23.
        let all = r#"<tr><td>All calls</td><td class="num">6</td><td class="num">4,500</td><td class="num">1,000</td><td class="num">1,100</td><td class="num">100</td><td class="num">$0.003725*</td><td><svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="usage trend"><polyline points="4.0,2.0 8.9,30.0 13.7,30.0 18.6,30.0 23.5,30.0 28.3,30.0 33.2,30.0 38.1,30.0 43.0,30.0 47.8,30.0 52.7,30.0 57.6,30.0 62.4,30.0 67.3,30.0 72.2,30.0 77.0,30.0 81.9,30.0 86.8,30.0 91.7,30.0 96.5,30.0 101.4,30.0 106.3,30.0 111.1,30.0 116.0,10.0" fill="none"></polyline><circle cx="116.0" cy="10.0" r="2"></circle></svg></td></tr>"#;
        assert!(body.contains(all), "{all}\n{body}");
        // The unpriced model's calls cost nothing: a flat line.
        let other = format!(
            r#"<tr><td>other-model</td><td class="num">2</td><td class="num">500</td><td class="num">0</td><td class="num">100</td><td class="num">0</td><td class="num">—</td>{FLAT_24}"#
        );
        assert!(body.contains(&other), "{other}\n{body}");
        for row in [
            // By model.
            r#"<tr><td>gemini-3.8-flash</td><td class="num">4</td><td class="num">4,000</td><td class="num">1,000</td><td class="num">1,000</td><td class="num">100</td><td class="num">$0.003725</td>"#,
            // By agent.
            r#"<tr><td>triage</td><td class="num">4</td><td class="num">4,000</td><td class="num">1,000</td><td class="num">1,000</td><td class="num">100</td><td class="num">$0.003725</td>"#,
            r#"<tr><td>writer</td><td class="num">2</td><td class="num">500</td><td class="num">0</td><td class="num">100</td><td class="num">0</td><td class="num">—</td>"#,
            // By prompt.
            r#"<tr><td>(none)</td><td class="num">1</td><td class="num">1,000</td><td class="num">0</td><td class="num">400</td><td class="num">100</td><td class="num">$0.001550</td>"#,
            r#"<tr><td>triage@2</td><td class="num">5</td><td class="num">3,500</td><td class="num">1,000</td><td class="num">700</td><td class="num">0</td><td class="num">$0.002175*</td>"#,
        ] {
            let row = format!("{row}{SPARK}");
            assert!(body.contains(&row), "{row}\n{body}");
        }
        for row in [
            // Top traces: no trend.
            r#"<tr><td class="mono"><a href="/traces/0af7651916cd43dd8448eb211c80319c">0af7651916cd43dd8448eb211c80319c</a></td><td class="num">5,100</td><td class="num">4</td><td class="num">$0.003725</td></tr>"#,
            r#"<tr><td class="mono"><a href="/traces/11111111111111111111111111111111">11111111111111111111111111111111</a></td><td class="num">600</td><td class="num">2</td><td class="num">—</td></tr>"#,
        ] {
            assert!(body.contains(row), "{row}\n{body}");
        }
        for heading in [
            "Totals",
            "Over time",
            "By model",
            "By agent",
            "By prompt",
            "Top traces",
        ] {
            assert!(body.contains(&format!("<h2>{heading}</h2>")), "{heading}");
        }
        // Totals and three tables of two rows each.
        assert_eq!(body.matches("<th>Trend</th>").count(), 4, "{body}");
        assert_eq!(body.matches(r#"<svg class="trend""#).count(), 7, "{body}");
        let chart = r#"<h2>Over time</h2><p class="csv"><a href="/usage.csv?range=24h&amp;table=trend">Download CSV</a></p><svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label="cost per hour"><polyline points="8.0,10.0 33.4,100.0 58.8,100.0 84.2,100.0 109.6,100.0 135.0,100.0 160.3,100.0 185.7,100.0 211.1,100.0 236.5,100.0 261.9,100.0 287.3,100.0 312.7,100.0 338.1,100.0 363.5,100.0 388.9,100.0 414.3,100.0 439.7,100.0 465.0,100.0 490.4,100.0 515.8,100.0 541.2,100.0 566.6,100.0 592.0,35.9" fill="none"></polyline><text x="8" y="12" text-anchor="start">max $0.002175</text><text x="592" y="12" text-anchor="end">latest $0.001550</text><text x="8" y="116" text-anchor="start">12:00</text><text x="592" y="116" text-anchor="end">11:00</text></svg><p class="empty">Excludes calls to models without a price.</p><h2>By model</h2>"#;
        assert!(body.contains(chart), "{chart}\n{body}");
        assert!(
            body.contains("* Costs exclude calls to models without a price."),
            "{body}"
        );
        // The default range is 24h, in hour buckets.
        let breakdown = fake.body(0);
        assert_eq!(
            param_value(&breakdown, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&breakdown, "end")["parameterValue"]["value"],
            json!("2026-11-16 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&fake.body(1), "limit")["parameterValue"]["value"],
            json!("20")
        );
        let trend = fake.body(2);
        assert!(
            trend["query"].as_str().unwrap().contains("AS bucket"),
            "{trend}"
        );
        assert_eq!(
            param_value(&trend, "bucket")["parameterValue"]["value"],
            json!("3600")
        );
        assert_eq!(
            param_value(&trend, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
        assert!(
            body.contains(r#"<option value="24h" selected>24h</option>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn usage_takes_the_range_and_shows_tokens_without_prices() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(false)]);
        let (status, _, body) = get(bq_state(&fake, false), "/usage?range=7d").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("No price file"), "{body}");
        let all = format!(
            r#"<tr><td>All calls</td><td class="num">6</td><td class="num">4,500</td><td class="num">1,000</td><td class="num">1,100</td><td class="num">100</td><td class="num">—</td>{SPARK}"#
        );
        assert!(body.contains(&all), "{all}\n{body}");
        assert_eq!(body.matches(r#"<svg class="trend""#).count(), 7, "{body}");
        // 3,600 tokens in bucket 0, 2,100 in bucket 23 of 28 six-hour buckets; the last starts
        // 11-16 06:00.
        let chart = r#"<svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label="tokens per 6 hours"><polyline points="8.0,10.0 29.6,100.0 51.3,100.0 72.9,100.0 94.5,100.0 116.1,100.0 137.8,100.0 159.4,100.0 181.0,100.0 202.7,100.0 224.3,100.0 245.9,100.0 267.6,100.0 289.2,100.0 310.8,100.0 332.4,100.0 354.1,100.0 375.7,100.0 397.3,100.0 419.0,100.0 440.6,100.0 462.2,100.0 483.9,100.0 505.5,47.5 527.1,100.0 548.7,100.0 570.4,100.0 592.0,100.0" fill="none"></polyline><text x="8" y="12" text-anchor="start">max 3,600</text><text x="592" y="12" text-anchor="end">latest 0</text><text x="8" y="116" text-anchor="start">11-09</text><text x="592" y="116" text-anchor="end">11-16</text></svg><h2>By model</h2>"#;
        assert!(body.contains(chart), "{chart}\n{body}");
        assert!(
            !body.contains("Excludes calls to models without a price."),
            "no caption without prices: {body}"
        );
        assert_eq!(
            param_value(&fake.body(0), "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&fake.body(2), "bucket")["parameterValue"]["value"],
            json!("21600")
        );
    }

    #[tokio::test]
    async fn usage_takes_30_days_in_day_buckets() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(false)]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage?range=30d").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<option value="30d" selected>30d</option>"#),
            "{body}"
        );
        assert!(body.contains(r#"aria-label="cost per day""#), "{body}");
        assert!(
            body.contains(r#"<text x="8" y="116" text-anchor="start">10-17</text><text x="592" y="116" text-anchor="end">11-15</text>"#),
            "{body}"
        );
        for i in 0..3 {
            let req = fake.body(i);
            assert_eq!(
                param_value(&req, "start")["parameterValue"]["value"],
                json!("2026-10-17 12:00:00.000000+00:00"),
                "request {i}"
            );
            assert_eq!(
                param_value(&req, "end")["parameterValue"]["value"],
                json!("2026-11-16 12:00:00.000000+00:00"),
                "request {i}"
            );
        }
        assert_eq!(
            param_value(&fake.body(2), "bucket")["parameterValue"]["value"],
            json!("86400")
        );
    }

    #[tokio::test]
    async fn usage_with_an_unknown_range_is_24h() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(false)]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage?range=bogus").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<option value="24h" selected>24h</option>"#),
            "{body}"
        );
        assert_eq!(
            param_value(&fake.body(0), "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&fake.body(2), "bucket")["parameterValue"]["value"],
            json!("3600")
        );
    }

    #[tokio::test]
    async fn a_truncated_trend_drops_the_charts_but_keeps_the_tables() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(true)]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<h2>Over time</h2><p class="csv"><a href="/usage.csv?range=24h&amp;table=trend">Download CSV</a></p><p class="empty">Too many groups to chart for this range.</p><h2>By model</h2>"#),
            "{body}"
        );
        assert!(!body.contains("usage-chart"), "{body}");
        assert!(!body.contains(r#"<svg class="trend""#), "{body}");
        assert!(
            !body.contains("Excludes calls to models without a price."),
            "{body}"
        );
        assert_eq!(body.matches("<th>Trend</th>").count(), 4, "{body}");
        for row in [
            r#"<tr><td>All calls</td><td class="num">6</td><td class="num">4,500</td><td class="num">1,000</td><td class="num">1,100</td><td class="num">100</td><td class="num">$0.003725*</td><td></td></tr>"#,
            r#"<tr><td>gemini-3.8-flash</td><td class="num">4</td><td class="num">4,000</td><td class="num">1,000</td><td class="num">1,000</td><td class="num">100</td><td class="num">$0.003725</td><td></td></tr>"#,
            r#"<tr><td>writer</td><td class="num">2</td><td class="num">500</td><td class="num">0</td><td class="num">100</td><td class="num">0</td><td class="num">—</td><td></td></tr>"#,
            r#"<tr><td>(none)</td><td class="num">1</td><td class="num">1,000</td><td class="num">0</td><td class="num">400</td><td class="num">100</td><td class="num">$0.001550</td><td></td></tr>"#,
        ] {
            assert!(body.contains(row), "{row}\n{body}");
        }
    }

    #[tokio::test]
    async fn a_failed_trend_query_is_the_usage_error_page() {
        let fake = FakeHttp::new(vec![
            breakdown_reply(),
            top_reply(),
            Ok((
                403,
                json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
            )),
        ]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("BigQuery: HTTP 403: Access Denied"), "{body}");
        assert!(
            body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
            "{body}"
        );
        assert_eq!(fake.uris().len(), 3);
    }

    #[tokio::test]
    async fn no_calls_is_the_message_without_a_chart() {
        let fake = FakeHttp::new(vec![
            empty_reply(&[("model", "STRING"), ("calls", "INT64")]),
            empty_reply(&[("trace_id", "STRING"), ("total", "INT64")]),
            empty_reply(&[("bucket", "INT64"), ("model", "STRING")]),
        ]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage?range=1h").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("No model calls in the last 1h."), "{body}");
        assert!(!body.contains("Over time"), "{body}");
        assert!(!body.contains("usage-chart"), "{body}");
        assert!(!body.contains("<table"), "{body}");
        assert_eq!(
            param_value(&fake.body(2), "bucket")["parameterValue"]["value"],
            json!("300")
        );
    }

    #[tokio::test]
    async fn usage_failure_is_a_502_with_the_hint_under_the_usage_nav() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("BigQuery: HTTP 403: Access Denied"), "{body}");
        assert!(
            body.contains("needs roles/bigquery.dataViewer and roles/bigquery.jobUser"),
            "{body}"
        );
        assert!(
            body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn usage_with_the_cloud_trace_source_is_a_note_without_queries() {
        let src = Arc::new(FakeSource::new());
        let (status, _, body) = get(state(src.clone(), None), "/usage").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Usage needs the BigQuery trace source"),
            "{body}"
        );
        assert!(
            body.contains(r#"<select name="range"><option value="1h">1h</option><option value="6h">6h</option><option value="24h" selected>24h</option><option value="7d">7d</option><option value="30d">30d</option></select>"#),
            "{body}"
        );
        let (_, _, body) = get(state(src.clone(), None), "/usage?range=30d").await;
        assert!(
            body.contains(r#"<select name="range"><option value="1h">1h</option><option value="6h">6h</option><option value="24h">24h</option><option value="7d">7d</option><option value="30d" selected>30d</option></select>"#),
            "{body}"
        );
        assert_eq!(src.calls.load(Ordering::SeqCst), 0);
    }

    /// The `Content-Disposition` of the download `name`.
    fn attachment(name: &str) -> String {
        format!(r#"attachment; filename="{name}""#)
    }

    #[tokio::test]
    async fn usage_links_each_table_to_its_csv() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(false)]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage?range=7d").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for (heading, table) in [
            ("Over time", "trend"),
            ("By model", "model"),
            ("By agent", "agent"),
            ("By prompt", "prompt"),
        ] {
            let link = format!(
                r#"<h2>{heading}</h2><p class="csv"><a href="/usage.csv?range=7d&amp;table={table}">Download CSV</a></p>"#
            );
            assert!(body.contains(&link), "{link}\n{body}");
        }
        assert!(
            body.contains(r#"table=model">Download CSV</a></p><table>"#),
            "{body}"
        );
        assert_eq!(body.matches("Download CSV").count(), 4, "{body}");
        assert!(
            !body.contains(r#"<h2>Totals</h2><p class="csv">"#),
            "{body}"
        );
        assert!(
            !body.contains(r#"<h2>Top traces</h2><p class="csv">"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn no_links_without_calls_or_without_bigquery() {
        let fake = FakeHttp::new(vec![
            empty_reply(&[("model", "STRING"), ("calls", "INT64")]),
            empty_reply(&[("trace_id", "STRING"), ("total", "INT64")]),
            empty_reply(&[("bucket", "INT64"), ("model", "STRING")]),
        ]);
        let (_, _, body) = get(bq_state(&fake, true), "/usage").await;
        assert!(!body.contains("Download CSV"), "{body}");
        let (_, _, body) = get(state(Arc::new(FakeSource::new()), None), "/usage").await;
        assert!(!body.contains("Download CSV"), "{body}");
    }

    #[tokio::test]
    async fn usage_csv_by_model_agent_and_prompt_are_the_page_tables() {
        for (table, csv) in [
            (
                "model",
                "model,llm_calls,input,cached,output,reasoning,cost_usd,cost_partial\r\n\
                 gemini-3.8-flash,4,4000,1000,1000,100,0.003725,false\r\n\
                 other-model,2,500,0,100,0,,false\r\n",
            ),
            (
                "agent",
                "agent,llm_calls,input,cached,output,reasoning,cost_usd,cost_partial\r\n\
                 triage,4,4000,1000,1000,100,0.003725,false\r\n\
                 writer,2,500,0,100,0,,false\r\n",
            ),
            (
                "prompt",
                "prompt,llm_calls,input,cached,output,reasoning,cost_usd,cost_partial\r\n\
                 (none),1,1000,0,400,100,0.001550,false\r\n\
                 triage@2,5,3500,1000,700,0,0.002175,true\r\n",
            ),
        ] {
            let fake = FakeHttp::new(vec![breakdown_reply()]);
            let path = format!("/usage.csv?table={table}");
            let (status, headers, body) = get(bq_state(&fake, true), &path).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            assert_eq!(body, csv, "{path}");
            assert_eq!(headers[CONTENT_TYPE], "text/csv; charset=utf-8");
            assert_eq!(
                headers[CONTENT_DISPOSITION],
                attachment(&format!("gadk-usage-{table}-24h-20261116T1200Z.csv")).as_str()
            );
            assert!(headers.get("x-gadk-truncated").is_none(), "{path}");
            // Only the breakdown query runs, over the default 24h.
            assert_eq!(fake.uris().len(), 1, "{path}");
            let sent = fake.body(0);
            assert!(
                !sent["query"].as_str().unwrap().contains("AS bucket"),
                "{sent}"
            );
            assert_eq!(
                param_value(&sent, "start")["parameterValue"]["value"],
                json!("2026-11-15 12:00:00.000000+00:00")
            );
        }
    }

    #[tokio::test]
    async fn an_empty_agent_is_unknown_and_text_is_guarded() {
        let fields = [
            ("model", "STRING"),
            ("agent", "STRING"),
            ("prompt", "STRING"),
            ("calls", "INT64"),
            ("input", "INT64"),
            ("cached", "INT64"),
            ("output", "INT64"),
            ("reasoning", "INT64"),
        ];
        let rows = vec![vec![
            json!("=HYPERLINK(\"x\")"),
            json!(""),
            json!("p,1"),
            json!("1"),
            json!("10"),
            json!("0"),
            json!("5"),
            json!("0"),
        ]];
        for (table, row) in [
            ("model", "\"'=HYPERLINK(\"\"x\"\")\",1,10,0,5,0,,false\r\n"),
            ("agent", "(unknown),1,10,0,5,0,,false\r\n"),
            ("prompt", "\"p,1\",1,10,0,5,0,,false\r\n"),
        ] {
            let fake = FakeHttp::new(vec![reply(&fields, rows.clone(), false)]);
            let (status, _, body) =
                get(bq_state(&fake, false), &format!("/usage.csv?table={table}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert!(body.ends_with(&format!("\r\n{row}")), "{table}: {body}");
        }
    }

    #[tokio::test]
    async fn usage_csv_trend_has_one_row_per_group_and_bucket() {
        let fake = FakeHttp::new(vec![trend_reply(false)]);
        let (status, headers, body) =
            get(bq_state(&fake, true), "/usage.csv?table=trend&range=24h").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            "bucket_start,model,agent,prompt,llm_calls,input,cached,output,reasoning,cost_usd,cost_partial\r\n\
             2026-11-15T12:00:00.000Z,gemini-3.8-flash,triage,triage@2,3,3000,1000,600,0,0.002175,false\r\n\
             2026-11-16T11:00:00.000Z,gemini-3.8-flash,triage,,1,1000,0,400,100,0.001550,false\r\n\
             2026-11-16T11:00:00.000Z,other-model,writer,triage@2,2,500,0,100,0,,false\r\n"
        );
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-usage-trend-24h-20261116T1200Z.csv").as_str()
        );
        assert_eq!(fake.uris().len(), 1);
        let sent = fake.body(0);
        assert!(
            sent["query"].as_str().unwrap().contains("AS bucket"),
            "{sent}"
        );
        assert_eq!(
            param_value(&sent, "bucket")["parameterValue"]["value"],
            json!("3600")
        );
    }

    #[tokio::test]
    async fn usage_csv_takes_the_range_and_an_unknown_one_is_24h() {
        let fake = FakeHttp::new(vec![trend_reply(false)]);
        let (_, headers, body) =
            get(bq_state(&fake, false), "/usage.csv?range=7d&table=trend").await;
        // Bucket 23 of 6-hour buckets from 11-09 12:00 starts 11-15 06:00; no prices, no cost.
        assert!(
            body.ends_with(
                "\r\n2026-11-15T06:00:00.000Z,other-model,writer,triage@2,2,500,0,100,0,,false\r\n"
            ),
            "{body}"
        );
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-usage-trend-7d-20261116T1200Z.csv").as_str()
        );
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&sent, "bucket")["parameterValue"]["value"],
            json!("21600")
        );

        let fake = FakeHttp::new(vec![breakdown_reply()]);
        let (status, headers, _) =
            get(bq_state(&fake, false), "/usage.csv?range=bogus&table=model").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-usage-model-24h-20261116T1200Z.csv").as_str()
        );
        assert_eq!(
            param_value(&fake.body(0), "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn a_truncated_query_is_a_truncated_download() {
        let fake = FakeHttp::new(vec![trend_reply(true)]);
        let (status, headers, body) = get(bq_state(&fake, true), "/usage.csv?table=trend").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["x-gadk-truncated"], "true");
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-usage-trend-24h-20261116T1200Z-truncated.csv").as_str()
        );
        assert!(body.starts_with("bucket_start,"), "{body}");
        assert_eq!(body.matches("\r\n").count(), 4, "no comment rows: {body}");
    }

    #[tokio::test]
    async fn usage_csv_needs_a_known_table() {
        for path in [
            "/usage.csv",
            "/usage.csv?table=",
            "/usage.csv?table=bogus",
            "/usage.csv?range=7d&table=Model",
        ] {
            let fake = FakeHttp::new(vec![]);
            let (status, headers, body) = get(bq_state(&fake, true), path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
            assert!(
                body.contains("table: must be model, agent, prompt or trend"),
                "{path}: {body}"
            );
            assert!(
                body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
                "{body}"
            );
            assert!(fake.uris().is_empty(), "{path}");
        }
    }

    #[tokio::test]
    async fn usage_csv_without_bigquery_is_404_with_the_usage_note() {
        let src = Arc::new(FakeSource::new());
        let (status, _, body) = get(state(src.clone(), None), "/usage.csv?table=model").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body.contains("Usage needs the BigQuery trace source: start the console with --traces bigquery://&lt;project&gt;.&lt;dataset&gt;.&lt;table&gt;."),
            "{body}"
        );
        assert!(
            body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
            "{body}"
        );
        assert_eq!(src.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_failed_usage_csv_query_is_a_502_page() {
        for table in ["model", "trend"] {
            let fake = FakeHttp::new(vec![Ok((
                403,
                json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
            ))]);
            let (status, headers, body) =
                get(bq_state(&fake, true), &format!("/usage.csv?table={table}")).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{table}");
            assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
            assert!(body.contains("BigQuery: HTTP 403: Access Denied"), "{body}");
            assert!(
                body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn the_csv_totals_equal_the_page_totals() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply(), trend_reply(false)]);
        let (_, _, page) = get(bq_state(&fake, true), "/usage").await;
        for table in ["model", "agent", "prompt"] {
            let fake = FakeHttp::new(vec![breakdown_reply()]);
            let (_, _, csv) =
                get(bq_state(&fake, true), &format!("/usage.csv?table={table}")).await;
            let rows: Vec<&str> = csv
                .split("\r\n")
                .skip(1)
                .filter(|r| !r.is_empty())
                .collect();
            assert_eq!(rows.len(), 2, "{csv}");
            for row in rows {
                let cells: Vec<&str> = row.split(',').collect();
                let num = |i: usize| thousands(cells[i].parse().unwrap());
                let cost = match (cells[6], cells[7]) {
                    ("", _) => "—".to_owned(),
                    (usd, "true") => format!("${usd}*"),
                    (usd, _) => format!("${usd}"),
                };
                let tr = format!(
                    r#"<tr><td>{}</td><td class="num">{}</td><td class="num">{}</td><td class="num">{}</td><td class="num">{}</td><td class="num">{}</td><td class="num">{cost}</td>"#,
                    cells[0],
                    num(1),
                    num(2),
                    num(3),
                    num(4),
                    num(5),
                );
                assert!(page.contains(&tr), "{table}: {tr}\n{page}");
            }
        }
    }
}
