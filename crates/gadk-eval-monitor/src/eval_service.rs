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

//! The Vertex AI Gen AI evaluation service (`evaluateInstances`) as a judge: pointwise grades
//! for `llm_judge` and `reference` scorers ([`EvalServiceJudge`] as a [`JudgeBackend`]) and
//! pairwise comparisons of two conversations ([`PairwiseJudge`]).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use gadk_eval::{JudgeBackend, JudgeKind, JudgeOutcome, JudgeRequest};
use gadk_model::HttpClient;
use http::Method;
use serde_json::{Value, json};

use crate::error::MonitorError;
use crate::gcp::{AdcTokenSource, GcpHttp, TokenSource, encode, excerpt};
use crate::spec::is_location;

/// The API version of the `evaluateInstances` endpoint.
const API_VERSION: &str = "v1";

/// The host of the `global` location; other locations use `{location}-aiplatform.googleapis.com`.
const GLOBAL_HOST: &str = "aiplatform.googleapis.com";

/// The criteria of a `reference` grade.
const REFERENCE_CRITERIA: &str = "The answer agrees in meaning with the reference answer and \
does not contradict it or omit its key facts.";

/// The pointwise template before the criteria.
const POINTWISE_HEAD: &str = "# Instruction\nYou are an expert evaluator. Rate the AI response \
against the criteria.\n\n# Evaluation\n## Criteria\n";

/// The pointwise rating rubric, 5 (best) down to 1 (worst).
const POINTWISE_RUBRIC: &str = "\n\n## Rating Rubric\n5: fully meets the criteria\n4: mostly \
meets\n3: partly meets\n2: barely meets\n1: does not meet\n\n";

/// The steps and inputs of a rubric (`llm_judge`) grade.
const RUBRIC_TAIL: &str = "## Evaluation Steps\nSTEP 1: Read the conversation and response.\n\
STEP 2: Rate the response from 1 to 5 using the rubric.\n\n\
# User Inputs and AI-generated Response\n## Conversation\n{conversation}\n\n\
## AI-generated Response\n{response}\n";

/// The steps and inputs of a `reference` grade.
const REFERENCE_TAIL: &str = "## Evaluation Steps\nSTEP 1: Read the question, the response and \
the reference answer.\nSTEP 2: Rate the response from 1 to 5 using the rubric.\n\n\
# User Inputs and AI-generated Response\n## Question\n{question}\n\n\
## AI-generated Response\n{response}\n\n## Reference Answer\n{reference}\n";

/// The pairwise template before the criteria.
const PAIRWISE_HEAD: &str = "# Instruction\nYou are an expert evaluator. Compare two AI agent \
conversations, a baseline and a candidate, against the criteria, and choose the better one.\n\n\
# Evaluation\n## Criteria\n";

/// The pairwise rubric, steps and inputs; `A` is the baseline and `B` the candidate.
const PAIRWISE_TAIL: &str = "\n\n## Rating Rubric\n\"A\": the baseline conversation is better.\n\
\"SAME\": both are equally good.\n\"B\": the candidate conversation is better.\n\n\
## Evaluation Steps\nSTEP 1: Read both conversations.\nSTEP 2: Judge each against the \
criteria.\nSTEP 3: Choose \"A\", \"SAME\" or \"B\" and explain why.\n\n\
# User Inputs and AI-generated Responses\n## Baseline conversation (A)\n{baseline}\n\n\
## Candidate conversation (B)\n{candidate}\n";

/// Which of two conversations a [`PairwiseJudge`] prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairwiseChoice {
    /// The baseline is better.
    Baseline,
    /// The candidate is better.
    Candidate,
    /// Both are equally good.
    Tie,
}

/// A [`PairwiseJudge`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairwiseOutcome {
    /// The judge chose.
    Choice {
        /// The preferred conversation.
        choice: PairwiseChoice,
        /// Why.
        explanation: String,
    },
    /// The judge could not compare; the text says why.
    Error(String),
}

