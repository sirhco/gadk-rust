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

//! Generation config types.

use crate::content::Content;
use crate::schema::Schema;
use crate::serde_util::{base64_bytes, base64_bytes_opt, is_zero};
use crate::tool::{Tool, ToolConfig};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

crate::string_enum!(
    /// The number of thoughts tokens that the model should generate.
    ThinkingLevel {
        Unspecified => "THINKING_LEVEL_UNSPECIFIED",
        Minimal => "MINIMAL",
        Low => "LOW",
        Medium => "MEDIUM",
        High => "HIGH",
    }
);

crate::string_enum!(
    /// Controls whether prominent people (celebrities) generation is allowed. If used with
    /// personGeneration, personGeneration enum would take precedence. For instance, if ALLOW_NONE
    /// is set, all person generation would be blocked. If this field is unspecified, the default
    /// behavior is to allow prominent people. This enum is not supported in Gemini API.
    ProminentPeople {
        Unspecified => "PROMINENT_PEOPLE_UNSPECIFIED",
        AllowProminentPeople => "ALLOW_PROMINENT_PEOPLE",
        BlockProminentPeople => "BLOCK_PROMINENT_PEOPLE",
    }
);

crate::string_enum!(
    /// The harm category to be blocked.
    HarmCategory {
        Unspecified => "HARM_CATEGORY_UNSPECIFIED",
        Harassment => "HARM_CATEGORY_HARASSMENT",
        HateSpeech => "HARM_CATEGORY_HATE_SPEECH",
        SexuallyExplicit => "HARM_CATEGORY_SEXUALLY_EXPLICIT",
        DangerousContent => "HARM_CATEGORY_DANGEROUS_CONTENT",
        CivicIntegrity => "HARM_CATEGORY_CIVIC_INTEGRITY",
        Jailbreak => "HARM_CATEGORY_JAILBREAK",
        ImageHate => "HARM_CATEGORY_IMAGE_HATE",
        ImageDangerousContent => "HARM_CATEGORY_IMAGE_DANGEROUS_CONTENT",
        ImageHarassment => "HARM_CATEGORY_IMAGE_HARASSMENT",
        ImageSexuallyExplicit => "HARM_CATEGORY_IMAGE_SEXUALLY_EXPLICIT",
    }
);

crate::string_enum!(
    /// The method for blocking content. If not specified, the default behavior is to use the
    /// probability score. This enum is not supported in Gemini API.
    HarmBlockMethod {
        Unspecified => "HARM_BLOCK_METHOD_UNSPECIFIED",
        Severity => "SEVERITY",
        Probability => "PROBABILITY",
    }
);

crate::string_enum!(
    /// The threshold for blocking content. If the harm probability exceeds this threshold, the
    /// content will be blocked.
    HarmBlockThreshold {
        Unspecified => "HARM_BLOCK_THRESHOLD_UNSPECIFIED",
        BlockLowAndAbove => "BLOCK_LOW_AND_ABOVE",
        BlockMediumAndAbove => "BLOCK_MEDIUM_AND_ABOVE",
        BlockOnlyHigh => "BLOCK_ONLY_HIGH",
        BlockNone => "BLOCK_NONE",
        Off => "OFF",
    }
);

crate::string_enum!(
    /// Transcription mode.
    AudioTranscriptionConfigMode {
        Unspecified => "MODE_UNSPECIFIED",
        Verbatim => "VERBATIM",
        Smart => "SMART",
    }
);

crate::string_enum!(
    /// The media resolution to use.
    MediaResolution {
        Unspecified => "MEDIA_RESOLUTION_UNSPECIFIED",
        Low => "MEDIA_RESOLUTION_LOW",
        Medium => "MEDIA_RESOLUTION_MEDIUM",
        High => "MEDIA_RESOLUTION_HIGH",
    }
);

crate::string_enum!(
    /// Server content modalities.
    Modality {
        Unspecified => "MODALITY_UNSPECIFIED",
        Text => "TEXT",
        Image => "IMAGE",
        Audio => "AUDIO",
        Video => "VIDEO",
    }
);

crate::string_enum!(
    /// Pricing and performance service tier.
    ServiceTier {
        Unspecified => "unspecified",
        Flex => "flex",
        Standard => "standard",
        Priority => "priority",
    }
);

crate::string_enum!(
    /// Resource scope.
    ResourceScope {
        Collection => "COLLECTION",
    }
);

crate::string_enum!(
    /// Options for feature selection preference.
    FeatureSelectionPreference {
        Unspecified => "FEATURE_SELECTION_PREFERENCE_UNSPECIFIED",
        PrioritizeQuality => "PRIORITIZE_QUALITY",
        Balanced => "BALANCED",
        PrioritizeCost => "PRIORITIZE_COST",
    }
);

