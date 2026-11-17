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

//! Session errors: a missing session and a missing state key.

/// Errors returned by [`crate::SessionService`], [`crate::Session`] and [`crate::State`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The requested session does not exist.
    #[error("session not found")]
    NotFound,
    /// The state key does not exist.
    #[error("state key does not exist")]
    StateKeyNotExist,
    /// The request failed validation.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// A storage backend failure.
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync>),
}
