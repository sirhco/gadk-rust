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
        /// Not specified; the model's default level applies.
        Unspecified => "THINKING_LEVEL_UNSPECIFIED",
        /// As little thinking as possible.
        Minimal => "MINIMAL",
        /// Little thinking: lower latency and cost.
        Low => "LOW",
        /// A moderate amount of thinking.
        Medium => "MEDIUM",
        /// Deep thinking: higher quality on hard problems, at higher latency and cost.
        High => "HIGH",
    }
);

crate::string_enum!(
    /// Controls whether prominent people (celebrities) generation is allowed. If used with
    /// personGeneration, personGeneration enum would take precedence. For instance, if ALLOW_NONE
    /// is set, all person generation would be blocked. If this field is unspecified, the default
    /// behavior is to allow prominent people. This enum is not supported in Gemini API.
    ProminentPeople {
        /// Not specified.
        Unspecified => "PROMINENT_PEOPLE_UNSPECIFIED",
        /// Allow generating images of prominent people.
        AllowProminentPeople => "ALLOW_PROMINENT_PEOPLE",
        /// Block generating images of prominent people.
        BlockProminentPeople => "BLOCK_PROMINENT_PEOPLE",
    }
);

crate::string_enum!(
    /// The harm category to be blocked.
    HarmCategory {
        /// Not specified.
        Unspecified => "HARM_CATEGORY_UNSPECIFIED",
        /// Harassment.
        Harassment => "HARM_CATEGORY_HARASSMENT",
        /// Hate speech.
        HateSpeech => "HARM_CATEGORY_HATE_SPEECH",
        /// Sexually explicit content.
        SexuallyExplicit => "HARM_CATEGORY_SEXUALLY_EXPLICIT",
        /// Dangerous content.
        DangerousContent => "HARM_CATEGORY_DANGEROUS_CONTENT",
        /// Content that may harm civic integrity.
        CivicIntegrity => "HARM_CATEGORY_CIVIC_INTEGRITY",
        /// Jailbreak attempts.
        Jailbreak => "HARM_CATEGORY_JAILBREAK",
        /// Hateful imagery.
        ImageHate => "HARM_CATEGORY_IMAGE_HATE",
        /// Dangerous imagery.
        ImageDangerousContent => "HARM_CATEGORY_IMAGE_DANGEROUS_CONTENT",
        /// Harassing imagery.
        ImageHarassment => "HARM_CATEGORY_IMAGE_HARASSMENT",
        /// Sexually explicit imagery.
        ImageSexuallyExplicit => "HARM_CATEGORY_IMAGE_SEXUALLY_EXPLICIT",
    }
);

crate::string_enum!(
    /// The method for blocking content. If not specified, the default behavior is to use the
    /// probability score. This enum is not supported in Gemini API.
    HarmBlockMethod {
        /// Not specified.
        Unspecified => "HARM_BLOCK_METHOD_UNSPECIFIED",
        /// Block based on both the probability and the severity of harm.
        Severity => "SEVERITY",
        /// Block based on the probability of harm only.
        Probability => "PROBABILITY",
    }
);

crate::string_enum!(
    /// The threshold for blocking content. If the harm probability exceeds this threshold, the
    /// content will be blocked.
    HarmBlockThreshold {
        /// Not specified.
        Unspecified => "HARM_BLOCK_THRESHOLD_UNSPECIFIED",
        /// Block content with a low or higher probability of harm.
        BlockLowAndAbove => "BLOCK_LOW_AND_ABOVE",
        /// Block content with a medium or higher probability of harm.
        BlockMediumAndAbove => "BLOCK_MEDIUM_AND_ABOVE",
        /// Block only content with a high probability of harm.
        BlockOnlyHigh => "BLOCK_ONLY_HIGH",
        /// Block nothing.
        BlockNone => "BLOCK_NONE",
        /// Turn the safety filter off.
        Off => "OFF",
    }
);

crate::string_enum!(
    /// Transcription mode.
    AudioTranscriptionConfigMode {
        /// Not specified.
        Unspecified => "MODE_UNSPECIFIED",
        /// Transcribe exactly what was said.
        Verbatim => "VERBATIM",
        /// Transcribe with light clean-up, such as removing disfluencies.
        Smart => "SMART",
    }
);

crate::string_enum!(
    /// The media resolution to use.
    MediaResolution {
        /// Not specified.
        Unspecified => "MEDIA_RESOLUTION_UNSPECIFIED",
        /// Low resolution: fewer tokens per image or frame.
        Low => "MEDIA_RESOLUTION_LOW",
        /// Medium resolution.
        Medium => "MEDIA_RESOLUTION_MEDIUM",
        /// High resolution: more tokens per image or frame.
        High => "MEDIA_RESOLUTION_HIGH",
    }
);

