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

//! Live API types.

use crate::config::{
    AudioTranscriptionConfig, InteractionStatus, MediaResolution, Modality, SafetySetting,
    ServiceTier, SpeechConfig, ThinkingConfig, VoiceConsentSignature,
};
use crate::content::{Blob, Content, FunctionCall, FunctionResponse, Transcription};
use crate::response::{GroundingMetadata, ModalityTokenCount, TrafficType, URLContextMetadata};
use crate::serde_util::{base64_bytes, go_duration, i64_string, i64_string_opt, is_zero};
use crate::tool::Tool;
use bytes::Bytes;
use chrono::TimeDelta;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

crate::string_enum!(
    /// The reason why the turn is complete.
    TurnCompleteReason {
        /// Not specified.
        Unspecified => "TURN_COMPLETE_REASON_UNSPECIFIED",
        /// The model produced an invalid function call.
        MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
        /// The response was rejected by the model.
        ResponseRejected => "RESPONSE_REJECTED",
        /// The model needs more input before it can respond.
        NeedMoreInput => "NEED_MORE_INPUT",
        /// The input contained prohibited content.
        ProhibitedInputContent => "PROHIBITED_INPUT_CONTENT",
        /// The input image contained prohibited content.
        ImageProhibitedInputContent => "IMAGE_PROHIBITED_INPUT_CONTENT",
        /// The input text named a prominent person whose depiction is prohibited.
        InputTextContainProminentPersonProhibited => "INPUT_TEXT_CONTAIN_PROMINENT_PERSON_PROHIBITED",
        /// The input image showed a celebrity.
        InputImageCelebrity => "INPUT_IMAGE_CELEBRITY",
        /// The input image showed a photorealistic child, which is prohibited.
        InputImagePhotoRealisticChildProhibited => "INPUT_IMAGE_PHOTO_REALISTIC_CHILD_PROHIBITED",
        /// The input text contained non-consensual intimate imagery (NCII) content.
        InputTextNciiProhibited => "INPUT_TEXT_NCII_PROHIBITED",
        /// The input was blocked for another reason.
        InputOther => "INPUT_OTHER",
        /// The input infringed intellectual property.
        InputIpProhibited => "INPUT_IP_PROHIBITED",
        /// The input contained blocklisted terms.
        Blocklist => "BLOCKLIST",
        /// The prompt was unsafe for image generation.
        UnsafePromptForImageGeneration => "UNSAFE_PROMPT_FOR_IMAGE_GENERATION",
        /// The generated image was flagged for safety reasons.
        GeneratedImageSafety => "GENERATED_IMAGE_SAFETY",
        /// The generated content was flagged for safety reasons.
        GeneratedContentSafety => "GENERATED_CONTENT_SAFETY",
        /// The generated audio was flagged for safety reasons.
        GeneratedAudioSafety => "GENERATED_AUDIO_SAFETY",
        /// The generated video was flagged for safety reasons.
        GeneratedVideoSafety => "GENERATED_VIDEO_SAFETY",
        /// The generated content was prohibited.
        GeneratedContentProhibited => "GENERATED_CONTENT_PROHIBITED",
        /// The generated content contained blocklisted terms.
        GeneratedContentBlocklist => "GENERATED_CONTENT_BLOCKLIST",
        /// The generated image was prohibited.
        GeneratedImageProhibited => "GENERATED_IMAGE_PROHIBITED",
        /// The generated image showed a celebrity.
        GeneratedImageCelebrity => "GENERATED_IMAGE_CELEBRITY",
        /// The prompt rewriter detected prominent people in the generated image.
        GeneratedImageProminentPeopleDetectedByRewriter => "GENERATED_IMAGE_PROMINENT_PEOPLE_DETECTED_BY_REWRITER",
        /// The generated image showed identifiable people.
        GeneratedImageIdentifiablePeople => "GENERATED_IMAGE_IDENTIFIABLE_PEOPLE",
        /// The generated image showed minors.
        GeneratedImageMinors => "GENERATED_IMAGE_MINORS",
        /// The generated image infringed intellectual property.
        OutputImageIpProhibited => "OUTPUT_IMAGE_IP_PROHIBITED",
        /// The output was blocked for another reason.
        GeneratedOther => "GENERATED_OTHER",
        /// The maximum number of regeneration attempts was reached.
        MaxRegenerationReached => "MAX_REGENERATION_REACHED",
    }
);

crate::string_enum!(
    /// The type of the VAD signal.
    VADSignalType {
        /// Not specified.
        Unspecified => "VAD_SIGNAL_TYPE_UNSPECIFIED",
        /// Start of speech detected.
        Sos => "VAD_SIGNAL_TYPE_SOS",
        /// End of speech detected.
        Eos => "VAD_SIGNAL_TYPE_EOS",
    }
);

