// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Port of Go `server/adkrest/controllers/devfeatures_test.go`, plus byte checks of the
//! dev-feature handlers against the ADK-Go wire fixtures `testdata/wire/main_{46,52}_*.json`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::body::Body;
use gadk_server::rest::controllers::{
    ErrorResponse, NOT_INSTALLED_DETAIL, agent_builder_config_handler, metrics_info_handler,
    new_not_implemented_handler,
};
use http::StatusCode;
use serde_json::{Map, Value};

type Response = axum::response::Response;

/// What gorilla/mux writes for a path it does not route; the dev-feature endpoints exist so a
/// client can tell "not implemented" apart from that.
const MUX_NOT_FOUND_BODY: &str = "404 page not found";

/// A bare GET of `target`.
fn get(target: &str) -> http::Request<Body> {
    http::Request::builder()
        .uri(target)
        .body(Body::empty())
        .unwrap()
}

/// Go `serve`: status, Content-Type and body of `resp`.
async fn serve(resp: Response) -> (StatusCode, String, String) {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        content_type,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn test_new_not_implemented_handler() {
    const FEATURE: &str = "the widget tab";
    const DETAIL: &str = "gadk has no widgets.";
    let h = new_not_implemented_handler(FEATURE, DETAIL);
    let (status, content_type, raw) = serve(h(get("/dev/apps/test-app/widgets")).await).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        content_type.contains("application/json"),
        "Content-Type = {content_type:?}"
    );
    assert!(
        !raw.contains(MUX_NOT_FOUND_BODY),
        "body {raw:?} is mux's unrouted-path text"
    );
    let got: ErrorResponse = serde_json::from_str(&raw).unwrap();
    assert!(!got.error.is_empty(), "error field is empty");
    assert!(
        got.error.contains(FEATURE),
        "error {:?} does not name {FEATURE:?}",
        got.error
    );
    assert_eq!(got.detail, DETAIL);
}

/// `detail` is omitted (Go `omitempty`) when empty.
#[tokio::test]
async fn test_not_implemented_handler_omits_empty_detail() {
    let h = new_not_implemented_handler("the widget tab", "");
    let (status, _, raw) = serve(h(get("/dev/apps/test-app/widgets")).await).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let body: Map<String, Value> = serde_json::from_str(&raw).unwrap();
    assert!(
        !body.contains_key("detail"),
        "body {raw:?} has a detail key"
    );
    assert!(body.contains_key("error"), "body {raw:?} has no error key");
}

/// The raw bytes: an empty list, never `null`.
#[tokio::test]
async fn test_metrics_info_handler() {
    let (status, _, raw) =
        serve(metrics_info_handler(get("/dev/apps/test-app/metrics-info")).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(raw.contains(r#""metricsInfo":[]"#), "raw body {raw:?}");
}

/// The empty document the UI reads as "nothing to edit here".
#[tokio::test]
async fn test_agent_builder_config_handler() {
    let (status, content_type, raw) =
        serve(agent_builder_config_handler(get("/dev/apps/test-app/builder")).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw, "");
    assert_eq!(content_type, "text/plain; charset=UTF-8");
}

// parity: adapted: the eval route table is crate-private (Go reads the exported
// routers.EvalAPIRouter); this checks the phrase against ADK-Go's own run-eval-family 501 body
// (wire fixture main_49) and the crate's devfeatures unit test checks the RunEval route's bytes
// against the same fixture.
#[tokio::test]
async fn test_eval_detail_reports_not_installed() {
    let path = format!(
        "{}/testdata/wire/main_49_eval_sets_list.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let f: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(f["response"]["status"], 501);
    let got: ErrorResponse = serde_json::from_str(f["response"]["body"].as_str().unwrap()).unwrap();
    assert!(
        got.detail.contains(NOT_INSTALLED_DETAIL),
        "run_eval detail {:?} does not contain {NOT_INSTALLED_DETAIL:?}",
        got.detail
    );
}

fn fixture(name: &str) -> (u16, String, String) {
    let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let f: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        f["response"]["headers"].as_array().unwrap().len(),
        1,
        "{name}"
    );
    (
        u16::try_from(f["response"]["status"].as_u64().unwrap()).unwrap(),
        f["response"]["headers"][0][1].as_str().unwrap().to_owned(),
        f["response"]["body"].as_str().unwrap().to_owned(),
    )
}

/// `main_46_builder_get` and `main_52_metrics_info`, byte for byte.
#[tokio::test]
async fn dev_feature_handlers_match_go_wire_fixtures() {
    let (status, ct, body) =
        serve(agent_builder_config_handler(get("/dev/apps/assistant/builder")).await).await;
    assert_eq!((status.as_u16(), ct, body), fixture("main_46_builder_get"));
    let (status, ct, body) =
        serve(metrics_info_handler(get("/dev/apps/assistant/metrics-info")).await).await;
    assert_eq!((status.as_u16(), ct, body), fixture("main_52_metrics_info"));
}
