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
    /// The reason why the model stopped generating tokens. If empty, the model has not stopped
    /// generating the tokens.
    FinishReason {
        /// Not specified.
        Unspecified => "FINISH_REASON_UNSPECIFIED",
        /// The model finished naturally or hit a stop sequence.
        Stop => "STOP",
        /// The candidate reached the maximum number of output tokens.
        MaxTokens => "MAX_TOKENS",
        /// The candidate was flagged for safety reasons.
        Safety => "SAFETY",
        /// The candidate was flagged for recitation.
        Recitation => "RECITATION",
        /// The candidate used an unsupported language.
        Language => "LANGUAGE",
        /// Another, unknown reason.
        Other => "OTHER",
        /// The candidate contained forbidden terms.
        Blocklist => "BLOCKLIST",
        /// The candidate may contain prohibited content.
        ProhibitedContent => "PROHIBITED_CONTENT",
        /// The candidate may contain sensitive personally identifiable information (SPII).
        Spii => "SPII",
        /// The model produced an invalid function call.
        MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
        /// The generated image was flagged for safety reasons.
        ImageSafety => "IMAGE_SAFETY",
        /// The model called a tool when none was enabled.
        UnexpectedToolCall => "UNEXPECTED_TOOL_CALL",
        /// The model made too many tool calls in a row.
        TooManyToolCalls => "TOO_MANY_TOOL_CALLS",
        /// The generated image may contain prohibited content.
        ImageProhibitedContent => "IMAGE_PROHIBITED_CONTENT",
        /// The model was expected to generate an image but did not.
        NoImage => "NO_IMAGE",
        /// The generated image was flagged for recitation.
        ImageRecitation => "IMAGE_RECITATION",
        /// Image generation stopped for another reason.
        ImageOther => "IMAGE_OTHER",
        /// The response is incomplete; the candidate's continuation token continues it.
        Continuation => "CONTINUATION",
    }
);

crate::string_enum!(
    /// The probability of harm for this category.
    HarmProbability {
        /// Not specified.
        Unspecified => "HARM_PROBABILITY_UNSPECIFIED",
        /// Negligible probability of harm.
        Negligible => "NEGLIGIBLE",
        /// Low probability of harm.
        Low => "LOW",
        /// Medium probability of harm.
        Medium => "MEDIUM",
        /// High probability of harm.
        High => "HIGH",
    }
);

crate::string_enum!(
    /// The severity of harm for this category. This enum is not supported in Gemini API.
    HarmSeverity {
        /// Not specified.
        Unspecified => "HARM_SEVERITY_UNSPECIFIED",
        /// Negligible severity.
        Negligible => "HARM_SEVERITY_NEGLIGIBLE",
        /// Low severity.
        Low => "HARM_SEVERITY_LOW",
        /// Medium severity.
        Medium => "HARM_SEVERITY_MEDIUM",
        /// High severity.
        High => "HARM_SEVERITY_HIGH",
    }
);

crate::string_enum!(
    /// The status of the URL retrieval.
    URLRetrievalStatus {
        /// Not specified.
        Unspecified => "URL_RETRIEVAL_STATUS_UNSPECIFIED",
        /// The URL was retrieved.
        Success => "URL_RETRIEVAL_STATUS_SUCCESS",
        /// Retrieving the URL failed.
        Error => "URL_RETRIEVAL_STATUS_ERROR",
        /// The URL is behind a paywall.
        Paywall => "URL_RETRIEVAL_STATUS_PAYWALL",
        /// The URL was judged unsafe.
        Unsafe => "URL_RETRIEVAL_STATUS_UNSAFE",
    }
);

crate::string_enum!(
    /// The reason why the prompt was blocked.
    BlockedReason {
        /// Not specified.
        Unspecified => "BLOCKED_REASON_UNSPECIFIED",
        /// The prompt was blocked for safety reasons.
        Safety => "SAFETY",
        /// The prompt was blocked for another reason.
        Other => "OTHER",
        /// The prompt contained forbidden terms.
        Blocklist => "BLOCKLIST",
        /// The prompt contained prohibited content.
        ProhibitedContent => "PROHIBITED_CONTENT",
        /// The prompt contained unsafe image content.
        ImageSafety => "IMAGE_SAFETY",
        /// The prompt was blocked by Model Armor.
        ModelArmor => "MODEL_ARMOR",
        /// The prompt was judged a jailbreak attempt.
        Jailbreak => "JAILBREAK",
    }
);

