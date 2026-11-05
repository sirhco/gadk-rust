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

//! gadk-gemini: Gemini API and Vertex AI model implementation.

mod auth;
mod config;
mod continuation;
mod env;
mod error;
mod model;
mod request;
mod sse;
mod transport;

pub use config::{Auth, CLOUD_PLATFORM_SCOPE, ClientConfig, ResolvedConfig, resolve};
pub use env::{EnvProvider, MapEnv, ProcessEnv};
pub use error::GeminiError;
pub use model::GeminiModel;

/// Internals exposed for the integration tests. Not part of the public API.
#[doc(hidden)]
pub mod internal {
    pub use crate::request::{build_request_body, preserve_empty_text_thought_signature_parts};
}
