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

//! GenerateContent response types.

use crate::config::{HarmBlockThreshold, HarmCategory};
use crate::content::Content;
use crate::serde_util::{base64_bytes, base64_bytes_opt, go_duration, is_zero};
use bytes::Bytes;
use chrono::{DateTime, TimeDelta, Utc};
use serde::de::Error as _;
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

crate::string_enum!(
    /// The reason why the model stopped generating tokens. If empty,
    /// the model has not stopped generating the tokens.
    FinishReason {
        Unspecified => "FINISH_REASON_UNSPECIFIED",
        Stop => "STOP",
        MaxTokens => "MAX_TOKENS",
        Safety => "SAFETY",
        Recitation => "RECITATION",
        Language => "LANGUAGE",
        Other => "OTHER",
        Blocklist => "BLOCKLIST",
        ProhibitedContent => "PROHIBITED_CONTENT",
        Spii => "SPII",
        MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
        ImageSafety => "IMAGE_SAFETY",
        UnexpectedToolCall => "UNEXPECTED_TOOL_CALL",
        TooManyToolCalls => "TOO_MANY_TOOL_CALLS",
        ImageProhibitedContent => "IMAGE_PROHIBITED_CONTENT",
        NoImage => "NO_IMAGE",
        ImageRecitation => "IMAGE_RECITATION",
        ImageOther => "IMAGE_OTHER",
        Continuation => "CONTINUATION",
    }
);

crate::string_enum!(
    /// The probability of harm for this category.
    HarmProbability {
        Unspecified => "HARM_PROBABILITY_UNSPECIFIED",
        Negligible => "NEGLIGIBLE",
        Low => "LOW",
        Medium => "MEDIUM",
        High => "HIGH",
    }
);

crate::string_enum!(
    /// The severity of harm for this category. This enum is not
    /// supported in Gemini API.
    HarmSeverity {
        Unspecified => "HARM_SEVERITY_UNSPECIFIED",
        Negligible => "HARM_SEVERITY_NEGLIGIBLE",
        Low => "HARM_SEVERITY_LOW",
        Medium => "HARM_SEVERITY_MEDIUM",
        High => "HARM_SEVERITY_HIGH",
    }
);

crate::string_enum!(
    /// The status of the URL retrieval.
    URLRetrievalStatus {
        Unspecified => "URL_RETRIEVAL_STATUS_UNSPECIFIED",
        Success => "URL_RETRIEVAL_STATUS_SUCCESS",
        Error => "URL_RETRIEVAL_STATUS_ERROR",
        Paywall => "URL_RETRIEVAL_STATUS_PAYWALL",
        Unsafe => "URL_RETRIEVAL_STATUS_UNSAFE",
    }
);

crate::string_enum!(
    /// The reason why the prompt was blocked.
    BlockedReason {
        Unspecified => "BLOCKED_REASON_UNSPECIFIED",
        Safety => "SAFETY",
        Other => "OTHER",
        Blocklist => "BLOCKLIST",
        ProhibitedContent => "PROHIBITED_CONTENT",
        ImageSafety => "IMAGE_SAFETY",
        ModelArmor => "MODEL_ARMOR",
        Jailbreak => "JAILBREAK",
    }
);

crate::string_enum!(
    /// The traffic type for this request. This enum is not supported
    /// in Gemini API.
    TrafficType {
        Unspecified => "TRAFFIC_TYPE_UNSPECIFIED",
        OnDemand => "ON_DEMAND",
        OnDemandPriority => "ON_DEMAND_PRIORITY",
        OnDemandFlex => "ON_DEMAND_FLEX",
        OnDemandOffpeak => "ON_DEMAND_OFFPEAK",
        ProvisionedThroughput => "PROVISIONED_THROUGHPUT",
    }
);

crate::string_enum!(
    /// The modality that this token count applies to.
    MediaModality {
        Unspecified => "MODALITY_UNSPECIFIED",
        Text => "TEXT",
        Image => "IMAGE",
        Video => "VIDEO",
        Audio => "AUDIO",
        Document => "DOCUMENT",
    }
);

crate::string_enum!(
    /// The stage of the underlying model. This enum is not supported in
    /// Vertex AI.
    ModelStage {
        Unspecified => "MODEL_STAGE_UNSPECIFIED",
        UnstableExperimental => "UNSTABLE_EXPERIMENTAL",
        Experimental => "EXPERIMENTAL",
        Preview => "PREVIEW",
        Stable => "STABLE",
        Legacy => "LEGACY",
        Deprecated => "DEPRECATED",
        Retired => "RETIRED",
    }
);

