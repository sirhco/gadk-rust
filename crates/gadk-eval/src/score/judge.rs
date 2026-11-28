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

//! The LLM judge behind the `llm_judge` and `reference` scorers.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use gadk_genai::{Content, GenerateContentConfig, ROLE_USER};
use gadk_model::{Llm, LlmRequest, ModelError};
use serde_json::Value;

#[cfg(test)]
use super::backend::score_outcome;
use super::backend::{JudgeBackend, JudgeKind, JudgeOutcome, JudgeRequest};
#[cfg(test)]
use crate::result::ScoreResult;
use crate::result::{RunTranscript, excerpt};

/// The longest unparseable judge reply quoted in an error rationale.
const RAW_REPLY_MAX: usize = 1024;

const REPLY_FORMAT: &str = "Reply with only a JSON object, no other text:\n\
{\"pass\": true or false, \"score\": an integer from 1 (worst) to 5 (best), \"rationale\": \"one or two sentences\"}";

/// A judge model, called at temperature 0 for a JSON verdict; each call is bounded by `timeout`.
pub(crate) struct Judge {
    model: Arc<dyn Llm>,
    timeout: Duration,
}

/// A parsed judge verdict.
#[derive(Debug, PartialEq)]
pub(crate) struct Verdict {
    pub(crate) pass: bool,
    pub(crate) score: u8,
    pub(crate) rationale: String,
}

/// The `llm_judge` prompt: grades `conversation` against `rubric`.
fn rubric_prompt(conversation: &str, rubric: &str) -> String {
    format!(
        "You are grading an AI agent's conversation against a rubric.\n\n\
         Rubric:\n{rubric}\n\nConversation:\n{conversation}\n{REPLY_FORMAT}"
    )
}

/// The `reference` prompt: whether `reply` to `question` agrees with the reference `answer`.
fn reference_prompt(question: &str, reply: &str, answer: &str) -> String {
    format!(
        "You are checking whether an AI agent's answer agrees in meaning with a reference \
         answer. Wording may differ; the answer must not contradict the reference or omit \
         its key facts.\n\nQuestion:\n{question}\n\nAgent answer:\n{reply}\n\n\
         Reference answer:\n{answer}\n\n{REPLY_FORMAT}"
    )
}

impl Judge {
    pub(crate) fn new(model: Arc<dyn Llm>, timeout: Duration) -> Self {
        Judge { model, timeout }
    }

    /// `llm_judge`: grades the whole conversation against `rubric`.
    #[cfg(test)]
    pub(crate) async fn rubric(
        &self,
        t: &RunTranscript,
        rubric: &str,
        min_score: u8,
    ) -> ScoreResult {
        let prompt = rubric_prompt(&render_conversation(t), rubric);
        score_outcome("llm_judge", self.ask(&prompt).await, min_score)
    }

    /// `reference`: whether the reply of `turn` is consistent with the reference `answer`.
    #[cfg(test)]
    pub(crate) async fn reference(
        &self,
        t: &RunTranscript,
        turn: Option<usize>,
        answer: &str,
        min_score: u8,
    ) -> ScoreResult {
        let question = t.turn(turn).map_or("", |r| r.user.as_str());
        let prompt = reference_prompt(question, t.reply(turn), answer);
        score_outcome("reference", self.ask(&prompt).await, min_score)
    }

    /// Grades `req` with the prompt of its kind ([`judge_prompt`]).
    pub(crate) async fn grade_request(&self, req: &JudgeRequest) -> JudgeOutcome {
        self.ask(&judge_prompt(req)).await
    }

    /// Asks the judge, retrying once on an unparseable reply; a model error or a call that takes
    /// longer than `timeout` is not retried.
    async fn ask(&self, prompt: &str) -> JudgeOutcome {
        let mut last = String::new();
        for _ in 0..2 {
            match tokio::time::timeout(self.timeout, self.call(prompt)).await {
                Err(_) => {
                    return JudgeOutcome::Error(format!(
                        "judge timed out after {:?}",
                        self.timeout
                    ));
                }
                Ok(Err(e)) => return JudgeOutcome::Error(format!("judge model error: {e}")),
                Ok(Ok(text)) => match parse_judge_reply(&text) {
                    JudgeOutcome::Error(e) => last = e,
                    verdict => return verdict,
                },
            }
        }
        JudgeOutcome::Error(last)
    }

