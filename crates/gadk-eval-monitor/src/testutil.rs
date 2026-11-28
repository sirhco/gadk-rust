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

//! Test helpers: hand-built httprr traces, a scripted judge model, a fixed judge backend and
//! pairwise judge, a scripted batch predictor, a fixed transcript source and run builders.

use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_eval::{JudgeBackend, JudgeOutcome, JudgeRequest, RunTranscript, ToolCall, TurnRecord};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_httprr::RecordReplay;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient};
use serde_json::Map;

use crate::batch::{BatchLine, BatchPredictor, BatchReply, BatchResult};
use crate::error::MonitorError;
use crate::eval_service::{PairwiseChoice, PairwiseJudge, PairwiseOutcome};
use crate::judge_rows::{JudgeRow, ScoreSink};
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

/// A judge backend that gives every request the verdict `score` (rationale `fake backend`)
/// and records the requests.
pub(crate) struct FakeBackend {
    score: u8,
    pub(crate) requests: Mutex<Vec<JudgeRequest>>,
}

impl FakeBackend {
    pub(crate) fn new(score: u8) -> Arc<Self> {
        Arc::new(Self {
            score,
            requests: Mutex::new(Vec::new()),
        })
    }

    /// The number of requests so far.
    pub(crate) fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl JudgeBackend for FakeBackend {
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        self.requests.lock().unwrap().push(req.clone());
        let score = self.score;
        Box::pin(async move {
            JudgeOutcome::Verdict {
                pass: true,
                score,
                rationale: "fake backend".into(),
            }
        })
    }
}

/// A pairwise judge giving every comparison `outcome`, recording each `(criteria, baseline,
/// candidate)` and the most calls in flight at once.
pub(crate) struct FakePairwise {
    outcome: PairwiseOutcome,
    calls: Mutex<Vec<(String, String, String)>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl FakePairwise {
    /// Chooses `choice` with the explanation `fake pairwise`.
    pub(crate) fn new(choice: PairwiseChoice) -> Arc<Self> {
        Self::with(PairwiseOutcome::Choice {
            choice,
            explanation: "fake pairwise".into(),
        })
    }

    /// Fails every comparison with `message`.
    pub(crate) fn failing(message: &str) -> Arc<Self> {
        Self::with(PairwiseOutcome::Error(message.into()))
    }

    fn with(outcome: PairwiseOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            calls: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        })
    }

    /// Every comparison so far, in call order.
    pub(crate) fn calls(&self) -> Vec<(String, String, String)> {
        self.calls.lock().unwrap().clone()
    }

    /// The most comparisons that were in flight at once.
    pub(crate) fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

impl PairwiseJudge for FakePairwise {
    fn compare<'a>(
        &'a self,
        criteria: &'a str,
        baseline: &'a str,
        candidate: &'a str,
    ) -> BoxFuture<'a, PairwiseOutcome> {
        Box::pin(async move {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            self.calls.lock().unwrap().push((
                criteria.to_owned(),
                baseline.to_owned(),
                candidate.to_owned(),
            ));
            tokio::task::yield_now().await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            self.outcome.clone()
        })
    }
}

/// The job name [`FakePredictor`] returns.
pub(crate) const FAKE_JOB: &str = "projects/p/locations/us/batchPredictionJobs/1";

/// The reply [`FakePredictor`] gives a line, if any.
type LineReply = Box<dyn Fn(&BatchLine) -> Option<BatchReply> + Send + Sync>;

/// A batch predictor answering each line with `reply(line)` (no reply when `None`) as job
/// [`FAKE_JOB`], or failing with `fail`, and recording the lines of each call.
pub(crate) struct FakePredictor {
    reply: LineReply,
    fail: Option<String>,
    calls: Mutex<Vec<Vec<BatchLine>>>,
}

impl FakePredictor {
    /// Answers every line with the text `text`.
    pub(crate) fn new(text: &str) -> Arc<Self> {
        let text = text.to_owned();
        Self::with(move |_| Some(BatchReply::Text(text.clone())))
    }

    /// Answers each line with `reply(line)`.
    pub(crate) fn with(
        reply: impl Fn(&BatchLine) -> Option<BatchReply> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            reply: Box::new(reply),
            fail: None,
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Fails every call with `MonitorError::Judge(message)`.
    pub(crate) fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            reply: Box::new(|_| None),
            fail: Some(message.to_owned()),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// The lines of each call so far.
    pub(crate) fn calls(&self) -> Vec<Vec<BatchLine>> {
        self.calls.lock().unwrap().clone()
    }
}

impl BatchPredictor for FakePredictor {
    fn predict<'a>(
        &'a self,
        lines: &'a [BatchLine],
    ) -> BoxFuture<'a, Result<BatchResult, MonitorError>> {
        self.calls.lock().unwrap().push(lines.to_vec());
        let result = match &self.fail {
            Some(e) => Err(MonitorError::Judge(e.clone())),
            None => {
                let replies: HashMap<String, BatchReply> = lines
                    .iter()
                    .filter_map(|l| (self.reply)(l).map(|r| (l.key.clone(), r)))
                    .collect();
                Ok(BatchResult::new(FAKE_JOB, replies))
            }
        };
        Box::pin(async move { result })
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

/// A score sink recording its calls: `existing` answers `present`, and `insert` records each
/// chunk with its author, or fails with the source error `fail_insert`.
#[derive(Default)]
pub(crate) struct FakeSink {
    pub(crate) present: HashSet<(String, String)>,
    pub(crate) fail_insert: Option<String>,
    /// The number of rows and the `since` of each `existing` call.
    pub(crate) existing_calls: Mutex<Vec<(usize, DateTime<Utc>)>>,
    /// The rows and author of each successful `insert` call.
    pub(crate) inserts: Mutex<Vec<(Vec<JudgeRow>, String)>>,
}

#[async_trait::async_trait]
impl ScoreSink for FakeSink {
    async fn existing(
        &self,
        rows: &[JudgeRow],
        since: DateTime<Utc>,
    ) -> Result<HashSet<(String, String)>, MonitorError> {
        self.existing_calls
            .lock()
            .unwrap()
            .push((rows.len(), since));
        Ok(self.present.clone())
    }

    async fn insert(&self, rows: &[JudgeRow], author: &str) -> Result<(), MonitorError> {
        if let Some(e) = &self.fail_insert {
            return Err(MonitorError::Source(e.clone()));
        }
        self.inserts
            .lock()
            .unwrap()
            .push((rows.to_vec(), author.to_owned()));
        Ok(())
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
        tools_complete: true,
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
