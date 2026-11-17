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

//! The ADK version written into each row: gadk reports its own crate version (every gadk crate
//! shares the workspace version) rather than the version of a linked ADK module.

/// The module path of the upstream ADK (used by the build-info test).
#[cfg(test)]
pub(crate) const ADK_MODULE_PATH: &str = "google.golang.org/adk/v2";

/// The `adk_version` written in `attributes` (gadk-agentanalytics' `CARGO_PKG_VERSION`).
pub(crate) fn adk_version() -> &'static str {
    // Every gadk crate shares the workspace version, so this is gadk's.
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adk_version_is_the_crate_version() {
        assert_eq!(adk_version(), env!("CARGO_PKG_VERSION"));
        assert!(!adk_version().is_empty());
        assert!(!adk_version().starts_with('v'));
    }
}
