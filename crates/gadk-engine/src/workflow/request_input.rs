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

//! Port of Go workflow/request_input.go: human-in-the-loop input requests.

use gadk_agent::{AgentError, Context, InvocationContext};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_session::{Event, RequestInput};
use serde_json::{Map, Value};

use crate::error::WorkflowErrorKind;
use crate::workflow::Emit;
use crate::workflow::scheduler::wf_error;

/// Port of Go WorkflowInputFunctionCallName: the function-call name of an input request (a wire
/// identifier shared with other ADK runtimes).
pub const WORKFLOW_INPUT_FUNCTION_CALL_NAME: &str = "adk_request_input";

/// Port of Go NewRequestInputEvent: an event that pauses the workflow for human input. An empty
/// interrupt id gets a fresh platform UUID. The request rides as a long-running
/// `adk_request_input` function call whose id is the interrupt id.
pub fn new_request_input_event(ctx: &InvocationContext, mut req: RequestInput) -> Event {
    if req.interrupt_id.is_empty() {
        req.interrupt_id = ctx.platform().new_uuid();
    }
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    let mut args = Map::new();
    args.insert(
        "interruptId".into(),
        Value::String(req.interrupt_id.clone()),
    );
    args.insert("message".into(), Value::String(req.message.clone()));
    args.insert("payload".into(), req.payload.clone().unwrap_or(Value::Null));
    if let Some(schema) = &req.response_schema {
        args.insert("responseSchema".into(), schema.clone());
    }
    ev.response.content = Some(Content {
        role: "model".into(),
        parts: vec![Part {
            function_call: Some(FunctionCall {
                id: req.interrupt_id.clone(),
                name: WORKFLOW_INPUT_FUNCTION_CALL_NAME.into(),
                args,
                ..FunctionCall::default()
            }),
            ..Part::default()
        }],
    });
    ev.long_running_tool_ids = vec![req.interrupt_id.clone()];
    // Go sets the author only when ctx.Agent() != nil; a Rust context always has an agent.
    ev.author = ctx.agent().name().to_string();
    ev.branch = ctx.branch();
    ev.requested_input = Some(req);
    ev
}

/// Port of Go ResumeOrRequestInput: the reply when the node is re-run after the human answered;
/// otherwise emits the request and returns the NodeInterrupted sentinel (an emit failure is
/// returned instead).
pub async fn resume_or_request_input(
    ctx: &Context,
    emit: &Emit,
    req: RequestInput,
) -> Result<Option<Value>, AgentError> {
    if let Some(reply) = ctx.resumed_input(&req.interrupt_id) {
        return Ok(Some(reply));
    }
    emit(new_request_input_event(ctx.as_invocation(), req)).await?;
    Err(wf_error(WorkflowErrorKind::NodeInterrupted, ""))
}
