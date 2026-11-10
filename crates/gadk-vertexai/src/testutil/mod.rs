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

//! Test servers for the Vertex AI services (feature `testutil`): [`ReplayServer`] serves Go's
//! recorded Agent Engine conversations strictly, and [`FakeAgentEngine`] is a stateful Agent
//! Engine fake. Both run on gadk-model's 127.0.0.1 fake HTTP server and see each request as
//! the gRPC call Go's generated REST client made it from ([`grpc_request`]).

pub mod engine;
pub mod replay;

use std::collections::HashMap;
use std::sync::Arc;

use gadk_model::rest::test_util::{FakeResponse, RecordedRequest};
use gadk_platform::env::MapEnv;
use serde_json::{Map, Value, json};

pub use engine::FakeAgentEngine;
pub use replay::ReplayServer;

use crate::session::VertexAiSessionServiceConfig;

/// The project of Go's recordings (Go `ProjectID`).
pub const PROJECT_ID: &str = "adk-go-e2e";
/// The location of Go's recordings (Go `Location`).
pub const LOCATION: &str = "us-central1";
/// The reasoning engine of Go's recordings (Go `EngineID`).
pub const ENGINE_ID: &str = "1491331942182813696";
/// The second reasoning engine Go's tests name (Go `EngineID2`).
pub const ENGINE_ID2: &str = "6857370898194759680";

/// The replay fixture stem of a shared-suite case (Go `sanitizeFilename` without the
/// extension, prefixed): `"Test_vertexaiService_" + case` with `" "`, `,` and `/` each
/// replaced by `_` — e.g. `fixture_stem("Create/when_already_exists,_it_fails")` =
/// `"Test_vertexaiService_Create_when_already_exists__it_fails"`.
pub fn fixture_stem(case: &str) -> String {
    format!(
        "Test_vertexaiService_{}",
        case.replace([' ', ',', '/'], "_")
    )
}

/// A session config for a fake at `url`: project [`PROJECT_ID`], location [`LOCATION`], no
/// reasoning engine, `without_authentication`, `endpoint: Some(url)`, and an empty
/// environment.
pub fn session_config(url: &str) -> VertexAiSessionServiceConfig {
    VertexAiSessionServiceConfig {
        project_id: PROJECT_ID.to_owned(),
        location: LOCATION.to_owned(),
        reasoning_engine: None,
        endpoint: Some(url.to_owned()),
        credentials: None,
        without_authentication: true,
        http_client: None,
        env: Arc::new(MapEnv(HashMap::new())),
    }
}

/// The gRPC method prefix of the session service's methods.
const SESSION_SERVICE: &str = "/google.cloud.aiplatform.v1beta1.SessionService/";

/// The full gRPC method of a short name: `"GetOperation"` →
/// `/google.longrunning.Operations/GetOperation`, every other name → the session service's
/// (`"GetSession"` → `/google.cloud.aiplatform.v1beta1.SessionService/GetSession`).
pub fn full_method(short: &str) -> String {
    if short == "GetOperation" {
        "/google.longrunning.Operations/GetOperation".to_owned()
    } else {
        format!("{SESSION_SERVICE}{short}")
    }
}

/// The gRPC call (method short name, protojson request) Go's generated REST client
/// (`aiplatform/apiv1beta1/session_client.go`, `auxiliary.go`) sent as `req`, or `None` for
/// a request no session-service method makes:
///
/// - `POST /v1beta1/{parent}/sessions[?sessionId=]`, body Session → `CreateSession`
///   `{parent, sessionId?, session: body}`;
/// - `GET /v1beta1/{parent}/sessions?…` → `ListSessions` `{parent, filter?, orderBy?,
///   pageSize?, pageToken?}`; `GET /v1beta1/{parent}/events?…` → `ListEvents` (same fields);
/// - `GET /v1beta1/{name}` → `GetSession` `{name}`; `DELETE /v1beta1/{name}` →
///   `DeleteSession` `{name}`;
/// - `POST /v1beta1/{name}:appendEvent`, body SessionEvent → `AppendEvent` `{name, event: body}`;
/// - `GET /ui/{name}` → `GetOperation` `{name}`.
///
/// The path is percent-decoded (`+` stays `+`, as in Go's `r.URL.Path`); every query
/// parameter but `$alt` becomes a string field (the last value wins), except `pageSize`
/// (int32), a number when it parses as one. An empty POST body is `{}`; one that is not JSON
/// makes the request unknown.
pub fn grpc_request(req: &RecordedRequest) -> Option<(&'static str, Value)> {
    let path = path_unescape(&req.path);
    let mut out = Map::new();
    let method = if let Some(name) = path.strip_prefix("/ui/") {
        if req.method != "GET" {
            return None;
        }
        out.insert("name".to_owned(), json!(name));
        "GetOperation"
    } else {
        let rest = path.strip_prefix("/v1beta1/")?;
        match req.method.as_str() {
            "POST" => {
                let body: Value = if req.body.is_empty() {
                    json!({})
                } else {
                    serde_json::from_slice(&req.body).ok()?
                };
                if let Some(name) = rest.strip_suffix(":appendEvent") {
                    out.insert("name".to_owned(), json!(name));
                    out.insert("event".to_owned(), body);
                    "AppendEvent"
                } else {
                    let parent = rest.strip_suffix("/sessions")?;
                    out.insert("parent".to_owned(), json!(parent));
                    out.insert("session".to_owned(), body);
                    "CreateSession"
                }
            }
            "GET" => {
                if let Some(parent) = rest.strip_suffix("/sessions") {
                    out.insert("parent".to_owned(), json!(parent));
                    "ListSessions"
                } else if let Some(parent) = rest.strip_suffix("/events") {
                    out.insert("parent".to_owned(), json!(parent));
                    "ListEvents"
                } else {
                    out.insert("name".to_owned(), json!(rest));
                    "GetSession"
                }
            }
            "DELETE" => {
                out.insert("name".to_owned(), json!(rest));
                "DeleteSession"
            }
            _ => return None,
        }
    };
    let query = req.query.as_deref().unwrap_or_default();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key == "$alt" {
            continue;
        }
        let value = match value.parse::<i64>() {
            Ok(n) if key == "pageSize" => json!(n),
            _ => json!(value),
        };
        out.insert(key.into_owned(), value);
    }
    Some((method, Value::Object(out)))
}

