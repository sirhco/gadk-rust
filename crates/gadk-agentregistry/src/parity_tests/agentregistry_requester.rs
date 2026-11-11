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

//! Ports of ADK-Go `agentregistry/requester_test.go`, plus the test server and the
//! `newTestRequester` / `newTestClient` helpers the other registry parity modules share.

use std::sync::{Arc, Mutex};

use http::{HeaderMap, StatusCode};

use crate::config::Client;
use crate::error::{ApiError, Error};
use crate::requester::RestRequester;
use crate::types::{Agent, ListAgentsResponse, McpServer};

/// One request the test server received (Go's handler reading `r.URL` and `r.Header`).
#[derive(Debug, Clone)]
pub(super) struct Seen {
    /// Go `r.URL.Path`.
    pub(super) path: String,
    /// Go `r.URL.RawQuery`.
    pub(super) query: String,
    /// Go `r.Header`.
    pub(super) headers: HeaderMap,
}

impl Seen {
    /// Go `r.URL.Query().Get(key)`: the first value of `key`, decoded; `""` when absent.
    pub(super) fn query_get(&self, key: &str) -> String {
        self.query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
            .find(|(k, _)| query_unescape(k) == key)
            .map(|(_, v)| query_unescape(v))
            .unwrap_or_default()
    }
}

/// Go `url.QueryUnescape` for well-formed input: `+` is a space, `%XX` a byte.
fn query_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

/// Go `httptest.NewServer`: a loopback server answering every request with `handler`'s
/// `(status, body)` and recording what it received.
pub(super) struct TestServer {
    /// Go `srv.URL`: `http://127.0.0.1:<port>`.
    pub(super) url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl TestServer {
    /// Every request received so far, in order.
    pub(super) fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// The only request received.
    pub(super) fn only_request(&self) -> Seen {
        let seen = self.requests();
        assert_eq!(seen.len(), 1, "requests = {seen:?}");
        seen.into_iter().next().unwrap()
    }
}

/// Starts a [`TestServer`].
pub(super) async fn serve<F>(handler: F) -> TestServer
where
    F: Fn(&Seen) -> (u16, String) + Send + Sync + 'static,
{
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let router = {
        let seen = Arc::clone(&seen);
        axum::Router::new().fallback(move |req: axum::extract::Request| {
            let seen = Arc::clone(&seen);
            let handler = Arc::clone(&handler);
            async move {
                let s = Seen {
                    path: req.uri().path().to_owned(),
                    query: req.uri().query().unwrap_or_default().to_owned(),
                    headers: req.headers().clone(),
                };
                seen.lock().unwrap().push(s.clone());
                let (status, body) = handler(&s);
                (StatusCode::from_u16(status).unwrap(), body)
            }
        })
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestServer { url, seen }
}

/// Go `srv.Client()`: a plain client (no proxy, so loopback is reached directly).
pub(super) fn test_http_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// Go `newTestRequester`: a requester pointing at `srv` with parent `projects/p/locations/l`
/// and quota project `p`.
pub(super) fn new_test_requester(srv: &TestServer) -> RestRequester {
    RestRequester {
        http: test_http_client(),
        auth: None,
        base_url: srv.url.clone(),
        base_path: "projects/p/locations/l".to_owned(),
        user_project: "p".to_owned(),
    }
}

/// Go `newTestClient`: a [`Client`] whose requester talks to `srv` (method → requester → HTTP).
pub(super) fn new_test_client(srv: &TestServer) -> Client {
    Client {
        requester: new_test_requester(srv),
    }
}

/// Go `TestRestRequester_Get_RelativePathAndHeaders`.
#[tokio::test]
async fn test_rest_requester_get_relative_path_and_headers() {
    let srv = serve(|_| {
        (
            200,
            r#"{"agents":[{"displayName":"Foo"}],"nextPageToken":"next"}"#.to_owned(),
        )
    })
    .await;

    let params = vec![("pageSize".to_owned(), "5".to_owned())];
    let page: ListAgentsResponse = new_test_requester(&srv)
        .get_json("agents", &params)
        .await
        .unwrap();

    let seen = srv.only_request();
    assert_eq!(seen.path, "/projects/p/locations/l/agents");
    assert_eq!(seen.query, "pageSize=5");
    assert_eq!(
        seen.headers
            .get_all("content-type")
            .iter()
            .collect::<Vec<_>>(),
        ["application/json"]
    );
    assert_eq!(
        seen.headers
            .get_all("x-goog-user-project")
            .iter()
            .collect::<Vec<_>>(),
        ["p"]
    );
    assert_eq!(page.agents.len(), 1, "decoded page = {page:?}");
    assert_eq!(page.agents[0].display_name, "Foo");
    assert_eq!(page.next_page_token, "next");
}

/// Go `TestRestRequester_Get_AbsoluteResourcePath`.
#[tokio::test]
async fn test_rest_requester_get_absolute_resource_path() {
    let srv = serve(|_| (200, r#"{"displayName":"Bar"}"#.to_owned())).await;

    let name = "projects/p/locations/l/mcpServers/bar";
    let got: McpServer = new_test_requester(&srv).get_json(name, &[]).await.unwrap();

    // The absolute resource name, not prefixed with the parent.
    assert_eq!(srv.only_request().path, format!("/{name}"));
    assert_eq!(got.display_name, "Bar");
}

/// Go `TestRestRequester_Get_APIError`.
#[tokio::test]
async fn test_rest_requester_get_api_error() {
    let srv = serve(|_| (404, r#"{"error":"not found"}"#.to_owned())).await;

    let err = new_test_requester(&srv)
        .get_json::<Agent>("agents/missing", &[])
        .await
        .unwrap_err();
    let Error::Api(api) = &err else {
        panic!("get error = {err:?}, want Error::Api");
    };
    assert_eq!(
        api,
        &ApiError {
            status_code: 404,
            body: r#"{"error":"not found"}"#.to_owned(),
        }
    );
    assert_eq!(
        err.to_string(),
        r#"agentregistry: API request failed with status 404: {"error":"not found"}"#
    );
}

/// Go `TestRestRequester_Get_DecodeError`.
#[tokio::test]
async fn test_rest_requester_get_decode_error() {
    let srv = serve(|_| (200, "{not valid json".to_owned())).await;

    let err = new_test_requester(&srv)
        .get_json::<ListAgentsResponse>("agents", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Decode(_)),
        "get error = {err:?}, want a decode error"
    );
    assert!(
        err.to_string()
            .starts_with("agentregistry: decoding response: "),
        "{err}"
    );
}

/// Go `TestRestRequester_Get_NilOutSkipsDecode`.
#[tokio::test]
async fn test_rest_requester_get_nil_out_skips_decode() {
    let srv = serve(|_| (200, "not json but ignored".to_owned())).await;

    let body = new_test_requester(&srv).get("agents", &[]).await.unwrap();
    assert_eq!(&body[..], b"not json but ignored");
}

/// Go `TestRestRequester_Get_NoUserProjectHeaderWhenEmpty`.
#[tokio::test]
async fn test_rest_requester_get_no_user_project_header_when_empty() {
    let srv = serve(|_| (200, "{}".to_owned())).await;

    let rt = RestRequester {
        user_project: String::new(),
        ..new_test_requester(&srv)
    };
    rt.get("agents", &[]).await.unwrap();
    assert!(
        !srv.only_request()
            .headers
            .contains_key("x-goog-user-project"),
        "x-goog-user-project header set, want it omitted when user_project is empty"
    );
}
