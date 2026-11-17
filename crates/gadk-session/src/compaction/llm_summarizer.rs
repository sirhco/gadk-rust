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

//! The default, model-backed [`Summarizer`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use gadk_genai::{Content, FinishReason, GenerateContentConfig, Part};
use gadk_model::{Backend, Llm, LlmRequest, ModelError};

use super::go_fmt::{as_go_floats, go_sprint};
use super::{
    CompactionConfigError, SummarizeFailure, SummarizeResult, Summarizer, content, is_prose_part,
};
use crate::Event;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The token an [`LlmSummarizer`] prompt template must contain. It is replaced with the
/// rendered event transcript.
pub const CONVERSATION_HISTORY_PLACEHOLDER: &str = "{conversation_history}";

/// The default prompt, as a single template with the placeholder.
pub(crate) const DEFAULT_PROMPT_TEMPLATE: &str = concat!(
    "The following is a conversation history between a user and an AI agent.",
    " It may or may not start from a compacted history. Please identify and",
    " reiterate the user request, summarize the context so far, focusing on",
    " key decisions made and information obtained, as well as any unresolved",
    " questions or tasks. ",
    "CRITICAL INSTRUCTIONS: ",
    "1. Explicitly identify and state the primary language used by the user ",
    "at the top of your summary (e.g., \"Conversation Language: English\"). ",
    "2. If the agent called any tools, accurately list the exact tool names ",
    "used to maintain tool grounding. ",
    "3. Maintain a section titled \"Durable facts\" listing every concrete ",
    "detail the user has stated: identifiers, names, dates, numbers, chosen ",
    "options and the reasons given for them. Copy each one verbatim. This ",
    "history may already be a summary of a summary, so any durable fact ",
    "present in the history MUST be carried forward unchanged, even if it is ",
    "old and the recent turns are about something else. Never drop or ",
    "generalize a durable fact to save space; drop narrative instead. ",
    "The rest of the summary should be concise and capture the",
    " essence of the interaction.\n\n",
    "{conversation_history}"
);

/// Default per-part character cap in the transcript.
const DEFAULT_MAX_TOOL_CONTENT_CHARS: i32 = 2000;
/// Default whole-transcript character budget.
const DEFAULT_MAX_TRANSCRIPT_CHARS: i32 = 200_000;
/// Room reserved per part for "... [truncated N chars]".
const TRUNCATION_SUFFIX_BUDGET: i32 = 32;
/// Every character that can end a transcript line.
pub(crate) const LINE_BREAKERS: &str = "\r\n\u{b}\u{c}\u{85}\u{2028}\u{2029}";

/// Settings for an [`LlmSummarizer`].
#[derive(Clone, Default)]
pub struct LlmSummarizerConfig {
    /// The model that writes the summary. Required.
    pub model: Option<Arc<dyn Llm>>,
    /// The prompt; must contain [`CONVERSATION_HISTORY_PLACEHOLDER`]. Empty uses the default,
    /// which requires concrete values to be carried forward verbatim.
    pub prompt_template: String,
    /// Per-part character cap in the transcript. 0 uses 2000; negative disables truncation.
    pub max_tool_content_chars: i32,
    /// Whole-transcript character budget. 0 uses 200 000; negative disables the budget.
    pub max_transcript_chars: i32,
    /// Bound on the model call. Zero means no timeout.
    pub timeout: Duration,
    /// Generation config; only safety settings, temperature, top-p, top-k, seed and labels
    /// carry over.
    pub generate_content_config: Option<GenerateContentConfig>,
}

/// Summarizes events by asking a model.
pub struct LlmSummarizer {
    pub(crate) model: Arc<dyn Llm>,
    pub(crate) prompt_template: String,
    pub(crate) max_tool_content_chars: i32,
    pub(crate) max_transcript_chars: i32,
    pub(crate) gen_config: Option<GenerateContentConfig>,
    pub(crate) timeout: Duration,
}

