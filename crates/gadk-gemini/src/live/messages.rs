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

//! Live wire messages.
//!
//! Outgoing: the client envelope ([`LiveClientMessage`]) with the converters for the setup,
//! client-content, tool-response, context-update and realtime-input messages on each backend.
//!
//! Incoming: decoding of one server frame (Vertex AI messages are converted to the Gemini API
//! shape) and the mapping of server messages to [`LlmResponse`]s.

use std::collections::VecDeque;

use gadk_genai::{
    Content, FunctionResponse, GenerateContentResponseUsageMetadata, LiveClientContentInput,
    LiveClientContextUpdate, LiveConnectConfig, LiveRealtimeInput, LiveServerMessage,
    LiveToolResponseInput, Part, Transcription, UsageMetadata,
};
use gadk_model::{Backend, LlmResponse, ModelError, body_prefix};
use serde::Serialize;
use serde_json::Value;

use super::live_other;
use crate::error::GeminiError;
use crate::request::{
    Obj, as_obj, content, convert_list, convert_obj, copy, function_response_vertex, reject,
    safety_setting_mldev, speech_config_vertex, to_value, tool,
};

/// The client envelope: exactly one field is set per message. Fields hold the
/// already-converted wire objects.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveClientMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<Obj>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_content: Option<Obj>,
    #[serde(skip_serializing_if = "Option::is_none")]
    realtime_input: Option<Obj>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_response: Option<Obj>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_update: Option<Obj>,
}

impl LiveClientMessage {
    fn to_json(&self) -> Result<String, GeminiError> {
        serde_json::to_string(self)
            .map_err(|e| live_other(format!("marshal client message error: {e}")))
    }
}

/// Sets `["setup"]["generationConfig"][key]` on the setup message.
fn set_generation(setup: &mut Obj, key: &str, v: Value) {
    if let Value::Object(gc) = setup
        .entry("generationConfig")
        .or_insert_with(|| Value::Object(Obj::new()))
    {
        gc.insert(key.to_string(), v);
    }
}

fn get<'a>(from: &'a Obj, key: &str) -> Option<&'a Value> {
    from.get(key).filter(|v| !v.is_null())
}

/// Builds the `{"setup": ...}` message as JSON text, converting the connect config
/// (including session resumption and the speech config) for the backend.
pub(crate) fn setup_message(
    backend: Backend,
    model_full_name: &str,
    cfg: &LiveConnectConfig,
) -> Result<String, GeminiError> {
    let vertex = backend == Backend::VertexAi;
    let from = as_obj(&to_value(cfg)?);
    let mut setup = Obj::new();
    setup.insert("model".into(), Value::String(model_full_name.to_string()));

    for key in [
        "responseModalities",
        "temperature",
        "topP",
        "topK",
        "maxOutputTokens",
        "mediaResolution",
        "seed",
    ] {
        if let Some(v) = get(&from, key) {
            set_generation(&mut setup, key, v.clone());
        }
    }
    if let Some(v) = get(&from, "speechConfig") {
        // tLiveSpeechConfig.
        let sc = as_obj(v);
        if sc.contains_key("multiSpeakerVoiceConfig") {
            return Err(GeminiError::Config(
                "multiSpeakerVoiceConfig is not supported in the live API".into(),
            ));
        }
        let sc = if vertex {
            speech_config_vertex(&sc)?
        } else {
            sc
        };
        set_generation(&mut setup, "speechConfig", Value::Object(sc));
    }
    for key in ["thinkingConfig", "enableAffectiveDialog"] {
        if let Some(v) = get(&from, key) {
            set_generation(&mut setup, key, v.clone());
        }
    }
    if let Some(v) = get(&from, "systemInstruction") {
        setup.insert(
            "systemInstruction".into(),
            Value::Object(content(&as_obj(v), backend)?),
        );
    }
    convert_list(&from, &mut setup, "tools", |t| tool(t, backend))?;
    if vertex {
        copy(&from, &mut setup, &["sessionResumption"]);
    } else {
        convert_obj(&from, &mut setup, "sessionResumption", |sr| {
            let mut out = Obj::new();
            copy(sr, &mut out, &["handle"]);
            reject(sr, &["transparent"], backend)?;
            Ok(out)
        })?;
    }
    copy(
        &from,
        &mut setup,
        &[
            "inputAudioTranscription",
            "outputAudioTranscription",
            "realtimeInputConfig",
            "contextWindowCompression",
            "proactivity",
        ],
    );
    if vertex {
        copy(&from, &mut setup, &["explicitVadSignal"]);
    } else {
        reject(&from, &["explicitVadSignal"], backend)?;
    }
    copy(&from, &mut setup, &["avatarConfig"]);
    if vertex {
        copy(&from, &mut setup, &["safetySettings"]);
    } else {
        convert_list(&from, &mut setup, "safetySettings", safety_setting_mldev)?;
    }
    if let Some(v) = get(&from, "translationConfig") {
        set_generation(&mut setup, "translationConfig", v.clone());
    }
    copy(&from, &mut setup, &["labels"]);

    LiveClientMessage {
        setup: Some(setup),
        ..Default::default()
    }
    .to_json()
}

