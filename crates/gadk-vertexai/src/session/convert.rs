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

//! Event ↔ `SessionEvent` conversions, with the raw-event and struct helpers. Every error
//! text is byte-identical to an ADK server's, already wrapped with the prefixes of the append
//! and list-events calls (e.g. `failed to convert event actions: …`).
//!
//! The `SessionEvent` side is the protojson form of the Vertex AI protobuf messages as JSON
//! values: camelCase names, enum numbers, standard base64 bytes, unpopulated scalars and
//! empty lists omitted, set message fields (and set `optional string` fields) present
//! even when empty. A `google.protobuf.Struct` holds only doubles, so every number in one
//! (state delta, custom metadata, function args and responses, the raw event) becomes the
//! JSON number written for that `f64`: an integral value below 1e21 as an integer, so
//! `3` stays `3` and `9007199254740993` becomes `9007199254740992`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    GroundingChunk, GroundingChunkMaps, GroundingChunkMapsPlaceAnswerSources,
    GroundingChunkMapsPlaceAnswerSourcesReviewSnippet, GroundingChunkRetrievedContext,
    GroundingChunkWeb, GroundingMetadata, GroundingSupport, Language, Outcome, Part, RAGChunk,
    RAGChunkPageSpan, RetrievalMetadata, SearchEntryPoint, Segment,
};
use gadk_platform::go_quote;
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value, json};

use crate::error::{BoxError, wrap};
use crate::protojson::{
    decode_bytes, encode_bytes, format_timestamp, int64_value, language_name, language_number,
    outcome_name, outcome_number, parse_timestamp,
};

/// The prefix on every failure to read an event when listing events.
const LIST_EVENTS: &str = "error fetching session events";

/// An event as the `SessionEvent` JSON the append call sends.
pub(crate) fn event_to_session_event(event: &Event) -> Result<Value, BoxError> {
    let actions = actions_to_wire(event).map_err(|e| wrap("failed to convert event actions", e))?;
    let content = match &event.response.content {
        Some(c) => Some(content_to_wire(c).map_err(|e| wrap("error creating content", e))?),
        None => None,
    };
    let metadata = metadata_to_wire(event).map_err(|e| wrap("error creating metadata", e))?;
    // The legacy column-backed fields are still written as a fallback for readers that
    // ignore rawEvent.
    let raw = if event_needs_raw_event(event) {
        Some(event_to_raw_event(event).map_err(|e| wrap("error creating raw event", e))?)
    } else {
        None
    };

    let mut m = Map::new();
    m.insert(
        "timestamp".into(),
        Value::String(format_timestamp(&event.timestamp)),
    );
    put_str(&mut m, "author", &event.author);
    put_str(&mut m, "invocationId", &event.invocation_id);
    if let Some(c) = content {
        m.insert("content".into(), c);
    }
    if let Some(a) = actions {
        m.insert("actions".into(), a);
    }
    m.insert("eventMetadata".into(), metadata);
    put_str(&mut m, "errorCode", &event.response.error_code);
    put_str(&mut m, "errorMessage", &event.response.error_message);
    if let Some(r) = raw {
        m.insert("rawEvent".into(), r);
    }
    Ok(Value::Object(m))
}

/// A `SessionEvent` JSON as the event listing builds, including the event ID
/// from the last segment of `name`; `rawEvent` wins when present.
pub(crate) fn session_event_to_event(v: &Value) -> Result<Event, BoxError> {
    // The whole response is decoded before any event is read, so malformed scalars anywhere in
    // it fail the call; the lenient readers below are only used on validated input.
    validate_scalars(v).map_err(|e| wrap(LIST_EVENTS, e))?;
    let id = event_id_from_name(str_field(v, "name")).map_err(|e| wrap(LIST_EVENTS, e))?;
    let timestamp = timestamp_field(v, "timestamp").map_err(|e| wrap(LIST_EVENTS, e))?;
    let invocation_id = str_field(v, "invocationId").to_owned();
    let author = str_field(v, "author").to_owned();

    // Prefer rawEvent; fall back to the legacy field reconstruction for events written
    // before rawEvent support.
    if let Some(raw) = field(v, "rawEvent") {
        let mut event = event_from_raw_event(raw).map_err(|e| wrap(LIST_EVENTS, e))?;
        // Identity fields are authoritative on the envelope.
        event.id = id;
        event.timestamp = timestamp;
        event.invocation_id = invocation_id;
        event.author = author;
        return Ok(event);
    }

    let mut event = Event {
        id,
        timestamp,
        invocation_id,
        author,
        actions: actions_from_wire(field(v, "actions")).map_err(|e| wrap(LIST_EVENTS, e))?,
        ..Default::default()
    };
    event.response.content = field(v, "content").map(content_from_wire);
    event.response.error_code = str_field(v, "errorCode").to_owned();
    event.response.error_message = str_field(v, "errorMessage").to_owned();
    if let Some(md) = field(v, "eventMetadata") {
        event.branch = str_field(md, "branch").to_owned();
        event.response.turn_complete = bool_field(md, "turnComplete");
        event.response.partial = bool_field(md, "partial");
        event.response.interrupted = bool_field(md, "interrupted");
        event.long_running_tool_ids = strings_field(md, "longRunningToolIds");
        event.response.grounding_metadata = field(md, "groundingMetadata").map(grounding_from_wire);
        if let Some(cm) = field(md, "customMetadata") {
            event.response.custom_metadata = struct_as_map(Some(cm));
        }
    }
    Ok(event)
}

