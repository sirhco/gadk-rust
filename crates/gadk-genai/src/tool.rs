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
        /// Not specified.
        Unspecified => "AUTH_TYPE_UNSPECIFIED",
        /// No authentication.
        NoAuth => "NO_AUTH",
        /// An API key.
        ApiKeyAuth => "API_KEY_AUTH",
        /// HTTP basic authentication.
        HttpBasicAuth => "HTTP_BASIC_AUTH",
        /// A Google service account.
        GoogleServiceAccountAuth => "GOOGLE_SERVICE_ACCOUNT_AUTH",
        /// OAuth.
        Oauth => "OAUTH",
        /// OpenID Connect (OIDC).
        OidcAuth => "OIDC_AUTH",
    }
);

crate::string_enum!(
    /// The location of the API key. This enum is not supported in Gemini API.
    HTTPElementLocation {
        /// Not specified.
        HttpInUnspecified => "HTTP_IN_UNSPECIFIED",
        /// In a query parameter.
        HttpInQuery => "HTTP_IN_QUERY",
        /// In a header.
        HttpInHeader => "HTTP_IN_HEADER",
        /// In a path parameter.
        HttpInPath => "HTTP_IN_PATH",
        /// In the request body.
        HttpInBody => "HTTP_IN_BODY",
        /// In a cookie.
        HttpInCookie => "HTTP_IN_COOKIE",
    }
);

crate::string_enum!(
    /// The API spec that the external API implements. This enum is not supported in Gemini API.
    APISpec {
        /// Not specified.
        Unspecified => "API_SPEC_UNSPECIFIED",
        /// The simple search API spec.
        SimpleSearch => "SIMPLE_SEARCH",
        /// The Elasticsearch API spec.
        ElasticSearch => "ELASTIC_SEARCH",
    }
);

crate::string_enum!(
    /// SafetyPolicy
    SafetyPolicy {
        /// Not specified.
        Unspecified => "SAFETY_POLICY_UNSPECIFIED",
        /// Financial transactions.
        FinancialTransactions => "FINANCIAL_TRANSACTIONS",
        /// Modifying sensitive data.
        SensitiveDataModification => "SENSITIVE_DATA_MODIFICATION",
        /// Communication tools, such as sending email or messages.
        CommunicationTool => "COMMUNICATION_TOOL",
        /// Creating accounts.
        AccountCreation => "ACCOUNT_CREATION",
        /// Modifying data.
        DataModification => "DATA_MODIFICATION",
        /// Managing user consent.
        UserConsentManagement => "USER_CONSENT_MANAGEMENT",
        /// Accepting legal terms and agreements.
        LegalTermsAndAgreements => "LEGAL_TERMS_AND_AGREEMENTS",
    }
);

crate::string_enum!(
    /// The environment being operated.
    Environment {
        /// Not specified.
        Unspecified => "ENVIRONMENT_UNSPECIFIED",
        /// A web browser.
        Browser => "ENVIRONMENT_BROWSER",
        /// A mobile device.
        Mobile => "ENVIRONMENT_MOBILE",
        /// A desktop.
        Desktop => "ENVIRONMENT_DESKTOP",
    }
);

crate::string_enum!(
    /// Sites with confidence level chosen & above this value will be blocked from the search
    /// results. This enum is not supported in Gemini API.
    PhishBlockThreshold {
        /// Not specified.
        Unspecified => "PHISH_BLOCK_THRESHOLD_UNSPECIFIED",
        /// Block sites with low or greater phishing confidence.
        BlockLowAndAbove => "BLOCK_LOW_AND_ABOVE",
        /// Block sites with medium or greater phishing confidence.
        BlockMediumAndAbove => "BLOCK_MEDIUM_AND_ABOVE",
        /// Block sites with high or greater phishing confidence.
        BlockHighAndAbove => "BLOCK_HIGH_AND_ABOVE",
        /// Block sites with higher or greater phishing confidence.
        BlockHigherAndAbove => "BLOCK_HIGHER_AND_ABOVE",
        /// Block sites with very high or greater phishing confidence.
        BlockVeryHighAndAbove => "BLOCK_VERY_HIGH_AND_ABOVE",
        /// Block only sites with extremely high phishing confidence.
        BlockOnlyExtremelyHigh => "BLOCK_ONLY_EXTREMELY_HIGH",
    }
);

