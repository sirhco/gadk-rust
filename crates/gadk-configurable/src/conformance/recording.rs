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

//! Go `internal/configurable/conformance/replayplugin/recording/recording.go`: the recording
//! types the record plugin writes and the replay plugin reads.
//!
//! Go's types hold `*model.LLMRequest` and `*genai.FunctionCall` / `*genai.FunctionResponse`,
//! which yaml.v3 encodes by reflection. Here:
//! - [`LlmRequestView`] is Go's `model.LLMRequest` as recorded (`Model`, `Contents`, `Config`,
//!   `Tools`); gadk's `LlmRequest` has no serde shape and keeps `http_options` beside `config`.
//! - [`RecordedFunctionCall`] / [`RecordedFunctionResponse`] are Go's `genai.FunctionCall` /
//!   `genai.FunctionResponse` with every Go field and Go's `map[string]any` as [`GoMap`]
//!   (`None` = Go's nil map), so recorded tool args keep yaml.v3's dynamic types for Go's
//!   `cmp.Diff`.
//!
//! The Go-shaped `Serialize`/`Deserialize` impls and the node encoding live in
//! [`super::gonode`].

use gadk_genai::{
    Content, FunctionResponsePart, FunctionResponseScheduling, GenerateContentConfig, PartialArg,
};
use gadk_model::{LlmRequest, LlmResponse};

use crate::govalue::GoMap;

/// Go `Recordings`: all recordings in chronological order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recordings {
    /// Go `Recordings []Recording yaml:"recordings"`.
    pub recordings: Vec<Recording>,
}

/// Go `Recording`: a single interaction recording, ordered by request timestamp.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    /// Go `UserMessageIndex int yaml:"user_message_index"` (0-based).
    pub user_message_index: i64,
    /// Go `AgentName string yaml:"agent_name"`.
    pub agent_name: String,
    /// Go `LLMRecording *LLMRecording yaml:"llm_recording,omitempty"` (oneof).
    pub llm_recording: Option<LlmRecording>,
    /// Go `ToolRecording *ToolRecording yaml:"tool_recording,omitempty"` (oneof).
    pub tool_recording: Option<ToolRecording>,
    /// Go `Index int yaml:"-"`: the LLM recording's index within its user message (replay
    /// sync), `-1` for tool recordings; never serialized.
    pub index: i64,
}

/// Go `LLMRecording`: a paired LLM request and its responses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmRecording {
    /// Go `LLMRequest *model.LLMRequest yaml:"llm_request,omitempty"`.
    pub llm_request: Option<LlmRequestView>,
    /// Go `LLMResponses []*model.LLMResponse yaml:"llm_responses,omitempty"`.
    pub llm_responses: Vec<LlmResponse>,
}

/// Go `ToolRecording`: a paired tool call and response.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolRecording {
    /// Go `ToolCall *genai.FunctionCall yaml:"tool_call,omitempty"`.
    pub tool_call: Option<RecordedFunctionCall>,
    /// Go `ToolResponse *genai.FunctionResponse yaml:"tool_response,omitempty"`.
    pub tool_response: Option<RecordedFunctionResponse>,
}

/// Go `model.LLMRequest` as recorded (untagged: keys `model`, `contents`, `config`, `tools`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmRequestView {
    /// Go `Model string`.
    pub model: String,
    /// Go `Contents []*genai.Content`.
    pub contents: Vec<Content>,
    /// Go `Config *genai.GenerateContentConfig` (Go's `Config.HTTPOptions` is gadk's
    /// `LlmRequest.http_options`, never recorded).
    pub config: Option<GenerateContentConfig>,
    /// Go `Tools map[string]any json:"-"`: nil after the record plugin's sanitizing (yaml.v3
    /// encodes a nil map as `{}`); decoded but ignored by replay matching.
    pub tools: GoMap,
}

impl From<&LlmRequest> for LlmRequestView {
    /// The recorded view of `req`: model, contents and config; Go's sanitized `Tools` (nil)
    /// and `Config.HTTPOptions` (nil) are not carried.
    fn from(req: &LlmRequest) -> Self {
        LlmRequestView {
            model: req.model.clone(),
            contents: req.contents.clone(),
            config: req.config.clone(),
            tools: GoMap::new(),
        }
    }
}

/// Go `genai.FunctionCall` as recorded, every Go field in Go's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordedFunctionCall {
    /// Go `ID string`.
    pub id: String,
    /// Go `Args map[string]any` (`None` = nil map).
    pub args: Option<GoMap>,
    /// Go `Name string`.
    pub name: String,
    /// Go `PartialArgs []*PartialArg`.
    pub partial_args: Vec<PartialArg>,
    /// Go `WillContinue *bool`.
    pub will_continue: Option<bool>,
}

/// Go `genai.FunctionResponse` as recorded, every Go field in Go's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordedFunctionResponse {
    /// Go `WillContinue *bool`.
    pub will_continue: Option<bool>,
    /// Go `Scheduling FunctionResponseScheduling` (a non-pointer string: `None` = Go's `""`).
    pub scheduling: Option<FunctionResponseScheduling>,
    /// Go `Parts []*FunctionResponsePart`.
    pub parts: Vec<FunctionResponsePart>,
    /// Go `ID string`.
    pub id: String,
    /// Go `Name string`.
    pub name: String,
    /// Go `Response map[string]any` (`None` = nil map).
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