    async fn call(&self, prompt: &str) -> Result<String, ModelError> {
        let req = LlmRequest {
            model: self.model.name().to_owned(),
            contents: vec![Content::from_text(prompt, ROLE_USER)],
            config: Some(GenerateContentConfig {
                temperature: Some(0.0),
                response_mime_type: "application/json".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut stream = self.model.generate_content(req, false);
        let mut text = String::new();
        while let Some(resp) = stream.next().await {
            if let Some(c) = resp?.content {
                for p in c.parts.iter().filter(|p| !p.thought) {
                    text.push_str(&p.text);
                }
            }
        }
        Ok(text)
    }
}

/// The judge backend that asks a Gemini (or any [`Llm`]) model for a JSON verdict, the one
/// [`crate::Evaluator::new`] and [`crate::score_transcript`] use.
///
/// Each call runs at temperature 0 and is bounded by `timeout`. An unparseable reply is retried
/// once; a model error or a timeout is not.
#[non_exhaustive]
pub struct GeminiJudge(Judge);

impl GeminiJudge {
    /// A backend that asks `model`, each call bounded by `timeout`.
    pub fn new(model: Arc<dyn Llm>, timeout: Duration) -> Self {
        GeminiJudge(Judge::new(model, timeout))
    }
}

impl JudgeBackend for GeminiJudge {
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        Box::pin(self.0.grade_request(req))
    }
}

/// The prompt [`GeminiJudge`] sends for `req`: the `llm_judge` prompt (the rubric and the
/// conversation) for [`JudgeKind::Rubric`], the `reference` prompt (the question, the reply and
/// the reference answer) for [`JudgeKind::Reference`]. Both end by asking for a JSON verdict
/// (`{"pass", "score", "rationale"}`), which [`parse_judge_reply`] reads.
///
/// Use it to send the judge's prompt through another channel, such as a batch prediction job.
pub fn judge_prompt(req: &JudgeRequest) -> String {
    match &req.kind {
        JudgeKind::Rubric { rubric } => rubric_prompt(&req.conversation, rubric),
        JudgeKind::Reference { question, answer } => reference_prompt(question, &req.reply, answer),
    }
}

/// The outcome of a judge's reply `text` to a [`judge_prompt`]: [`JudgeOutcome::Verdict`] when
/// the text holds a valid verdict (the whole reply, a fenced JSON block or the first JSON
/// object), else [`JudgeOutcome::Error`] with `unparseable judge reply: <the first 1024 bytes of
/// the text>`. [`GeminiJudge`] reads each reply this way; it retries once when the first reply is
/// unparseable.
pub fn parse_judge_reply(text: &str) -> JudgeOutcome {
    match parse_verdict(text) {
        Some(v) => JudgeOutcome::Verdict {
            pass: v.pass,
            score: v.score,
            rationale: v.rationale,
        },
        None => JudgeOutcome::Error(format!(
            "unparseable judge reply: {}",
            excerpt(text, RAW_REPLY_MAX)
        )),
    }
}

/// The conversation as the judge sees it: each user turn (`User: …`), the tools called
/// (`Agent tool calls: a, b`) and the reply (`Agent: …`), each turn followed by a blank line.
pub fn render_conversation(t: &RunTranscript) -> String {
    let mut out = String::new();
    for turn in &t.turns {
        let _ = writeln!(out, "User: {}", turn.user);
        if !turn.calls.is_empty() {
            let names: Vec<&str> = turn.calls.iter().map(|c| c.name.as_str()).collect();
            let _ = writeln!(out, "Agent tool calls: {}", names.join(", "));
        }
        let _ = writeln!(out, "Agent: {}\n", turn.reply);
    }
    out
}

/// Parses a verdict from the whole reply, else the first ```json (or ```) fenced block, else the
/// first JSON object in the text. `None` when none of them is a valid verdict.
pub(crate) fn parse_verdict(text: &str) -> Option<Verdict> {
    let mut candidates: Vec<&str> = vec![text.trim()];
    if let Some(start) = text.find("```") {
        let rest = &text[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```") {
            candidates.push(rest[..end].trim());
        }
    }
    if let Some(i) = text.find('{') {
        candidates.push(&text[i..]);
    }
    candidates.into_iter().find_map(|c| {
        let v = serde_json::Deserializer::from_str(c)
            .into_iter::<Value>()
            .next()?
            .ok()?;
        verdict(&v)
    })
}

fn verdict(v: &Value) -> Option<Verdict> {
    let pass = v.get("pass")?.as_bool()?;
    let score = v.get("score")?;
    // An integer, or a float with no fractional part (some models reply `4.0`).
    let score = match score.as_u64() {
        Some(n) => u8::try_from(n).ok()?,
        None => {
            let f = score
                .as_f64()
                .filter(|f| f.fract() == 0.0 && (1.0..=5.0).contains(f))?;
            f as u8
        }
    };
    let score = (1..=5).contains(&score).then_some(score)?;
    let rationale = match v.get("rationale") {
        None | Some(Value::Null) => String::new(),
        Some(r) => r.as_str()?.to_owned(),
    };
    Some(Verdict {
        pass,
        score,
        rationale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{Outcome, TurnRecord};
    use gadk_engine::testutil::MockModel;
    use gadk_genai::ROLE_MODEL;
    use gadk_model::LlmResponse;

    const T: Duration = Duration::from_secs(120);

    fn reply(text: &str) -> Content {
        Content::from_text(text, ROLE_MODEL)
    }

    fn transcript() -> RunTranscript {
        RunTranscript {
            turns: vec![
                TurnRecord {
                    user: "I want a refund".into(),
                    reply: "Which order?".into(),
                    calls: vec![],
                },
                TurnRecord {
                    user: "Order 123".into(),
                    reply: "Refunds close after 30 days; here is store credit.".into(),
                    calls: vec![crate::result::ToolCall {
                        name: "lookup_order".into(),
                        args: Default::default(),
                    }],
                },
            ],
            error: None,
        }
    }

    fn prompt_of(model: &MockModel, i: usize) -> String {
        model.requests()[i].contents[0].parts[0].text.clone()
    }

    #[test]
    fn parses_bare_fenced_and_embedded_json() {
        let want = Verdict {
            pass: true,
            score: 4,
            rationale: "ok".into(),
        };
        let bare = r#"{"pass": true, "score": 4, "rationale": "ok"}"#;
        assert_eq!(parse_verdict(bare), Some(want));
        let fenced =
            "Here you go:\n```json\n{\"pass\": true, \"score\": 4, \"rationale\": \"ok\"}\n```";
        assert_eq!(parse_verdict(fenced).map(|v| v.score), Some(4));
        let prose = "Verdict: {\"pass\": false, \"score\": 2, \"rationale\": \"no\"} — done.";
        assert_eq!(
            parse_verdict(prose).map(|v| (v.pass, v.score)),
            Some((false, 2))
        );
    }

    #[test]
    fn rejects_out_of_range_or_malformed_verdicts() {
        assert_eq!(parse_verdict(r#"{"pass": true, "score": 6}"#), None);
        assert_eq!(parse_verdict(r#"{"pass": true, "score": 0}"#), None);
        assert_eq!(parse_verdict(r#"{"pass": "yes", "score": 3}"#), None);
        assert_eq!(parse_verdict("no json here"), None);
        assert_eq!(
            parse_verdict(r#"{"pass": true, "score": 3}"#).map(|v| v.rationale),
            Some(String::new())
        );
    }

    #[test]
    fn accepts_a_whole_float_score() {
        assert_eq!(
            parse_verdict(r#"{"pass": true, "score": 4.0, "rationale": "ok"}"#).map(|v| v.score),
            Some(4)
        );
        assert_eq!(parse_verdict(r#"{"pass": true, "score": 4.5}"#), None);
        assert_eq!(parse_verdict(r#"{"pass": true, "score": 6.0}"#), None);
        assert_eq!(parse_verdict(r#"{"pass": true, "score": 0.0}"#), None);
        assert_eq!(parse_verdict(r#"{"pass": true, "score": -1.0}"#), None);
    }

    #[tokio::test]
    async fn rubric_request_and_min_score_boundary() {
        let model = MockModel::from_contents(vec![
            reply(r#"{"pass": true, "score": 4, "rationale": "good"}"#),
            reply(r#"{"pass": true, "score": 3, "rationale": "meh"}"#),
        ]);
        let judge = Judge::new(model.clone(), T);
        let r = judge.rubric(&transcript(), "Offers store credit", 4).await;
        assert_eq!(
            (r.kind.as_str(), r.outcome, r.score),
            ("llm_judge", Outcome::Pass, Some(4))
        );
        assert_eq!(r.rationale, "good");
        let r = judge.rubric(&transcript(), "Offers store credit", 4).await;
        assert_eq!((r.outcome, r.score), (Outcome::Fail, Some(3)));

        let req = &model.requests()[0];
        let cfg = req.config.as_ref().unwrap();
        assert_eq!(cfg.temperature, Some(0.0));
        assert_eq!(cfg.response_mime_type, "application/json");
        assert_eq!(req.model, "mock");
        let p = prompt_of(&model, 0);
        assert!(p.contains("Rubric:\nOffers store credit"), "{p}");
        assert!(
            p.contains("User: Order 123\nAgent tool calls: lookup_order\nAgent: Refunds"),
            "{p}"
        );
    }

    #[tokio::test]
    async fn judge_fail_verdict_fails_even_with_high_score() {
        let model = MockModel::from_contents(vec![reply(
            r#"{"pass": false, "score": 5, "rationale": "x"}"#,
        )]);
        let r = Judge::new(model, T).rubric(&transcript(), "r", 3).await;
        assert_eq!(r.outcome, Outcome::Fail);
    }

    #[tokio::test]
    async fn reference_request_carries_question_answer_and_reference() {
        let model = MockModel::from_contents(vec![reply(
            r#"{"pass": true, "score": 5, "rationale": "same"}"#,
        )]);
        let r = Judge::new(model.clone(), T)
            .reference(&transcript(), Some(2), "Refunds only within 30 days.", 3)
            .await;
        assert_eq!((r.kind.as_str(), r.outcome), ("reference", Outcome::Pass));
        let p = prompt_of(&model, 0);
        assert!(p.contains("Question:\nOrder 123"), "{p}");
        assert!(
            p.contains("Agent answer:\nRefunds close after 30 days"),
            "{p}"
        );
        assert!(
            p.contains("Reference answer:\nRefunds only within 30 days."),
            "{p}"
        );
    }

    #[tokio::test]
    async fn unparseable_reply_is_retried_once() {
        let model = MockModel::from_contents(vec![
            reply("I think it passes."),
            reply(r#"{"pass": true, "score": 4, "rationale": "ok"}"#),
        ]);
        let r = Judge::new(model.clone(), T)
            .rubric(&transcript(), "r", 3)
            .await;
        assert_eq!(r.outcome, Outcome::Pass);
        assert_eq!(model.requests().len(), 2);
    }

    #[tokio::test]
    async fn unparseable_twice_is_an_error_with_a_bounded_excerpt() {
        let garbage = "x".repeat(5000);
        let model = MockModel::from_contents(vec![reply("nope"), reply(&garbage)]);
        let r = Judge::new(model, T).rubric(&transcript(), "r", 3).await;
        assert_eq!((r.outcome, r.score), (Outcome::Error, None));
        assert!(
            r.rationale.starts_with("unparseable judge reply: xxx"),
            "{}",
            &r.rationale[..40]
        );
        assert_eq!(
            r.rationale.len(),
            "unparseable judge reply: ".len() + RAW_REPLY_MAX
        );
    }

    #[tokio::test]
    async fn model_error_is_an_error_and_not_retried() {
        let model = MockModel::new(vec![
            Err(ModelError::Other("quota".into())),
            Ok(LlmResponse {
                content: Some(reply(r#"{"pass": true, "score": 5}"#)),
                ..Default::default()
            }),
        ]);
        let r = Judge::new(model.clone(), T)
            .rubric(&transcript(), "r", 3)
            .await;
        assert_eq!(r.outcome, Outcome::Error);
        assert_eq!(r.rationale, "judge model error: quota");
        assert_eq!(model.requests().len(), 1);
    }

    #[tokio::test]
    async fn judge_prompt_is_the_prompt_the_live_judge_sends() {
        let t = transcript();
        let requests = [
            JudgeRequest::new(
                JudgeKind::Rubric {
                    rubric: "Offers store credit".into(),
                },
                render_conversation(&t),
                t.reply(None),
            ),
            JudgeRequest::new(
                JudgeKind::Reference {
                    question: "Order 123".into(),
                    answer: "Refunds only within 30 days.".into(),
                },
                render_conversation(&t),
                t.reply(Some(2)),
            ),
        ];
        for (i, req) in requests.iter().enumerate() {
            let model = MockModel::from_contents(vec![reply(
                r#"{"pass": true, "score": 4, "rationale": "ok"}"#,
            )]);
            let outcome = GeminiJudge::new(model.clone(), T).grade(req).await;
            assert!(
                matches!(outcome, JudgeOutcome::Verdict { score: 4, .. }),
                "{i}"
            );
            assert_eq!(prompt_of(&model, 0), judge_prompt(req), "{i}");
        }
        let rubric = judge_prompt(&requests[0]);
        assert!(rubric.contains("Rubric:\nOffers store credit"), "{rubric}");
        assert!(rubric.ends_with(REPLY_FORMAT), "{rubric}");
        let reference = judge_prompt(&requests[1]);
        assert!(reference.contains("Question:\nOrder 123"), "{reference}");
        assert!(reference.ends_with(REPLY_FORMAT), "{reference}");
    }

    #[test]
    fn parse_judge_reply_gives_a_verdict_or_the_unparseable_error() {
        assert_eq!(
            parse_judge_reply(
                "```json\n{\"pass\": false, \"score\": 2, \"rationale\": \"rude\"}\n```"
            ),
            JudgeOutcome::Verdict {
                pass: false,
                score: 2,
                rationale: "rude".into(),
            }
        );
        assert_eq!(
            parse_judge_reply("I think it passes."),
            JudgeOutcome::Error("unparseable judge reply: I think it passes.".into())
        );
        let JudgeOutcome::Error(e) = parse_judge_reply(&"x".repeat(5000)) else {
            panic!("a verdict from garbage");
        };
        assert_eq!(e.len(), "unparseable judge reply: ".len() + RAW_REPLY_MAX);
    }
}
