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

//! Port of Go internal/llminternal/base_flow.go: the `Flow` struct and processor types (Task 9);
//! the step loop (`impl Flow`) is added by Task 11.

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{
    AfterModelCallback, AfterToolCallback, AgentError, BeforeModelCallback, BeforeToolCallback,
    InvocationContext, OnModelErrorCallback, OnToolErrorCallback, Tool,
};
use gadk_model::{Llm, LlmRequest, LlmResponse};
use gadk_session::Event;

/// Port of Go `func(ctx agent.InvocationContext, req *model.LLMRequest, f *Flow) iter.Seq2[*session.Event, error]`.
/// Returns the events the processor emits; the flow yields them in order.
pub(crate) type RequestProcessor = for<'a> fn(
    &'a InvocationContext,
    &'a mut LlmRequest,
    &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>>;

/// Port of Go `func(ctx agent.InvocationContext, req *model.LLMRequest, resp *model.LLMResponse) error`.
pub(crate) type ResponseProcessor = for<'a> fn(
    &'a InvocationContext,
    &'a LlmRequest,
    &'a mut LlmResponse,
) -> Result<(), AgentError>;

/// Port of Go internal/llminternal/base_flow.go `Flow`.
#[derive(Default)]
pub(crate) struct Flow {
    pub(crate) model: Option<Arc<dyn Llm>>,
    /// Go `Flow.Tools`. The tools processor (slot 2) rewrites it on every step
    /// (`tools_processor.go:49`), and the request-confirmation processor and Task 11's
    /// `tool_preprocess` read it. Interior mutability is needed because processors get
    /// `&Flow` (C5 D5). Never hold the guard across an `.await`. There is no `toolsets`
    /// field: the processors read toolsets through `reveal(ctx.agent())` exactly as Go does.
    pub(crate) tools: parking_lot::RwLock<Vec<Arc<dyn Tool>>>,
    pub(crate) request_processors: Vec<RequestProcessor>,
    pub(crate) response_processors: Vec<ResponseProcessor>,
    pub(crate) before_model_callbacks: Vec<BeforeModelCallback>,
    pub(crate) after_model_callbacks: Vec<AfterModelCallback>,
    pub(crate) on_model_error_callbacks: Vec<OnModelErrorCallback>,
    pub(crate) before_tool_callbacks: Vec<BeforeToolCallback>,
    pub(crate) after_tool_callbacks: Vec<AfterToolCallback>,
    pub(crate) on_tool_error_callbacks: Vec<OnToolErrorCallback>,
}

// impl: Task 11
