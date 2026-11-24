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

//! The usage route: `/usage`.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use http::StatusCode;
use serde::Deserialize;

use super::AppState;
use super::pages::error::source_error_in;
use super::pages::layout::{Nav, html};
use super::pages::usage;
use crate::usage::{TOP_TRACES, UsageRange};

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/usage", get(usage_page))
}

/// `/usage` query parameters.
#[derive(Debug, Default, Deserialize)]
struct UsageParams {
    range: Option<String>,
}

/// An unknown or missing `range` is 24h. Without the BigQuery source the page is a note and
/// nothing is queried; otherwise the breakdown, top traces and trend queries run concurrently,
/// and a failed query is the error page with the source's hint.
async fn usage_page(State(state): State<AppState>, Query(p): Query<UsageParams>) -> Response {
    let range = p
        .range
        .as_deref()
        .and_then(UsageRange::parse)
        .unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use gadk_bigquery::{BigQueryClient, NoAuth};
    use http::StatusCode;
    use serde_json::{Value, json};

    use crate::prices::PriceTable;
    use crate::traces::BigQueryTable;
    use crate::traces::test_http::{FakeHttp, param_value, reply};
    use crate::usage::UsageSource;
    use crate::usage::tests::TREND_FIELDS;
    use crate::web::AppState;
    use crate::web::testutil::{FakeSource, get, state};

    const PRICES: &str =
        "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n";

    fn bq_state(fake: &Arc<FakeHttp>, prices: bool) -> AppState {
        let mut st = state(Arc::new(FakeSource::new()), None);
        st.usage = Some(Arc::new(UsageSource::with_client(
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
        let chart = r#"<h2>Over time</h2><svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label="cost per hour"><polyline points="8.0,10.0 33.4,100.0 58.8,100.0 84.2,100.0 109.6,100.0 135.0,100.0 160.3,100.0 185.7,100.0 211.1,100.0 236.5,100.0 261.9,100.0 287.3,100.0 312.7,100.0 338.1,100.0 363.5,100.0 388.9,100.0 414.3,100.0 439.7,100.0 465.0,100.0 490.4,100.0 515.8,100.0 541.2,100.0 566.6,100.0 592.0,35.9" fill="none"></polyline><text x="8" y="12" text-anchor="start">max $0.002175</text><text x="592" y="12" text-anchor="end">latest $0.001550</text><text x="8" y="116" text-anchor="start">12:00</text><text x="592" y="116" text-anchor="end">11:00</text></svg><p class="empty">Excludes calls to models without a price.</p><h2>By model</h2>"#;
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
            body.contains(r#"<h2>Over time</h2><p class="empty">Too many groups to chart for this range.</p><h2>By model</h2>"#),
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
}
