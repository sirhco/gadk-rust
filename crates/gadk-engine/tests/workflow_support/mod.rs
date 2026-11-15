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
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Helpers shared by the external runner/workflow targets: ports of the Go runner tests'
//! `resumeContent`, `joinResumeAgent`, `joinResumeAgentNode` and `scriptedModel`
//! (ADK-Go runner/*_test.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{Agent, AgentConfig, AgentError, AgentRunFn, EventStream, InvocationContext};
use gadk_engine::workflow::{AgentNode, Emit, Node, NodeConfig, resume_or_request_input};
use gadk_genai::{Content, FunctionResponse};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, RequestInput};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Port of Go `resumeContent`: one FunctionResponse with the payload under "payload".
pub fn resume_content(call_id: &str, call_name: &str, payload: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            id: call_id.into(),
            name: call_name.into(),
            response: json!({ "payload": payload })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        },
        "user",
    )
}

/// Go `joinResumeAgent`: a custom agent that either pauses via ResumeOrRequestInput
/// (interrupt id = its name) and then answers `output`, or answers `output` at once.
pub fn join_resume_agent(name: &str, output: &str, interrupt: bool) -> Arc<dyn Agent> {
    let (agent_name, output) = (name.to_string(), output.to_string());
    let run_fn: AgentRunFn = Arc::new(move |ctx: InvocationContext| -> EventStream {
        let (name, output) = (agent_name.clone(), output.clone());
        Box::pin(async_stream::stream! {
            if interrupt {
                // Go's emit yields straight to the consumer; ResumeOrRequestInput emits at most one
                // event and then returns, so buffering it until the call returns keeps the order.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
                let emit: Emit = Arc::new(move |ev: Event| {
                    let tx = tx.clone();
                    Box::pin(async move { tx.send(ev).map_err(|_| AgentError::other("consumer stopped")) })
                });
                let res = resume_or_request_input(
                    &ctx.clone().into_context(),
                    &emit,
                    RequestInput { interrupt_id: name.clone(), message: "approve".into(), ..Default::default() },
                )
                .await;
                drop(emit);
                while let Ok(ev) = rx.try_recv() {
                    yield Ok::<Event, AgentError>(ev);
                }
                if res.is_err() {
                    return;
                }
                // Go yields a bare &session.Event{LLMResponse: …}; the agent wrapper fills author/ids.
                let mut ev = Event::default();
                ev.response.content = Some(Content::from_text(output.clone(), "model"));
                yield Ok(ev);
                return;
            }
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.response.content = Some(Content::from_text(output.clone(), "model"));
            yield Ok(ev);
        })
    });
    gadk_agent::agent::new(AgentConfig {
        name: name.into(),
        run: Some(run_fn),
        ..Default::default()
    })
    .expect("agent.New")
}

/// Go `joinResumeAgentNode`.
pub fn agent_node(a: &Arc<dyn Agent>) -> Arc<dyn Node> {
    AgentNode::new(a.clone(), NodeConfig::default()).expect("workflow.NewAgentNode()")
}

/// Port of Go `scriptedModel`: one response per call; past the end, repeats the last.
pub struct ScriptedModel {
    responses: Vec<Content>,
    call: Mutex<usize>,
}

impl ScriptedModel {
    pub fn new(responses: Vec<Content>) -> Arc<Self> {
        Arc::new(Self {
            responses,
            call: Mutex::new(0),
        })
    }

    /// [`ScriptedModel::new`] as a trait object.
    pub fn arc(responses: Vec<Content>) -> Arc<dyn Llm> {
        Self::new(responses)
    }

    /// How many times the model has been called.
    pub fn calls(&self) -> usize {
        *self.call.lock()
    }
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut call = self.call.lock();
        let i = (*call).min(self.responses.len() - 1);
        *call += 1;
        let resp = LlmResponse {
            content: Some(self.responses[i].clone()),
            ..Default::default()
        };
        futures::stream::iter([Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
