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

//! Test helpers: hand-built httprr traces, a scripted judge model, a fixed transcript source
//! and run builders.

use std::any::Any;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gadk_eval::{RunTranscript, ToolCall, TurnRecord};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_httprr::RecordReplay;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient};
use serde_json::Map;

use crate::error::MonitorError;
use crate::source::{ProductionRun, TranscriptQuery, TranscriptSource};

/// One HTTP exchange of a trace.
pub(crate) struct Exchange {
    method: String,
    url: String,
    request: String,
    status: u16,
    response: String,
}

/// An exchange: `method url` with body `request` answered by `status` with JSON `response`.
pub(crate) fn exchange(
    method: &str,
    url: &str,
    request: &str,
    status: u16,
    response: &str,
) -> Exchange {
    Exchange {
        method: method.into(),
        url: url.into(),
        request: request.into(),
        status,
        response: response.into(),
    }
}

/// Writes `exchanges` as an `httprr trace v1` file in `dir` and opens it for replay. Replay
/// matches method, URL and (canonical JSON) body, so an unexpected request fails.
pub(crate) fn replay(dir: &Path, exchanges: &[Exchange]) -> Arc<RecordReplay> {
    let mut trace = b"httprr trace v1\n".to_vec();
    for e in exchanges {
        let req = format!("{} {} HTTP/1.1\r\n\r\n{}", e.method, e.url, e.request);
        let reason = http::StatusCode::from_u16(e.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Unknown");
        let resp = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\n\r\n{}",
            e.status, e.response
        );
        trace.extend_from_slice(format!("{} {}\n", req.len(), resp.len()).as_bytes());
        trace.extend_from_slice(req.as_bytes());
        trace.extend_from_slice(resp.as_bytes());
    }
    let path = dir.join("trace.httprr");
    std::fs::write(&path, trace).unwrap();
    Arc::new(RecordReplay::open_with(&path, Arc::new(ReqwestClient::default()), None).unwrap())
}

/// A judge model answering call `i` with `replies[i]`; further calls are model errors.
pub(crate) struct FakeJudge {
    replies: Mutex<VecDeque<String>>,
    calls: AtomicUsize,
}

impl FakeJudge {
    pub(crate) fn new(replies: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.iter().map(|r| (*r).to_owned()).collect()),
            calls: AtomicUsize::new(0),
        })
    }

    /// The number of calls so far.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Llm for FakeJudge {
    fn name(&self) -> &str {
        "fake-judge"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self.replies.lock().unwrap().pop_front();
        futures::stream::once(async move {
            match reply {
                Some(text) => Ok(LlmResponse {
                    content: Some(Content::from_text(&text, ROLE_MODEL)),
                    ..Default::default()
                }),
                None => Err(ModelError::Other("no scripted judge reply".into())),
            }
        })
        .boxed()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A source returning `runs` as given (or the error `fail`) and recording each query.
pub(crate) struct FakeSource {
    pub(crate) runs: Vec<ProductionRun>,
    pub(crate) fail: Option<String>,
    pub(crate) queries: Mutex<Vec<TranscriptQuery>>,
}

impl FakeSource {
    pub(crate) fn new(runs: Vec<ProductionRun>) -> Self {
        Self {
            runs,
            fail: None,
            queries: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl TranscriptSource for FakeSource {
    async fn list(&self, q: &TranscriptQuery) -> Result<Vec<ProductionRun>, MonitorError> {
        self.queries.lock().unwrap().push(q.clone());
        match &self.fail {
            Some(e) => Err(MonitorError::Source(e.clone())),
            None => Ok(self.runs.clone()),
        }
    }
}

/// `secs` after the Unix epoch.
pub(crate) fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap()
}

/// A complete run `id` of agent `agent` started at `at(started)` with `turns` of
/// `(user, reply, tool names)`.
pub(crate) fn run(id: &str, started: i64, turns: &[(&str, &str, &[&str])]) -> ProductionRun {
    ProductionRun {
        invocation_id: id.into(),
        session_id: format!("session-{id}"),
        trace_id: None,
        agent: "agent".into(),
        started_at: at(started),
        transcript: RunTranscript {
            turns: turns
                .iter()
                .map(|(user, reply, calls)| TurnRecord {
                    user: (*user).into(),
                    reply: (*reply).into(),
                    calls: calls
                        .iter()
                        .map(|name| ToolCall {
                            name: (*name).into(),
                            args: Map::new(),
                        })
                        .collect(),
                })
                .collect(),
            error: None,
        },
        content_complete: true,
        notes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use gadk_model::{HttpClient, collect_body};

    use super::*;

    #[tokio::test]
    async fn replay_serves_the_exchanges() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[exchange(
                "POST",
                "https://example.test/q",
                r#"{"a": 1}"#,
                403,
                r#"{"error":{"message":"no"}}"#,
            )],
        );
        let mut req = http::Request::new(Bytes::from(r#"{"a":1}"#));
        *req.method_mut() = http::Method::POST;
        *req.uri_mut() = "https://example.test/q".parse().unwrap();
        let resp = http.send(req).await.unwrap();
        assert_eq!(resp.status(), 403);
        let body = collect_body(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], br#"{"error":{"message":"no"}}"#);
        let mut other = http::Request::new(Bytes::new());
        *other.uri_mut() = "https://example.test/other".parse().unwrap();
        assert!(http.send(other).await.is_err());
    }

    #[tokio::test]
    async fn fake_judge_replies_in_order_then_errors() {
        let judge = FakeJudge::new(&["one"]);
        let mut s = judge.generate_content(LlmRequest::default(), false);
        let text = s.next().await.unwrap().unwrap().content.unwrap().parts[0]
            .text
            .clone();
        assert_eq!(text, "one");
        let mut s = judge.generate_content(LlmRequest::default(), false);
        assert!(s.next().await.unwrap().is_err());
        assert_eq!(judge.calls(), 2);
    }

    #[tokio::test]
    async fn fake_source_records_queries() {
        let source = FakeSource::new(vec![run("a", 10, &[("hi", "hello", &[])])]);
        let q = TranscriptQuery {
            from: at(0),
            to: at(100),
            agent: None,
            limit: 5,
        };
        let runs = source.list(&q).await.unwrap();
        assert_eq!(runs[0].transcript.turns[0].reply, "hello");
        assert_eq!(source.queries.lock().unwrap()[0], q);
    }
}
