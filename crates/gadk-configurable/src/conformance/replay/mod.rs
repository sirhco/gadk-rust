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

//! Go `internal/configurable/conformance/replayplugin/replay_plugin.go`: the conformance replay
//! plugin (`replay_plugin`), which answers model and tool calls from the case directory's
//! `generated-recordings.yaml` (named by the session state's `_adk_replay_config`), verifying
//! each request against the recording. [`normalize`] ports `yaml_utils.go`; `matching` holds
//! the request/tool-call comparison; `state` the per-invocation state.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub(crate) mod matching;
pub mod normalize;
pub(crate) mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gadk_agent::{AgentError, Context, InvocationContext, Plugin, Tool};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::State;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use self::state::InvocationReplayState;
use crate::conformance::recording::{LlmRecording, Recording, ToolRecording};

/// Go `replayPlugin`.
pub(crate) struct ReplayPlugin {
    /// Go `mu` + `invocationStates`, keyed by invocation id.
    pub(crate) invocation_states: Mutex<HashMap<String, Arc<InvocationReplayState>>>,
    /// Go `allowedBaseDir`.
    pub(crate) allowed_base_dir: PathBuf,
}

/// Go `New(allowedBaseDir)` (and `MustNew`: Go's `plugin.New` error is always nil): the
/// `replay_plugin` plugin with before run/model/tool and after run callbacks.
pub fn new(allowed_base_dir: impl Into<PathBuf>) -> Plugin {
    // Go: replay_plugin.go New
    todo!("Task 10")
}

impl ReplayPlugin {
    /// Go `beforeRun`: loads the invocation's replay state when replay mode is on.
    pub(crate) fn before_run(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Option<Content>, AgentError> {
        // Go: replay_plugin.go beforeRun
        todo!("Task 10")
    }

    /// Go `beforeModel`: the next LLM recording's first response, after verifying the request
    /// (`no LLM responses found in recording for agent %q`).
    pub(crate) async fn before_model(
        &self,
        ctx: &Context,
        req: &LlmRequest,
    ) -> Result<Option<LlmResponse>, AgentError> {
        // Go: replay_plugin.go beforeModel
        todo!("Task 10")
    }

    /// Go `beforeTool`: the matching tool recording's response; non-agent function tools are
    /// still run (errors printed as `Error calling tool: <err>`).
    pub(crate) async fn before_tool(
        &self,
        ctx: &Context,
        tool: Arc<dyn Tool>,
        args: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        // Go: replay_plugin.go beforeTool
        todo!("Task 10")
    }

    /// Go `afterRun`: drops the invocation's state.
    pub(crate) fn after_run(&self, ctx: &InvocationContext) {
        // Go: replay_plugin.go afterRun
        todo!("Task 10")
    }

    /// Go `isReplayModeOn`: `invalid path format: %v`,
    /// `replay config error: 'dir' is not within the allowed base directory`.
    pub(crate) fn is_replay_mode_on(&self, session_state: &dyn State) -> Result<bool, AgentError> {
        // Go: replay_plugin.go isReplayModeOn
        todo!("Task 10")
    }

    /// Go `getInvocationState`: `replay state not initialized. ensure before_run created it`.
    pub(crate) fn get_invocation_state(
        &self,
        ctx: &Context,
    ) -> Result<Arc<InvocationReplayState>, AgentError> {
        // Go: replay_plugin.go getInvocationState
        todo!("Task 10")
    }

    /// Go `loadInvocationState`: validates `_adk_replay_config`, reads
    /// `generated-recordings.yaml`, `normalizeYAMLNode`, decodes, assigns per-message LLM
    /// indices (`-1` for tool recordings).
    pub(crate) fn load_invocation_state(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Arc<InvocationReplayState>, AgentError> {
        // Go: replay_plugin.go loadInvocationState
        todo!("Task 10")
    }

    /// Go `verifyAndGetNextLLMRecordingForAgent`: `expected LLM recording for agent '%s' at
    /// index %d, but found tool recording`, then `verifyLLMRequestMatch`.
    pub(crate) async fn verify_and_get_next_llm_recording_for_agent(
        &self,
        state: Arc<InvocationReplayState>,
        agent_name: &str,
        llm_request: &LlmRequest,
    ) -> Result<LlmRecording, AgentError> {
        // Go: replay_plugin.go verifyAndGetNextLLMRecordingForAgent
        todo!("Task 10")
    }

    /// Go `verifyAndGetNextToolRecordingForAgent`.
    pub(crate) fn verify_and_get_next_tool_recording_for_agent(
        &self,
        state: &InvocationReplayState,
        agent_name: &str,
        tool: &dyn Tool,
        args: &Map<String, Value>,
    ) -> Result<ToolRecording, AgentError> {
        // Go: replay_plugin.go verifyAndGetNextToolRecordingForAgent
        todo!("Task 10")
    }
}

/// Go `getNextRecordingForAgent`: the agent's next recording in order (`runtime sent more
/// requests than expected for agent '%s' at user_message_index %d. Expected %d, but got request
/// at index %d`), waiting on the condition variable until `curIndex` reaches its index, then
/// sleeping `index × 10ms`.
pub(crate) async fn get_next_recording_for_agent(
    state: Arc<InvocationReplayState>,
    agent_name: &str,
) -> Result<Recording, AgentError> {
    // Go: replay_plugin.go getNextRecordingForAgent
    todo!("Task 10")
}

/// Go `getNextToolRecordingForAgent`: the first unconsumed recording of the agent and user
/// message that `match_fn` accepts (marked consumed), else the first match error, else
/// `no matching tool recording found for agent '%s' at user_message_index %d`.
pub(crate) fn get_next_tool_recording_for_agent(
    state: &InvocationReplayState,
    agent_name: &str,
    match_fn: &mut dyn FnMut(&Recording) -> Result<bool, String>,
) -> Result<Recording, AgentError> {
    // Go: replay_plugin.go getNextToolRecordingForAgent
    todo!("Task 10")
}
