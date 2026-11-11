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

//! The ADK A2A extension marker (port of ADK-Go `server/adka2a/v2/extension.go`). Bodies: plan
//! 5b Task 3.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;

use serde_json::Value;

/// Go `ADKExtensionURI`: the metadata key the executor writes on every task and task status
/// update it emits, telling a client that the payload follows ADK conventions.
pub const ADK_EXTENSION_URI: &str = "https://google.github.io/adk-docs/a2a/a2a-extension/";

/// Go `addADKExtensionMeta`: adds [`ADK_EXTENSION_URI`] to a task's or task status update's
/// metadata; other events are left alone.
pub(crate) fn add_adk_extension_meta(event: &mut a2a::StreamResponse) {
    todo!("plan 5b Task 3")
}

/// Go `metaWithADKExtension`: a copy of `meta` holding [`ADK_EXTENSION_URI`] = `true`.
pub(crate) fn meta_with_adk_extension(
    meta: Option<HashMap<String, Value>>,
) -> HashMap<String, Value> {
    todo!("plan 5b Task 3")
}