/// Builds a client-content message (a
/// missing `turnComplete` means true), converting the contents for the backend.
pub(crate) fn client_content_message(
    backend: Backend,
    input: &LiveClientContentInput,
) -> Result<String, GeminiError> {
    let mut cc = Obj::new();
    // LiveClientContent: `turns` and `turnComplete` are omitempty.
    if !input.turns.is_empty() {
        let turns = input
            .turns
            .iter()
            .map(|c| content(&as_obj(&to_value(c)?), backend).map(Value::Object))
            .collect::<Result<Vec<_>, ModelError>>()?;
        cc.insert("turns".into(), Value::Array(turns));
    }
    if input.turn_complete.unwrap_or(true) {
        cc.insert("turnComplete".into(), Value::Bool(true));
    }
    LiveClientMessage {
        client_content: Some(cc),
        ..Default::default()
    }
    .to_json()
}

/// Builds a tool-response message. The
/// Gemini API copies the tool response unconverted; Vertex AI converts each function response
/// with [`function_response_vertex`].
pub(crate) fn tool_response_message(
    backend: Backend,
    input: &LiveToolResponseInput,
) -> Result<String, GeminiError> {
    let from = as_obj(&to_value(input)?);
    let tool_response = match backend {
        Backend::GeminiApi => from,
        Backend::VertexAi => {
            let mut to = Obj::new();
            convert_list(
                &from,
                &mut to,
                "functionResponses",
                function_response_vertex,
            )?;
            to
        }
    };
    LiveClientMessage {
        tool_response: Some(tool_response),
        ..Default::default()
    }
    .to_json()
}

/// Builds a context-update message. The system instruction goes through
/// [`content`], each tool through [`tool`]; a set but empty tools wrapper stays `{}`.
pub(crate) fn context_update_message(
    backend: Backend,
    input: &LiveClientContextUpdate,
) -> Result<String, GeminiError> {
    let from = as_obj(&to_value(input)?);
    let mut to = Obj::new();
    if let Some(v) = get(&from, "systemInstruction") {
        to.insert(
            "systemInstruction".into(),
            Value::Object(content(&as_obj(v), backend)?),
        );
    }
    convert_obj(&from, &mut to, "tools", |tools| {
        let mut out = Obj::new();
        convert_list(tools, &mut out, "tools", |t| tool(t, backend))?;
        Ok(out)
    })?;
    LiveClientMessage {
        context_update: Some(to),
        ..Default::default()
    }
    .to_json()
}

/// Checks that the blob's MIME type has the given prefix (`audio/` or `image/`) and returns a
/// copy of the blob.
fn typed_blob(blob: &Value, prefix: &str) -> Result<Value, GeminiError> {
    let mime = blob.get("mimeType").and_then(Value::as_str).unwrap_or("");
    if mime.starts_with(prefix) {
        Ok(blob.clone())
    } else {
        Err(live_other(format!("Unsupported mime type: {mime}")))
    }
}

