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

//! Part conversions (port of ADK-Go `server/adka2a/v2/parts.go`).

use std::collections::HashMap;

use gadk_genai::{Blob, Content, FileData, Part, ROLE_MODEL, ROLE_USER};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::metadata::{METADATA_PARTIAL_KEY, to_map_structure, unmarshal_into_map_error};
use super::{A2APartConverter, StreamResponseRef};
use crate::Error;

/// Go `a2aDataPartMetaTypeKey`: the A2A data part metadata key naming the genai part type.
pub(crate) const DATA_PART_META_TYPE_KEY: &str = "adk_type";
/// Go `a2aDataPartMetaLongRunningKey`: marks a function call data part as long-running.
pub(crate) const DATA_PART_META_LONG_RUNNING_KEY: &str = "adk_is_long_running";
/// Go `a2aDataPartTypeFunctionCall`.
pub(crate) const DATA_PART_TYPE_FUNCTION_CALL: &str = "function_call";
/// Go `a2aDataPartTypeFunctionResponse`.
pub(crate) const DATA_PART_TYPE_FUNCTION_RESPONSE: &str = "function_response";
/// Go `a2aDataPartTypeCodeExecResult`.
pub(crate) const DATA_PART_TYPE_CODE_EXEC_RESULT: &str = "code_execution_result";
/// Go `a2aDataPartTypeCodeExecutableCode`.
pub(crate) const DATA_PART_TYPE_EXECUTABLE_CODE: &str = "executable_code";
/// Go `ToA2AMetaKey("thought")`: marks a text part as a thought.
const META_THOUGHT_KEY: &str = "adk_thought";

/// The wrapper an A2A data part's JSON gets inside a genai text blob.
const DATA_PART_JSON_PREFIX: &str = "<a2a_datapart_json>";
const DATA_PART_JSON_SUFFIX: &str = "</a2a_datapart_json>";

/// Go `IsPartial`: whether A2A metadata (of a part, artifact or event) marks the object as
/// partial, from the ADK partial flag set on the original ADK object.
pub fn is_partial(meta: Option<&HashMap<String, Value>>) -> bool {
    matches!(
        meta.and_then(|m| m.get(METADATA_PARTIAL_KEY)),
        Some(Value::Bool(true))
    )
}

/// Go `IsPartialFlagSet`: whether the ADK partial flag (a boolean) is present in A2A metadata.
pub fn is_partial_flag_set(meta: Option<&HashMap<String, Value>>) -> bool {
    matches!(
        meta.and_then(|m| m.get(METADATA_PARTIAL_KEY)),
        Some(Value::Bool(_))
    )
}

/// Go `ToA2APart`: converts one genai part; `long_running_tool_ids` marks long-running function
/// call data parts.
pub fn to_a2a_part(part: &Part, long_running_tool_ids: &[String]) -> Result<a2a::Part, Error> {
    if !part.text.is_empty() {
        let mut result = a2a::Part::text(part.text.clone());
        if part.thought {
            set_meta(&mut result, META_THOUGHT_KEY, Value::Bool(true));
        }
        return Ok(result);
    }
    if let Some(json) = validate_data_part_json(part) {
        let data: Value = serde_json::from_slice(json)?;
        if !matches!(data, Value::Object(_) | Value::Null) {
            return Err(unmarshal_into_map_error(&data));
        }
        return Ok(a2a::Part::data(data));
    }
    if part.inline_data.is_some() || part.file_data.is_some() {
        return to_a2a_file_part(part);
    }
    to_a2a_data_part(part, long_running_tool_ids)
}

/// Go `ToA2AParts`: converts genai parts; `long_running_tool_ids` marks long-running function
/// call data parts.
pub fn to_a2a_parts(
    parts: &[Part],
    long_running_tool_ids: &[String],
) -> Result<Vec<a2a::Part>, Error> {
    parts
        .iter()
        .map(|part| to_a2a_part(part, long_running_tool_ids))
        .collect()
}

