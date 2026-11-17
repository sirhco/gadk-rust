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

//! Responses API wire types: the parts of the Responses API that the OpenAI model touches,
//! following the conventions of [`crate::oaishared`] (declaration order, zero values skipped,
//! lenient decoding).

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::oaishared::de::{go_f64, go_i64, go_string, lenient};
use crate::oaishared::{
    JsonSchemaTag, ReasoningParam, ResponseFormatJsonObjectParam, ResponseFormatTextParam, tag,
};

// ---------------------------------------------------------------- request types

tag!(
    /// `"input_text"`.
    InputTextTag = "input_text"
);
tag!(
    /// `"input_image"`.
    InputImageTag = "input_image"
);
tag!(
    /// `"input_file"`.
    InputFileTag = "input_file"
);
tag!(
    /// `"output_text"`.
    OutputTextTag = "output_text"
);
tag!(
    /// `"assistant"`.
    AssistantRoleTag = "assistant"
);
tag!(
    /// `"message"`.
    MessageTag = "message"
);
tag!(
    /// `"function_call"`.
    FunctionCallTag = "function_call"
);
tag!(
    /// `"function_call_output"`.
    FunctionCallOutputTag = "function_call_output"
);
tag!(
    /// `"function"`.
    FunctionTag = "function"
);
tag!(
    /// `"allowed_tools"`.
    AllowedToolsTag = "allowed_tools"
);

/// The Responses API request body, the fields set, in declaration order:
/// `instructions, max_output_tokens, temperature, top_logprobs, top_p, include, service_tier,
/// input, model, reasoning, text, tool_choice, tools`. The streaming flag is not a field:
/// [`crate::client::encode_body`] appends `"stream":true` after every key.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseNewParams {
    /// `instructions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instructions: Option<String>,
    /// `max_output_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<i64>,
    /// `temperature`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// `top_logprobs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_logprobs: Option<i64>,
    /// `top_p`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f64>,
    /// `include`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) include: Vec<ResponseIncludable>,
    /// `service_tier`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) service_tier: Option<ResponseServiceTier>,
    /// `input` (always an input item list).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<ResponseInput>,
    /// `model`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) model: String,
    /// `reasoning` (omitted when neither field is set).
    #[serde(skip_serializing_if = "ReasoningParam::is_zero")]
    pub(crate) reasoning: ReasoningParam,
    /// `text` (omitted when no format is set).
    #[serde(skip_serializing_if = "ResponseTextConfigParam::is_zero")]
    pub(crate) text: ResponseTextConfigParam,
    /// `tool_choice`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<ResponseToolChoice>,
    /// `tools`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolParam>,
}

/// The values of `include`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[allow(dead_code)] // the whole value set of the API; only `message.output_text.logprobs` is sent.
pub(crate) enum ResponseIncludable {
    /// `"file_search_call.results"`.
    #[serde(rename = "file_search_call.results")]
    FileSearchCallResults,
    /// `"web_search_call.results"`.
    #[serde(rename = "web_search_call.results")]
    WebSearchCallResults,
    /// `"web_search_call.action.sources"`.
    #[serde(rename = "web_search_call.action.sources")]
    WebSearchCallActionSources,
    /// `"message.input_image.image_url"`.
    #[serde(rename = "message.input_image.image_url")]
    MessageInputImageImageUrl,
    /// `"computer_call_output.output.image_url"`.
    #[serde(rename = "computer_call_output.output.image_url")]
    ComputerCallOutputOutputImageUrl,
    /// `"code_interpreter_call.outputs"`.
    #[serde(rename = "code_interpreter_call.outputs")]
    CodeInterpreterCallOutputs,
    /// `"reasoning.encrypted_content"`.
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
    /// `"message.output_text.logprobs"` (the one sent).
    #[serde(rename = "message.output_text.logprobs")]
    MessageOutputTextLogprobs,
}

/// The values of `service_tier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // the whole value set of the API; `auto`, `scale`, `fast`, `ultrafast` unsent.
pub(crate) enum ResponseServiceTier {
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
    /// `"ultrafast"`.
    Ultrafast,
}