crate::string_enum!(
    /// The type of the voice activity signal.
    VoiceActivityType {
        /// Not specified.
        Unspecified => "TYPE_UNSPECIFIED",
        /// Voice activity started.
        ActivityStart => "ACTIVITY_START",
        /// Voice activity ended.
        ActivityEnd => "ACTIVITY_END",
    }
);

crate::string_enum!(
    /// Start of speech sensitivity.
    StartSensitivity {
        /// Not specified; the default is `High`.
        Unspecified => "START_SENSITIVITY_UNSPECIFIED",
        /// Detect the start of speech more readily.
        High => "START_SENSITIVITY_HIGH",
        /// Detect the start of speech less readily.
        Low => "START_SENSITIVITY_LOW",
    }
);

crate::string_enum!(
    /// End of speech sensitivity.
    EndSensitivity {
        /// Not specified; the default is `High`.
        Unspecified => "END_SENSITIVITY_UNSPECIFIED",
        /// Detect the end of speech more readily.
        High => "END_SENSITIVITY_HIGH",
        /// Detect the end of speech less readily.
        Low => "END_SENSITIVITY_LOW",
    }
);

crate::string_enum!(
    /// The different ways of handling user activity.
    ActivityHandling {
        /// Not specified; the default is `StartOfActivityInterrupts`.
        Unspecified => "ACTIVITY_HANDLING_UNSPECIFIED",
        /// The start of user activity interrupts the model's response (barge-in).
        StartOfActivityInterrupts => "START_OF_ACTIVITY_INTERRUPTS",
        /// The model's response is not interrupted by user activity.
        NoInterruption => "NO_INTERRUPTION",
    }
);

crate::string_enum!(
    /// Options about which input is included in the user's turn.
    TurnCoverage {
        /// Not specified; the default is `TurnIncludesOnlyActivity`.
        Unspecified => "TURN_COVERAGE_UNSPECIFIED",
        /// A user turn includes only input since the last activity, skipping inactivity.
        TurnIncludesOnlyActivity => "TURN_INCLUDES_ONLY_ACTIVITY",
        /// A user turn includes all input since the last turn, including inactivity.
        TurnIncludesAllInput => "TURN_INCLUDES_ALL_INPUT",
        /// A user turn includes audio during activity and all video since the last turn.
        TurnIncludesAudioActivityAndAllVideo => "TURN_INCLUDES_AUDIO_ACTIVITY_AND_ALL_VIDEO",
    }
);

/// Config for stream translation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationConfig {
    /// Whether to also return the input speech in the target language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_target_language: Option<bool>,
    /// BCP-47 code of the language to translate into.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub target_language_code: String,
}

/// Sent in response to a `LiveGenerateContentSetup` message from the client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerSetupComplete {
    /// ID of the session.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session_id: String,
    /// Voice consent signature to reuse in later sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_consent_signature: Option<VoiceConsentSignature>,
}

/// Incremental server update generated by the model in response to client messages. Content is
/// generated as quickly as possible, and not in real time. Clients may choose to buffer and play it
/// out in real time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerContent {
    /// Content the model generated in this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_turn: Option<Content>,
    /// Whether the model finished its turn; generation starts only after new input.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turn_complete: bool,
    /// Whether client input interrupted the model's generation.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub interrupted: bool,
    /// Sources the content is grounded on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<GroundingMetadata>,
    /// Whether the model finished generating (it may still be streaming the rest of the turn).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation_complete: bool,
    /// Transcription of the input audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_transcription: Option<Transcription>,
    /// Transcription of the output audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_transcription: Option<Transcription>,
    /// Metadata about URLs retrieved by the URL context tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_context_metadata: Option<URLContextMetadata>,
    /// Why the turn completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_complete_reason: Option<TurnCompleteReason>,
    /// Whether the model is waiting for input.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waiting_for_input: bool,
    /// Interim (not yet final) transcription of the input audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interim_input_transcription: Option<Transcription>,
    /// Activity state of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_status: Option<InteractionStatus>,
}

/// Request for the client to execute the `function_calls` and return the responses with the
/// matching `id`s.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerToolCall {
    /// Function calls to run; reply with a tool response carrying the matching IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_calls: Vec<FunctionCall>,
}

