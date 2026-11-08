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

//! Messages API wire types: the parts of anthropic-sdk-go v1.81.0 (`message.go`) that ADK-Go's
//! `anthropicmodel` touches.
//!
//! Request types serialize their fields in the SDK's struct declaration order, with Go's
//! `omitzero` as `skip_serializing_if`; each block's constant `type` is a zero-sized tag field,
//! last, as in the SDK. Response and stream-event types decode with Go's leniency (`null` and
//! missing fields are zero values). [`to_go_json`] writes JSON as Go's `encoding/json` does
//! (HTML-escaped strings, Go float formatting); [`encode_request`] adds the SDK's streaming flag
//! and the Vertex body rewrite.

use std::io;

use serde::de::Deserializer;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::config::{Effort, PromptCacheTtl};

/// The `anthropic_version` the Vertex middleware adds to every body (SDK
/// `vertex.DefaultVersion`).
pub(crate) const VERTEX_ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

// ---------------------------------------------------------------- Go-compatible JSON

/// A `serde_json` formatter that writes what Go's `encoding/json` writes for the same value:
/// `<`, `>`, `&`, U+2028 and U+2029 in strings are escape sequences (`\u003c`, `\u003e`,
/// `\u0026`, `\u2028`, `\u2029`); floats use Go's format (`0` not `0.0`, `1e-7`, `1e+21`).
struct GoFormatter;

/// Go `encoding/json` `floatEncoder`: `'f'` format with the shortest representation, `'e'`
/// below 1e-6 or from 1e21 on, with a two-digit negative exponent's leading zero removed.
fn go_float(value: f64, shortest: impl Fn() -> String, exponent: impl Fn() -> String) -> String {
    let abs = value.abs();
    if abs == 0.0 || (1e-6..1e21).contains(&abs) {
        return shortest();
    }
    let e = exponent();
    match e.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp:0>2}"),
        _ => e,
    }
}

impl serde_json::ser::Formatter for GoFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        let s = go_float(value, || format!("{value}"), || format!("{value:e}"));
        writer.write_all(s.as_bytes())
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        let s = go_float(
            f64::from(value),
            || format!("{value}"),
            || format!("{value:e}"),
        );
        writer.write_all(s.as_bytes())
    }

    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut start = 0;
        for (i, c) in fragment.char_indices() {
            let esc = match c {
                '<' => "\\u003c",
                '>' => "\\u003e",
                '&' => "\\u0026",
                '\u{2028}' => "\\u2028",
                '\u{2029}' => "\\u2029",
                _ => continue,
            };
            writer.write_all(&fragment.as_bytes()[start..i])?;
            writer.write_all(esc.as_bytes())?;
            start = i + c.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }
}

/// Serializes `value` as Go's `json.Marshal` would: compact, HTML-escaped strings, Go float
/// formatting, and map keys sorted (as `serde_json::Map` is without `preserve_order`).
pub(crate) fn to_go_json<T: ?Sized + Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, GoFormatter);
    value.serialize(&mut ser)?;
    Ok(out)
}

/// The request body the SDK sends for `params`:
/// - the params as [`to_go_json`] writes them;
/// - streaming: `,"stream":true` appended (the SDK's `option.WithJSONSet("stream", true)`);
/// - Vertex: without `"model"` and with `,"anthropic_version":"vertex-2023-10-16"` appended
///   after every other key (the SDK's Vertex middleware: `sjson.SetBytes` appends,
///   `sjson.DeleteBytes` removes; omitting `model` gives the same bytes).
pub(crate) fn encode_request(
    params: &MessageNewParams,
    stream: bool,
    vertex: bool,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut body = if vertex {
        let without_model = MessageNewParams {
            model: String::new(),
            ..params.clone()
        };
        to_go_json(&without_model)?
    } else {
        to_go_json(params)?
    };
    let mut append = |key_value: &str| {
        // The params always serialize as a non-empty object (`max_tokens` is never omitted).
        body.pop();
        body.push(b',');
        body.extend_from_slice(key_value.as_bytes());
        body.push(b'}');
    };
    if stream {
        append(r#""stream":true"#);
    }
    if vertex {
        append(&format!(
            r#""anthropic_version":"{VERTEX_ANTHROPIC_VERSION}""#
        ));
    }
    Ok(body)
}

// ---------------------------------------------------------------- request types

/// Declares a zero-sized type that serializes as one constant string: the SDK's
/// `constant.*` `type` fields.
macro_rules! tag {
    ($(#[$m:meta])* $name:ident = $value:literal) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub(crate) struct $name;

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($value)
            }
        }
    };
}

tag!(
    /// `"text"`.
    TextTag = "text"
);
tag!(
    /// `"image"`.
    ImageTag = "image"
);
tag!(
    /// `"document"`.
    DocumentTag = "document"
);
tag!(
    /// `"base64"`.
    Base64Tag = "base64"
);
tag!(
    /// `"url"`.
    UrlTag = "url"
);
tag!(
    /// `"application/pdf"` (the PDF sources' constant `media_type`).
    PdfMediaType = "application/pdf"
);
tag!(
    /// `"text/plain"` (the plain-text source's constant `media_type`).
    PlainTextMediaType = "text/plain"
);
tag!(
    /// `"tool_use"`.
    ToolUseTag = "tool_use"
);
tag!(
    /// `"tool_result"`.
    ToolResultTag = "tool_result"
);
tag!(
    /// `"thinking"`.
    ThinkingTag = "thinking"
);
tag!(
    /// `"redacted_thinking"`.
    RedactedThinkingTag = "redacted_thinking"
);
tag!(
    /// `"ephemeral"`.
    EphemeralTag = "ephemeral"
);
tag!(
    /// `"object"` (the tool input schema's constant `type`).
    ObjectTag = "object"
);
tag!(
    /// `"json_schema"`.
    JsonSchemaTag = "json_schema"
);