/// Compares a baseline and a candidate conversation against criteria.
///
/// [`EvalServiceJudge`] is the built-in implementation. `compare` returns a boxed future so the
/// trait works as `dyn PairwiseJudge`.
///
/// ```
/// use futures::future::BoxFuture;
/// use gadk_eval_monitor::{PairwiseChoice, PairwiseJudge, PairwiseOutcome};
///
/// /// Prefers the shorter conversation.
/// struct Shorter;
///
/// impl PairwiseJudge for Shorter {
///     fn compare<'a>(
///         &'a self,
///         _criteria: &'a str,
///         baseline: &'a str,
///         candidate: &'a str,
///     ) -> BoxFuture<'a, PairwiseOutcome> {
///         Box::pin(async move {
///             let choice = match baseline.len().cmp(&candidate.len()) {
///                 std::cmp::Ordering::Less => PairwiseChoice::Baseline,
///                 std::cmp::Ordering::Greater => PairwiseChoice::Candidate,
///                 std::cmp::Ordering::Equal => PairwiseChoice::Tie,
///             };
///             PairwiseOutcome::Choice { choice, explanation: "length".into() }
///         })
///     }
/// }
///
/// # futures::executor::block_on(async {
/// let got = Shorter.compare("concise", "User: hi\nAgent: hello there\n", "User: hi\nAgent: hi\n");
/// assert_eq!(
///     got.await,
///     PairwiseOutcome::Choice { choice: PairwiseChoice::Candidate, explanation: "length".into() }
/// );
/// # });
/// ```
pub trait PairwiseJudge: Send + Sync {
    /// Compares `baseline` with `candidate` against `criteria`.
    fn compare<'a>(
        &'a self,
        criteria: &'a str,
        baseline: &'a str,
        candidate: &'a str,
    ) -> BoxFuture<'a, PairwiseOutcome>;
}

/// The Vertex AI Gen AI evaluation service as a judge.
///
/// Each [`JudgeBackend::grade`] or [`PairwiseJudge::compare`] is one `evaluateInstances` call,
/// graded by the judge model `model` and bounded by `timeout`; nothing is retried. A pointwise
/// score is rounded to the nearest integer (half away from zero) and clamped to 1–5, and the
/// verdict's `pass` flag is always set, so a scorer passes when the score reaches its
/// `min_score`.
#[non_exhaustive]
pub struct EvalServiceJudge {
    gcp: GcpHttp,
    url: String,
    autorater: String,
    timeout: Duration,
}

impl fmt::Debug for EvalServiceJudge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvalServiceJudge")
            .field("url", &self.url)
            .field("autorater", &self.autorater)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Why `location` cannot be an evaluation service location, else `Ok`: it must be `global` or
/// a region matching `[A-Za-z0-9-]+` (it becomes part of the endpoint host, which receives the
/// bearer token). The multi-regions `us` and `eu` are refused, because their endpoints use a host
/// form this client does not build. The text follows the location, as in
/// `--eval-location {text}`.
pub(crate) fn location_problem(location: &str) -> Result<(), String> {
    if location == "global" {
        Ok(())
    } else if location.eq_ignore_ascii_case("us") || location.eq_ignore_ascii_case("eu") {
        Err(format!(
            "{location} is a multi-region; use global or a region such as us-central1"
        ))
    } else if is_location(location) {
        Ok(())
    } else {
        Err(format!(
            "{location:?} is not a location; use global or a region such as us-central1"
        ))
    }
}

/// [`location_problem`] as `MonitorError::Judge("evaluation service location …")`.
fn check_location(location: &str) -> Result<(), MonitorError> {
    location_problem(location)
        .map_err(|why| MonitorError::Judge(format!("evaluation service location {why}")))
}

impl EvalServiceJudge {
    /// The service in `project` / `location`, with Application Default Credentials. Use location
    /// `global` (recommended) or a region such as `us-central1`; multi-region locations (`us`,
    /// `eu`) are rejected, because this client does not build multi-region endpoints. Must be
    /// called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Judge`] (`evaluation service location …`) when `location` is not `global`
    /// or a region matching `[A-Za-z0-9-]+`; [`MonitorError::Judge`]
    /// (`Application Default Credentials: …`) when no credentials can be built.
    pub fn new(
        project: &str,
        location: &str,
        model: &str,
        timeout: Duration,
    ) -> Result<Self, MonitorError> {
        check_location(location)?;
        let auth = AdcTokenSource::adc()
            .map_err(|e| MonitorError::Judge(format!("Application Default Credentials: {e}")))?;
        Self::with_http(
            project,
            location,
            model,
            timeout,
            GcpHttp::default_http(),
            Arc::new(auth),
        )
    }