crate::string_enum!(
    /// Specifies the function Behavior. If not specified, the system keeps the current function
    /// call behavior. This field is currently only supported by the BidiGenerateContent method.
    Behavior {
        /// Not specified; treated as blocking.
        Unspecified => "UNSPECIFIED",
        /// The model waits for the function response before continuing.
        Blocking => "BLOCKING",
        /// The model continues while the function runs; the response arrives later.
        NonBlocking => "NON_BLOCKING",
    }
);

crate::string_enum!(
    /// The mode of the predictor to be used in dynamic retrieval.
    DynamicRetrievalConfigMode {
        /// Not specified; retrieval always runs.
        Unspecified => "MODE_UNSPECIFIED",
        /// Retrieve only when the predicted benefit exceeds the dynamic threshold.
        Dynamic => "MODE_DYNAMIC",
    }
);

crate::string_enum!(
    /// Function calling mode.
    FunctionCallingConfigMode {
        /// Not specified; behaves like `Auto`.
        Unspecified => "MODE_UNSPECIFIED",
        /// The model decides whether to call a function or answer in text.
        Auto => "AUTO",
        /// The model must call a function (optionally limited to the allowed names).
        Any => "ANY",
        /// The model must not call functions.
        None => "NONE",
        /// Like `Auto`, but calls are validated against the function schemas.
        Validated => "VALIDATED",
    }
);

/// Config for authentication with API key. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyConfig {
    /// Secret Manager resource name of the API key secret.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_secret: String,
    /// The API key itself.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_string: String,
    /// Where the API key is sent in the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_element_location: Option<HTTPElementLocation>,
    /// Name of the parameter (query, header, ...) that carries the API key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
}

/// Config for Google Service Account Authentication. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigGoogleServiceAccountConfig {
    /// Service account the agent impersonates to call the API.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub service_account: String,
}

/// Config for HTTP Basic Authentication. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigHTTPBasicAuthConfig {
    /// Secret Manager resource name of the base64-encoded `username:password` credential.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub credential_secret: String,
}

/// Config for user oauth. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigOauthConfig {
    /// OAuth access token to send with the request.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub access_token: String,
    /// Service account used to mint an access token.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub service_account: String,
}

/// Config for user OIDC auth. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigOidcConfig {
    /// OIDC ID token to send with the request.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id_token: String,
    /// Service account used to mint an ID token.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub service_account: String,
}

/// The authentication config to access the API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    /// The API key, as a shorthand for an API-key `auth_type`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key: String,
    /// API key settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_config: Option<APIKeyConfig>,
    /// The authentication scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<AuthType>,
    /// Google service account settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_service_account_config: Option<AuthConfigGoogleServiceAccountConfig>,
    /// HTTP basic authentication settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_basic_auth_config: Option<AuthConfigHTTPBasicAuthConfig>,
    /// OAuth settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_config: Option<AuthConfigOauthConfig>,
    /// OIDC settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_config: Option<AuthConfigOidcConfig>,
}

/// Grounding with Google Maps Places data (e.g. QueryPlaces). This is the default Google Maps
/// grounding type when no other type is specified. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMapsPlaces {}

/// Grounding with Google Maps Routing APIs (ComputeRoutes and SearchAlongRoute). This data type is
/// not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMapsRouting {}

/// Defines the types of Google Maps grounding that can be enabled and their configurations. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMapsGroundingTypes {
    /// Ground on Places data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub places: Option<GoogleMapsPlaces>,
    /// Ground on routing data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<GoogleMapsRouting>,
}

/// Tool to retrieve knowledge from Google Maps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMaps {
    /// Authentication for the Google Maps API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<AuthConfig>,
    /// Whether to return a context token for rendering the Google Maps widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_widget: Option<bool>,
    /// Which kinds of Google Maps grounding to enable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_types: Option<GoogleMapsGroundingTypes>,
}

