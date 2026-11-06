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

//! Port of Go agent/llmagent/llm_agent_wrapper.go (RunLLMAgentAsNode). PROVISIONAL body by Task 11;
//! Task 15 replaces it with the real port.

use std::sync::Arc;

use gadk_agent::{Agent, Context, EventStream};
use serde_json::Value;

/// Port of Go llmagent.RunLLMAgentAsNode (C4 signature, not async).
/// PROVISIONAL (Task 11): runs the agent directly on the node context's invocation context and
/// ignores `input`. Task 15 replaces this body with the chat/task/single_turn port.
pub(crate) fn run_llm_agent_as_node(
    ctx: Context,
    agent: Arc<dyn Agent>,
    input: Value,
) -> EventStream {
    let _ = input;
    agent.run(ctx.as_invocation().clone())
}
