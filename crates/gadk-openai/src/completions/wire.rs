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

//! Chat Completions wire types: the parts of openai-go v3.91.1 (`chatcompletion.go`,
//! `completion.go`) that ADK-Go's `internal/completions` touches, following the conventions of
//! [`crate::oaishared`] (declaration order, `omitzero`, `apijson` leniency).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::oaishared::de::{go_f64, go_i64, go_string, lenient};
use crate::oaishared::{
    FunctionDefinitionParam, ReasoningEffort, ResponseFormatJsonObjectParam,
    ResponseFormatJsonSchemaParam, ResponseFormatTextParam, tag,
};

// ---------------------------------------------------------------- request types

tag!(
    /// `"system"`.
    SystemRoleTag = "system"
);
tag!(
    /// `"developer"`.
    DeveloperRoleTag = "developer"
);
tag!(
    /// `"user"`.
    UserRoleTag = "user"
);
tag!(
    /// `"assistant"`.
    AssistantRoleTag = "assistant"
);
tag!(
    /// `"tool"`.
    ToolRoleTag = "tool"
);
tag!(
    /// `"text"`.
    TextPartTag = "text"
);
tag!(
    /// `"image_url"`.
    ImageUrlPartTag = "image_url"
);
tag!(
    /// `"file"`.
    FilePartTag = "file"
);
tag!(
    /// `"function"`.
    FunctionTag = "function"
);
tag!(
    /// `"allowed_tools"`.
    AllowedToolsTag = "allowed_tools"
);

/// openai-go `ChatCompletionNewParams`, the fields ADK-Go sets, in declaration order:
/// `messages, model, frequency_penalty, logprobs, max_completion_tokens, presence_penalty,
/// seed, temperature, top_logprobs, top_p, reasoning_effort, service_tier, stop,
/// stream_options, response_format, tool_choice, tools`. The streaming flag is not a field:
/// [`crate::client::encode_body`] appends `"stream":true` after every key, as openai-go's
/// `option.WithJSONSet("stream", true)` does.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ChatCompletionNewParams {
    /// `messages`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) messages: Vec<ChatCompletionMessageParam>,
    /// `model`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) model: String,
    /// `frequency_penalty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frequency_penalty: Option<f64>,
    /// `logprobs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) logprobs: Option<bool>,
    /// `max_completion_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<i64>,
    /// `presence_penalty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) presence_penalty: Option<f64>,
    /// `seed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seed: Option<i64>,
    /// `temperature`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// `top_logprobs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_logprobs: Option<i64>,
    /// `top_p`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f64>,
    /// `reasoning_effort` (Go `shared.ReasoningEffort`, `""` omitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    /// `service_tier`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) service_tier: Option<ChatCompletionServiceTier>,
    /// `stop`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop: Option<ChatCompletionStop>,
    /// `stream_options` (omitted when no field is set).
    #[serde(skip_serializing_if = "ChatCompletionStreamOptionsParam::is_zero")]
    pub(crate) stream_options: ChatCompletionStreamOptionsParam,
    /// `response_format`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ChatCompletionResponseFormat>,
    /// `tool_choice`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<ChatCompletionToolChoiceOption>,
    /// `tools`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ChatCompletionToolParam>,
}

/// openai-go `ChatCompletionNewParamsServiceTier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // D5: openai-go's whole value set; ADK-Go never sends `auto`, `scale` or `fast`.
pub(crate) enum ChatCompletionServiceTier {
    /// `"auto"`.
    Auto,
    /// `"default"`.
    Default,
    /// `"flex"`.
    Flex,
    /// `"scale"`.
    Scale,
    /// `"priority"`.
    Priority,
    /// `"fast"`.
    Fast,
}

/// openai-go `ChatCompletionNewParamsStopUnion` (ADK-Go sends `OfStringArray`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(dead_code)] // D5: openai-go's whole value set; ADK-Go sends only `OfStringArray`.
pub(crate) enum ChatCompletionStop {
    /// `OfString`.
    String(String),
    /// `OfStringArray`.
    StringArray(Vec<String>),
}

/// openai-go `ChatCompletionStreamOptionsParam`: `include_obfuscation, include_usage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ChatCompletionStreamOptionsParam {
    /// `include_obfuscation`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) include_obfuscation: Option<bool>,
    /// `include_usage` (ADK-Go sets it on every streamed call).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) include_usage: Option<bool>,
}

