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

//! Identity request processor (port of Go internal/llminternal/identity_request_processor.go).

use futures::future::BoxFuture;
use gadk_agent::{AgentError, InvocationContext, Mode};
use gadk_model::LlmRequest;
use gadk_session::Event;

use crate::{
    llm::{flow::Flow, mode::mode_for, state::reveal},
    tool::utils::append_instructions,
};

/// Port of Go identity_request_processor.go:29 identityRequestProcessor: gives the agent its
/// identity from the framework.
pub(crate) fn identity_request_processor<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        if reveal(&*agent).is_none() {
            return Ok(Vec::new());
        }
        // RESOLVED mode (Go ModeFor).
        if mode_for(ctx, &*agent) == Mode::SingleTurn {
            return Ok(Vec::new());
        }
        let mut parts = vec![format!(
            "You are an agent. Your internal name is {:?}.",
            agent.name()
        )];
        if !agent.description().is_empty() {
            parts.push(format!(
                "The description about you is {:?}.",
                agent.description()
            ));
        }
        append_instructions(req, &[parts.join(" ").as_str()]);
        Ok(Vec::new())
    })
}