/// SDK `MessageNewParams`, the fields Go sets, in declaration order: `max_tokens, messages,
/// model, temperature, top_k, top_p, output_config, stop_sequences, system, thinking,
/// tool_choice, tools` (the SDK's `inference_geo`, `container`, `cache_control`, `diagnostics`,
/// `metadata` and `service_tier` are never set by Go and are not modelled).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct MessageNewParams {
    /// `max_tokens` (required, never omitted).
    pub(crate) max_tokens: u64,
    /// `messages`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) messages: Vec<MessageParam>,
    /// `model`; omitted when empty (the Vertex body).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) model: String,
    /// `temperature` (Go `param.Opt[float64]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// `top_k`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_k: Option<i64>,
    /// `top_p`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f64>,
    /// `output_config`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_config: Option<OutputConfig>,
    /// `stop_sequences`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stop_sequences: Vec<String>,
    /// `system`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) system: Vec<TextBlockParam>,
    /// `thinking`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking: Option<ThinkingConfigParam>,
    /// `tool_choice`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<ToolChoiceParam>,
    /// `tools`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolParam>,
}

/// SDK `MessageParamRole`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    /// `"user"`.
    User,
    /// `"assistant"`.
    Assistant,
}

impl Role {
    /// The wire value (Go `string(role)`, used in warnings and drop reasons).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// SDK `MessageParam`: `content, role`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct MessageParam {
    /// `content`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) content: Vec<ContentBlockParam>,
    /// `role`.
    pub(crate) role: Role,
}

/// SDK `ContentBlockParamUnion`, the variants Go builds; serializes as the variant's object.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ContentBlockParam {
    /// `text`.
    Text(TextBlockParam),
    /// `image`.
    Image(ImageBlockParam),
    /// `document`.
    Document(DocumentBlockParam),
    /// `thinking`.
    Thinking(ThinkingBlockParam),
    /// `redacted_thinking`.
    RedactedThinking(RedactedThinkingBlockParam),
    /// `tool_use`.
    ToolUse(ToolUseBlockParam),
    /// `tool_result`.
    ToolResult(ToolResultBlockParam),
}

impl ContentBlockParam {
    /// SDK `NewTextBlock`.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::Text(TextBlockParam::new(text))
    }
}

/// SDK `CacheControlEphemeralParam`: `ttl, type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CacheControlEphemeral {
    /// `ttl` (Go always sets it).
    pub(crate) ttl: PromptCacheTtl,
    /// `"ephemeral"`.
    #[serde(rename = "type")]
    pub(crate) kind: EphemeralTag,
}

impl CacheControlEphemeral {
    /// SDK `NewCacheControlEphemeralParam()` with `TTL` set.
    pub(crate) fn new(ttl: PromptCacheTtl) -> Self {
        Self {
            ttl,
            kind: EphemeralTag,
        }
    }
}

/// SDK `TextBlockParam`: `text, cache_control, type` (`citations` never set by Go).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct TextBlockParam {
    /// `text` (never omitted).
    pub(crate) text: String,
    /// `cache_control`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControlEphemeral>,
    /// `"text"`.
    #[serde(rename = "type")]
    pub(crate) kind: TextTag,
}

impl TextBlockParam {
    /// A text block without cache control.
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache_control: None,
            kind: TextTag,
        }
    }
}

/// SDK `ImageBlockParam`: `source, cache_control, type` (`transformations` never set).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ImageBlockParam {
    /// `source`.
    pub(crate) source: ImageSource,
    /// `cache_control`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControlEphemeral>,
    /// `"image"`.
    #[serde(rename = "type")]
    pub(crate) kind: ImageTag,
}

impl ImageBlockParam {
    /// SDK `NewImageBlockBase64(mediaType, data)`.
    pub(crate) fn base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::with_source(ImageSource::Base64(Base64ImageSource {
            data: data.into(),
            media_type: media_type.into(),
            kind: Base64Tag,
        }))
    }

    /// SDK `NewImageBlock(URLImageSourceParam{URL})`.
    pub(crate) fn url(url: impl Into<String>) -> Self {
        Self::with_source(ImageSource::Url(UrlSource {
            url: url.into(),
            kind: UrlTag,
        }))
    }

    fn with_source(source: ImageSource) -> Self {
        Self {
            source,
            cache_control: None,
            kind: ImageTag,
        }
    }
}

/// SDK `ImageBlockParamSourceUnion` (base64 or URL).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ImageSource {
    /// SDK `Base64ImageSourceParam`.
    Base64(Base64ImageSource),
    /// SDK `URLImageSourceParam`.
    Url(UrlSource),
}

/// SDK `Base64ImageSourceParam`: `data, media_type, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Base64ImageSource {
    /// `data` (standard base64).
    pub(crate) data: String,
    /// `media_type`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) media_type: String,
    /// `"base64"`.
    #[serde(rename = "type")]
    pub(crate) kind: Base64Tag,
}