crate::string_enum!(
    /// The traffic type for this request. This enum is not supported in Gemini API.
    TrafficType {
        /// Not specified.
        Unspecified => "TRAFFIC_TYPE_UNSPECIFIED",
        /// Pay-as-you-go traffic.
        OnDemand => "ON_DEMAND",
        /// Pay-as-you-go traffic on the priority tier.
        OnDemandPriority => "ON_DEMAND_PRIORITY",
        /// Pay-as-you-go traffic on the flex tier.
        OnDemandFlex => "ON_DEMAND_FLEX",
        /// Pay-as-you-go traffic during off-peak hours.
        OnDemandOffpeak => "ON_DEMAND_OFFPEAK",
        /// Traffic served from Provisioned Throughput.
        ProvisionedThroughput => "PROVISIONED_THROUGHPUT",
    }
);

crate::string_enum!(
    /// The modality that this token count applies to.
    MediaModality {
        /// Not specified.
        Unspecified => "MODALITY_UNSPECIFIED",
        /// Text.
        Text => "TEXT",
        /// Images.
        Image => "IMAGE",
        /// Video.
        Video => "VIDEO",
        /// Audio.
        Audio => "AUDIO",
        /// Documents, such as PDF.
        Document => "DOCUMENT",
    }
);

crate::string_enum!(
    /// The stage of the underlying model. This enum is not supported in Vertex AI.
    ModelStage {
        /// Not specified.
        Unspecified => "MODEL_STAGE_UNSPECIFIED",
        /// Unstable experimental.
        UnstableExperimental => "UNSTABLE_EXPERIMENTAL",
        /// Experimental.
        Experimental => "EXPERIMENTAL",
        /// Preview.
        Preview => "PREVIEW",
        /// Generally available.
        Stable => "STABLE",
        /// Legacy: superseded, but still served.
        Legacy => "LEGACY",
        /// Deprecated: scheduled for retirement.
        Deprecated => "DEPRECATED",
        /// Retired: no longer served.
        Retired => "RETIRED",
    }
);

/// A citation for a piece of generatedcontent. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    /// End of the cited span in the response text (exclusive).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_index: i32,
    /// License of the cited source.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub license: String,
    /// Publication date of the cited source.
    #[serde(default, skip_serializing_if = "CivilDate::is_zero")]
    pub publication_date: CivilDate,
    /// Start of the cited span in the response text.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_index: i32,
    /// Title of the cited source.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    /// URI of the cited source.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
}

/// Citation information when the model quotes another source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationMetadata {
    /// The citations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
}

/// Author attribution for a photo or review.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution {
    /// Name of the author.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    /// URI of the author's profile photo.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub photo_uri: String,
    /// URI of the author's profile.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
}

/// Encapsulates a review snippet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsPlaceAnswerSourcesReviewSnippet {
    /// Who wrote the review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_attribution: Option<GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution>,
    /// URI for flagging the review as inappropriate.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flag_content_uri: String,
    /// Google Maps URI of the review.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub google_maps_uri: String,
    /// When the review was published, relative to now (e.g. "a month ago").
    #[serde(default, skip_serializing_if = "is_zero")]
    pub relative_publish_time_description: String,
    /// Text of the review.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review: String,
    /// ID of the review.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_id: String,
    /// Title of the review.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
}

/// The sources that were used to generate the place answer. This includes review snippets and
/// photos that were used to generate the answer, as well as URIs to flag content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsPlaceAnswerSources {
    /// Review snippets used to generate the answer, under the singular wire name `reviewSnippet`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_snippet: Vec<GroundingChunkMapsPlaceAnswerSourcesReviewSnippet>,
    /// URI for flagging the place as inappropriate.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flag_content_uri: String,
    /// Review snippets used to generate the answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_snippets: Vec<GroundingChunkMapsPlaceAnswerSourcesReviewSnippet>,
}

