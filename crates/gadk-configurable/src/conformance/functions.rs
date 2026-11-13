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

//! Go `internal/configurable/conformance/functions.go`: the conformance agents' function
//! tools (`validate_email`, `get_user_id`, `create_booking`, `search_flights`,
//! `calculate_trip_cost`, `reimburse`, `ask_for_approval`), registered as tool factories under
//! `tools_agent_00N.tools.*`. Task 11 defines the argument structs (serde + schemars, Go's
//! `json` tags as field names) and the functions next to [`register_functions`].

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use crate::{ConfigError, Registry};

/// Go `RegisterFunctions`: builds the seven `functiontool.New` tools
/// (`error creating validate email tool: %w`, …) and registers a factory returning each
/// (`error registering validate email tool: %w`, …).
pub fn register_functions(registry: &Registry) -> Result<(), ConfigError> {
    // Go: functions.go RegisterFunctions
    todo!("Task 11")
}