/// Builds a realtime-input message (the Gemini API and Vertex AI shapes are identical):
/// `media` becomes `mediaChunks: [media]`, `audio`/`video` are checked
/// for an `audio/`/`image/` MIME type.
pub(crate) fn realtime_input_message(input: &LiveRealtimeInput) -> Result<String, GeminiError> {
    let from = as_obj(&to_value(input)?);
    let mut to = Obj::new();
    if let Some(media) = get(&from, "media") {
        to.insert("mediaChunks".into(), Value::Array(vec![media.clone()]));
    }
    if let Some(audio) = get(&from, "audio") {
        to.insert("audio".into(), typed_blob(audio, "audio/")?);
    }
    copy(&from, &mut to, &["audioStreamEnd"]);
    if let Some(video) = get(&from, "video") {
        to.insert("video".into(), typed_blob(video, "image/")?);
    }
    copy(&from, &mut to, &["text", "activityStart", "activityEnd"]);
    LiveClientMessage {
        realtime_input: Some(to),
        ..Default::default()
    }
    .to_json()
}

/// Decodes one frame: rejects `error` payloads, and converts the message from the Vertex AI
/// shape on Vertex AI (the Gemini API message is used as is).
pub(crate) fn decode_server_message(
    backend: Backend,
    data: &[u8],
) -> Result<LiveServerMessage, GeminiError> {
    let value: Value = serde_json::from_slice(data).map_err(|e| {
        live_other(format!(
            "invalid message format. Error {e}. message: {}",
            body_prefix(data)
        ))
    })?;
    if value.get("error").is_some_and(|e| !e.is_null()) {
        return Err(live_other(format!(
            "received error in response: {}",
            body_prefix(data)
        )));
    }
    let value = match backend {
        Backend::VertexAi => Value::Object(server_message_from_vertex(&as_obj(&value))),
        Backend::GeminiApi => value,
    };
    serde_json::from_value(value).map_err(|e| live_other(format!("invalid server message: {e}")))
}

/// Converts a Vertex AI server message to the Gemini API shape (including its usage metadata
/// and voice activity).
fn server_message_from_vertex(from: &Obj) -> Obj {
    let mut to = Obj::new();
    copy(
        from,
        &mut to,
        &[
            "setupComplete",
            "serverContent",
            "toolCall",
            "toolCallCancellation",
        ],
    );
    if let Some(um) = get(from, "usageMetadata") {
        let um = as_obj(um);
        let mut out = Obj::new();
        for (src, dst) in [
            ("promptTokenCount", "promptTokenCount"),
            ("cachedContentTokenCount", "cachedContentTokenCount"),
            ("candidatesTokenCount", "responseTokenCount"),
            ("toolUsePromptTokenCount", "toolUsePromptTokenCount"),
            ("thoughtsTokenCount", "thoughtsTokenCount"),
            ("totalTokenCount", "totalTokenCount"),
            ("promptTokensDetails", "promptTokensDetails"),
            ("cacheTokensDetails", "cacheTokensDetails"),
            ("candidatesTokensDetails", "responseTokensDetails"),
            ("toolUsePromptTokensDetails", "toolUsePromptTokensDetails"),
            ("trafficType", "trafficType"),
        ] {
            if let Some(v) = get(&um, src) {
                out.insert(dst.to_string(), v.clone());
            }
        }
        to.insert("usageMetadata".into(), Value::Object(out));
    }
    copy(
        from,
        &mut to,
        &[
            "goAway",
            "sessionResumptionUpdate",
            "voiceActivityDetectionSignal",
        ],
    );
    if let Some(va) = get(from, "voiceActivity") {
        let va = as_obj(va);
        let mut out = Obj::new();
        if let Some(v) = get(&va, "type") {
            out.insert("voiceActivityType".into(), v.clone());
        }
        copy(&va, &mut out, &["audioOffset"]);
        to.insert("voiceActivity".into(), Value::Object(out));
    }
    to
}

