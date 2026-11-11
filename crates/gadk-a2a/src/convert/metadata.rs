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

//! Metadata conversions (port of ADK-Go `server/adka2a/v2/metadata.go`, without
//! `toInvocationMeta`, which the executor owns). Bodies: plan 5b Task 3.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;

use gadk_session::{Event, EventActions};
use serde_json::Value;

use crate::Error;

/// Go `ToA2AMetaKey`.
pub fn to_a2a_meta_key(key: &str) -> String {
    todo!("plan 5b Task 3")
}

/// Go `ToADKMetaKey`.
pub fn to_adk_meta_key(key: &str) -> String {
    todo!("plan 5b Task 3")
}

/// Go `TransferToAgentFromMeta`.
pub fn transfer_to_agent_from_meta(meta: Option<&HashMap<String, Value>>) -> Option<String> {
    todo!("plan 5b Task 3")
}

/// Go `toEventActions`.
pub(crate) fn to_event_actions(meta: Option<&HashMap<String, Value>>) -> EventActions {
    todo!("plan 5b Task 3")
}

/// Go `setActionsMeta`.
pub(crate) fn set_actions_meta(meta: &mut HashMap<String, Value>, actions: &EventActions) {
    todo!("plan 5b Task 3")
}

/// Go `processA2AMeta`.
pub(crate) fn process_a2a_meta(
    a2a_event: &a2a::StreamResponse,
    event: &mut Event,
) -> Result<(), Error> {
    todo!("plan 5b Task 3")
}
