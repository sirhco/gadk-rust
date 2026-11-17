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

//! [`ReplayServer`]: serves one recorded Agent Engine conversation
//! (`testdata/replays/{stem}.json`) strictly, answering each REST request with the first unused
//! recorded call of the same gRPC method and an equal request.

use std::sync::Arc;

use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use parking_lot::Mutex;
use serde_json::Value;

use super::{error_response, full_method, grpc_request};

/// A strict replay fake on 127.0.0.1 (needs a running tokio runtime).
///
/// Each request is mapped back to the gRPC call the REST request stands for
/// ([`grpc_request`](super::grpc_request)) and answered with the first unused recorded call
/// whose method is the same and whose request is equal under [`json_eq`]: the recorded
/// `response` as a 200 JSON body, or the recorded `error` as Google's JSON error with the
/// HTTP status Google maps its gRPC code to (`Canceled` → 499 `CANCELLED`, `NotFound` → 404,
/// `InvalidArgument` → 400, …). A request nothing matches is answered 500 and listed by
/// [`unmatched`](Self::unmatched). Recorded calls left unused are allowed.
pub struct ReplayServer {
    server: FakeServer,
    state: Arc<Mutex<Replay>>,
}

/// One recorded call of a fixture.
struct Call {
    /// The full gRPC method (`/google.cloud.aiplatform.v1beta1.SessionService/GetSession`).
    method: String,
    request: Value,
    /// The recorded response, or the recorded error (gRPC code, message).
    outcome: Result<Value, (i64, String)>,
    used: bool,
}

struct Replay {
    calls: Vec<Call>,
    unmatched: Vec<String>,
}

impl ReplayServer {
    /// Serves `testdata/replays/{stem}.json`; panics if it is missing or malformed.
    #[allow(clippy::panic, clippy::expect_used)]
    pub fn start(stem: &str) -> Self {
        let path = format!(
            "{}{stem}.json",
            concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/replays/")
        );
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("replay fixture {path}: {e}"));
        let fixture: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("replay fixture {path}: {e}"));
        let calls = fixture
            .get("calls")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("replay fixture {path}: no calls array"))
            .iter()
            .map(|call| {
                let outcome = match call.get("error") {
                    Some(err) => Err((
                        err.get("code").and_then(Value::as_i64).unwrap_or(2),
                        err.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    )),
                    None => Ok(call.get("response").cloned().unwrap_or(Value::Null)),
                };
                Call {
                    method: call
                        .get("method")
                        .and_then(Value::as_str)
                        .expect("a recorded call has a method")
                        .to_owned(),
                    request: call.get("request").cloned().unwrap_or(Value::Null),
                    outcome,
                    used: false,
                }
            })
            .collect();
        let state = Arc::new(Mutex::new(Replay {
            calls,
            unmatched: Vec::new(),
        }));
        let handler_state = state.clone();
        let server = FakeServer::start(move |req| handler_state.lock().serve(req));
        Self { server, state }
    }

    /// The server's base URL (`http://127.0.0.1:<port>`).
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Each unmatched request rendered as
    /// `"{method} {request JSON}\n  closest unused: {recorded JSON or <none>}"`, in arrival
    /// order. A request that maps to no gRPC method is rendered with its HTTP method and path
    /// as the method and its body (or `null`) as the JSON.
    pub fn unmatched(&self) -> Vec<String> {
        self.state.lock().unmatched.clone()
    }

    /// Every request the client sent, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.server.requests()
    }
}

impl Replay {
    fn serve(&mut self, req: &RecordedRequest) -> FakeResponse {
        let Some((short, request)) = grpc_request(req) else {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let shown = format!("{} {}", req.method, req.path);
            return self.unmatched(&shown, &body, None);
        };
        let method = full_method(short);
        let hit = self
            .calls
            .iter_mut()
            .find(|c| !c.used && c.method == method && json_eq(&c.request, &request));
        let Some(call) = hit else {
            let closest = self
                .calls
                .iter()
                .filter(|c| !c.used && c.method == method)
                .min_by_key(|c| json_distance(&c.request, &request))
                .map(|c| c.request.clone());
            return self.unmatched(&method, &request, closest.as_ref());
        };
        call.used = true;
        match &call.outcome {
            Ok(response) => FakeResponse::json(response.to_string()),
            Err((code, message)) => error_response(crate::error::grpc_code_name(*code), message),
        }
    }

    fn unmatched(
        &mut self,
        method: &str,
        request: &Value,
        closest: Option<&Value>,
    ) -> FakeResponse {
        let closest = closest.map_or_else(|| "<none>".to_owned(), Value::to_string);
        let entry = format!("{method} {request}\n  closest unused: {closest}");
        let message = format!("replay: no unused recorded call matches {entry}");
        self.unmatched.push(entry);
        error_response("INTERNAL", &message)
    }
}