impl ChatCompletionStreamOptionsParam {
    /// Go `omitzero`: no field set.
    pub(crate) fn is_zero(&self) -> bool {
        self.include_obfuscation.is_none() && self.include_usage.is_none()
    }
}

/// openai-go `ChatCompletionMessageParamUnion`, the variants ADK-Go builds; serializes as the
/// variant's object.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionMessageParam {
    /// `OfDeveloper`.
    Developer(ChatCompletionDeveloperMessageParam),
    /// `OfSystem`.
    System(ChatCompletionSystemMessageParam),
    /// `OfUser`.
    User(ChatCompletionUserMessageParam),
    /// `OfAssistant`.
    Assistant(ChatCompletionAssistantMessageParam),
    /// `OfTool`.
    Tool(ChatCompletionToolMessageParam),
}

impl ChatCompletionMessageParam {
    /// openai-go `SystemMessage(text)`.
    pub(crate) fn system(text: impl Into<String>) -> Self {
        Self::System(ChatCompletionSystemMessageParam {
            content: ChatCompletionTextContent::String(text.into()),
            name: None,
            role: SystemRoleTag,
        })
    }

    /// openai-go `DeveloperMessage(text)`.
    pub(crate) fn developer(text: impl Into<String>) -> Self {
        Self::Developer(ChatCompletionDeveloperMessageParam {
            content: ChatCompletionTextContent::String(text.into()),
            name: None,
            role: DeveloperRoleTag,
        })
    }

    /// openai-go `UserMessage(text)`.
    pub(crate) fn user(text: impl Into<String>) -> Self {
        Self::User(ChatCompletionUserMessageParam {
            content: ChatCompletionUserMessageContent::String(text.into()),
            name: None,
            role: UserRoleTag,
        })
    }

    /// openai-go `UserMessage(parts)`.
    pub(crate) fn user_parts(parts: Vec<ChatCompletionContentPartParam>) -> Self {
        Self::User(ChatCompletionUserMessageParam {
            content: ChatCompletionUserMessageContent::ArrayOfContentParts(parts),
            name: None,
            role: UserRoleTag,
        })
    }

    /// openai-go `ToolMessage(content, toolCallID)`.
    pub(crate) fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self::Tool(ChatCompletionToolMessageParam {
            content: ChatCompletionTextContent::String(content.into()),
            tool_call_id: tool_call_id.into(),
            role: ToolRoleTag,
        })
    }
}

/// The content union of the system, developer and tool messages (openai-go
/// `ChatCompletion{System,Developer,Tool}MessageParamContentUnion`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(dead_code)] // D5: openai-go's whole value set; ADK-Go sends only `OfString`.
pub(crate) enum ChatCompletionTextContent {
    /// `OfString`.
    String(String),
    /// `OfArrayOfContentParts`.
    ArrayOfContentParts(Vec<ChatCompletionContentPartTextParam>),
}

/// openai-go `ChatCompletionSystemMessageParam`: `content, name, role`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionSystemMessageParam {
    /// `content`.
    pub(crate) content: ChatCompletionTextContent,
    /// `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// `role`: `"system"`.
    pub(crate) role: SystemRoleTag,
}

/// openai-go `ChatCompletionDeveloperMessageParam`: `content, name, role`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionDeveloperMessageParam {
    /// `content`.
    pub(crate) content: ChatCompletionTextContent,
    /// `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// `role`: `"developer"`.
    pub(crate) role: DeveloperRoleTag,
}

/// openai-go `ChatCompletionUserMessageParamContentUnion`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionUserMessageContent {
    /// `OfString`.
    String(String),
    /// `OfArrayOfContentParts`.
    ArrayOfContentParts(Vec<ChatCompletionContentPartParam>),
}

/// openai-go `ChatCompletionUserMessageParam`: `content, name, role`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionUserMessageParam {
    /// `content`.
    pub(crate) content: ChatCompletionUserMessageContent,
    /// `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// `role`: `"user"`.
    pub(crate) role: UserRoleTag,
}

/// openai-go `ChatCompletionAssistantMessageParamContentUnion` (ADK-Go sends `OfString`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionAssistantContent {
    /// `OfString`.
    String(String),
}

