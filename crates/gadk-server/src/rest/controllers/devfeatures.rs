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

//! The developer endpoints the web UI calls but ADK servers do not implement (agent builder, tests
//! tab, eval): they answer 501 with a body naming the missing feature.

use axum::body::Body;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::rest::http_error::encode_json_response;
use crate::rest::routing::{DEV_PREFIX, Handler, Request, Response, Route, handler, route};

/// The marker the web UI looks for in an eval error.
pub const NOT_INSTALLED_DETAIL: &str = "not installed";

/// The `detail` of the agent builder's 501 body.
const BUILDER_DETAIL: &str =
    "gadk agents are defined in Rust code, so there is no server-side agent configuration to edit.";
/// The `detail` of the tests tab's 501 body.
const TESTS_DETAIL: &str =
    "gadk does not implement the web UI tests tab. Write Rust tests against the Runner instead.";

/// The `detail` of the eval endpoints' 501 body.
fn eval_detail() -> String {
    format!(
        "agent evaluation is {NOT_INSTALLED_DETAIL} in the gadk server. Use the gadk-eval crate or the gadk-eval CLI to run evals, and the gadk-console Evals pages to view results."
    )
}

/// A machine-readable error body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// What failed.
    pub error: String,
    /// What a caller can do about it; omitted from the JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// A handler answering 501 with an [`ErrorResponse`]: `error` names the missing `feature`,
/// `detail` is `detail`.
pub fn new_not_implemented_handler(feature: &str, detail: &str) -> Handler {
    let body = ErrorResponse {
        error: format!("{feature} is not implemented in the gadk server"),
        detail: detail.to_owned(),
    };
    handler(move |_req| {
        let resp = encode_json_response(&body, StatusCode::NOT_IMPLEMENTED);
        async move { resp }
    })
}

/// The metrics info response body.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsInfoResponse {
    /// The metrics info entries (`metricsInfo`).
    #[serde(rename = "metricsInfo")]
    pub metrics_info: Vec<serde_json::Value>,
}

/// Answers `{"metricsInfo":[]}`.
pub async fn metrics_info_handler(_req: Request) -> Response {
    encode_json_response(
        &MetricsInfoResponse {
            metrics_info: Vec::new(),
        },
        StatusCode::OK,
    )
}

/// Answers an empty `text/plain; charset=UTF-8` 200.
pub async fn agent_builder_config_handler(_req: Request) -> Response {
    let mut resp = Response::new(Body::empty());
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=UTF-8"),
    );
    resp
}

/// The agent builder routes.
pub(crate) fn agent_builder_routes() -> Vec<Route> {
    let not_implemented = new_not_implemented_handler("the agent builder", BUILDER_DETAIL);
    vec![
        route(
            "GetAgentBuilderConfig",
            &[Method::GET],
            format!("{DEV_PREFIX}/builder"),
            handler(agent_builder_config_handler),
        ),
        route(
            "SaveAgentBuilderConfig",
            &[Method::POST],
            format!("{DEV_PREFIX}/builder/save"),
            not_implemented.clone(),
        ),
        route(
            "CancelAgentBuilderChanges",
            &[Method::POST],
            format!("{DEV_PREFIX}/builder/cancel"),
            not_implemented,
        ),
    ]
}

/// The tests tab routes.
pub(crate) fn tests_routes() -> Vec<Route> {
    let not_implemented = new_not_implemented_handler("the tests tab", TESTS_DETAIL);
    [
        ("ListTests", Method::GET, "/tests"),
        ("RebuildTests", Method::POST, "/tests/rebuild"),
        ("RunTests", Method::POST, "/tests/run"),
        ("GetTest", Method::GET, "/tests/{test_name}"),
        ("CreateTest", Method::PUT, "/tests/{test_name}"),
        ("DeleteTest", Method::DELETE, "/tests/{test_name}"),
    ]
    .into_iter()
    .map(|(name, method, pattern)| {
        route(
            name,
            &[method],
            format!("{DEV_PREFIX}{pattern}"),
            not_implemented.clone(),
        )
    })
    .collect()
}

