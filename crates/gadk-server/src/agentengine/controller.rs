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
//! Go `server/agentengine/controllers/agent_engine.go`: `AgentEngineAPIController`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, HttpBody as _};
use bytes::Bytes;
use futures::StreamExt as _;
use gadk_platform::GoDuration;
use http::StatusCode;

use super::Log;
use super::encode_shapes;
use super::method::MethodHandler;
use super::models::Query;
use super::unmarshal::go_unmarshal;
use crate::mux::{Request, Response, WriteDeadline, http_error};

/// Go `defaultSSETimeout`: the write deadline when none is configured.
pub(crate) const DEFAULT_SSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Go `AgentEngineAPIController`: dispatches a query to the method named by `class_method`.
/// Go's unused `service` field has no counterpart.
pub(crate) struct Controller {
    /// Go `handlers`: by method name.
    pub(crate) handlers: HashMap<String, Arc<dyn MethodHandler>>,
    /// Go `maxPayloadSize`: bytes of body read; the rest is silently dropped (`<= 0` reads
    /// nothing).
    pub(crate) max_payload_size: i64,
    /// Go `sseTimeout` after `NewAgentEngineAPIController` replaced zero with 120 s; negative
    /// means no write deadline.
    pub(crate) sse_timeout: GoDuration,
    /// Go `log`.
    pub(crate) log: Log,
}

impl Controller {
    /// Go `NewAgentEngineAPIController`: the handlers by name (a repeated name is `Err(name)`,
    /// Go's `duplicate method name: %v`), and a zero `sse_timeout` replaced by
    /// [`DEFAULT_SSE_TIMEOUT`].
    pub(crate) fn new(
        sse_timeout: GoDuration,
        max_payload_size: i64,
        handlers: Vec<Arc<dyn MethodHandler>>,
        log: Log,
    ) -> Result<Controller, String> {
        let mut by_name: HashMap<String, Arc<dyn MethodHandler>> = HashMap::new();
        for handler in handlers {
            let name = handler.name().to_owned();
            if by_name.contains_key(&name) {
                return Err(name);
            }
            by_name.insert(name, handler);
        }
        let sse_timeout = if sse_timeout.0 == 0 {
            GoDuration::from_std(DEFAULT_SSE_TIMEOUT)
        } else {
            sse_timeout
        };
        Ok(Controller {
            handlers: by_name,
            max_payload_size,
            sse_timeout,
            log,
        })
    }

    /// Go `Query`:
    /// 1. the request's [`WriteDeadline`](crate::mux::WriteDeadline) is set to now +
    ///    `sse_timeout`, or cleared (`None`) when it is negative; without the extension Go's
    ///    `SetWriteDeadline` fails and `SetWriteDeadline failed: feature not supported` is logged;
    /// 2. a present body (not already at end of stream, Go's `req.Body != http.NoBody`) is read up
    ///    to `max_payload_size` bytes; a read error answers 400
    ///    `io.ReadAll with LimitReader failed: <err>`; the bytes are decoded as `models.Query` by
    ///    [`go_unmarshal`](super::unmarshal::go_unmarshal), an error answering 400 with its text;
    ///    an absent body leaves `class_method` empty and the payload empty;
    /// 3. the payload goes to the handler named `class_method`: unknown → 500
    ///    `unrecognized class method: <name>`; a handler `Err(text)` → 500 `text`; each 500 is
    ///    also logged `handleQuery failed: <text>`. Error answers are Go `http.Error`
    ///    ([`crate::mux::http_error`]).
    pub(crate) async fn query(&self, req: Request) -> Response {
        // Go: `time.Now().Add(sseTimeout)` when positive, else the zero time (no deadline). A
        // deadline past `Instant`'s range is no deadline.
        let deadline = match self.sse_timeout.to_std() {
            Some(d) if !d.is_zero() => tokio::time::Instant::now().checked_add(d),
            _ => None,
        };
        match req.extensions().get::<WriteDeadline>() {
            Some(write_deadline) => write_deadline.set(deadline),
            None => self
                .log
                .print("SetWriteDeadline failed: feature not supported"),
        }

        let mut class_method = String::new();
        let mut payload = Bytes::new();
        if !req.body().is_end_stream() {
            payload = match read_limited(req.into_body(), self.max_payload_size).await {
                Ok(p) => p,
                Err(e) => {
                    return http_error(
                        StatusCode::BAD_REQUEST,
                        &format!("io.ReadAll with LimitReader failed: {e}"),
                    );
                }
            };
            match go_unmarshal::<Query>(&payload, &encode_shapes::MODELS_QUERY) {
                Ok(query) => class_method = query.class_method,
                Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
            }
        }

        let result = match self.handlers.get(&class_method) {
            Some(handler) => handler.handle(payload).await,
            None => Err(format!("unrecognized class method: {class_method}")),
        };
        match result {
            Ok(resp) => resp,
            Err(e) => {
                self.log.print(&format!("handleQuery failed: {e}"));
                http_error(StatusCode::INTERNAL_SERVER_ERROR, &e)
            }
        }
    }
}

