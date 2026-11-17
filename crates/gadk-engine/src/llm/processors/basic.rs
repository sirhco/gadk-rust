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

//! Basic request processor: copies the agent's generate-content config into the request.

use futures::future::BoxFuture;
use gadk_agent::{AgentError, InvocationContext, Mode};
use gadk_genai::GenerateContentConfig;
use gadk_model::LlmRequest;
use gadk_session::Event;

use crate::llm::{
    flow::Flow, output_schema_binding::output_schema_for,
    processors::output_schema::need_output_schema_processor, state::reveal,
};

/// Sets the request config from the agent's `generate_content_config`, plus the response schema
/// where one applies.
pub(crate) fn basic_request_processor<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        let Some(state) = reveal(&*agent) else {
            return Ok(Vec::new());
        };
        // The request config is a clone of the agent's `generate_content_config`, replacing
        // whatever was there; `None` becomes an empty config.
        let mut cfg = clone_generate_content_config(state.cfg.generate_content_config.as_ref())
            .unwrap_or_default();
        // Task-mode agents never set ResponseSchema (the finish_task declaration carries it).
        // Reads the DECLARED mode (see the `mode` module docs).
        // The schema is the declared one, else one a typed workflow node bound.
        if state.cfg.mode != Mode::Task
            && !need_output_schema_processor(state)
            && let Some(schema) = output_schema_for(ctx, state)
        {
            cfg.response_schema = Some(schema);
            cfg.response_mime_type = "application/json".to_owned();
        }
        req.config = Some(cfg);
        Ok(Vec::new())
    })
}

/// Clones the config; `None` stays `None`.
pub(crate) fn clone_generate_content_config(
    cfg: Option<&GenerateContentConfig>,
) -> Option<GenerateContentConfig> {
    cfg.cloned()
}