/// SDK `URLImageSourceParam` / `URLPDFSourceParam`: `url, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct UrlSource {
    /// `url`.
    pub(crate) url: String,
    /// `"url"`.
    #[serde(rename = "type")]
    pub(crate) kind: UrlTag,
}

/// SDK `DocumentBlockParam`: `source, cache_control, type` (`context`, `title`, `citations`
/// never set).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DocumentBlockParam {
    /// `source`.
    pub(crate) source: DocumentSource,
    /// `cache_control`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControlEphemeral>,
    /// `"document"`.
    #[serde(rename = "type")]
    pub(crate) kind: DocumentTag,
}

impl DocumentBlockParam {
    /// SDK `NewDocumentBlock(Base64PDFSourceParam{Data})`.
    pub(crate) fn base64_pdf(data: impl Into<String>) -> Self {
        Self::with_source(DocumentSource::Base64Pdf(Base64PdfSource {
            data: data.into(),
            media_type: PdfMediaType,
            kind: Base64Tag,
        }))
    }

    /// SDK `NewDocumentBlock(PlainTextSourceParam{Data})`.
    pub(crate) fn plain_text(data: impl Into<String>) -> Self {
        Self::with_source(DocumentSource::PlainText(PlainTextSource {
            data: data.into(),
            media_type: PlainTextMediaType,
            kind: TextTag,
        }))
    }

    /// SDK `NewDocumentBlock(URLPDFSourceParam{URL})`.
    pub(crate) fn url_pdf(url: impl Into<String>) -> Self {
        Self::with_source(DocumentSource::UrlPdf(UrlSource {
            url: url.into(),
            kind: UrlTag,
        }))
    }

    fn with_source(source: DocumentSource) -> Self {
        Self {
            source,
            cache_control: None,
            kind: DocumentTag,
        }
    }
}

/// SDK `DocumentBlockParamSourceUnion`, the variants Go builds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum DocumentSource {
    /// SDK `Base64PDFSourceParam`.
    Base64Pdf(Base64PdfSource),
    /// SDK `PlainTextSourceParam`.
    PlainText(PlainTextSource),
    /// SDK `URLPDFSourceParam`.
    UrlPdf(UrlSource),
}

/// SDK `Base64PDFSourceParam`: `data, media_type, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Base64PdfSource {
    /// `data` (standard base64).
    pub(crate) data: String,
    /// `"application/pdf"`.
    pub(crate) media_type: PdfMediaType,
    /// `"base64"`.
    #[serde(rename = "type")]
    pub(crate) kind: Base64Tag,
}

/// SDK `PlainTextSourceParam`: `data, media_type, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PlainTextSource {
    /// `data`.
    pub(crate) data: String,
    /// `"text/plain"`.
    pub(crate) media_type: PlainTextMediaType,
    /// `"text"`.
    #[serde(rename = "type")]
    pub(crate) kind: TextTag,
}

/// SDK `ToolUseBlockParam`: `id, input, name, cache_control, type` (`toolset_name`, `caller`
/// never set).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolUseBlockParam {
    /// `id`.
    pub(crate) id: String,
    /// `input` (Go always passes a non-nil map, so it is never omitted).
    pub(crate) input: Map<String, Value>,
    /// `name`.
    pub(crate) name: String,
    /// `cache_control`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControlEphemeral>,
    /// `"tool_use"`.
    #[serde(rename = "type")]
    pub(crate) kind: ToolUseTag,
}

impl ToolUseBlockParam {
    /// SDK `NewToolUseBlock(id, input, name)`.
    pub(crate) fn new(
        id: impl Into<String>,
        input: Map<String, Value>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            input,
            name: name.into(),
            cache_control: None,
            kind: ToolUseTag,
        }
    }
}

/// SDK `ToolResultBlockParam`: `tool_use_id, is_error, cache_control, content, type`
/// (`toolset_name` never set).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolResultBlockParam {
    /// `tool_use_id`.
    pub(crate) tool_use_id: String,
    /// `is_error` (Go always sets `false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) is_error: Option<bool>,
    /// `cache_control`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControlEphemeral>,
    /// `content`; omitted when empty (Go's nil slice).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) content: Vec<ToolResultContent>,
    /// `"tool_result"`.
    #[serde(rename = "type")]
    pub(crate) kind: ToolResultTag,
}

/// SDK `ToolResultBlockParamContentUnion`, the variants Go builds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ToolResultContent {
    /// `text`.
    Text(TextBlockParam),
    /// `image`.
    Image(ImageBlockParam),
    /// `document`.
    Document(DocumentBlockParam),
}

/// SDK `ThinkingBlockParam`: `signature, thinking, type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ThinkingBlockParam {
    /// `signature`.
    pub(crate) signature: String,
    /// `thinking`.
    pub(crate) thinking: String,
    /// `"thinking"`.
    #[serde(rename = "type")]
    pub(crate) kind: ThinkingTag,
}

impl ThinkingBlockParam {
    /// SDK `NewThinkingBlock(signature, thinking)`.
    pub(crate) fn new(signature: impl Into<String>, thinking: impl Into<String>) -> Self {
        Self {
            signature: signature.into(),
            thinking: thinking.into(),
            kind: ThinkingTag,
        }
    }
}

