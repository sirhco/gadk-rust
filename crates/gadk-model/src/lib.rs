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

//! gadk-model: the LLM abstraction, the model registry, the HTTP seam and the shared SSE parser
//! ([`sse`]).

mod aggregator;
mod convert;
mod error;
#[cfg(feature = "gcp-auth")]
pub mod gcp_auth;
mod go_json;
mod http;
mod jsonpath;
mod live;
mod llm;
mod registry;
pub mod rest;
pub mod sse;
mod variant;

pub use aggregator::StreamingResponseAggregator;
pub use convert::llm_response_from_genai;
pub use error::{ModelError, api_error_from_body, body_prefix};
pub use go_json::to_go_json;
pub use http::{
    BodyStream, HttpClient, ReqwestClient, collect_body, error_chain, redact_url,
    transport_error_detail,
};
pub use live::{LiveConnError, LiveConnErrorKind, LiveConnection, RealtimeInput};
pub use llm::{Backend, ExtrasRequestProvider, HttpOptions, Llm, LlmRequest, LlmResponse};
pub use registry::*;
pub use variant::{
    can_use_output_schema_with_tools, google_variant, is_gemini_api_variant, is_gemini_model,
    is_gemini1_model, is_gemini2_or_above, is_gemini25_or_lower, needs_output_schema_processor,
};