/// openai-go `ChatCompletionAssistantMessageParam`, the fields ADK-Go sets, in declaration
/// order: `refusal, name, content, tool_calls, role` (`audio` and `function_call` are never
/// set).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ChatCompletionAssistantMessageParam {
    /// `refusal`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refusal: Option<String>,
    /// `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// `content` (omitted when ADK-Go has no text for the turn).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<ChatCompletionAssistantContent>,
    /// `tool_calls`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<ChatCompletionMessageToolCallParam>,
    /// `role`: `"assistant"`.
    pub(crate) role: AssistantRoleTag,
}

/// openai-go `ChatCompletionToolMessageParam`: `content, tool_call_id, role`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionToolMessageParam {
    /// `content`.
    pub(crate) content: ChatCompletionTextContent,
    /// `tool_call_id` (required, always sent).
    pub(crate) tool_call_id: String,
    /// `role`: `"tool"`.
    pub(crate) role: ToolRoleTag,
}

/// openai-go `ChatCompletionContentPartUnionParam`, the variants ADK-Go builds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionContentPartParam {
    /// `OfText` (openai-go `TextContentPart`).
    Text(ChatCompletionContentPartTextParam),
    /// `OfImageURL` (openai-go `ImageContentPart`).
    ImageUrl(ChatCompletionContentPartImageParam),
    /// `OfFile` (openai-go `FileContentPart`).
    File(ChatCompletionContentPartFileParam),
}

impl ChatCompletionContentPartParam {
    /// openai-go `TextContentPart(text)`.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::Text(ChatCompletionContentPartTextParam {
            text: text.into(),
            kind: TextPartTag,
        })
    }

    /// openai-go `ImageContentPart(imageURL)`.
    pub(crate) fn image(image_url: ChatCompletionContentPartImageImageUrlParam) -> Self {
        Self::ImageUrl(ChatCompletionContentPartImageParam {
            image_url,
            kind: ImageUrlPartTag,
        })
    }

    /// openai-go `FileContentPart(file)`.
    pub(crate) fn file(file: ChatCompletionContentPartFileFileParam) -> Self {
        Self::File(ChatCompletionContentPartFileParam {
            file,
            kind: FilePartTag,
        })
    }
}

/// openai-go `ChatCompletionContentPartTextParam`: `text, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionContentPartTextParam {
    /// `text` (required, always sent).
    pub(crate) text: String,
    /// `type`: `"text"`.
    #[serde(rename = "type")]
    pub(crate) kind: TextPartTag,
}

/// openai-go `ChatCompletionContentPartImageParam`: `image_url, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionContentPartImageParam {
    /// `image_url`.
    pub(crate) image_url: ChatCompletionContentPartImageImageUrlParam,
    /// `type`: `"image_url"`.
    #[serde(rename = "type")]
    pub(crate) kind: ImageUrlPartTag,
}

/// openai-go `ChatCompletionContentPartImageImageURLParam`: `url, detail`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ChatCompletionContentPartImageImageUrlParam {
    /// `url` (required, always sent).
    pub(crate) url: String,
    /// `detail` (`omitzero` string).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) detail: String,
}

/// openai-go `ChatCompletionContentPartFileParam`: `file, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionContentPartFileParam {
    /// `file`.
    pub(crate) file: ChatCompletionContentPartFileFileParam,
    /// `type`: `"file"`.
    #[serde(rename = "type")]
    pub(crate) kind: FilePartTag,
}

/// openai-go `ChatCompletionContentPartFileFileParam`: `file_data, file_id, filename`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ChatCompletionContentPartFileFileParam {
    /// `file_data`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_data: Option<String>,
    /// `file_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_id: Option<String>,
    /// `filename`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) filename: Option<String>,
}

/// openai-go `ChatCompletionMessageToolCallUnionParam` (ADK-Go sends `OfFunction`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionMessageToolCallParam {
    /// `OfFunction`.
    Function(ChatCompletionMessageFunctionToolCallParam),
}

/// openai-go `ChatCompletionMessageFunctionToolCallParam`: `id, function, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionMessageFunctionToolCallParam {
    /// `id` (required, always sent).
    pub(crate) id: String,
    /// `function`.
    pub(crate) function: ChatCompletionMessageFunctionToolCallFunctionParam,
    /// `type`: `"function"`.
    #[serde(rename = "type")]
    pub(crate) kind: FunctionTag,
}