    /// [`EvalServiceJudge::new`] with an explicit HTTP client and credentials (tests).
    ///
    /// # Errors
    ///
    /// [`MonitorError::Judge`] (`evaluation service location …`) when `location` is not `global`
    /// or a region matching `[A-Za-z0-9-]+`, as [`EvalServiceJudge::new`].
    pub fn with_http(
        project: &str,
        location: &str,
        model: &str,
        timeout: Duration,
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn TokenSource>,
    ) -> Result<Self, MonitorError> {
        check_location(location)?;
        let host = if location == "global" {
            GLOBAL_HOST.to_owned()
        } else {
            format!("{location}-{GLOBAL_HOST}")
        };
        Ok(EvalServiceJudge {
            gcp: GcpHttp::new(http, auth),
            url: format!(
                "https://{host}/{API_VERSION}/projects/{}/locations/{}:evaluateInstances",
                encode(project),
                encode(location)
            ),
            autorater: format!(
                "projects/{project}/locations/{location}/publishers/google/models/{model}"
            ),
            timeout,
        })
    }

    /// Sends `body` with the judge model added; the 2xx reply's body, else the error text.
    async fn call(&self, mut body: Value) -> Result<Bytes, String> {
        body["autoraterConfig"] = json!({ "autoraterModel": self.autorater });
        let bytes = Bytes::from(body.to_string());
        let send = self
            .gcp
            .send(Method::POST, &self.url, Some(("application/json", bytes)));
        let reply = match tokio::time::timeout(self.timeout, send).await {
            Err(_) => {
                return Err(format!(
                    "evaluation service timed out after {:?}",
                    self.timeout
                ));
            }
            Ok(Err(e)) => return Err(format!("evaluation service: {e}")),
            Ok(Ok(reply)) => reply,
        };
        if !reply.ok() {
            return Err(format!("evaluation service: {}", reply.error_text()));
        }
        Ok(reply.body)
    }
}

impl JudgeBackend for EvalServiceJudge {
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        Box::pin(async move {
            let Some(body) = pointwise_body(req) else {
                return JudgeOutcome::Error(
                    "evaluation service: this kind of judge request is not supported".into(),
                );
            };
            match self.call(body).await {
                Ok(reply) => pointwise_outcome(&reply),
                Err(e) => JudgeOutcome::Error(e),
            }
        })
    }
}

impl PairwiseJudge for EvalServiceJudge {
    fn compare<'a>(
        &'a self,
        criteria: &'a str,
        baseline: &'a str,
        candidate: &'a str,
    ) -> BoxFuture<'a, PairwiseOutcome> {
        Box::pin(async move {
            match self
                .call(pairwise_body(criteria, baseline, candidate))
                .await
            {
                Ok(reply) => pairwise_outcome(&reply),
                Err(e) => PairwiseOutcome::Error(e),
            }
        })
    }
}

/// The `pointwiseMetricInput` request for `req` (without the judge model); `None` for a kind
/// the service does not grade.
fn pointwise_body(req: &JudgeRequest) -> Option<Value> {
    let (criteria, tail, instance) = match &req.kind {
        JudgeKind::Rubric { rubric } => (
            rubric.as_str(),
            RUBRIC_TAIL,
            json!({ "conversation": req.conversation, "response": req.reply }),
        ),
        JudgeKind::Reference { question, answer } => (
            REFERENCE_CRITERIA,
            REFERENCE_TAIL,
            json!({ "question": question, "response": req.reply, "reference": answer }),
        ),
        _ => return None,
    };
    let template = format!("{POINTWISE_HEAD}{criteria}{POINTWISE_RUBRIC}{tail}");
    Some(json!({
        "pointwiseMetricInput": {
            "metricSpec": { "metricPromptTemplate": template },
            "instance": { "jsonInstance": instance.to_string() },
        }
    }))
}