crate::string_enum!(
    /// The different activity states of the live session.
    InteractionStatus {
        Unspecified => "INTERACTION_STATUS_UNSPECIFIED",
        InProgress => "IN_PROGRESS",
        RequiresAction => "REQUIRES_ACTION",
        Idle => "IDLE",
    }
);

/// Config for model selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_selection_preference: Option<FeatureSelectionPreference>,
}

/// The signature of the voice consent check.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConsentSignature {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub signature: String,
}

/// The configuration for the replicated voice to use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicatedVoiceConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub voice_sample_audio: Bytes,
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub consent_audio: Bytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_consent_signature: Option<VoiceConsentSignature>,
}

/// Configuration for a prebuilt voice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrebuiltVoiceConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub voice_name: String,
}

/// The configuration for the voice to use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicated_voice_config: Option<ReplicatedVoiceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prebuilt_voice_config: Option<PrebuiltVoiceConfig>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub voice: String,
}

/// Configuration for a single speaker in a multi-speaker setup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerVoiceConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub speaker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_config: Option<VoiceConfig>,
}

/// Configuration for a multi-speaker text-to-speech request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiSpeakerVoiceConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speaker_voice_configs: Vec<SpeakerVoiceConfig>,
}

/// Config for speech generation and transcription.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_config: Option<VoiceConfig>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub language_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_speaker_voice_config: Option<MultiSpeakerVoiceConfig>,
}

/// The thinking features configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub include_thoughts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

/// The image output format for generated images. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageConfigImageOutputOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_quality: Option<i32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
}

/// The image generation configuration to be used in GenerateContentConfig.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub aspect_ratio: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_size: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub person_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prominent_people: Option<ProminentPeople>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_compression_quality: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output_options: Option<ImageConfigImageOutputOptions>,
}

/// The configuration for automated routing. When automated routing is specified, the routing will
/// be determined by the pretrained routing model and customer provided model routing preference.
/// This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigRoutingConfigAutoRoutingMode {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_routing_preference: String,
}

/// The configuration for manual routing. When manual routing is specified, the model will be
/// selected based on the model name provided. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigRoutingConfigManualRoutingMode {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_name: String,
}

/// The configuration for routing the request to a specific model. This can be used to control which
/// model is used for the generation, either automatically or by specifying a model name. This data
/// type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigRoutingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_mode: Option<GenerationConfigRoutingConfigAutoRoutingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_mode: Option<GenerationConfigRoutingConfigManualRoutingMode>,
}

/// A safety setting that affects the safety-blocking behavior. A SafetySetting consists of a harm
/// category and a threshold for that category.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<HarmCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<HarmBlockMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<HarmBlockThreshold>,
}

/// Configuration for Model Armor. Model Armor is a Google Cloud service that provides safety and
/// security filtering for prompts and responses. It helps protect your AI applications from risks
/// such as harmful content, sensitive data leakage, and prompt injection attacks. This data type is
/// not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelArmorConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_template_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_template_name: String,
}

/// Deprecated: Language auto-detection is now the default when language_codes is omitted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageAuto {}

/// Deprecated: Use AudioTranscriptionConfig.language_codes instead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageHints {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
}

/// The audio transcription configuration in Setup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_auto: Option<LanguageAuto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_hints: Option<LanguageHints>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_vocabulary: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adaptation_phrases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_timestamp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<AudioTranscriptionConfigMode>,
}

/// Optional model configuration parameters. For
/// more information, see `Content generation parameters
/// \<<https://cloud.google.com/vertex-ai/generative-ai/docs/multimodal/content-generation-parameters>\>`_.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<f32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub candidate_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_output_tokens: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_logprobs: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Schema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_config: Option<GenerationConfigRoutingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection_config: Option<ModelSelectionConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_settings: Vec<SafetySetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_modalities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<SpeechConfig>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub audio_timestamp: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_config: Option<ImageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_enhanced_civic_answers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_armor_config: Option<ModelArmorConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_transcription_config: Option<AudioTranscriptionConfig>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "base64_bytes_opt"
    )]
    pub continuation_token: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_continuation: Option<bool>,
    /// Optional. Configuration for persisting output assets to external storage (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_output_config: Option<GenerationOutputConfig>,
    /// Optional. Configures the logging behavior for a given request; takes precedence over
    /// the project-level logging config (Gemini API only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
}

/// Configuration for persisting output assets to external storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationOutputConfig {
    /// Optional. The Cloud Storage URI prefix or directory where generated media assets
    /// should be written (e.g. `"gs://my-bucket/prefix/"`). Supported URI schemes: `"gs://"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_uri_prefix: String,
}