/// openai-go `ChatCompletionMessageFunctionToolCallFunctionParam`: `arguments, name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ChatCompletionMessageFunctionToolCallFunctionParam {
    /// `arguments` (required, always sent).
    pub(crate) arguments: String,
    /// `name` (required, always sent).
    pub(crate) name: String,
}

/// openai-go `ChatCompletionToolUnionParam` (ADK-Go sends `OfFunction`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionToolParam {
    /// `OfFunction`.
    Function(ChatCompletionFunctionToolParam),
}

/// openai-go `ChatCompletionFunctionToolParam`: `function, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionFunctionToolParam {
    /// `function`.
    pub(crate) function: FunctionDefinitionParam,
    /// `type`: `"function"`.
    #[serde(rename = "type")]
    pub(crate) kind: FunctionTag,
}

/// openai-go `ChatCompletionToolChoiceOptionUnionParam`, the variants ADK-Go builds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionToolChoiceOption {
    /// `OfAuto`: `"none"`, `"auto"` or `"required"`.
    Auto(String),
    /// `OfAllowedTools`.
    AllowedTools(ChatCompletionAllowedToolChoiceParam),
    /// `OfFunctionToolChoice`.
    FunctionToolChoice(ChatCompletionNamedToolChoiceParam),
}

/// openai-go `ChatCompletionAllowedToolChoiceParam`: `allowed_tools, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionAllowedToolChoiceParam {
    /// `allowed_tools`.
    pub(crate) allowed_tools: ChatCompletionAllowedToolsParam,
    /// `type`: `"allowed_tools"`.
    #[serde(rename = "type")]
    pub(crate) kind: AllowedToolsTag,
}

/// openai-go `ChatCompletionAllowedToolsParam`: `mode, tools`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionAllowedToolsParam {
    /// `mode`.
    pub(crate) mode: ChatCompletionAllowedToolsMode,
    /// `tools` (Go `[]map[string]any`: `{"type":"function","function":{"name":…}}`).
    pub(crate) tools: Vec<Map<String, Value>>,
}

/// openai-go `ChatCompletionAllowedToolsMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChatCompletionAllowedToolsMode {
    /// `"auto"`.
    Auto,
    /// `"required"`.
    Required,
}

/// openai-go `ChatCompletionNamedToolChoiceParam`: `function, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionNamedToolChoiceParam {
    /// `function`.
    pub(crate) function: ChatCompletionNamedToolChoiceFunctionParam,
    /// `type`: `"function"`.
    #[serde(rename = "type")]
    pub(crate) kind: FunctionTag,
}

/// openai-go `ChatCompletionNamedToolChoiceFunctionParam`: `name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ChatCompletionNamedToolChoiceFunctionParam {
    /// `name` (required, always sent).
    pub(crate) name: String,
}

/// openai-go `ChatCompletionNewParamsResponseFormatUnion`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(dead_code)] // D5: openai-go's whole value set; ADK-Go never sends `OfText`.
pub(crate) enum ChatCompletionResponseFormat {
    /// `OfText`.
    Text(ResponseFormatTextParam),
    /// `OfJSONSchema`.
    JsonSchema(ResponseFormatJsonSchemaParam),
    /// `OfJSONObject`.
    JsonObject(ResponseFormatJsonObjectParam),
}

// ---------------------------------------------------------------- response types

/// openai-go `ChatCompletion`, the fields ADK-Go and the stream accumulator read: `id,
/// choices, created, model, object, service_tier, system_fingerprint, usage`. `usage` is
/// `Some` only when present and an object (Go `JSON.Usage.Valid()`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletion {
    /// `id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) id: String,
    /// `choices`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) choices: Vec<ChatCompletionChoice>,
    /// `created`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) created: i64,
    /// `model`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) model: String,
    /// `object`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) object: String,
    /// `service_tier`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) service_tier: String,
    /// `system_fingerprint`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) system_fingerprint: String,
    /// `usage` (Go `JSON.Usage.Valid()` is `is_some()`).
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) usage: Option<CompletionUsage>,
}

/// openai-go `ChatCompletionChoice`: `finish_reason, index, logprobs, message`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChoice {
    /// `finish_reason`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) finish_reason: String,
    /// `index`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) index: i64,
    /// `logprobs`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) logprobs: ChatCompletionChoiceLogprobs,
    /// `message`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) message: ChatCompletionMessage,
}