/// A citation for a piece of generatedcontent. This data
/// type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_index: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub license: String,
    #[serde(default, skip_serializing_if = "CivilDate::is_zero")]
    pub publication_date: CivilDate,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_index: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
}

/// Citation information when the model quotes
/// another source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
}

/// Author
/// attribution for a photo or review.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub photo_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
}

/// Encapsulates a
/// review snippet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsPlaceAnswerSourcesReviewSnippet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_attribution: Option<GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flag_content_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub google_maps_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub relative_publish_time_description: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
}

/// The sources that were used to
/// generate the place answer. This includes review snippets and photos that were used to
/// generate the answer, as well as URIs to flag content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsPlaceAnswerSources {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_snippet: Vec<GroundingChunkMapsPlaceAnswerSourcesReviewSnippet>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flag_content_uri: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_snippets: Vec<GroundingChunkMapsPlaceAnswerSourcesReviewSnippet>,
}

/// Route information from Google Maps. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsRoute {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_meters: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub duration: TimeDelta,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub encoded_polyline: String,
}

/// A `Maps` chunk is a piece of evidence that
/// comes from Google Maps. It contains information about a place, such as its name, address,
/// and reviews. This is used to provide the user with rich, location-based information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_answer_sources: Option<GroundingChunkMapsPlaceAnswerSources>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub place_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GroundingChunkMapsRoute>,
}

/// An `Image` chunk is a piece of evidence that
/// comes from an image search result. It contains the URI of the image search result and the
/// URI of the image. This is used to provide the user with a link to the source of the
/// information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkImage {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub domain: String,
}

/// Represents where the chunk starts and ends in the
/// document. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGChunkPageSpan {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_page: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_page: i32,
}

/// A RAGChunk includes the content of a chunk of a RAGFile,
/// and associated metadata. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_span: Option<RAGChunkPageSpan>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chunk_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_id: String,
}

/// A list of string values. This data type
/// is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkStringList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

/// User provided metadata about the
/// GroundingFact. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkCustomMetadata {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string_list_value: Option<GroundingChunkStringList>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub string_value: String,
}

/// Context retrieved from a data
/// source to ground the model's response. This is used when a retrieval tool fetches
/// information from a user-provided corpus or a public dataset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkRetrievedContext {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub document_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rag_chunk: Option<RAGChunk>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_metadata: Vec<GroundingChunkCustomMetadata>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_search_store: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<i32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub media_id: String,
}

/// A `Web` chunk is a piece of evidence that comes
/// from a web page. It contains the URI of the web page, the title of the page, and the domain
/// of the page. This is used to provide the user with a link to the source of the information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkWeb {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub domain: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
}

/// A piece of evidence that supports a claim made by
/// the model. This is used to show a citation for a claim made by the model. When grounding is
/// enabled, the model returns a `GroundingChunk` that contains a reference to the source of the
/// information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<GroundingChunkImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maps: Option<GroundingChunkMaps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_context: Option<GroundingChunkRetrievedContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<GroundingChunkWeb>,
}

/// Segment of the content this support belongs to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_index: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_index: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub part_index: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
}

/// Grounding support.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingSupport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confidence_scores: Vec<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_chunk_indices: Vec<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<Segment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_parts: Vec<i32>,
}

/// Metadata returned to client when grounding is
/// enabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalMetadata {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub google_search_dynamic_retrieval_score: f32,
}

/// The entry point used to search for grounding
/// sources.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEntryPoint {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rendered_content: String,
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub sdk_blob: Bytes,
}

/// A URI that can be used to flag
/// a place or review for inappropriate content. This is populated only when the grounding
/// source is Google Maps. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingMetadataSourceFlaggingURI {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flag_content_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_id: String,
}

/// Information for various kinds of grounding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_search_queries: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_chunks: Vec<GroundingChunk>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_supports: Vec<GroundingSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_metadata: Option<RetrievalMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_entry_point: Option<SearchEntryPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_queries: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub google_maps_widget_context_token: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retrieval_queries: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_flagging_uris: Vec<GroundingMetadataSourceFlaggingURI>,
}

/// A single token and its associated log
/// probability.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogprobsResultCandidate {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub log_probability: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token_id: i32,
}

