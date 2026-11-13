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

//! Go `internal/configurable/conformance/recordplugin/record_plugin.go`: the conformance
//! record plugin (`record_plugin`), which writes each invocation's LLM and tool interactions
//! to `generated-recordings.yaml` (`generated-recordings-sse.yaml` for `sse` streaming) in the
//! case directory named by the session state's `_adk_recordings_config`. The YAML
//! post-processing lives in [`clean`], the per-invocation state in `state`.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub mod clean;
pub(crate) mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gadk_agent::{AgentError, Context, InvocationContext, Plugin, Tool, ToolError};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::State;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use self::state::InvocationRecordState;
use crate::conformance::recording::LlmRequestView;

/// Go `recordPlugin`.
pub(crate) struct RecordPlugin {
    /// Go `mu` + `invocationStates`, keyed by invocation id.
    pub(crate) invocation_states: Mutex<HashMap<String, Arc<InvocationRecordState>>>,
    /// Go `allowedBaseDir`.
    pub(crate) allowed_base_dir: PathBuf,
}

/// Go `New(allowedBaseDir)` (and `MustNew`: Go's `plugin.New` error is always nil, so both are
/// this function): the `record_plugin` plugin with before/after run, model and tool callbacks.
pub fn new(allowed_base_dir: impl Into<PathBuf>) -> Plugin {
    // Go: record_plugin.go New
    todo!("Task 9")
}

impl RecordPlugin {
    /// Go `beforeRun`: creates the invocation's record state when record mode is on.
    pub(crate) fn before_run(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Option<Content>, AgentError> {
        // Go: record_plugin.go beforeRun
        todo!("Task 9")
    }

    /// Go `beforeModel`: starts an LLM recording with the sanitized request.
    pub(crate) fn before_model(
        &self,
        ctx: &Context,
        req: &LlmRequest,
    ) -> Result<Option<LlmResponse>, AgentError> {
        // Go: record_plugin.go beforeModel
        todo!("Task 9")
    }

    /// Go `afterModel`: appends the response; a non-partial response completes the recording.
    pub(crate) fn after_model(
        &self,
        ctx: &Context,
        resp: &LlmResponse,
        err: Option<&ModelError>,
    ) -> Result<Option<LlmResponse>, AgentError> {
        // Go: record_plugin.go afterModel
        todo!("Task 9")
    }

    /// Go `beforeTool`: starts a tool recording keyed by the function call id.
    pub(crate) fn before_tool(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        args: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        // Go: record_plugin.go beforeTool
        todo!("Task 9")
    }

    /// Go `afterTool`: completes the tool recording with the result.
    pub(crate) fn after_tool(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        args: &Map<String, Value>,
        result: &Map<String, Value>,
        err: Option<&ToolError>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        // Go: record_plugin.go afterTool
        todo!("Task 9")
    }

    /// Go `afterRun`: saves the recordings and drops the invocation's state.
    pub(crate) fn after_run(&self, ctx: &InvocationContext) {
        // Go: record_plugin.go afterRun
        todo!("Task 9")
    }

    /// Go `parseRecordConfig`: `(caseDir, msgIndex, streamingMode)`, `("", 0, "")` when off;
    /// `invalid path format: %v`,
    /// `record config error: 'dir' is not within the allowed base directory`,
    /// `record config 'user_message_index' is not a number`.
    pub(crate) fn parse_record_config(
        &self,
        session_state: &dyn State,
    ) -> Result<(String, i64, String), AgentError> {
        // Go: record_plugin.go parseRecordConfig
        todo!("Task 9")
    }

    /// Go `isRecordModeOn`.
    pub(crate) fn is_record_mode_on(&self, session_state: &dyn State) -> Result<bool, AgentError> {
        // Go: record_plugin.go isRecordModeOn
        todo!("Task 9")
    }

    /// Go `getInvocationState`: `record state not initialized for invocation: %s`.
    pub(crate) fn get_invocation_state(
        &self,
        id: &str,
    ) -> Result<Arc<InvocationRecordState>, AgentError> {
        // Go: record_plugin.go getInvocationState
        todo!("Task 9")
    }

    /// Go `createInvocationState`: `record state not configured` when record mode is off.
    pub(crate) fn create_invocation_state(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Arc<InvocationRecordState>, AgentError> {
        // Go: record_plugin.go createInvocationState
        todo!("Task 9")
    }

    /// Go `saveRecordings`: merge with the existing file (dropping the current
    /// `user_message_index`), encode, `cleanYAMLNode`, marshal, write `0o644`; every error is
    /// ignored, as in Go.
    pub(crate) fn save_recordings(&self, state: &InvocationRecordState) {
        // Go: record_plugin.go saveRecordings
        todo!("Task 9")
    }
}

/// Go `sanitizeLLMRequest`: the request without `Tools` and `Config.HTTPOptions` (gadk's
/// `LlmRequest.http_options`), as recorded.
pub(crate) fn sanitize_llm_request(req: &LlmRequest) -> LlmRequestView {
    // Go: record_plugin.go sanitizeLLMRequest
    todo!("Task 9")
}