/// Route information from Google Maps. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMapsRoute {
    /// Length of the route in meters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_meters: Option<i32>,
    /// Estimated travel time of the route.
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub duration: TimeDelta,
    /// Route geometry as an encoded polyline.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub encoded_polyline: String,
}

/// A `Maps` chunk is a piece of evidence that comes from Google Maps. It contains information about
/// a place, such as its name, address, and reviews. This is used to provide the user with rich,
/// location-based information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkMaps {
    /// Sources used to generate the place answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_answer_sources: Option<GroundingChunkMapsPlaceAnswerSources>,
    /// Google Maps place ID.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub place_id: String,
    /// Text describing the place.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    /// Title of the place.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    /// Google Maps URI of the place.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
    /// A route, for routing results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GroundingChunkMapsRoute>,
}

/// An `Image` chunk is a piece of evidence that comes from an image search result. It contains the
/// URI of the image search result and the URI of the image. This is used to provide the user with a
/// link to the source of the information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkImage {
    /// URI of the web page the image comes from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_uri: String,
    /// URI of the image.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_uri: String,
    /// Title of the page the image comes from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    /// Domain of the page the image comes from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub domain: String,
}

/// Represents where the chunk starts and ends in the document. This data type is not supported in
/// Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGChunkPageSpan {
    /// First page of the chunk (1-based).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_page: i32,
    /// Last page of the chunk (1-based).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_page: i32,
}

/// A RAGChunk includes the content of a chunk of a RAGFile, and associated metadata. This data type
/// is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGChunk {
    /// Pages the chunk spans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_span: Option<RAGChunkPageSpan>,
    /// Text of the chunk.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    /// ID of the chunk.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chunk_id: String,
    /// ID of the file the chunk belongs to.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_id: String,
}

/// A list of string values. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkStringList {
    /// The values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

/// User provided metadata about the GroundingFact. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkCustomMetadata {
    /// Metadata key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key: String,
    /// Numeric value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value: Option<f32>,
    /// String list value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string_list_value: Option<GroundingChunkStringList>,
    /// String value.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub string_value: String,
}

/// Context retrieved from a data source to ground the model's response. This is used when a
/// retrieval tool fetches information from a user-provided corpus or a public dataset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkRetrievedContext {
    /// Name of the document the context comes from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub document_name: String,
    /// The RAG chunk the context comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rag_chunk: Option<RAGChunk>,
    /// Text of the retrieved context.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    /// Title of the retrieved context.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    /// URI of the retrieved context.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
    /// Custom metadata of the document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_metadata: Vec<GroundingChunkCustomMetadata>,
    /// Name of the file search store the context comes from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_search_store: String,
    /// Page number of the context within its document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<i32>,
    /// ID of the media the context comes from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub media_id: String,
}

/// A `Web` chunk is a piece of evidence that comes from a web page. It contains the URI of the web
/// page, the title of the page, and the domain of the page. This is used to provide the user with a
/// link to the source of the information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunkWeb {
    /// Domain of the web page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub domain: String,
    /// Title of the web page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    /// URI of the web page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
}

/// A piece of evidence that supports a claim made by the model. This is used to show a citation for
/// a claim made by the model. When grounding is enabled, the model returns a `GroundingChunk` that
/// contains a reference to the source of the information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunk {
    /// An image search result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<GroundingChunkImage>,
    /// A Google Maps result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maps: Option<GroundingChunkMaps>,
    /// Context from a retrieval tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_context: Option<GroundingChunkRetrievedContext>,
    /// A web search result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<GroundingChunkWeb>,
}

/// Segment of the content this support belongs to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// Start of the segment in the part, in bytes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_index: i32,
    /// End of the segment in the part, in bytes (exclusive).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_index: i32,
    /// Index of the [`Part`](crate::Part) the segment belongs to.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub part_index: i32,
    /// Text of the segment.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
}