/// The `pairwiseMetricInput` request (without the judge model).
fn pairwise_body(criteria: &str, baseline: &str, candidate: &str) -> Value {
    let instance = json!({ "baseline": baseline, "candidate": candidate });
    json!({
        "pairwiseMetricInput": {
            "metricSpec": {
                "metricPromptTemplate": format!("{PAIRWISE_HEAD}{criteria}{PAIRWISE_TAIL}"),
            },
            "instance": { "jsonInstance": instance.to_string() },
        }
    })
}

/// The verdict in a pointwise reply: the score rounded and clamped to 1–5.
fn pointwise_outcome(body: &[u8]) -> JudgeOutcome {
    let reply: Value = serde_json::from_slice(body).unwrap_or_default();
    let result = reply.get("pointwiseMetricResult");
    let Some(score) = result.and_then(|r| r.get("score")).and_then(Value::as_f64) else {
        return JudgeOutcome::Error(format!(
            "evaluation service: no score in reply: {}",
            excerpt(body)
        ));
    };
    JudgeOutcome::Verdict {
        pass: true,
        score: score.round().clamp(1.0, 5.0) as u8,
        rationale: explanation(result),
    }
}

/// The choice in a pairwise reply.
fn pairwise_outcome(body: &[u8]) -> PairwiseOutcome {
    let reply: Value = serde_json::from_slice(body).unwrap_or_default();
    let result = reply.get("pairwiseMetricResult");
    let choice = match result
        .and_then(|r| r.get("pairwiseChoice"))
        .and_then(Value::as_str)
    {
        Some("BASELINE") => PairwiseChoice::Baseline,
        Some("CANDIDATE") => PairwiseChoice::Candidate,
        Some("TIE") => PairwiseChoice::Tie,
        _ => {
            return PairwiseOutcome::Error(format!(
                "evaluation service: no choice in reply: {}",
                excerpt(body)
            ));
        }
    };
    PairwiseOutcome::Choice {
        choice,
        explanation: explanation(result),
    }
}

