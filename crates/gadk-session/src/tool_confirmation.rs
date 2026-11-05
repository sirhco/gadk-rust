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

//! Port of ADK-Go `tool/toolconfirmation.ToolConfirmation`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The state and details of a user confirmation request for a tool execution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolConfirmation {
    /// Message explaining to the user why confirmation is needed.
    #[serde(default)]
    pub hint: String,
    /// The user's decision: `true` if approved.
    #[serde(default)]
    pub confirmed: bool,
    /// Application-specific data related to the request (Go `any`; `None` encodes as `null`).
    #[serde(default)]
    pub payload: Option<Value>,
}
