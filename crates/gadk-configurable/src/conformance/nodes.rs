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

//! Go `internal/configurable/conformance/nodes.go`: the `conformance.uppercase_formatter`
//! node function.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_agent::{AgentError, Context};
use serde_json::Value;

use crate::{ConfigError, Registry};

/// Go `uppercaseFormatter`: a string is uppercased; a map has its string values uppercased;
/// any other input goes through `typeutil.ConvertToWithJSONSchema[any, map[string]any]`
/// (JSON `null` → an empty map), else `node_input must be a string or a map/dict`.
pub(crate) async fn uppercase_formatter(ctx: Context, input: Value) -> Result<Value, AgentError> {
    // Go: nodes.go uppercaseFormatter
    todo!("Task 11")
}

/// Go `RegisterNodeFunctions`: registers `conformance.uppercase_formatter`; Go's error is
/// always nil.
pub fn register_node_functions(registry: &Registry) -> Result<(), ConfigError> {
    // Go: nodes.go RegisterNodeFunctions
    todo!("Task 11")
}