/// openai-go `ChatCompletionChoiceLogprobs` (and `ChatCompletionChunkChoiceLogprobs`, the same
/// shape): `content, refusal`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChoiceLogprobs {
    /// `content`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) content: Vec<ChatCompletionTokenLogprob>,
    /// `refusal`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) refusal: Vec<ChatCompletionTokenLogprob>,
}

/// openai-go `ChatCompletionMessage`, the fields ADK-Go reads: `content, refusal, role,
/// tool_calls`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionMessage {
    /// `content`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) content: String,
    /// `refusal`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) refusal: String,
    /// `role`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) role: String,
    /// `tool_calls`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) tool_calls: Vec<ChatCompletionMessageToolCall>,
}

/// openai-go `ChatCompletionMessageToolCallUnion`, the fields ADK-Go reads: `id, function,
/// type`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionMessageToolCall {
    /// `id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) id: String,
    /// `function`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) function: ChatCompletionMessageFunctionToolCallFunction,
    /// `type`.
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
}

/// openai-go `ChatCompletionMessageFunctionToolCallFunction`: `arguments, name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionMessageFunctionToolCallFunction {
    /// `arguments`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) arguments: String,
    /// `name`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) name: String,
}

/// openai-go `ChatCompletionTokenLogprob`: `token, bytes, logprob, top_logprobs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionTokenLogprob {
    /// `token`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) token: String,
    /// `bytes`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) bytes: Vec<i64>,
    /// `logprob`.
    #[serde(default, deserialize_with = "go_f64")]
    pub(crate) logprob: f64,
    /// `top_logprobs`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) top_logprobs: Vec<ChatCompletionTokenLogprobTopLogprob>,
}

/// openai-go `ChatCompletionTokenLogprobTopLogprob`: `token, bytes, logprob`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionTokenLogprobTopLogprob {
    /// `token`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) token: String,
    /// `bytes`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) bytes: Vec<i64>,
    /// `logprob`.
    #[serde(default, deserialize_with = "go_f64")]
    pub(crate) logprob: f64,
}

/// openai-go `CompletionUsage`: `completion_tokens, prompt_tokens, total_tokens,
/// completion_tokens_details, prompt_tokens_details`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletionUsage {
    /// `completion_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) completion_tokens: i64,
    /// `prompt_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) prompt_tokens: i64,
    /// `total_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) total_tokens: i64,
    /// `completion_tokens_details`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) completion_tokens_details: CompletionUsageCompletionTokensDetails,
    /// `prompt_tokens_details`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) prompt_tokens_details: CompletionUsagePromptTokensDetails,
}

/// openai-go `CompletionUsageCompletionTokensDetails`: `accepted_prediction_tokens,
/// audio_tokens, reasoning_tokens, rejected_prediction_tokens, text_tokens`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletionUsageCompletionTokensDetails {
    /// `accepted_prediction_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) accepted_prediction_tokens: i64,
    /// `audio_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) audio_tokens: i64,
    /// `reasoning_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) reasoning_tokens: i64,
    /// `rejected_prediction_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) rejected_prediction_tokens: i64,
    /// `text_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) text_tokens: i64,
}

/// openai-go `CompletionUsagePromptTokensDetails`: `audio_tokens, cache_write_tokens,
/// cached_tokens, image_tokens, text_tokens`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletionUsagePromptTokensDetails {
    /// `audio_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) audio_tokens: i64,
    /// `cache_write_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) cache_write_tokens: i64,
    /// `cached_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) cached_tokens: i64,
    /// `image_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) image_tokens: i64,
    /// `text_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) text_tokens: i64,
}

// ---------------------------------------------------------------- stream types

/// openai-go `ChatCompletionChunk`, the fields ADK-Go and the stream accumulator read: `id,
/// choices, created, model, object, service_tier, system_fingerprint, usage`. `usage` is
/// `Some` only when present and an object (Go `JSON.Usage.Valid()`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunk {
    /// `id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) id: String,
    /// `choices`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) choices: Vec<ChatCompletionChunkChoice>,
    /// `created`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) created: i64,
    /// `model`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) model: String,
    /// `object`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) object: String,
    /// `service_tier`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) service_tier: String,
    /// `system_fingerprint`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) system_fingerprint: String,
    /// `usage` (Go `JSON.Usage.Valid()` is `is_some()`).
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) usage: Option<CompletionUsage>,
}

