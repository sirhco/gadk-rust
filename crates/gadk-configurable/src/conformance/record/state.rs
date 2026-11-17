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

//! The per-invocation record state (the lock guards the mutable fields:
//! [`RecordInner`]).

use std::collections::HashMap;

use gadk_model::LlmResponse;
use parking_lot::Mutex;

use crate::conformance::recording::{
    LlmRecording, LlmRequestView, RecordedFunctionCall, RecordedFunctionResponse, Recording,
    ToolRecording,
};

/// The record state of one invocation.
pub(crate) struct InvocationRecordState {
    /// The case directory the recordings are written to.
    pub(crate) case_dir: String,
    /// The index of the user message being recorded.
    pub(crate) user_message_index: i64,
    /// The streaming mode (`none` by default, `sse` selects the SSE file name).
    pub(crate) streaming_mode: String,
    /// The mutable fields, behind a lock.
    pub(crate) inner: Mutex<RecordInner>,
}

/// The mutable fields of [`InvocationRecordState`], guarded by its lock.
#[derive(Default)]
pub(crate) struct RecordInner {
    /// Completed recordings of the current turn.
    pub(crate) recordings: Vec<Recording>,
    /// The LLM request awaiting its responses.
    pub(crate) pending_llm_request: Option<LlmRequestView>,
    /// The responses collected for the pending request.
    pub(crate) pending_llm_responses: Vec<LlmResponse>,
    /// The tool calls awaiting their results, keyed by function call id.
    pub(crate) pending_tool_calls: HashMap<String, RecordedFunctionCall>,
}

impl InvocationRecordState {
    /// Creates the state for an invocation.
    pub(crate) fn new(case_dir: String, msg_index: i64, streaming_mode: String) -> Self {
        InvocationRecordState {
            case_dir,
            user_message_index: msg_index,
            streaming_mode,
            inner: Mutex::new(RecordInner::default()),
        }
    }

    /// Replaces the pending request and clears pending responses.
    pub(crate) fn start_llm_recording(&self, req: LlmRequestView) {
        let mut inner = self.inner.lock();
        inner.pending_llm_request = Some(req);
        inner.pending_llm_responses = Vec::new();
    }

    /// Appends a response to the pending ones.
    pub(crate) fn append_llm_response(&self, resp: LlmResponse) {
        self.inner.lock().pending_llm_responses.push(resp);
    }

    /// Appends the LLM recording (no-op without a pending request).
    pub(crate) fn complete_llm_recording(&self, agent_name: &str) {
        let mut inner = self.inner.lock();
        let Some(llm_request) = inner.pending_llm_request.take() else {
            return;
        };
        let llm_responses = std::mem::take(&mut inner.pending_llm_responses);
        inner.recordings.push(Recording {
            user_message_index: self.user_message_index,
            agent_name: agent_name.to_owned(),
            llm_recording: Some(LlmRecording {
                llm_request: Some(llm_request),
                llm_responses,
            }),
            tool_recording: None,
            index: 0,
        });
    }

    /// Records a tool call as pending under its id.
    pub(crate) fn start_tool_recording(&self, id: &str, fc: RecordedFunctionCall) {
        self.inner
            .lock()
            .pending_tool_calls
            .insert(id.to_owned(), fc);
    }