/// Grounding support.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingSupport {
    /// Confidence of each supporting chunk, in the order of `grounding_chunk_indices`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confidence_scores: Vec<f32>,
    /// Indices into [`GroundingMetadata::grounding_chunks`] of the chunks supporting the claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_chunk_indices: Vec<i32>,
    /// The part of the response the support applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<Segment>,
    /// Indices of the parts of the response that render the support.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_parts: Vec<i32>,
}

/// Metadata returned to client when grounding is enabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalMetadata {
    /// Predicted likelihood (0–1) that Google Search would help answer the prompt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub google_search_dynamic_retrieval_score: f32,
}

/// The entry point used to search for grounding sources.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEntryPoint {
    /// HTML and CSS snippet that renders the Google Search suggestions.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rendered_content: String,
    /// Encoded JSON of search terms and URLs; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub sdk_blob: Bytes,
}

/// A URI that can be used to flag a place or review for inappropriate content. This is populated
/// only when the grounding source is Google Maps. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingMetadataSourceFlaggingURI {
    /// URI for flagging the source as inappropriate.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flag_content_uri: String,
    /// ID of the source (a place or review).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_id: String,
}

/// Information for various kinds of grounding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingMetadata {
    /// Queries used for image search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_search_queries: Vec<String>,
    /// Sources the response was grounded on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_chunks: Vec<GroundingChunk>,
    /// Which chunks support which parts of the response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_supports: Vec<GroundingSupport>,
    /// Metadata about retrieval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_metadata: Option<RetrievalMetadata>,
    /// Google Search suggestions to display alongside the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_entry_point: Option<SearchEntryPoint>,
    /// Queries used for web search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_queries: Vec<String>,
    /// Context token for rendering the Google Maps widget.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub google_maps_widget_context_token: String,
    /// Queries used for retrieval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retrieval_queries: Vec<String>,
    /// URIs for flagging Google Maps sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_flagging_uris: Vec<GroundingMetadataSourceFlaggingURI>,
}

/// A single token and its associated log probability.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogprobsResultCandidate {
    /// Log probability of the token.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub log_probability: f32,
    /// The token.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token: String,
    /// ID of the token.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token_id: i32,
}

/// A list of the top candidate tokens and their log probabilities at each decoding step. This can
/// be used to see what other tokens the model considered.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogprobsResultTopCandidates {
    /// The most likely candidates, in descending log probability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<LogprobsResultCandidate>,
}

/// The log probabilities of the tokens generated by the model. This is useful for understanding the
/// model's confidence in its predictions and for debugging. For example, you can use log
/// probabilities to identify when the model is making a less confident prediction or to explore
/// alternative responses that the model considered. A low log probability can also indicate that
/// the model is "hallucinating" or generating factually incorrect information.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogprobsResult {
    /// The chosen token at each decoding step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_candidates: Vec<LogprobsResultCandidate>,
    /// The most likely candidate tokens at each decoding step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_candidates: Vec<LogprobsResultTopCandidates>,
    /// Sum of the log probabilities of all tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_probability_sum: Option<f32>,
}

/// A safety rating for a piece of content. The safety rating contains the harm category and the
/// harm probability level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyRating {
    /// Whether the content was blocked because of this rating.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blocked: bool,
    /// The harm category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<HarmCategory>,
    /// The threshold that applied instead of the configured one, when overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwritten_threshold: Option<HarmBlockThreshold>,
    /// Probability that the content is harmful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<HarmProbability>,
    /// Probability score (0–1).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub probability_score: f32,
    /// Severity of the harm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<HarmSeverity>,
    /// Severity score (0–1).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub severity_score: f32,
}

/// The metadata for a single URL retrieval.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct URLMetadata {
    /// The URL that was retrieved.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retrieved_url: String,
    /// Whether retrieving the URL succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_retrieval_status: Option<URLRetrievalStatus>,
}