/// The receive state of a [`LiveConnection`](super::LiveConnection): accumulated transcription
/// text and the
/// responses queued to be returned before reading the next message.
#[derive(Debug, Default)]
pub(crate) struct RecvState {
    input_transcription_text: String,
    output_transcription_text: String,
    buffered: VecDeque<LlmResponse>,
}

impl RecvState {
    /// The next queued response, if any.
    pub(crate) fn pop_buffered(&mut self) -> Option<LlmResponse> {
        self.buffered.pop_front()
    }

    /// Maps one server message to one response. Messages
    /// without a branch (setupComplete, goAway, ...) map to an empty response; a message's
    /// usage metadata rides on its response.
    pub(crate) fn map(&mut self, msg: LiveServerMessage) -> LlmResponse {
        let mut resp = LlmResponse {
            usage_metadata: msg.usage_metadata.map(live_usage_metadata),
            ..LlmResponse::default()
        };

        if let Some(update) = msg.session_resumption_update {
            resp.session_resumption_handle = update.new_handle;
            return resp;
        }

        if let Some(sc) = msg.server_content {
            if let Some(turn) = sc.model_turn {
                resp.content = Some(turn);
            }
            resp.turn_complete = sc.turn_complete;
            resp.interrupted = sc.interrupted;

            if let Some(t) = sc.input_transcription {
                self.input_transcription_text.push_str(&t.text);
                resp.input_transcription = Some(t);
                resp.partial = true; // partial chunks are not saved to the session
            }
            if let Some(t) = sc.output_transcription {
                self.output_transcription_text.push_str(&t.text);
                resp.output_transcription = Some(t);
                resp.partial = true;
            }

            // Transcription finalization on completion signals.
            if (sc.turn_complete || sc.interrupted)
                && (!self.input_transcription_text.is_empty()
                    || !self.output_transcription_text.is_empty())
            {
                if !self.input_transcription_text.is_empty() {
                    let text = std::mem::take(&mut self.input_transcription_text);
                    self.buffered.push_back(LlmResponse {
                        input_transcription: Some(finished(text)),
                        ..Default::default()
                    });
                }
                if !self.output_transcription_text.is_empty() {
                    let text = std::mem::take(&mut self.output_transcription_text);
                    self.buffered.push_back(LlmResponse {
                        output_transcription: Some(finished(text)),
                        ..Default::default()
                    });
                }
                // The completion response is delivered after the transcriptions.
                self.buffered.push_back(resp);
                return self.buffered.pop_front().unwrap_or_default();
            }
        }

        if let Some(tc) = msg.tool_call {
            let content = resp.content.get_or_insert_with(|| Content {
                role: "model".into(),
                ..Default::default()
            });
            for call in tc.function_calls {
                content.parts.push(Part {
                    function_call: Some(call),
                    ..Default::default()
                });
            }
        }

        resp
    }
}

/// The Live API token usage as the usage type [`LlmResponse`] carries.
/// The Live API names output tokens `responseTokenCount` / `responseTokensDetails` where the
/// generateContent type calls them `candidatesTokenCount` / `candidatesTokensDetails`
/// (adk-python maps the fields the same way). `service_tier` has no counterpart and is dropped.
fn live_usage_metadata(u: UsageMetadata) -> GenerateContentResponseUsageMetadata {
    GenerateContentResponseUsageMetadata {
        prompt_token_count: u.prompt_token_count,
        cached_content_token_count: u.cached_content_token_count,
        candidates_token_count: u.response_token_count,
        total_token_count: u.total_token_count,
        thoughts_token_count: u.thoughts_token_count,
        tool_use_prompt_token_count: u.tool_use_prompt_token_count,
        prompt_tokens_details: u.prompt_tokens_details,
        cache_tokens_details: u.cache_tokens_details,
        candidates_tokens_details: u.response_tokens_details,
        tool_use_prompt_tokens_details: u.tool_use_prompt_tokens_details,
        traffic_type: u.traffic_type,
    }
}

