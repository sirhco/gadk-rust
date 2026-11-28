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

//! Batch judging: a first scoring pass records every judge request ([`RecordingJudge`]),
//! [`judge_in_batch`] grades them all in one batch prediction job ([`BatchPredictor`]), and a
//! second pass replays the outcomes ([`ReplayJudge`]).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use futures::future::BoxFuture;
use gadk_eval::{JudgeBackend, JudgeOutcome, JudgeRequest, judge_prompt, parse_judge_reply};

use crate::error::MonitorError;

/// The outcome [`RecordingJudge`] gives every request.
const RECORDED: &str = "recorded for batch";

/// The outcome [`ReplayJudge`] gives a request it has no result for.
const NO_RESULT: &str = "no batch result for this request";

/// A [`JudgeBackend`] that grades nothing: it records each distinct request and answers
/// [`JudgeOutcome::Error`] (`recorded for batch`), so the scoring pass it serves is thrown away.
///
/// [`RecordingJudge::requests`] then gives the requests to grade in one batch
/// ([`judge_in_batch`]).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RecordingJudge {
    recorded: Mutex<Recorded>,
}

/// The requests recorded so far: in first-seen order, and as a set.
#[derive(Debug, Default)]
struct Recorded {
    order: Vec<JudgeRequest>,
    seen: HashSet<JudgeRequest>,
}

impl RecordingJudge {
    /// A recorder with no requests.
    pub fn new() -> Self {
        Self::default()
    }

    /// The distinct requests recorded so far, in first-seen order, keyed `r0`, `r1`, … in that
    /// order.
    ///
    /// The order follows scoring, which runs concurrently, so use the keys only to match a
    /// batch reply to its request, never to order results.
    pub fn requests(&self) -> Vec<(String, JudgeRequest)> {
        let recorded = self.recorded.lock().unwrap_or_else(|e| e.into_inner());
        recorded
            .order
            .iter()
            .enumerate()
            .map(|(i, req)| (format!("r{i}"), req.clone()))
            .collect()
    }
}

impl JudgeBackend for RecordingJudge {
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        {
            let mut recorded = self.recorded.lock().unwrap_or_else(|e| e.into_inner());
            if recorded.seen.insert(req.clone()) {
                recorded.order.push(req.clone());
            }
        }
        Box::pin(async { JudgeOutcome::Error(RECORDED.into()) })
    }
}

/// A [`JudgeBackend`] that answers each request with a stored outcome: the outcome of an equal
/// request, else [`JudgeOutcome::Error`] (`no batch result for this request`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ReplayJudge {
    results: HashMap<JudgeRequest, JudgeOutcome>,
}

impl ReplayJudge {
    /// A judge answering from `results`.
    pub fn new(results: HashMap<JudgeRequest, JudgeOutcome>) -> Self {
        Self { results }
    }
}

impl JudgeBackend for ReplayJudge {
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        let outcome = self
            .results
            .get(req)
            .cloned()
            .unwrap_or_else(|| JudgeOutcome::Error(NO_RESULT.into()));
        Box::pin(async move { outcome })
    }
}

/// One prompt of a batch: `key` identifies its reply in [`BatchResult::replies`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BatchLine {
    /// The key of this line's reply.
    pub key: String,
    /// The prompt, sent as one user turn.
    pub prompt: String,
}

impl BatchLine {
    /// The line `key` asking `prompt`.
    pub fn new(key: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            prompt: prompt.into(),
        }
    }
}

/// The reply to one [`BatchLine`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BatchReply {
    /// The model's reply text.
    Text(String),
    /// The line failed; the text says why.
    Error(String),
}

/// A finished batch: the job that ran it and the replies by line key. A line without a reply
/// is missing from `replies`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BatchResult {
    /// The job's resource name.
    pub job: String,
    /// The replies, by [`BatchLine::key`].
    pub replies: HashMap<String, BatchReply>,
}

impl BatchResult {
    /// The result of job `job` with `replies`.
    pub fn new(job: impl Into<String>, replies: HashMap<String, BatchReply>) -> Self {
        Self {
            job: job.into(),
            replies,
        }
    }
}

