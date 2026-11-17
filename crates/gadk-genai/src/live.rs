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
        Unspecified => "TURN_COMPLETE_REASON_UNSPECIFIED",
        MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
        ResponseRejected => "RESPONSE_REJECTED",
        NeedMoreInput => "NEED_MORE_INPUT",
        ProhibitedInputContent => "PROHIBITED_INPUT_CONTENT",
        ImageProhibitedInputContent => "IMAGE_PROHIBITED_INPUT_CONTENT",
        InputTextContainProminentPersonProhibited => "INPUT_TEXT_CONTAIN_PROMINENT_PERSON_PROHIBITED",
        InputImageCelebrity => "INPUT_IMAGE_CELEBRITY",
        InputImagePhotoRealisticChildProhibited => "INPUT_IMAGE_PHOTO_REALISTIC_CHILD_PROHIBITED",
        InputTextNciiProhibited => "INPUT_TEXT_NCII_PROHIBITED",
        InputOther => "INPUT_OTHER",
        InputIpProhibited => "INPUT_IP_PROHIBITED",
        Blocklist => "BLOCKLIST",
        UnsafePromptForImageGeneration => "UNSAFE_PROMPT_FOR_IMAGE_GENERATION",
        GeneratedImageSafety => "GENERATED_IMAGE_SAFETY",
        GeneratedContentSafety => "GENERATED_CONTENT_SAFETY",
        GeneratedAudioSafety => "GENERATED_AUDIO_SAFETY",
        GeneratedVideoSafety => "GENERATED_VIDEO_SAFETY",
        GeneratedContentProhibited => "GENERATED_CONTENT_PROHIBITED",
        GeneratedContentBlocklist => "GENERATED_CONTENT_BLOCKLIST",
        GeneratedImageProhibited => "GENERATED_IMAGE_PROHIBITED",
        GeneratedImageCelebrity => "GENERATED_IMAGE_CELEBRITY",
        GeneratedImageProminentPeopleDetectedByRewriter => "GENERATED_IMAGE_PROMINENT_PEOPLE_DETECTED_BY_REWRITER",
        GeneratedImageIdentifiablePeople => "GENERATED_IMAGE_IDENTIFIABLE_PEOPLE",
        GeneratedImageMinors => "GENERATED_IMAGE_MINORS",
        OutputImageIpProhibited => "OUTPUT_IMAGE_IP_PROHIBITED",
        GeneratedOther => "GENERATED_OTHER",
        MaxRegenerationReached => "MAX_REGENERATION_REACHED",
    }
);

crate::string_enum!(
    /// The type of the VAD signal.
    VADSignalType {
        Unspecified => "VAD_SIGNAL_TYPE_UNSPECIFIED",
        Sos => "VAD_SIGNAL_TYPE_SOS",
        Eos => "VAD_SIGNAL_TYPE_EOS",
    }
);

crate::string_enum!(
    /// The type of the voice activity signal.
    VoiceActivityType {
        Unspecified => "TYPE_UNSPECIFIED",
        ActivityStart => "ACTIVITY_START",
        ActivityEnd => "ACTIVITY_END",
    }
);

crate::string_enum!(
    /// Start of speech sensitivity.
    StartSensitivity {
        Unspecified => "START_SENSITIVITY_UNSPECIFIED",
        High => "START_SENSITIVITY_HIGH",
        Low => "START_SENSITIVITY_LOW",
    }
);

crate::string_enum!(
    /// End of speech sensitivity.
    EndSensitivity {
        Unspecified => "END_SENSITIVITY_UNSPECIFIED",
        High => "END_SENSITIVITY_HIGH",
        Low => "END_SENSITIVITY_LOW",
    }
);

crate::string_enum!(
    /// The different ways of handling user activity.
    ActivityHandling {
        Unspecified => "ACTIVITY_HANDLING_UNSPECIFIED",
        StartOfActivityInterrupts => "START_OF_ACTIVITY_INTERRUPTS",
        NoInterruption => "NO_INTERRUPTION",
    }
);

crate::string_enum!(
    /// Options about which input is included in the user's turn.
    TurnCoverage {
        Unspecified => "TURN_COVERAGE_UNSPECIFIED",
        TurnIncludesOnlyActivity => "TURN_INCLUDES_ONLY_ACTIVITY",
        TurnIncludesAllInput => "TURN_INCLUDES_ALL_INPUT",
        TurnIncludesAudioActivityAndAllVideo => "TURN_INCLUDES_AUDIO_ACTIVITY_AND_ALL_VIDEO",
    }
);

