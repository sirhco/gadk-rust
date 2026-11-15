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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Every `gadk_genai::` path listed in `docs/parity/genai-inventory.md` exists
//! (generated from that table).

#[test]
fn inventory_types_exist() {
    let _: Option<gadk_genai::ActivityEnd> = None;
    let _: Option<gadk_genai::ActivityStart> = None;
    let _: Option<gadk_genai::AudioTranscriptionConfig> = None;
    let _: Option<gadk_genai::AutomaticActivityDetection> = None;
    let _: Option<gadk_genai::Blob> = None;
    let _: Option<gadk_genai::BlockedReason> = None;
    let _: Option<gadk_genai::Candidate> = None;
    let _: Option<gadk_genai::Citation> = None;
    let _: Option<gadk_genai::CitationMetadata> = None;
    let _: Option<gadk_genai::CodeExecutionResult> = None;
    let _: Option<gadk_genai::ComputerUse> = None;
    let _: Option<gadk_genai::Content> = None;
    let _: Option<gadk_genai::EnterpriseWebSearch> = None;
    let _: Option<gadk_genai::ExecutableCode> = None;
    let _: Option<gadk_genai::ExternalAPI> = None;
    let _: Option<gadk_genai::File> = None;
    let _: Option<gadk_genai::FileData> = None;
    let _: Option<gadk_genai::FileSearch> = None;
    let _: Option<gadk_genai::FinishReason> = None;
    let _: Option<gadk_genai::FunctionCall> = None;
    let _: Option<gadk_genai::FunctionCallingConfig> = None;
    let _: Option<gadk_genai::FunctionCallingConfigMode> = None;
    let _: Option<gadk_genai::FunctionDeclaration> = None;
    let _: Option<gadk_genai::FunctionResponse> = None;
    let _: Option<gadk_genai::FunctionResponseBlob> = None;
    let _: Option<gadk_genai::FunctionResponseFileData> = None;
    let _: Option<gadk_genai::FunctionResponsePart> = None;
    let _: Option<gadk_genai::GenerateContentConfig> = None;
    let _: Option<gadk_genai::GenerateContentResponse> = None;
    let _: Option<gadk_genai::GenerateContentResponsePromptFeedback> = None;
    let _: Option<gadk_genai::GenerateContentResponseUsageMetadata> = None;
    let _: Option<gadk_genai::GeneratedImage> = None;
    let _: Option<gadk_genai::GenerateImagesConfig> = None;
    let _: Option<gadk_genai::GenerateImagesResponse> = None;
    let _: Option<gadk_genai::GenerationConfigRoutingConfig> = None;
    let _: Option<gadk_genai::GenerationOutputConfig> = None;
    let _: Option<gadk_genai::GoogleMaps> = None;
    let _: Option<gadk_genai::GoogleSearch> = None;
    let _: Option<gadk_genai::GoogleSearchRetrieval> = None;
    let _: Option<gadk_genai::GroundingChunk> = None;
    let _: Option<gadk_genai::GroundingChunkMaps> = None;
    let _: Option<gadk_genai::GroundingChunkMapsPlaceAnswerSources> = None;
    let _: Option<gadk_genai::GroundingChunkMapsPlaceAnswerSourcesReviewSnippet> = None;
    let _: Option<gadk_genai::GroundingChunkRetrievedContext> = None;
    let _: Option<gadk_genai::GroundingChunkWeb> = None;
    let _: Option<gadk_genai::GroundingMetadata> = None;
    let _: Option<gadk_genai::GroundingMetadataSourceFlaggingURI> = None;
    let _: Option<gadk_genai::GroundingSupport> = None;
    let _: Option<gadk_genai::Image> = None;
    let _: Option<gadk_genai::ImageConfig> = None;
    let _: Option<gadk_genai::Language> = None;
    let _: Option<gadk_genai::LiveClientContentInput> = None;
    let _: Option<gadk_genai::LiveConnectConfig> = None;
    let _: Option<gadk_genai::LiveRealtimeInput> = None;
    let _: Option<gadk_genai::LiveToolResponseInput> = None;
    let _: Option<gadk_genai::LogprobsResult> = None;
    let _: Option<gadk_genai::LogprobsResultCandidate> = None;
    let _: Option<gadk_genai::LogprobsResultTopCandidates> = None;
    let _: Option<gadk_genai::MCPServer> = None;
    let _: Option<gadk_genai::Modality> = None;
    let _: Option<gadk_genai::ModalityTokenCount> = None;
    let _: Option<gadk_genai::ModelArmorConfig> = None;
    let _: Option<gadk_genai::ModelSelectionConfig> = None;
    let _: Option<gadk_genai::Outcome> = None;
    let _: Option<gadk_genai::Part> = None;
    let _: Option<gadk_genai::PartialArg> = None;
    let _: Option<gadk_genai::PartMediaResolution> = None;
    let _: Option<gadk_genai::ProactivityConfig> = None;
    let _: Option<gadk_genai::RAGChunk> = None;
    let _: Option<gadk_genai::RAGChunkPageSpan> = None;
    let _: Option<gadk_genai::RealtimeInputConfig> = None;
    let _: Option<gadk_genai::ResourceScope> = None;
    let _: Option<gadk_genai::Retrieval> = None;
    let _: Option<gadk_genai::RetrievalMetadata> = None;
    let _: Option<gadk_genai::SafetySetting> = None;
    let _: Option<gadk_genai::Schema> = None;
    let _: Option<gadk_genai::SearchEntryPoint> = None;
    let _: Option<gadk_genai::Segment> = None;
    let _: Option<gadk_genai::ServiceTier> = None;
    let _: Option<gadk_genai::SessionResumptionConfig> = None;
    let _: Option<gadk_genai::SpeechConfig> = None;
    let _: Option<gadk_genai::SpeechMetadata> = None;
    let _: Option<gadk_genai::ThinkingConfig> = None;
    let _: Option<gadk_genai::ThinkingLevel> = None;
    let _: Option<gadk_genai::Tool> = None;
    let _: Option<gadk_genai::ToolCall> = None;
    let _: Option<gadk_genai::ToolCodeExecution> = None;
    let _: Option<gadk_genai::ToolConfig> = None;
    let _: Option<gadk_genai::ToolExaAISearch> = None;
    let _: Option<gadk_genai::ToolParallelAISearch> = None;
    let _: Option<gadk_genai::ToolResponse> = None;
    let _: Option<gadk_genai::Transcription> = None;
    let _: Option<gadk_genai::Type> = None;
    let _: Option<gadk_genai::URLContext> = None;
    let _: Option<gadk_genai::UsageMetadata> = None;
    let _: Option<gadk_genai::VertexRAGStore> = None;
    let _: Option<gadk_genai::VertexRAGStoreRAGResource> = None;
    let _: Option<gadk_genai::VideoMetadata> = None;
}