/// The error of a failed model call: `summarizer model call failed: <cause>`.
#[derive(Debug, thiserror::Error)]
enum ModelCallFailed {
    #[error("summarizer model call failed: {0}")]
    Model(#[source] ModelError),
    #[error("summarizer model call failed: context deadline exceeded")]
    DeadlineExceeded,
}

impl LlmSummarizer {
    /// Creates a summarizer; fails when the config has no model or a prompt template without
    /// [`CONVERSATION_HISTORY_PLACEHOLDER`].
    pub fn new(cfg: LlmSummarizerConfig) -> Result<LlmSummarizer, CompactionConfigError> {
        let Some(model) = cfg.model else {
            return Err(CompactionConfigError::new(
                "LLMSummarizerConfig.Model is required",
            ));
        };
        let template = if cfg.prompt_template.is_empty() {
            DEFAULT_PROMPT_TEMPLATE.to_owned()
        } else {
            cfg.prompt_template
        };
        if !template.contains(CONVERSATION_HISTORY_PLACEHOLDER) {
            return Err(CompactionConfigError::new(format!(
                "PromptTemplate must contain the placeholder \"{CONVERSATION_HISTORY_PLACEHOLDER}\""
            )));
        }
        let max_transcript = if cfg.max_transcript_chars == 0 {
            DEFAULT_MAX_TRANSCRIPT_CHARS
        } else {
            cfg.max_transcript_chars
        };
        let max_chars = if cfg.max_tool_content_chars == 0 {
            DEFAULT_MAX_TOOL_CONTENT_CHARS
        } else {
            cfg.max_tool_content_chars
        };
        Ok(LlmSummarizer {
            model,
            prompt_template: template,
            max_tool_content_chars: max_chars,
            max_transcript_chars: max_transcript,
            timeout: cfg.timeout,
            gen_config: summarizer_gen_config(cfg.generate_content_config.as_ref()),
        })
    }

    /// Calls the model with `req` and returns the first complete response that has text; a finish
    /// reason other than `Stop`, or no usable content, is an error.
    async fn call_model(&self, req: LlmRequest) -> Result<SummarizeResult, BoxError> {
        let mut responses = self.model.generate_content(req, false);
        let mut finish_reason: Option<FinishReason> = None;
        while let Some(item) = responses.next().await {
            let resp = item.map_err(ModelCallFailed::Model)?;
            // A streamed fragment is not the summary.
            if resp.partial {
                continue;
            }
            if let Some(r) = &resp.finish_reason {
                finish_reason = Some(r.clone());
            }
            if !has_text(resp.content.as_ref()) {
                continue;
            }
            if let Some(r) = finish_reason.as_ref().filter(|r| **r != FinishReason::Stop) {
                return Err(Box::new(SummarizeFailure {
                    usage: resp.usage_metadata,
                    source: format!(
                        "summarizer stopped before finishing (finish reason \"{}\"), so the summary is incomplete",
                        r.as_str()
                    )
                    .into(),
                }));
            }
            return Ok(SummarizeResult {
                content: resp.content,
                usage: resp.usage_metadata,
            });
        }
        match finish_reason {
            Some(r) => Err(format!(
                "summarizer returned no usable content (finish reason \"{}\")",
                r.as_str()
            )
            .into()),
            None => Err("summarizer returned no usable content".into()),
        }
    }

    /// One transcript line per rendered part.
    pub(crate) fn format_events(&self, events: &[Event], cap: i32) -> String {
        let mut lines: Vec<String> = Vec::new();
        for ev in events {
            let Some(c) = content(ev).filter(|c| !c.parts.is_empty()) else {
                continue;
            };
            let is_compaction = ev.actions.compaction.is_some();
            let author = escape_lines(&ev.author);
            for p in &c.parts {
                if p.thought && !p.text.is_empty() {
                    // A previous summary's own reasoning is not folded into the next one.
                    if !is_compaction {
                        lines.push(format!(
                            "{author} (thought): {}",
                            escape_lines(&self.truncate_to(&p.text, cap))
                        ));
                    }
                } else if !p.text.is_empty() {
                    lines.push(format!(
                        "{author}: {}",
                        escape_lines(&self.truncate_to(&p.text, cap))
                    ));
                }
                if let Some(fc) = &p.function_call {
                    lines.push(format!(
                        "{author} called tool: {}({})",
                        escape_lines(&fc.name),
                        escape_lines(&self.truncate_to(&stringify(&fc.args), cap))
                    ));
                }
                if let Some(fr) = &p.function_response {
                    lines.push(format!(
                        "Tool response from {}: {}",
                        escape_lines(&fr.name),
                        escape_lines(&self.truncate_to(&stringify(&fr.response), cap))
                    ));
                }
                let kind = placeholder_kind(p);
                if !kind.is_empty() {
                    lines.push(format!(
                        "{author}: [{}]",
                        escape_lines(&self.truncate_to(&kind, cap))
                    ));
                }
            }
        }
        lines.join("\n")
    }

