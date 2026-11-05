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

//! Errors produced by the Gemini backend.

use gadk_model::ModelError;

/// Errors produced by `gadk-gemini`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GeminiError {
    /// Invalid or incomplete client configuration.
    #[error("{0}")]
    Config(String),
    /// Credentials could not be obtained.
    #[error("auth error: {0}")]
    Auth(String),
    /// Live (bidi streaming) session failure.
    #[error("live error: {0}")]
    Live(String),
    /// Error from the model layer.
    #[error(transparent)]
    Model(#[from] ModelError),
}
