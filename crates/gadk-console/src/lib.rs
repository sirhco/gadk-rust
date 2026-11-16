// Copyright 2026 Chris Olson
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

//! A local web console for gadk: stored traces from Cloud Trace and Cloud Logging, and the
//! prompt registry.

#[allow(dead_code)] // removed in Task 8 once the web layer uses it
mod model;
#[allow(dead_code)] // removed in Task 8
mod prompts;
pub mod traces;
#[allow(dead_code)] // removed in Task 8
mod web;

/// Console settings (from the command line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleConfig {
    /// GCP project whose traces and logs are read.
    pub project: String,
    /// Prompt store URL (`file://…` or `firestore://…`); `None` disables the prompt pages.
    pub store: Option<String>,
    /// Loopback port; 0 picks a free one.
    pub port: u16,
    /// Cloud Logging log id that gadk-telemetry writes (default `gadk`).
    pub log_id: String,
}

/// Startup failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsoleError {
    #[error("prompt store: {0}")]
    Store(#[from] gadk_prompts::StoreError),
    #[error("bind 127.0.0.1:{port}: {detail}")]
    Bind { port: u16, detail: String },
    #[error("serve: {0}")]
    Serve(String),
}