/// The segment after the last `/` of an event name.
///
/// # Errors
///
/// The texts `invalid session name format "{name}": missing separator '/'` and
/// `invalid session name "{name}": empty session ID`.
pub(super) fn event_id_from_name(name: &str) -> Result<String, String> {
    let Some(idx) = name.rfind('/') else {
        return Err(format!(
            "invalid session name format {}: missing separator '/'",
            go_quote(name)
        ));
    };
    let id = &name[idx + 1..];
    if id.is_empty() {
        return Err(format!(
            "invalid session name {}: empty session ID",
            go_quote(name)
        ));
    }
    Ok(id.to_owned())
}

/// Whether the event carries state that no `SessionEvent` column
/// holds (or that ADK does not read back), so it must also travel as `rawEvent`. Plain events
/// keep the legacy wire format.
pub(crate) fn event_needs_raw_event(event: &Event) -> bool {
    event.output.is_some()
        || event.node_info.is_some()
        || !event.isolation_scope.is_empty()
        || event.requested_input.is_some()
        || !event.routes.is_empty()
        // A finished live transcription is appended as an event with no content.
        || event.response.input_transcription.is_some()
        || event.response.output_transcription.is_some()
        // A context-compaction summary lives entirely on actions.compaction.
        || event.actions.compaction.is_some()
}

/// The event's JSON as a `Struct` (its numbers written as doubles).
pub(crate) fn event_to_raw_event(event: &Event) -> Result<Value, BoxError> {
    // Through bytes, so float32 fields keep their shortest decimal form.
    let bytes = serde_json::to_vec(event).map_err(|e| wrap("marshaling event", e))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| wrap("converting event to structpb", e))?;
    Ok(struct_value(&v))
}

/// Decodes a `rawEvent` written by any ADK runtime. Identity fields
/// are left to the caller, which takes them from the envelope.
pub(crate) fn event_from_raw_event(raw: &Value) -> Result<Event, BoxError> {
    serde_json::from_value(struct_value(raw)).map_err(|e| wrap("unmarshaling raw event", e))
}

/// A JSON value as a protobuf `Struct`: `null` → `{}`; numbers become protobuf doubles.
///
/// # Errors
///
/// A value that is not an object or `null` fails with protojson's text, e.g.
/// `failed to unmarshal JSON data to structpb: ` followed by
/// `proto: syntax error (line 1:1): unexpected token "hello"`.
pub(crate) fn to_struct(value: &Value) -> Result<Value, BoxError> {
    let token = match value {
        Value::Null => return Ok(json!({})),
        Value::Object(_) => return Ok(struct_value(value)),
        Value::Array(_) => "[".to_owned(),
        other => other.to_string(),
    };
    Err(format!(
        "failed to unmarshal JSON data to structpb: proto: syntax error (line 1:1): unexpected token {token}"
    )
    .into())
}