/// SDK `RedactedThinkingBlockParam`: `data, type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RedactedThinkingBlockParam {
    /// `data`.
    pub(crate) data: String,
    /// `"redacted_thinking"`.
    #[serde(rename = "type")]
    pub(crate) kind: RedactedThinkingTag,
}

impl RedactedThinkingBlockParam {
    /// SDK `NewRedactedThinkingBlock(data)`.
    pub(crate) fn new(data: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            kind: RedactedThinkingTag,
        }
    }
}

/// SDK `ToolParam` (the `OfTool` member of `ToolUnionParam`): `input_schema, name,
/// description, cache_control` (`eager_input_streaming`, `defer_loading`, `strict`, `type`,
/// `allowed_callers`, `input_examples` never set by Go; `description` sits between
/// `defer_loading` and `strict`, `cache_control` between `allowed_callers` and
/// `input_examples`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolParam {
    /// `input_schema`.
    pub(crate) input_schema: ToolInputSchema,
    /// `name`.
    pub(crate) name: String,
    /// `description` (Go sets it only when non-empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// `cache_control`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControlEphemeral>,
}

/// SDK `ToolInputSchemaParam`: `properties, required, type`, then `ExtraFields`.
///
/// The SDK appends each extra field with `sjson.SetRawBytes` while ranging over a Go map, so
/// with several extras Go's key order is random; gadk writes them sorted (`serde_json::Map`).
/// `extra` must not hold `type`, `properties` or `required` (Go `buildToolInputSchema` skips
/// them).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolInputSchema {
    /// `properties` (Go `any`; Go always sets it, `{}` when the schema has none).
    pub(crate) properties: Value,
    /// `required`; omitted when empty (Go's nil slice).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) required: Vec<String>,
    /// `"object"`.
    #[serde(rename = "type")]
    pub(crate) kind: ObjectTag,
    /// `ExtraFields`, after the declared fields.
    #[serde(flatten)]
    pub(crate) extra: Map<String, Value>,
}

/// SDK `ToolChoiceUnionParam`, the variants Go builds (`disable_parallel_tool_use` is never
/// set): `{"type":"auto"}`, `{"type":"any"}`, `{"name":…,"type":"tool"}`, `{"type":"none"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolChoiceParam {
    /// SDK `ToolChoiceAutoParam`.
    Auto,
    /// SDK `ToolChoiceAnyParam`.
    Any,
    /// SDK `ToolChoiceToolParam` (`ToolChoiceParamOfTool(name)`).
    Tool {
        /// `name`.
        name: String,
    },
    /// SDK `ToolChoiceNoneParam`.
    None,
}

impl Serialize for ToolChoiceParam {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        match self {
            Self::Auto => m.serialize_entry("type", "auto")?,
            Self::Any => m.serialize_entry("type", "any")?,
            Self::Tool { name } => {
                m.serialize_entry("name", name)?;
                m.serialize_entry("type", "tool")?;
            }
            Self::None => m.serialize_entry("type", "none")?,
        }
        m.end()
    }
}

/// SDK `ThinkingConfigParamUnion`, the variants Go builds: `{"budget_tokens":N,"type":"enabled"}`,
/// `{"type":"disabled"}`, `{"display":"summarized","type":"adaptive"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThinkingConfigParam {
    /// SDK `ThinkingConfigParamOfEnabled(budget)`.
    Enabled {
        /// `budget_tokens`.
        budget_tokens: i64,
    },
    /// SDK `NewThinkingConfigDisabledParam()`.
    Disabled,
    /// SDK `ThinkingConfigAdaptiveParam{Display: ThinkingConfigAdaptiveDisplaySummarized}`.
    AdaptiveSummarized,
}

impl Serialize for ThinkingConfigParam {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        match self {
            Self::Enabled { budget_tokens } => {
                m.serialize_entry("budget_tokens", budget_tokens)?;
                m.serialize_entry("type", "enabled")?;
            }
            Self::Disabled => m.serialize_entry("type", "disabled")?,
            Self::AdaptiveSummarized => {
                m.serialize_entry("display", "summarized")?;
                m.serialize_entry("type", "adaptive")?;
            }
        }
        m.end()
    }
}

/// SDK `OutputConfigParam`: `effort, format`. Go omits the whole object when both are unset.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutputConfig {
    /// `effort`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<Effort>,
    /// `format`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<JsonOutputFormat>,
}

/// SDK `JSONOutputFormatParam`: `schema, type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct JsonOutputFormat {
    /// `schema`.
    pub(crate) schema: Map<String, Value>,
    /// `"json_schema"`.
    #[serde(rename = "type")]
    pub(crate) kind: JsonSchemaTag,
}

// ---------------------------------------------------------------- response types

/// Go's decoding of `null` into a non-pointer field: the zero value.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Go `json.RawMessage`: the value's JSON text as received (`null` stays `"null"`).
fn raw_json<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let raw = Box::<RawValue>::deserialize(d)?;
    Ok(Some(raw.get().to_string()))
}

/// SDK `Message`, the fields Go reads (`id, content, model, role, stop_reason, stop_sequence,
/// type, usage`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct Message {
    /// `id`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) id: String,
    /// `content`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) content: Vec<ContentBlock>,
    /// `model`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) model: String,
    /// `role`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) role: String,
    /// `stop_reason` (`""` for `null`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) stop_reason: String,
    /// `stop_sequence` (`""` for `null`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) stop_sequence: String,
    /// `type`.
    #[serde(default, rename = "type", deserialize_with = "null_default")]
    pub(crate) kind: String,
    /// `usage`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) usage: Usage,
}

