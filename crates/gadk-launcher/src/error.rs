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

//! Launcher errors; `Display` is the error text ADK launchers print.

/// A launcher failure; `Display` is the text ADK launchers print.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A command-line parsing or routing failure (sub-launcher parse errors, the universal
    /// launcher's routing errors).
    #[error("{0}")]
    Parse(String),
    /// An unusable configuration (sub-launcher setup, flag validation).
    #[error("{0}")]
    Config(String),
    /// The server or console failed while running.
    #[error("{0}")]
    Serve(String),
    /// Telemetry setup failed.
    #[error("{0}")]
    Telemetry(String),
    /// A source error with a context prefix, displayed as `<context>: <source>`.
    #[error("{context}: {source}")]
    Wrapped {
        /// The text before `: `.
        context: String,
        /// The wrapped error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