/// The `input` of a request: a string or an input item list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(dead_code)] // the whole value set of the API; only an input item list is sent.
pub(crate) enum ResponseInput {
    /// A string.
    String(String),
    /// A list of input items.
    InputItemList(Vec<ResponseInputItem>),
}

/// An input item, the variants built.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseInputItem {
    /// A message.
    Message(EasyInputMessageParam),
    /// An output message (a replayed assistant turn).
    OutputMessage(ResponseOutputMessageParam),
    /// A function call.
    FunctionCall(ResponseFunctionToolCallParam),
    /// A function-call output.
    FunctionCallOutput(ResponseInputItemFunctionCallOutputParam),
}

/// The role of an input message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EasyInputMessageRole {
    /// `"user"`.
    User,
    /// `"assistant"`.
    Assistant,
    /// `"system"`.
    System,
    /// `"developer"`.
    Developer,
}

impl EasyInputMessageRole {
    /// The wire value (as error texts print it).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Developer => "developer",
        }
    }
}

/// The type of an input message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EasyInputMessageType {
    /// `"message"`.
    Message,
}

/// An easy input message, the fields set: `content, role, type`
/// (`phase` is never set).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct EasyInputMessageParam {
    /// `content`.
    pub(crate) content: EasyInputMessageContent,
    /// `role`.
    pub(crate) role: EasyInputMessageRole,
    /// `type` (a zero value is skipped; `"message"` is set).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<EasyInputMessageType>,
}

/// The content of an input message: a string or a list of content parts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(dead_code)] // the whole value set of the API; only a content part list is sent.
pub(crate) enum EasyInputMessageContent {
    /// A string.
    String(String),
    /// A list of input content parts.
    InputItemContentList(Vec<ResponseInputContent>),
}

/// An input content part.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseInputContent {
    /// A text part.
    Text(ResponseInputTextParam),
    /// An image part.
    Image(ResponseInputImageParam),
    /// A file part.
    File(ResponseInputFileParam),
}

/// An input text part: `text, type`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseInputTextParam {
    /// `text` (required, always sent).
    pub(crate) text: String,
    /// `type`: `"input_text"`.
    #[serde(rename = "type")]
    pub(crate) kind: InputTextTag,
}

/// The detail level of an input image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // the whole value set of the API; only `auto` is sent.
pub(crate) enum ResponseInputImageDetail {
    /// `"low"`.
    Low,
    /// `"high"`.
    High,
    /// `"auto"` (the one sent).
    Auto,
    /// `"original"`.
    Original,
}

/// An input image part: `detail, file_id, image_url, type`.
/// `None` drops `detail` from the request.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseInputImageParam {
    /// `detail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<ResponseInputImageDetail>,
    /// `file_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_id: Option<String>,
    /// `image_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) image_url: Option<String>,
    /// `type`: `"input_image"`.
    #[serde(rename = "type")]
    pub(crate) kind: InputImageTag,
}

/// An input file part, the fields set: `file_id, file_data,
/// file_url, filename, type` (`detail` is never set).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseInputFileParam {
    /// `file_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_id: Option<String>,
    /// `file_data`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_data: Option<String>,
    /// `file_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_url: Option<String>,
    /// `filename`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) filename: Option<String>,
    /// `type`: `"input_file"`.
    #[serde(rename = "type")]
    pub(crate) kind: InputFileTag,
}

/// The status of an output message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // the whole value set of the API; only `completed` is sent.
pub(crate) enum ResponseOutputMessageStatus {
    /// `"in_progress"`.
    InProgress,
    /// `"completed"` (the one sent).
    Completed,
    /// `"incomplete"`.
    Incomplete,
}

/// An output message, the fields set: `id, content,
/// status, role, type` (`phase` is never set; `id` is skipped when empty, so an empty id is
/// not sent).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseOutputMessageParam {
    /// `id`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    /// `content`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) content: Vec<ResponseOutputMessageContent>,
    /// `status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<ResponseOutputMessageStatus>,
    /// `role`: `"assistant"`.
    pub(crate) role: AssistantRoleTag,
    /// `type`: `"message"`.
    #[serde(rename = "type")]
    pub(crate) kind: MessageTag,
}

/// The content of an output message (only an output-text part is sent).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseOutputMessageContent {
    /// An output-text part.
    OutputText(ResponseOutputTextParam),
}

