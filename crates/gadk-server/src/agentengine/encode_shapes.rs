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
//! Struct shapes for the snake-case conversion and the request decoder: every struct reachable
//! from `session.Event` and the Agent Engine request/response models.
//!
//! Recorded from an ADK server's type definitions at commit `0990f9e2`; the
//! generator is no longer part of this repository, so edit with care.
#![allow(dead_code)]

use super::encode::{GoField, GoKind, GoStruct};

/// Shape of `civil.Date`.
pub(crate) const CIVIL_DATE: GoStruct = GoStruct {
    go_type: "civil.Date",
    fields: &[
        GoField { name: "Year", tag: "", embedded: false, exported: true, go_type: "int", kind: GoKind::Int },
        GoField { name: "Month", tag: "", embedded: false, exported: true, go_type: "time.Month", kind: GoKind::Int },
        GoField { name: "Day", tag: "", embedded: false, exported: true, go_type: "int", kind: GoKind::Int },
    ],
};

/// Shape of `genai.Blob`.
pub(crate) const GENAI_BLOB: GoStruct = GoStruct {
    go_type: "genai.Blob",
    fields: &[
        GoField { name: "Data", tag: "data,omitempty", embedded: false, exported: true, go_type: "[]uint8", kind: GoKind::Bytes },
        GoField { name: "DisplayName", tag: "displayName,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "MIMEType", tag: "mimeType,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.Citation`.
pub(crate) const GENAI_CITATION: GoStruct = GoStruct {
    go_type: "genai.Citation",
    fields: &[
        GoField { name: "EndIndex", tag: "endIndex,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "License", tag: "license,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PublicationDate", tag: "publicationDate,omitempty", embedded: false, exported: true, go_type: "civil.Date", kind: GoKind::Struct("civil.Date") },
        GoField { name: "StartIndex", tag: "startIndex,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "URI", tag: "uri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.CitationMetadata`.
pub(crate) const GENAI_CITATION_METADATA: GoStruct = GoStruct {
    go_type: "genai.CitationMetadata",
    fields: &[
        GoField { name: "Citations", tag: "citations,omitempty", embedded: false, exported: true, go_type: "[]*genai.Citation", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.Citation"))) },
    ],
};

/// Shape of `genai.CodeExecutionResult`.
pub(crate) const GENAI_CODE_EXECUTION_RESULT: GoStruct = GoStruct {
    go_type: "genai.CodeExecutionResult",
    fields: &[
        GoField { name: "Outcome", tag: "outcome,omitempty", embedded: false, exported: true, go_type: "genai.Outcome", kind: GoKind::NamedString },
        GoField { name: "Output", tag: "output,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ID", tag: "id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.Content`.
pub(crate) const GENAI_CONTENT: GoStruct = GoStruct {
    go_type: "genai.Content",
    fields: &[
        GoField { name: "Parts", tag: "parts,omitempty", embedded: false, exported: true, go_type: "[]*genai.Part", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.Part"))) },
        GoField { name: "Role", tag: "role,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.ExecutableCode`.
pub(crate) const GENAI_EXECUTABLE_CODE: GoStruct = GoStruct {
    go_type: "genai.ExecutableCode",
    fields: &[
        GoField { name: "Code", tag: "code,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Language", tag: "language,omitempty", embedded: false, exported: true, go_type: "genai.Language", kind: GoKind::NamedString },
        GoField { name: "ID", tag: "id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.FileData`.
pub(crate) const GENAI_FILE_DATA: GoStruct = GoStruct {
    go_type: "genai.FileData",
    fields: &[
        GoField { name: "DisplayName", tag: "displayName,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "FileURI", tag: "fileUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "MIMEType", tag: "mimeType,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.FunctionCall`.
pub(crate) const GENAI_FUNCTION_CALL: GoStruct = GoStruct {
    go_type: "genai.FunctionCall",
    fields: &[
        GoField { name: "ID", tag: "id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Args", tag: "args,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
        GoField { name: "Name", tag: "name,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PartialArgs", tag: "partialArgs,omitempty", embedded: false, exported: true, go_type: "[]*genai.PartialArg", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.PartialArg"))) },
        GoField { name: "WillContinue", tag: "willContinue,omitempty", embedded: false, exported: true, go_type: "*bool", kind: GoKind::Pointer(&GoKind::Bool) },
    ],
};

/// Shape of `genai.FunctionResponse`.
pub(crate) const GENAI_FUNCTION_RESPONSE: GoStruct = GoStruct {
    go_type: "genai.FunctionResponse",
    fields: &[
        GoField { name: "WillContinue", tag: "willContinue,omitempty", embedded: false, exported: true, go_type: "*bool", kind: GoKind::Pointer(&GoKind::Bool) },
        GoField { name: "Scheduling", tag: "scheduling,omitempty", embedded: false, exported: true, go_type: "genai.FunctionResponseScheduling", kind: GoKind::NamedString },
        GoField { name: "Parts", tag: "parts,omitempty", embedded: false, exported: true, go_type: "[]*genai.FunctionResponsePart", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.FunctionResponsePart"))) },
        GoField { name: "ID", tag: "id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Name", tag: "name,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Response", tag: "response,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `genai.FunctionResponseBlob`.
pub(crate) const GENAI_FUNCTION_RESPONSE_BLOB: GoStruct = GoStruct {
    go_type: "genai.FunctionResponseBlob",
    fields: &[
        GoField { name: "MIMEType", tag: "mimeType,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Data", tag: "data,omitempty", embedded: false, exported: true, go_type: "[]uint8", kind: GoKind::Bytes },
        GoField { name: "DisplayName", tag: "displayName,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.FunctionResponseFileData`.
pub(crate) const GENAI_FUNCTION_RESPONSE_FILE_DATA: GoStruct = GoStruct {
    go_type: "genai.FunctionResponseFileData",
    fields: &[
        GoField { name: "FileURI", tag: "fileUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "MIMEType", tag: "mimeType,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "DisplayName", tag: "displayName,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.FunctionResponsePart`.
pub(crate) const GENAI_FUNCTION_RESPONSE_PART: GoStruct = GoStruct {
    go_type: "genai.FunctionResponsePart",
    fields: &[
        GoField { name: "InlineData", tag: "inlineData,omitempty", embedded: false, exported: true, go_type: "*genai.FunctionResponseBlob", kind: GoKind::Pointer(&GoKind::Struct("genai.FunctionResponseBlob")) },
        GoField { name: "FileData", tag: "fileData,omitempty", embedded: false, exported: true, go_type: "*genai.FunctionResponseFileData", kind: GoKind::Pointer(&GoKind::Struct("genai.FunctionResponseFileData")) },
        GoField { name: "MediaProcessing", tag: "mediaProcessing,omitempty", embedded: false, exported: true, go_type: "genai.MediaProcessing", kind: GoKind::NamedString },
    ],
};

/// Shape of `genai.GenerateContentResponseUsageMetadata`.
pub(crate) const GENAI_GENERATE_CONTENT_RESPONSE_USAGE_METADATA: GoStruct = GoStruct {
    go_type: "genai.GenerateContentResponseUsageMetadata",
    fields: &[
        GoField { name: "CacheTokensDetails", tag: "cacheTokensDetails,omitempty", embedded: false, exported: true, go_type: "[]*genai.ModalityTokenCount", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.ModalityTokenCount"))) },
        GoField { name: "CachedContentTokenCount", tag: "cachedContentTokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "CandidatesTokenCount", tag: "candidatesTokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "CandidatesTokensDetails", tag: "candidatesTokensDetails,omitempty", embedded: false, exported: true, go_type: "[]*genai.ModalityTokenCount", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.ModalityTokenCount"))) },
        GoField { name: "PromptTokenCount", tag: "promptTokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "PromptTokensDetails", tag: "promptTokensDetails,omitempty", embedded: false, exported: true, go_type: "[]*genai.ModalityTokenCount", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.ModalityTokenCount"))) },
        GoField { name: "ThoughtsTokenCount", tag: "thoughtsTokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "ToolUsePromptTokenCount", tag: "toolUsePromptTokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "ToolUsePromptTokensDetails", tag: "toolUsePromptTokensDetails,omitempty", embedded: false, exported: true, go_type: "[]*genai.ModalityTokenCount", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.ModalityTokenCount"))) },
        GoField { name: "TotalTokenCount", tag: "totalTokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "TrafficType", tag: "trafficType,omitempty", embedded: false, exported: true, go_type: "genai.TrafficType", kind: GoKind::NamedString },
    ],
};

/// Shape of `genai.GroundingChunk`.
pub(crate) const GENAI_GROUNDING_CHUNK: GoStruct = GoStruct {
    go_type: "genai.GroundingChunk",
    fields: &[
        GoField { name: "Image", tag: "image,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkImage", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkImage")) },
        GoField { name: "Maps", tag: "maps,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkMaps", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkMaps")) },
        GoField { name: "RetrievedContext", tag: "retrievedContext,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkRetrievedContext", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkRetrievedContext")) },
        GoField { name: "Web", tag: "web,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkWeb", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkWeb")) },
    ],
};

/// Shape of `genai.GroundingChunkCustomMetadata`.
pub(crate) const GENAI_GROUNDING_CHUNK_CUSTOM_METADATA: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkCustomMetadata",
    fields: &[
        GoField { name: "Key", tag: "key,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "NumericValue", tag: "numericValue,omitempty", embedded: false, exported: true, go_type: "*float32", kind: GoKind::Pointer(&GoKind::Float32) },
        GoField { name: "StringListValue", tag: "stringListValue,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkStringList", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkStringList")) },
        GoField { name: "StringValue", tag: "stringValue,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingChunkImage`.
pub(crate) const GENAI_GROUNDING_CHUNK_IMAGE: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkImage",
    fields: &[
        GoField { name: "SourceURI", tag: "sourceUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ImageURI", tag: "imageUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Domain", tag: "domain,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingChunkMaps`.
pub(crate) const GENAI_GROUNDING_CHUNK_MAPS: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkMaps",
    fields: &[
        GoField { name: "PlaceAnswerSources", tag: "placeAnswerSources,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkMapsPlaceAnswerSources", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkMapsPlaceAnswerSources")) },
        GoField { name: "PlaceID", tag: "placeId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Text", tag: "text,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "URI", tag: "uri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Route", tag: "route,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkMapsRoute", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkMapsRoute")) },
    ],
};

/// Shape of `genai.GroundingChunkMapsPlaceAnswerSources`.
pub(crate) const GENAI_GROUNDING_CHUNK_MAPS_PLACE_ANSWER_SOURCES: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkMapsPlaceAnswerSources",
    fields: &[
        GoField { name: "ReviewSnippet", tag: "reviewSnippet,omitempty", embedded: false, exported: true, go_type: "[]*genai.GroundingChunkMapsPlaceAnswerSourcesReviewSnippet", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkMapsPlaceAnswerSourcesReviewSnippet"))) },
        GoField { name: "FlagContentURI", tag: "flagContentUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ReviewSnippets", tag: "reviewSnippets,omitempty", embedded: false, exported: true, go_type: "[]*genai.GroundingChunkMapsPlaceAnswerSourcesReviewSnippet", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkMapsPlaceAnswerSourcesReviewSnippet"))) },
    ],
};

/// Shape of `genai.GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution`.
pub(crate) const GENAI_GROUNDING_CHUNK_MAPS_PLACE_ANSWER_SOURCES_AUTHOR_ATTRIBUTION: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution",
    fields: &[
        GoField { name: "DisplayName", tag: "displayName,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PhotoURI", tag: "photoUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "URI", tag: "uri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingChunkMapsPlaceAnswerSourcesReviewSnippet`.
pub(crate) const GENAI_GROUNDING_CHUNK_MAPS_PLACE_ANSWER_SOURCES_REVIEW_SNIPPET: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkMapsPlaceAnswerSourcesReviewSnippet",
    fields: &[
        GoField { name: "AuthorAttribution", tag: "authorAttribution,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkMapsPlaceAnswerSourcesAuthorAttribution")) },
        GoField { name: "FlagContentURI", tag: "flagContentUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "GoogleMapsURI", tag: "googleMapsUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RelativePublishTimeDescription", tag: "relativePublishTimeDescription,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Review", tag: "review,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ReviewID", tag: "reviewId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingChunkMapsRoute`.
pub(crate) const GENAI_GROUNDING_CHUNK_MAPS_ROUTE: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkMapsRoute",
    fields: &[
        GoField { name: "DistanceMeters", tag: "distanceMeters,omitempty", embedded: false, exported: true, go_type: "*int32", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "Duration", tag: "duration,omitempty", embedded: false, exported: true, go_type: "time.Duration", kind: GoKind::Int },
        GoField { name: "EncodedPolyline", tag: "encodedPolyline,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingChunkRetrievedContext`.
pub(crate) const GENAI_GROUNDING_CHUNK_RETRIEVED_CONTEXT: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkRetrievedContext",
    fields: &[
        GoField { name: "DocumentName", tag: "documentName,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RAGChunk", tag: "ragChunk,omitempty", embedded: false, exported: true, go_type: "*genai.RAGChunk", kind: GoKind::Pointer(&GoKind::Struct("genai.RAGChunk")) },
        GoField { name: "Text", tag: "text,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "URI", tag: "uri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "CustomMetadata", tag: "customMetadata,omitempty", embedded: false, exported: true, go_type: "[]*genai.GroundingChunkCustomMetadata", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.GroundingChunkCustomMetadata"))) },
        GoField { name: "FileSearchStore", tag: "fileSearchStore,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PageNumber", tag: "pageNumber,omitempty", embedded: false, exported: true, go_type: "*int32", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "MediaID", tag: "mediaId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingChunkStringList`.
pub(crate) const GENAI_GROUNDING_CHUNK_STRING_LIST: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkStringList",
    fields: &[
        GoField { name: "Values", tag: "values,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
    ],
};

/// Shape of `genai.GroundingChunkWeb`.
pub(crate) const GENAI_GROUNDING_CHUNK_WEB: GoStruct = GoStruct {
    go_type: "genai.GroundingChunkWeb",
    fields: &[
        GoField { name: "Domain", tag: "domain,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "URI", tag: "uri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingMetadata`.
pub(crate) const GENAI_GROUNDING_METADATA: GoStruct = GoStruct {
    go_type: "genai.GroundingMetadata",
    fields: &[
        GoField { name: "ImageSearchQueries", tag: "imageSearchQueries,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "GroundingChunks", tag: "groundingChunks,omitempty", embedded: false, exported: true, go_type: "[]*genai.GroundingChunk", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.GroundingChunk"))) },
        GoField { name: "GroundingSupports", tag: "groundingSupports,omitempty", embedded: false, exported: true, go_type: "[]*genai.GroundingSupport", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.GroundingSupport"))) },
        GoField { name: "RetrievalMetadata", tag: "retrievalMetadata,omitempty", embedded: false, exported: true, go_type: "*genai.RetrievalMetadata", kind: GoKind::Pointer(&GoKind::Struct("genai.RetrievalMetadata")) },
        GoField { name: "SearchEntryPoint", tag: "searchEntryPoint,omitempty", embedded: false, exported: true, go_type: "*genai.SearchEntryPoint", kind: GoKind::Pointer(&GoKind::Struct("genai.SearchEntryPoint")) },
        GoField { name: "WebSearchQueries", tag: "webSearchQueries,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "GoogleMapsWidgetContextToken", tag: "googleMapsWidgetContextToken,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RetrievalQueries", tag: "retrievalQueries,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "SourceFlaggingUris", tag: "sourceFlaggingUris,omitempty", embedded: false, exported: true, go_type: "[]*genai.GroundingMetadataSourceFlaggingURI", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.GroundingMetadataSourceFlaggingURI"))) },
    ],
};

/// Shape of `genai.GroundingMetadataSourceFlaggingURI`.
pub(crate) const GENAI_GROUNDING_METADATA_SOURCE_FLAGGING_URI: GoStruct = GoStruct {
    go_type: "genai.GroundingMetadataSourceFlaggingURI",
    fields: &[
        GoField { name: "FlagContentURI", tag: "flagContentUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SourceID", tag: "sourceId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.GroundingSupport`.
pub(crate) const GENAI_GROUNDING_SUPPORT: GoStruct = GoStruct {
    go_type: "genai.GroundingSupport",
    fields: &[
        GoField { name: "ConfidenceScores", tag: "confidenceScores,omitempty", embedded: false, exported: true, go_type: "[]float32", kind: GoKind::Slice(&GoKind::Float32) },
        GoField { name: "GroundingChunkIndices", tag: "groundingChunkIndices,omitempty", embedded: false, exported: true, go_type: "[]int32", kind: GoKind::Slice(&GoKind::Int) },
        GoField { name: "Segment", tag: "segment,omitempty", embedded: false, exported: true, go_type: "*genai.Segment", kind: GoKind::Pointer(&GoKind::Struct("genai.Segment")) },
        GoField { name: "RenderedParts", tag: "renderedParts,omitempty", embedded: false, exported: true, go_type: "[]int32", kind: GoKind::Slice(&GoKind::Int) },
    ],
};

/// Shape of `genai.LogprobsResult`.
pub(crate) const GENAI_LOGPROBS_RESULT: GoStruct = GoStruct {
    go_type: "genai.LogprobsResult",
    fields: &[
        GoField { name: "ChosenCandidates", tag: "chosenCandidates,omitempty", embedded: false, exported: true, go_type: "[]*genai.LogprobsResultCandidate", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.LogprobsResultCandidate"))) },
        GoField { name: "TopCandidates", tag: "topCandidates,omitempty", embedded: false, exported: true, go_type: "[]*genai.LogprobsResultTopCandidates", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.LogprobsResultTopCandidates"))) },
        GoField { name: "LogProbabilitySum", tag: "logProbabilitySum,omitempty", embedded: false, exported: true, go_type: "*float32", kind: GoKind::Pointer(&GoKind::Float32) },
    ],
};

/// Shape of `genai.LogprobsResultCandidate`.
pub(crate) const GENAI_LOGPROBS_RESULT_CANDIDATE: GoStruct = GoStruct {
    go_type: "genai.LogprobsResultCandidate",
    fields: &[
        GoField { name: "LogProbability", tag: "logProbability,omitempty", embedded: false, exported: true, go_type: "float32", kind: GoKind::Float32 },
        GoField { name: "Token", tag: "token,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "TokenID", tag: "tokenId,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
    ],
};

/// Shape of `genai.LogprobsResultTopCandidates`.
pub(crate) const GENAI_LOGPROBS_RESULT_TOP_CANDIDATES: GoStruct = GoStruct {
    go_type: "genai.LogprobsResultTopCandidates",
    fields: &[
        GoField { name: "Candidates", tag: "candidates,omitempty", embedded: false, exported: true, go_type: "[]*genai.LogprobsResultCandidate", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.LogprobsResultCandidate"))) },
    ],
};

/// Shape of `genai.ModalityTokenCount`.
pub(crate) const GENAI_MODALITY_TOKEN_COUNT: GoStruct = GoStruct {
    go_type: "genai.ModalityTokenCount",
    fields: &[
        GoField { name: "Modality", tag: "modality,omitempty", embedded: false, exported: true, go_type: "genai.MediaModality", kind: GoKind::NamedString },
        GoField { name: "TokenCount", tag: "tokenCount,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
    ],
};

/// Shape of `genai.Part`.
pub(crate) const GENAI_PART: GoStruct = GoStruct {
    go_type: "genai.Part",
    fields: &[
        GoField { name: "MediaResolution", tag: "mediaResolution,omitempty", embedded: false, exported: true, go_type: "*genai.PartMediaResolution", kind: GoKind::Pointer(&GoKind::Struct("genai.PartMediaResolution")) },
        GoField { name: "CodeExecutionResult", tag: "codeExecutionResult,omitempty", embedded: false, exported: true, go_type: "*genai.CodeExecutionResult", kind: GoKind::Pointer(&GoKind::Struct("genai.CodeExecutionResult")) },
        GoField { name: "ExecutableCode", tag: "executableCode,omitempty", embedded: false, exported: true, go_type: "*genai.ExecutableCode", kind: GoKind::Pointer(&GoKind::Struct("genai.ExecutableCode")) },
        GoField { name: "FileData", tag: "fileData,omitempty", embedded: false, exported: true, go_type: "*genai.FileData", kind: GoKind::Pointer(&GoKind::Struct("genai.FileData")) },
        GoField { name: "FunctionCall", tag: "functionCall,omitempty", embedded: false, exported: true, go_type: "*genai.FunctionCall", kind: GoKind::Pointer(&GoKind::Struct("genai.FunctionCall")) },
        GoField { name: "FunctionResponse", tag: "functionResponse,omitempty", embedded: false, exported: true, go_type: "*genai.FunctionResponse", kind: GoKind::Pointer(&GoKind::Struct("genai.FunctionResponse")) },
        GoField { name: "InlineData", tag: "inlineData,omitempty", embedded: false, exported: true, go_type: "*genai.Blob", kind: GoKind::Pointer(&GoKind::Struct("genai.Blob")) },
        GoField { name: "Text", tag: "text,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Thought", tag: "thought,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "ThoughtSignature", tag: "thoughtSignature,omitempty", embedded: false, exported: true, go_type: "[]uint8", kind: GoKind::Bytes },
        GoField { name: "VideoMetadata", tag: "videoMetadata,omitempty", embedded: false, exported: true, go_type: "*genai.VideoMetadata", kind: GoKind::Pointer(&GoKind::Struct("genai.VideoMetadata")) },
        GoField { name: "ToolCall", tag: "toolCall,omitempty", embedded: false, exported: true, go_type: "*genai.ToolCall", kind: GoKind::Pointer(&GoKind::Struct("genai.ToolCall")) },
        GoField { name: "ToolResponse", tag: "toolResponse,omitempty", embedded: false, exported: true, go_type: "*genai.ToolResponse", kind: GoKind::Pointer(&GoKind::Struct("genai.ToolResponse")) },
        GoField { name: "PartMetadata", tag: "partMetadata,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
        GoField { name: "AudioTranscription", tag: "audioTranscription,omitempty", embedded: false, exported: true, go_type: "*genai.Transcription", kind: GoKind::Pointer(&GoKind::Struct("genai.Transcription")) },
        GoField { name: "MediaProcessing", tag: "mediaProcessing,omitempty", embedded: false, exported: true, go_type: "genai.MediaProcessing", kind: GoKind::NamedString },
        GoField { name: "SpeechMetadata", tag: "speechMetadata,omitempty", embedded: false, exported: true, go_type: "*genai.SpeechMetadata", kind: GoKind::Pointer(&GoKind::Struct("genai.SpeechMetadata")) },
    ],
};

/// Shape of `genai.PartMediaResolution`.
pub(crate) const GENAI_PART_MEDIA_RESOLUTION: GoStruct = GoStruct {
    go_type: "genai.PartMediaResolution",
    fields: &[
        GoField { name: "Level", tag: "level,omitempty", embedded: false, exported: true, go_type: "genai.PartMediaResolutionLevel", kind: GoKind::NamedString },
        GoField { name: "NumTokens", tag: "numTokens,omitempty", embedded: false, exported: true, go_type: "*int32", kind: GoKind::Pointer(&GoKind::Int) },
    ],
};

/// Shape of `genai.PartialArg`.
pub(crate) const GENAI_PARTIAL_ARG: GoStruct = GoStruct {
    go_type: "genai.PartialArg",
    fields: &[
        GoField { name: "BoolValue", tag: "boolValue,omitempty", embedded: false, exported: true, go_type: "*bool", kind: GoKind::Pointer(&GoKind::Bool) },
        GoField { name: "JsonPath", tag: "jsonPath,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "NULLValue", tag: "nullValue,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "NumberValue", tag: "numberValue,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "StringValue", tag: "stringValue,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "WillContinue", tag: "willContinue,omitempty", embedded: false, exported: true, go_type: "*bool", kind: GoKind::Pointer(&GoKind::Bool) },
    ],
};

/// Shape of `genai.RAGChunk`.
pub(crate) const GENAI_RAG_CHUNK: GoStruct = GoStruct {
    go_type: "genai.RAGChunk",
    fields: &[
        GoField { name: "PageSpan", tag: "pageSpan,omitempty", embedded: false, exported: true, go_type: "*genai.RAGChunkPageSpan", kind: GoKind::Pointer(&GoKind::Struct("genai.RAGChunkPageSpan")) },
        GoField { name: "Text", tag: "text,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ChunkID", tag: "chunkId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "FileID", tag: "fileId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.RAGChunkPageSpan`.
pub(crate) const GENAI_RAG_CHUNK_PAGE_SPAN: GoStruct = GoStruct {
    go_type: "genai.RAGChunkPageSpan",
    fields: &[
        GoField { name: "FirstPage", tag: "firstPage,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "LastPage", tag: "lastPage,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
    ],
};

/// Shape of `genai.RetrievalMetadata`.
pub(crate) const GENAI_RETRIEVAL_METADATA: GoStruct = GoStruct {
    go_type: "genai.RetrievalMetadata",
    fields: &[
        GoField { name: "GoogleSearchDynamicRetrievalScore", tag: "googleSearchDynamicRetrievalScore,omitempty", embedded: false, exported: true, go_type: "float32", kind: GoKind::Float32 },
    ],
};

/// Shape of `genai.SearchEntryPoint`.
pub(crate) const GENAI_SEARCH_ENTRY_POINT: GoStruct = GoStruct {
    go_type: "genai.SearchEntryPoint",
    fields: &[
        GoField { name: "RenderedContent", tag: "renderedContent,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SDKBlob", tag: "sdkBlob,omitempty", embedded: false, exported: true, go_type: "[]uint8", kind: GoKind::Bytes },
    ],
};

/// Shape of `genai.Segment`.
pub(crate) const GENAI_SEGMENT: GoStruct = GoStruct {
    go_type: "genai.Segment",
    fields: &[
        GoField { name: "StartIndex", tag: "startIndex,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "EndIndex", tag: "endIndex,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "PartIndex", tag: "partIndex,omitempty", embedded: false, exported: true, go_type: "int32", kind: GoKind::Int },
        GoField { name: "Text", tag: "text,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.SpeechMetadata`.
pub(crate) const GENAI_SPEECH_METADATA: GoStruct = GoStruct {
    go_type: "genai.SpeechMetadata",
    fields: &[
        GoField { name: "Speaker", tag: "speaker,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Style", tag: "style,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `genai.ToolCall`.
pub(crate) const GENAI_TOOL_CALL: GoStruct = GoStruct {
    go_type: "genai.ToolCall",
    fields: &[
        GoField { name: "ID", tag: "id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ToolType", tag: "toolType,omitempty", embedded: false, exported: true, go_type: "genai.ToolType", kind: GoKind::NamedString },
        GoField { name: "Args", tag: "args,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `genai.ToolResponse`.
pub(crate) const GENAI_TOOL_RESPONSE: GoStruct = GoStruct {
    go_type: "genai.ToolResponse",
    fields: &[
        GoField { name: "ID", tag: "id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ToolType", tag: "toolType,omitempty", embedded: false, exported: true, go_type: "genai.ToolType", kind: GoKind::NamedString },
        GoField { name: "Response", tag: "response,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `genai.Transcription`.
pub(crate) const GENAI_TRANSCRIPTION: GoStruct = GoStruct {
    go_type: "genai.Transcription",
    fields: &[
        GoField { name: "Text", tag: "text,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Finished", tag: "finished,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "LanguageCode", tag: "languageCode,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SpeakerLabel", tag: "speakerLabel,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Words", tag: "words,omitempty", embedded: false, exported: true, go_type: "[]*genai.WordInfo", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("genai.WordInfo"))) },
        GoField { name: "StartOffset", tag: "startOffset,omitempty", embedded: false, exported: true, go_type: "time.Duration", kind: GoKind::Int },
        GoField { name: "EndOffset", tag: "endOffset,omitempty", embedded: false, exported: true, go_type: "time.Duration", kind: GoKind::Int },
    ],
};

/// Shape of `genai.VideoMetadata`.
pub(crate) const GENAI_VIDEO_METADATA: GoStruct = GoStruct {
    go_type: "genai.VideoMetadata",
    fields: &[
        GoField { name: "EndOffset", tag: "endOffset,omitempty", embedded: false, exported: true, go_type: "time.Duration", kind: GoKind::Int },
        GoField { name: "FPS", tag: "fps,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "StartOffset", tag: "startOffset,omitempty", embedded: false, exported: true, go_type: "time.Duration", kind: GoKind::Int },
    ],
};

/// Shape of `genai.WordInfo`.
pub(crate) const GENAI_WORD_INFO: GoStruct = GoStruct {
    go_type: "genai.WordInfo",
    fields: &[
        GoField { name: "Word", tag: "word,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "StartOffset", tag: "startOffset,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "EndOffset", tag: "endOffset,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `jsonschema.Schema`.
pub(crate) const JSONSCHEMA_SCHEMA: GoStruct = GoStruct {
    go_type: "jsonschema.Schema",
    fields: &[
        GoField { name: "ID", tag: "$id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Schema", tag: "$schema,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Ref", tag: "$ref,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Comment", tag: "$comment,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Defs", tag: "$defs,omitempty", embedded: false, exported: true, go_type: "map[string]*jsonschema.Schema", kind: GoKind::Map(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "Definitions", tag: "definitions,omitempty", embedded: false, exported: true, go_type: "map[string]*jsonschema.Schema", kind: GoKind::Map(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "DependencySchemas", tag: "-", embedded: false, exported: true, go_type: "map[string]*jsonschema.Schema", kind: GoKind::Map(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "DependencyStrings", tag: "-", embedded: false, exported: true, go_type: "map[string][]string", kind: GoKind::Map(&GoKind::Slice(&GoKind::String)) },
        GoField { name: "Anchor", tag: "$anchor,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "DynamicAnchor", tag: "$dynamicAnchor,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "DynamicRef", tag: "$dynamicRef,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Vocabulary", tag: "$vocabulary,omitempty", embedded: false, exported: true, go_type: "map[string]bool", kind: GoKind::Map(&GoKind::Bool) },
        GoField { name: "Title", tag: "title,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Description", tag: "description,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Default", tag: "default,omitempty", embedded: false, exported: true, go_type: "jsontext.Value", kind: GoKind::RawMessage },
        GoField { name: "Deprecated", tag: "deprecated,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "ReadOnly", tag: "readOnly,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "WriteOnly", tag: "writeOnly,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "Examples", tag: "examples,omitempty", embedded: false, exported: true, go_type: "[]interface {}", kind: GoKind::Slice(&GoKind::Interface) },
        GoField { name: "Type", tag: "-", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Types", tag: "-", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "Enum", tag: "enum,omitempty", embedded: false, exported: true, go_type: "[]interface {}", kind: GoKind::Slice(&GoKind::Interface) },
        GoField { name: "Const", tag: "const,omitempty", embedded: false, exported: true, go_type: "*interface {}", kind: GoKind::Pointer(&GoKind::Interface) },
        GoField { name: "MultipleOf", tag: "multipleOf,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "Minimum", tag: "minimum,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "Maximum", tag: "maximum,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "ExclusiveMinimum", tag: "exclusiveMinimum,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "ExclusiveMaximum", tag: "exclusiveMaximum,omitempty", embedded: false, exported: true, go_type: "*float64", kind: GoKind::Pointer(&GoKind::Float64) },
        GoField { name: "MinLength", tag: "minLength,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "MaxLength", tag: "maxLength,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "Pattern", tag: "pattern,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PrefixItems", tag: "prefixItems,omitempty", embedded: false, exported: true, go_type: "[]*jsonschema.Schema", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "Items", tag: "-", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "ItemsArray", tag: "-", embedded: false, exported: true, go_type: "[]*jsonschema.Schema", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "MinItems", tag: "minItems,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "MaxItems", tag: "maxItems,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "AdditionalItems", tag: "additionalItems,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "UniqueItems", tag: "uniqueItems,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "Contains", tag: "contains,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "MinContains", tag: "minContains,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "MaxContains", tag: "maxContains,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "UnevaluatedItems", tag: "unevaluatedItems,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "MinProperties", tag: "minProperties,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "MaxProperties", tag: "maxProperties,omitempty", embedded: false, exported: true, go_type: "*int", kind: GoKind::Pointer(&GoKind::Int) },
        GoField { name: "Required", tag: "required,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "DependentRequired", tag: "dependentRequired,omitempty", embedded: false, exported: true, go_type: "map[string][]string", kind: GoKind::Map(&GoKind::Slice(&GoKind::String)) },
        GoField { name: "Properties", tag: "properties,omitempty", embedded: false, exported: true, go_type: "map[string]*jsonschema.Schema", kind: GoKind::Map(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "PatternProperties", tag: "patternProperties,omitempty", embedded: false, exported: true, go_type: "map[string]*jsonschema.Schema", kind: GoKind::Map(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "AdditionalProperties", tag: "additionalProperties,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "PropertyNames", tag: "propertyNames,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "UnevaluatedProperties", tag: "unevaluatedProperties,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "AllOf", tag: "allOf,omitempty", embedded: false, exported: true, go_type: "[]*jsonschema.Schema", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "AnyOf", tag: "anyOf,omitempty", embedded: false, exported: true, go_type: "[]*jsonschema.Schema", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "OneOf", tag: "oneOf,omitempty", embedded: false, exported: true, go_type: "[]*jsonschema.Schema", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "Not", tag: "not,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "If", tag: "if,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "Then", tag: "then,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "Else", tag: "else,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "DependentSchemas", tag: "dependentSchemas,omitempty", embedded: false, exported: true, go_type: "map[string]*jsonschema.Schema", kind: GoKind::Map(&GoKind::Pointer(&GoKind::Struct("jsonschema.Schema"))) },
        GoField { name: "ContentEncoding", tag: "contentEncoding,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ContentMediaType", tag: "contentMediaType,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ContentSchema", tag: "contentSchema,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "Format", tag: "format,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
        GoField { name: "PropertyOrder", tag: "-", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
    ],
};

/// Shape of `model.LLMResponse`.
pub(crate) const MODEL_LLM_RESPONSE: GoStruct = GoStruct {
    go_type: "model.LLMResponse",
    fields: &[
        GoField { name: "Content", tag: "content,omitempty", embedded: false, exported: true, go_type: "*genai.Content", kind: GoKind::Pointer(&GoKind::Struct("genai.Content")) },
        GoField { name: "CitationMetadata", tag: "citationMetadata,omitempty", embedded: false, exported: true, go_type: "*genai.CitationMetadata", kind: GoKind::Pointer(&GoKind::Struct("genai.CitationMetadata")) },
        GoField { name: "GroundingMetadata", tag: "groundingMetadata,omitempty", embedded: false, exported: true, go_type: "*genai.GroundingMetadata", kind: GoKind::Pointer(&GoKind::Struct("genai.GroundingMetadata")) },
        GoField { name: "UsageMetadata", tag: "usageMetadata,omitempty", embedded: false, exported: true, go_type: "*genai.GenerateContentResponseUsageMetadata", kind: GoKind::Pointer(&GoKind::Struct("genai.GenerateContentResponseUsageMetadata")) },
        GoField { name: "CustomMetadata", tag: "customMetadata,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
        GoField { name: "LogprobsResult", tag: "logprobsResult,omitempty", embedded: false, exported: true, go_type: "*genai.LogprobsResult", kind: GoKind::Pointer(&GoKind::Struct("genai.LogprobsResult")) },
        GoField { name: "InputTranscription", tag: "inputTranscription,omitempty", embedded: false, exported: true, go_type: "*genai.Transcription", kind: GoKind::Pointer(&GoKind::Struct("genai.Transcription")) },
        GoField { name: "OutputTranscription", tag: "outputTranscription,omitempty", embedded: false, exported: true, go_type: "*genai.Transcription", kind: GoKind::Pointer(&GoKind::Struct("genai.Transcription")) },
        GoField { name: "ModelVersion", tag: "modelVersion,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Partial", tag: "partial,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "TurnComplete", tag: "turnComplete,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "Interrupted", tag: "interrupted,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "SessionResumptionHandle", tag: "sessionResumptionHandle,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ErrorCode", tag: "errorCode,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ErrorMessage", tag: "errorMessage,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "FinishReason", tag: "finishReason,omitempty", embedded: false, exported: true, go_type: "genai.FinishReason", kind: GoKind::NamedString },
        GoField { name: "AvgLogprobs", tag: "avgLogprobs,omitempty", embedded: false, exported: true, go_type: "float64", kind: GoKind::Float64 },
    ],
};

/// Shape of `models.CreateSessionInput`.
pub(crate) const MODELS_CREATE_SESSION_INPUT: GoStruct = GoStruct {
    go_type: "models.CreateSessionInput",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "State", tag: "state,omitempty", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `models.CreateSessionRequest`.
pub(crate) const MODELS_CREATE_SESSION_REQUEST: GoStruct = GoStruct {
    go_type: "models.CreateSessionRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.CreateSessionInput", kind: GoKind::Struct("models.CreateSessionInput") },
    ],
};

/// Shape of `models.CreateSessionResponse`.
pub(crate) const MODELS_CREATE_SESSION_RESPONSE: GoStruct = GoStruct {
    go_type: "models.CreateSessionResponse",
    fields: &[
        GoField { name: "Output", tag: "output", embedded: false, exported: true, go_type: "models.SessionData", kind: GoKind::Struct("models.SessionData") },
    ],
};

/// Shape of `models.DeleteSessionInput`.
pub(crate) const MODELS_DELETE_SESSION_INPUT: GoStruct = GoStruct {
    go_type: "models.DeleteSessionInput",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SessionID", tag: "session_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.DeleteSessionRequest`.
pub(crate) const MODELS_DELETE_SESSION_REQUEST: GoStruct = GoStruct {
    go_type: "models.DeleteSessionRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.DeleteSessionInput", kind: GoKind::Struct("models.DeleteSessionInput") },
    ],
};

/// Shape of `models.DeleteSessionResponse`.
pub(crate) const MODELS_DELETE_SESSION_RESPONSE: GoStruct = GoStruct {
    go_type: "models.DeleteSessionResponse",
    fields: &[
        GoField { name: "Output", tag: "output", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.GetSessionInput`.
pub(crate) const MODELS_GET_SESSION_INPUT: GoStruct = GoStruct {
    go_type: "models.GetSessionInput",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SessionID", tag: "session_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.GetSessionRequest`.
pub(crate) const MODELS_GET_SESSION_REQUEST: GoStruct = GoStruct {
    go_type: "models.GetSessionRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.GetSessionInput", kind: GoKind::Struct("models.GetSessionInput") },
    ],
};

/// Shape of `models.GetSessionResponse`.
pub(crate) const MODELS_GET_SESSION_RESPONSE: GoStruct = GoStruct {
    go_type: "models.GetSessionResponse",
    fields: &[
        GoField { name: "Output", tag: "output", embedded: false, exported: true, go_type: "models.SessionData", kind: GoKind::Struct("models.SessionData") },
    ],
};

/// Shape of `models.ListSessionInput`.
pub(crate) const MODELS_LIST_SESSION_INPUT: GoStruct = GoStruct {
    go_type: "models.ListSessionInput",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.ListSessionRequest`.
pub(crate) const MODELS_LIST_SESSION_REQUEST: GoStruct = GoStruct {
    go_type: "models.ListSessionRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.ListSessionInput", kind: GoKind::Struct("models.ListSessionInput") },
    ],
};

/// Shape of `models.ListSessionResponse`.
pub(crate) const MODELS_LIST_SESSION_RESPONSE: GoStruct = GoStruct {
    go_type: "models.ListSessionResponse",
    fields: &[
        GoField { name: "Output", tag: "output", embedded: false, exported: true, go_type: "models.Sessions", kind: GoKind::Struct("models.Sessions") },
    ],
};

/// Shape of `models.Query`.
pub(crate) const MODELS_QUERY: GoStruct = GoStruct {
    go_type: "models.Query",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "interface {}", kind: GoKind::Interface },
    ],
};

/// Shape of `models.SessionData`.
pub(crate) const MODELS_SESSION_DATA: GoStruct = GoStruct {
    go_type: "models.SessionData",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "LastUpdateTime", tag: "last_update_time", embedded: false, exported: true, go_type: "float64", kind: GoKind::Float64 },
        GoField { name: "AppName", tag: "app_name", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ID", tag: "id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "State", tag: "state", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
        GoField { name: "Events", tag: "events", embedded: false, exported: true, go_type: "[]session.Event", kind: GoKind::Slice(&GoKind::Struct("session.Event")) },
    ],
};

/// Shape of `models.Sessions`.
pub(crate) const MODELS_SESSIONS: GoStruct = GoStruct {
    go_type: "models.Sessions",
    fields: &[
        GoField { name: "Sessions", tag: "sessions", embedded: false, exported: true, go_type: "[]models.SessionData", kind: GoKind::Slice(&GoKind::Struct("models.SessionData")) },
    ],
};

/// Shape of `models.StreamQueryInput`.
pub(crate) const MODELS_STREAM_QUERY_INPUT: GoStruct = GoStruct {
    go_type: "models.StreamQueryInput",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SessionID", tag: "session_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Message", tag: "message", embedded: false, exported: true, go_type: "genai.Content", kind: GoKind::Struct("genai.Content") },
    ],
};

/// Shape of `models.StreamQueryRequest`.
pub(crate) const MODELS_STREAM_QUERY_REQUEST: GoStruct = GoStruct {
    go_type: "models.StreamQueryRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.StreamQueryInput", kind: GoKind::Struct("models.StreamQueryInput") },
    ],
};

/// Shape of `models.StreamQueryTextInput`.
pub(crate) const MODELS_STREAM_QUERY_TEXT_INPUT: GoStruct = GoStruct {
    go_type: "models.StreamQueryTextInput",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SessionID", tag: "session_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Message", tag: "message", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.StreamQueryTextRequest`.
pub(crate) const MODELS_STREAM_QUERY_TEXT_REQUEST: GoStruct = GoStruct {
    go_type: "models.StreamQueryTextRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.StreamQueryTextInput", kind: GoKind::Struct("models.StreamQueryTextInput") },
    ],
};

/// Shape of `models.StreamingAgentRunWithEventsInput`.
pub(crate) const MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_INPUT: GoStruct = GoStruct {
    go_type: "models.StreamingAgentRunWithEventsInput",
    fields: &[
        GoField { name: "RequestJSON", tag: "request_json", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.StreamingAgentRunWithEventsRequest`.
pub(crate) const MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_REQUEST: GoStruct = GoStruct {
    go_type: "models.StreamingAgentRunWithEventsRequest",
    fields: &[
        GoField { name: "ClassMethod", tag: "class_method", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Input", tag: "input", embedded: false, exported: true, go_type: "models.StreamingAgentRunWithEventsInput", kind: GoKind::Struct("models.StreamingAgentRunWithEventsInput") },
    ],
};

/// Shape of `models.StreamingAgentRunWithEventsResponse`.
pub(crate) const MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE: GoStruct = GoStruct {
    go_type: "models.StreamingAgentRunWithEventsResponse",
    fields: &[
        GoField { name: "Events", tag: "events,omitempty", embedded: false, exported: true, go_type: "[]*session.Event", kind: GoKind::Slice(&GoKind::Pointer(&GoKind::Struct("session.Event"))) },
        GoField { name: "Artifacts", tag: "artifacts,omitempty", embedded: false, exported: true, go_type: "[]interface {}", kind: GoKind::Slice(&GoKind::Interface) },
        GoField { name: "SessionID", tag: "session_id,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
    ],
};

/// Shape of `models.StreamingAgentRunWithEventsRunRequest`.
pub(crate) const MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RUN_REQUEST: GoStruct = GoStruct {
    go_type: "models.StreamingAgentRunWithEventsRunRequest",
    fields: &[
        GoField { name: "UserID", tag: "user_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "SessionID", tag: "session_id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Message", tag: "message", embedded: false, exported: true, go_type: "genai.Content", kind: GoKind::Struct("genai.Content") },
    ],
};

/// Shape of `session.Event`.
pub(crate) const SESSION_EVENT: GoStruct = GoStruct {
    go_type: "session.Event",
    fields: &[
        GoField { name: "LLMResponse", tag: "", embedded: true, exported: true, go_type: "model.LLMResponse", kind: GoKind::Struct("model.LLMResponse") },
        GoField { name: "ID", tag: "id", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Timestamp", tag: "timestamp", embedded: false, exported: true, go_type: "time.Time", kind: GoKind::Time },
        GoField { name: "InvocationID", tag: "invocationId", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Branch", tag: "branch,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "IsolationScope", tag: "isolationScope,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Author", tag: "author", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Actions", tag: "actions", embedded: false, exported: true, go_type: "session.EventActions", kind: GoKind::Struct("session.EventActions") },
        GoField { name: "LongRunningToolIDs", tag: "longRunningToolIds,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "Routes", tag: "routes,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "RequestedInput", tag: "requestedInput,omitempty", embedded: false, exported: true, go_type: "*session.RequestInput", kind: GoKind::Pointer(&GoKind::Struct("session.RequestInput")) },
        GoField { name: "Output", tag: "output,omitempty", embedded: false, exported: true, go_type: "interface {}", kind: GoKind::Interface },
        GoField { name: "NodeInfo", tag: "nodeInfo,omitempty", embedded: false, exported: true, go_type: "*session.NodeInfo", kind: GoKind::Pointer(&GoKind::Struct("session.NodeInfo")) },
    ],
};

/// Shape of `session.EventActions`.
pub(crate) const SESSION_EVENT_ACTIONS: GoStruct = GoStruct {
    go_type: "session.EventActions",
    fields: &[
        GoField { name: "StateDelta", tag: "stateDelta", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
        GoField { name: "ArtifactDelta", tag: "artifactDelta", embedded: false, exported: true, go_type: "map[string]int64", kind: GoKind::Map(&GoKind::Int) },
        GoField { name: "RequestedToolConfirmations", tag: "requestedToolConfirmations,omitempty", embedded: false, exported: true, go_type: "map[string]toolconfirmation.ToolConfirmation", kind: GoKind::Map(&GoKind::Struct("toolconfirmation.ToolConfirmation")) },
        GoField { name: "RequestedAuthConfigs", tag: "requestedAuthConfigs,omitempty", embedded: false, exported: true, go_type: "map[string]toolauth.Config", kind: GoKind::Map(&GoKind::Struct("toolauth.Config")) },
        GoField { name: "SkipSummarization", tag: "skipSummarization,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "TransferToAgent", tag: "transferToAgent,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Escalate", tag: "escalate,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "Compaction", tag: "compaction,omitempty", embedded: false, exported: true, go_type: "*session.EventCompaction", kind: GoKind::Pointer(&GoKind::Struct("session.EventCompaction")) },
    ],
};

/// Shape of `session.EventCompaction`.
pub(crate) const SESSION_EVENT_COMPACTION: GoStruct = GoStruct {
    go_type: "session.EventCompaction",
    fields: &[
        GoField { name: "StartTimestamp", tag: "startTimestamp", embedded: false, exported: true, go_type: "time.Time", kind: GoKind::Time },
        GoField { name: "EndTimestamp", tag: "endTimestamp", embedded: false, exported: true, go_type: "time.Time", kind: GoKind::Time },
        GoField { name: "CompactedContent", tag: "compactedContent", embedded: false, exported: true, go_type: "*genai.Content", kind: GoKind::Pointer(&GoKind::Struct("genai.Content")) },
        GoField { name: "ExcludedEvents", tag: "excludedEvents,omitempty", embedded: false, exported: true, go_type: "[]session.EventRef", kind: GoKind::Slice(&GoKind::Struct("session.EventRef")) },
    ],
};

/// Shape of `session.EventRef`.
pub(crate) const SESSION_EVENT_REF: GoStruct = GoStruct {
    go_type: "session.EventRef",
    fields: &[
        GoField { name: "InvocationID", tag: "invocationId", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Timestamp", tag: "timestamp", embedded: false, exported: true, go_type: "time.Time", kind: GoKind::Time },
    ],
};

/// Shape of `session.NodeInfo`.
pub(crate) const SESSION_NODE_INFO: GoStruct = GoStruct {
    go_type: "session.NodeInfo",
    fields: &[
        GoField { name: "Path", tag: "path,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "MessageAsOutput", tag: "messageAsOutput,omitempty", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "OutputFor", tag: "outputFor,omitempty", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
    ],
};

/// Shape of `session.RequestInput`.
pub(crate) const SESSION_REQUEST_INPUT: GoStruct = GoStruct {
    go_type: "session.RequestInput",
    fields: &[
        GoField { name: "InterruptID", tag: "interruptId", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Message", tag: "message,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ResponseSchema", tag: "responseSchema,omitempty", embedded: false, exported: true, go_type: "*jsonschema.Schema", kind: GoKind::Pointer(&GoKind::Struct("jsonschema.Schema")) },
        GoField { name: "Payload", tag: "payload,omitempty", embedded: false, exported: true, go_type: "interface {}", kind: GoKind::Interface },
    ],
};

/// Shape of `toolauth.Config`.
pub(crate) const TOOLAUTH_CONFIG: GoStruct = GoStruct {
    go_type: "toolauth.Config",
    fields: &[
        GoField { name: "AuthScheme", tag: "authScheme", embedded: false, exported: true, go_type: "toolauth.Scheme", kind: GoKind::Struct("toolauth.Scheme") },
        GoField { name: "RawAuthCredential", tag: "rawAuthCredential,omitempty", embedded: false, exported: true, go_type: "*toolauth.Credential", kind: GoKind::Pointer(&GoKind::Struct("toolauth.Credential")) },
        GoField { name: "ExchangedAuthCredential", tag: "exchangedAuthCredential,omitempty", embedded: false, exported: true, go_type: "*toolauth.Credential", kind: GoKind::Pointer(&GoKind::Struct("toolauth.Credential")) },
        GoField { name: "CredentialKey", tag: "credentialKey,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.Credential`.
pub(crate) const TOOLAUTH_CREDENTIAL: GoStruct = GoStruct {
    go_type: "toolauth.Credential",
    fields: &[
        GoField { name: "AuthType", tag: "authType", embedded: false, exported: true, go_type: "toolauth.CredentialType", kind: GoKind::NamedString },
        GoField { name: "ResourceRef", tag: "resourceRef,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "APIKey", tag: "apiKey,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "HTTP", tag: "http,omitempty", embedded: false, exported: true, go_type: "*toolauth.HTTPAuth", kind: GoKind::Pointer(&GoKind::Struct("toolauth.HTTPAuth")) },
        GoField { name: "ServiceAccount", tag: "serviceAccount,omitempty", embedded: false, exported: true, go_type: "*toolauth.ServiceAccount", kind: GoKind::Pointer(&GoKind::Struct("toolauth.ServiceAccount")) },
        GoField { name: "OAuth2", tag: "oauth2,omitempty", embedded: false, exported: true, go_type: "*toolauth.OAuth2Auth", kind: GoKind::Pointer(&GoKind::Struct("toolauth.OAuth2Auth")) },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.HTTPAuth`.
pub(crate) const TOOLAUTH_HTTP_AUTH: GoStruct = GoStruct {
    go_type: "toolauth.HTTPAuth",
    fields: &[
        GoField { name: "Scheme", tag: "scheme", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Credentials", tag: "credentials", embedded: false, exported: true, go_type: "toolauth.HTTPCredentials", kind: GoKind::Struct("toolauth.HTTPCredentials") },
        GoField { name: "AdditionalHeaders", tag: "additionalHeaders,omitzero", embedded: false, exported: true, go_type: "map[string]string", kind: GoKind::Map(&GoKind::String) },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.HTTPCredentials`.
pub(crate) const TOOLAUTH_HTTP_CREDENTIALS: GoStruct = GoStruct {
    go_type: "toolauth.HTTPCredentials",
    fields: &[
        GoField { name: "Username", tag: "username,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Password", tag: "password,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Token", tag: "token,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.OAuth2Auth`.
pub(crate) const TOOLAUTH_O_AUTH2_AUTH: GoStruct = GoStruct {
    go_type: "toolauth.OAuth2Auth",
    fields: &[
        GoField { name: "ClientID", tag: "clientId,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ClientSecret", tag: "clientSecret,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AuthURI", tag: "authUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Nonce", tag: "nonce,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "State", tag: "state,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RedirectURI", tag: "redirectUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AuthResponseURI", tag: "authResponseUri,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AuthCode", tag: "authCode,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AccessToken", tag: "accessToken,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RefreshToken", tag: "refreshToken,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "IDToken", tag: "idToken,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ExpiresAt", tag: "expiresAt,omitempty", embedded: false, exported: true, go_type: "int64", kind: GoKind::Int },
        GoField { name: "ExpiresIn", tag: "expiresIn,omitempty", embedded: false, exported: true, go_type: "int64", kind: GoKind::Int },
        GoField { name: "Audience", tag: "audience,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Prompt", tag: "prompt,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "CodeVerifier", tag: "codeVerifier,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "CodeChallengeMethod", tag: "codeChallengeMethod,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "TokenEndpointAuthMethod", tag: "tokenEndpointAuthMethod,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.OAuthFlow`.
pub(crate) const TOOLAUTH_O_AUTH_FLOW: GoStruct = GoStruct {
    go_type: "toolauth.OAuthFlow",
    fields: &[
        GoField { name: "AuthorizationURL", tag: "authorizationUrl,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "TokenURL", tag: "tokenUrl,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RefreshURL", tag: "refreshUrl,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Scopes", tag: "scopes", embedded: false, exported: true, go_type: "map[string]string", kind: GoKind::Map(&GoKind::String) },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.OAuthFlows`.
pub(crate) const TOOLAUTH_O_AUTH_FLOWS: GoStruct = GoStruct {
    go_type: "toolauth.OAuthFlows",
    fields: &[
        GoField { name: "Implicit", tag: "implicit,omitempty", embedded: false, exported: true, go_type: "*toolauth.OAuthFlow", kind: GoKind::Pointer(&GoKind::Struct("toolauth.OAuthFlow")) },
        GoField { name: "Password", tag: "password,omitempty", embedded: false, exported: true, go_type: "*toolauth.OAuthFlow", kind: GoKind::Pointer(&GoKind::Struct("toolauth.OAuthFlow")) },
        GoField { name: "ClientCredentials", tag: "clientCredentials,omitempty", embedded: false, exported: true, go_type: "*toolauth.OAuthFlow", kind: GoKind::Pointer(&GoKind::Struct("toolauth.OAuthFlow")) },
        GoField { name: "AuthorizationCode", tag: "authorizationCode,omitempty", embedded: false, exported: true, go_type: "*toolauth.OAuthFlow", kind: GoKind::Pointer(&GoKind::Struct("toolauth.OAuthFlow")) },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.Scheme`.
pub(crate) const TOOLAUTH_SCHEME: GoStruct = GoStruct {
    go_type: "toolauth.Scheme",
    fields: &[
        GoField { name: "Type", tag: "type", embedded: false, exported: true, go_type: "toolauth.SchemeType", kind: GoKind::NamedString },
        GoField { name: "Description", tag: "description,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "In", tag: "in,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Name", tag: "name,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Scheme", tag: "scheme,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "BearerFormat", tag: "bearerFormat,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Flows", tag: "flows,omitempty", embedded: false, exported: true, go_type: "*toolauth.OAuthFlows", kind: GoKind::Pointer(&GoKind::Struct("toolauth.OAuthFlows")) },
        GoField { name: "IssuerURL", tag: "issuer_url,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "OpenIDConnectURL", tag: "openIdConnectUrl,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AuthorizationEndpoint", tag: "authorization_endpoint,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "TokenEndpoint", tag: "token_endpoint,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "UserinfoEndpoint", tag: "userinfo_endpoint,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "RevocationEndpoint", tag: "revocation_endpoint,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "TokenEndpointAuthMethodsSupported", tag: "token_endpoint_auth_methods_supported,omitzero", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "GrantTypesSupported", tag: "grant_types_supported,omitzero", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "Scopes", tag: "scopes,omitzero", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.ServiceAccount`.
pub(crate) const TOOLAUTH_SERVICE_ACCOUNT: GoStruct = GoStruct {
    go_type: "toolauth.ServiceAccount",
    fields: &[
        GoField { name: "ServiceAccountCredential", tag: "serviceAccountCredential,omitempty", embedded: false, exported: true, go_type: "*toolauth.ServiceAccountCredential", kind: GoKind::Pointer(&GoKind::Struct("toolauth.ServiceAccountCredential")) },
        GoField { name: "Scopes", tag: "scopes,omitzero", embedded: false, exported: true, go_type: "[]string", kind: GoKind::Slice(&GoKind::String) },
        GoField { name: "UseDefaultCredential", tag: "useDefaultCredential", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "UseIDToken", tag: "useIdToken", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "Audience", tag: "audience,omitempty", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolauth.ServiceAccountCredential`.
pub(crate) const TOOLAUTH_SERVICE_ACCOUNT_CREDENTIAL: GoStruct = GoStruct {
    go_type: "toolauth.ServiceAccountCredential",
    fields: &[
        GoField { name: "Type", tag: "type", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ProjectID", tag: "projectId", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PrivateKeyID", tag: "privateKeyId", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "PrivateKey", tag: "privateKey", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ClientEmail", tag: "clientEmail", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ClientID", tag: "clientId", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AuthURI", tag: "authUri", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "TokenURI", tag: "tokenUri", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "AuthProviderX509CertURL", tag: "authProviderX509CertUrl", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "ClientX509CertURL", tag: "clientX509CertUrl", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "UniverseDomain", tag: "universeDomain", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Extra", tag: "-", embedded: false, exported: true, go_type: "map[string]interface {}", kind: GoKind::Map(&GoKind::Interface) },
    ],
};

/// Shape of `toolconfirmation.ToolConfirmation`.
pub(crate) const TOOLCONFIRMATION_TOOL_CONFIRMATION: GoStruct = GoStruct {
    go_type: "toolconfirmation.ToolConfirmation",
    fields: &[
        GoField { name: "Hint", tag: "hint", embedded: false, exported: true, go_type: "string", kind: GoKind::String },
        GoField { name: "Confirmed", tag: "confirmed", embedded: false, exported: true, go_type: "bool", kind: GoKind::Bool },
        GoField { name: "Payload", tag: "payload", embedded: false, exported: true, go_type: "interface {}", kind: GoKind::Interface },
    ],
};

/// Every shape, sorted by `go_type`.
pub(crate) const ALL: &[&GoStruct] = &[
    &CIVIL_DATE,
    &GENAI_BLOB,
    &GENAI_CITATION,
    &GENAI_CITATION_METADATA,
    &GENAI_CODE_EXECUTION_RESULT,
    &GENAI_CONTENT,
    &GENAI_EXECUTABLE_CODE,
    &GENAI_FILE_DATA,
    &GENAI_FUNCTION_CALL,
    &GENAI_FUNCTION_RESPONSE,
    &GENAI_FUNCTION_RESPONSE_BLOB,
    &GENAI_FUNCTION_RESPONSE_FILE_DATA,
    &GENAI_FUNCTION_RESPONSE_PART,
    &GENAI_GENERATE_CONTENT_RESPONSE_USAGE_METADATA,
    &GENAI_GROUNDING_CHUNK,
    &GENAI_GROUNDING_CHUNK_CUSTOM_METADATA,
    &GENAI_GROUNDING_CHUNK_IMAGE,
    &GENAI_GROUNDING_CHUNK_MAPS,
    &GENAI_GROUNDING_CHUNK_MAPS_PLACE_ANSWER_SOURCES,
    &GENAI_GROUNDING_CHUNK_MAPS_PLACE_ANSWER_SOURCES_AUTHOR_ATTRIBUTION,
    &GENAI_GROUNDING_CHUNK_MAPS_PLACE_ANSWER_SOURCES_REVIEW_SNIPPET,
    &GENAI_GROUNDING_CHUNK_MAPS_ROUTE,
    &GENAI_GROUNDING_CHUNK_RETRIEVED_CONTEXT,
    &GENAI_GROUNDING_CHUNK_STRING_LIST,
    &GENAI_GROUNDING_CHUNK_WEB,
    &GENAI_GROUNDING_METADATA,
    &GENAI_GROUNDING_METADATA_SOURCE_FLAGGING_URI,
    &GENAI_GROUNDING_SUPPORT,
    &GENAI_LOGPROBS_RESULT,
    &GENAI_LOGPROBS_RESULT_CANDIDATE,
    &GENAI_LOGPROBS_RESULT_TOP_CANDIDATES,
    &GENAI_MODALITY_TOKEN_COUNT,
    &GENAI_PART,
    &GENAI_PART_MEDIA_RESOLUTION,
    &GENAI_PARTIAL_ARG,
    &GENAI_RAG_CHUNK,
    &GENAI_RAG_CHUNK_PAGE_SPAN,
    &GENAI_RETRIEVAL_METADATA,
    &GENAI_SEARCH_ENTRY_POINT,
    &GENAI_SEGMENT,
    &GENAI_SPEECH_METADATA,
    &GENAI_TOOL_CALL,
    &GENAI_TOOL_RESPONSE,
    &GENAI_TRANSCRIPTION,
    &GENAI_VIDEO_METADATA,
    &GENAI_WORD_INFO,
    &JSONSCHEMA_SCHEMA,
    &MODEL_LLM_RESPONSE,
    &MODELS_CREATE_SESSION_INPUT,
    &MODELS_CREATE_SESSION_REQUEST,
    &MODELS_CREATE_SESSION_RESPONSE,
    &MODELS_DELETE_SESSION_INPUT,
    &MODELS_DELETE_SESSION_REQUEST,
    &MODELS_DELETE_SESSION_RESPONSE,
    &MODELS_GET_SESSION_INPUT,
    &MODELS_GET_SESSION_REQUEST,
    &MODELS_GET_SESSION_RESPONSE,
    &MODELS_LIST_SESSION_INPUT,
    &MODELS_LIST_SESSION_REQUEST,
    &MODELS_LIST_SESSION_RESPONSE,
    &MODELS_QUERY,
    &MODELS_SESSION_DATA,
    &MODELS_SESSIONS,
    &MODELS_STREAM_QUERY_INPUT,
    &MODELS_STREAM_QUERY_REQUEST,
    &MODELS_STREAM_QUERY_TEXT_INPUT,
    &MODELS_STREAM_QUERY_TEXT_REQUEST,
    &MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_INPUT,
    &MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_REQUEST,
    &MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE,
    &MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RUN_REQUEST,
    &SESSION_EVENT,
    &SESSION_EVENT_ACTIONS,
    &SESSION_EVENT_COMPACTION,
    &SESSION_EVENT_REF,
    &SESSION_NODE_INFO,
    &SESSION_REQUEST_INPUT,
    &TOOLAUTH_CONFIG,
    &TOOLAUTH_CREDENTIAL,
    &TOOLAUTH_HTTP_AUTH,
    &TOOLAUTH_HTTP_CREDENTIALS,
    &TOOLAUTH_O_AUTH2_AUTH,
    &TOOLAUTH_O_AUTH_FLOW,
    &TOOLAUTH_O_AUTH_FLOWS,
    &TOOLAUTH_SCHEME,
    &TOOLAUTH_SERVICE_ACCOUNT,
    &TOOLAUTH_SERVICE_ACCOUNT_CREDENTIAL,
    &TOOLCONFIRMATION_TOOL_CONFIRMATION,
];