/// Metadata returned when the model uses the `url_context` tool to get information from a
/// user-provided URL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct URLContextMetadata {
    /// One entry per URL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub url_metadata: Vec<URLMetadata>,
}

/// A response candidate generated from the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The generated content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    /// Sources the content cites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_metadata: Option<CitationMetadata>,
    /// Detail on `finish_reason`, when set.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub finish_message: String,
    /// Number of tokens in the candidate.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token_count: i32,
    /// Why generation stopped; unset while still generating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Sources the content is grounded on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<GroundingMetadata>,
    /// Average log probability of the candidate's tokens.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub avg_logprobs: f64,
    /// Index of the candidate among the response's candidates.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub index: i32,
    /// Log probabilities of the tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs_result: Option<LogprobsResult>,
    /// Safety ratings of the candidate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_ratings: Vec<SafetyRating>,
    /// Metadata about URLs retrieved by the URL context tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_context_metadata: Option<URLContextMetadata>,
    /// Token for continuing this response's generation in a later request; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "base64_bytes_opt"
    )]
    pub continuation_token: Option<Bytes>,
}

/// Content filter results for a prompt sent in the request. Note: This is sent only in the first
/// stream chunk and only if no candidates were generated due to content violations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponsePromptFeedback {
    /// Why the prompt was blocked; unset when not blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<BlockedReason>,
    /// Readable detail on `block_reason`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub block_reason_message: String,
    /// Safety ratings of the prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_ratings: Vec<SafetyRating>,
}

/// Represents a breakdown of token usage by modality. This message is used in CountTokensResponse
/// and GenerateContentResponse.UsageMetadata to provide a detailed view of how many tokens are used
/// by each modality (e.g., text, image, video) in a request. This is particularly useful for
/// multimodal models, allowing you to track and manage token consumption for billing and quota
/// purposes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModalityTokenCount {
    /// The modality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<MediaModality>,
    /// Number of tokens of that modality.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub token_count: i32,
}

/// Usage metadata about the content generation request and response. This message provides a
/// detailed breakdown of token usage and other relevant metrics. This data type is not supported in
/// Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponseUsageMetadata {
    /// Cached tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_tokens_details: Vec<ModalityTokenCount>,
    /// Number of tokens in the cached content.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_content_token_count: i32,
    /// Number of tokens across all generated candidates.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub candidates_token_count: i32,
    /// Candidate tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates_tokens_details: Vec<ModalityTokenCount>,
    /// Number of tokens in the prompt, including cached content.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_token_count: i32,
    /// Prompt tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_tokens_details: Vec<ModalityTokenCount>,
    /// Number of thinking tokens.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thoughts_token_count: i32,
    /// Number of tokens in tool-use prompts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_use_prompt_token_count: i32,
    /// Tool-use prompt tokens by modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_use_prompt_tokens_details: Vec<ModalityTokenCount>,
    /// Total tokens: prompt, candidates, thoughts and tool-use prompts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_token_count: i32,
    /// Billing traffic type of the request (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_type: Option<TrafficType>,
}

/// The status of the underlying model. This is used to indicate the stage of the underlying model
/// and the retirement time if applicable. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    /// Readable detail on the model's status.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub message: String,
    /// Release stage of the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_stage: Option<ModelStage>,
    /// When the model is retired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_time: Option<DateTime<Utc>>,
}

/// Response message for PredictionService.GenerateContent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    /// Generated candidates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
    /// When the request was made to the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<DateTime<Utc>>,
    /// Model version used to generate the response.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_version: String,
    /// Feedback on the prompt, set when it was blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_feedback: Option<GenerateContentResponsePromptFeedback>,
    /// ID of the response.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub response_id: String,
    /// Token counts of the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GenerateContentResponseUsageMetadata>,
    /// Status of the model that served the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_status: Option<ModelStatus>,
}

/// A calendar date encoded as a `{"year": .., "month": .., "day": ..}` object with zero components
/// omitted. Decoding a non-empty object without `year` is an error; `{}` and `null` decode to the
/// zero date, which fields omit on encode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CivilDate {
    /// The year.
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