/// openai-go `ChatCompletionChunkChoice`: `delta, finish_reason, index, logprobs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunkChoice {
    /// `delta`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) delta: ChatCompletionChunkChoiceDelta,
    /// `finish_reason`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) finish_reason: String,
    /// `index`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) index: i64,
    /// `logprobs`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) logprobs: ChatCompletionChoiceLogprobs,
}

/// openai-go `ChatCompletionChunkChoiceDelta`, the fields ADK-Go and the accumulator read:
/// `content, refusal, role, tool_calls`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunkChoiceDelta {
    /// `content`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) content: String,
    /// `refusal`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) refusal: String,
    /// `role`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) role: String,
    /// `tool_calls`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) tool_calls: Vec<ChatCompletionChunkChoiceDeltaToolCall>,
}

/// openai-go `ChatCompletionChunkChoiceDeltaToolCall`: `index, id, function, type`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunkChoiceDeltaToolCall {
    /// `index`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) index: i64,
    /// `id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) id: String,
    /// `function`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) function: ChatCompletionChunkChoiceDeltaToolCallFunction,
    /// `type`.
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
}

/// openai-go `ChatCompletionChunkChoiceDeltaToolCallFunction`: `arguments, name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunkChoiceDeltaToolCallFunction {
    /// `arguments`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) arguments: String,
    /// `name`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) name: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::client::encode_body;
    use crate::oaishared::{JsonSchemaTag, ResponseFormatJsonSchemaJsonSchemaParam};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn object(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn body(params: &ChatCompletionNewParams, stream: bool) -> String {
        String::from_utf8(encode_body(params, stream).unwrap().to_vec()).unwrap()
    }

    /// Every request shape ADK-Go builds, against the bytes openai-go v3.91.1's `json.Marshal`
    /// writes for the same params (each `want` was generated by a Go probe marshalling the
    /// openai-go params).
    #[test]
    fn params_encode_as_openai_go_does() {
        let params = ChatCompletionNewParams {
            messages: vec![
                ChatCompletionMessageParam::system("sys"),
                ChatCompletionMessageParam::developer("dev"),
                ChatCompletionMessageParam::user("hi"),
                ChatCompletionMessageParam::user_parts(vec![
                    ChatCompletionContentPartParam::text("look"),
                    ChatCompletionContentPartParam::image(
                        ChatCompletionContentPartImageImageUrlParam {
                            url: "data:image/png;base64,AA==".into(),
                            detail: "auto".into(),
                        },
                    ),
                    ChatCompletionContentPartParam::file(ChatCompletionContentPartFileFileParam {
                        file_data: Some("data:application/pdf;base64,AA==".into()),
                        filename: Some("a.pdf".into()),
                        ..Default::default()
                    }),
                    ChatCompletionContentPartParam::file(ChatCompletionContentPartFileFileParam {
                        file_id: Some("file-1".into()),
                        ..Default::default()
                    }),
                ]),
                ChatCompletionMessageParam::Assistant(ChatCompletionAssistantMessageParam {
                    content: Some(ChatCompletionAssistantContent::String(
                        "thinking <ok> & done".into(),
                    )),
                    tool_calls: vec![ChatCompletionMessageToolCallParam::Function(
                        ChatCompletionMessageFunctionToolCallParam {
                            id: "call_1".into(),
                            function: ChatCompletionMessageFunctionToolCallFunctionParam {
                                arguments: r#"{"a":1}"#.into(),
                                name: "f".into(),
                            },
                            kind: FunctionTag,
                        },
                    )],
                    ..Default::default()
                }),
                ChatCompletionMessageParam::Assistant(
                    ChatCompletionAssistantMessageParam::default(),
                ),
                ChatCompletionMessageParam::tool(r#"{"r":1}"#, "call_1"),
            ],
            model: "gpt-4o-mini".into(),
            frequency_penalty: Some(f64::from(0.5_f32)),
            logprobs: Some(true),
            max_completion_tokens: Some(100),
            presence_penalty: Some(0.0),
            seed: Some(7),
            temperature: Some(f64::from(0.7_f32)),
            top_logprobs: Some(3),
            top_p: Some(1e-7),
            reasoning_effort: Some(ReasoningEffort::Low),
            service_tier: Some(ChatCompletionServiceTier::Flex),
            stop: Some(ChatCompletionStop::StringArray(vec![
                "a".into(),
                "b".into(),
            ])),
            stream_options: ChatCompletionStreamOptionsParam {
                include_usage: Some(true),
                ..Default::default()
            },
            response_format: Some(ChatCompletionResponseFormat::JsonSchema(
                ResponseFormatJsonSchemaParam {
                    json_schema: ResponseFormatJsonSchemaJsonSchemaParam {
                        name: "adk_response".into(),
                        strict: Some(true),
                        description: None,
                        schema: Some(object(json!({"type": "object", "b": 1, "a": 2.5}))),
                    },
                    kind: JsonSchemaTag,
                },
            )),
            tool_choice: Some(ChatCompletionToolChoiceOption::AllowedTools(
                ChatCompletionAllowedToolChoiceParam {
                    allowed_tools: ChatCompletionAllowedToolsParam {
                        mode: ChatCompletionAllowedToolsMode::Required,
                        tools: vec![object(
                            json!({"type": "function", "function": {"name": "f"}}),
                        )],
                    },
                    kind: AllowedToolsTag,
                },
            )),
            tools: vec![ChatCompletionToolParam::Function(
                ChatCompletionFunctionToolParam {
                    function: FunctionDefinitionParam {
                        name: "f".into(),
                        strict: None,
                        description: Some("d".into()),
                        parameters: Some(object(json!({"type": "object", "properties": {}}))),
                    },
                    kind: FunctionTag,
                },
            )],
        };
        let want = concat!(
            r#"{"messages":[{"content":"sys","role":"system"},{"content":"dev","role":"developer"},"#,
            r#"{"content":"hi","role":"user"},{"content":[{"text":"look","type":"text"},"#,
            r#"{"image_url":{"url":"data:image/png;base64,AA==","detail":"auto"},"type":"image_url"},"#,
            r#"{"file":{"file_data":"data:application/pdf;base64,AA==","filename":"a.pdf"},"type":"file"},"#,
            r#"{"file":{"file_id":"file-1"},"type":"file"}],"role":"user"},"#,
            r#"{"content":"thinking \u003cok\u003e \u0026 done","tool_calls":[{"id":"call_1","#,
            r#""function":{"arguments":"{\"a\":1}","name":"f"},"type":"function"}],"role":"assistant"},"#,
            r#"{"role":"assistant"},{"content":"{\"r\":1}","tool_call_id":"call_1","role":"tool"}],"#,
            r#""model":"gpt-4o-mini","frequency_penalty":0.5,"logprobs":true,"max_completion_tokens":100,"#,
            r#""presence_penalty":0,"seed":7,"temperature":0.699999988079071,"top_logprobs":3,"top_p":1e-7,"#,
            r#""reasoning_effort":"low","service_tier":"flex","stop":["a","b"],"#,
            r#""stream_options":{"include_usage":true},"#,
            r#""response_format":{"json_schema":{"name":"adk_response","strict":true,"#,
            r#""schema":{"a":2.5,"b":1,"type":"object"}},"type":"json_schema"},"#,
            r#""tool_choice":{"allowed_tools":{"mode":"required","tools":[{"function":{"name":"f"},"type":"function"}]},"type":"allowed_tools"},"#,
            r#""tools":[{"function":{"name":"f","description":"d","parameters":{"properties":{},"type":"object"}},"type":"function"}]}"#
        );
        assert_eq!(body(&params, false), want);
        assert_eq!(
            body(&params, true),
            format!("{},\"stream\":true}}", &want[..want.len() - 1])
        );

        let json_object = ChatCompletionNewParams {
            model: "m".into(),
            response_format: Some(ChatCompletionResponseFormat::JsonObject(
                ResponseFormatJsonObjectParam::default(),
            )),
            tool_choice: Some(ChatCompletionToolChoiceOption::Auto("none".into())),
            ..Default::default()
        };
        assert_eq!(
            body(&json_object, false),
            r#"{"model":"m","response_format":{"type":"json_object"},"tool_choice":"none"}"#
        );
        let named = ChatCompletionNewParams {
            model: "m".into(),
            tool_choice: Some(ChatCompletionToolChoiceOption::FunctionToolChoice(
                ChatCompletionNamedToolChoiceParam {
                    function: ChatCompletionNamedToolChoiceFunctionParam { name: "f".into() },
                    kind: FunctionTag,
                },
            )),
            ..Default::default()
        };
        assert_eq!(
            body(&named, false),
            r#"{"model":"m","tool_choice":{"function":{"name":"f"},"type":"function"}}"#
        );
    }

    #[test]
    fn completion_decodes_and_round_trips() {
        let raw = r#"{"id":"c1","object":"chat.completion","created":7,"model":"m",
            "choices":[{"index":0,"finish_reason":"tool_calls","logprobs":{"content":[
              {"token":"a","bytes":[97],"logprob":-0.5,"top_logprobs":[{"token":"b","bytes":null,"logprob":-1}]}],"refusal":null},
              "message":{"role":"assistant","content":"hi","refusal":null,
                "tool_calls":[{"id":"call_1","type":"function","function":{"name":"f","arguments":"{\"x\":1}"}}]}}],
            "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12,
              "prompt_tokens_details":{"cached_tokens":2},"completion_tokens_details":{"reasoning_tokens":1}}}"#;
        let c: ChatCompletion = serde_json::from_str(raw).unwrap();
        assert_eq!(c.id, "c1");
        assert_eq!(c.model, "m");
        let choice = &c.choices[0];
        assert_eq!(choice.finish_reason, "tool_calls");
        assert_eq!(choice.message.content, "hi");
        assert_eq!(choice.message.refusal, "");
        assert_eq!(choice.message.tool_calls[0].kind, "function");
        assert_eq!(
            choice.message.tool_calls[0].function.arguments,
            r#"{"x":1}"#
        );
        assert_eq!(choice.logprobs.content[0].top_logprobs[0].logprob, -1.0);
        let usage = c.usage.as_ref().unwrap();
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (8, 4, 12)
        );
        assert_eq!(usage.prompt_tokens_details.cached_tokens, 2);
        assert_eq!(usage.completion_tokens_details.reasoning_tokens, 1);
        let again: ChatCompletion =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(again, c);

        // Go `JSON.Usage.Valid()`: absent, null or not an object is no usage; `{}` is usage.
        for (raw, valid) in [
            (r#"{"id":"a"}"#, false),
            (r#"{"id":"a","usage":null}"#, false),
            (r#"{"id":"a","usage":"x"}"#, false),
            (r#"{"id":"a","usage":{}}"#, true),
        ] {
            let c: ChatCompletion = serde_json::from_str(raw).unwrap();
            assert_eq!(c.usage.is_some(), valid, "{raw}");
        }
        // apijson's coercions (openai-go v3.91.1).
        let c: ChatCompletion = serde_json::from_str(
            r#"{"id":5,"model":true,"created":"7","choices":[{"index":"1","finish_reason":3,"message":{"content":12.5,"refusal":null}}]}"#,
        )
        .unwrap();
        assert_eq!(
            (c.id.as_str(), c.model.as_str(), c.created),
            ("5", "true", 7)
        );
        assert_eq!(c.choices[0].index, 1);
        assert_eq!(c.choices[0].finish_reason, "3");
        assert_eq!(c.choices[0].message.content, "12.5");
        let c: ChatCompletion =
            serde_json::from_str(r#"{"id":"a","choices":[1,{"index":0}]}"#).unwrap();
        assert!(c.choices.is_empty());
    }

    #[test]
    fn chunk_decodes_and_round_trips() {
        let raw = r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m",
            "choices":[{"index":0,"delta":{"role":"assistant","content":"he","refusal":null,
              "tool_calls":[{"index":1,"id":"call_1","type":"function","function":{"name":"f","arguments":"{\"a"}}]},
              "finish_reason":null,"logprobs":null}],"usage":null}"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let choice = &chunk.choices[0];
        assert_eq!(choice.delta.content, "he");
        assert_eq!(choice.delta.role, "assistant");
        assert_eq!(choice.finish_reason, "");
        let call = &choice.delta.tool_calls[0];
        assert_eq!(
            (call.index, call.id.as_str(), call.kind.as_str()),
            (1, "call_1", "function")
        );
        assert_eq!(call.function.arguments, "{\"a");
        assert!(chunk.usage.is_none());
        let again: ChatCompletionChunk =
            serde_json::from_str(&serde_json::to_string(&chunk).unwrap()).unwrap();
        assert_eq!(again, chunk);

        let usage_only: ChatCompletionChunk = serde_json::from_str(
            r#"{"id":"c","choices":[],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}"#,
        )
        .unwrap();
        assert!(usage_only.choices.is_empty());
        assert_eq!(usage_only.usage.unwrap().total_tokens, 12);
    }
}
