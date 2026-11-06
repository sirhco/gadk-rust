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

//! Go no-op processors (port of Go internal/llminternal/other_processors.go): auth,
//! NL-planning and code-execution request processors, NL-planning and code-execution response
//! processors.

use futures::future::BoxFuture;
use gadk_agent::{AgentError, InvocationContext};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::Event;

use crate::llm::flow::Flow;

/// Port of other_processors.go authPreprocessor (Go TODO no-op; kept so the chain order stays
/// visible).
pub(crate) fn auth_preprocessor<'a>(
    _ctx: &'a InvocationContext,
    _req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async { Ok(Vec::new()) })
}

/// Port of other_processors.go nlPlanningRequestProcessor (Go TODO no-op; kept so the chain
/// order stays visible).
pub(crate) fn nl_planning_request_processor<'a>(
    _ctx: &'a InvocationContext,
    _req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async { Ok(Vec::new()) })
}

/// Port of other_processors.go codeExecutionRequestProcessor (Go TODO no-op; kept so the chain
/// order stays visible).
pub(crate) fn code_execution_request_processor<'a>(
    _ctx: &'a InvocationContext,
    _req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async { Ok(Vec::new()) })
}

/// Port of other_processors.go nlPlanningResponseProcessor (Go TODO no-op; kept so the chain
/// order stays visible).
pub(crate) fn nl_planning_response_processor(
    _ctx: &InvocationContext,
    _req: &LlmRequest,
    _resp: &mut LlmResponse,
) -> Result<(), AgentError> {
    Ok(())
}

/// Port of other_processors.go codeExecutionResponseProcessor (Go TODO no-op; kept so the chain
/// order stays visible).
pub(crate) fn code_execution_response_processor(
    _ctx: &InvocationContext,
    _req: &LlmRequest,
    _resp: &mut LlmResponse,
) -> Result<(), AgentError> {
    Ok(())
}
