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
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{AgentError, InvocationContext, Mode};
use gadk_model::LlmRequest;
use gadk_session::Event;

use crate::{
    llm::{flow::Flow, mode::mode_for, state::reveal},
    workflow::route::go_quote,
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
            "You are an agent. Your internal name is {}.",
            go_quote(agent.name())
        )];
        if !agent.description().is_empty() {
            parts.push(format!(
                "The description about you is {}.",
                go_quote(agent.description())
            ));
        }
        append_instructions(req, &[parts.join(" ").as_str()]);
        Ok(Vec::new())
    })
}

#[cfg(test)]
mod tests {
    use gadk_agent::InvocationContext;
    use gadk_model::LlmRequest;
    use pretty_assertions::assert_eq;

    use super::identity_request_processor;
    use crate::llm::{config::LlmAgentConfig, flow::Flow, processors::test_llm_agent};

    /// Go `%q` leaves printable runes (a combining mark included) raw and escapes the rest; the
    /// expected text is Go's `fmt.Sprintf` output.
    #[tokio::test]
    async fn identity_text_quotes_like_go() {
        let agent = test_llm_agent(LlmAgentConfig {
            name: "cafe\u{301}".into(),
            description: "bell \u{7} shy \u{ad} \u{e9}".into(),
            ..Default::default()
        });
        let ctx = InvocationContext::builder().agent(agent).build_invocation();
        let mut req = LlmRequest::default();
        identity_request_processor(&ctx, &mut req, &Flow::default())
            .await
            .unwrap();
        let si = req.config.unwrap().system_instruction.unwrap();
        assert_eq!(
            si.parts[0].text,
            "You are an agent. Your internal name is \"cafe\u{301}\". The description about you is \"bell \\a shy \\u00ad \u{e9}\"."
        );
    }
}
