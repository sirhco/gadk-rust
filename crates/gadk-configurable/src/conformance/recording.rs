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

//! The recording types the record plugin writes and the replay plugin reads.
//!
//! The recorded request, function call and function response are the types ADK's recorder
//! holds, which yaml.v3 encodes by reflection. Here:
//! - [`LlmRequestView`] is the request as recorded (`model`, `contents`, `config`, `tools`);
//!   gadk's `LlmRequest` has no serde shape and keeps `http_options` beside `config`.
//! - [`RecordedFunctionCall`] / [`RecordedFunctionResponse`] are the function call and
//!   response with every field, and their free-form maps are [`GoMap`] (`None` = nil map), so
//!   recorded tool args keep yaml.v3's dynamic types for the replay comparison.
//!
//! The reflection-shaped `Serialize`/`Deserialize` impls and the node encoding live in
//! [`super::gonode`].

use gadk_genai::{
    Content, FunctionResponsePart, FunctionResponseScheduling, GenerateContentConfig, PartialArg,
};
use gadk_model::{LlmRequest, LlmResponse};

use crate::govalue::GoMap;

/// All recordings in chronological order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recordings {
    /// The `recordings` key.
    pub recordings: Vec<Recording>,
}

/// A single interaction recording, ordered by request timestamp.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    /// The `user_message_index` key (0-based).
    pub user_message_index: i64,
    /// The `agent_name` key.
    pub agent_name: String,
    /// The `llm_recording` key (one of this and `tool_recording`).
    pub llm_recording: Option<LlmRecording>,
    /// The `tool_recording` key (one of this and `llm_recording`).
    pub tool_recording: Option<ToolRecording>,
    /// The LLM recording's index within its user message (replay
    /// sync), `-1` for tool recordings; never serialized.
    pub index: i64,
}

/// A paired LLM request and its responses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmRecording {
    /// The `llm_request` key.
    pub llm_request: Option<LlmRequestView>,
    /// The `llm_responses` key.
    pub llm_responses: Vec<LlmResponse>,
}

/// A paired tool call and response.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolRecording {
    /// The `tool_call` key.
    pub tool_call: Option<RecordedFunctionCall>,
    /// The `tool_response` key.
    pub tool_response: Option<RecordedFunctionResponse>,
}

/// The LLM request as recorded (untagged: keys `model`, `contents`, `config`, `tools`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmRequestView {
    /// The model name.
    pub model: String,
    /// The request contents.
    pub contents: Vec<Content>,
    /// The generation config (the request's HTTP options are gadk's
    /// `LlmRequest.http_options`, never recorded).
    pub config: Option<GenerateContentConfig>,
    /// The tools map: nil after the record plugin's sanitizing (yaml.v3
    /// encodes a nil map as `{}`); decoded but ignored by replay matching.
    pub tools: GoMap,
}

impl From<&LlmRequest> for LlmRequestView {
    /// The recorded view of `req`: model, contents and config; the sanitized tools (nil)
    /// and HTTP options (nil) are not carried.
    fn from(req: &LlmRequest) -> Self {
        LlmRequestView {
            model: req.model.clone(),
            contents: req.contents.clone(),
            config: req.config.clone(),
            tools: GoMap::new(),
        }
    }
}

/// A function call as recorded, every field in declaration order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordedFunctionCall {
    /// The call id.
    pub id: String,
    /// The arguments (`None` = nil map).
    pub args: Option<GoMap>,
    /// The function name.
    pub name: String,
    /// The partial arguments.
    pub partial_args: Vec<PartialArg>,
    /// Whether the call continues.
    pub will_continue: Option<bool>,
}

/// A function response as recorded, every field in declaration order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordedFunctionResponse {
    /// Whether the response continues.
    pub will_continue: Option<bool>,
    /// The scheduling (a non-pointer string: `None` = `""`).
    pub scheduling: Option<FunctionResponseScheduling>,
    /// The response parts.
    pub parts: Vec<FunctionResponsePart>,
    /// The call id.
    pub id: String,
    /// The function name.
    pub name: String,
    /// The response map (`None` = nil map).
    pub response: Option<GoMap>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use gadk_genai::Part;
    use gadk_model::HttpOptions;

    use super::*;

    #[test]
    fn view_drops_tools_and_http_options() {
        let mut req = LlmRequest {
            model: "gemini-2.5-flash".into(),
            contents: vec![Content {
                role: "user".into(),
                parts: vec![Part::from_text("hi")],
            }],
            config: Some(GenerateContentConfig {
                temperature: Some(0.5),
                ..Default::default()
            }),
            http_options: Some(HttpOptions::default()),
            ..Default::default()
        };
        req.tools.insert("t".into(), Arc::new(1_u8));
        let view = LlmRequestView::from(&req);
        assert_eq!(view.model, "gemini-2.5-flash");
        assert_eq!(view.contents, req.contents);
        assert_eq!(view.config, req.config);
        assert!(view.tools.is_empty());
    }

    #[test]
    fn defaults_are_go_zero_values() {
        let r = Recording::default();
        assert_eq!((r.user_message_index, r.index), (0, 0));
        assert!(r.llm_recording.is_none() && r.tool_recording.is_none());
        let fc = RecordedFunctionCall::default();
        assert!(fc.args.is_none() && fc.will_continue.is_none());
        let fr = RecordedFunctionResponse::default();
        assert!(fr.response.is_none() && fr.scheduling.is_none());
    }
}