crate::string_enum!(
    /// Server content modalities.
    Modality {
        /// Not specified.
        Unspecified => "MODALITY_UNSPECIFIED",
        /// Text.
        Text => "TEXT",
        /// Images.
        Image => "IMAGE",
        /// Audio.
        Audio => "AUDIO",
        /// Video.
        Video => "VIDEO",
    }
);

crate::string_enum!(
    /// Pricing and performance service tier.
    ServiceTier {
        /// Not specified; the default tier applies.
        Unspecified => "unspecified",
        /// Flex tier: lower cost, best-effort latency.
        Flex => "flex",
        /// Standard tier.
        Standard => "standard",
        /// Priority tier: prioritized capacity, at higher cost.
        Priority => "priority",
    }
);

crate::string_enum!(
    /// Resource scope.
    ResourceScope {
        /// A collection.
        Collection => "COLLECTION",
    }
);

crate::string_enum!(
    /// Options for feature selection preference.
    FeatureSelectionPreference {
        /// Not specified.
        Unspecified => "FEATURE_SELECTION_PREFERENCE_UNSPECIFIED",
        /// Prefer the higher-quality model.
        PrioritizeQuality => "PRIORITIZE_QUALITY",
        /// Balance quality and cost.
        Balanced => "BALANCED",
        /// Prefer the lower-cost model.
        PrioritizeCost => "PRIORITIZE_COST",
    }
);

crate::string_enum!(
    /// The different activity states of the live session.
    InteractionStatus {
        /// Not specified.
        Unspecified => "INTERACTION_STATUS_UNSPECIFIED",
        /// The session is processing.
        InProgress => "IN_PROGRESS",
        /// The session is waiting for client action, such as a function response.
        RequiresAction => "REQUIRES_ACTION",
        /// The session is idle.
        Idle => "IDLE",
    }
);

/// Config for model selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectionConfig {
    /// Whether automatic model selection favours quality, cost, or a balance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_selection_preference: Option<FeatureSelectionPreference>,
}

/// The signature of the voice consent check.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConsentSignature {
    /// The signature returned by the voice consent check.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub signature: String,
}

/// The configuration for the replicated voice to use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicatedVoiceConfig {
    /// IANA MIME type of the audio samples.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    /// Audio sample of the voice to replicate; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub voice_sample_audio: Bytes,
    /// Audio recording of the speaker's consent; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub consent_audio: Bytes,
    /// Signature of a previously passed consent check, used instead of `consent_audio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_consent_signature: Option<VoiceConsentSignature>,
}

/// Configuration for a prebuilt voice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrebuiltVoiceConfig {
    /// Name of the prebuilt voice, such as `"Kore"` or `"Puck"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub voice_name: String,
}

/// The configuration for the voice to use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfig {
    /// Use a voice replicated from an audio sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicated_voice_config: Option<ReplicatedVoiceConfig>,
    /// Use a prebuilt voice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prebuilt_voice_config: Option<PrebuiltVoiceConfig>,
    /// Name of the voice to use.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub voice: String,
}

/// Configuration for a single speaker in a multi-speaker setup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerVoiceConfig {
    /// Name of the speaker, as it appears in the prompt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub speaker: String,
    /// Voice for this speaker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_config: Option<VoiceConfig>,
}

/// Configuration for a multi-speaker text-to-speech request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiSpeakerVoiceConfig {
    /// Voice of each speaker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speaker_voice_configs: Vec<SpeakerVoiceConfig>,
}

/// Config for speech generation and transcription.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechConfig {
    /// Voice for single-speaker output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_config: Option<VoiceConfig>,
    /// BCP-47 language code of the speech, such as `"en-US"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub language_code: String,
    /// Voices for multi-speaker output; mutually exclusive with `voice_config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_speaker_voice_config: Option<MultiSpeakerVoiceConfig>,
}

/// The thinking features configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    /// Whether the response includes the model's thoughts (as parts with `thought` set).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub include_thoughts: bool,
    /// Maximum number of thinking tokens; `0` turns thinking off and `-1` lets the model decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    /// Relative amount of thinking; an alternative to `thinking_budget` for newer models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

/// The image output format for generated images. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageConfigImageOutputOptions {
    /// JPEG compression quality (0–100), when `mime_type` is `"image/jpeg"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_quality: Option<i32>,
    /// MIME type of the generated images, such as `"image/png"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
}

/// The image generation configuration to be used in GenerateContentConfig.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageConfig {
    /// Aspect ratio of generated images, such as `"1:1"` or `"16:9"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub aspect_ratio: String,
    /// Size of generated images, such as `"1K"`, `"2K"` or `"4K"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_size: String,
    /// Whether people may be generated, such as `"ALLOW_ADULT"` or `"DONT_ALLOW"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub person_generation: String,
    /// Whether images of prominent people may be generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prominent_people: Option<ProminentPeople>,
    /// MIME type of generated images.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_mime_type: String,
    /// JPEG compression quality (0–100) of generated images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_compression_quality: Option<i32>,
    /// Output format of generated images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output_options: Option<ImageConfigImageOutputOptions>,
}