fn finished(text: String) -> Transcription {
    Transcription {
        text,
        finished: true,
        ..Default::default()
    }
}

/// The function responses of `content`, when its first part is one.
pub(crate) fn function_responses(content: &Content) -> Option<Vec<FunctionResponse>> {
    content.parts.first()?.function_response.as_ref()?;
    Some(
        content
            .parts
            .iter()
            .filter_map(|p| p.function_response.clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn setup_routes_config_fields() {
        let cfg: LiveConnectConfig = serde_json::from_value(json!({
            "seed": 3,
            "speechConfig": {"languageCode": "en"},
            "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}],
            "translationConfig": {"targetLanguageCode": "fr"},
            "outputAudioTranscription": {},
        }))
        .unwrap();
        for backend in [Backend::GeminiApi, Backend::VertexAi] {
            let got = parse(&setup_message(backend, "models/m", &cfg).unwrap());
            assert_eq!(
                got,
                json!({"setup": {
                    "model": "models/m",
                    "generationConfig": {
                        "seed": 3,
                        "speechConfig": {"languageCode": "en"},
                        "translationConfig": {"targetLanguageCode": "fr"},
                    },
                    "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}],
                    "outputAudioTranscription": {},
                }})
            );
        }
    }

    #[test]
    fn setup_rejects_multi_speaker_speech() {
        let cfg: LiveConnectConfig = serde_json::from_value(json!({
            "speechConfig": {"multiSpeakerVoiceConfig": {}},
        }))
        .unwrap();
        assert!(setup_message(Backend::GeminiApi, "models/m", &cfg).is_err());
    }

    #[test]
    fn realtime_audio_and_video_check_mime() {
        let bad = LiveRealtimeInput {
            audio: Some(gadk_genai::Blob {
                mime_type: "image/png".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(realtime_input_message(&bad).is_err());
        let bad = LiveRealtimeInput {
            video: Some(gadk_genai::Blob::default()),
            ..Default::default()
        };
        assert!(realtime_input_message(&bad).is_err());
    }

    #[test]
    fn vertex_server_message_renames_usage() {
        let msg = decode_server_message(
            Backend::VertexAi,
            br#"{"usageMetadata":{"candidatesTokenCount":4,"totalTokenCount":9},"voiceActivity":{"type":"ACTIVITY_START"}}"#,
        )
        .unwrap();
        let um = msg.usage_metadata.unwrap();
        assert_eq!(um.response_token_count, 4);
        assert_eq!(um.total_token_count, 9);
        assert!(msg.voice_activity.unwrap().voice_activity_type.is_some());
    }

    #[test]
    fn decode_rejects_invalid_json_and_errors() {
        assert!(decode_server_message(Backend::GeminiApi, b"nope").is_err());
        assert!(decode_server_message(Backend::GeminiApi, br#"{"error":{"code":1}}"#).is_err());
    }

    #[test]
    fn decode_errors_truncate_large_frames() {
        // An unparsable multi-megabyte frame (e.g. base64 audio) is not embedded whole.
        let mut bad = b"{\"serverContent\":\"".to_vec();
        bad.extend(std::iter::repeat_n(b'A', 1 << 20));
        let msg = decode_server_message(Backend::GeminiApi, &bad)
            .unwrap_err()
            .to_string();
        assert!(msg.len() < 4096, "error message is {} bytes", msg.len());
        assert!(msg.contains("invalid message format"));

        let mut err_frame = b"{\"error\":{\"message\":\"".to_vec();
        err_frame.extend(std::iter::repeat_n(b'B', 1 << 20));
        err_frame.extend(b"\"}}");
        let msg = decode_server_message(Backend::GeminiApi, &err_frame)
            .unwrap_err()
            .to_string();
        assert!(msg.len() < 4096, "error message is {} bytes", msg.len());
        assert!(msg.contains("received error in response"));
    }
}