/// Go `io.ReadAll(io.LimitReader(body, max))`: at most `max` bytes (none when `max <= 0`), the
/// rest left unread.
async fn read_limited(body: Body, max: i64) -> Result<Bytes, axum::Error> {
    let limit = usize::try_from(max).unwrap_or(0);
    let mut out = Vec::new();
    if limit == 0 {
        return Ok(Bytes::new());
    }
    let mut stream = body.into_data_stream();
    while out.len() < limit {
        match stream.next().await {
            None => break,
            Some(Ok(chunk)) => {
                let take = (limit - out.len()).min(chunk.len());
                out.extend_from_slice(&chunk[..take]);
            }
            Some(Err(e)) => return Err(e),
        }
    }
    Ok(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex;

    use axum::body::{Body, to_bytes};
    use bytes::Bytes;
    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value};

    use super::*;
    use crate::mux::WriteDeadline;

    /// A method handler answering `reply` (200 with that body) or `Err(text)`, recording payloads.
    struct Fake {
        name: &'static str,
        reply: Result<&'static str, &'static str>,
        seen: Mutex<Vec<Bytes>>,
    }

    impl Fake {
        fn new(name: &'static str, reply: Result<&'static str, &'static str>) -> Arc<Fake> {
            Arc::new(Fake {
                name,
                reply,
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl MethodHandler for Fake {
        fn name(&self) -> &str {
            self.name
        }

        async fn handle(&self, payload: Bytes) -> Result<Response, String> {
            self.seen.lock().unwrap().push(payload);
            match self.reply {
                Ok(body) => Ok(Response::new(Body::from(body))),
                Err(e) => Err(e.to_owned()),
            }
        }

        fn metadata(&self) -> Map<String, Value> {
            Map::new()
        }
    }

    type Lines = Arc<Mutex<Vec<String>>>;

    fn controller(
        max: i64,
        sse: GoDuration,
        handlers: Vec<Arc<dyn MethodHandler>>,
    ) -> (Controller, Lines) {
        let lines: Lines = Arc::default();
        let sink = Arc::clone(&lines);
        let log = Log(Some(Arc::new(move |m: &str| {
            sink.lock().unwrap().push(m.to_owned());
        })));
        (Controller::new(sse, max, handlers, log).unwrap(), lines)
    }

    fn post(body: Body) -> Request {
        let mut req = Request::new(body);
        *req.method_mut() = Method::POST;
        req.extensions_mut().insert(WriteDeadline::new(|_| {}));
        req
    }

    async fn parts(resp: Response) -> (u16, Vec<(String, String)>, String) {
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
            .collect();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    fn http_error_headers() -> Vec<(String, String)> {
        vec![
            (
                "content-type".to_owned(),
                "text/plain; charset=utf-8".to_owned(),
            ),
            ("x-content-type-options".to_owned(), "nosniff".to_owned()),
        ]
    }

    #[test]
    fn new_rejects_a_duplicate_name_and_defaults_the_timeout() {
        let log = Log::default();
        let dup: Vec<Arc<dyn MethodHandler>> = vec![Fake::new("a", Ok("")), Fake::new("a", Ok(""))];
        assert_eq!(
            Controller::new(GoDuration(0), 1, dup, log.clone()).err(),
            Some("a".to_owned())
        );
        let c = Controller::new(GoDuration(0), 1, vec![], log.clone()).unwrap();
        assert_eq!(c.sse_timeout, GoDuration::from_secs(120));
        let c = Controller::new(GoDuration(-1), 1, vec![], log).unwrap();
        assert_eq!(c.sse_timeout, GoDuration(-1));
    }

    #[tokio::test]
    async fn dispatches_the_whole_payload_to_the_named_method() {
        let fake = Fake::new("m", Ok("ok"));
        let (c, lines) = controller(4096, GoDuration(0), vec![fake.clone()]);
        let body = r#"{"class_method":"m","input":{"x":1}}"#;
        let resp = c.query(post(Body::from(body))).await;
        assert_eq!(parts(resp).await, (200, vec![], "ok".to_owned()));
        assert_eq!(*fake.seen.lock().unwrap(), vec![Bytes::from(body)]);
        assert!(lines.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_method_is_500_and_logged() {
        let (c, lines) = controller(4096, GoDuration(0), vec![Fake::new("m", Ok(""))]);
        let resp = c
            .query(post(Body::from(r#"{"class_method":"nope","input":{}}"#)))
            .await;
        assert_eq!(
            parts(resp).await,
            (
                500,
                http_error_headers(),
                "unrecognized class method: nope\n".to_owned()
            )
        );
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["handleQuery failed: unrecognized class method: nope".to_owned()]
        );
    }

    #[tokio::test]
    async fn handler_error_is_500_and_logged() {
        let (c, lines) = controller(4096, GoDuration(0), vec![Fake::new("m", Err("boom"))]);
        let resp = c.query(post(Body::from(r#"{"class_method":"m"}"#))).await;
        assert_eq!(
            parts(resp).await,
            (500, http_error_headers(), "boom\n".to_owned())
        );
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["handleQuery failed: boom".to_owned()]
        );
    }

    #[tokio::test]
    async fn absent_body_dispatches_the_empty_method_name() {
        let fake = Fake::new("", Ok("empty"));
        let (c, _) = controller(4096, GoDuration(0), vec![fake.clone()]);
        let resp = c.query(post(Body::empty())).await;
        assert_eq!(parts(resp).await, (200, vec![], "empty".to_owned()));
        assert_eq!(*fake.seen.lock().unwrap(), vec![Bytes::new()]);

        let (c, _) = controller(4096, GoDuration(0), vec![]);
        let resp = c.query(post(Body::from(""))).await;
        assert_eq!(
            parts(resp).await,
            (
                500,
                http_error_headers(),
                "unrecognized class method: \n".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn a_present_empty_body_is_decoded() {
        let (c, _) = controller(4096, GoDuration(0), vec![]);
        let stream = futures::stream::empty::<Result<Bytes, std::io::Error>>();
        let resp = c.query(post(Body::from_stream(stream))).await;
        assert_eq!(
            parts(resp).await,
            (
                400,
                http_error_headers(),
                "unexpected end of JSON input\n".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn payload_is_truncated_at_the_limit() {
        let (c, _) = controller(32, GoDuration(0), vec![]);
        let body = r#"{"class_method":"async_list_sessions","input":{"user_id":"u1"}}"#;
        let resp = c.query(post(Body::from(body))).await;
        assert_eq!(
            parts(resp).await,
            (
                400,
                http_error_headers(),
                "unexpected end of JSON input\n".to_owned()
            )
        );

        let fake = Fake::new("m", Ok("ok"));
        let (c, _) = controller(20, GoDuration(0), vec![fake.clone()]);
        let resp = c
            .query(post(Body::from(r#"{"class_method":"m"}   trailing"#)))
            .await;
        assert_eq!(parts(resp).await.0, 200);
        assert_eq!(
            *fake.seen.lock().unwrap(),
            vec![Bytes::from(r#"{"class_method":"m"}"#)]
        );

        for max in [0, -1] {
            let (c, _) = controller(max, GoDuration(0), vec![]);
            let resp = c.query(post(Body::from(r#"{"class_method":"m"}"#))).await;
            assert_eq!(
                parts(resp).await,
                (
                    400,
                    http_error_headers(),
                    "unexpected end of JSON input\n".to_owned()
                )
            );
        }
    }

    #[tokio::test]
    async fn decode_error_is_400_with_go_text() {
        let (c, lines) = controller(4096, GoDuration(0), vec![]);
        let resp = c.query(post(Body::from(r#"{"class_method":5}"#))).await;
        assert_eq!(
            parts(resp).await,
            (
                400,
                http_error_headers(),
                "json: cannot unmarshal number into Go struct field Query.class_method of type string\n"
                    .to_owned()
            )
        );
        assert!(lines.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_error_is_400() {
        let (c, _) = controller(4096, GoDuration(0), vec![]);
        let stream = futures::stream::iter(vec![
            Ok(Bytes::from("{")),
            Err(std::io::Error::other("http: request body too large")),
        ]);
        let resp = c.query(post(Body::from_stream(stream))).await;
        assert_eq!(
            parts(resp).await,
            (
                400,
                http_error_headers(),
                "io.ReadAll with LimitReader failed: http: request body too large\n".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn a_read_error_past_the_limit_is_not_seen() {
        let fake = Fake::new("m", Ok("ok"));
        let (c, _) = controller(20, GoDuration(0), vec![fake.clone()]);
        let stream = futures::stream::iter(vec![
            Ok(Bytes::from(r#"{"class_method":"m"}"#)),
            Err(std::io::Error::other("late")),
        ]);
        let resp = c.query(post(Body::from_stream(stream))).await;
        assert_eq!(parts(resp).await.0, 200);
    }

    #[tokio::test]
    async fn write_deadline_is_set_on_every_answer() {
        for (sse, want) in [
            (GoDuration(0), Some(std::time::Duration::from_secs(120))),
            (
                GoDuration::from_secs(30),
                Some(std::time::Duration::from_secs(30)),
            ),
            (GoDuration(-1), None),
        ] {
            let (c, _) = controller(4096, sse, vec![]);
            let recorded: Arc<Mutex<Vec<Option<tokio::time::Instant>>>> = Arc::default();
            let sink = Arc::clone(&recorded);
            let mut req = Request::new(Body::from(r#"{"class_method":5}"#));
            req.extensions_mut()
                .insert(WriteDeadline::new(move |d| sink.lock().unwrap().push(d)));
            let start = tokio::time::Instant::now();
            let resp = c.query(req).await;
            let end = tokio::time::Instant::now();
            assert_eq!(resp.status().as_u16(), 400);
            let got = recorded.lock().unwrap().clone();
            assert_eq!(got.len(), 1);
            match want {
                None => assert_eq!(got[0], None),
                Some(w) => {
                    let d = got[0].unwrap();
                    assert!(d >= start + w && d <= end + w, "deadline {d:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn without_a_write_deadline_hook_go_logs_the_failure() {
        let (c, lines) = controller(4096, GoDuration(0), vec![Fake::new("m", Ok("ok"))]);
        let resp = c
            .query(Request::new(Body::from(r#"{"class_method":"m"}"#)))
            .await;
        assert_eq!(parts(resp).await.0, 200);
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["SetWriteDeadline failed: feature not supported".to_owned()]
        );
    }
}