/// The configuration for automated routing. When automated routing is specified, the routing will
/// be determined by the pretrained routing model and customer provided model routing preference.
/// This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigRoutingConfigAutoRoutingMode {
    /// Routing preference, such as `"PRIORITIZE_QUALITY"`, `"BALANCED"` or `"PRIORITIZE_COST"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_routing_preference: String,
}

/// The configuration for manual routing. When manual routing is specified, the model will be
/// selected based on the model name provided. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigRoutingConfigManualRoutingMode {
    /// Name of the model to route the request to.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_name: String,
}

/// The configuration for routing the request to a specific model. This can be used to control which
/// model is used for the generation, either automatically or by specifying a model name. This data
/// type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigRoutingConfig {
    /// Route automatically according to a preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_mode: Option<GenerationConfigRoutingConfigAutoRoutingMode>,
    /// Route to a named model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_mode: Option<GenerationConfigRoutingConfigManualRoutingMode>,
}

/// A safety setting that affects the safety-blocking behavior. A SafetySetting consists of a harm
/// category and a threshold for that category.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySetting {
    /// The harm category the setting applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<HarmCategory>,
    /// Whether the threshold applies to probability or severity scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<HarmBlockMethod>,
    /// The threshold at which content in the category is blocked.
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
    /// Name of the Model Armor template applied to prompts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_template_name: String,
    /// Name of the Model Armor template applied to responses.
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
    /// BCP-47 codes of the languages expected in the audio.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
}

/// The audio transcription configuration in Setup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {
    /// BCP-47 codes of the languages expected in the audio; omitted means auto-detect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
    /// Deprecated: auto-detection is the default when `language_codes` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_auto: Option<LanguageAuto>,
    /// Deprecated: use `language_codes` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_hints: Option<LanguageHints>,
    /// Words and phrases to recognise, such as names and domain terms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_vocabulary: Vec<String>,
    /// Phrases that bias recognition towards them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adaptation_phrases: Vec<String>,
    /// Whether to label speakers in the transcription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization: Option<bool>,
    /// Whether to include word-level timestamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_timestamp: Option<bool>,
    /// Whether to transcribe verbatim or with clean-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<AudioTranscriptionConfigMode>,
}

/// Optional model configuration parameters. For
/// more information, see `Content generation parameters
/// \<<https://cloud.google.com/vertex-ai/generative-ai/docs/multimodal/content-generation-parameters>\>`_.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentConfig {
    /// Instructions that steer the model's behaviour across the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    /// Randomness of token selection; lower is more deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling: tokens are drawn from the smallest set whose probability sums to this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top-k sampling: tokens are drawn from the `k` most likely ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<f32>,
    /// Number of response candidates to generate; `0` is omitted (one candidate).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub candidate_count: i32,
    /// Maximum number of tokens in a candidate; `0` is omitted (the model default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_output_tokens: i32,
    /// Strings that stop generation when produced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    /// Whether to return the log probabilities of the chosen tokens.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_logprobs: bool,
    /// Number of top candidate tokens to return log probabilities for at each step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<i32>,
    /// Penalises tokens already present in the output, encouraging new topics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    /// Penalises tokens in proportion to how often they appear in the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    /// Random seed, for more reproducible output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i32>,
    /// MIME type of the response, such as `"text/plain"` or `"application/json"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_mime_type: String,
    /// Schema the response must follow (with a JSON `response_mime_type`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Schema>,
    /// Schema the response must follow, as raw JSON Schema; alternative to `response_schema`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<Value>,
    /// Model routing (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_config: Option<GenerationConfigRoutingConfig>,
    /// Automatic model selection preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection_config: Option<ModelSelectionConfig>,
    /// Per-category safety thresholds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_settings: Vec<SafetySetting>,
    /// Tools the model may use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// How the model may use `tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    /// User-defined labels attached to the request (Vertex AI only), e.g. for billing breakdown.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Resource name of cached content to use as context, `cachedContents/{id}`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_content: String,
    /// Modalities the response may contain, such as `"TEXT"`, `"IMAGE"` or `"AUDIO"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_modalities: Vec<String>,
    /// Resolution at which input media is tokenised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
    /// Speech generation settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<SpeechConfig>,
    /// Whether the model should understand timestamps in audio input.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub audio_timestamp: bool,
    /// Thinking settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    /// Image generation settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_config: Option<ImageConfig>,
    /// Whether to enable enhanced answers to civic questions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_enhanced_civic_answers: Option<bool>,
    /// Model Armor filtering of prompts and responses (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_armor_config: Option<ModelArmorConfig>,
    /// Service tier to serve the request from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Settings for transcribing audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_transcription_config: Option<AudioTranscriptionConfig>,
    /// Token from an earlier response for continuing its generation; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "base64_bytes_opt"
    )]
    pub continuation_token: Option<Bytes>,
    /// Whether the API continues generation automatically.
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