/// Go `ToGenAIPart`: converts one A2A part.
pub fn to_genai_part(part: &a2a::Part) -> Result<Part, Error> {
    match &part.content {
        a2a::PartContent::Text(text) if !text.is_empty() => {
            let mut result = Part {
                text: text.clone(),
                ..Default::default()
            };
            if let Some(Value::Bool(thought)) = part_meta(part, META_THOUGHT_KEY) {
                result.thought = *thought;
            }
            Ok(result)
        }
        a2a::PartContent::Data(data) if !data.is_null() => to_genai_data_part(part, data),
        a2a::PartContent::Raw(raw) => Ok(Part {
            inline_data: Some(Blob {
                data: raw.clone().into(),
                mime_type: part.media_type.clone().unwrap_or_default(),
                display_name: part.filename.clone().unwrap_or_default(),
            }),
            ..Default::default()
        }),
        a2a::PartContent::Url(url) if !url.is_empty() => Ok(Part {
            file_data: Some(FileData {
                file_uri: url.clone(),
                mime_type: part.media_type.clone().unwrap_or_default(),
                display_name: part.filename.clone().unwrap_or_default(),
            }),
            ..Default::default()
        }),
        _ => Err(Error::msg(format!("unknown part type: {part:?}"))),
    }
}

/// Go `ToGenAIParts`: converts A2A parts.
pub fn to_genai_parts(parts: &[a2a::Part]) -> Result<Vec<Part>, Error> {
    parts.iter().map(to_genai_part).collect()
}

/// Go `toGenAIContent`: the message's parts as genai content, through `converter` when set
/// (a converter's `Ok(None)` drops the part).
pub async fn to_genai_content(
    msg: &a2a::Message,
    converter: Option<&A2APartConverter>,
) -> Result<Content, Error> {
    let parts = match converter {
        None => to_genai_parts(&msg.parts)?,
        Some(converter) => {
            let mut parts = Vec::with_capacity(msg.parts.len());
            for part in &msg.parts {
                if let Some(converted) = converter(StreamResponseRef::Message(msg), part).await? {
                    parts.push(converted);
                }
            }
            parts
        }
    };
    Ok(Content {
        parts,
        role: to_genai_role(&msg.role).to_owned(),
    })
}

/// Go `toGenAIRole`: `user` for a user message, `model` otherwise.
pub(crate) fn to_genai_role(role: &a2a::Role) -> &'static str {
    if *role == a2a::Role::User {
        ROLE_USER
    } else {
        ROLE_MODEL
    }
}

/// Go `Part.Meta()[key]`.
pub(crate) fn part_meta<'a>(part: &'a a2a::Part, key: &str) -> Option<&'a Value> {
    part.metadata.as_ref()?.get(key)
}

/// Go `Part.SetMeta`.
fn set_meta(part: &mut a2a::Part, key: &str, value: Value) {
    part.metadata
        .get_or_insert_with(HashMap::new)
        .insert(key.to_owned(), value);
}

/// Go's `""` string fields are `None` in a2a-rs.
fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// A map from [`to_map_structure`] as data-part content (`null` for Go's nil map).
fn map_value(map: Option<Map<String, Value>>) -> Value {
    map.map_or(Value::Null, Value::Object)
}

/// Go `validateDataPartJSON`: the JSON inside a `text/plain` blob wrapped in the data-part
/// markers.
fn validate_data_part_json(part: &Part) -> Option<&[u8]> {
    let blob = part.inline_data.as_ref()?;
    if blob.mime_type != "text/plain" {
        return None;
    }
    blob.data
        .strip_prefix(DATA_PART_JSON_PREFIX.as_bytes())?
        .strip_suffix(DATA_PART_JSON_SUFFIX.as_bytes())
}

