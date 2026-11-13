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

//! Go `internal/configurable/conformance/recordplugin/invocation_record_state.go`: the
//! per-invocation record state (Go's `mu` guards the mutable fields: [`RecordInner`]).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;

use gadk_model::LlmResponse;
use parking_lot::Mutex;

use crate::conformance::recording::{
    LlmRequestView, RecordedFunctionCall, RecordedFunctionResponse, Recording,
};

/// Go `invocationRecordState`.
pub(crate) struct InvocationRecordState {
    /// Go `caseDir`.
    pub(crate) case_dir: String,
    /// Go `userMessageIndex`.
    pub(crate) user_message_index: i64,
    /// Go `streamingMode` (`none` by default, `sse` selects the SSE file name).
    pub(crate) streaming_mode: String,
    /// Go `mu` and the fields it guards.
    pub(crate) inner: Mutex<RecordInner>,
}

/// The fields of Go's `invocationRecordState` guarded by `mu`.
#[derive(Default)]
pub(crate) struct RecordInner {
    /// Go `recordings`: completed recordings of the current turn.
    pub(crate) recordings: Vec<Recording>,
    /// Go `pendingLLMRequest`.
    pub(crate) pending_llm_request: Option<LlmRequestView>,
    /// Go `pendingLLMResponses`.
    pub(crate) pending_llm_responses: Vec<LlmResponse>,
    /// Go `pendingToolCalls`, keyed by function call id.
    pub(crate) pending_tool_calls: HashMap<String, RecordedFunctionCall>,
}

impl InvocationRecordState {
    /// Go `newInvocationRecordState`.
    pub(crate) fn new(case_dir: String, msg_index: i64, streaming_mode: String) -> Self {
        // Go: invocation_record_state.go newInvocationRecordState
        todo!("Task 9")
    }

    /// Go `StartLLMRecording`: replaces the pending request and clears pending responses.
    pub(crate) fn start_llm_recording(&self, req: LlmRequestView) {
        // Go: invocation_record_state.go StartLLMRecording
        todo!("Task 9")
    }

    /// Go `AppendLLMResponse`.
    pub(crate) fn append_llm_response(&self, resp: LlmResponse) {
        // Go: invocation_record_state.go AppendLLMResponse
        todo!("Task 9")
    }

    /// Go `CompleteLLMRecording`: appends the LLM recording (no-op without a pending request).
    pub(crate) fn complete_llm_recording(&self, agent_name: &str) {
        // Go: invocation_record_state.go CompleteLLMRecording
        todo!("Task 9")
    }

    /// Go `StartToolRecording`.
    pub(crate) fn start_tool_recording(&self, id: &str, fc: RecordedFunctionCall) {
        // Go: invocation_record_state.go StartToolRecording
        todo!("Task 9")
    }

    /// Go `CompleteToolRecording`: appends the tool recording for a pending call `id`.
    pub(crate) fn complete_tool_recording(
        &self,
        id: &str,
        agent_name: &str,
        resp: RecordedFunctionResponse,
    ) {
        // Go: invocation_record_state.go CompleteToolRecording
        todo!("Task 9")
    }
}