/// rpcreplay's request equality on protojson values: objects key-order-insensitively, arrays
/// in order, strings and booleans exactly, numbers by numeric value (`1` equals `1.0`; an
/// int64 sent as a string stays a string, so `"1"` does not equal `1`).
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| json_eq(v, w))
        }
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i == j,
            _ => match (x.as_u64(), y.as_u64()) {
                (Some(i), Some(j)) => i == j,
                _ => x.as_f64() == y.as_f64(),
            },
        },
        _ => a == b,
    }
}

/// How far apart two requests are, for naming the closest unused recorded request: the number
/// of differing leaves (a key present on one side only counts its leaves).
fn json_distance(a: &Value, b: &Value) -> usize {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let only_x = x
                .iter()
                .filter(|(k, _)| !y.contains_key(*k))
                .map(|(_, v)| leaves(v));
            let only_y = y
                .iter()
                .filter(|(k, _)| !x.contains_key(*k))
                .map(|(_, v)| leaves(v));
            let both = x
                .iter()
                .filter_map(|(k, v)| y.get(k).map(|w| json_distance(v, w)));
            only_x.chain(only_y).chain(both).sum()
        }
        (Value::Array(x), Value::Array(y)) => {
            let common: usize = x.iter().zip(y).map(|(v, w)| json_distance(v, w)).sum();
            let extra: usize = x
                .iter()
                .skip(y.len())
                .chain(y.iter().skip(x.len()))
                .map(leaves)
                .sum();
            common + extra
        }
        _ => usize::from(!json_eq(a, b)),
    }
}

