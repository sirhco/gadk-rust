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

//! Go `internal/configurable/conformance/callbacks.go`: the conformance agents' before/after
//! agent callbacks, registered under `callback_agent_00N.callbacks.*`.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_agent::{AgentError, Context};
use gadk_genai::Content;

use crate::{ConfigError, Registry};

/// Go `beforeAgentCallback1`: sets `before_agent_callback_state_key` to `value1`.
pub(crate) async fn before_agent_callback1(ctx: Context) -> Result<Option<Content>, AgentError> {
    // Go: callbacks.go beforeAgentCallback1
    todo!("Task 11")
}

/// Go `beforeAgentCallback2`: appends `+value2` to `before_agent_callback_state_key`
/// (`state value for 'before_agent_callback_state_key' is not a string, but %T`).
pub(crate) async fn before_agent_callback2(ctx: Context) -> Result<Option<Content>, AgentError> {
    // Go: callbacks.go beforeAgentCallback2
    todo!("Task 11")
}

/// Go `shortcutAgentExecution`: sets `conversation_limit_reached` to `True` on the first call,
/// then answers `Sorry, you have reached the limit of the conversation.` as the model.
pub(crate) async fn shortcut_agent_execution(ctx: Context) -> Result<Option<Content>, AgentError> {
    // Go: callbacks.go shortcutAgentExecution
    todo!("Task 11")
}

/// Go `afterAgentCallback1`: sets `after_agent_callback_state_key` to `value1`.
pub(crate) async fn after_agent_callback1(ctx: Context) -> Result<Option<Content>, AgentError> {
    // Go: callbacks.go afterAgentCallback1
    todo!("Task 11")
}

/// Go `afterAgentCallback2`: appends `+value2` to `after_agent_callback_state_key`.
pub(crate) async fn after_agent_callback2(ctx: Context) -> Result<Option<Content>, AgentError> {
    // Go: callbacks.go afterAgentCallback2
    todo!("Task 11")
}

/// Go `RegisterCallbacks`: the five callbacks under their Go names
/// (`error registering before agent callback 1: %w`, …).
pub fn register_callbacks(registry: &Registry) -> Result<(), ConfigError> {
    // Go: callbacks.go RegisterCallbacks
    todo!("Task 11")
}
