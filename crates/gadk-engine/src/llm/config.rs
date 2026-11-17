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

//! `LlmAgentConfig`, `InstructionProvider` and `IncludeContents`, plus the re-exported `Mode`.

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, Agent, AgentError,
    BeforeAgentCallback, BeforeModelCallback, BeforeToolCallback, OnModelErrorCallback,
    OnToolErrorCallback, ReadonlyContext, Tool, Toolset,
};
use gadk_genai::{GenerateContentConfig, Schema};
use gadk_model::Llm;

pub use gadk_agent::Mode;

/// Builds an instruction dynamically from the agent context.
pub type InstructionProvider =
    Arc<dyn Fn(ReadonlyContext) -> BoxFuture<'static, Result<String, AgentError>> + Send + Sync>;

/// What parts of prior conversation history the agent receives (`""` / `"default"` / `"none"`
/// as strings; see [`as_str`](Self::as_str)).
///
/// `Unset` acts as `Default` except under a single_turn placement, where history is hidden
/// (the contents processor hides history whenever the value is not `Default`). An explicit
/// `Default` keeps history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IncludeContents {
    /// Not set (the empty string).
    #[default]
    Unset,
    /// The agent receives the relevant conversation history, even when placed as a single_turn
    /// workflow node.
    Default,
    /// The agent operates solely on its current turn: latest user input plus any following agent
    /// events.
    None,
}

impl IncludeContents {
    /// The string form: `""`, `"default"` or `"none"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unset => "",
            Self::Default => "default",
            Self::None => "none",
        }
    }
}

/// The configuration of an LLM agent.
#[derive(Clone, Default)]
pub struct LlmAgentConfig {
    /// Name must be a non-empty string, unique within the agent tree; it cannot be "user", since
    /// that is reserved for end-user's input.
    pub name: String,
    /// Description of the agent's capability; the LLM uses it to determine whether to delegate
    /// control to the agent.
    pub description: String,
    /// The child agents that this agent can delegate tasks to.
    pub sub_agents: Vec<Arc<dyn Agent>>,
    /// Called sequentially before the agent starts its run.
    pub before_agent_callbacks: Vec<BeforeAgentCallback>,
    /// Called sequentially after the agent completes its run.
    pub after_agent_callbacks: Vec<AfterAgentCallback>,
    /// Additional content generation configuration (tools must be configured via `tools`).
    pub generate_content_config: Option<GenerateContentConfig>,
    /// Called in order until one returns a response or error, which then replaces the actual LLM
    /// call.
    pub before_model_callbacks: Vec<BeforeModelCallback>,
    /// Model that is used by the agent.
    pub model: Option<Arc<dyn Llm>>,
    /// Called in order until one returns a response or error, which then replaces the actual LLM
    /// response.
    pub after_model_callbacks: Vec<AfterModelCallback>,
    /// Called when the model returns an error.
    pub on_model_error_callbacks: Vec<OnModelErrorCallback>,
    /// Instruction for the LLM model guiding the agent's behavior; a template with `{key_name}`
    /// placeholders resolved from session state and artifacts.
    pub instruction: String,
    /// Creates instructions dynamically based on the agent context; takes over `instruction` if both
    /// are set.
    pub instruction_provider: Option<InstructionProvider>,
    /// Instruction for all agents in the entire agent tree; only the root agent's takes effect.
    pub global_instruction: String,
    /// Creates global instructions dynamically based on the agent context; takes over
    /// `global_instruction` if both are set.
    pub global_instruction_provider: Option<InstructionProvider>,
    /// Prevents transferring to the parent agent if the LLM decides to.
    pub disallow_transfer_to_parent: bool,
    /// Prevents transferring to peer agents.
    pub disallow_transfer_to_peers: bool,
    /// Whether to include contents (conversation history) in the model request.
    pub include_contents: IncludeContents,
    /// The input schema when the agent is used as a tool.
    pub input_schema: Option<Schema>,
    /// The output schema when the agent replies.
    pub output_schema: Option<Schema>,
    /// Executed in order; if one returns a result or error, the remaining callbacks and the actual
    /// tool call are skipped.
    pub before_tool_callbacks: Vec<BeforeToolCallback>,
    /// Tools available to the agent.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Executed in order; if one returns a result or error, the list stops and that is returned
    /// instead.
    pub after_tool_callbacks: Vec<AfterToolCallback>,
    /// Toolsets the agent extracts tools from to pass to the underlying LLM.
    pub toolsets: Vec<Arc<dyn Toolset>>,
    /// Called when a tool returns an error.
    pub on_tool_error_callbacks: Vec<OnToolErrorCallback>,
    /// Optional key in session state for the agent output.
    pub output_key: String,
    /// The delegation mode for this agent; defaults to chat as a sub-agent and single_turn as a
    /// workflow node.
    pub mode: Mode,
}