/// The number of leaves of a JSON value (an empty object or array counts as one).
fn leaves(v: &Value) -> usize {
    match v {
        Value::Object(m) if !m.is_empty() => m.values().map(leaves).sum(),
        Value::Array(a) if !a.is_empty() => a.iter().map(leaves).sum(),
        _ => 1,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testutil::send;

    const ENGINE: &str =
        "projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696";
    const ENGINE2: &str =
        "projects/adk-go-e2e/locations/us-central1/reasoningEngines/6857370898194759680";
    const DELETED: &str = "projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696/sessions/8685635740484239360";

    #[tokio::test]
    async fn serves_a_fixture_in_order_with_recorded_errors() {
        let replay = ReplayServer::start("Test_vertexaiService_Delete");
        let url = replay.url();

        let created = json!({"userId": "user1"});
        let (status, op) = send(
            &url,
            "POST",
            &format!("/v1beta1/{ENGINE}/sessions"),
            &[],
            Some(&created),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(op["done"], json!(true));
        assert_eq!(
            op["response"]["@type"],
            json!("type.googleapis.com/google.cloud.aiplatform.v1beta1.Session")
        );

        for _ in 0..2 {
            let (status, got) = send(&url, "GET", &format!("/v1beta1/{DELETED}"), &[], None).await;
            assert_eq!(status, 200);
            assert_eq!(got["userId"], json!("user1"));
        }
        let (status, op) = send(&url, "DELETE", &format!("/v1beta1/{DELETED}"), &[], None).await;
        assert_eq!((status, &op["done"]), (200, &json!(true)));

        let (status, body) = send(
            &url,
            "GET",
            &format!("/v1beta1/{DELETED}/events"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(
            body,
            json!({"error": {
                "code": 404,
                "message": format!("Session {DELETED} not found."),
                "status": "NOT_FOUND",
            }})
        );

        // The third GetSession of the same name is the recording's third: the canceled one.
        let (status, body) = send(&url, "GET", &format!("/v1beta1/{DELETED}"), &[], None).await;
        assert_eq!(status, 499);
        assert_eq!(
            body,
            json!({"error": {"code": 499, "message": "context canceled", "status": "CANCELLED"}})
        );

        let (status, body) = send(
            &url,
            "GET",
            &format!("/v1beta1/{ENGINE}/sessions/nonExistent"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["status"], json!("INVALID_ARGUMENT"));
        assert_eq!(
            body["error"]["message"],
            json!(
                "List of found errors:\t1.Field: name; Message: Invalid Session resource name.\t"
            )
        );

        for engine in [ENGINE, ENGINE2] {
            let (status, body) = send(
                &url,
                "GET",
                &format!("/v1beta1/{engine}/sessions"),
                &[],
                None,
            )
            .await;
            assert_eq!((status, body), (200, json!({})));
        }
        assert_eq!(replay.unmatched(), Vec::<String>::new());
        assert_eq!(replay.requests().len(), 9);
        assert!(
            replay.requests()[0]
                .query
                .as_deref()
                .unwrap()
                .contains("%24alt=json%3Benum-encoding%3Dint")
        );
    }

    #[tokio::test]
    async fn a_used_call_is_not_served_again() {
        let replay = ReplayServer::start("Test_vertexaiService_Delete");
        let url = replay.url();
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(
                send(&url, "GET", &format!("/v1beta1/{DELETED}"), &[], None)
                    .await
                    .0,
            );
        }
        // Three recorded GetSession calls of that name (200, 200, Canceled); the fourth is
        // unmatched. Leftover calls of other methods are allowed.
        assert_eq!(statuses, [200, 200, 499, 500]);
        assert_eq!(replay.unmatched().len(), 1);
    }

    #[tokio::test]
    async fn numbers_match_by_value_and_keys_in_any_order() {
        let replay = ReplayServer::start("Test_vertexaiService_Create_full_key");
        let body = json!({"userId": "testUserID", "sessionState": {"k": 5.0}});
        let (status, op) = send(
            &replay.url(),
            "POST",
            &format!("/v1beta1/{ENGINE}/sessions"),
            &[("sessionId", "test-session-id")],
            Some(&body),
        )
        .await;
        assert_eq!(status, 200, "{:?}", replay.unmatched());
        assert_eq!(op["response"]["sessionState"], json!({"k": 5}));
    }

    #[tokio::test]
    async fn unmatched_requests_are_answered_500_and_listed_with_the_closest_call() {
        let replay = ReplayServer::start("Test_vertexaiService_Create_full_key");
        let url = replay.url();
        let body = json!({"userId": "someoneElse", "sessionState": {"k": 5}});
        let (status, err) = send(
            &url,
            "POST",
            &format!("/v1beta1/{ENGINE}/sessions"),
            &[("sessionId", "test-session-id")],
            Some(&body),
        )
        .await;
        assert_eq!(status, 500);
        assert_eq!(err["error"]["status"], json!("INTERNAL"));
        let (status, _) = send(
            &url,
            "POST",
            "/v1beta1/projects/p:unknown",
            &[],
            Some(&json!({})),
        )
        .await;
        assert_eq!(status, 500);
        let (status, _) = send(
            &url,
            "GET",
            &format!("/ui/{ENGINE}/operations/1"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 500);

        let unmatched = replay.unmatched();
        assert_eq!(unmatched.len(), 3, "{unmatched:#?}");
        let sent = json!({"parent": ENGINE, "sessionId": "test-session-id", "session": body});
        let recorded = json!({
            "parent": ENGINE,
            "session": {"sessionState": {"k": 5}, "userId": "testUserID"},
            "sessionId": "test-session-id",
        });
        assert_eq!(
            unmatched[0],
            format!(
                "/google.cloud.aiplatform.v1beta1.SessionService/CreateSession {sent}\n  closest unused: {recorded}"
            )
        );
        assert_eq!(
            unmatched[1],
            "POST /v1beta1/projects/p:unknown {}\n  closest unused: <none>"
        );
        assert_eq!(
            unmatched[2],
            format!(
                "/google.longrunning.Operations/GetOperation {}\n  closest unused: <none>",
                json!({"name": format!("{ENGINE}/operations/1")})
            )
        );
    }

    #[test]
    fn json_eq_is_rpcreplays_comparator() {
        assert!(json_eq(&json!(1), &json!(1.0)));
        assert!(json_eq(
            &json!({"a": 1, "b": [1, 2]}),
            &json!({"b": [1.0, 2], "a": 1})
        ));
        assert!(json_eq(&json!(u64::MAX), &json!(u64::MAX)));
        assert!(!json_eq(&json!("1"), &json!(1)));
        assert!(!json_eq(&json!([1, 2]), &json!([2, 1])));
        assert!(!json_eq(&json!({"a": 1}), &json!({"a": 1, "b": null})));
        assert!(!json_eq(&json!({"a": {}}), &json!({"a": null})));
        assert!(!json_eq(&json!("a"), &json!("A")));
        assert!(!json_eq(&json!(1), &json!(1.5)));
    }

    #[test]
    fn json_distance_counts_differing_leaves() {
        assert_eq!(json_distance(&json!({"a": 1}), &json!({"a": 1.0})), 0);
        assert_eq!(json_distance(&json!({"a": 1, "b": 2}), &json!({"a": 2})), 2);
        assert_eq!(json_distance(&json!([1, 2, 3]), &json!([1, 5])), 2);
        assert_eq!(
            json_distance(&json!({"a": {"x": 1, "y": 2}}), &json!({})),
            2
        );
    }
}