    /// Caps `text` at `cap` characters (negative =
    /// no cap), ending with `... [truncated N chars]` when cut.
    pub(crate) fn truncate_to(&self, text: &str, cap: i32) -> String {
        let Ok(cap) = usize::try_from(cap) else {
            return text.to_owned();
        };
        if text.len() <= cap {
            return text.to_owned();
        }
        let n = text.chars().count();
        if n <= cap {
            return text.to_owned();
        }
        let kept: String = text.chars().take(cap).collect();
        format!("{kept}... [truncated {} chars]", n - cap)
    }

    /// The transcript within the budget, after
    /// one shrink pass; refused when it still does not fit.
    pub(crate) fn render_transcript(&self, events: &[Event]) -> Result<String, BoxError> {
        let mut transcript = self.format_events(events, self.max_tool_content_chars);
        let mut size = rune_count(&transcript);
        if self.max_transcript_chars < 0 || size <= self.max_transcript_chars {
            return Ok(transcript);
        }
        let parts = count_rendered_parts(events);
        if parts > 0 {
            let cap = self.max_transcript_chars / parts - self.per_part_overhead(events);
            if cap > 0 && (self.max_tool_content_chars < 0 || cap < self.max_tool_content_chars) {
                let shrunk = self.format_events(events, cap);
                let shrunk_size = rune_count(&shrunk);
                if shrunk_size < size {
                    transcript = shrunk;
                    size = shrunk_size;
                }
            }
        }
        if size <= self.max_transcript_chars {
            return Ok(transcript);
        }
        Err(format!(
            "rendered transcript is {size} characters, over the {} limit, for a window of {} events: compact a smaller window",
            self.max_transcript_chars,
            events.len()
        )
        .into())
    }

    /// The longest author (in bytes) plus ": "
    /// and the newline, plus the truncation-suffix budget.
    fn per_part_overhead(&self, events: &[Event]) -> i32 {
        let longest = events.iter().map(|ev| ev.author.len()).max().unwrap_or(0);
        i32::try_from(longest)
            .unwrap_or(i32::MAX)
            .saturating_add(3 + TRUNCATION_SUFFIX_BUDGET)
    }
}

#[async_trait]
impl Summarizer for LlmSummarizer {
    /// Summarizes `events` by calling the model on their transcript.
    async fn summarize_events(&self, events: &[Event]) -> Result<SummarizeResult, BoxError> {
        if events.is_empty() {
            return Ok(SummarizeResult::default());
        }
        let transcript = self.render_transcript(events)?;
        let prompt =
            self.prompt_template
                .replacen(CONVERSATION_HISTORY_PLACEHOLDER, &transcript, 1);
        let req = LlmRequest {
            model: self.model.name().to_owned(),
            contents: vec![Content::from_text(prompt, "user")],
            // Every call owns its config.
            config: self.gen_config.clone(),
            ..Default::default()
        };
        if self.timeout.is_zero() {
            return self.call_model(req).await;
        }
        match tokio::time::timeout(self.timeout, self.call_model(req)).await {
            Ok(res) => res,
            Err(_) => Err(Box::new(ModelCallFailed::DeadlineExceeded)),
        }
    }

    /// The type name reported on the compaction span.
    fn type_name(&self) -> &str {
        "LLMSummarizer"
    }

