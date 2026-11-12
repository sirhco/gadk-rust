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

//! The telemetry error: `Display` is ADK-Go's error text, `%w` chains included.

/// A telemetry setup or shutdown failure (Go's `error` values from `telemetry`); `Display` is
/// Go's text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

impl Error {
    /// An error with `text` as its message.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The message (the `Display` text).
    pub fn message(&self) -> &str {
        &self.0
    }
}