/// Sends prompts as one batch job and waits for the replies.
///
/// `VertexBatchPredictor` is the built-in implementation.
/// `predict` returns a boxed future so the trait works as `dyn BatchPredictor`.
///
/// ```
/// use std::collections::HashMap;
///
/// use futures::future::BoxFuture;
/// use gadk_eval_monitor::{BatchLine, BatchPredictor, BatchReply, BatchResult, MonitorError};
///
/// /// Passes every line.
/// struct AlwaysPass;
///
/// impl BatchPredictor for AlwaysPass {
///     fn predict<'a>(
///         &'a self,
///         lines: &'a [BatchLine],
///     ) -> BoxFuture<'a, Result<BatchResult, MonitorError>> {
///         Box::pin(async move {
///             let verdict = r#"{"pass": true, "score": 5, "rationale": "fine"}"#;
///             let replies: HashMap<String, BatchReply> = lines
///                 .iter()
///                 .map(|l| (l.key.clone(), BatchReply::Text(verdict.into())))
///                 .collect();
///             Ok(BatchResult::new("jobs/1", replies))
///         })
///     }
/// }
///
/// # futures::executor::block_on(async {
/// let result = AlwaysPass.predict(&[BatchLine::new("r0", "Grade this.")]).await.unwrap();
/// assert_eq!(result.job, "jobs/1");
/// assert!(matches!(result.replies["r0"], BatchReply::Text(_)));
/// # });
/// ```
pub trait BatchPredictor: Send + Sync {
    /// Runs `lines` as one batch and returns its replies.
    ///
    /// # Errors
    ///
    /// A [`MonitorError`] when the batch as a whole fails; a failed line is a
    /// [`BatchReply::Error`].
    fn predict<'a>(
        &'a self,
        lines: &'a [BatchLine],
    ) -> BoxFuture<'a, Result<BatchResult, MonitorError>>;
}

/// Grades every request `recorder` holds in one [`BatchPredictor::predict`] call, and returns
/// the judge that replays the outcomes with the job's name.
///
/// Each request becomes one [`BatchLine`] keyed as [`RecordingJudge::requests`] keys it, with
/// [`judge_prompt`] as its prompt. A [`BatchReply::Text`] is read with [`parse_judge_reply`]; a
/// [`BatchReply::Error`] becomes [`JudgeOutcome::Error`] (`batch line error: <text>`); a request
/// without a reply is left out, so the [`ReplayJudge`] reports it as missing. With no recorded
/// requests, `predict` is not called, and the judge is empty and the name `None`.
///
/// # Errors
///
/// The error of `predict`.
pub async fn judge_in_batch(
    predictor: &dyn BatchPredictor,
    recorder: &RecordingJudge,
) -> Result<(ReplayJudge, Option<String>), MonitorError> {
    let requests = recorder.requests();
    if requests.is_empty() {
        return Ok((ReplayJudge::default(), None));
    }
    let lines: Vec<BatchLine> = requests
        .iter()
        .map(|(key, req)| BatchLine::new(key.clone(), judge_prompt(req)))
        .collect();
    let mut result = predictor.predict(&lines).await?;
    let mut results = HashMap::new();
    for (key, req) in requests {
        let outcome = match result.replies.remove(&key) {
            Some(BatchReply::Text(text)) => parse_judge_reply(&text),
            Some(BatchReply::Error(e)) => JudgeOutcome::Error(format!("batch line error: {e}")),
            None => continue,
        };
        results.insert(req, outcome);
    }
    Ok((ReplayJudge::new(results), Some(result.job)))
}

#[cfg(test)]
mod tests {
    use gadk_eval::JudgeKind;

    use crate::testutil::{FAKE_JOB, FakePredictor};

    use super::*;

    fn rubric(text: &str) -> JudgeRequest {
        JudgeRequest::new(
            JudgeKind::Rubric {
                rubric: text.into(),
            },
            "User: hi\nAgent: hello\n\n",
            "hello",
        )
    }