/// SDK `ContentBlockUnion` (flattened, as in the SDK), the fields Go reads.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ContentBlock {
    /// `type`: `text`, `thinking`, `redacted_thinking`, `tool_use`, or another block type.
    #[serde(default, rename = "type", deserialize_with = "null_default")]
    pub(crate) kind: String,
    /// `text` (text blocks).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) text: String,
    /// `thinking` (thinking blocks).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) thinking: String,
    /// `signature` (thinking blocks).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) signature: String,
    /// `data` (redacted thinking blocks).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) data: String,
    /// `id` (tool use blocks).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) id: String,
    /// `name` (tool use blocks).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) name: String,
    /// `input` as raw JSON text (Go `json.RawMessage`); `None` when absent. Streaming
    /// accumulation replaces it with the concatenated `partial_json` deltas, which may not be
    /// valid JSON.
    #[serde(default, deserialize_with = "raw_json")]
    pub(crate) input: Option<String>,
}

/// SDK `Usage`, the fields Go reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct Usage {
    /// `cache_creation_input_tokens`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) cache_creation_input_tokens: i64,
    /// `cache_read_input_tokens`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) cache_read_input_tokens: i64,
    /// `input_tokens`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) input_tokens: i64,
    /// `output_tokens`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) output_tokens: i64,
    /// `output_tokens_details`; `None` when absent or `null` (Go `JSON.….Valid()` false).
    #[serde(default)]
    pub(crate) output_tokens_details: Option<OutputTokensDetails>,
}

/// SDK `OutputTokensDetails`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct OutputTokensDetails {
    /// `thinking_tokens`; `None` when absent or `null` (Go `JSON.ThinkingTokens.Valid()` false).
    #[serde(default)]
    pub(crate) thinking_tokens: Option<i64>,
}

/// SDK `MessageStreamEventUnion`, the fields `Message.Accumulate` and Go `generateStreaming`
/// read. `type` is the event's own `type` member (the SSE `event:` name selects whether the
/// SDK decodes the event at all).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamEvent {
    /// `type`.
    #[serde(default, rename = "type", deserialize_with = "null_default")]
    pub(crate) kind: String,
    /// `message` (`message_start`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) message: Message,
    /// `index` (content block events).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) index: i64,
    /// `content_block` (`content_block_start`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) content_block: ContentBlock,
    /// `delta` (`content_block_delta`, `message_delta`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) delta: StreamDelta,
    /// `usage` (`message_delta`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) usage: MessageDeltaUsage,
}

/// SDK `MessageStreamEventUnionDelta`, the fields Go reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct StreamDelta {
    /// `type`: `text_delta`, `input_json_delta`, `thinking_delta`, `signature_delta`, ….
    #[serde(default, rename = "type", deserialize_with = "null_default")]
    pub(crate) kind: String,
    /// `text`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) text: String,
    /// `partial_json`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) partial_json: String,
    /// `thinking`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) thinking: String,
    /// `signature`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) signature: String,
    /// `stop_reason` (`message_delta`; `""` for `null`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) stop_reason: String,
    /// `stop_sequence` (`message_delta`; `""` for `null`).
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) stop_sequence: String,
}

