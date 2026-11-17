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
use gadk_genai::{Content, GenerateContentConfig, ROLE_USER};
use gadk_model::{Llm, LlmRequest, ModelError};
use serde_json::Value;

use crate::result::{RunTranscript, ScoreResult, excerpt};

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

impl Judge {
    pub(crate) fn new(model: Arc<dyn Llm>, timeout: Duration) -> Self {
        Judge { model, timeout }
    }

    /// `llm_judge`: grades the whole conversation against `rubric`.
    pub(crate) async fn rubric(
        &self,
        t: &RunTranscript,
        rubric: &str,
        min_score: u8,
    ) -> ScoreResult {
        let prompt = format!(
            "You are grading an AI agent's conversation against a rubric.\n\n\
             Rubric:\n{rubric}\n\nConversation:\n{}\n{REPLY_FORMAT}",
            render_conversation(t)
        );
        self.ask("llm_judge", &prompt, min_score).await
    }

    /// `reference`: whether the reply of `turn` is consistent with the reference `answer`.
    pub(crate) async fn reference(
        &self,
        t: &RunTranscript,
        turn: Option<usize>,
        answer: &str,
        min_score: u8,
    ) -> ScoreResult {
        let question = t.turn(turn).map_or("", |r| r.user.as_str());
        let prompt = format!(
            "You are checking whether an AI agent's answer agrees in meaning with a reference \
             answer. Wording may differ; the answer must not contradict the reference or omit \
             its key facts.\n\nQuestion:\n{question}\n\nAgent answer:\n{}\n\n\
             Reference answer:\n{answer}\n\n{REPLY_FORMAT}",
            t.reply(turn)
        );
        self.ask("reference", &prompt, min_score).await
    }

    /// Asks the judge, retrying once on an unparseable reply; a model error or a call that takes
    /// longer than `timeout` is not retried.
    async fn ask(&self, kind: &str, prompt: &str, min_score: u8) -> ScoreResult {
        let mut last = String::new();
        for _ in 0..2 {
            match tokio::time::timeout(self.timeout, self.call(prompt)).await {
                Err(_) => {
                    return ScoreResult::error(
                        kind,
                        format!("judge timed out after {:?}", self.timeout),
                    );
                }
                Ok(Err(e)) => return ScoreResult::error(kind, format!("judge model error: {e}")),
                Ok(Ok(text)) => match parse_verdict(&text) {
                    Some(v) => {
                        return ScoreResult::judged(
                            kind,
                            v.pass && v.score >= min_score,
                            v.score,
                            v.rationale,
                        );
                    }
                    None => last = text,
                },
            }
        }
        ScoreResult::error(
            kind,
            format!("unparseable judge reply: {}", excerpt(&last, RAW_REPLY_MAX)),
        )
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

/// The conversation as the judge sees it: each user turn, the tools called, and the reply.
fn render_conversation(t: &RunTranscript) -> String {
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
}