    fn verdict(score: u8) -> JudgeOutcome {
        JudgeOutcome::Verdict {
            pass: true,
            score,
            rationale: "fake backend".into(),
        }
    }

    /// A recorder that has seen `rubrics`, in order.
    async fn recorded(rubrics: &[&str]) -> RecordingJudge {
        let recorder = RecordingJudge::new();
        for r in rubrics {
            recorder.grade(&rubric(r)).await;
        }
        recorder
    }

    #[tokio::test]
    async fn the_recorder_keeps_distinct_requests_in_first_seen_order() {
        let recorder = RecordingJudge::new();
        for r in ["Polite", "Brief", "Polite"] {
            assert_eq!(
                recorder.grade(&rubric(r)).await,
                JudgeOutcome::Error("recorded for batch".into())
            );
        }
        assert_eq!(
            recorder.requests(),
            [
                ("r0".to_owned(), rubric("Polite")),
                ("r1".to_owned(), rubric("Brief"))
            ]
        );
    }

    #[tokio::test]
    async fn the_replay_judge_answers_hits_and_reports_misses() {
        let judge = ReplayJudge::new(HashMap::from([(rubric("Polite"), verdict(4))]));
        assert_eq!(judge.grade(&rubric("Polite")).await, verdict(4));
        assert_eq!(
            judge.grade(&rubric("Brief")).await,
            JudgeOutcome::Error("no batch result for this request".into())
        );
    }

    #[tokio::test]
    async fn replies_map_back_to_their_requests() {
        let recorder = recorded(&["Polite", "Brief"]).await;
        let predictor =
            FakePredictor::new(r#"{"pass": true, "score": 4, "rationale": "fake backend"}"#);
        let (judge, job) = judge_in_batch(predictor.as_ref(), &recorder).await.unwrap();
        assert_eq!(job.as_deref(), Some(FAKE_JOB));
        assert_eq!(judge.grade(&rubric("Polite")).await, verdict(4));
        assert_eq!(judge.grade(&rubric("Brief")).await, verdict(4));
        let calls = predictor.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            [
                BatchLine::new("r0", judge_prompt(&rubric("Polite"))),
                BatchLine::new("r1", judge_prompt(&rubric("Brief")))
            ]
        );
    }

    #[tokio::test]
    async fn line_errors_unparseable_replies_and_missing_keys() {
        let recorder = recorded(&["Polite", "Brief", "Kind"]).await;
        let predictor = FakePredictor::with(|line| match line.key.as_str() {
            "r0" => Some(BatchReply::Error("quota".into())),
            "r1" => Some(BatchReply::Text("I think it passes.".into())),
            _ => None,
        });
        let (judge, _) = judge_in_batch(predictor.as_ref(), &recorder).await.unwrap();
        assert_eq!(
            judge.grade(&rubric("Polite")).await,
            JudgeOutcome::Error("batch line error: quota".into())
        );
        assert_eq!(
            judge.grade(&rubric("Brief")).await,
            JudgeOutcome::Error("unparseable judge reply: I think it passes.".into())
        );
        assert_eq!(
            judge.grade(&rubric("Kind")).await,
            JudgeOutcome::Error("no batch result for this request".into())
        );
    }

    #[tokio::test]
    async fn no_requests_means_no_job() {
        let predictor = FakePredictor::new("unused");
        let (judge, job) = judge_in_batch(predictor.as_ref(), &RecordingJudge::new())
            .await
            .unwrap();
        assert_eq!(job, None);
        assert!(predictor.calls().is_empty());
        assert_eq!(
            judge.grade(&rubric("Polite")).await,
            JudgeOutcome::Error("no batch result for this request".into())
        );
    }

    #[tokio::test]
    async fn a_failed_batch_is_an_error() {
        let recorder = recorded(&["Polite"]).await;
        let predictor = FakePredictor::failing("batch: HTTP 403: denied");
        let err = judge_in_batch(predictor.as_ref(), &recorder)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "judge: batch: HTTP 403: denied");
    }
}