/// The API secret. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIAuthAPIKeyConfig {
    /// Secret Manager secret version holding the API key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_secret_version: String,
    /// The API key itself.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key_string: String,
}

/// The generic reusable API auth config. Deprecated. Please use AuthConfig
/// (google/cloud/aiplatform/master/auth.proto) instead. This data type is not supported in Gemini
/// API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIAuth {
    /// API key settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_config: Option<APIAuthAPIKeyConfig>,
}

/// The search parameters to use for the ELASTIC_SEARCH spec. This data type is not supported in
/// Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAPIElasticSearchParams {
    /// Elasticsearch index to search.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub index: String,
    /// Number of hits to request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_hits: Option<i32>,
    /// Elasticsearch search template to use.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub search_template: String,
}

/// The search parameters to use for SIMPLE_SEARCH spec. This data type is not supported in Gemini
/// API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAPISimpleSearchParams {}

/// Retrieve from data source powered by external API for grounding. The external API is not owned
/// by Google, but need to follow the pre-defined API spec. This data type is not supported in
/// Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAPI {
    /// Deprecated API auth settings; use `auth_config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_auth: Option<APIAuth>,
    /// The API spec the external API follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_spec: Option<APISpec>,
    /// Authentication for the external API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<AuthConfig>,
    /// Parameters for the Elasticsearch spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elastic_search_params: Option<ExternalAPIElasticSearchParams>,
    /// Endpoint URL of the external API.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub endpoint: String,
    /// Parameters for the simple search spec.
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
    /// Resource name of the data store.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub data_store: String,
    /// Filter applied to the data store's results.
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
    /// Data stores to search, when `engine` is used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_store_specs: Vec<VertexAISearchDataStoreSpec>,
    /// Resource name of a Vertex AI Search data store.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub datastore: String,
    /// Resource name of a Vertex AI Search engine.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub engine: String,
    /// Filter applied to the search results.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub filter: String,
    /// Maximum number of results to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<i32>,
}

/// The definition of the RAG resource. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexRAGStoreRAGResource {
    /// Resource name of the RAG corpus.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rag_corpus: String,
    /// IDs of the files in the corpus to retrieve from; empty means all files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rag_file_ids: Vec<String>,
}

/// Config for filters. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigFilter {
    /// Metadata filter expression.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub metadata_filter: String,
    /// Only return contexts whose vector distance is below this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance_threshold: Option<f64>,
    /// Only return contexts whose vector similarity is above this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_similarity_threshold: Option<f64>,
}

/// Config for Hybrid Search. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigHybridSearch {
    /// Weight between dense (1.0) and sparse (0.0) vector search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
}

/// Config for LlmRanker. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigRankingLlmRanker {
    /// Name of the model used to rank results.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_name: String,
}

/// Config for Rank Service. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigRankingRankService {
    /// Name of the ranking model of the rank service.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_name: String,
}

/// Config for ranking and reranking. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfigRanking {
    /// Rank with an LLM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_ranker: Option<RAGRetrievalConfigRankingLlmRanker>,
    /// Rank with the rank service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_service: Option<RAGRetrievalConfigRankingRankService>,
}

/// Specifies the context retrieval config. This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RAGRetrievalConfig {
    /// Filtering of retrieved contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<RAGRetrievalConfigFilter>,
    /// Hybrid search settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_search: Option<RAGRetrievalConfigHybridSearch>,
    /// Ranking of retrieved contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<RAGRetrievalConfigRanking>,
    /// Number of contexts to retrieve.
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
    /// Deprecated: use `rag_resources`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rag_corpora: Vec<String>,
    /// RAG corpora (and optionally files) to retrieve from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rag_resources: Vec<VertexRAGStoreRAGResource>,
    /// How contexts are retrieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rag_retrieval_config: Option<RAGRetrievalConfig>,
    /// Deprecated: number of contexts to retrieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity_top_k: Option<i32>,
    /// Whether to store the retrieved context for later use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_context: Option<bool>,
    /// Deprecated: only return contexts within this vector distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance_threshold: Option<f64>,
}