/// An output-text part, the fields set: `text, type`
/// (`annotations` and `logprobs` are never set and are skipped).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseOutputTextParam {
    /// `text` (required, always sent).
    pub(crate) text: String,
    /// `type`: `"output_text"`.
    #[serde(rename = "type")]
    pub(crate) kind: OutputTextTag,
}

/// A function tool call, the fields set: `arguments,
/// call_id, name, type`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseFunctionToolCallParam {
    /// `arguments` (required, always sent).
    pub(crate) arguments: String,
    /// `call_id` (required, always sent).
    pub(crate) call_id: String,
    /// `name` (required, always sent).
    pub(crate) name: String,
    /// `type`: `"function_call"`.
    #[serde(rename = "type")]
    pub(crate) kind: FunctionCallTag,
}

/// A function-call output item, the fields set:
/// `output, call_id, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ResponseInputItemFunctionCallOutputParam {
    /// `output`.
    pub(crate) output: ResponseFunctionCallOutput,
    /// `call_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) call_id: Option<String>,
    /// `type`: `"function_call_output"`.
    #[serde(rename = "type")]
    pub(crate) kind: FunctionCallOutputTag,
}

/// The output of a function call (only a string is sent).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseFunctionCallOutput {
    /// A string.
    String(String),
}

/// The text config, the field set: `format` (omitted
/// from the params when unset, [`is_zero`](Self::is_zero)).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseTextConfigParam {
    /// `format`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<ResponseFormatTextConfig>,
}

impl ResponseTextConfigParam {
    /// Whether no field is set.
    pub(crate) fn is_zero(&self) -> bool {
        self.format.is_none()
    }
}

/// The text format of a response.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[allow(dead_code)] // the whole value set of the API; plain text is never sent.
pub(crate) enum ResponseFormatTextConfig {
    /// Plain text.
    Text(ResponseFormatTextParam),
    /// A JSON schema.
    JsonSchema(ResponseFormatTextJsonSchemaConfigParam),
    /// A JSON object.
    JsonObject(ResponseFormatJsonObjectParam),
}

/// A JSON schema text format: `name, schema, strict,
/// description, type`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseFormatTextJsonSchemaConfigParam {
    /// `name` (required, always sent).
    pub(crate) name: String,
    /// `schema` (a JSON object; omitted when `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schema: Option<Map<String, Value>>,
    /// `strict`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strict: Option<bool>,
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// `type`: `"json_schema"`.
    #[serde(rename = "type")]
    pub(crate) kind: JsonSchemaTag,
}

/// The tool-choice options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // the whole value set of the API; `auto` is never sent.
pub(crate) enum ToolChoiceOptions {
    /// `"none"`.
    None,
    /// `"auto"`.
    Auto,
    /// `"required"`.
    Required,
}

/// The tool choice, the variants built.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseToolChoice {
    /// A mode.
    Mode(ToolChoiceOptions),
    /// An allowed-tools list.
    AllowedTools(ToolChoiceAllowedParam),
}

/// The mode of an allowed-tools tool choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ToolChoiceAllowedMode {
    /// `"auto"`.
    Auto,
    /// `"required"`.
    Required,
}

/// An allowed-tools tool choice: `mode, tools, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolChoiceAllowedParam {
    /// `mode`.
    pub(crate) mode: ToolChoiceAllowedMode,
    /// `tools` (a list of `{"type":"function","name":…}` objects).
    pub(crate) tools: Vec<Map<String, Value>>,
    /// `type`: `"allowed_tools"`.
    #[serde(rename = "type")]
    pub(crate) kind: AllowedToolsTag,
}

/// A tool declaration (only a function tool is sent).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ToolParam {
    /// A function tool.
    Function(FunctionToolParam),
}

/// A function tool, the fields set: `strict, parameters,
/// name, description, type`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct FunctionToolParam {
    /// `strict` (always `false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strict: Option<bool>,
    /// `parameters` (a JSON object; omitted when `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<Map<String, Value>>,
    /// `name` (required, always sent).
    pub(crate) name: String,
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// `type`: `"function"`.
    #[serde(rename = "type")]
    pub(crate) kind: FunctionTag,
}

// ---------------------------------------------------------------- response types

