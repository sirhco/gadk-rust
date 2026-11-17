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

//! The hand-written reflection-shaped `Serialize`/`Deserialize` impls of the recording types
//! ([`crate::conformance::recording`]): declaration field order, yaml-tagged keys verbatim,
//! `omitempty` honoured; `LlmRequestView` always emits `model`, `contents`, `config` and
//! `tools` (nil as `null`, the nil map as `{}`); the recorded function call/response emit
//! every field. Below the roots, gadk serde types go through gadk-goyaml with default
//! `Options` (lowercased keys). Nothing outside `gonode` uses its items directly.
//!
//! Encoding builds the nodes directly (`encode_recordings`): the recording types by their
//! yaml tags, the roots below them (`LLMRequest`, `LLMResponse`,
//! `FunctionCall`, `FunctionResponse`) through the shapes table of
//! [`super::fixups`], fed with gadk serde's JSON. gadk serde writes genai's whole-second
//! duration strings, so the exact `TimeDelta`s are laid over the JSON first
//! (`overlay_content` and friends: a JSON integer of nanoseconds). Decoding uses private
//! serde mirrors named after the recorded types (`RecordingsDe`).

use chrono::TimeDelta;
use gadk_genai::{
    Content, FunctionResponsePart, FunctionResponseScheduling, GenerateContentConfig, Part,
    PartialArg, Transcription,
};
use gadk_goyaml::{GoValue, NodeId, Options};
use gadk_model::LlmResponse;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::fixups::{Builder, Keys, shape};
use crate::conformance::recording::{
    LlmRecording, LlmRequestView, RecordedFunctionCall, RecordedFunctionResponse, Recording,
    Recordings, ToolRecording,
};
use crate::govalue::GoMap;

type Result<T> = std::result::Result<T, gadk_goyaml::Error>;

/// The recordings' mapping: `recordings:` and each `Recording` by its
/// yaml tags (`llm_recording`/`tool_recording` and their fields `omitempty`).
pub(crate) fn encode_recordings(b: &mut Builder, recs: &Recordings) -> Result<NodeId> {
    let mut items = Vec::with_capacity(recs.recordings.len());
    for r in &recs.recordings {
        items.push(encode_recording(b, r)?);
    }
    let seq = b.sequence(items);
    let k = b.key("recordings")?;
    Ok(b.mapping(vec![k, seq]))
}

fn encode_recording(b: &mut Builder, r: &Recording) -> Result<NodeId> {
    let mut content = vec![
        b.key("user_message_index")?,
        b.leaf(&GoValue::Int(r.user_message_index))?,
        b.key("agent_name")?,
        b.leaf(&GoValue::String(r.agent_name.clone()))?,
    ];
    if let Some(l) = &r.llm_recording {
        let mut lm = Vec::new();
        if let Some(req) = &l.llm_request {
            lm.push(b.key("llm_request")?);
            lm.push(encode_llm_request(b, req)?);
        }
        if !l.llm_responses.is_empty() {
            let mut items = Vec::with_capacity(l.llm_responses.len());
            for resp in &l.llm_responses {
                items.push(encode_llm_response(b, resp)?);
            }
            lm.push(b.key("llm_responses")?);
            lm.push(b.sequence(items));
        }
        content.push(b.key("llm_recording")?);
        content.push(b.mapping(lm));
    }
    if let Some(t) = &r.tool_recording {
        let mut tm = Vec::new();
        if let Some(fc) = &t.tool_call {
            tm.push(b.key("tool_call")?);
            tm.push(encode_function_call(b, fc)?);
        }
        if let Some(fr) = &t.tool_response {
            tm.push(b.key("tool_response")?);
            tm.push(encode_function_response(b, fr)?);
        }
        content.push(b.key("tool_recording")?);
        content.push(b.mapping(tm));
    }
    Ok(b.mapping(content))
}

