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

//! Port of ADK-Go `model/llm.go`: the [`Llm`] trait and its request/response types.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_genai::serde_util::is_zero;
use gadk_genai::{
    CitationMetadata, Content, FinishReason, GenerateContentConfig,
    GenerateContentResponseUsageMetadata, GroundingMetadata, LiveConnectConfig, LogprobsResult,
    Transcription,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ModelError;
use crate::live::LiveConnection;

/// Which Google backend serves a model (port of genai `Backend`, minus `BackendUnspecified`,
/// which is represented as `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// The Gemini Developer API.
    GeminiApi,
    /// Vertex AI.
    VertexAi,
}

/// Access to an underlying LLM (port of ADK-Go `model.LLM`).
pub trait Llm: Send + Sync {
    /// The model name.
    fn name(&self) -> &str;

    /// Generates content for `req`. With `stream == false` the stream yields one final
    /// response; with `stream == true` it yields partial responses followed by the final one.
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>>;

    /// The Google backend variant, for Google models (port of ADK-Go `GoogleLLM`).
    fn google_variant(&self) -> Option<Backend> {
        None
    }

    /// Opens a live (bidi streaming) connection. Models without live support keep this default,
    /// which fails with Go's text `model does not support live connection`.
    fn connect_live<'a>(
        &'a self,
        cfg: LiveConnectConfig,
    ) -> BoxFuture<'a, Result<Box<dyn LiveConnection>, ModelError>> {
        let _ = cfg;
        Box::pin(async {
            Err(ModelError::Other(
                "model does not support live connection".into(),
            ))
        })
    }

    /// Downcasting support: lets a holder of `Arc<dyn Llm>` reach the concrete model
    /// (for example to open a Live connection on a Gemini model). Implementations return
    /// `self`.
    fn as_any(&self) -> &dyn Any;
}

/// Hook that may add or rewrite fields of the outgoing JSON request body.
pub type ExtrasRequestProvider =
    Arc<dyn Fn(serde_json::Map<String, Value>) -> serde_json::Map<String, Value> + Send + Sync>;

/// Per-request HTTP options (port of genai `HTTPOptions`).
///
/// The `Debug` output lists header names only; header values are redacted.
#[derive(Clone, Default)]
pub struct HttpOptions {
    /// Overrides the backend base URL.
    pub base_url: Option<String>,
    /// Overrides the API version (e.g. `v1beta`).
    pub api_version: Option<String>,
    /// Extra headers sent with the request.
    pub headers: http::HeaderMap,
    /// Request timeout.
    pub timeout: Option<Duration>,
    /// Hook applied to the JSON request body before sending.
    pub extras_request_provider: Option<ExtrasRequestProvider>,
}

impl fmt::Debug for HttpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpOptions")
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("headers", &RedactedHeaders(&self.headers))
            .field("timeout", &self.timeout)
            .field(
                "extras_request_provider",
                &self.extras_request_provider.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

/// Debug view of a header map that prints each header name with its value redacted, so
/// credentials passed as headers never reach logs.
struct RedactedHeaders<'a>(&'a http::HeaderMap);

impl fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|k| (k.as_str(), "<redacted>")))
            .finish()
    }
}

/// The raw LLM request (port of ADK-Go `model.LLMRequest`).
#[derive(Clone, Default)]
pub struct LlmRequest {
    /// The model name.
    pub model: String,
    /// The conversation contents.
    pub contents: Vec<Content>,
    /// Generation config.
    pub config: Option<GenerateContentConfig>,
    /// HTTP options for this request.
    pub http_options: Option<HttpOptions>,
    /// Tools available to this request, keyed by name (not serialized).
    pub tools: HashMap<String, Arc<dyn Any + Send + Sync>>,
}

impl fmt::Debug for LlmRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut tool_names: Vec<&String> = self.tools.keys().collect();
        tool_names.sort();
        f.debug_struct("LlmRequest")
            .field("model", &self.model)
            .field("contents", &self.contents)
            .field("config", &self.config)
            .field("http_options", &self.http_options)
            .field("tools", &tool_names)
            .finish()
    }
}

/// The raw LLM response (port of ADK-Go `model.LLMResponse`).
/// It provides the first candidate response from the model if available.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResponse {
    /// Content of the first candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    /// Citation metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_metadata: Option<CitationMetadata>,
    /// Grounding metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<GroundingMetadata>,
    /// Usage metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GenerateContentResponseUsageMetadata>,
    /// Arbitrary caller metadata.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub custom_metadata: serde_json::Map<String, Value>,
    /// Log-probability results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs_result: Option<LogprobsResult>,
    /// Transcription of audio input (live).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_transcription: Option<Transcription>,
    /// Transcription of audio output (live).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_transcription: Option<Transcription>,
    /// Model version that produced the response.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub model_version: String,
    /// Whether the content is part of an unfinished content stream.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub partial: bool,
    /// Whether the response from the model is complete (streaming only).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turn_complete: bool,
    /// Whether the LLM was interrupted while generating.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub interrupted: bool,
    /// Live session resumption handle.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session_resumption_handle: String,
    /// Error code, if any.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub error_code: String,
    /// Error message, if any.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub error_message: String,
    /// Finish reason of the first candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Average log probability of the first candidate.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub avg_logprobs: f64,
}
