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

//! Tool and tool-config types.

use crate::schema::Schema;
use crate::serde_util::{go_duration, is_zero};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

crate::string_enum!(
    /// Type of auth scheme. This enum is not supported in Gemini API.
    AuthType {
        Unspecified => "AUTH_TYPE_UNSPECIFIED",
        NoAuth => "NO_AUTH",
        ApiKeyAuth => "API_KEY_AUTH",
        HttpBasicAuth => "HTTP_BASIC_AUTH",
        GoogleServiceAccountAuth => "GOOGLE_SERVICE_ACCOUNT_AUTH",
        Oauth => "OAUTH",
        OidcAuth => "OIDC_AUTH",
    }
);

crate::string_enum!(
    /// The location of the API key. This enum is not supported
    /// in Gemini API.
    HTTPElementLocation {
        HttpInUnspecified => "HTTP_IN_UNSPECIFIED",
        HttpInQuery => "HTTP_IN_QUERY",
        HttpInHeader => "HTTP_IN_HEADER",
        HttpInPath => "HTTP_IN_PATH",
        HttpInBody => "HTTP_IN_BODY",
        HttpInCookie => "HTTP_IN_COOKIE",
    }
);

crate::string_enum!(
    /// The API spec that the external API implements. This enum is not
    /// supported in Gemini API.
    APISpec {
        Unspecified => "API_SPEC_UNSPECIFIED",
        SimpleSearch => "SIMPLE_SEARCH",
        ElasticSearch => "ELASTIC_SEARCH",
    }
);

crate::string_enum!(
    /// SafetyPolicy
    SafetyPolicy {
        Unspecified => "SAFETY_POLICY_UNSPECIFIED",
        FinancialTransactions => "FINANCIAL_TRANSACTIONS",
        SensitiveDataModification => "SENSITIVE_DATA_MODIFICATION",
        CommunicationTool => "COMMUNICATION_TOOL",
        AccountCreation => "ACCOUNT_CREATION",
        DataModification => "DATA_MODIFICATION",
        UserConsentManagement => "USER_CONSENT_MANAGEMENT",
        LegalTermsAndAgreements => "LEGAL_TERMS_AND_AGREEMENTS",
    }
);

crate::string_enum!(
    /// The environment being operated.
    Environment {
        Unspecified => "ENVIRONMENT_UNSPECIFIED",
        Browser => "ENVIRONMENT_BROWSER",
        Mobile => "ENVIRONMENT_MOBILE",
        Desktop => "ENVIRONMENT_DESKTOP",
    }
);

crate::string_enum!(
    /// Sites with confidence level chosen & above this value
    /// will be blocked from the search results. This enum is not supported in Gemini API.
    PhishBlockThreshold {
        Unspecified => "PHISH_BLOCK_THRESHOLD_UNSPECIFIED",
        BlockLowAndAbove => "BLOCK_LOW_AND_ABOVE",
        BlockMediumAndAbove => "BLOCK_MEDIUM_AND_ABOVE",
        BlockHighAndAbove => "BLOCK_HIGH_AND_ABOVE",
        BlockHigherAndAbove => "BLOCK_HIGHER_AND_ABOVE",
        BlockVeryHighAndAbove => "BLOCK_VERY_HIGH_AND_ABOVE",
        BlockOnlyExtremelyHigh => "BLOCK_ONLY_EXTREMELY_HIGH",
    }
);

crate::string_enum!(
    /// Specifies the function Behavior. If not specified, the system
    /// keeps the current function call behavior. This field is currently only supported by the
    /// BidiGenerateContent method.
    Behavior {
        Unspecified => "UNSPECIFIED",
        Blocking => "BLOCKING",
        NonBlocking => "NON_BLOCKING",
    }
);

crate::string_enum!(
    /// The mode of the predictor to be used in dynamic
    /// retrieval.
    DynamicRetrievalConfigMode {
        Unspecified => "MODE_UNSPECIFIED",
        Dynamic => "MODE_DYNAMIC",
    }
);

crate::string_enum!(
    /// Function calling mode.
    FunctionCallingConfigMode {
        Unspecified => "MODE_UNSPECIFIED",
        Auto => "AUTO",
        Any => "ANY",
        None => "NONE",
        Validated => "VALIDATED",
    }
);

/// Config for authentication with API key. This data
/// type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_secret: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_string: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_element_location: Option<HTTPElementLocation>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
}