/// SDK `MessageDeltaUsage`. `Option` fields are `None` when absent or `null` (Go
/// `JSON.….Valid()` false); `output_tokens` always overwrites.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct MessageDeltaUsage {
    /// `cache_creation_input_tokens`.
    #[serde(default)]
    pub(crate) cache_creation_input_tokens: Option<i64>,
    /// `cache_read_input_tokens`.
    #[serde(default)]
    pub(crate) cache_read_input_tokens: Option<i64>,
    /// `input_tokens`.
    #[serde(default)]
    pub(crate) input_tokens: Option<i64>,
    /// `output_tokens`.
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) output_tokens: i64,
    /// `output_tokens_details`.
    #[serde(default)]
    pub(crate) output_tokens_details: Option<OutputTokensDetails>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// The request bodies of an httprr v1 trace (`N M\n`, then N request bytes and M response
    /// bytes per record).
    fn recorded_request_bodies(trace: &[u8]) -> Vec<String> {
        let mut rest = trace
            .strip_prefix(b"httprr trace v1\n".as_slice())
            .expect("httprr header");
        let mut bodies = Vec::new();
        while !rest.is_empty() {
            let nl = rest.iter().position(|b| *b == b'\n').expect("record line");
            let line = std::str::from_utf8(&rest[..nl]).unwrap();
            let (n, m) = line.split_once(' ').unwrap();
            let (n, m): (usize, usize) = (n.parse().unwrap(), m.parse().unwrap());
            let req = &rest[nl + 1..nl + 1 + n];
            let split = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            bodies.push(String::from_utf8(req[split + 4..].to_vec()).unwrap());
            rest = &rest[nl + 1 + n + m..];
        }
        bodies
    }

    fn user_text(text: &str) -> MessageParam {
        MessageParam {
            content: vec![ContentBlockParam::text(text)],
            role: Role::User,
        }
    }

    /// The params Go's `TestGenerate_Anthropic` builds: system instruction, temperature 0,
    /// default max tokens.
    fn anthropic_params() -> MessageNewParams {
        MessageNewParams {
            max_tokens: 8192,
            messages: vec![user_text(
                "What is the capital of France? Reply with one word.",
            )],
            model: "claude-sonnet-4-5-20250929".into(),
            temperature: Some(0.0),
            system: vec![TextBlockParam::new("Reply with the city name only.")],
            ..MessageNewParams::default()
        }
    }

    #[test]
    fn anthropic_recording_bodies_match_byte_for_byte() {
        let bodies = recorded_request_bodies(include_bytes!(
            "../tests/testdata/model/anthropicmodel/testdata/TestGenerate_Anthropic.httprr"
        ));
        assert_eq!(bodies.len(), 2);
        let params = anthropic_params();
        let non_streaming =
            String::from_utf8(encode_request(&params, false, false).unwrap()).unwrap();
        assert_eq!(non_streaming, bodies[0]);
        let streaming = String::from_utf8(encode_request(&params, true, false).unwrap()).unwrap();
        assert_eq!(streaming, bodies[1]);
    }

    #[test]
    fn vertex_recording_body_matches_byte_for_byte() {
        let bodies = recorded_request_bodies(include_bytes!(
            "../tests/testdata/model/anthropicmodel/testdata/TestGenerate_Vertex.httprr"
        ));
        assert_eq!(bodies.len(), 1);
        let params = MessageNewParams {
            max_tokens: 8192,
            messages: vec![user_text("What is 2 + 2? Reply with the digit only.")],
            model: "claude-sonnet-4-5-20250929".into(),
            temperature: Some(0.0),
            ..MessageNewParams::default()
        };
        let body = String::from_utf8(encode_request(&params, false, true).unwrap()).unwrap();
        assert_eq!(body, bodies[0]);
    }

    /// On Vertex, `anthropic_version` follows `stream` (the middleware runs after the SDK set
    /// `stream`).
    #[test]
    fn vertex_streaming_body_appends_stream_then_version() {
        let body =
            String::from_utf8(encode_request(&anthropic_params(), true, true).unwrap()).unwrap();
        assert_eq!(
            body,
            r#"{"max_tokens":8192,"messages":[{"content":[{"text":"What is the capital of France? Reply with one word.","type":"text"}],"role":"user"}],"temperature":0,"system":[{"text":"Reply with the city name only.","type":"text"}],"stream":true,"anthropic_version":"vertex-2023-10-16"}"#
        );
    }

    /// The runner recording's first request: an inline PNG, tools with an `additionalProperties`
    /// extra, and `tool_choice` auto; its second adds `tool_use` and `tool_result` blocks.
    #[test]
    fn tool_image_and_tool_result_shapes_match_the_runner_recording() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAAEElEQVR4nGJiYPgPCAAA//8BCwEC1pVDqAAAAABJRU5ErkJggg==";
        let system = "Identify the color of the image and call get_palette_info to report its hex code.\n\nYou are an agent. Your internal name is \"vision_palette_agent\". The description about you is \"Inspects an image and looks up its palette info.\".";
        let user = MessageParam {
            content: vec![
                ContentBlockParam::Image(ImageBlockParam::base64("image/png", png)),
                ContentBlockParam::text(
                    "What color is this image and what does get_palette_info return for it?",
                ),
            ],
            role: Role::User,
        };
        let tool = ToolParam {
            input_schema: ToolInputSchema {
                properties: json!({"color": {"description": "Dominant color name", "type": "string"}}),
                required: vec!["color".into()],
                kind: ObjectTag,
                extra: json!({"additionalProperties": false})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
            name: "get_palette_info".into(),
            description: Some("Returns the canonical hex code for a color name.".into()),
            cache_control: None,
        };
        let mut params = MessageNewParams {
            max_tokens: 8192,
            messages: vec![user],
            model: "claude-sonnet-4-5-20250929".into(),
            system: vec![TextBlockParam::new(system)],
            tool_choice: Some(ToolChoiceParam::Auto),
            tools: vec![tool],
            ..MessageNewParams::default()
        };
        let trace = include_bytes!(
            "../../gadk/tests/testdata/model/anthropicmodel/testdata/TestAgentRunner_MultiTurnImageAndTool.httprr"
        );
        let bodies = recorded_request_bodies(trace);
        assert_eq!(
            String::from_utf8(encode_request(&params, false, false).unwrap()).unwrap(),
            bodies[0]
        );

        let input = json!({"color": "blue"}).as_object().unwrap().clone();
        params.messages.push(MessageParam {
            content: vec![ContentBlockParam::ToolUse(ToolUseBlockParam::new(
                "toolu_palette_01",
                input,
                "get_palette_info",
            ))],
            role: Role::Assistant,
        });
        params.messages.push(MessageParam {
            content: vec![ContentBlockParam::ToolResult(ToolResultBlockParam {
                tool_use_id: "toolu_palette_01".into(),
                is_error: Some(false),
                cache_control: None,
                content: vec![ToolResultContent::Text(TextBlockParam::new(
                    r##"{"color":"blue","hex_code":"#0055FF"}"##,
                ))],
                kind: ToolResultTag,
            })],
            role: Role::User,
        });
        assert_eq!(
            String::from_utf8(encode_request(&params, false, false).unwrap()).unwrap(),
            bodies[1]
        );
    }

    /// Every other block, choice, thinking and output shape, in the SDK's key order.
    #[test]
    fn remaining_param_shapes_serialize_in_sdk_order() {
        let cc = Some(CacheControlEphemeral::new(PromptCacheTtl::OneHour));
        let mut text = TextBlockParam::new("t");
        text.cache_control = cc;
        let cases: Vec<(Value, &str)> = vec![
            (
                serde_json::to_value(ContentBlockParam::Text(text)).unwrap(),
                r#"{"text":"t","cache_control":{"ttl":"1h","type":"ephemeral"},"type":"text"}"#,
            ),
            (
                serde_json::to_value(ContentBlockParam::Image(ImageBlockParam::url(
                    "https://x/i.png",
                )))
                .unwrap(),
                r#"{"source":{"url":"https://x/i.png","type":"url"},"type":"image"}"#,
            ),
            (
                serde_json::to_value(ContentBlockParam::Document(DocumentBlockParam::base64_pdf(
                    "UERG",
                )))
                .unwrap(),
                r#"{"source":{"data":"UERG","media_type":"application/pdf","type":"base64"},"type":"document"}"#,
            ),
            (
                serde_json::to_value(ContentBlockParam::Document(DocumentBlockParam::plain_text(
                    "hi",
                )))
                .unwrap(),
                r#"{"source":{"data":"hi","media_type":"text/plain","type":"text"},"type":"document"}"#,
            ),
            (
                serde_json::to_value(ContentBlockParam::Document(DocumentBlockParam::url_pdf(
                    "https://x/d.pdf",
                )))
                .unwrap(),
                r#"{"source":{"url":"https://x/d.pdf","type":"url"},"type":"document"}"#,
            ),
            (
                serde_json::to_value(ContentBlockParam::Thinking(ThinkingBlockParam::new(
                    "sig", "hmm",
                )))
                .unwrap(),
                r#"{"signature":"sig","thinking":"hmm","type":"thinking"}"#,
            ),
            (
                serde_json::to_value(ContentBlockParam::RedactedThinking(
                    RedactedThinkingBlockParam::new("opaque"),
                ))
                .unwrap(),
                r#"{"data":"opaque","type":"redacted_thinking"}"#,
            ),
            (
                serde_json::to_value(ToolResultBlockParam {
                    tool_use_id: "id".into(),
                    is_error: Some(false),
                    cache_control: cc,
                    content: vec![],
                    kind: ToolResultTag,
                })
                .unwrap(),
                r#"{"tool_use_id":"id","is_error":false,"cache_control":{"ttl":"1h","type":"ephemeral"},"type":"tool_result"}"#,
            ),
            (
                serde_json::to_value(ToolChoiceParam::Any).unwrap(),
                r#"{"type":"any"}"#,
            ),
            (
                serde_json::to_value(ToolChoiceParam::None).unwrap(),
                r#"{"type":"none"}"#,
            ),
            (
                serde_json::to_value(ToolChoiceParam::Tool { name: "f".into() }).unwrap(),
                r#"{"name":"f","type":"tool"}"#,
            ),
            (
                serde_json::to_value(ThinkingConfigParam::Enabled {
                    budget_tokens: 2048,
                })
                .unwrap(),
                r#"{"budget_tokens":2048,"type":"enabled"}"#,
            ),
            (
                serde_json::to_value(ThinkingConfigParam::Disabled).unwrap(),
                r#"{"type":"disabled"}"#,
            ),
            (
                serde_json::to_value(ThinkingConfigParam::AdaptiveSummarized).unwrap(),
                r#"{"display":"summarized","type":"adaptive"}"#,
            ),
        ];
        for (value, want) in cases {
            // `to_value` sorts keys; the byte check below uses the declared order.
            let want_value: Value = serde_json::from_str(want).unwrap();
            assert_eq!(value, want_value);
        }

        let mut tool = ToolParam {
            input_schema: ToolInputSchema {
                properties: json!({}),
                required: vec![],
                kind: ObjectTag,
                extra: Map::new(),
            },
            name: "f".into(),
            description: None,
            cache_control: cc,
        };
        assert_eq!(
            String::from_utf8(to_go_json(&tool).unwrap()).unwrap(),
            r#"{"input_schema":{"properties":{},"type":"object"},"name":"f","cache_control":{"ttl":"1h","type":"ephemeral"}}"#
        );
        tool.input_schema.extra = json!({"z": 1, "$defs": {}, "anyOf": []})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            String::from_utf8(to_go_json(&tool.input_schema).unwrap()).unwrap(),
            r#"{"properties":{},"type":"object","$defs":{},"anyOf":[],"z":1}"#
        );

        let params = MessageNewParams {
            max_tokens: 1024,
            messages: vec![user_text("x")],
            model: "m".into(),
            temperature: Some(0.5),
            top_k: Some(40),
            top_p: Some(0.25),
            output_config: Some(OutputConfig {
                effort: Some(Effort::High),
                format: Some(JsonOutputFormat {
                    schema:
                        json!({"type": "object", "properties": {}, "additionalProperties": false})
                            .as_object()
                            .unwrap()
                            .clone(),
                    kind: JsonSchemaTag,
                }),
            }),
            stop_sequences: vec!["END".into()],
            system: vec![TextBlockParam::new("s")],
            thinking: Some(ThinkingConfigParam::Disabled),
            tool_choice: Some(ToolChoiceParam::Auto),
            tools: vec![],
        };
        assert_eq!(
            String::from_utf8(to_go_json(&params).unwrap()).unwrap(),
            r#"{"max_tokens":1024,"messages":[{"content":[{"text":"x","type":"text"}],"role":"user"}],"model":"m","temperature":0.5,"top_k":40,"top_p":0.25,"output_config":{"effort":"high","format":{"schema":{"additionalProperties":false,"properties":{},"type":"object"},"type":"json_schema"}},"stop_sequences":["END"],"system":[{"text":"s","type":"text"}],"thinking":{"type":"disabled"},"tool_choice":{"type":"auto"}}"#
        );
        assert_eq!(
            String::from_utf8(
                to_go_json(&OutputConfig {
                    effort: None,
                    format: None
                })
                .unwrap()
            )
            .unwrap(),
            "{}"
        );
    }

    /// Go `encoding/json` float output (`strconv.AppendFloat(…, 'f' or 'e', -1, 64)` plus the
    /// exponent clean-up) and f32 values widened as Go's `float64(*cfg.Temperature)` does.
    #[test]
    fn floats_use_gos_format() {
        let cases: [(f64, &str); 12] = [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (-2.5e-10, "-2.5e-10"),
            (123456.789, "123456.789"),
            (f64::from(0.7_f32), "0.699999988079071"),
        ];
        for (v, want) in cases {
            assert_eq!(
                String::from_utf8(to_go_json(&v).unwrap()).unwrap(),
                want,
                "{v:e}"
            );
        }
        assert_eq!(
            String::from_utf8(to_go_json(&0.1_f32).unwrap()).unwrap(),
            "0.1"
        );
        assert_eq!(
            String::from_utf8(to_go_json(&json!({"n": 2.0, "i": 2})).unwrap()).unwrap(),
            r#"{"i":2,"n":2}"#
        );
    }

    /// Go's `json.Marshal` escapes HTML-significant characters and the JavaScript line
    /// separators; control characters use the same escapes as serde_json.
    #[test]
    fn strings_use_gos_escaping() {
        let s = "a<b>&c\u{2028}\u{2029}\"\\\n\t\u{1}\u{8}\u{c}é";
        assert_eq!(
            String::from_utf8(to_go_json(s).unwrap()).unwrap(),
            "\"a\\u003cb\\u003e\\u0026c\\u2028\\u2029\\\"\\\\\\n\\t\\u0001\\b\\fé\""
        );
        assert_eq!(
            String::from_utf8(to_go_json(&json!({"<k>": "&"})).unwrap()).unwrap(),
            r#"{"\u003ck\u003e":"\u0026"}"#
        );
    }

    #[test]
    fn message_decodes_with_gos_leniency() {
        let m: Message = serde_json::from_str(
            r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude","stop_reason":null,"stop_sequence":null,
               "content":[{"type":"text","text":"hi","citations":null},
                          {"type":"thinking","thinking":"t","signature":"s"},
                          {"type":"redacted_thinking","data":"d"},
                          {"type":"tool_use","id":"toolu_1","name":"f","input":{"a":1}},
                          {"type":"tool_use","id":"toolu_2","name":"g","input":null},
                          {"type":"server_tool_use","id":"x"}],
               "usage":{"input_tokens":3,"output_tokens":5,"cache_read_input_tokens":null,"output_tokens_details":{"thinking_tokens":2}}}"#,
        )
        .unwrap();
        assert_eq!(m.id, "msg_1");
        assert_eq!(m.stop_reason, "");
        assert_eq!(m.content.len(), 6);
        assert_eq!(m.content[0].text, "hi");
        assert_eq!(m.content[1].signature, "s");
        assert_eq!(m.content[2].data, "d");
        assert_eq!(m.content[3].input.as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(m.content[4].input.as_deref(), Some("null"));
        assert_eq!(m.content[5].input, None);
        assert_eq!(
            m.usage,
            Usage {
                input_tokens: 3,
                output_tokens: 5,
                output_tokens_details: Some(OutputTokensDetails {
                    thinking_tokens: Some(2)
                }),
                ..Usage::default()
            }
        );
    }

    /// The streaming recording's events decode into the fields the accumulator reads.
    #[test]
    fn stream_events_decode() {
        let start: StreamEvent = serde_json::from_str(r#"{"type":"message_start","message":{"id":"msg_stream_01","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":18,"output_tokens":1}}}"#).unwrap();
        assert_eq!(start.kind, "message_start");
        assert_eq!(start.message.model, "claude-sonnet-4-5-20250929");
        assert_eq!(start.message.usage.input_tokens, 18);

        let block: StreamEvent = serde_json::from_str(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"f","input":{}}}"#).unwrap();
        assert_eq!(block.index, 0);
        assert_eq!(block.content_block.input.as_deref(), Some("{}"));

        let delta: StreamEvent = serde_json::from_str(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#).unwrap();
        assert_eq!(delta.delta.kind, "input_json_delta");
        assert_eq!(delta.delta.partial_json, r#"{"a":"#);

        let md: StreamEvent = serde_json::from_str(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}"#).unwrap();
        assert_eq!(md.delta.stop_reason, "end_turn");
        assert_eq!(
            md.usage,
            MessageDeltaUsage {
                output_tokens: 2,
                ..MessageDeltaUsage::default()
            }
        );
        let md: StreamEvent = serde_json::from_str(r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":7,"input_tokens":null,"cache_read_input_tokens":4}}"#).unwrap();
        assert_eq!(md.usage.input_tokens, None);
        assert_eq!(md.usage.cache_read_input_tokens, Some(4));
    }
}
