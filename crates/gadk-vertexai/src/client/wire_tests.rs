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

//! Wire tests of `VertexClient` against gadk-model's fakes: URLs (Go's `EscapedPath` and
//! sorted `url.Values` with `$alt`), headers, bodies, status mapping, paging, and operation
//! polling. `FakeServer` (a real socket) needs real time; the polling tests use
//! `ScriptedClient` under paused time.

use std::sync::Arc;

use gadk_model::ReqwestClient;
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest, ScriptedClient};
use google_cloud_auth::credentials::{CacheableResource, CredentialsProvider, EntityTag};
use google_cloud_auth::errors::CredentialsError;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

/// Credentials whose headers are `Authorization: Bearer test-token`.
#[derive(Debug)]
struct TestCreds;

impl CredentialsProvider for TestCreds {
    async fn headers(
        &self,
        _extensions: http::Extensions,
    ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
        Ok(CacheableResource::New {
            entity_tag: EntityTag::new(),
            data: HeaderMap::from_iter([(
                http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer test-token"),
            )]),
        })
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

/// A client for `base_url` sending through reqwest.
fn client_at(base_url: &str, credentials: Option<Credentials>) -> VertexClient {
    VertexClient::new(ResolvedClient {
        base_url: base_url.to_owned(),
        credentials,
        http: Arc::new(ReqwestClient::default()),
    })
}

/// A client for `base_url` sending through `scripted` (no socket), without credentials.
fn scripted_at(base_url: &str, scripted: &Arc<ScriptedClient>) -> VertexClient {
    VertexClient::new(ResolvedClient {
        base_url: base_url.to_owned(),
        credentials: None,
        http: scripted.clone(),
    })
}

/// An unescaped resource name with every kind of byte Go's escapers treat differently.
const ODD_NAME: &str = "projects/p 1/locations/l/reasoningEngines/e/sessions/s?x#y%:ü";

/// The `$alt` pair as Go's `url.Values.Encode` writes it.
const ALT_QUERY: &str = "%24alt=json%3Benum-encoding%3Dint";

/// The `VertexError` a call failed with.
fn vertex_error(err: &BoxError) -> &VertexError {
    err.downcast_ref::<VertexError>()
        .unwrap_or_else(|| panic!("not a VertexError: {err}"))
}

/// URL bytes from a Go program printing `url.URL.String()` after `u.Path +=
/// fmt.Sprintf("/v1beta1/%v", name)` and `u.RawQuery = params.Encode()`.
#[tokio::test]
async fn call_escapes_the_path_and_sorts_the_query_like_go() {
    let server = FakeServer::json(r#"{"name":"x"}"#);
    let client = client_at(&server.url(), None);
    let got = client
        .call(
            Method::GET,
            &format!("/v1beta1/{ODD_NAME}"),
            &[
                ("pageToken", "tok/+=".to_owned()),
                ("filter", r#"userId="u 1&b""#.to_owned()),
            ],
            ("name", ODD_NAME),
            None,
        )
        .await
        .unwrap();
    assert_eq!(got, json!({"name": "x"}));
    let req = server.last_request();
    assert_eq!(req.method, "GET");
    assert_eq!(
        req.path_and_query(),
        format!(
            "/v1beta1/projects/p%201/locations/l/reasoningEngines/e/sessions/s%3Fx%23y%25:%C3%BC\
             ?{ALT_QUERY}&filter=userId%3D%22u+1%26b%22&pageToken=tok%2F%2B%3D"
        )
    );
    // Go `fmt.Sprintf("%s=%v", "name", url.QueryEscape(req.GetName()))`.
    assert_eq!(
        req.header("x-goog-request-params"),
        "name=projects%2Fp+1%2Flocations%2Fl%2FreasoningEngines%2Fe%2Fsessions%2Fs%3Fx%23y%25%3A%C3%BC"
    );
    assert_eq!(req.header("content-type"), "application/json");
    assert_eq!(req.header("user-agent"), gadk_platform::tracking_header());
    assert!(req.headers.get("authorization").is_none());
    assert!(req.body.is_empty());
}

#[tokio::test]
async fn call_keeps_an_endpoint_path_prefix() {
    let server = FakeServer::json("{}");
    let client = client_at(&format!("{}/prefix", server.url()), None);
    client
        .call(
            Method::GET,
            "/v1beta1/projects/p/locations/l/reasoningEngines/e",
            &[],
            ("name", "projects/p/locations/l/reasoningEngines/e"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        server.last_request().path_and_query(),
        format!("/prefix/v1beta1/projects/p/locations/l/reasoningEngines/e?{ALT_QUERY}")
    );
}

#[tokio::test]
async fn call_sends_the_json_body_and_credential_headers() {
    let server = FakeServer::json(r#"{"name":"op","done":true}"#);
    let client = client_at(&server.url(), Some(Credentials::from(TestCreds)));
    let body =
        json!({"author": "user", "invocationId": "inv", "timestamp": "2026-01-02T03:04:05Z"});
    let got = client
        .call(
            Method::POST,
            "/v1beta1/projects/p/locations/l/reasoningEngines/e/sessions/s:appendEvent",
            &[],
            (
                "name",
                "projects/p/locations/l/reasoningEngines/e/sessions/s",
            ),
            Some(&body),
        )
        .await
        .unwrap();
    assert_eq!(got, json!({"name": "op", "done": true}));
    let req = server.last_request();
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path_and_query(),
        format!(
            "/v1beta1/projects/p/locations/l/reasoningEngines/e/sessions/s:appendEvent?{ALT_QUERY}"
        )
    );
    assert_eq!(req.json(), body);
    assert_eq!(req.header("content-type"), "application/json");
    assert_eq!(req.header("authorization"), "Bearer test-token");
    assert_eq!(
        req.header("x-goog-request-params"),
        "name=projects%2Fp%2Flocations%2Fl%2FreasoningEngines%2Fe%2Fsessions%2Fs"
    );
}

#[tokio::test]
async fn an_empty_2xx_body_is_an_empty_object() {
    let server = FakeServer::always(FakeResponse::new(200));
    let client = client_at(&server.url(), None);
    let got = client
        .call(Method::DELETE, "/v1beta1/x", &[], ("name", "x"), None)
        .await
        .unwrap();
    assert_eq!(got, json!({}));
    assert_eq!(server.last_request().method, "DELETE");
}

#[tokio::test]
async fn a_2xx_body_that_is_not_json_is_an_error() {
    let server = FakeServer::always(FakeResponse::new(200).chunk("not json"));
    let client = client_at(&server.url(), None);
    let err = client
        .call(Method::GET, "/v1beta1/x", &[], ("name", "x"), None)
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<serde_json::Error>().is_some(), "{err}");
}

#[tokio::test]
async fn error_statuses_become_vertex_errors() {
    let cases: [(u16, &str, Option<&str>, &str, &str); 4] = [
        (
            404,
            r#"{"error":{"code":404,"message":"Session not found.","status":"NOT_FOUND"}}"#,
            Some("NOT_FOUND"),
            "Session not found.",
            "rpc error: code = NotFound desc = Session not found.",
        ),
        (
            400,
            r#"{"error":{"code":400,"message":"bad filter","status":"INVALID_ARGUMENT"}}"#,
            Some("INVALID_ARGUMENT"),
            "bad filter",
            "rpc error: code = InvalidArgument desc = bad filter",
        ),
        (
            403,
            r#"{"error":{"code":403,"message":"denied","status":"PERMISSION_DENIED"}}"#,
            Some("PERMISSION_DENIED"),
            "denied",
            "rpc error: code = PermissionDenied desc = denied",
        ),
        (
            502,
            "<html>bad gateway</html>",
            None,
            "<html>bad gateway</html>",
            "rpc error: code = Unknown desc = <html>bad gateway</html>",
        ),
    ];
    for (status, body, code, message, display) in cases {
        let server = FakeServer::always(FakeResponse::new(status).chunk(body));
        let client = client_at(&server.url(), None);
        let err = client
            .call(Method::GET, "/v1beta1/x", &[], ("name", "x"), None)
            .await
            .unwrap_err();
        assert_eq!(
            vertex_error(&err),
            &VertexError {
                status,
                code: code.map(str::to_owned),
                message: message.to_owned(),
            },
            "status {status}"
        );
        assert_eq!(err.to_string(), display);
    }
}

#[tokio::test]
async fn a_503_is_not_retried() {
    let server = FakeServer::always(FakeResponse::json_status(
        503,
        r#"{"error":{"code":503,"message":"busy","status":"UNAVAILABLE"}}"#,
    ));
    let client = client_at(&server.url(), None);
    let err = client
        .call(Method::GET, "/v1beta1/x", &[], ("name", "x"), None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "rpc error: code = Unavailable desc = busy");
    assert_eq!(server.hits(), 1);
}

#[tokio::test]
async fn a_connection_error_is_a_rest_error() {
    let scripted = ScriptedClient::always(FakeResponse::reset("connection refused"));
    let client = scripted_at("https://aiplatform.googleapis.com", &scripted);
    let err = client
        .call(Method::GET, "/v1beta1/x", &[], ("name", "x"), None)
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<RestError>().is_some(), "{err}");
    assert_eq!(scripted.requests().len(), 1);
}

/// Go's `ListSessions` / `ListEvents` iterators: one GET per page, `pageToken` added from the
/// previous page's `nextPageToken`, no routing header.
#[tokio::test]
async fn list_all_follows_next_page_tokens() {
    let server = FakeServer::start(|req: &RecordedRequest| {
        let query = req.query.clone().unwrap_or_default();
        if query.ends_with("pageToken=t2") {
            FakeResponse::json(r#"{"sessions":[{"name":"c"}],"nextPageToken":"t 3"}"#)
        } else if query.ends_with("pageToken=t+3") {
            FakeResponse::json(r#"{"nextPageToken":""}"#)
        } else {
            FakeResponse::json(r#"{"sessions":[{"name":"a"},{"name":"b"}],"nextPageToken":"t2"}"#)
        }
    });
    let client = client_at(&server.url(), None);
    let items = client
        .list_all(
            "/v1beta1/projects/p/locations/l/reasoningEngines/e/sessions",
            &[("filter", r#"userId="u""#.to_owned())],
            ("", ""),
            "sessions",
        )
        .await
        .unwrap();
    assert_eq!(
        items,
        [
            json!({"name": "a"}),
            json!({"name": "b"}),
            json!({"name": "c"})
        ]
    );
    let requests = server.requests();
    let queries: Vec<_> = requests.iter().map(|r| r.query.clone().unwrap()).collect();
    assert_eq!(
        queries,
        [
            format!("{ALT_QUERY}&filter=userId%3D%22u%22"),
            format!("{ALT_QUERY}&filter=userId%3D%22u%22&pageToken=t2"),
            format!("{ALT_QUERY}&filter=userId%3D%22u%22&pageToken=t+3"),
        ]
    );
    for req in &requests {
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.path,
            "/v1beta1/projects/p/locations/l/reasoningEngines/e/sessions"
        );
        assert!(req.headers.get("x-goog-request-params").is_none());
        assert_eq!(req.header("content-type"), "application/json");
    }
}

#[tokio::test]
async fn list_all_stops_at_the_first_error() {
    let server = FakeServer::always(FakeResponse::json_status(
        404,
        r#"{"error":{"code":404,"message":"no engine","status":"NOT_FOUND"}}"#,
    ));
    let client = client_at(&server.url(), None);
    let err = client
        .list_all("/v1beta1/x/sessions", &[], ("", ""), "sessions")
        .await
        .unwrap_err();
    assert!(vertex_error(&err).is_not_found());
    assert_eq!(server.hits(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_done_operation_sends_no_request() {
    let scripted = ScriptedClient::json("{}");
    let client = scripted_at("https://aiplatform.googleapis.com", &scripted);
    let op = json!({"name": "projects/p/locations/l/operations/1", "done": true});
    assert_eq!(client.wait_operation(op.clone()).await.unwrap(), op);

    let failed = json!({"name": "o", "done": true, "error": {"code": 5, "message": "gone"}});
    let err = client.wait_operation(failed).await.unwrap_err();
    assert_eq!(err.to_string(), "rpc error: code = NotFound desc = gone");
    assert!(scripted.requests().is_empty());
}

/// Go `Operation.wait`: poll at once, then sleep `bo.Pause()` (in `[1ns, 1s]`, then
/// `[1ns, 2s]`) between polls. The poll is longrunning's REST `GetOperation` with
/// `gax.WithPath("/ui/" + name)`: the endpoint's path prefix is replaced, no `$alt` query.
#[tokio::test(start_paused = true)]
async fn a_pending_operation_is_polled_at_its_ui_path_until_done() {
    let name = "projects/p/locations/l/reasoningEngines/e/sessions/s/operations/o 1";
    let done = json!({"name": name, "done": true, "response": {"@type": "x"}});
    let scripted = ScriptedClient::sequence(vec![
        FakeResponse::json(json!({"name": name}).to_string()),
        FakeResponse::json(json!({"name": name, "done": false}).to_string()),
        FakeResponse::json(done.to_string()),
    ]);
    let client = scripted_at("https://aiplatform.googleapis.com/prefix", &scripted);
    let start = Instant::now();
    let got = client
        .wait_operation(json!({"name": name, "done": false}))
        .await
        .unwrap();
    assert_eq!(got, done);
    let requests = scripted.requests();
    assert_eq!(requests.len(), 3);
    for req in &requests {
        assert_eq!(req.method, "GET");
        assert_eq!(
            req.url,
            "https://aiplatform.googleapis.com/ui/projects/p/locations/l/reasoningEngines/e/sessions/s/operations/o%201"
        );
        assert_eq!(
            req.header("x-goog-request-params"),
            "name=projects%2Fp%2Flocations%2Fl%2FreasoningEngines%2Fe%2Fsessions%2Fs%2Foperations%2Fo+1"
        );
        assert_eq!(req.header("content-type"), "application/json");
        assert_eq!(req.header("user-agent"), gadk_platform::tracking_header());
    }
    assert_eq!(requests[0].at, start, "the first poll is immediate");
    let first = requests[1].at - requests[0].at;
    let second = requests[2].at - requests[1].at;
    assert!(
        first > Duration::ZERO && first <= Duration::from_secs(1),
        "{first:?}"
    );
    assert!(
        second > Duration::ZERO && second <= Duration::from_secs(2),
        "{second:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_operation_done_with_an_error_is_a_vertex_error() {
    let scripted = ScriptedClient::json(
        r#"{"name":"o","done":true,"error":{"code":3,"message":"bad request"}}"#,
    );
    let client = scripted_at("https://aiplatform.googleapis.com", &scripted);
    let err = client
        .wait_operation(json!({"name": "o"}))
        .await
        .unwrap_err();
    assert_eq!(
        vertex_error(&err),
        &VertexError {
            status: 400,
            code: Some("INVALID_ARGUMENT".to_owned()),
            message: "bad request".to_owned(),
        }
    );
    assert_eq!(
        err.to_string(),
        "rpc error: code = InvalidArgument desc = bad request"
    );
    assert_eq!(scripted.requests().len(), 1);
}

/// longrunning's `GetOperation` call options: a 503 is retried (gax backoff from 500 ms)…
#[tokio::test(start_paused = true)]
async fn a_poll_retries_a_503() {
    let busy = FakeResponse::json_status(503, r#"{"error":{"code":503,"status":"UNAVAILABLE"}}"#);
    let scripted = ScriptedClient::sequence(vec![
        busy.clone(),
        busy,
        FakeResponse::json(r#"{"name":"o","done":true}"#),
    ]);
    let client = scripted_at("https://aiplatform.googleapis.com", &scripted);
    let got = client.wait_operation(json!({"name": "o"})).await.unwrap();
    assert_eq!(got, json!({"name": "o", "done": true}));
    let requests = scripted.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].at - requests[0].at <= Duration::from_millis(500));
    assert!(requests[2].at - requests[1].at <= Duration::from_secs(1));
}

/// …within a 10 s deadline, after which the poll fails with Go's `context deadline exceeded`.
#[tokio::test(start_paused = true)]
async fn a_poll_gives_up_on_503_after_ten_seconds() {
    let scripted = ScriptedClient::always(FakeResponse::json_status(
        503,
        r#"{"error":{"code":503,"status":"UNAVAILABLE"}}"#,
    ));
    let client = scripted_at("https://aiplatform.googleapis.com", &scripted);
    let start = Instant::now();
    let err = client
        .wait_operation(json!({"name": "o"}))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<RestError>(),
            Some(RestError::DeadlineExceeded)
        ),
        "{err}"
    );
    assert_eq!(err.to_string(), "context deadline exceeded");
    assert_eq!(Instant::now() - start, POLL_TIMEOUT);
    let requests = scripted.requests();
    assert!(requests.len() >= 2, "{}", requests.len());
    assert!(requests.iter().all(|r| r.at - start < POLL_TIMEOUT));
}

#[tokio::test(start_paused = true)]
async fn a_poll_error_other_than_503_is_not_retried() {
    let scripted = ScriptedClient::always(FakeResponse::json_status(
        404,
        r#"{"error":{"code":404,"message":"no op","status":"NOT_FOUND"}}"#,
    ));
    let client = scripted_at("https://aiplatform.googleapis.com", &scripted);
    let err = client
        .wait_operation(json!({"name": "o"}))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "rpc error: code = NotFound desc = no op");
    assert_eq!(scripted.requests().len(), 1);
}

/// gax `Backoff.Pause`: `1 + rand.Int63n(cur)` ns, then `cur = min(cur*2, max)`.
#[test]
fn backoff_pauses_are_full_jitter_up_to_the_doubling_bound() {
    let mut low = Backoff::new(WAIT_INITIAL, WAIT_INTERVAL);
    let mut high = Backoff::new(WAIT_INITIAL, WAIT_INTERVAL);
    let mut bounds = Vec::new();
    for _ in 0..8 {
        assert_eq!(low.pause_with(|_| 0), Duration::from_nanos(1));
        bounds.push(high.pause_with(|n| n - 1));
    }
    assert_eq!(
        bounds,
        [1, 2, 4, 8, 16, 32, 60, 60].map(Duration::from_secs)
    );

    // An out-of-range draw is clamped.
    let mut b = Backoff::new(Duration::from_nanos(5), Duration::from_secs(1));
    assert_eq!(b.pause_with(|_| u64::MAX), Duration::from_nanos(5));

    // A cap below the initial bound is raised to it (Go `waitWithInterval`).
    let mut b = Backoff::new(Duration::from_secs(2), Duration::from_secs(1));
    assert_eq!(b.pause_with(|n| n - 1), Duration::from_secs(2));
    assert_eq!(b.pause_with(|n| n - 1), Duration::from_secs(2));

    // The random draw stays in [1ns, cur].
    let mut b = Backoff::new(POLL_RETRY_INITIAL, POLL_RETRY_MAX);
    let mut bound = POLL_RETRY_INITIAL;
    for _ in 0..200 {
        let d = b.pause();
        assert!(
            d >= Duration::from_nanos(1) && d <= bound,
            "{d:?} > {bound:?}"
        );
        bound = (bound * 2).min(POLL_RETRY_MAX);
    }
}
