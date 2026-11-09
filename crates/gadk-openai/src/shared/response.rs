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

//! Response helpers both APIs share (port of ADK-Go `internal/shared/response.go`).

use futures::StreamExt as _;
use futures::stream::{self, BoxStream};
use gadk_genai::{Candidate, Content, FunctionCall, GenerateContentResponse, Part, ROLE_MODEL};
use gadk_model::{LlmResponse, ModelError};

use crate::error::{ErrorKind, OpenAiError};

/// Go `SafeInt32`: a token count narrowed to `i32`, saturating at `i32::MAX`.
pub(crate) fn safe_int32(v: i64) -> i32 {
    // Below the cap, `as` truncates exactly as Go's `int32(v)` does.
    if v > i64::from(i32::MAX) {
        i32::MAX
    } else {
        v as i32
    }
}

/// Go `SingleErrorSequence`: a response stream that yields `err` (as
/// [`ModelError::Other`]) and ends.
pub(crate) fn single_error_sequence(
    err: OpenAiError,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    stream::iter([Err(ModelError::from(err))]).boxed()
}

/// Go `IsEmptyOutput`: whether `err` is [`ErrorKind::NoOutputItems`](crate::error::ErrorKind::NoOutputItems),
/// [`ErrorKind::NoTextOrToolContent`](crate::error::ErrorKind::NoTextOrToolContent) or
/// [`ErrorKind::NoChoices`](crate::error::ErrorKind::NoChoices).
pub(crate) fn is_empty_output(err: &OpenAiError) -> bool {
    err.is(ErrorKind::NoOutputItems)
        || err.is(ErrorKind::NoTextOrToolContent)
        || err.is(ErrorKind::NoChoices)
}

/// Go `CarriesContent`: whether the response has content with at least one part.
pub(crate) fn carries_content(resp: Option<&LlmResponse>) -> bool {
    resp.and_then(|resp| resp.content.as_ref())
        .is_some_and(|content| !content.parts.is_empty())
}

/// Go `PartsWithoutCalls`: the parts that are not function calls, the calls (both in stream
/// order), and the index among the kept parts where the first call sat (the number of kept
/// parts when there is none). `None` is `(vec![], vec![], 0)`.
pub(crate) fn parts_without_calls(
    content: Option<&Content>,
) -> (Vec<Part>, Vec<FunctionCall>, usize) {
    let Some(content) = content else {
        return (Vec::new(), Vec::new(), 0);
    };
    let mut kept = Vec::with_capacity(content.parts.len());
    let mut calls = Vec::new();
    let mut at = None;
    for part in &content.parts {
        if let Some(call) = &part.function_call {
            at.get_or_insert(kept.len());
            calls.push(call.clone());
            continue;
        }
        kept.push(part.clone());
    }
    let at = at.unwrap_or(kept.len());
    (kept, calls, at)
}

/// Go `CompletedContentSupersedes`: whether a terminal snapshot can replace content assembled
/// from deltas (it carries visible text or a call, its text contains the aggregate's, and it
/// holds every aggregated call). `completed: None` is `false`; `aggregate: None` has no parts.
pub(crate) fn completed_content_supersedes(
    aggregate: Option<&Content>,
    completed: Option<&Content>,
) -> bool {
    let Some(completed) = completed else {
        return false;
    };
    let aggregate_parts = aggregate.map_or(&[][..], |c| c.parts.as_slice());
    let (aggregate_text, aggregate_calls) = visible_text_and_calls(aggregate_parts);
    let (completed_text, completed_calls) = visible_text_and_calls(&completed.parts);
    let usable = !completed_text.is_empty() || !completed_calls.is_empty();
    if !usable || !completed_text.contains(&aggregate_text) {
        return false;
    }
    // Every aggregated call must match a distinct completed call.
    let mut matched = vec![false; completed_calls.len()];
    aggregate_calls.iter().all(|aggregate_call| {
        let found = (0..completed_calls.len())
            .find(|&i| !matched[i] && completed_calls[i] == *aggregate_call);
        if let Some(i) = found {
            matched[i] = true;
        }
        found.is_some()
    })
}

/// The non-thought text of `parts`, concatenated, and their function calls in order.
fn visible_text_and_calls(parts: &[Part]) -> (String, Vec<&FunctionCall>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in parts {
        if !part.text.is_empty() && !part.thought {
            text.push_str(&part.text);
        }
        if let Some(call) = &part.function_call {
            calls.push(call);
        }
    }
    (text, calls)
}