/// Notification for the client that a previously issued `ToolCallMessage` with the specified `id`s
/// should have been not executed and should be cancelled. If there were side-effects to those tool
/// calls, clients may attempt to undo the tool calls. This message occurs only in cases where the
/// clients interrupt server turns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerToolCallCancellation {
    /// IDs of the function calls to cancel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// Usage metadata about response(s).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    /// Number of tokens in the prompt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_token_count: i32,
    /// Number of tokens in the cached part of the prompt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_content_token_count: i32,
    /// Number of tokens in the response.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_token_count: i32,
    /// Number of tokens in tool-use prompts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_use_prompt_token_count: i32,
    /// Number of thinking tokens.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thoughts_token_count: i32,
    /// Total number of tokens of the request and response.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_token_count: i32,
    /// Prompt tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_tokens_details: Vec<ModalityTokenCount>,
    /// Cached tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_tokens_details: Vec<ModalityTokenCount>,
    /// Response tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_tokens_details: Vec<ModalityTokenCount>,
    /// Tool-use prompt tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_use_prompt_tokens_details: Vec<ModalityTokenCount>,
    /// Billing traffic type (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_type: Option<TrafficType>,
    /// Service tier the request was served from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

/// Server will not be able to service client soon.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerGoAway {
    /// Time left before the connection is closed; zero is omitted.
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub time_left: TimeDelta,
}

/// Update of the session resumption state. Only sent if `session_resumption` was set in the
/// connection config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerSessionResumptionUpdate {
    /// Handle to resume the session from this point; empty when not resumable.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub new_handle: String,
    /// Whether the session can be resumed from this point.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resumable: bool,
    /// Index of the last client message included in the state saved by `new_handle`.
    #[serde(default, skip_serializing_if = "is_zero", with = "i64_string")]
    pub last_consumed_client_message_index: i64,
}

/// A voice-activity signal sent by the server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActivityDetectionSignal {
    /// The kind of voice-activity signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vad_signal_type: Option<VADSignalType>,
}

/// Voice activity signal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActivity {
    /// Whether voice activity started or ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_activity_type: Option<VoiceActivityType>,
    /// Offset of the activity in the input audio.
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub audio_offset: TimeDelta,
}

/// Response message for API call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerMessage {
    /// Sent once, in response to the setup message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_complete: Option<LiveServerSetupComplete>,
    /// Content generated by the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_content: Option<LiveServerContent>,
    /// A request to run function calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<LiveServerToolCall>,
    /// A notice that earlier function calls are cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_cancellation: Option<LiveServerToolCallCancellation>,
    /// Token usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,
    /// A notice that the server will close the connection soon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go_away: Option<LiveServerGoAway>,
    /// A new session resumption handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resumption_update: Option<LiveServerSessionResumptionUpdate>,
    /// A voice-activity detection signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_activity_detection_signal: Option<VoiceActivityDetectionSignal>,
    /// A voice-activity event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_activity: Option<VoiceActivity>,
}

/// Configures automatic detection of activity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticActivityDetection {
    /// Whether automatic activity detection is off; the client then sends activity signals.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disabled: bool,
    /// How readily the start of speech is detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_of_speech_sensitivity: Option<StartSensitivity>,
    /// How readily the end of speech is detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_of_speech_sensitivity: Option<EndSensitivity>,
    /// Milliseconds of speech required before the start of speech is committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<i32>,
    /// Milliseconds of silence required before the end of speech is committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<i32>,
}

/// Marks the end of user activity. This can only be sent if automatic (i.e. server-side) activity
/// detection is disabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputConfig {
    /// Server-side detection of user activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_activity_detection: Option<AutomaticActivityDetection>,
    /// What user activity does to an in-progress model response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_handling: Option<ActivityHandling>,
    /// Which input is included in a user turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_coverage: Option<TurnCoverage>,
    /// Whether interim transcripts carry timestamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interim_transcript_timestamp_enabled: Option<bool>,
}

/// Configuration of session resumption mechanism. Included in
/// `LiveConnectConfig.session_resumption`. If included server will send
/// `LiveServerSessionResumptionUpdate` messages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionConfig {
    /// Handle of the session to resume; empty starts a new session.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub handle: String,
    /// Whether the server sends the index of the last consumed client message, for transparent
    /// reconnects.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub transparent: bool,
}

/// Context window will be truncated by keeping only suffix of it. Context window will always be cut
/// at start of USER role turn. System instructions and `BidiGenerateContentSetup.prefix_turns` will
/// not be subject to the sliding window mechanism, they will always stay at the beginning of
/// context window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlidingWindow {
    /// Number of tokens to keep after compression.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "i64_string_opt"
    )]
    pub target_tokens: Option<i64>,
}

/// Enables context window compression
/// -- mechanism managing model context window so it does not exceed given length.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWindowCompressionConfig {
    /// Number of tokens (before a turn) that triggers compression.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "i64_string_opt"
    )]
    pub trigger_tokens: Option<i64>,
    /// Compress with a sliding window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sliding_window: Option<SlidingWindow>,
}