/// The values of a response `status`.
pub(crate) mod status {
    /// `"completed"`.
    pub(crate) const COMPLETED: &str = "completed";
    /// `"failed"`.
    pub(crate) const FAILED: &str = "failed";
    /// `"in_progress"`.
    #[cfg_attr(not(test), allow(dead_code))] // only the response tests name it
    pub(crate) const IN_PROGRESS: &str = "in_progress";
    /// `"cancelled"`.
    #[cfg_attr(not(test), allow(dead_code))] // only the response tests name it
    pub(crate) const CANCELLED: &str = "cancelled";
    /// `"queued"`.
    #[cfg_attr(not(test), allow(dead_code))] // only the response tests name it
    pub(crate) const QUEUED: &str = "queued";
    /// `"incomplete"`.
    pub(crate) const INCOMPLETE: &str = "incomplete";
}

/// A response, the fields read: `id, created_at, error,
/// incomplete_details, model, object, output, status, usage`.
///
/// The fields that may be absent or `null` (`id`, `error`, `incomplete_details`, `model`,
/// `output`, `status`) are `Option`s: `Some` only when present, non-null and of the right
/// shape. The `*_str`/accessor methods read them as plain fields (zero value when
/// `None`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Response {
    /// `id`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) id: Option<String>,
    /// `created_at`.
    #[serde(default, deserialize_with = "go_f64")]
    pub(crate) created_at: f64,
    /// `error`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) error: Option<ResponseError>,
    /// `incomplete_details`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) incomplete_details: Option<ResponseIncompleteDetails>,
    /// `model`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) model: Option<String>,
    /// `object`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) object: String,
    /// `output`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) output: Option<Vec<ResponseOutputItem>>,
    /// `status` (a `ResponseStatus`; see [`status`]).
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) status: Option<String>,
    /// `usage`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) usage: ResponseUsage,
}

impl Response {
    /// The response ID.
    pub(crate) fn id_str(&self) -> &str {
        self.id.as_deref().unwrap_or_default()
    }

    /// The model name.
    pub(crate) fn model_str(&self) -> &str {
        self.model.as_deref().unwrap_or_default()
    }

    /// The status.
    pub(crate) fn status_str(&self) -> &str {
        self.status.as_deref().unwrap_or_default()
    }

    /// The output items.
    pub(crate) fn output_items(&self) -> &[ResponseOutputItem] {
        self.output.as_deref().unwrap_or_default()
    }

    /// The error message.
    pub(crate) fn error_message(&self) -> &str {
        self.error.as_ref().map_or("", |e| e.message.as_str())
    }

    /// The error code.
    pub(crate) fn error_code(&self) -> &str {
        self.error.as_ref().map_or("", |e| e.code.as_str())
    }

    /// The incomplete reason.
    pub(crate) fn incomplete_reason(&self) -> &str {
        self.incomplete_details
            .as_ref()
            .map_or("", |d| d.reason.as_str())
    }
}

/// A response error: `code, message, param, type`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseError {
    /// `code` (`ResponseErrorCode`).
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) code: String,
    /// `message`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) message: String,
    /// `param`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) param: String,
    /// `type`.
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
}

/// The incomplete details: `reason`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseIncompleteDetails {
    /// `reason`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) reason: String,
}

/// The token usage: `input_tokens, input_tokens_details, output_tokens,
/// output_tokens_details, total_tokens`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseUsage {
    /// `input_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) input_tokens: i64,
    /// `input_tokens_details`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) input_tokens_details: ResponseUsageInputTokensDetails,
    /// `output_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) output_tokens: i64,
    /// `output_tokens_details`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) output_tokens_details: ResponseUsageOutputTokensDetails,
    /// `total_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) total_tokens: i64,
}

/// The input token details: `cache_write_tokens,
/// cached_tokens`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseUsageInputTokensDetails {
    /// `cache_write_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) cache_write_tokens: i64,
    /// `cached_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) cached_tokens: i64,
}

/// The output token details: `reasoning_tokens`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseUsageOutputTokensDetails {
    /// `reasoning_tokens`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) reasoning_tokens: i64,
}

