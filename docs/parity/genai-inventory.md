# genai inventory — identifiers referenced by ADK-Go

Generated from `grep -rhoE 'genai\.[A-Z][A-Za-z0-9]+' --include='*.go' ../adk-go | sort -u` against genai v1.75.0.

| genai identifier | kind | Rust path | note |
|---|---|---|---|
| ActivityEnd | struct | gadk_genai::ActivityEnd | |
| ActivityStart | struct | gadk_genai::ActivityStart | |
| APIError | client | gadk_model::ModelError::Api | |
| AudioTranscriptionConfig | struct | gadk_genai::AudioTranscriptionConfig | |
| Backend | client | gadk_model::Backend | |
| BackendGeminiAPI | client | gadk_model::Backend::GeminiApi | |
| BackendUnspecified | client | N/A — `Option<Backend>::None` | |
| BackendVertexAI | client | gadk_model::Backend::VertexAi | |
| Blob | struct | gadk_genai::Blob | |
| BlockedReason | enum | gadk_genai::BlockedReason | string_enum! with Unknown(String) |
| BlockedReasonBlocklist | enum value | gadk_genai::BlockedReason::Blocklist | serializes "BLOCKLIST" |
| BlockedReasonImageSafety | enum value | gadk_genai::BlockedReason::ImageSafety | serializes "IMAGE_SAFETY" |
| BlockedReasonJailbreak | enum value | gadk_genai::BlockedReason::Jailbreak | serializes "JAILBREAK" |
| BlockedReasonModelArmor | enum value | gadk_genai::BlockedReason::ModelArmor | serializes "MODEL_ARMOR" |
| BlockedReasonOther | enum value | gadk_genai::BlockedReason::Other | serializes "OTHER" |
| BlockedReasonProhibitedContent | enum value | gadk_genai::BlockedReason::ProhibitedContent | serializes "PROHIBITED_CONTENT" |
| BlockedReasonSafety | enum value | gadk_genai::BlockedReason::Safety | serializes "SAFETY" |
| BlockedReasonUnspecified | enum value | gadk_genai::BlockedReason::Unspecified | serializes "BLOCKED_REASON_UNSPECIFIED" |
| Candidate | struct | gadk_genai::Candidate | |
| Citation | struct | gadk_genai::Citation | |
| CitationMetadata | struct | gadk_genai::CitationMetadata | |
| Client | client | gadk_gemini::GeminiModel (client folded into model) | |
| ClientConfig | client | gadk_gemini::ClientConfig | |
| CodeExecutionResult | struct | gadk_genai::CodeExecutionResult | |
| ComputerUse | struct | gadk_genai::ComputerUse | |
| Content | struct | gadk_genai::Content | |
| EnterpriseWebSearch | struct | gadk_genai::EnterpriseWebSearch | |
| ExecutableCode | struct | gadk_genai::ExecutableCode | |
| ExternalAPI | struct | gadk_genai::ExternalAPI | |
| ExtrasRequestProvider | client | gadk_model::ExtrasRequestProvider | |
| File | struct | gadk_genai::File | |
| FileData | struct | gadk_genai::FileData | |
| FileSearch | struct | gadk_genai::FileSearch | |
| FinishReason | enum | gadk_genai::FinishReason | string_enum! with Unknown(String) |
| FinishReasonBlocklist | enum value | gadk_genai::FinishReason::Blocklist | serializes "BLOCKLIST" |
| FinishReasonContinuation | enum value | gadk_genai::FinishReason::Continuation | serializes "CONTINUATION" |
| FinishReasonImageOther | enum value | gadk_genai::FinishReason::ImageOther | serializes "IMAGE_OTHER" |
| FinishReasonImageProhibitedContent | enum value | gadk_genai::FinishReason::ImageProhibitedContent | serializes "IMAGE_PROHIBITED_CONTENT" |
| FinishReasonImageRecitation | enum value | gadk_genai::FinishReason::ImageRecitation | serializes "IMAGE_RECITATION" |
| FinishReasonImageSafety | enum value | gadk_genai::FinishReason::ImageSafety | serializes "IMAGE_SAFETY" |
| FinishReasonLanguage | enum value | gadk_genai::FinishReason::Language | serializes "LANGUAGE" |
| FinishReasonMalformedFunctionCall | enum value | gadk_genai::FinishReason::MalformedFunctionCall | serializes "MALFORMED_FUNCTION_CALL" |
| FinishReasonMaxTokens | enum value | gadk_genai::FinishReason::MaxTokens | serializes "MAX_TOKENS" |
| FinishReasonNoImage | enum value | gadk_genai::FinishReason::NoImage | serializes "NO_IMAGE" |
| FinishReasonOther | enum value | gadk_genai::FinishReason::Other | serializes "OTHER" |
| FinishReasonProhibitedContent | enum value | gadk_genai::FinishReason::ProhibitedContent | serializes "PROHIBITED_CONTENT" |
| FinishReasonRecitation | enum value | gadk_genai::FinishReason::Recitation | serializes "RECITATION" |
| FinishReasonSafety | enum value | gadk_genai::FinishReason::Safety | serializes "SAFETY" |
| FinishReasonSPII | enum value | gadk_genai::FinishReason::Spii | serializes "SPII" |
| FinishReasonStop | enum value | gadk_genai::FinishReason::Stop | serializes "STOP" |
| FinishReasonTooManyToolCalls | enum value | gadk_genai::FinishReason::TooManyToolCalls | serializes "TOO_MANY_TOOL_CALLS" |
| FinishReasonUnexpectedToolCall | enum value | gadk_genai::FinishReason::UnexpectedToolCall | serializes "UNEXPECTED_TOOL_CALL" |
| FinishReasonUnspecified | enum value | gadk_genai::FinishReason::Unspecified | serializes "FINISH_REASON_UNSPECIFIED" |
| FunctionCall | struct | gadk_genai::FunctionCall | |
| FunctionCallingConfig | struct | gadk_genai::FunctionCallingConfig | |
| FunctionCallingConfigMode | enum | gadk_genai::FunctionCallingConfigMode | string_enum! with Unknown(String) |
| FunctionCallingConfigModeAny | enum value | gadk_genai::FunctionCallingConfigMode::Any | serializes "ANY" |
| FunctionCallingConfigModeAuto | enum value | gadk_genai::FunctionCallingConfigMode::Auto | serializes "AUTO" |
| FunctionCallingConfigModeNone | enum value | gadk_genai::FunctionCallingConfigMode::None | serializes "NONE" |
| FunctionCallingConfigModeUnspecified | enum value | gadk_genai::FunctionCallingConfigMode::Unspecified | serializes "MODE_UNSPECIFIED" |
| FunctionCallingConfigModeValidated | enum value | gadk_genai::FunctionCallingConfigMode::Validated | serializes "VALIDATED" |
| FunctionDeclaration | struct | gadk_genai::FunctionDeclaration | |
| FunctionResponse | struct | gadk_genai::FunctionResponse | |
| FunctionResponseBlob | struct | gadk_genai::FunctionResponseBlob | |
| FunctionResponseFileData | struct | gadk_genai::FunctionResponseFileData | |
| FunctionResponsePart | struct | gadk_genai::FunctionResponsePart | |
| FunctionResponseSchedulingInterrupt | enum value | gadk_genai::FunctionResponseScheduling::Interrupt | serializes "INTERRUPT" |
| FunctionResponseSchedulingWhenIdle | enum value | gadk_genai::FunctionResponseScheduling::WhenIdle | serializes "WHEN_IDLE" |
| GenerateContentConfig | struct | gadk_genai::GenerateContentConfig | |
| GenerateContentResponse | struct | gadk_genai::GenerateContentResponse | |
| GenerateContentResponsePromptFeedback | struct | gadk_genai::GenerateContentResponsePromptFeedback | |
| GenerateContentResponseUsageMetadata | struct | gadk_genai::GenerateContentResponseUsageMetadata | |
| GeneratedImage | struct | gadk_genai::GeneratedImage | |
| GenerateImagesConfig | struct | gadk_genai::GenerateImagesConfig | |
| GenerateImagesResponse | struct | gadk_genai::GenerateImagesResponse | |
| GenerationConfigRoutingConfig | struct | gadk_genai::GenerationConfigRoutingConfig | |
| GoogleMaps | struct | gadk_genai::GoogleMaps | |
| GoogleSearch | struct | gadk_genai::GoogleSearch | |
| GoogleSearchRetrieval | struct | gadk_genai::GoogleSearchRetrieval | |
| GroundingChunk | struct | gadk_genai::GroundingChunk | |
| GroundingChunkMaps | struct | gadk_genai::GroundingChunkMaps | |
| GroundingChunkMapsPlaceAnswerSources | struct | gadk_genai::GroundingChunkMapsPlaceAnswerSources | |
| GroundingChunkMapsPlaceAnswerSourcesReviewSnippet | struct | gadk_genai::GroundingChunkMapsPlaceAnswerSourcesReviewSnippet | |
| GroundingChunkRetrievedContext | struct | gadk_genai::GroundingChunkRetrievedContext | |
| GroundingChunkWeb | struct | gadk_genai::GroundingChunkWeb | |
| GroundingMetadata | struct | gadk_genai::GroundingMetadata | |
| GroundingMetadataSourceFlaggingURI | struct | gadk_genai::GroundingMetadataSourceFlaggingURI | |
| GroundingSupport | struct | gadk_genai::GroundingSupport | |
| HarmBlockThresholdBlockLowAndAbove | enum value | gadk_genai::HarmBlockThreshold::BlockLowAndAbove | serializes "BLOCK_LOW_AND_ABOVE" |
| HarmBlockThresholdBlockNone | enum value | gadk_genai::HarmBlockThreshold::BlockNone | serializes "BLOCK_NONE" |
| HarmCategoryHarassment | enum value | gadk_genai::HarmCategory::Harassment | serializes "HARM_CATEGORY_HARASSMENT" |
| HarmCategoryHateSpeech | enum value | gadk_genai::HarmCategory::HateSpeech | serializes "HARM_CATEGORY_HATE_SPEECH" |
| HTTPOptions | client | gadk_model::HttpOptions | |
| HTTPRetryOptions | client | N/A — retries not in 1a; gax-style retry may be added with sub-project 4 | |
| Image | struct | gadk_genai::Image | |
| ImageConfig | struct | gadk_genai::ImageConfig | |
| Language | enum | gadk_genai::Language | string_enum! with Unknown(String) |
| LanguagePython | enum value | gadk_genai::Language::Python | serializes "PYTHON" |
| LanguageUnspecified | enum value | gadk_genai::Language::Unspecified | serializes "LANGUAGE_UNSPECIFIED" |
| LiveClientContentInput | struct (alias of LiveSendClientContentParameters) | gadk_genai::LiveClientContentInput | |
| LiveConnectConfig | struct | gadk_genai::LiveConnectConfig | |
| LiveRealtimeInput | struct (alias of LiveSendRealtimeInputParameters) | gadk_genai::LiveRealtimeInput | |
| LiveToolResponseInput | struct (alias of LiveSendToolResponseParameters) | gadk_genai::LiveToolResponseInput | |
| LogprobsResult | struct | gadk_genai::LogprobsResult | |
| LogprobsResultCandidate | struct | gadk_genai::LogprobsResultCandidate | |
| LogprobsResultTopCandidates | struct | gadk_genai::LogprobsResultTopCandidates | |
| MCPServer | struct | gadk_genai::MCPServer | |
| MediaModalityText | enum value | gadk_genai::MediaModality::Text | serializes "TEXT" |
| MediaResolutionHigh | enum value | gadk_genai::MediaResolution::High | serializes "MEDIA_RESOLUTION_HIGH" |
| MediaResolutionLow | enum value | gadk_genai::MediaResolution::Low | serializes "MEDIA_RESOLUTION_LOW" |
| Modality | enum | gadk_genai::Modality | string_enum! with Unknown(String) |
| ModalityAudio | enum value | gadk_genai::Modality::Audio | serializes "AUDIO" |
| ModalityTokenCount | struct | gadk_genai::ModalityTokenCount | |
| ModelArmorConfig | struct | gadk_genai::ModelArmorConfig | |
| ModelSelectionConfig | struct | gadk_genai::ModelSelectionConfig | |
| NewClient | client | gadk_gemini::GeminiModel::new | |
| NewContentFromBytes | constructor | gadk_genai::Content::from_bytes | |
| NewContentFromCodeExecutionResult | constructor | gadk_genai::Content::from_code_execution_result | |
| NewContentFromExecutableCode | constructor | gadk_genai::Content::from_executable_code | |
| NewContentFromFunctionCall | constructor | gadk_genai::Content::from_function_call | |
| NewContentFromFunctionResponse | constructor | gadk_genai::Content::from_function_response | |
| NewContentFromParts | constructor | gadk_genai::Content::from_parts | |
| NewContentFromText | constructor | gadk_genai::Content::from_text | |
| NewContentFromURI | constructor | gadk_genai::Content::from_uri | |
| NewPartFromBytes | constructor | gadk_genai::Part::from_bytes | |
| NewPartFromCodeExecutionResult | constructor | gadk_genai::Part::from_code_execution_result | |
| NewPartFromExecutableCode | constructor | gadk_genai::Part::from_executable_code | |
| NewPartFromFile | constructor | gadk_genai::Part::from_file | |
| NewPartFromFunctionCall | constructor | gadk_genai::Part::from_function_call | |
| NewPartFromFunctionResponse | constructor | gadk_genai::Part::from_function_response | |
| NewPartFromText | constructor | gadk_genai::Part::from_text | |
| NewPartFromURI | constructor | gadk_genai::Part::from_uri | |
| Outcome | enum | gadk_genai::Outcome | string_enum! with Unknown(String) |
| OutcomeOK | enum value | gadk_genai::Outcome::Ok | serializes "OUTCOME_OK" |
| Part | struct | gadk_genai::Part | |
| Partial | N/A | N/A | false positive: appears only inside a test string literal (`"*genai.Partial response"`) |
| PartialArg | struct | gadk_genai::PartialArg | |
| PartMediaResolution | struct | gadk_genai::PartMediaResolution | |
| PartMediaResolutionLevelMediaResolutionHigh | enum value | gadk_genai::PartMediaResolutionLevel::MediaResolutionHigh | serializes "MEDIA_RESOLUTION_HIGH" |
| PartMediaResolutionLevelMediaResolutionLow | enum value | gadk_genai::PartMediaResolutionLevel::MediaResolutionLow | serializes "MEDIA_RESOLUTION_LOW" |
| ProactivityConfig | struct | gadk_genai::ProactivityConfig | |
| Ptr | client | N/A — Rust uses `Some(v)` | |
| RAGChunk | struct | gadk_genai::RAGChunk | |
| RAGChunkPageSpan | struct | gadk_genai::RAGChunkPageSpan | |
| RealtimeInputConfig | struct | gadk_genai::RealtimeInputConfig | |
| ResourceScope | enum | gadk_genai::ResourceScope | string_enum! with Unknown(String) |
| ResourceScopeCollection | enum value | gadk_genai::ResourceScope::Collection | serializes "COLLECTION" |
| Retrieval | struct | gadk_genai::Retrieval | |
| RetrievalMetadata | struct | gadk_genai::RetrievalMetadata | |
| Role | type | N/A — Rust uses `String` roles with consts | Go `type Role string`; `Content.role` is a plain string in genai |
| RoleModel | const | gadk_genai::ROLE_MODEL | "model" |
| RoleUser | const | gadk_genai::ROLE_USER | "user" |
| SafetySetting | struct | gadk_genai::SafetySetting | |
| Schema | struct | gadk_genai::Schema | |
| SearchEntryPoint | struct | gadk_genai::SearchEntryPoint | |
| Segment | struct | gadk_genai::Segment | |
| ServiceTier | enum | gadk_genai::ServiceTier | string_enum! with Unknown(String) |
| ServiceTierFlex | enum value | gadk_genai::ServiceTier::Flex | serializes "flex" |
| ServiceTierPriority | enum value | gadk_genai::ServiceTier::Priority | serializes "priority" |
| ServiceTierStandard | enum value | gadk_genai::ServiceTier::Standard | serializes "standard" |
| ServiceTierUnspecified | enum value | gadk_genai::ServiceTier::Unspecified | serializes "unspecified" |
| Session | client | gadk_gemini::LiveConnection | |
| SessionResumptionConfig | struct | gadk_genai::SessionResumptionConfig | |
| SpeechConfig | struct | gadk_genai::SpeechConfig | |
| SpeechMetadata | struct | gadk_genai::SpeechMetadata | |
| Text | client | gadk_genai::text | |
| ThinkingConfig | struct | gadk_genai::ThinkingConfig | |
| ThinkingLevel | enum | gadk_genai::ThinkingLevel | string_enum! with Unknown(String) |
| ThinkingLevelHigh | enum value | gadk_genai::ThinkingLevel::High | serializes "HIGH" |
| ThinkingLevelLow | enum value | gadk_genai::ThinkingLevel::Low | serializes "LOW" |
| ThinkingLevelMedium | enum value | gadk_genai::ThinkingLevel::Medium | serializes "MEDIUM" |
| ThinkingLevelMinimal | enum value | gadk_genai::ThinkingLevel::Minimal | serializes "MINIMAL" |
| ThinkingLevelUnspecified | enum value | gadk_genai::ThinkingLevel::Unspecified | serializes "THINKING_LEVEL_UNSPECIFIED" |
| Tool | struct | gadk_genai::Tool | |
| ToolCall | struct | gadk_genai::ToolCall | |
| ToolCodeExecution | struct | gadk_genai::ToolCodeExecution | |
| ToolConfig | struct | gadk_genai::ToolConfig | |
| ToolExaAISearch | struct | gadk_genai::ToolExaAISearch | |
| ToolParallelAISearch | struct | gadk_genai::ToolParallelAISearch | |
| ToolResponse | struct | gadk_genai::ToolResponse | |
| Transcription | struct | gadk_genai::Transcription | |
| Type | enum | gadk_genai::Type | string_enum! with Unknown(String) |
| TypeArray | enum value | gadk_genai::Type::Array | serializes "ARRAY" |
| TypeBoolean | enum value | gadk_genai::Type::Boolean | serializes "BOOLEAN" |
| TypeInteger | enum value | gadk_genai::Type::Integer | serializes "INTEGER" |
| TypeNumber | enum value | gadk_genai::Type::Number | serializes "NUMBER" |
| TypeObject | enum value | gadk_genai::Type::Object | serializes "OBJECT" |
| TypeString | enum value | gadk_genai::Type::String | serializes "STRING" |
| URLContext | struct | gadk_genai::URLContext | |
| VideoMetadata | struct | gadk_genai::VideoMetadata | |
