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

//! The ADK version of ADK-Go `plugin/agentanalytics/bigquery_agent_analytics_plugin.go`
//! (`adkVersion`, `adkVersionFromBuildInfo`, `adkModulePath`). Go reads the linked ADK module's
//! version from the build info; gadk writes its own crate version. Divergence (ledger 7b-3a).

#![allow(dead_code, unused_variables)] // Task 7 implements the bodies.

/// Go `adkModulePath` (kept for the ported build-info test's table).
pub(crate) const ADK_MODULE_PATH: &str = "google.golang.org/adk/v2";

/// Go `adkVersion()`: the `adk_version` written in `attributes` (gadk-agentanalytics'
/// `CARGO_PKG_VERSION`).
pub(crate) fn adk_version() -> &'static str {
    todo!("Task 7")
}