/// A response output item, the fields read, in declaration
/// order: `id, content, role, status, type, arguments, call_id, name, summary`.
///
/// `arguments` is the field's raw JSON (a string's value, or any other value's text): `None`
/// when absent or `null`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ResponseOutputItem {
    /// `id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) id: String,
    /// `content` (a message's output text and refusals, or a reasoning item's text).
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) content: Vec<ResponseOutputContent>,
    /// `role`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) role: String,
    /// `status`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) status: String,
    /// `type`.
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
    /// `arguments`, raw.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) arguments: Option<Box<RawValue>>,
    /// `call_id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) call_id: String,
    /// `name`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) name: String,
    /// `summary` (a reasoning item's summaries).
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) summary: Vec<ResponseReasoningItemSummary>,
}

/// An output message content part, the fields read: `text,
/// type, logprobs, refusal`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ResponseOutputContent {
    /// `text`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) text: String,
    /// `type`.
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
    /// `logprobs`.
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) logprobs: Vec<ResponseOutputTextLogprob>,
    /// `refusal`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) refusal: String,
}

/// An output-text logprob: `token, bytes, logprob, top_logprobs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ResponseOutputTextLogprob {
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
    pub(crate) top_logprobs: Vec<ResponseOutputTextLogprobTopLogprob>,
}

/// A top logprob: `token, bytes, logprob`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ResponseOutputTextLogprobTopLogprob {
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

/// A reasoning summary part: `text, type`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseReasoningItemSummary {
    /// `text`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) text: String,
    /// `type`.
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
}

// ---------------------------------------------------------------- stream types