/// Config for stream translation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_target_language: Option<bool>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub target_language_code: String,
}

/// Sent in response to a `LiveGenerateContentSetup` message from the client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerSetupComplete {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_consent_signature: Option<VoiceConsentSignature>,
}

/// Incremental server update generated by the model in response to client messages. Content is
/// generated as quickly as possible, and not in real time. Clients may choose to buffer and play it
/// out in real time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_turn: Option<Content>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turn_complete: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub interrupted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<GroundingMetadata>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_transcription: Option<Transcription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_transcription: Option<Transcription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_context_metadata: Option<URLContextMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_complete_reason: Option<TurnCompleteReason>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waiting_for_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interim_input_transcription: Option<Transcription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_status: Option<InteractionStatus>,
}

/// Request for the client to execute the `function_calls` and return the responses with the
/// matching `id`s.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerToolCall {
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// Usage metadata about response(s).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_content_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_use_prompt_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thoughts_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_token_count: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_use_prompt_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_type: Option<TrafficType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

/// Server will not be able to service client soon.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveServerGoAway {
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
    #[serde(default, skip_serializing_if = "is_zero")]
    pub new_handle: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resumable: bool,
    #[serde(default, skip_serializing_if = "is_zero", with = "i64_string")]
    pub last_consumed_client_message_index: i64,
}

/// A voice-activity signal sent by the server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActivityDetectionSignal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vad_signal_type: Option<VADSignalType>,
}

/// Voice activity signal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActivity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_activity_type: Option<VoiceActivityType>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_complete: Option<LiveServerSetupComplete>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_content: Option<LiveServerContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<LiveServerToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_cancellation: Option<LiveServerToolCallCancellation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go_away: Option<LiveServerGoAway>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resumption_update: Option<LiveServerSessionResumptionUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_activity_detection_signal: Option<VoiceActivityDetectionSignal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_activity: Option<VoiceActivity>,
}

/// Configures automatic detection of activity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticActivityDetection {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_of_speech_sensitivity: Option<StartSensitivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_of_speech_sensitivity: Option<EndSensitivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<i32>,
}

/// Marks the end of user activity. This can only be sent if automatic (i.e. server-side) activity
/// detection is disabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeInputConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_activity_detection: Option<AutomaticActivityDetection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_handling: Option<ActivityHandling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_coverage: Option<TurnCoverage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interim_transcript_timestamp_enabled: Option<bool>,
}

/// Configuration of session resumption mechanism. Included in
/// `LiveConnectConfig.session_resumption`. If included server will send
/// `LiveServerSessionResumptionUpdate` messages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumptionConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub handle: String,
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "i64_string_opt"
    )]
    pub trigger_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sliding_window: Option<SlidingWindow>,
}

/// Config for proactivity features.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProactivityConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive_audio: Option<bool>,
}

/// Configures the customized avatar to be used in the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomizedAvatar {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_mime_type: String,
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
    #[serde(default, skip_serializing_if = "is_zero")]
    pub avatar_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customized_avatar: Option<CustomizedAvatar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_bps: Option<i32>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_modalities: Vec<Modality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<f32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_output_tokens: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<SpeechConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_affective_dialog: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resumption: Option<SessionResumptionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio_transcription: Option<AudioTranscriptionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio_transcription: Option<AudioTranscriptionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime_input_config: Option<RealtimeInputConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_compression: Option<ContextWindowCompressionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactivity: Option<ProactivityConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_vad_signal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_config: Option<AvatarConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_settings: Vec<SafetySetting>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
}

/// Parameters for sending realtime input to the live API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSendRealtimeInputParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Blob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Blob>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub audio_stream_end: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<Blob>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_start: Option<ActivityStart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_end: Option<ActivityEnd>,
}

/// Parameters for sending tool responses to the live API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSendToolResponseParameters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_responses: Vec<FunctionResponse>,
}

/// Alias of [`LiveSendClientContentParameters`].
pub type LiveClientContentInput = LiveSendClientContentParameters;

/// Alias of [`LiveSendRealtimeInputParameters`].
pub type LiveRealtimeInput = LiveSendRealtimeInputParameters;

/// Alias of [`LiveSendToolResponseParameters`].
pub type LiveToolResponseInput = LiveSendToolResponseParameters;