/// The eval routes.
pub(crate) fn eval_routes() -> Vec<Route> {
    let unavailable = new_not_implemented_handler("eval", &eval_detail());
    let mut routes = vec![route(
        "GetEvalMetricsInfo",
        &[Method::GET],
        format!("{DEV_PREFIX}/metrics-info"),
        handler(metrics_info_handler),
    )];
    let eval = [
        ("ListEvalSets", Method::GET, "/eval_sets"),
        ("CreateEvalSet", Method::POST, "/eval-sets"),
        ("CreateEvalSetUnderscore", Method::POST, "/eval_sets"),
        ("GetEvalSet", Method::GET, "/eval_sets/{eval_set_id}"),
        ("DeleteEvalSet", Method::DELETE, "/eval_sets/{eval_set_id}"),
        (
            "ListEvalCases",
            Method::GET,
            "/eval_sets/{eval_set_id}/evals",
        ),
        (
            "GetEvalCase",
            Method::GET,
            "/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        (
            "UpdateEvalCase",
            Method::PUT,
            "/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        (
            "DeleteEvalCase",
            Method::DELETE,
            "/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        (
            "AddSessionToEvalSet",
            Method::POST,
            "/eval_sets/{eval_set_id}/add_session",
        ),
        ("RunEval", Method::POST, "/eval_sets/{eval_set_id}/run_eval"),
        ("ListEvalResults", Method::GET, "/eval_results"),
        (
            "GetEvalResult",
            Method::GET,
            "/eval_results/{eval_result_id}",
        ),
    ];
    routes.extend(eval.into_iter().map(|(name, method, pattern)| {
        route(
            name,
            &[method],
            format!("{DEV_PREFIX}{pattern}"),
            unavailable.clone(),
        )
    }));
    let legacy = [
        (
            "ListEvalSetsLegacyPath",
            Method::GET,
            "/apps/{app_name}/eval_sets",
        ),
        (
            "ListEvalResultsLegacyPath",
            Method::GET,
            "/apps/{app_name}/eval_results",
        ),
        (
            "CreateEvalSetLegacyPath",
            Method::POST,
            "/apps/{app_name}/eval_sets/{eval_set_name}",
        ),
    ];
    routes.extend(
        legacy
            .into_iter()
            .map(|(name, method, pattern)| route(name, &[method], pattern, unavailable.clone())),
    );
    routes
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Go `NewNotImplementedHandler("eval", evalDetail)` on an `httptest.ResponseRecorder`.
    #[tokio::test]
    async fn not_implemented_handler_matches_go() {
        let h = new_not_implemented_handler("eval", &eval_detail());
        let resp = h(Request::new(axum::body::Body::empty())).await;
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            resp.headers()["content-type"],
            "application/json; charset=UTF-8"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            body.as_ref(),
            &b"{\"error\":\"eval is not implemented in the gadk server\",\"detail\":\"agent evaluation is not installed in the gadk server. Use the gadk-eval crate or the gadk-eval CLI to run evals, and the gadk-console Evals pages to view results.\"}\n"[..]
        );
    }

    /// Every builder, tests and eval 501 route the probe hit (`main_47` … `main_51`) answers
    /// ADK-Go's bytes; `RunEval` shares the eval handler, so its detail carries
    /// [`NOT_INSTALLED_DETAIL`] (Go `TestEvalDetailReportsNotInstalled` reads that route).
    #[tokio::test]
    async fn not_implemented_routes_match_go_wire_fixtures() {
        let mut routes = agent_builder_routes();
        routes.extend(tests_routes());
        routes.extend(eval_routes());
        let by_name = |name: &str| {
            routes
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.handler.clone())
                .unwrap()
        };
        for (fixture, route_name) in [
            ("main_47_builder_save", "SaveAgentBuilderConfig"),
            ("main_48_tests_list", "ListTests"),
            ("main_49_eval_sets_list", "ListEvalSets"),
            ("main_49_eval_sets_list", "RunEval"),
            ("main_50_eval_sets_create_dash", "CreateEvalSet"),
            ("main_51_eval_legacy_results", "ListEvalResultsLegacyPath"),
        ] {
            let path = format!(
                "{}/testdata/wire/{fixture}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let f: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            let resp = by_name(route_name)(Request::new(axum::body::Body::empty())).await;
            assert_eq!(resp.status().as_u16(), 501, "{route_name}");
            assert_eq!(
                resp.headers()["content-type"],
                f["response"]["headers"][0][1].as_str().unwrap(),
                "{route_name}"
            );
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(
                std::str::from_utf8(&body).unwrap(),
                f["response"]["body"].as_str().unwrap(),
                "{route_name}"
            );
        }
        let run_eval = by_name("RunEval")(Request::new(axum::body::Body::empty())).await;
        let body = axum::body::to_bytes(run_eval.into_body(), usize::MAX)
            .await
            .unwrap();
        let got: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert!(got.detail.contains(NOT_INSTALLED_DETAIL));
    }

    #[test]
    fn route_tables_follow_go() {
        let names: Vec<_> = eval_routes()
            .iter()
            .map(|r| (r.name, r.pattern.clone()))
            .collect();
        assert_eq!(names.len(), 17);
        assert_eq!(
            names[0],
            (
                "GetEvalMetricsInfo",
                "/dev/apps/{app_name}/metrics-info".into()
            )
        );
        assert_eq!(
            names[16],
            (
                "CreateEvalSetLegacyPath",
                "/apps/{app_name}/eval_sets/{eval_set_name}".into()
            )
        );
        assert_eq!(tests_routes().len(), 6);
        assert!(agent_builder_routes().iter().all(|r| !r.public));
    }
}