/// `m` without its `null` values.
pub(crate) fn filter_nil_values(m: &Map<String, Value>) -> Map<String, Value> {
    m.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// A content as protojson. Each part carries `thought`, `thoughtSignature` and the
/// last set of text, inline data, function call, function response, executable code, code
/// execution result and file data, in that order.
pub(crate) fn content_to_wire(content: &Content) -> Result<Value, BoxError> {
    let mut parts = Vec::with_capacity(content.parts.len());
    for part in &content.parts {
        let mut p = Map::new();
        put_bool(&mut p, "thought", part.thought);
        put_bytes(&mut p, "thoughtSignature", &part.thought_signature);
        let mut data: Option<(&str, Value)> = None;
        if !part.text.is_empty() {
            data = Some(("text", Value::String(part.text.clone())));
        }
        if let Some(b) = &part.inline_data {
            let mut blob = Map::new();
            put_bytes(&mut blob, "data", &b.data);
            put_str(&mut blob, "mimeType", &b.mime_type);
            data = Some(("inlineData", Value::Object(blob)));
        }
        if let Some(fc) = &part.function_call {
            let mut call = Map::new();
            put_str(&mut call, "id", &fc.id);
            put_str(&mut call, "name", &fc.name);
            call.insert("args".into(), struct_of(&fc.args));
            data = Some(("functionCall", Value::Object(call)));
        }
        if let Some(fr) = &part.function_response {
            let mut resp = Map::new();
            put_str(&mut resp, "id", &fr.id);
            put_str(&mut resp, "name", &fr.name);
            resp.insert("response".into(), struct_of(&fr.response));
            data = Some(("functionResponse", Value::Object(resp)));
        }
        if let Some(ec) = &part.executable_code {
            let mut code = Map::new();
            put_str(&mut code, "code", &ec.code);
            let language = language_number(ec.language.as_ref().map_or("", |l| l.as_str()));
            put_enum(&mut code, "language", language);
            data = Some(("executableCode", Value::Object(code)));
        }
        if let Some(cr) = &part.code_execution_result {
            let mut result = Map::new();
            let outcome = outcome_number(cr.outcome.as_ref().map_or("", |o| o.as_str()));
            put_enum(&mut result, "outcome", outcome);
            put_str(&mut result, "output", &cr.output);
            data = Some(("codeExecutionResult", Value::Object(result)));
        }
        if let Some(fd) = &part.file_data {
            let mut file = Map::new();
            put_str(&mut file, "mimeType", &fd.mime_type);
            put_str(&mut file, "fileUri", &fd.file_uri);
            data = Some(("fileData", Value::Object(file)));
        }
        if let Some((key, value)) = data {
            p.insert(key.into(), value);
        }
        parts.push(Value::Object(p));
    }
    let mut c = Map::new();
    put_str(&mut c, "role", &content.role);
    if !parts.is_empty() {
        c.insert("parts".into(), Value::Array(parts));
    }
    Ok(Value::Object(c))
}

/// A content from protojson: role and parts (thought, thought signature and the one set
/// data field).
pub(crate) fn content_from_wire(v: &Value) -> Content {
    let parts = array_field(v, "parts")
        .iter()
        .map(|p| {
            let mut part = Part {
                thought: bool_field(p, "thought"),
                thought_signature: bytes_field(p, "thoughtSignature").into(),
                ..Default::default()
            };
            if let Some(t) = field(p, "text") {
                part.text = t.as_str().unwrap_or_default().to_owned();
            } else if let Some(b) = field(p, "inlineData") {
                part.inline_data = Some(Blob {
                    mime_type: str_field(b, "mimeType").to_owned(),
                    data: bytes_field(b, "data").into(),
                    ..Default::default()
                });
            } else if let Some(fc) = field(p, "functionCall") {
                part.function_call = Some(FunctionCall {
                    id: str_field(fc, "id").to_owned(),
                    name: str_field(fc, "name").to_owned(),
                    args: struct_as_map(field(fc, "args")),
                    ..Default::default()
                });
            } else if let Some(fr) = field(p, "functionResponse") {
                part.function_response = Some(FunctionResponse {
                    id: str_field(fr, "id").to_owned(),
                    name: str_field(fr, "name").to_owned(),
                    response: struct_as_map(field(fr, "response")),
                    ..Default::default()
                });
            } else if let Some(ec) = field(p, "executableCode") {
                let name = language_name(ec.get("language").unwrap_or(&Value::Null));
                part.executable_code = Some(ExecutableCode {
                    code: str_field(ec, "code").to_owned(),
                    language: Some(name.parse::<Language>().unwrap_or_else(|e| match e {})),
                    ..Default::default()
                });
            } else if let Some(cr) = field(p, "codeExecutionResult") {
                let name = outcome_name(cr.get("outcome").unwrap_or(&Value::Null));
                part.code_execution_result = Some(CodeExecutionResult {
                    outcome: Some(name.parse::<Outcome>().unwrap_or_else(|e| match e {})),
                    output: str_field(cr, "output").to_owned(),
                    ..Default::default()
                });
            } else if let Some(fd) = field(p, "fileData") {
                part.file_data = Some(FileData {
                    mime_type: str_field(fd, "mimeType").to_owned(),
                    file_uri: str_field(fd, "fileUri").to_owned(),
                    ..Default::default()
                });
            }
            part
        })
        .collect();
    Content {
        parts,
        role: str_field(v, "role").to_owned(),
    }
}

/// The event actions as protojson (`None` when both deltas and the requested auth configs
/// are empty).
///
/// # Errors
///
/// `artifact "{name}" version {v} does not fit in int32` for a version outside `i32`;
/// `failed to convert requested auth configs to structpb: …` when the configs do not convert.
pub(crate) fn actions_to_wire(event: &Event) -> Result<Option<Value>, BoxError> {
    let state_delta = event.actions.state_delta.as_ref().filter(|m| !m.is_empty());
    let artifact_delta = event
        .actions
        .artifact_delta
        .as_ref()
        .filter(|m| !m.is_empty());
    let requested_auth_configs = &event.actions.requested_auth_configs;
    if state_delta.is_none() && artifact_delta.is_none() && requested_auth_configs.is_empty() {
        return Ok(None);
    }
    let mut actions = Map::new();
    if let Some(delta) = state_delta {
        actions.insert("stateDelta".into(), struct_of(delta));
    }
    if let Some(delta) = artifact_delta {
        let mut versions = Map::new();
        for (name, version) in delta {
            let Ok(v) = i32::try_from(*version) else {
                return Err(format!(
                    "artifact {} version {version} does not fit in int32",
                    go_quote(name)
                )
                .into());
            };
            versions.insert(name.clone(), json!(v));
        }
        actions.insert("artifactDelta".into(), Value::Object(versions));
    }
    if !requested_auth_configs.is_empty() {
        let configs = serde_json::to_value(requested_auth_configs)
            .map_err(|e| wrap("failed to marshal value", e))
            .and_then(|v| to_struct(&v))
            .map_err(|e| wrap("failed to convert requested auth configs to structpb", e))?;
        actions.insert("requestedAuthConfigs".into(), configs);
    }
    Ok(Some(Value::Object(actions)))
}

/// The event actions from protojson: absent actions give empty actions; present ones
/// always carry a state delta (`null` values dropped), an artifact delta when non-empty and the
/// requested auth configs when non-empty.
///
/// # Errors
///
/// `unmarshaling requested auth configs: …` when a requested auth config does not decode as a
/// `toolauth::Config`.
pub(crate) fn actions_from_wire(v: Option<&Value>) -> Result<EventActions, BoxError> {
    let Some(actions) = v.filter(|a| !a.is_null()) else {
        return Ok(EventActions::default());
    };
    let versions: BTreeMap<String, i64> = object_field(actions, "artifactDelta")
        .map(|m| {
            m.iter()
                .map(|(name, v)| (name.clone(), int64_value(v).unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    let configs = struct_as_map(field(actions, "requestedAuthConfigs"));
    let requested_auth_configs = if configs.is_empty() {
        BTreeMap::new()
    } else {
        serde_json::from_value(Value::Object(configs))
            .map_err(|e| wrap("unmarshaling requested auth configs", e))?
    };
    Ok(EventActions {
        state_delta: Some(filter_nil_values(&struct_as_map(field(
            actions,
            "stateDelta",
        )))),
        artifact_delta: (!versions.is_empty()).then_some(versions),
        requested_auth_configs,
        ..Default::default()
    })
}

/// The event metadata as protojson: always an object.
pub(crate) fn metadata_to_wire(event: &Event) -> Result<Value, BoxError> {
    let r = &event.response;
    let mut m = Map::new();
    put_bool(&mut m, "partial", r.partial);
    put_bool(&mut m, "turnComplete", r.turn_complete);
    put_bool(&mut m, "interrupted", r.interrupted);
    put_strings(&mut m, "longRunningToolIds", &event.long_running_tool_ids);
    put_str(&mut m, "branch", &event.branch);
    // The struct is written for a non-empty map; there is no nil/empty distinction here.
    if !r.custom_metadata.is_empty() {
        m.insert("customMetadata".into(), struct_of(&r.custom_metadata));
    }
    if let Some(g) = &r.grounding_metadata {
        m.insert("groundingMetadata".into(), grounding_to_wire(g));
    }
    Ok(Value::Object(m))
}

/// The `groundingMetadata` part of the event metadata.
fn grounding_to_wire(g: &GroundingMetadata) -> Value {
    let mut m = Map::new();
    put_strings(&mut m, "webSearchQueries", &g.web_search_queries);
    put_strings(&mut m, "retrievalQueries", &g.retrieval_queries);
    // The optional field is set, so it is present even when empty.
    m.insert(
        "googleMapsWidgetContextToken".into(),
        Value::String(g.google_maps_widget_context_token.clone()),
    );
    if let Some(sep) = &g.search_entry_point {
        let mut s = Map::new();
        put_str(&mut s, "renderedContent", &sep.rendered_content);
        put_bytes(&mut s, "sdkBlob", &sep.sdk_blob);
        m.insert("searchEntryPoint".into(), Value::Object(s));
    }
    if let Some(rm) = &g.retrieval_metadata {
        let mut r = Map::new();
        if rm.google_search_dynamic_retrieval_score != 0.0 {
            r.insert(
                "googleSearchDynamicRetrievalScore".into(),
                go_float32(rm.google_search_dynamic_retrieval_score),
            );
        }
        m.insert("retrievalMetadata".into(), Value::Object(r));
    }

    let mut chunks = Vec::new();
    for gc in &g.grounding_chunks {
        if let Some(maps) = &gc.maps {
            let mut mp = Map::new();
            mp.insert("uri".into(), Value::String(maps.uri.clone()));
            mp.insert("title".into(), Value::String(maps.title.clone()));
            mp.insert("text".into(), Value::String(maps.text.clone()));
            mp.insert("placeId".into(), Value::String(maps.place_id.clone()));
            if let Some(pas) = &maps.place_answer_sources {
                let snippets: Vec<Value> = pas
                    .review_snippets
                    .iter()
                    .map(|s| {
                        let mut sn = Map::new();
                        // `reviewId` comes from the snippet's `review` field.
                        put_str(&mut sn, "reviewId", &s.review);
                        put_str(&mut sn, "googleMapsUri", &s.google_maps_uri);
                        put_str(&mut sn, "title", &s.title);
                        Value::Object(sn)
                    })
                    .collect();
                let mut sources = Map::new();
                if !snippets.is_empty() {
                    sources.insert("reviewSnippets".into(), Value::Array(snippets));
                }
                mp.insert("placeAnswerSources".into(), Value::Object(sources));
            }
            chunks.push(json!({ "maps": mp }));
        }
        if let Some(rc) = &gc.retrieved_context {
            let mut r = Map::new();
            r.insert("uri".into(), Value::String(rc.uri.clone()));
            r.insert("title".into(), Value::String(rc.title.clone()));
            r.insert("text".into(), Value::String(rc.text.clone()));
            r.insert(
                "documentName".into(),
                Value::String(rc.document_name.clone()),
            );
            if let Some(rag) = &rc.rag_chunk
                && let Some(span) = &rag.page_span
            {
                let mut ps = Map::new();
                put_i32(&mut ps, "firstPage", span.first_page);
                put_i32(&mut ps, "lastPage", span.last_page);
                let mut chunk = Map::new();
                put_str(&mut chunk, "text", &rag.text);
                chunk.insert("pageSpan".into(), Value::Object(ps));
                r.insert("ragChunk".into(), Value::Object(chunk));
            }
            chunks.push(json!({ "retrievedContext": r }));
        }
        if let Some(web) = &gc.web {
            chunks.push(json!({ "web": { "uri": web.uri, "title": web.title } }));
        }
    }
    if !chunks.is_empty() {
        m.insert("groundingChunks".into(), Value::Array(chunks));
    }

    let supports: Vec<Value> = g
        .grounding_supports
        .iter()
        .map(|gs| {
            let mut s = Map::new();
            if !gs.grounding_chunk_indices.is_empty() {
                s.insert(
                    "groundingChunkIndices".into(),
                    json!(gs.grounding_chunk_indices),
                );
            }
            if !gs.confidence_scores.is_empty() {
                let scores = gs.confidence_scores.iter().map(|f| go_float32(*f));
                s.insert("confidenceScores".into(), Value::Array(scores.collect()));
            }
            if let Some(seg) = &gs.segment {
                let mut sg = Map::new();
                put_i32(&mut sg, "partIndex", seg.part_index);
                put_i32(&mut sg, "startIndex", seg.start_index);
                put_i32(&mut sg, "endIndex", seg.end_index);
                put_str(&mut sg, "text", &seg.text);
                s.insert("segment".into(), Value::Object(sg));
            }
            Value::Object(s)
        })
        .collect();
    if !supports.is_empty() {
        m.insert("groundingSupports".into(), Value::Array(supports));
    }
    Value::Object(m)
}

/// Grounding metadata from a present `groundingMetadata`.
pub(crate) fn grounding_from_wire(v: &Value) -> GroundingMetadata {
    let mut out = GroundingMetadata {
        web_search_queries: strings_field(v, "webSearchQueries"),
        retrieval_queries: strings_field(v, "retrievalQueries"),
        google_maps_widget_context_token: str_field(v, "googleMapsWidgetContextToken").to_owned(),
        ..Default::default()
    };
    if let Some(sep) = field(v, "searchEntryPoint") {
        out.search_entry_point = Some(SearchEntryPoint {
            rendered_content: str_field(sep, "renderedContent").to_owned(),
            sdk_blob: bytes_field(sep, "sdkBlob").into(),
        });
    }
    if let Some(rm) = field(v, "retrievalMetadata") {
        out.retrieval_metadata = Some(RetrievalMetadata {
            google_search_dynamic_retrieval_score: float32_field(
                rm,
                "googleSearchDynamicRetrievalScore",
            ),
        });
    }
    out.grounding_chunks = array_field(v, "groundingChunks")
        .iter()
        .map(|chunk| {
            let mut new_chunk = GroundingChunk::default();
            if let Some(maps) = field(chunk, "maps") {
                let mut new_maps = GroundingChunkMaps {
                    uri: str_field(maps, "uri").to_owned(),
                    title: str_field(maps, "title").to_owned(),
                    text: str_field(maps, "text").to_owned(),
                    place_id: str_field(maps, "placeId").to_owned(),
                    ..Default::default()
                };
                if let Some(pas) = field(maps, "placeAnswerSources") {
                    new_maps.place_answer_sources = Some(GroundingChunkMapsPlaceAnswerSources {
                        review_snippets: array_field(pas, "reviewSnippets")
                            .iter()
                            .map(|s| GroundingChunkMapsPlaceAnswerSourcesReviewSnippet {
                                review: str_field(s, "reviewId").to_owned(),
                                google_maps_uri: str_field(s, "googleMapsUri").to_owned(),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    });
                }
                new_chunk.maps = Some(new_maps);
            }
            if let Some(rc) = field(chunk, "retrievedContext") {
                let mut new_rc = GroundingChunkRetrievedContext {
                    uri: str_field(rc, "uri").to_owned(),
                    title: str_field(rc, "title").to_owned(),
                    text: str_field(rc, "text").to_owned(),
                    document_name: str_field(rc, "documentName").to_owned(),
                    ..Default::default()
                };
                if let Some(rag) = field(rc, "ragChunk") {
                    new_rc.rag_chunk = Some(RAGChunk {
                        text: str_field(rag, "text").to_owned(),
                        page_span: field(rag, "pageSpan").map(|ps| RAGChunkPageSpan {
                            first_page: int32_field(ps, "firstPage"),
                            last_page: int32_field(ps, "lastPage"),
                        }),
                        ..Default::default()
                    });
                }
                new_chunk.retrieved_context = Some(new_rc);
            }
            if let Some(web) = field(chunk, "web") {
                new_chunk.web = Some(GroundingChunkWeb {
                    uri: str_field(web, "uri").to_owned(),
                    title: str_field(web, "title").to_owned(),
                    ..Default::default()
                });
            }
            new_chunk
        })
        .collect();
    out.grounding_supports = array_field(v, "groundingSupports")
        .iter()
        .map(|gs| GroundingSupport {
            grounding_chunk_indices: array_field(gs, "groundingChunkIndices")
                .iter()
                .map(int32_value)
                .collect(),
            confidence_scores: array_field(gs, "confidenceScores")
                .iter()
                .map(float32_value)
                .collect(),
            segment: field(gs, "segment").map(|s| Segment {
                part_index: int32_field(s, "partIndex"),
                start_index: int32_field(s, "startIndex"),
                end_index: int32_field(s, "endIndex"),
                text: str_field(s, "text").to_owned(),
            }),
            ..Default::default()
        })
        .collect();
    out
}

// ---- Struct numbers ----

/// The JSON number written for the `f64` `f` (JSON and protojson alike), as
/// serde_json reads it back: an integral value below 1e21 has no fraction or exponent, so it
/// reads as an integer when it fits one.
fn go_double(f: f64) -> Value {
    // -0 is written as "-0", which serde_json reads as the float -0.0.
    let negative_zero = f == 0.0 && f.is_sign_negative();
    if f.fract() == 0.0 && f.abs() < 1e21 && !negative_zero {
        if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f) {
            // In range and integral, so the conversion is exact.
            return Value::from(f as i64);
        }
        if (0.0..18_446_744_073_709_551_616.0).contains(&f) {
            return Value::from(f as u64);
        }
    }
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A protojson `float` value: its shortest decimal form, read as a double;
/// non-finite values as protojson's strings.
fn go_float32(f: f32) -> Value {
    if f.is_nan() {
        return Value::String("NaN".into());
    }
    if f.is_infinite() {
        let name = if f > 0.0 { "Infinity" } else { "-Infinity" };
        return Value::String(name.into());
    }
    go_double(f.to_string().parse::<f64>().unwrap_or(f64::from(f)))
}

/// `v` as a `google.protobuf.Struct` / `Value` holds it: every number a double.
fn struct_value(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Null, go_double),
        Value::Array(a) => Value::Array(a.iter().map(struct_value).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), struct_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A map as a protobuf `Struct` (never fails: a map is always an object).
fn struct_of(m: &Map<String, Value>) -> Value {
    Value::Object(
        m.iter()
            .map(|(k, v)| (k.clone(), struct_value(v)))
            .collect(),
    )
}

/// An optional `Struct` field (e.g. a session's `sessionState`) as a map: its
/// numbers read back as doubles; an absent field is an empty map.
pub(crate) fn struct_as_map(v: Option<&Value>) -> Map<String, Value> {
    match v.filter(|s| !s.is_null()).map(struct_value) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

// ---- Reading protojson fields ----

/// A field's value; protojson treats `null` as absent.
pub(crate) fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|f| !f.is_null())
}

/// A protojson string field; an absent (or non-string) one is `""`.
pub(crate) fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn bool_field(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or_default()
}

pub(crate) fn array_field<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn object_field<'a>(v: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    v.get(key).and_then(Value::as_object)
}

fn strings_field(v: &Value, key: &str) -> Vec<String> {
    array_field(v, key)
        .iter()
        .map(|s| s.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A `bytes` field; text that is not base64 reads as empty. The lenience is safe because
/// [`session_event_to_event`] has already rejected such text through [`validate_scalars`]; the
/// infallible readers are otherwise reached by tests with well-formed input and by
/// the Example Store's response mapping (`crate::example_store`), which first rejects such text
/// through [`check_bytes`].
pub(crate) fn bytes_field(v: &Value, key: &str) -> Vec<u8> {
    decode_bytes(str_field(v, key)).unwrap_or_default()
}

fn int32_value(v: &Value) -> i32 {
    int64_value(v)
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or_default()
}

fn int32_field(v: &Value, key: &str) -> i32 {
    v.get(key).map(int32_value).unwrap_or_default()
}

/// A protojson `float`: a number, a numeric string, or `NaN` / `Infinity` / `-Infinity`.
fn float32_value(v: &Value) -> f32 {
    let f = match v {
        Value::Number(n) => n.as_f64().unwrap_or_default(),
        Value::String(s) => match s.as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            other => other.parse().unwrap_or_default(),
        },
        _ => 0.0,
    };
    f as f32
}

pub(crate) fn float32_field(v: &Value, key: &str) -> f32 {
    v.get(key).map(float32_value).unwrap_or_default()
}

/// The `google.protobuf.Timestamp` field `key` of a protojson object (an event's `timestamp`,
/// a session's `updateTime`); an absent or null one is the Unix epoch.
pub(crate) fn timestamp_field(v: &Value, key: &str) -> Result<DateTime<Utc>, String> {
    match field(v, key) {
        None => Ok(DateTime::UNIX_EPOCH),
        Some(Value::String(s)) => parse_timestamp(s),
        Some(other) => Err(format!("invalid google.protobuf.Timestamp value {other}")),
    }
}

// ---- Strict scalar validation (protojson's scalar rules) ----

/// protojson's `invalid value for {kind} field {name}: {raw token}`, without its
/// `proto: (line:col): ` prefix (as `parse_timestamp`'s texts).
fn invalid_scalar(kind: &str, name: &str, v: &Value) -> String {
    let raw = serde_json::to_string(v).unwrap_or_default();
    format!("invalid value for {kind} field {name}: {raw}")
}

/// Whether `s` is exactly one JSON number.
fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |start: usize| {
        let mut i = start;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    };
    let mut i = usize::from(b.first() == Some(&b'-'));
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => i = digits(i),
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        let end = digits(i + 1);
        if end == i + 1 {
            return false;
        }
        i = end;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let end = digits(i);
        if end == i {
            return false;
        }
        i = end;
    }
    i == b.len()
}

/// protojson's `unmarshalFloat(tok, 32)`: a number or numeric string that fits a `float`, or
/// `NaN` / `Infinity` / `-Infinity`.
fn float32_is_valid(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().is_some_and(|f| (f as f32).is_finite()),
        Value::String(s) => {
            matches!(s.as_str(), "NaN" | "Infinity" | "-Infinity")
                || (is_json_number(s) && s.parse::<f32>().is_ok_and(f32::is_finite))
        }
        _ => false,
    }
}

/// protojson's check of a `bytes` field: absent or `null` is fine; anything but valid base64
/// text fails with `invalid value for bytes field {key}: {raw token}`.
pub(crate) fn check_bytes(v: &Value, key: &str) -> Result<(), String> {
    match field(v, key) {
        Some(f) if f.as_str().is_none_or(|s| decode_bytes(s).is_err()) => {
            Err(invalid_scalar("bytes", key, f))
        }
        _ => Ok(()),
    }
}

/// `name` is the descriptor's JSON name (`value` for a map value).
fn check_int32(f: &Value, name: &str) -> Result<(), String> {
    // `null` is rejected here: protojson skips it only for a singular field (see
    // `check_int32_field`), not for list elements or map values.
    if int64_value(f).is_some_and(|n| i32::try_from(n).is_ok()) {
        Ok(())
    } else {
        Err(invalid_scalar("int32", name, f))
    }
}

fn check_int32_field(v: &Value, key: &str) -> Result<(), String> {
    field(v, key).map_or(Ok(()), |f| check_int32(f, key))
}

fn check_int32_list(v: &Value, key: &str) -> Result<(), String> {
    array_field(v, key)
        .iter()
        .try_for_each(|f| check_int32(f, key))
}

fn check_float_list(v: &Value, key: &str) -> Result<(), String> {
    for f in array_field(v, key) {
        if !float32_is_valid(f) {
            return Err(invalid_scalar("float", key, f));
        }
    }
    Ok(())
}

fn check_string_list(v: &Value, key: &str) -> Result<(), String> {
    for f in array_field(v, key) {
        if !f.is_string() {
            return Err(invalid_scalar("string", key, f));
        }
    }
    Ok(())
}

/// Rejects what protojson would reject in the fields [`session_event_to_event`] reads: `bytes`
/// that are not base64, `int32`s that are not whole numbers in range, `float`s that do not
/// parse, and non-string elements of repeated `string` fields.
fn validate_scalars(v: &Value) -> Result<(), String> {
    if let Some(content) = field(v, "content") {
        for part in array_field(content, "parts") {
            check_bytes(part, "thoughtSignature")?;
            if let Some(blob) = field(part, "inlineData") {
                check_bytes(blob, "data")?;
            }
        }
    }
    if let Some(deltas) = field(v, "actions").and_then(|a| object_field(a, "artifactDelta")) {
        for version in deltas.values() {
            check_int32(version, "value")?;
        }
    }
    if let Some(md) = field(v, "eventMetadata") {
        check_string_list(md, "longRunningToolIds")?;
        if let Some(g) = field(md, "groundingMetadata") {
            validate_grounding(g)?;
        }
    }
    Ok(())
}

fn validate_grounding(g: &Value) -> Result<(), String> {
    check_string_list(g, "webSearchQueries")?;
    check_string_list(g, "retrievalQueries")?;
    if let Some(sep) = field(g, "searchEntryPoint") {
        check_bytes(sep, "sdkBlob")?;
    }
    let score =
        field(g, "retrievalMetadata").and_then(|r| field(r, "googleSearchDynamicRetrievalScore"));
    if let Some(f) = score
        && !float32_is_valid(f)
    {
        return Err(invalid_scalar(
            "float",
            "googleSearchDynamicRetrievalScore",
            f,
        ));
    }
    for chunk in array_field(g, "groundingChunks") {
        let span = field(chunk, "retrievedContext")
            .and_then(|rc| field(rc, "ragChunk"))
            .and_then(|rag| field(rag, "pageSpan"));
        if let Some(span) = span {
            check_int32_field(span, "firstPage")?;
            check_int32_field(span, "lastPage")?;
        }
    }
    for support in array_field(g, "groundingSupports") {
        check_int32_list(support, "groundingChunkIndices")?;
        check_float_list(support, "confidenceScores")?;
        if let Some(seg) = field(support, "segment") {
            for key in ["partIndex", "startIndex", "endIndex"] {
                check_int32_field(seg, key)?;
            }
        }
    }
    Ok(())
}

// ---- Writing protojson fields (unpopulated scalars omitted) ----

fn put_str(m: &mut Map<String, Value>, key: &str, s: &str) {
    if !s.is_empty() {
        m.insert(key.into(), Value::String(s.to_owned()));
    }
}

fn put_bool(m: &mut Map<String, Value>, key: &str, b: bool) {
    if b {
        m.insert(key.into(), Value::Bool(true));
    }
}

fn put_bytes(m: &mut Map<String, Value>, key: &str, b: &[u8]) {
    if !b.is_empty() {
        m.insert(key.into(), Value::String(encode_bytes(b)));
    }
}

fn put_i32(m: &mut Map<String, Value>, key: &str, n: i32) {
    if n != 0 {
        m.insert(key.into(), json!(n));
    }
}

fn put_enum(m: &mut Map<String, Value>, key: &str, n: i64) {
    if n != 0 {
        m.insert(key.into(), json!(n));
    }
}

fn put_strings(m: &mut Map<String, Value>, key: &str, v: &[String]) {
    if !v.is_empty() {
        m.insert(key.into(), json!(v));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