/// A URL path percent-decoded as Go's server decodes `r.URL.Path` (`%XX` → byte, `+` kept),
/// through `url`'s form decoder with the bytes it treats specially (`+`, `&`, `=`) escaped
/// first. Invalid UTF-8 is replaced.
fn path_unescape(path: &str) -> String {
    let protected = path
        .replace('+', "%2B")
        .replace('&', "%26")
        .replace('=', "%3D");
    url::form_urlencoded::parse(protected.as_bytes())
        .next()
        .map(|(decoded, _)| decoded.into_owned())
        .unwrap_or_default()
}

/// A Google JSON error response for a status name (`"NOT_FOUND"`): the HTTP status Google
/// maps it to and the body `{"error": {"code": <http>, "message": …, "status": <name>}}`.
pub(crate) fn error_response(status_name: &str, message: &str) -> FakeResponse {
    let http = crate::error::http_status_for_code(status_name);
    let body = json!({"error": {"code": http, "message": message, "status": status_name}});
    FakeResponse::json_status(http, body.to_string())
}

/// Sends one request to a fake with gadk's reqwest client: `path` unescaped, encoded as Go's
/// client encodes it (`EscapedPath`), and `query` encoded as `url.Values.Encode` with Go's
/// `$alt=json;enum-encoding=int` added. Returns the status and the body as JSON (`Null`
/// when empty or not JSON).
#[cfg(test)]
pub(crate) async fn send(
    base_url: &str,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    body: Option<&Value>,
) -> (u16, Value) {
    use gadk_model::{HttpClient, ReqwestClient, collect_body};
    let mut pairs: Vec<(&str, String)> = vec![("$alt", "json;enum-encoding=int".to_owned())];
    pairs.extend(query.iter().map(|(k, v)| (*k, (*v).to_owned())));
    let uri = format!(
        "{base_url}{}?{}",
        crate::protojson::go_escaped_path(path),
        crate::protojson::go_values_encode(&pairs)
    );
    let mut builder = http::Request::builder().method(method).uri(uri);
    let bytes = match body {
        Some(b) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(b.to_string())
        }
        None => bytes::Bytes::new(),
    };
    let resp = ReqwestClient::default()
        .send(builder.body(bytes).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = collect_body(resp.into_body()).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn fixture_stems_name_committed_fixtures() {
        assert_eq!(
            fixture_stem("Create/when_already_exists,_it_fails"),
            "Test_vertexaiService_Create_when_already_exists__it_fails"
        );
        assert_eq!(fixture_stem("Get/ok"), "Test_vertexaiService_Get_ok");
        assert_eq!(fixture_stem("a b"), "Test_vertexaiService_a_b");
        for case in [
            "Create/full_key",
            "Create/generated_session_id",
            "Create/when_already_exists,_it_fails",
            "Get/ok",
            "List",
            "Delete",
            "delete_session_respects_user_id",
            "AppendEvent/with_all_fields",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata/replays")
                .join(format!("{}.json", fixture_stem(case)));
            assert!(path.is_file(), "{}", path.display());
        }
    }

    fn recorded(method: &str, path: &str, query: Option<&str>, body: &str) -> RecordedRequest {
        RecordedRequest {
            method: method.to_owned(),
            url: format!("http://127.0.0.1:9{path}"),
            path: path.to_owned(),
            query: query.map(str::to_owned),
            headers: http::HeaderMap::new(),
            body: bytes::Bytes::from(body.to_owned()),
            at: tokio::time::Instant::now(),
        }
    }

    const ENGINE: &str = "projects/p/locations/l/reasoningEngines/1";
    const ALT: Option<&str> = Some("%24alt=json%3Benum-encoding%3Dint");

    #[test]
    fn grpc_request_rebuilds_each_session_call() {
        let session = format!("{ENGINE}/sessions/s1");
        let cases: Vec<(RecordedRequest, &str, Value)> = vec![
            (
                recorded(
                    "POST",
                    &format!("/v1beta1/{ENGINE}/sessions"),
                    Some("%24alt=json%3Benum-encoding%3Dint&sessionId=s1"),
                    r#"{"userId":"u"}"#,
                ),
                "CreateSession",
                json!({"parent": ENGINE, "sessionId": "s1", "session": {"userId": "u"}}),
            ),
            (
                recorded("POST", &format!("/v1beta1/{ENGINE}/sessions"), ALT, ""),
                "CreateSession",
                json!({"parent": ENGINE, "session": {}}),
            ),
            (
                recorded("GET", &format!("/v1beta1/{session}"), ALT, ""),
                "GetSession",
                json!({"name": session}),
            ),
            (
                recorded(
                    "GET",
                    &format!("/v1beta1/{ENGINE}/sessions"),
                    Some(
                        "%24alt=json%3Benum-encoding%3Dint&filter=userId%3D%22u%5C%22%22&orderBy=x&pageSize=50&pageToken=t+1",
                    ),
                    "",
                ),
                "ListSessions",
                json!({"parent": ENGINE, "filter": "userId=\"u\\\"\"", "orderBy": "x", "pageSize": 50, "pageToken": "t 1"}),
            ),
            (
                recorded("DELETE", &format!("/v1beta1/{session}"), ALT, ""),
                "DeleteSession",
                json!({"name": session}),
            ),
            (
                recorded(
                    "POST",
                    &format!("/v1beta1/{session}:appendEvent"),
                    ALT,
                    r#"{"author":"user"}"#,
                ),
                "AppendEvent",
                json!({"name": session, "event": {"author": "user"}}),
            ),
            (
                recorded(
                    "GET",
                    &format!("/v1beta1/{session}/events"),
                    Some(
                        "%24alt=json%3Benum-encoding%3Dint&filter=timestamp%3E%3D%222025-01-01T00%3A00%3A01%2B00%3A00%22",
                    ),
                    "",
                ),
                "ListEvents",
                json!({"parent": session, "filter": "timestamp>=\"2025-01-01T00:00:01+00:00\""}),
            ),
            (
                recorded("GET", &format!("/ui/{session}/operations/7"), ALT, ""),
                "GetOperation",
                json!({"name": format!("{session}/operations/7")}),
            ),
        ];
        for (req, method, want) in cases {
            assert_eq!(
                grpc_request(&req),
                Some((method, want)),
                "{} {}",
                req.method,
                req.path_and_query()
            );
        }
    }

    #[test]
    fn grpc_request_decodes_the_path_and_rejects_other_requests() {
        let req = recorded(
            "GET",
            "/v1beta1/projects/p/locations/l/reasoningEngines/1/sessions/a%20b+c%2Fd%25",
            None,
            "",
        );
        assert_eq!(
            grpc_request(&req),
            Some((
                "GetSession",
                json!({"name": "projects/p/locations/l/reasoningEngines/1/sessions/a b+c/d%"})
            ))
        );
        for req in [
            recorded("GET", "/v1/projects/p", None, ""),
            recorded("POST", "/ui/projects/p/operations/1", None, ""),
            recorded("POST", "/v1beta1/projects/p:other", None, "{}"),
            recorded(
                "POST",
                &format!("/v1beta1/{ENGINE}/sessions"),
                None,
                "not json",
            ),
            recorded(
                "PATCH",
                &format!("/v1beta1/{ENGINE}/sessions/s"),
                None,
                "{}",
            ),
        ] {
            assert_eq!(grpc_request(&req), None, "{} {}", req.method, req.path);
        }
    }

    #[test]
    fn full_method_names_the_grpc_service() {
        assert_eq!(
            full_method("GetSession"),
            "/google.cloud.aiplatform.v1beta1.SessionService/GetSession"
        );
        assert_eq!(
            full_method("GetOperation"),
            "/google.longrunning.Operations/GetOperation"
        );
    }

    #[test]
    fn error_response_is_googles_error_json() {
        let resp = error_response("CANCELLED", "context canceled");
        assert_eq!(resp.status, 499);
        let body: Value = serde_json::from_slice(&resp.body_bytes()).unwrap();
        assert_eq!(
            body,
            json!({"error": {"code": 499, "message": "context canceled", "status": "CANCELLED"}})
        );
    }

    #[test]
    fn session_config_targets_the_fake() {
        let config = session_config("http://127.0.0.1:9");
        assert_eq!(config.project_id, "adk-go-e2e");
        assert_eq!(config.location, "us-central1");
        assert_eq!(config.reasoning_engine, None);
        assert_eq!(config.endpoint.as_deref(), Some("http://127.0.0.1:9"));
        assert!(config.without_authentication);
        assert_eq!(config.env.var("GOOGLE_APPLICATION_CREDENTIALS"), None);
    }
}