/// A list of the top candidate tokens and
/// their log probabilities at each decoding step. This can be used to see what other tokens the
/// model considered.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogprobsResultTopCandidates {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<LogprobsResultCandidate>,
}

/// The log probabilities of the tokens generated by
/// the model. This is useful for understanding the model's confidence in its predictions and
/// for debugging. For example, you can use log probabilities to identify when the model is
/// making a less confident prediction or to explore alternative responses that the model
/// considered. A low log probability can also indicate that the model is "hallucinating" or
/// generating factually incorrect information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogprobsResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_candidates: Vec<LogprobsResultCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_candidates: Vec<LogprobsResultTopCandidates>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_probability_sum: Option<f32>,
}

/// A safety rating for a piece of content. The safety
/// rating contains the harm category and the harm probability level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyRating {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<HarmCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwritten_threshold: Option<HarmBlockThreshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<HarmProbability>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub probability_score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<HarmSeverity>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub severity_score: f32,
}

/// The metadata for a single URL retrieval.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct URLMetadata {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retrieved_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_retrieval_status: Option<URLRetrievalStatus>,
}

/// Metadata returned when the model uses the
/// `url_context` tool to get information from a user-provided URL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct URLContextMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub url_metadata: Vec<URLMetadata>,
}

/// A response candidate generated from the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_metadata: Option<CitationMetadata>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub finish_message: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token_count: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<GroundingMetadata>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub avg_logprobs: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub index: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs_result: Option<LogprobsResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_ratings: Vec<SafetyRating>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_context_metadata: Option<URLContextMetadata>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "base64_bytes_opt"
    )]
    pub continuation_token: Option<Bytes>,
}

/// Content filter results for a
/// prompt sent in the request. Note: This is sent only in the first stream chunk and only if no
/// candidates were generated due to content violations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponsePromptFeedback {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<BlockedReason>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub block_reason_message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_ratings: Vec<SafetyRating>,
}

/// Represents a breakdown of token usage by
/// modality. This message is used in CountTokensResponse and
/// GenerateContentResponse.UsageMetadata to provide a detailed view of how many tokens are used
/// by each modality (e.g., text, image, video) in a request. This is particularly useful for
/// multimodal models, allowing you to track and manage token consumption for billing and quota
/// purposes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModalityTokenCount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<MediaModality>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token_count: i32,
}

/// Usage metadata about the
/// content generation request and response. This message provides a detailed breakdown of token
/// usage and other relevant metrics. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponseUsageMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_content_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub candidates_token_count: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_token_count: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thoughts_token_count: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_use_prompt_token_count: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_use_prompt_tokens_details: Vec<ModalityTokenCount>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_token_count: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_type: Option<TrafficType>,
}

/// The status of the underlying model. This is used to
/// indicate the stage of the underlying model and the retirement time if applicable. This data
/// type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_stage: Option<ModelStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_time: Option<DateTime<Utc>>,
}

/// Response message for
/// PredictionService.GenerateContent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_feedback: Option<GenerateContentResponsePromptFeedback>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GenerateContentResponseUsageMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_status: Option<ModelStatus>,
}

/// A calendar date encoded as a `{"year": .., "month": .., "day": ..}` object with zero
/// components omitted. Decoding a non-empty object without `year` is an error; `{}` and `null`
/// decode to the zero date, which fields omit on encode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CivilDate {
    pub year: i32,
    /// 1-12; 0 when unspecified.
    pub month: i32,
    /// 1-31; 0 when unspecified.
    pub day: i32,
}

impl CivilDate {
    /// Whether all components are zero.
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

impl Serialize for CivilDate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.is_zero() {
            return s.serialize_none();
        }
        let parts = [
            ("year", self.year),
            ("month", self.month),
            ("day", self.day),
        ];
        let mut m = s.serialize_map(None)?;
        for (k, v) in parts.iter().filter(|(_, v)| *v != 0) {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for CivilDate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let m = Option::<BTreeMap<String, i32>>::deserialize(d)?.unwrap_or_default();
        if m.is_empty() {
            return Ok(Self::default());
        }
        let year = *m
            .get("year")
            .ok_or_else(|| D::Error::custom("key \"year\" not found"))?;
        Ok(Self {
            year,
            month: m.get("month").copied().unwrap_or_default(),
            day: m.get("day").copied().unwrap_or_default(),
        })
    }
}