/// A stream event, the fields read: `delta,
/// sequence_number, type, item_id, output_index, code, response, content_index, message,
/// param, arguments, item, text, refusal`.
///
/// Every event's own fields are fields of this union with the same JSON names, so the Rust
/// code reads them here directly (the created event's response is
/// [`response`](Self::response), the output-item-added event's item is
/// [`item`](Self::item), and so on). A `response` that is absent, `null`, `{}` or not an
/// object is [`Response::default`] (every `Option` `None`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ResponseStreamEvent {
    /// `delta`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) delta: String,
    /// `sequence_number`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) sequence_number: i64,
    /// `type` (see [`crate::responses::consts`]).
    #[serde(rename = "type", default, deserialize_with = "go_string")]
    pub(crate) kind: String,
    /// `item_id`.
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) item_id: String,
    /// `output_index`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) output_index: i64,
    /// `code` (an `error` event's).
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) code: String,
    /// `response` (the `response.*` lifecycle events').
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) response: Response,
    /// `content_index`.
    #[serde(default, deserialize_with = "go_i64")]
    pub(crate) content_index: i64,
    /// `message` (an `error` event's).
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) message: String,
    /// `param` (an `error` event's).
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) param: String,
    /// `arguments` (`response.function_call_arguments.done`'s).
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) arguments: String,
    /// `item` (`response.output_item.added` / `.done`'s).
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) item: ResponseOutputItem,
    /// `text` (the `*.done` events').
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) text: String,
    /// `refusal` (`response.refusal.done`'s).
    #[serde(default, deserialize_with = "go_string")]
    pub(crate) refusal: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::client::encode_body;
    use crate::oaishared::{ReasoningEffort, ReasoningSummary};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn object(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn body(params: &ResponseNewParams, stream: bool) -> String {
        String::from_utf8(encode_body(params, stream).unwrap().to_vec()).unwrap()
    }

    fn text(t: &str) -> ResponseInputContent {
        ResponseInputContent::Text(ResponseInputTextParam {
            text: t.into(),
            kind: InputTextTag,
        })
    }

    /// Every request shape ADK-Go builds, against the bytes openai-go v3.91.1's `json.Marshal`
    /// writes for the same params (each `want` was generated by a Go probe marshalling the
    /// openai-go params).
    #[test]
    fn params_encode_as_openai_go_does() {
        let params = ResponseNewParams {
            instructions: Some("be brief".into()),
            max_output_tokens: Some(64),
            temperature: Some(0.0),
            top_logprobs: Some(1),
            top_p: Some(f64::from(0.9_f32)),
            include: vec![ResponseIncludable::MessageOutputTextLogprobs],
            service_tier: Some(ResponseServiceTier::Priority),
            input: Some(ResponseInput::InputItemList(vec![
                ResponseInputItem::Message(EasyInputMessageParam {
                    content: EasyInputMessageContent::InputItemContentList(vec![
                        text("hi"),
                        ResponseInputContent::Image(ResponseInputImageParam {
                            detail: Some(ResponseInputImageDetail::Auto),
                            image_url: Some("https://x/y.png".into()),
                            ..Default::default()
                        }),
                        ResponseInputContent::Image(ResponseInputImageParam {
                            detail: Some(ResponseInputImageDetail::Auto),
                            file_id: Some("file-1".into()),
                            ..Default::default()
                        }),
                        ResponseInputContent::File(ResponseInputFileParam {
                            file_data: Some("data:application/pdf;base64,AA==".into()),
                            filename: Some("inline_data".into()),
                            ..Default::default()
                        }),
                        ResponseInputContent::File(ResponseInputFileParam {
                            file_url: Some("https://x/a.pdf".into()),
                            ..Default::default()
                        }),
                    ]),
                    role: EasyInputMessageRole::User,
                    kind: Some(EasyInputMessageType::Message),
                }),
                ResponseInputItem::OutputMessage(ResponseOutputMessageParam {
                    content: vec![ResponseOutputMessageContent::OutputText(
                        ResponseOutputTextParam {
                            text: "4".into(),
                            kind: OutputTextTag,
                        },
                    )],
                    status: Some(ResponseOutputMessageStatus::Completed),
                    ..Default::default()
                }),
                ResponseInputItem::FunctionCall(ResponseFunctionToolCallParam {
                    arguments: "{}".into(),
                    call_id: "c1".into(),
                    name: "f".into(),
                    kind: FunctionCallTag,
                }),
                ResponseInputItem::FunctionCallOutput(ResponseInputItemFunctionCallOutputParam {
                    output: ResponseFunctionCallOutput::String(r#"{"ok":true}"#.into()),
                    call_id: Some("c1".into()),
                    kind: FunctionCallOutputTag,
                }),
            ])),
            model: "gpt-4o-mini".into(),
            reasoning: ReasoningParam {
                effort: Some(ReasoningEffort::Medium),
                summary: Some(ReasoningSummary::Auto),
            },
            text: ResponseTextConfigParam {
                format: Some(ResponseFormatTextConfig::JsonSchema(
                    ResponseFormatTextJsonSchemaConfigParam {
                        name: "adk_response".into(),
                        schema: Some(object(json!({"type": "object"}))),
                        strict: Some(true),
                        description: None,
                        kind: JsonSchemaTag,
                    },
                )),
            },
            tool_choice: Some(ResponseToolChoice::AllowedTools(ToolChoiceAllowedParam {
                mode: ToolChoiceAllowedMode::Auto,
                tools: vec![object(json!({"type": "function", "name": "f"}))],
                kind: AllowedToolsTag,
            })),
            tools: vec![ToolParam::Function(FunctionToolParam {
                strict: Some(false),
                parameters: Some(object(json!({"type": "object", "properties": {}}))),
                name: "f".into(),
                description: Some("d".into()),
                kind: FunctionTag,
            })],
        };
        let want = concat!(
            r#"{"instructions":"be brief","max_output_tokens":64,"temperature":0,"top_logprobs":1,"#,
            r#""top_p":0.8999999761581421,"include":["message.output_text.logprobs"],"service_tier":"priority","#,
            r#""input":[{"content":[{"text":"hi","type":"input_text"},"#,
            r#"{"detail":"auto","image_url":"https://x/y.png","type":"input_image"},"#,
            r#"{"detail":"auto","file_id":"file-1","type":"input_image"},"#,
            r#"{"file_data":"data:application/pdf;base64,AA==","filename":"inline_data","type":"input_file"},"#,
            r#"{"file_url":"https://x/a.pdf","type":"input_file"}],"role":"user","type":"message"},"#,
            r#"{"content":[{"text":"4","type":"output_text"}],"status":"completed","role":"assistant","type":"message"},"#,
            r#"{"arguments":"{}","call_id":"c1","name":"f","type":"function_call"},"#,
            r#"{"output":"{\"ok\":true}","call_id":"c1","type":"function_call_output"}],"#,
            r#""model":"gpt-4o-mini","reasoning":{"effort":"medium","summary":"auto"},"#,
            r#""text":{"format":{"name":"adk_response","schema":{"type":"object"},"strict":true,"type":"json_schema"}},"#,
            r#""tool_choice":{"mode":"auto","tools":[{"name":"f","type":"function"}],"type":"allowed_tools"},"#,
            r#""tools":[{"strict":false,"parameters":{"properties":{},"type":"object"},"name":"f","description":"d","type":"function"}]}"#
        );
        assert_eq!(body(&params, false), want);
        assert_eq!(
            body(&params, true),
            format!("{},\"stream\":true}}", &want[..want.len() - 1])
        );

        let small = ResponseNewParams {
            model: "m".into(),
            reasoning: ReasoningParam {
                effort: Some(ReasoningEffort::None),
                summary: None,
            },
            text: ResponseTextConfigParam {
                format: Some(ResponseFormatTextConfig::JsonObject(
                    ResponseFormatJsonObjectParam::default(),
                )),
            },
            tool_choice: Some(ResponseToolChoice::Mode(ToolChoiceOptions::Required)),
            ..Default::default()
        };
        assert_eq!(
            body(&small, false),
            r#"{"model":"m","reasoning":{"effort":"none"},"text":{"format":{"type":"json_object"}},"tool_choice":"required"}"#
        );
        let developer = ResponseNewParams {
            model: "m".into(),
            input: Some(ResponseInput::InputItemList(vec![
                ResponseInputItem::Message(EasyInputMessageParam {
                    content: EasyInputMessageContent::InputItemContentList(vec![text("hi")]),
                    role: EasyInputMessageRole::Developer,
                    kind: Some(EasyInputMessageType::Message),
                }),
            ])),
            ..Default::default()
        };
        assert_eq!(
            body(&developer, false),
            r#"{"input":[{"content":[{"text":"hi","type":"input_text"}],"role":"developer","type":"message"}],"model":"m"}"#
        );
        assert_eq!(EasyInputMessageRole::Developer.as_str(), "developer");
    }

    #[test]
    fn response_decodes_with_presence_and_round_trips() {
        let raw = r#"{"id":"resp_1","object":"response","created_at":1.5,"model":"gpt-4o",
            "status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
            "error":null,
            "output":[
              {"type":"message","id":"m1","role":"assistant","status":"completed","content":[
                {"type":"output_text","text":"hi","annotations":[],"logprobs":[
                  {"token":"hi","bytes":[104,105],"logprob":-0.1,"top_logprobs":[{"token":"yo","bytes":[],"logprob":-2}]}]},
                {"type":"refusal","refusal":"no"}]},
              {"type":"function_call","id":"fc1","call_id":"call_1","name":"f","arguments":"{\"a\":1}"},
              {"type":"reasoning","id":"r1","content":[{"type":"reasoning_text","text":"think"}],
               "summary":[{"type":"summary_text","text":"sum"}]}],
            "usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":1},"output_tokens":2,
              "output_tokens_details":{"reasoning_tokens":1},"total_tokens":5}}"#;
        let resp: Response = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.id_str(), "resp_1");
        assert_eq!(resp.model_str(), "gpt-4o");
        assert_eq!(resp.status_str(), status::INCOMPLETE);
        assert_eq!(resp.incomplete_reason(), "max_output_tokens");
        assert!(resp.error.is_none());
        assert_eq!((resp.error_message(), resp.error_code()), ("", ""));
        let items = resp.output_items();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].content[0].logprobs[0].top_logprobs[0].token, "yo");
        assert_eq!(items[0].content[1].refusal, "no");
        assert_eq!(items[1].arguments.as_ref().unwrap().get(), r#""{\"a\":1}""#);
        assert_eq!(items[2].content[0].text, "think");
        assert_eq!(items[2].summary[0].text, "sum");
        assert_eq!(resp.usage.total_tokens, 5);
        assert_eq!(resp.usage.input_tokens_details.cached_tokens, 1);
        assert_eq!(resp.usage.output_tokens_details.reasoning_tokens, 1);

        let again: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&again).unwrap(),
            serde_json::to_value(&resp).unwrap()
        );

        // Presence (Go `JSON.X.Valid()`).
        let empty: Response = serde_json::from_str("{}").unwrap();
        assert!(empty.id.is_none() && empty.status.is_none() && empty.output.is_none());
        assert!(
            empty.incomplete_details.is_none() && empty.error.is_none() && empty.model.is_none()
        );
        let shapes: Response = serde_json::from_str(
            r#"{"incomplete_details":{},"output":[],"error":{"code":"server_error","message":"boom"}}"#,
        )
        .unwrap();
        assert_eq!(
            shapes.incomplete_details,
            Some(ResponseIncompleteDetails::default())
        );
        assert_eq!(shapes.output.as_deref().map(<[_]>::len), Some(0));
        assert_eq!(
            (shapes.error_code(), shapes.error_message()),
            ("server_error", "boom")
        );
        let null_details: Response =
            serde_json::from_str(r#"{"incomplete_details":null}"#).unwrap();
        assert!(null_details.incomplete_details.is_none());
    }

    /// Arguments are kept raw, whatever their JSON type (Go's union with `OfString` and the raw
    /// `OfResponseToolSearchCallArguments`); `1e400` does not fail the document.
    #[test]
    fn arguments_stay_raw() {
        for (raw, want) in [
            (r#""{\"a\":1}""#, Some(r#""{\"a\":1}""#)),
            (r#"{"location":"Paris"}"#, Some(r#"{"location":"Paris"}"#)),
            (r#"{"x":1e400}"#, Some(r#"{"x":1e400}"#)),
            ("42", Some("42")),
            ("true", Some("true")),
            ("[1]", Some("[1]")),
            ("null", None),
        ] {
            let item: ResponseOutputItem =
                serde_json::from_str(&format!(r#"{{"type":"function_call","arguments":{raw}}}"#))
                    .unwrap();
            assert_eq!(item.arguments.as_ref().map(|r| r.get()), want, "{raw}");
        }
        let item: ResponseOutputItem = serde_json::from_str(r#"{"type":"function_call"}"#).unwrap();
        assert!(item.arguments.is_none());
    }

    #[test]
    fn stream_events_decode_every_field_adk_go_reads() {
        let delta: ResponseStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_text.delta","item_id":"m1","output_index":0,"content_index":0,"delta":"hel","sequence_number":3}"#,
        )
        .unwrap();
        assert_eq!(
            (
                delta.kind.as_str(),
                delta.delta.as_str(),
                delta.item_id.as_str(),
                delta.sequence_number
            ),
            ("response.output_text.delta", "hel", "m1", 3)
        );
        let added: ResponseStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","call_id":"call_1","name":"get_weather"}}"#,
        )
        .unwrap();
        assert_eq!(
            (
                added.item.id.as_str(),
                added.item.call_id.as_str(),
                added.item.name.as_str()
            ),
            ("item_1", "call_1", "get_weather")
        );
        let done: ResponseStreamEvent = serde_json::from_str(
            r#"{"type":"response.function_call_arguments.done","item_id":"i1","arguments":"{"}"#,
        )
        .unwrap();
        assert_eq!(done.arguments, "{");
        let error: ResponseStreamEvent = serde_json::from_str(
            r#"{"type":"error","code":"server_error","message":"stream blew up","param":null}"#,
        )
        .unwrap();
        assert_eq!(
            (
                error.code.as_str(),
                error.message.as_str(),
                error.param.as_str()
            ),
            ("server_error", "stream blew up", "")
        );
        let completed: ResponseStreamEvent = serde_json::from_str(
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[]}}"#,
        )
        .unwrap();
        assert_eq!(completed.response.id_str(), "resp_1");
        assert_eq!(completed.response.status_str(), "completed");
        for raw in [
            r#"{"type":"response.completed"}"#,
            r#"{"type":"response.completed","response":null}"#,
            r#"{"type":"response.completed","response":{}}"#,
            r#"{"type":"response.completed","response":"x"}"#,
        ] {
            let e: ResponseStreamEvent = serde_json::from_str(raw).unwrap();
            let r = &e.response;
            assert!(
                r.id.is_none()
                    && r.status.is_none()
                    && r.output.is_none()
                    && r.incomplete_details.is_none()
                    && r.error.is_none()
                    && r.model.is_none(),
                "{raw}"
            );
        }
    }
}