/// Config for Google Service
/// Account Authentication. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigGoogleServiceAccountConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub service_account: String,
}

/// Config for HTTP Basic
/// Authentication. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigHTTPBasicAuthConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub credential_secret: String,
}

/// Config for user oauth. This data type is not
/// supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigOauthConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub access_token: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub service_account: String,
}

/// Config for user OIDC auth. This data type is
/// not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigOidcConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id_token: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub service_account: String,
}

/// The authentication config to access the API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_config: Option<APIKeyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<AuthType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_service_account_config: Option<AuthConfigGoogleServiceAccountConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_basic_auth_config: Option<AuthConfigHTTPBasicAuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_config: Option<AuthConfigOauthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_config: Option<AuthConfigOidcConfig>,
}

/// Grounding with Google Maps Places data (e.g.
/// QueryPlaces). This is the default Google Maps grounding type when no other type is
/// specified. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMapsPlaces {}

/// Grounding with Google Maps Routing APIs
/// (ComputeRoutes and SearchAlongRoute). This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMapsRouting {}

/// Defines the types of Google Maps
/// grounding that can be enabled and their configurations. This data type is not supported in
/// Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMapsGroundingTypes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub places: Option<GoogleMapsPlaces>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<GoogleMapsRouting>,
}

/// Tool to retrieve knowledge from Google Maps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<AuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_widget: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_types: Option<GoogleMapsGroundingTypes>,
}

/// The API secret. This data type is not
/// supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIAuthAPIKeyConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_secret_version: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_string: String,
}

/// The generic reusable API auth config. Deprecated. Please
/// use AuthConfig (google/cloud/aiplatform/master/auth.proto) instead. This data type is not
/// supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_config: Option<APIAuthAPIKeyConfig>,
}

/// The search parameters to use for
/// the ELASTIC_SEARCH spec. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAPIElasticSearchParams {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub index: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_hits: Option<i32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub search_template: String,
}

/// The search parameters to use for
/// SIMPLE_SEARCH spec. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAPISimpleSearchParams {}

/// Retrieve from data source powered by external API for
/// grounding. The external API is not owned by Google, but need to follow the pre-defined API
/// spec. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAPI {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_auth: Option<APIAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_spec: Option<APISpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<AuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elastic_search_params: Option<ExternalAPIElasticSearchParams>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simple_search_params: Option<ExternalAPISimpleSearchParams>,
}

/// Define data stores within engine to
/// filter on in a search call and configurations for those data stores. For more information,
/// see
/// <https://cloud.google.com/generative-ai-app-builder/docs/reference/rpc/google.cloud.discoveryengine.v1#datastorespec>.
/// This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexAISearchDataStoreSpec {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub data_store: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub filter: String,
}

/// Retrieve from Vertex AI Search datastore or engine
/// for grounding. datastore and engine are mutually exclusive. See
/// <https://cloud.google.com/products/agent-builder>. This data type is not supported in Gemini
/// API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexAISearch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_store_specs: Vec<VertexAISearchDataStoreSpec>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub datastore: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub engine: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub filter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<i32>,
}

/// The definition of the RAG resource. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexRAGStoreRAGResource {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rag_corpus: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rag_file_ids: Vec<String>,
}

/// Config for filters. This data type is not
/// supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigFilter {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub metadata_filter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_similarity_threshold: Option<f64>,
}

/// Config for Hybrid Search. This data
/// type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigHybridSearch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
}

/// Config for LlmRanker. This data
/// type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigRankingLlmRanker {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_name: String,
}

/// Config for Rank Service. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigRankingRankService {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_name: String,
}

/// Config for ranking and reranking. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigRanking {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_ranker: Option<RAGRetrievalConfigRankingLlmRanker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_service: Option<RAGRetrievalConfigRankingRankService>,
}

/// Specifies the context retrieval config. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<RAGRetrievalConfigFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_search: Option<RAGRetrievalConfigHybridSearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<RAGRetrievalConfigRanking>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
}

/// Retrieve from Vertex RAG Store for grounding. This
/// data type is not supported in Gemini API. You can find API default values and more details
/// at
/// <https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/rag-api-v1#parameters-list>
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexRAGStore {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rag_corpora: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rag_resources: Vec<VertexRAGStoreRAGResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rag_retrieval_config: Option<RAGRetrievalConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity_top_k: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance_threshold: Option<f64>,
}