#[test]
fn inventory_functions_and_consts_exist() {
    let _ = gadk_genai::Content::from_bytes;
    let _ = gadk_genai::Content::from_code_execution_result;
    let _ = gadk_genai::Content::from_executable_code;
    let _ = gadk_genai::Content::from_function_call;
    let _ = gadk_genai::Content::from_function_response;
    let _ = gadk_genai::Content::from_parts;
    let _ = gadk_genai::Content::from_text("x", "user");
    let _ = gadk_genai::Content::from_uri;
    let _ = gadk_genai::Part::from_bytes;
    let _ = gadk_genai::Part::from_code_execution_result;
    let _ = gadk_genai::Part::from_executable_code;
    let _ = gadk_genai::Part::from_file;
    let _ = gadk_genai::Part::from_function_call;
    let _ = gadk_genai::Part::from_function_response;
    let _ = gadk_genai::Part::from_text("x");
    let _ = gadk_genai::Part::from_uri;
    let _ = gadk_genai::ROLE_MODEL;
    let _ = gadk_genai::ROLE_USER;
    let _ = gadk_genai::text("x");
}

#[test]
fn inventory_enum_values_serialize_as_listed() {
    assert_eq!(gadk_genai::BlockedReason::Blocklist.as_str(), "BLOCKLIST");
    assert_eq!(
        gadk_genai::BlockedReason::ImageSafety.as_str(),
        "IMAGE_SAFETY"
    );
    assert_eq!(gadk_genai::BlockedReason::Jailbreak.as_str(), "JAILBREAK");
    assert_eq!(
        gadk_genai::BlockedReason::ModelArmor.as_str(),
        "MODEL_ARMOR"
    );
    assert_eq!(gadk_genai::BlockedReason::Other.as_str(), "OTHER");
    assert_eq!(
        gadk_genai::BlockedReason::ProhibitedContent.as_str(),
        "PROHIBITED_CONTENT"
    );
    assert_eq!(gadk_genai::BlockedReason::Safety.as_str(), "SAFETY");
    assert_eq!(
        gadk_genai::BlockedReason::Unspecified.as_str(),
        "BLOCKED_REASON_UNSPECIFIED"
    );
    assert_eq!(gadk_genai::Behavior::NonBlocking.as_str(), "NON_BLOCKING");
    assert_eq!(
        gadk_genai::EndSensitivity::High.as_str(),
        "END_SENSITIVITY_HIGH"
    );
    assert_eq!(gadk_genai::FinishReason::Blocklist.as_str(), "BLOCKLIST");
    assert_eq!(
        gadk_genai::FinishReason::Continuation.as_str(),
        "CONTINUATION"
    );
    assert_eq!(gadk_genai::FinishReason::ImageOther.as_str(), "IMAGE_OTHER");
    assert_eq!(
        gadk_genai::FinishReason::ImageProhibitedContent.as_str(),
        "IMAGE_PROHIBITED_CONTENT"
    );
    assert_eq!(
        gadk_genai::FinishReason::ImageRecitation.as_str(),
        "IMAGE_RECITATION"
    );
    assert_eq!(
        gadk_genai::FinishReason::ImageSafety.as_str(),
        "IMAGE_SAFETY"
    );
    assert_eq!(gadk_genai::FinishReason::Language.as_str(), "LANGUAGE");
    assert_eq!(
        gadk_genai::FinishReason::MalformedFunctionCall.as_str(),
        "MALFORMED_FUNCTION_CALL"
    );
    assert_eq!(gadk_genai::FinishReason::MaxTokens.as_str(), "MAX_TOKENS");
    assert_eq!(gadk_genai::FinishReason::NoImage.as_str(), "NO_IMAGE");
    assert_eq!(gadk_genai::FinishReason::Other.as_str(), "OTHER");
    assert_eq!(
        gadk_genai::FinishReason::ProhibitedContent.as_str(),
        "PROHIBITED_CONTENT"
    );
    assert_eq!(gadk_genai::FinishReason::Recitation.as_str(), "RECITATION");
    assert_eq!(gadk_genai::FinishReason::Safety.as_str(), "SAFETY");
    assert_eq!(gadk_genai::FinishReason::Spii.as_str(), "SPII");
    assert_eq!(gadk_genai::FinishReason::Stop.as_str(), "STOP");
    assert_eq!(
        gadk_genai::FinishReason::TooManyToolCalls.as_str(),
        "TOO_MANY_TOOL_CALLS"
    );
    assert_eq!(
        gadk_genai::FinishReason::UnexpectedToolCall.as_str(),
        "UNEXPECTED_TOOL_CALL"
    );
    assert_eq!(
        gadk_genai::FinishReason::Unspecified.as_str(),
        "FINISH_REASON_UNSPECIFIED"
    );
    assert_eq!(gadk_genai::FunctionCallingConfigMode::Any.as_str(), "ANY");
    assert_eq!(gadk_genai::FunctionCallingConfigMode::Auto.as_str(), "AUTO");
    assert_eq!(gadk_genai::FunctionCallingConfigMode::None.as_str(), "NONE");
    assert_eq!(
        gadk_genai::FunctionCallingConfigMode::Unspecified.as_str(),
        "MODE_UNSPECIFIED"
    );
    assert_eq!(
        gadk_genai::FunctionCallingConfigMode::Validated.as_str(),
        "VALIDATED"
    );
    assert_eq!(
        gadk_genai::FunctionResponseScheduling::Interrupt.as_str(),
        "INTERRUPT"
    );
    assert_eq!(
        gadk_genai::FunctionResponseScheduling::WhenIdle.as_str(),
        "WHEN_IDLE"
    );
    assert_eq!(
        gadk_genai::HarmBlockThreshold::BlockLowAndAbove.as_str(),
        "BLOCK_LOW_AND_ABOVE"
    );
    assert_eq!(
        gadk_genai::HarmBlockThreshold::BlockNone.as_str(),
        "BLOCK_NONE"
    );
    assert_eq!(
        gadk_genai::HarmCategory::Harassment.as_str(),
        "HARM_CATEGORY_HARASSMENT"
    );
    assert_eq!(
        gadk_genai::HarmCategory::HateSpeech.as_str(),
        "HARM_CATEGORY_HATE_SPEECH"
    );
    assert_eq!(gadk_genai::Language::Python.as_str(), "PYTHON");
    assert_eq!(
        gadk_genai::Language::Unspecified.as_str(),
        "LANGUAGE_UNSPECIFIED"
    );
    assert_eq!(gadk_genai::MediaModality::Audio.as_str(), "AUDIO");
    assert_eq!(gadk_genai::MediaModality::Text.as_str(), "TEXT");
    assert_eq!(gadk_genai::MediaProcessing::Static.as_str(), "STATIC");
    assert_eq!(
        gadk_genai::MediaResolution::High.as_str(),
        "MEDIA_RESOLUTION_HIGH"
    );
    assert_eq!(
        gadk_genai::MediaResolution::Low.as_str(),
        "MEDIA_RESOLUTION_LOW"
    );
    assert_eq!(gadk_genai::Modality::Audio.as_str(), "AUDIO");
    assert_eq!(gadk_genai::Outcome::Ok.as_str(), "OUTCOME_OK");
    assert_eq!(
        gadk_genai::PartMediaResolutionLevel::MediaResolutionHigh.as_str(),
        "MEDIA_RESOLUTION_HIGH"
    );
    assert_eq!(
        gadk_genai::PartMediaResolutionLevel::MediaResolutionLow.as_str(),
        "MEDIA_RESOLUTION_LOW"
    );
    assert_eq!(gadk_genai::ResourceScope::Collection.as_str(), "COLLECTION");
    assert_eq!(gadk_genai::ServiceTier::Flex.as_str(), "flex");
    assert_eq!(gadk_genai::ServiceTier::Priority.as_str(), "priority");
    assert_eq!(gadk_genai::ServiceTier::Standard.as_str(), "standard");
    assert_eq!(gadk_genai::ServiceTier::Unspecified.as_str(), "unspecified");
    assert_eq!(gadk_genai::ThinkingLevel::High.as_str(), "HIGH");
    assert_eq!(gadk_genai::ThinkingLevel::Low.as_str(), "LOW");
    assert_eq!(gadk_genai::ThinkingLevel::Medium.as_str(), "MEDIUM");
    assert_eq!(gadk_genai::ThinkingLevel::Minimal.as_str(), "MINIMAL");
    assert_eq!(
        gadk_genai::ThinkingLevel::Unspecified.as_str(),
        "THINKING_LEVEL_UNSPECIFIED"
    );
    assert_eq!(gadk_genai::TrafficType::OnDemand.as_str(), "ON_DEMAND");
    assert_eq!(gadk_genai::Type::Array.as_str(), "ARRAY");
    assert_eq!(gadk_genai::Type::Boolean.as_str(), "BOOLEAN");
    assert_eq!(gadk_genai::Type::Integer.as_str(), "INTEGER");
    assert_eq!(gadk_genai::Type::Null.as_str(), "NULL");
    assert_eq!(gadk_genai::Type::Number.as_str(), "NUMBER");
    assert_eq!(gadk_genai::Type::Object.as_str(), "OBJECT");
    assert_eq!(gadk_genai::Type::String.as_str(), "STRING");
    assert_eq!(gadk_genai::Type::Unspecified.as_str(), "TYPE_UNSPECIFIED");
}