/// Defines a retrieval tool that model can call to access external knowledge. This data type is not
/// supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retrieval {
    /// Deprecated: whether to turn off attribution.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disable_attribution: bool,
    /// Retrieve from an external API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_api: Option<ExternalAPI>,
    /// Retrieve from Vertex AI Search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_ai_search: Option<VertexAISearch>,
    /// Retrieve from a Vertex AI RAG store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_rag_store: Option<VertexRAGStore>,
}

/// Tool to support computer use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUse {
    /// The environment the model operates in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    /// Predefined computer-use functions the model must not call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_predefined_functions: Vec<String>,
    /// Whether to detect prompt injection in the observed screens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_prompt_injection_detection: Option<bool>,
    /// Safety policies to turn off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_safety_policies: Vec<SafetyPolicy>,
}

/// The FileSearch tool that retrieves knowledge from Semantic Retrieval corpora. Files are imported
/// to Semantic Retrieval corpora using the ImportFile API. This data type is not supported in
/// Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearch {
    /// Names of the file search stores to search, `fileSearchStores/{id}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_search_store_names: Vec<String>,
    /// Number of chunks to retrieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    /// Metadata filter applied to the documents and chunks.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub metadata_filter: String,
}

/// Standard web search for grounding and related configurations. Only text results are returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearch {}

/// Image search for grounding and related configurations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSearch {}

/// Different types of search that can be enabled on the GoogleSearch tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTypes {
    /// Web search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearch>,
    /// Image search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_search: Option<ImageSearch>,
}

/// Represents a time interval, encoded as a Timestamp start (inclusive) and a Timestamp end
/// (exclusive). The start must be less than or equal to the end. When the start equals the end, the
/// interval is empty (matches no time). When both start and end are unspecified, the interval
/// matches any time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    /// Exclusive end of the interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<DateTime<Utc>>,
    /// Inclusive start of the interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<DateTime<Utc>>,
}

/// GoogleSearch tool type. Tool to support Google Search in Model. Powered by Google.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleSearch {
    /// Which kinds of search to enable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_types: Option<SearchTypes>,
    /// Phishing confidence at which sites are blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_confidence: Option<PhishBlockThreshold>,
    /// Domains excluded from the search results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_domains: Vec<String>,
    /// Only return results published within this interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range_filter: Option<Interval>,
}

/// Tool that executes code generated by the model, and automatically returns the result to the
/// model. See also ExecutableCode and CodeExecutionResult, which are input and output to this tool.
/// This data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCodeExecution {}

/// Tool to search public web data, powered by Vertex AI Search and Sec4 compliance. This data type
/// is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnterpriseWebSearch {
    /// Phishing confidence at which sites are blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_confidence: Option<PhishBlockThreshold>,
    /// Domains excluded from the search results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_domains: Vec<String>,
}

/// Structured representation of a function declaration as defined by the \[OpenAPI 3.0
/// specification\](<https://spec.openapis.org/oas/v3.0.3>). Included in this declaration are the
/// function name, description, parameters and response type. This FunctionDeclaration is a
/// representation of a block of code that can be used as a `Tool` by the model and executed by the
/// client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    /// What the function does; the model reads it to decide when and how to call it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub description: String,
    /// Name of the function: letters, digits, underscores, colons, dots and dashes, at most 64.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    /// Schema of the function parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Schema>,
    /// Schema of the function parameters, as raw JSON Schema; alternative to `parameters`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters_json_schema: Option<Value>,
    /// Schema of the function response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Schema>,
    /// Schema of the function response, as raw JSON Schema; alternative to `response`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<Value>,
    /// Whether the function blocks the model while it runs (Live API only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<Behavior>,
}

/// Describes the options to customize dynamic retrieval.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicRetrievalConfig {
    /// Prediction threshold above which retrieval runs, in dynamic mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_threshold: Option<f32>,
    /// Whether retrieval always runs or only when predicted to help.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DynamicRetrievalConfigMode>,
}

/// Tool to retrieve public web data for grounding, powered by Google.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleSearchRetrieval {
    /// When retrieval runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_retrieval_config: Option<DynamicRetrievalConfig>,
}