/// Config for proactivity features.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProactivityConfig {
    /// Whether the model may stay silent when input is not relevant (proactive audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive_audio: Option<bool>,
}

/// Configures the customized avatar to be used in the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomizedAvatar {
    /// IANA MIME type of `image_data`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_mime_type: String,
    /// The avatar image; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub image_data: Bytes,
}

/// Configures the avatar to be used in the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarConfig {
    /// Name of a predefined avatar.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub avatar_name: String,
    /// A custom avatar image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customized_avatar: Option<CustomizedAvatar>,
    /// Bitrate of the avatar audio stream, in bits per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_bps: Option<i32>,
    /// Bitrate of the avatar video stream, in bits per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_bitrate_bps: Option<i32>,
}

/// Marks the start of user activity. This can only be sent if automatic (i.e. server-side) activity
/// detection is disabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStart {}

/// Marks the end of user activity. This can only be sent if automatic (i.e. server-side) activity
/// detection is disabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEnd {}

/// Session config for the API connection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveConnectConfig {
    /// Modalities the model responds with, such as `AUDIO` or `TEXT`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_modalities: Vec<Modality>,
    /// Randomness of token selection; lower is more deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling probability threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top-k sampling: tokens are drawn from the `k` most likely ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<f32>,
    /// Maximum number of tokens per response; `0` is omitted (the model default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_output_tokens: i32,
    /// Resolution at which input media is tokenised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
    /// Random seed, for more reproducible output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i32>,
    /// Speech generation settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<SpeechConfig>,
    /// Thinking settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    /// Whether the model adapts its response style to the tone of the input (affective dialog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_affective_dialog: Option<bool>,
    /// Instructions that steer the model's behaviour for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    /// Tools the model may use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// Session resumption settings; when set, the server sends resumption updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resumption: Option<SessionResumptionConfig>,
    /// Transcription of the input audio; set to turn it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio_transcription: Option<AudioTranscriptionConfig>,
    /// Transcription of the output audio; set to turn it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio_transcription: Option<AudioTranscriptionConfig>,
    /// How realtime input is handled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime_input_config: Option<RealtimeInputConfig>,
    /// Context window compression, for sessions longer than the context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_compression: Option<ContextWindowCompressionConfig>,
    /// Proactivity settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactivity: Option<ProactivityConfig>,
    /// Whether the server sends voice-activity detection signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_vad_signal: Option<bool>,
    /// Avatar settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_config: Option<AvatarConfig>,
    /// Per-category safety thresholds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_settings: Vec<SafetySetting>,
    /// Stream translation settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_config: Option<TranslationConfig>,
    /// Optional. Labels with user-defined metadata for the session.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A wrapper around the list of tools: an unset [`LiveClientContextUpdate::tools`] keeps the
/// previous tools, a set one (even with an empty list) replaces them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveClientContextUpdateTools {
    /// Optional. The list of tools the model may use to generate the next response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

/// Updates to the context of the current session: only fields that are set are updated, in order
/// with the rest of the inputs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveClientContextUpdate {
    /// Optional. Updated system instruction; overrides the setup's system instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    /// Optional. An updated list of tools; replaces the previously provided tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<LiveClientContextUpdateTools>,
}

/// Parameters for sending client content to the live API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSendClientContentParameters {
    /// Conversation turns to append to the session context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<Content>,
    /// Whether the client's turn is complete and the model should respond.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
}

/// Parameters for sending realtime input to the live API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSendRealtimeInputParameters {
    /// Deprecated: realtime media; use `audio`, `video` or `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Blob>,
    /// Realtime audio input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Blob>,
    /// Signals that the audio stream ended (for example, the microphone was turned off).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub audio_stream_end: bool,
    /// Realtime video input (frames).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<Blob>,
    /// Realtime text input.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    /// Marks the start of user activity (when automatic detection is off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    /// Marks the end of user activity (when automatic detection is off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
}

/// Parameters for sending tool responses to the live API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSendToolResponseParameters {
    /// Responses to the server's function calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_responses: Vec<FunctionResponse>,
}

/// Alias of [`LiveSendClientContentParameters`].
pub type LiveClientContentInput = LiveSendClientContentParameters;

/// Alias of [`LiveSendRealtimeInputParameters`].
pub type LiveRealtimeInput = LiveSendRealtimeInputParameters;

/// Alias of [`LiveSendToolResponseParameters`].
pub type LiveToolResponseInput = LiveSendToolResponseParameters;