    /// Appends the tool recording for a pending call `id`.
    pub(crate) fn complete_tool_recording(
        &self,
        id: &str,
        agent_name: &str,
        resp: RecordedFunctionResponse,
    ) {
        let mut inner = self.inner.lock();
        let Some(fc) = inner.pending_tool_calls.remove(id) else {
            return;
        };
        inner.recordings.push(Recording {
            user_message_index: self.user_message_index,
            agent_name: agent_name.to_owned(),
            llm_recording: None,
            tool_recording: Some(ToolRecording {
                tool_call: Some(fc),
                tool_response: Some(resp),
            }),
            index: 0,
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_genai::{Content, Part};

    use super::*;
    use crate::{GoMap, GoValue};

    fn request(model: &str) -> LlmRequestView {
        LlmRequestView {
            model: model.to_owned(),
            ..Default::default()
        }
    }

    fn response(text: &str, partial: bool) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                parts: vec![Part::from_text(text)],
                role: "model".to_owned(),
            }),
            partial,
            ..Default::default()
        }
    }

    fn call(id: &str, name: &str) -> RecordedFunctionCall {
        RecordedFunctionCall {
            id: id.to_owned(),
            name: name.to_owned(),
            args: Some(GoMap::from([("a".to_owned(), GoValue::Float(1.0))])),
            ..Default::default()
        }
    }

    fn reply(id: &str, name: &str) -> RecordedFunctionResponse {
        RecordedFunctionResponse {
            id: id.to_owned(),
            name: name.to_owned(),
            response: Some(GoMap::from([(
                "ok".to_owned(),
                GoValue::String(id.to_owned()),
            )])),
            ..Default::default()
        }
    }

    #[test]
    fn new_keeps_the_config_and_starts_empty() {
        let s = InvocationRecordState::new("dir".into(), 3, "sse".into());
        assert_eq!(
            (
                s.case_dir.as_str(),
                s.user_message_index,
                s.streaming_mode.as_str()
            ),
            ("dir", 3, "sse")
        );
        let inner = s.inner.lock();
        assert!(inner.recordings.is_empty());
        assert!(inner.pending_llm_request.is_none());
        assert!(inner.pending_llm_responses.is_empty());
        assert!(inner.pending_tool_calls.is_empty());
    }

    #[test]
    fn llm_recording_collects_partial_and_final_responses() {
        let s = InvocationRecordState::new("d".into(), 2, "none".into());
        s.start_llm_recording(request("m"));
        s.append_llm_response(response("a", true));
        s.append_llm_response(response("b", false));
        s.complete_llm_recording("agent");
        let inner = s.inner.lock();
        assert_eq!(
            inner.recordings,
            vec![Recording {
                user_message_index: 2,
                agent_name: "agent".to_owned(),
                llm_recording: Some(LlmRecording {
                    llm_request: Some(request("m")),
                    llm_responses: vec![response("a", true), response("b", false)],
                }),
                tool_recording: None,
                index: 0,
            }]
        );
        assert!(inner.pending_llm_request.is_none());
        assert!(inner.pending_llm_responses.is_empty());
    }

    #[test]
    fn start_llm_recording_drops_earlier_pending_responses() {
        let s = InvocationRecordState::new("d".into(), 0, "none".into());
        s.start_llm_recording(request("first"));
        s.append_llm_response(response("stale", true));
        s.start_llm_recording(request("second"));
        s.append_llm_response(response("fresh", false));
        s.complete_llm_recording("a");
        let inner = s.inner.lock();
        let llm = inner.recordings[0].llm_recording.as_ref().unwrap();
        assert_eq!(llm.llm_request, Some(request("second")));
        assert_eq!(llm.llm_responses, vec![response("fresh", false)]);
    }

    /// Go's `CompleteLLMRecording` returns early without a pending request and leaves the
    /// pending responses in place (the next `StartLLMRecording` clears them).
    #[test]
    fn complete_without_pending_request_is_a_no_op() {
        let s = InvocationRecordState::new("d".into(), 0, "none".into());
        s.append_llm_response(response("orphan", false));
        s.complete_llm_recording("a");
        let inner = s.inner.lock();
        assert!(inner.recordings.is_empty());
        assert_eq!(inner.pending_llm_responses, vec![response("orphan", false)]);
    }

    #[test]
    fn llm_recording_without_responses() {
        let s = InvocationRecordState::new("d".into(), 0, "none".into());
        s.start_llm_recording(request("m"));
        s.complete_llm_recording("a");
        let inner = s.inner.lock();
        let llm = inner.recordings[0].llm_recording.as_ref().unwrap();
        assert!(llm.llm_responses.is_empty());
    }

    #[test]
    fn tool_recordings_pair_by_function_call_id() {
        let s = InvocationRecordState::new("d".into(), 1, "none".into());
        s.start_tool_recording("id-1", call("id-1", "t1"));
        s.start_tool_recording("id-2", call("id-2", "t2"));
        // Completed out of order: each pairs with its own call.
        s.complete_tool_recording("id-2", "agent", reply("id-2", "t2"));
        s.complete_tool_recording("id-1", "agent", reply("id-1", "t1"));
        let inner = s.inner.lock();
        let tool = |i: usize| inner.recordings[i].tool_recording.clone().unwrap();
        assert_eq!(
            tool(0),
            ToolRecording {
                tool_call: Some(call("id-2", "t2")),
                tool_response: Some(reply("id-2", "t2")),
            }
        );
        assert_eq!(tool(1).tool_call, Some(call("id-1", "t1")));
        assert_eq!(inner.recordings[0].user_message_index, 1);
        assert_eq!(inner.recordings[0].agent_name, "agent");
        assert!(inner.recordings[0].llm_recording.is_none());
        assert!(inner.pending_tool_calls.is_empty());
    }

    #[test]
    fn complete_tool_recording_without_a_pending_call_is_a_no_op() {
        let s = InvocationRecordState::new("d".into(), 0, "none".into());
        s.complete_tool_recording("missing", "a", reply("missing", "t"));
        s.start_tool_recording("id", call("id", "t"));
        s.complete_tool_recording("id", "a", reply("id", "t"));
        // A second response for the same id has no pending call any more.
        s.complete_tool_recording("id", "a", reply("id", "t"));
        assert_eq!(s.inner.lock().recordings.len(), 1);
    }

    /// Go's map assignment: a second call with the same id replaces the pending one.
    #[test]
    fn start_tool_recording_replaces_a_pending_call_with_the_same_id() {
        let s = InvocationRecordState::new("d".into(), 0, "none".into());
        s.start_tool_recording("id", call("id", "old"));
        s.start_tool_recording("id", call("id", "new"));
        s.complete_tool_recording("id", "a", reply("id", "new"));
        let inner = s.inner.lock();
        let tool = inner.recordings[0].tool_recording.as_ref().unwrap();
        assert_eq!(tool.tool_call, Some(call("id", "new")));
    }
}