/// Builds a root struct by its shape: `field(key)` gives a present field's node, `None` takes
/// the field's zero node.
fn root_struct(
    b: &mut Builder,
    go_type: &str,
    mut field: impl FnMut(&mut Builder, &str) -> Result<Option<NodeId>>,
) -> Result<NodeId> {
    let Some(s) = shape(go_type) else {
        return Err(gadk_goyaml::Error::Unsupported(format!(
            "no Go shape for {go_type}"
        )));
    };
    let mut content = Vec::with_capacity(s.fields.len() * 2);
    for f in s.fields {
        let k = b.key(f.key)?;
        let v = match field(b, f.key)? {
            Some(v) => v,
            None => b.zero(f.zero)?,
        };
        content.push(k);
        content.push(v);
    }
    Ok(b.mapping(content))
}

/// A free-form map (`None` = nil, encoded `{}`).
fn go_map(b: &mut Builder, m: Option<&GoMap>) -> Result<Option<NodeId>> {
    match m {
        Some(m) if !m.is_empty() => {
            let v = GoValue::Map(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
            b.leaf(&v).map(Some)
        }
        _ => Ok(None),
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| gadk_goyaml::Error::Custom(e.to_string()))
}

/// `LLMRequest` (untagged: `model`, `contents`, `config`, `tools`).
fn encode_llm_request(b: &mut Builder, req: &LlmRequestView) -> Result<NodeId> {
    root_struct(b, "model.LLMRequest", |b, key| match key {
        "model" if !req.model.is_empty() => b.leaf(&GoValue::String(req.model.clone())).map(Some),
        "contents" if !req.contents.is_empty() => {
            let mut items = Vec::with_capacity(req.contents.len());
            for c in &req.contents {
                let mut j = to_json(c)?;
                overlay_content(&mut j, c);
                items.push(b.struct_node("genai.Content", j.as_object(), Keys::Json)?);
            }
            Ok(Some(b.sequence(items)))
        }
        "config" => match &req.config {
            Some(cfg) => {
                let mut j = to_json(cfg)?;
                overlay_config(&mut j, cfg);
                b.struct_node("genai.GenerateContentConfig", j.as_object(), Keys::Json)
                    .map(Some)
            }
            None => Ok(None),
        },
        "tools" => go_map(b, Some(&req.tools)),
        _ => Ok(None),
    })
}

/// `LLMResponse` (JSON-tagged, so gadk serde's names are its fields' JSON names).
fn encode_llm_response(b: &mut Builder, resp: &LlmResponse) -> Result<NodeId> {
    let mut j = to_json(resp)?;
    overlay_response(&mut j, resp);
    b.struct_node("model.LLMResponse", j.as_object(), Keys::Json)
}

/// The recorded `FunctionCall`: every field.
fn encode_function_call(b: &mut Builder, fc: &RecordedFunctionCall) -> Result<NodeId> {
    root_struct(b, "genai.FunctionCall", |b, key| match key {
        "id" if !fc.id.is_empty() => b.leaf(&GoValue::String(fc.id.clone())).map(Some),
        "args" => go_map(b, fc.args.as_ref()),
        "name" if !fc.name.is_empty() => b.leaf(&GoValue::String(fc.name.clone())).map(Some),
        "partialargs" if !fc.partial_args.is_empty() => {
            struct_seq(b, "genai.PartialArg", &fc.partial_args).map(Some)
        }
        "willcontinue" => opt_bool(b, fc.will_continue),
        _ => Ok(None),
    })
}

/// The recorded `FunctionResponse`: every field.
fn encode_function_response(b: &mut Builder, fr: &RecordedFunctionResponse) -> Result<NodeId> {
    root_struct(b, "genai.FunctionResponse", |b, key| match key {
        "willcontinue" => opt_bool(b, fr.will_continue),
        "scheduling" => match &fr.scheduling {
            Some(s) => b.leaf(&GoValue::String(s.as_str().to_owned())).map(Some),
            None => Ok(None),
        },
        "parts" if !fr.parts.is_empty() => {
            struct_seq::<FunctionResponsePart>(b, "genai.FunctionResponsePart", &fr.parts).map(Some)
        }
        "id" if !fr.id.is_empty() => b.leaf(&GoValue::String(fr.id.clone())).map(Some),
        "name" if !fr.name.is_empty() => b.leaf(&GoValue::String(fr.name.clone())).map(Some),
        "response" => go_map(b, fr.response.as_ref()),
        _ => Ok(None),
    })
}

fn struct_seq<T: serde::Serialize>(b: &mut Builder, go_type: &str, items: &[T]) -> Result<NodeId> {
    let mut nodes = Vec::with_capacity(items.len());
    for item in items {
        let j = to_json(item)?;
        nodes.push(b.struct_node(go_type, j.as_object(), Keys::Json)?);
    }
    Ok(b.sequence(nodes))
}

fn opt_bool(b: &mut Builder, v: Option<bool>) -> Result<Option<NodeId>> {
    match v {
        Some(v) => b.leaf(&GoValue::Bool(v)).map(Some),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------------------------
// The typed duration overlay (gadk serde's duration strings are whole seconds).
// ---------------------------------------------------------------------------------------------

fn nanos(d: TimeDelta) -> Value {
    Value::from(d.num_nanoseconds().unwrap_or(i64::MAX))
}

/// Sets `j[path…]` (objects only) to `v` when the path exists.
fn set_at(j: &mut Value, path: &[&str], v: Value) {
    let mut cur = j;
    for p in path {
        match cur.get_mut(*p) {
            Some(next) => cur = next,
            None => return,
        }
    }
    *cur = v;
}

fn overlay_transcription(j: &mut Value, t: &Transcription) {
    if let Some(obj) = j.as_object_mut() {
        obj.insert("startOffset".to_owned(), nanos(t.start_offset));
        obj.insert("endOffset".to_owned(), nanos(t.end_offset));
    }
}

fn overlay_part(j: &mut Value, p: &Part) {
    if let Some(vm) = &p.video_metadata
        && let Some(obj) = j.get_mut("videoMetadata").and_then(Value::as_object_mut)
    {
        obj.insert("startOffset".to_owned(), nanos(vm.start_offset));
        obj.insert("endOffset".to_owned(), nanos(vm.end_offset));
    }
    if let Some(t) = &p.audio_transcription
        && let Some(tj) = j.get_mut("audioTranscription")
    {
        overlay_transcription(tj, t);
    }
}

/// Lays `c`'s exact durations over its gadk JSON `j`.
pub(crate) fn overlay_content(j: &mut Value, c: &Content) {
    if let Some(parts) = j.get_mut("parts").and_then(Value::as_array_mut) {
        for (pj, p) in parts.iter_mut().zip(&c.parts) {
            overlay_part(pj, p);
        }
    }
}

fn overlay_config(j: &mut Value, cfg: &GenerateContentConfig) {
    if let Some(si) = &cfg.system_instruction
        && let Some(sj) = j.get_mut("systemInstruction")
    {
        overlay_content(sj, si);
    }
    let Some(tools) = j.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for (tj, t) in tools.iter_mut().zip(&cfg.tools) {
        let Some(servers) = tj.get_mut("mcpServers").and_then(Value::as_array_mut) else {
            continue;
        };
        for (sj, s) in servers.iter_mut().zip(&t.mcp_servers) {
            if let Some(h) = &s.streamable_http_transport {
                set_at(
                    sj,
                    &["streamableHttpTransport", "timeout"],
                    nanos(h.timeout),
                );
                set_at(
                    sj,
                    &["streamableHttpTransport", "sseReadTimeout"],
                    nanos(h.sse_read_timeout),
                );
            }
        }
    }
}

fn overlay_response(j: &mut Value, r: &LlmResponse) {
    if let Some(c) = &r.content
        && let Some(cj) = j.get_mut("content")
    {
        overlay_content(cj, c);
    }
    if let Some(t) = &r.input_transcription
        && let Some(tj) = j.get_mut("inputTranscription")
    {
        overlay_transcription(tj, t);
    }
    if let Some(t) = &r.output_transcription
        && let Some(tj) = j.get_mut("outputTranscription")
    {
        overlay_transcription(tj, t);
    }
    let Some(gm) = &r.grounding_metadata else {
        return;
    };
    let Some(chunks) = j
        .get_mut("groundingMetadata")
        .and_then(|g| g.get_mut("groundingChunks"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for (cj, chunk) in chunks.iter_mut().zip(&gm.grounding_chunks) {
        if let Some(route) = chunk.maps.as_ref().and_then(|m| m.route.as_ref()) {
            set_at(cj, &["maps", "route", "duration"], nanos(route.duration));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Decoding: serde mirrors named after the recorded types (yaml.v3's type errors name them).
// ---------------------------------------------------------------------------------------------

/// The decode options: the recording types' yaml tags are their serde names verbatim; gadk
/// types below them keep their default lowercased keys.
pub(crate) fn decode_options() -> Options {
    let mut o = Options::default();
    for s in [
        "recording.Recordings",
        "recording.Recording",
        "recording.LLMRecording",
        "recording.ToolRecording",
    ] {
        o.verbatim_structs.insert(s);
    }
    o
}

/// The `Recordings` root.
#[derive(Deserialize, Default)]
#[serde(rename = "recording.Recordings", default)]
pub(crate) struct RecordingsDe {
    recordings: Vec<RecordingDe>,
}

#[derive(Deserialize, Default)]
#[serde(rename = "recording.Recording", default)]
struct RecordingDe {
    user_message_index: i64,
    agent_name: String,
    llm_recording: Option<LlmRecordingDe>,
    tool_recording: Option<ToolRecordingDe>,
}

#[derive(Deserialize, Default)]
#[serde(rename = "recording.LLMRecording", default)]
struct LlmRecordingDe {
    llm_request: Option<LlmRequestDe>,
    // A nil element of the response list (`llm_responses: [null]`) is representable in the
    // recorded format; gadk's `Vec<LlmResponse>` cannot represent it.
    llm_responses: Vec<LlmResponse>,
}

#[derive(Deserialize, Default)]
#[serde(rename = "recording.ToolRecording", default)]
struct ToolRecordingDe {
    tool_call: Option<FunctionCallDe>,
    tool_response: Option<FunctionResponseDe>,
}

#[derive(Deserialize, Default)]
#[serde(rename = "model.LLMRequest", default)]
struct LlmRequestDe {
    model: String,
    contents: Vec<Content>,
    config: Option<GenerateContentConfig>,
    tools: GoMap,
}

#[derive(Deserialize, Default)]
#[serde(rename = "genai.FunctionCall", default)]
struct FunctionCallDe {
    id: String,
    args: Option<GoMap>,
    name: String,
    partial_args: Vec<PartialArg>,
    will_continue: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename = "genai.FunctionResponse", default)]
struct FunctionResponseDe {
    will_continue: Option<bool>,
    #[serde(deserialize_with = "scheduling")]
    scheduling: Option<FunctionResponseScheduling>,
    parts: Vec<FunctionResponsePart>,
    id: String,
    name: String,
    response: Option<GoMap>,
}

/// A non-pointer string enum: `""` (and null) is gadk's `None`.
fn scheduling<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<FunctionResponseScheduling>, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    if s.is_empty() {
        return Ok(None);
    }
    serde_json::from_value(Value::String(s))
        .map(Some)
        .map_err(serde::de::Error::custom)
}

impl From<RecordingsDe> for Recordings {
    fn from(d: RecordingsDe) -> Recordings {
        Recordings {
            recordings: d.recordings.into_iter().map(Recording::from).collect(),
        }
    }
}

impl From<RecordingDe> for Recording {
    fn from(d: RecordingDe) -> Recording {
        Recording {
            user_message_index: d.user_message_index,
            agent_name: d.agent_name,
            llm_recording: d.llm_recording.map(|l| LlmRecording {
                llm_request: l.llm_request.map(|q| LlmRequestView {
                    model: q.model,
                    contents: q.contents,
                    config: q.config,
                    tools: q.tools,
                }),
                llm_responses: l.llm_responses,
            }),
            tool_recording: d.tool_recording.map(|t| ToolRecording {
                tool_call: t.tool_call.map(|c| RecordedFunctionCall {
                    id: c.id,
                    args: c.args,
                    name: c.name,
                    partial_args: c.partial_args,
                    will_continue: c.will_continue,
                }),
                tool_response: t.tool_response.map(|r| RecordedFunctionResponse {
                    will_continue: r.will_continue,
                    scheduling: r.scheduling,
                    parts: r.parts,
                    id: r.id,
                    name: r.name,
                    response: r.response,
                }),
            }),
            // `Index` is never decoded (not a YAML field).
            index: 0,
        }
    }
}