/// Go `toA2AFilePart`.
fn to_a2a_file_part(part: &Part) -> Result<a2a::Part, Error> {
    if let Some(file) = &part.file_data {
        let mut result = a2a::Part::url(file.file_uri.clone());
        result.media_type = non_empty(&file.mime_type);
        result.filename = non_empty(&file.display_name);
        return Ok(result);
    }
    let Some(blob) = &part.inline_data else {
        return Err(Error::msg(format!("not a file part: {part:?}")));
    };
    let mut result = a2a::Part::raw(blob.data.to_vec());
    result.media_type = non_empty(&blob.mime_type);
    result.filename = non_empty(&blob.display_name);
    if let Some(video) = &part.video_metadata {
        let data = to_map_structure(video)?;
        set_meta(&mut result, "video_metadata", map_value(data));
    }
    Ok(result)
}

/// Go `toA2ADataPart`.
fn to_a2a_data_part(part: &Part, long_running_tool_ids: &[String]) -> Result<a2a::Part, Error> {
    let typed = |data, kind: &str| {
        let mut result = a2a::Part::data(map_value(data));
        set_meta(
            &mut result,
            DATA_PART_META_TYPE_KEY,
            Value::String(kind.to_owned()),
        );
        result
    };
    if let Some(result) = &part.code_execution_result {
        return Ok(typed(
            to_map_structure(result)?,
            DATA_PART_TYPE_CODE_EXEC_RESULT,
        ));
    }
    if let Some(response) = &part.function_response {
        return Ok(typed(
            to_map_structure(response)?,
            DATA_PART_TYPE_FUNCTION_RESPONSE,
        ));
    }
    if let Some(code) = &part.executable_code {
        return Ok(typed(
            to_map_structure(code)?,
            DATA_PART_TYPE_EXECUTABLE_CODE,
        ));
    }
    if let Some(call) = &part.function_call {
        let mut result = typed(to_map_structure(call)?, DATA_PART_TYPE_FUNCTION_CALL);
        set_meta(
            &mut result,
            DATA_PART_META_LONG_RUNNING_KEY,
            Value::Bool(long_running_tool_ids.contains(&call.id)),
        );
        return Ok(result);
    }
    Ok(a2a::Part::data(map_value(to_map_structure(part)?)))
}

/// Go `toGenAIDataPart`: a typed data part becomes its genai part; any other data part becomes
/// a `text/plain` blob holding the data's JSON (Go `json.Marshal` bytes) between the data-part
/// markers.
fn to_genai_data_part(part: &a2a::Part, data: &Value) -> Result<Part, Error> {
    let bytes = gadk_model::to_go_json(data)?;
    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
        Ok(serde_json::from_slice(bytes)?)
    }
    let kind = match part_meta(part, DATA_PART_META_TYPE_KEY) {
        Some(Value::String(kind)) => kind.as_str(),
        _ => "",
    };
    Ok(match kind {
        DATA_PART_TYPE_CODE_EXEC_RESULT => Part {
            code_execution_result: Some(decode(&bytes)?),
            ..Default::default()
        },
        DATA_PART_TYPE_FUNCTION_CALL => Part {
            function_call: Some(decode(&bytes)?),
            ..Default::default()
        },
        DATA_PART_TYPE_EXECUTABLE_CODE => Part {
            executable_code: Some(decode(&bytes)?),
            ..Default::default()
        },
        DATA_PART_TYPE_FUNCTION_RESPONSE => Part {
            function_response: Some(decode(&bytes)?),
            ..Default::default()
        },
        _ => {
            let mut json = Vec::with_capacity(
                DATA_PART_JSON_PREFIX.len() + bytes.len() + DATA_PART_JSON_SUFFIX.len(),
            );
            json.extend_from_slice(DATA_PART_JSON_PREFIX.as_bytes());
            json.extend_from_slice(&bytes);
            json.extend_from_slice(DATA_PART_JSON_SUFFIX.as_bytes());
            Part {
                inline_data: Some(Blob {
                    data: json.into(),
                    mime_type: "text/plain".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
    })
}
