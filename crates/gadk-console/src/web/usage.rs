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
use crate::traces::TimeRange;
use crate::usage::TOP_TRACES;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/usage", get(usage_page))
}

/// `/usage` query parameters.
#[derive(Debug, Default, Deserialize)]
struct UsageParams {
    range: Option<String>,
}

/// An unknown or missing `range` is 24h. Without the BigQuery source the page is a note and
/// nothing is queried; a failed query is the error page with the source's hint.
async fn usage_page(State(state): State<AppState>, Query(p): Query<UsageParams>) -> Response {
    let range = p
        .range
        .as_deref()
        .and_then(TimeRange::parse)
        .unwrap_or(TimeRange::Hours24);
    let Some(source) = &state.usage else {
        return html(StatusCode::OK, usage::render_unavailable(range));
    };
    let end = (state.now)();
    let start = end - range.duration();
    let (breakdown, top) = match tokio::try_join!(
        source.breakdown(start, end),
        source.top_traces(start, end, TOP_TRACES),
    ) {
        Ok(both) => both,
        Err(e) => return source_error_in(Nav::Usage, &e),
    };
    html(
        StatusCode::OK,
        usage::render(range, &breakdown, &top, state.prices.as_deref()),
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

    #[tokio::test]
    async fn usage_shows_totals_breakdowns_and_top_traces_with_cost() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply()]);
        let (status, _, body) = get(bq_state(&fake, true), "/usage").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<a class="active" href="/usage">Usage</a>"#),
            "{body}"
        );
        for row in [
            // Totals.
            r#"<tr><td>All calls</td><td class="num">6</td><td class="num">4,500</td><td class="num">1,000</td><td class="num">1,100</td><td class="num">100</td><td class="num">$0.003725*</td></tr>"#,
            // By model.
            r#"<tr><td>gemini-3.8-flash</td><td class="num">4</td><td class="num">4,000</td><td class="num">1,000</td><td class="num">1,000</td><td class="num">100</td><td class="num">$0.003725</td></tr>"#,
            r#"<tr><td>other-model</td><td class="num">2</td><td class="num">500</td><td class="num">0</td><td class="num">100</td><td class="num">0</td><td class="num">—</td></tr>"#,
            // By agent.
            r#"<tr><td>triage</td><td class="num">4</td><td class="num">4,000</td><td class="num">1,000</td><td class="num">1,000</td><td class="num">100</td><td class="num">$0.003725</td></tr>"#,
            r#"<tr><td>writer</td><td class="num">2</td><td class="num">500</td><td class="num">0</td><td class="num">100</td><td class="num">0</td><td class="num">—</td></tr>"#,
            // By prompt.
            r#"<tr><td>(none)</td><td class="num">1</td><td class="num">1,000</td><td class="num">0</td><td class="num">400</td><td class="num">100</td><td class="num">$0.001550</td></tr>"#,
            r#"<tr><td>triage@2</td><td class="num">5</td><td class="num">3,500</td><td class="num">1,000</td><td class="num">700</td><td class="num">0</td><td class="num">$0.002175*</td></tr>"#,
            // Top traces.
            r#"<tr><td class="mono"><a href="/traces/0af7651916cd43dd8448eb211c80319c">0af7651916cd43dd8448eb211c80319c</a></td><td class="num">5,100</td><td class="num">4</td><td class="num">$0.003725</td></tr>"#,
            r#"<tr><td class="mono"><a href="/traces/11111111111111111111111111111111">11111111111111111111111111111111</a></td><td class="num">600</td><td class="num">2</td><td class="num">—</td></tr>"#,
        ] {
            assert!(body.contains(row), "{row}\n{body}");
        }
        for heading in ["Totals", "By model", "By agent", "By prompt", "Top traces"] {
            assert!(body.contains(&format!("<h2>{heading}</h2>")), "{heading}");
        }
        assert!(
            body.contains("* Costs exclude calls to models without a price."),
            "{body}"
        );
        // The default range is 24h.
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
        assert!(
            body.contains(r#"<option value="24h" selected>24h</option>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn usage_takes_the_range_and_shows_tokens_without_prices() {
        let fake = FakeHttp::new(vec![breakdown_reply(), top_reply()]);
        let (status, _, body) = get(bq_state(&fake, false), "/usage?range=7d").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("No price file"), "{body}");
        assert!(
            body.contains(r#"<tr><td>All calls</td><td class="num">6</td><td class="num">4,500</td><td class="num">1,000</td><td class="num">1,100</td><td class="num">100</td><td class="num">—</td></tr>"#),
            "{body}"
        );
        assert_eq!(
            param_value(&fake.body(0), "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
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
        assert_eq!(src.calls.load(Ordering::SeqCst), 0);
    }
}