/// Go `AnswerText`: the content's non-thought text, concatenated.
pub(crate) fn answer_text(content: Option<&Content>) -> String {
    content
        .map(|content| {
            content
                .parts
                .iter()
                .filter(|part| !part.thought)
                .map(|part| part.text.as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// Go `SinglePartResponse`: one streamed part as a genai response (one candidate, role
/// `model`, no finish reason).
pub(crate) fn single_part_response(part: Part) -> GenerateContentResponse {
    GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(Content {
                role: ROLE_MODEL.to_string(),
                parts: vec![part],
            }),
            ..Candidate::default()
        }],
        ..GenerateContentResponse::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn text(t: &str) -> Part {
        Part {
            text: t.to_string(),
            ..Part::default()
        }
    }

    fn call(name: &str) -> Part {
        Part {
            function_call: Some(FunctionCall {
                name: name.to_string(),
                ..FunctionCall::default()
            }),
            ..Part::default()
        }
    }

    fn content(parts: Vec<Part>) -> Content {
        Content {
            role: ROLE_MODEL.to_string(),
            parts,
        }
    }

    #[test]
    fn safe_int32_saturates() {
        assert_eq!(safe_int32(7), 7);
        assert_eq!(safe_int32(i64::from(i32::MAX) + 1), i32::MAX);
        assert_eq!(safe_int32(i64::MAX), i32::MAX);
    }

    #[tokio::test]
    async fn single_error_sequence_yields_the_error_once() {
        let items: Vec<_> = single_error_sequence(OpenAiError::sentinel(ErrorKind::NoChoices))
            .collect()
            .await;
        assert_eq!(items.len(), 1);
        match &items[0] {
            Err(err) => assert_eq!(err.to_string(), "openai: response included no choices"),
            Ok(resp) => panic!("want an error, got {resp:?}"),
        }
    }

    #[test]
    fn empty_output_is_the_three_sentinels() {
        for kind in [
            ErrorKind::NoOutputItems,
            ErrorKind::NoTextOrToolContent,
            ErrorKind::NoChoices,
        ] {
            assert!(is_empty_output(&OpenAiError::sentinel(kind)));
            assert!(is_empty_output(
                &OpenAiError::sentinel(kind).prefixed("wrapped: ")
            ));
        }
        assert!(!is_empty_output(&OpenAiError::sentinel(
            ErrorKind::EmptyResponse
        )));
    }

    #[test]
    fn carries_content_needs_a_part() {
        assert!(!carries_content(None));
        assert!(!carries_content(Some(&LlmResponse::default())));
        let empty = LlmResponse {
            content: Some(content(vec![])),
            ..LlmResponse::default()
        };
        assert!(!carries_content(Some(&empty)));
        let full = LlmResponse {
            content: Some(content(vec![text("hi")])),
            ..LlmResponse::default()
        };
        assert!(carries_content(Some(&full)));
    }

    #[test]
    fn parts_without_calls_reports_where_the_first_call_sat() {
        assert_eq!(parts_without_calls(None), (vec![], vec![], 0));
        let c = content(vec![text("a"), call("f"), text("b"), call("g")]);
        let (kept, calls, at) = parts_without_calls(Some(&c));
        assert_eq!(kept, vec![text("a"), text("b")]);
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["f", "g"]);
        assert_eq!(at, 1);
        let (_, calls, at) = parts_without_calls(Some(&content(vec![text("a"), text("b")])));
        assert!(calls.is_empty());
        assert_eq!(at, 2);
    }

    #[test]
    fn completed_content_needs_a_completed_snapshot() {
        let completed = content(vec![text("hello")]);
        assert!(!completed_content_supersedes(Some(&completed), None));
        assert!(completed_content_supersedes(None, Some(&completed)));
    }

    #[test]
    fn answer_text_skips_thoughts() {
        assert_eq!(answer_text(None), "");
        let thought = Part {
            thought: true,
            ..text("hmm ")
        };
        assert_eq!(
            answer_text(Some(&content(vec![
                thought,
                text("a"),
                call("f"),
                text("b")
            ]))),
            "ab"
        );
    }

    #[test]
    fn single_part_response_is_one_model_candidate() {
        let resp = single_part_response(text("x"));
        assert_eq!(resp.candidates.len(), 1);
        assert_eq!(resp.candidates[0].content, Some(content(vec![text("x")])));
        assert_eq!(resp.candidates[0].finish_reason, None);
    }
}