/// ParallelAISearch tool type. A tool that uses the Parallel.ai search engine for grounding. This
/// data type is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolParallelAISearch {
    /// Parallel.ai API key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key: String,
    /// Custom settings passed to the Parallel.ai search API.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub custom_configs: Map<String, Value>,
    /// Whether Parallel.ai may retain data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_data_retention: Option<bool>,
    /// Whether to request zero data retention from Parallel.ai.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_zero_data_retention: Option<bool>,
}

/// Tool to support URL context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct URLContext {}

/// A transport that can stream HTTP requests and responses. Next ID: 6. This data type is not
/// supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamableHTTPTransport {
    /// HTTP headers sent with each request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Read timeout for server-sent events; zero is omitted.
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub sse_read_timeout: TimeDelta,
    /// Whether to close the session when the transport closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate_on_close: Option<bool>,
    /// Timeout of HTTP requests; zero is omitted.
    #[serde(
        default,
        skip_serializing_if = "TimeDelta::is_zero",
        with = "go_duration"
    )]
    pub timeout: TimeDelta,
    /// URL of the MCP server endpoint.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub url: String,
}

/// A MCPServer is a server that can be called by the model to perform actions. It is a server that
/// implements the MCP protocol. Next ID: 7. This data type is not supported in Vertex AI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPServer {
    /// Name of the MCP server.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    /// Streamable HTTP transport to reach the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streamable_http_transport: Option<StreamableHTTPTransport>,
}

/// ExaAISearch tool type. A tool that uses the Exa.ai search engine for grounding. This data type
/// is not supported in Gemini API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExaAISearch {
    /// Exa AI API key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub api_key: String,
    /// Custom settings passed to the Exa AI search API.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub custom_configs: Map<String, Value>,
}

/// Tool details of a tool that the model may use to generate a response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Retrieval from a data store for grounding (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<Retrieval>,
    /// Lets the model operate a computer by calling computer-use functions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<ComputerUse>,
    /// Retrieval from file search stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_search: Option<FileSearch>,
    /// Grounding with Google Search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_search: Option<GoogleSearch>,
    /// Grounding with Google Maps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_maps: Option<GoogleMaps>,
    /// Lets the model generate and run code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution: Option<ToolCodeExecution>,
    /// Grounding with enterprise web search (Vertex AI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enterprise_web_search: Option<EnterpriseWebSearch>,
    /// Functions the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_declarations: Vec<FunctionDeclaration>,
    /// Grounding with Google Search using dynamic retrieval (legacy models).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_search_retrieval: Option<GoogleSearchRetrieval>,
    /// Grounding with Parallel.ai search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_ai_search: Option<ToolParallelAISearch>,
    /// Lets the model read the content of URLs in the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_context: Option<URLContext>,
    /// MCP servers the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<MCPServer>,
    /// Grounding with Exa AI search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exa_ai_search: Option<ToolExaAISearch>,
}

/// An object that represents a latitude/longitude pair. This is expressed as a pair of doubles to
/// represent degrees latitude and degrees longitude. Unless specified otherwise, this object must
/// conform to the WGS84 standard. Values must be within normalized ranges.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatLng {
    /// Latitude in degrees, in `[-90.0, 90.0]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Longitude in degrees, in `[-180.0, 180.0]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

/// Retrieval config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalConfig {
    /// The user's location, for location-aware tools such as Google Maps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat_lng: Option<LatLng>,
    /// BCP-47 language code of the user.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub language_code: String,
}

/// Function calling config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    /// Functions the model may call, in `Any` and `Validated` modes; empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_function_names: Vec<String>,
    /// Whether and how the model calls functions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FunctionCallingConfigMode>,
    /// Whether to stream function call arguments as [`PartialArg`](crate::PartialArg) fragments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_function_call_arguments: Option<bool>,
}

/// Tool config. This config is shared for all tools provided in the request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    /// Settings for retrieval tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_config: Option<RetrievalConfig>,
    /// Settings for function calling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
    /// Whether to return server-side tool calls and responses as parts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_server_side_tool_invocations: Option<bool>,
}