    /// The Google backend of the model, if any.
    fn google_variant(&self) -> Option<Backend> {
        self.model.google_variant()
    }
}

/// True for a non-thought part with non-blank text.
fn has_text(c: Option<&Content>) -> bool {
    c.is_some_and(|c| {
        c.parts
            .iter()
            .any(|p| !p.thought && !p.text.trim().is_empty())
    })
}

/// The number of characters in `s`, as the i32 the budgets use.
fn rune_count(s: &str) -> i32 {
    i32::try_from(s.chars().count()).unwrap_or(i32::MAX)
}

/// `{k1: v1, k2: v2}` with sorted keys and `%v`-style values; `""` when
/// empty.
fn stringify(v: &serde_json::Map<String, serde_json::Value>) -> String {
    if v.is_empty() {
        return String::new();
    }
    let mut keys: Vec<&String> = v.keys().collect();
    keys.sort();
    let body = keys
        .iter()
        .map(|k| format!("{k}: {}", go_sprint(&as_go_floats(&v[k.as_str()]))))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

/// Replaces every line break with `\n`; a `\r\n` pair becomes one `\n`.
fn escape_lines(text: &str) -> String {
    if !text.contains(|c| LINE_BREAKERS.contains(c)) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            out.push_str("\\n");
        } else if LINE_BREAKERS.contains(c) {
            out.push_str("\\n");
        } else {
            out.push(c);
        }
    }
    out
}

/// Keeps only what shapes prose: safety settings, temperature, top-p, top-k, seed and labels.
pub(crate) fn summarizer_gen_config(
    cfg: Option<&GenerateContentConfig>,
) -> Option<GenerateContentConfig> {
    let cfg = cfg?;
    Some(GenerateContentConfig {
        safety_settings: cfg.safety_settings.clone(),
        temperature: cfg.temperature,
        top_p: cfg.top_p,
        top_k: cfg.top_k,
        seed: cfg.seed,
        labels: cfg.labels.clone(),
        ..Default::default()
    })
}

/// The stand-in for a part the transcript cannot render.
fn placeholder_kind(p: &Part) -> String {
    if let Some(b) = &p.inline_data {
        mime_or(&b.mime_type, "inline data")
    } else if let Some(f) = &p.file_data {
        mime_or(&f.mime_type, "file")
    } else if p.executable_code.is_some() {
        "executable code".to_owned()
    } else if p.code_execution_result.is_some() {
        "code execution result".to_owned()
    } else {
        String::new()
    }
}

/// `fallback` when `mime_type` is empty, else `"<mime_type> attachment"`.
fn mime_or(mime_type: &str, fallback: &str) -> String {
    if mime_type.is_empty() {
        fallback.to_owned()
    } else {
        format!("{mime_type} attachment")
    }
}

/// How many transcript lines a window renders.
pub(crate) fn count_rendered_parts(events: &[Event]) -> i32 {
    let mut n = 0i32;
    for ev in events {
        let Some(c) = content(ev) else { continue };
        for p in &c.parts {
            if !p.text.is_empty()
                || p.function_call.is_some()
                || p.function_response.is_some()
                || is_prose_part(p)
                || !placeholder_kind(p).is_empty()
            {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn escape_lines_treats_crlf_as_one_break() {
        assert_eq!(escape_lines("a\r\nb\rc\nd\u{2028}e"), "a\\nb\\nc\\nd\\ne");
        assert_eq!(escape_lines("plain"), "plain");
    }

    #[test]
    fn default_template_ends_with_the_placeholder() {
        assert!(
            DEFAULT_PROMPT_TEMPLATE.ends_with(&format!("\n\n{CONVERSATION_HISTORY_PLACEHOLDER}"))
        );
        assert_eq!(
            DEFAULT_PROMPT_TEMPLATE
                .matches(CONVERSATION_HISTORY_PLACEHOLDER)
                .count(),
            1
        );
    }

    #[test]
    fn summarizer_gen_config_keeps_the_six_fields() {
        let input = GenerateContentConfig {
            temperature: Some(0.1),
            top_p: Some(0.2),
            top_k: Some(3.0),
            seed: Some(4),
            labels: [("k".to_owned(), "v".to_owned())].into(),
            max_output_tokens: 9,
            ..Default::default()
        };
        let got = summarizer_gen_config(Some(&input)).unwrap();
        assert_eq!(
            (
                got.temperature,
                got.top_p,
                got.top_k,
                got.seed,
                got.labels.clone(),
                got.max_output_tokens
            ),
            (
                Some(0.1),
                Some(0.2),
                Some(3.0),
                Some(4),
                input.labels.clone(),
                0
            )
        );
        assert!(summarizer_gen_config(None).is_none());
    }
}