/// The result's `explanation`, or `""`.
fn explanation(result: Option<&Value>) -> String {
    result
        .and_then(|r| r.get("explanation"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use gadk_model::rest::test_util::{FakeResponse, ScriptedClient, bearer_credentials};

    use super::*;
    use crate::gcp::BODY_QUOTE_MAX;

    const MODEL: &str = "gemini-3.8-flash";

    /// The live-verified pointwise template for the rubric
    /// "The agent answers the question correctly and politely.".
    const VERIFIED_POINTWISE: &str = "# Instruction\nYou are an expert evaluator. Rate the AI response against the criteria.\n\n# Evaluation\n## Criteria\nThe agent answers the question correctly and politely.\n\n## Rating Rubric\n5: fully meets the criteria\n4: mostly meets\n3: partly meets\n2: barely meets\n1: does not meet\n\n## Evaluation Steps\nSTEP 1: Read the conversation and response.\nSTEP 2: Rate the response from 1 to 5 using the rubric.\n\n# User Inputs and AI-generated Response\n## Conversation\n{conversation}\n\n## AI-generated Response\n{response}\n";

    /// The live-verified pairwise template for the default experiment criteria.
    const VERIFIED_PAIRWISE: &str = "# Instruction\nYou are an expert evaluator. Compare two AI agent conversations, a baseline and a candidate, against the criteria, and choose the better one.\n\n# Evaluation\n## Criteria\nWhich conversation's agent is more helpful and correct, and follows the user's instructions better?\n\n## Rating Rubric\n\"A\": the baseline conversation is better.\n\"SAME\": both are equally good.\n\"B\": the candidate conversation is better.\n\n## Evaluation Steps\nSTEP 1: Read both conversations.\nSTEP 2: Judge each against the criteria.\nSTEP 3: Choose \"A\", \"SAME\" or \"B\" and explain why.\n\n# User Inputs and AI-generated Responses\n## Baseline conversation (A)\n{baseline}\n\n## Candidate conversation (B)\n{candidate}\n";

    fn judge(location: &str, client: Arc<ScriptedClient>) -> EvalServiceJudge {
        let auth = AdcTokenSource::from_credentials(bearer_credentials("tok"));
        EvalServiceJudge::with_http(
            "my-proj",
            location,
            MODEL,
            Duration::from_secs(5),
            client,
            Arc::new(auth),
        )
        .expect("a valid location")
    }

    fn rubric_request() -> JudgeRequest {
        JudgeRequest::new(
            JudgeKind::Rubric {
                rubric: "The agent answers the question correctly and politely.".into(),
            },
            "User: What is 2+2?\nAgent: \"4\"\n\n",
            "\"4\"",
        )
    }

    fn reference_request() -> JudgeRequest {
        JudgeRequest::new(
            JudgeKind::Reference {
                question: "Capital of France?".into(),
                answer: "Paris".into(),
            },
            "User: Capital of France?\nAgent: Paris.\n\n",
            "Paris.",
        )
    }

    fn pointwise(score: &str) -> Arc<ScriptedClient> {
        ScriptedClient::json(format!(
            r#"{{"pointwiseMetricResult": {{"score": {score}, "explanation": "why"}}}}"#
        ))
    }

    async fn grade(client: Arc<ScriptedClient>, req: &JudgeRequest) -> JudgeOutcome {
        judge("global", client).grade(req).await
    }

    fn verdict(score: u8) -> JudgeOutcome {
        JudgeOutcome::Verdict {
            pass: true,
            score,
            rationale: "why".into(),
        }
    }

    #[tokio::test]
    async fn rubric_request_body_url_and_credentials() {
        let client = pointwise("4");
        assert_eq!(grade(client.clone(), &rubric_request()).await, verdict(4));
        let req = client.last_request();
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://aiplatform.googleapis.com/v1/projects/my-proj/locations/global:evaluateInstances"
        );
        assert_eq!(req.header("authorization"), "Bearer tok");
        assert_eq!(req.header("content-type"), "application/json");
        let instance = json!({
            "conversation": "User: What is 2+2?\nAgent: \"4\"\n\n",
            "response": "\"4\"",
        });
        assert_eq!(
            req.json(),
            json!({
                "pointwiseMetricInput": {
                    "metricSpec": { "metricPromptTemplate": VERIFIED_POINTWISE },
                    "instance": { "jsonInstance": instance.to_string() },
                },
                "autoraterConfig": {
                    "autoraterModel":
                        "projects/my-proj/locations/global/publishers/google/models/gemini-3.8-flash",
                },
            })
        );
        let sent = req.json();
        let json_instance = sent
            .pointer("/pointwiseMetricInput/instance/jsonInstance")
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(json_instance).unwrap(),
            instance
        );
    }

    #[tokio::test]
    async fn reference_request_body() {
        let client = pointwise("5");
        assert_eq!(
            grade(client.clone(), &reference_request()).await,
            verdict(5)
        );
        let template = "# Instruction\nYou are an expert evaluator. Rate the AI response against the criteria.\n\n\
             # Evaluation\n## Criteria\nThe answer agrees in meaning with the reference answer and does \
             not contradict it or omit its key facts.\n\n## Rating Rubric\n5: fully meets the criteria\n\
             4: mostly meets\n3: partly meets\n2: barely meets\n1: does not meet\n\n\
             ## Evaluation Steps\nSTEP 1: Read the question, the response and the reference answer.\n\
             STEP 2: Rate the response from 1 to 5 using the rubric.\n\n\
             # User Inputs and AI-generated Response\n## Question\n{question}\n\n\
             ## AI-generated Response\n{response}\n\n## Reference Answer\n{reference}\n";
        let instance = json!({
            "question": "Capital of France?",
            "response": "Paris.",
            "reference": "Paris",
        });
        assert_eq!(
            client.last_request().json(),
            json!({
                "pointwiseMetricInput": {
                    "metricSpec": { "metricPromptTemplate": template },
                    "instance": { "jsonInstance": instance.to_string() },
                },
                "autoraterConfig": {
                    "autoraterModel":
                        "projects/my-proj/locations/global/publishers/google/models/gemini-3.8-flash",
                },
            })
        );
    }

    #[tokio::test]
    async fn a_regional_location_uses_the_regional_host_and_model() {
        let client = pointwise("3");
        let got = judge("us-central1", client.clone())
            .grade(&rubric_request())
            .await;
        assert_eq!(got, verdict(3));
        let req = client.last_request();
        assert_eq!(
            req.url,
            "https://us-central1-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1:evaluateInstances"
        );
        assert_eq!(
            req.json()["autoraterConfig"]["autoraterModel"],
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-3.8-flash"
        );
    }

    #[tokio::test]
    async fn scores_are_rounded_and_clamped() {
        for (raw, want) in [("3.6", 4), ("0.2", 1), ("7", 5), ("2.5", 3), ("1", 1)] {
            assert_eq!(
                grade(pointwise(raw), &rubric_request()).await,
                verdict(want),
                "{raw}"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_or_non_numeric_score_is_an_error() {
        let client = ScriptedClient::json(r#"{"pointwiseMetricResult": {"explanation": "hm"}}"#);
        assert_eq!(
            grade(client, &rubric_request()).await,
            JudgeOutcome::Error(
                r#"evaluation service: no score in reply: {"pointwiseMetricResult": {"explanation": "hm"}}"#
                    .into()
            )
        );
        let client = ScriptedClient::json(r#"{"pointwiseMetricResult": {"score": "4"}}"#);
        let JudgeOutcome::Error(e) = grade(client, &rubric_request()).await else {
            panic!("a string score is an error");
        };
        assert!(
            e.starts_with("evaluation service: no score in reply: "),
            "{e}"
        );
        let long = format!("<html>{}</html>", "x".repeat(2000));
        let client = ScriptedClient::always(FakeResponse::new(200).chunk(long));
        let JudgeOutcome::Error(e) = grade(client, &rubric_request()).await else {
            panic!("a non-JSON reply is an error");
        };
        assert_eq!(
            e.len(),
            "evaluation service: no score in reply: ".len() + BODY_QUOTE_MAX
        );
    }

    #[tokio::test]
    async fn a_non_2xx_reply_is_an_error_and_not_retried() {
        let client = ScriptedClient::always(FakeResponse::json_status(
            403,
            r#"{"error": {"code": 403, "message": "Permission denied"}}"#,
        ));
        assert_eq!(
            grade(client.clone(), &rubric_request()).await,
            JudgeOutcome::Error("evaluation service: HTTP 403: Permission denied".into())
        );
        assert_eq!(client.hits(), 1);
    }

    #[tokio::test]
    async fn a_slow_call_times_out() {
        let auth = AdcTokenSource::from_credentials(bearer_credentials("tok"));
        let slow = EvalServiceJudge::with_http(
            "my-proj",
            "global",
            MODEL,
            Duration::from_millis(20),
            ScriptedClient::always(FakeResponse::never()),
            Arc::new(auth),
        )
        .expect("a valid location");
        assert_eq!(
            slow.grade(&rubric_request()).await,
            JudgeOutcome::Error("evaluation service timed out after 20ms".into())
        );
    }

    fn pairwise_reply(choice: &str) -> Arc<ScriptedClient> {
        ScriptedClient::json(format!(
            r#"{{"pairwiseMetricResult": {{"pairwiseChoice": "{choice}", "explanation": "because"}}}}"#
        ))
    }

    const DEFAULT_CRITERIA: &str = "Which conversation's agent is more helpful and correct, and follows the user's instructions better?";

    #[tokio::test]
    async fn pairwise_request_body() {
        let client = pairwise_reply("CANDIDATE");
        let got = judge("global", client.clone())
            .compare(
                DEFAULT_CRITERIA,
                "User: q\nAgent: a\n\n",
                "User: q\nAgent: \"b\"\n\n",
            )
            .await;
        assert_eq!(
            got,
            PairwiseOutcome::Choice {
                choice: PairwiseChoice::Candidate,
                explanation: "because".into()
            }
        );
        let instance = json!({
            "baseline": "User: q\nAgent: a\n\n",
            "candidate": "User: q\nAgent: \"b\"\n\n",
        });
        let req = client.last_request();
        assert_eq!(
            req.url,
            "https://aiplatform.googleapis.com/v1/projects/my-proj/locations/global:evaluateInstances"
        );
        assert_eq!(
            req.json(),
            json!({
                "pairwiseMetricInput": {
                    "metricSpec": { "metricPromptTemplate": VERIFIED_PAIRWISE },
                    "instance": { "jsonInstance": instance.to_string() },
                },
                "autoraterConfig": {
                    "autoraterModel":
                        "projects/my-proj/locations/global/publishers/google/models/gemini-3.8-flash",
                },
            })
        );
    }

    #[tokio::test]
    async fn pairwise_choices_map_and_unknown_ones_are_errors() {
        for (raw, want) in [
            ("BASELINE", PairwiseChoice::Baseline),
            ("CANDIDATE", PairwiseChoice::Candidate),
            ("TIE", PairwiseChoice::Tie),
        ] {
            let got = judge("global", pairwise_reply(raw))
                .compare("c", "a", "b")
                .await;
            assert_eq!(
                got,
                PairwiseOutcome::Choice {
                    choice: want,
                    explanation: "because".into()
                }
            );
        }
        let got = judge("global", pairwise_reply("MAYBE"))
            .compare("c", "a", "b")
            .await;
        assert_eq!(
            got,
            PairwiseOutcome::Error(
                r#"evaluation service: no choice in reply: {"pairwiseMetricResult": {"pairwiseChoice": "MAYBE", "explanation": "because"}}"#
                    .into()
            )
        );
        let got = judge("global", ScriptedClient::json("{}"))
            .compare("c", "a", "b")
            .await;
        assert_eq!(
            got,
            PairwiseOutcome::Error("evaluation service: no choice in reply: {}".into())
        );
        let client = ScriptedClient::always(FakeResponse::json_status(500, "oops"));
        let got = judge("global", client).compare("c", "a", "b").await;
        assert_eq!(
            got,
            PairwiseOutcome::Error("evaluation service: HTTP 500: oops".into())
        );
    }

    #[test]
    fn a_bad_location_is_a_judge_error() {
        for (location, want) in [
            (
                "evil.example/x#",
                r#"evaluation service location "evil.example/x#" is not a location; use global or a region such as us-central1"#,
            ),
            (
                "",
                r#"evaluation service location "" is not a location; use global or a region such as us-central1"#,
            ),
            (
                "us",
                "evaluation service location us is a multi-region; use global or a region such as us-central1",
            ),
            (
                "eu",
                "evaluation service location eu is a multi-region; use global or a region such as us-central1",
            ),
        ] {
            let Err(MonitorError::Judge(got)) =
                EvalServiceJudge::new("my-proj", location, MODEL, Duration::from_secs(5))
            else {
                panic!("{location:?} must be refused by new");
            };
            assert_eq!(got, want, "{location:?}");
            let auth = AdcTokenSource::from_credentials(bearer_credentials("tok"));
            let Err(MonitorError::Judge(got)) = EvalServiceJudge::with_http(
                "my-proj",
                location,
                MODEL,
                Duration::from_secs(5),
                ScriptedClient::json("{}"),
                Arc::new(auth),
            ) else {
                panic!("{location:?} must be refused by with_http");
            };
            assert_eq!(got, want, "{location:?}");
        }
    }

    #[test]
    fn global_and_regions_are_locations() {
        for location in ["global", "us-central1", "europe-west4"] {
            assert_eq!(location_problem(location), Ok(()), "{location}");
            let auth = AdcTokenSource::from_credentials(bearer_credentials("tok"));
            assert!(
                EvalServiceJudge::with_http(
                    "my-proj",
                    location,
                    MODEL,
                    Duration::from_secs(5),
                    ScriptedClient::json("{}"),
                    Arc::new(auth),
                )
                .is_ok(),
                "{location}"
            );
        }
    }
}