/// Defines a retrieval tool that model can call to access
/// external knowledge. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retrieval {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disable_attribution: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_api: Option<ExternalAPI>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_ai_search: Option<VertexAISearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_rag_store: Option<VertexRAGStore>,
}

/// Tool to support computer use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_predefined_functions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_prompt_injection_detection: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_safety_policies: Vec<SafetyPolicy>,
}

/// The FileSearch tool that retrieves knowledge from
/// Semantic Retrieval corpora. Files are imported to Semantic Retrieval corpora using the
/// ImportFile API. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_search_store_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub metadata_filter: String,
}

/// Standard web search for grounding and related
/// configurations. Only text results are returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearch {}

/// Image search for grounding and related configurations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSearch {}

/// Different types of search that can be enabled on the
/// GoogleSearch tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTypes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_search: Option<ImageSearch>,
}

/// Represents a time interval, encoded as a Timestamp start
/// (inclusive) and a Timestamp end (exclusive). The start must be less than or equal to the
/// end. When the start equals the end, the interval is empty (matches no time). When both start
/// and end are unspecified, the interval matches any time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<DateTime<Utc>>,
}

/// GoogleSearch tool type. Tool to support Google Search
/// in Model. Powered by Google.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleSearch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_types: Option<SearchTypes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_confidence: Option<PhishBlockThreshold>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range_filter: Option<Interval>,
}

/// Tool that executes code generated by the model,
/// and automatically returns the result to the model. See also ExecutableCode and
/// CodeExecutionResult, which are input and output to this tool. This data type is not
/// supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCodeExecution {}

/// Tool to search public web data, powered by
/// Vertex AI Search and Sec4 compliance. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnterpriseWebSearch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_confidence: Option<PhishBlockThreshold>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_domains: Vec<String>,
}

/// Structured representation of a function
/// declaration as defined by the \[OpenAPI 3.0
/// specification\](<https://spec.openapis.org/oas/v3.0.3>). Included in this declaration are
/// the function name, description, parameters and response type. This FunctionDeclaration is a
/// representation of a block of code that can be used as a `Tool` by the model and executed by
/// the client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub description: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Schema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Schema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<Behavior>,
}

/// Describes the options to customize dynamic
/// retrieval.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicRetrievalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DynamicRetrievalConfigMode>,
}

/// Tool to retrieve public web data for
/// grounding, powered by Google.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleSearchRetrieval {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_retrieval_config: Option<DynamicRetrievalConfig>,
}

/// ParallelAISearch tool type. A tool that uses
/// the Parallel.ai search engine for grounding. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolParallelAISearch {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub custom_configs: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_data_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_zero_data_retention: Option<bool>,
}

/// Tool to support URL context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct URLContext {}

/// A transport that can stream HTTP requests
/// and responses. Next ID: 6. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamableHTTPTransport {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub sse_read_timeout: TimeDelta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate_on_close: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub timeout: TimeDelta,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub url: String,
}

/// A MCPServer is a server that can be called by the model
/// to perform actions. It is a server that implements the MCP protocol. Next ID: 7. This data
/// type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPServer {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streamable_http_transport: Option<StreamableHTTPTransport>,
}

/// ExaAISearch tool type. A tool that uses the Exa.ai
/// search engine for grounding. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExaAISearch {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub custom_configs: Map<String, Value>,
}

/// Tool details of a tool that the model may use to generate a
/// response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<Retrieval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<ComputerUse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_search: Option<FileSearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_search: Option<GoogleSearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_maps: Option<GoogleMaps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution: Option<ToolCodeExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enterprise_web_search: Option<EnterpriseWebSearch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_declarations: Vec<FunctionDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_search_retrieval: Option<GoogleSearchRetrieval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_ai_search: Option<ToolParallelAISearch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_context: Option<URLContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<MCPServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exa_ai_search: Option<ToolExaAISearch>,
}

/// An object that represents a latitude/longitude pair. This
/// is expressed as a pair of doubles to represent degrees latitude and degrees longitude.
/// Unless specified otherwise, this object must conform to the WGS84 standard. Values must be
/// within normalized ranges.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatLng {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

/// Retrieval config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat_lng: Option<LatLng>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub language_code: String,
}

/// Function calling config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_function_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FunctionCallingConfigMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_function_call_arguments: Option<bool>,
}

/// Tool config. This config is shared for all tools
/// provided in the request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_config: Option<RetrievalConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_server_side_tool_invocations: Option<bool>,
}
