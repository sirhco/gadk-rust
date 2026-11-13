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

//! Go `internal/configurable/configurable.go` (`to*Config`, `resolveSubAgents`,
//! `resolveTools`, `resolveCallbacks`) and the agent factories of `configurable_utils.go`
//! (`newLLMAgent`, `newLoopAgent`, `newParallelAgent`, `newSequentialAgent`).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_agent::{Agent, Tool, Toolset};
use gadk_engine::agents::loop_agent::LoopAgentConfig;
use gadk_engine::agents::parallel_agent::ParallelAgentConfig;
use gadk_engine::agents::sequential_agent::SequentialAgentConfig;
use gadk_engine::llm::LlmAgentConfig;

use crate::config::{
    AgentRefConfig, CodeConfig, LlmAgentYamlConfig, LoopAgentYamlConfig, ParallelAgentYamlConfig,
    SequentialAgentYamlConfig, ToolConfig,
};
use crate::{CallbackType, ConfigError, LoadCtx, Registry};

/// Go `(*llmAgentYAMLConfig).toLLMAgentConfig`: `model %s is not supported` for a non-Gemini
/// model, `gemini.NewModel` with `GOOGLE_API_KEY` (`failed to create model: %w`), then
/// sub-agents, tools/toolsets and callbacks.
pub(crate) async fn to_llm_agent_config(
    c: &LlmAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<LlmAgentConfig, ConfigError> {
    // Go: configurable.go toLLMAgentConfig
    todo!("Task 3")
}

/// Go `(*loopAgentYAMLConfig).toLoopAgentConfig`.
pub(crate) async fn to_loop_agent_config(
    c: &LoopAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<LoopAgentConfig, ConfigError> {
    // Go: configurable.go toLoopAgentConfig
    todo!("Task 3")
}

/// Go `(*parallelAgentYAMLConfig).toParallelAgentConfig`.
pub(crate) async fn to_parallel_agent_config(
    c: &ParallelAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<ParallelAgentConfig, ConfigError> {
    // Go: configurable.go toParallelAgentConfig
    todo!("Task 3")
}

/// Go `(*sequentialAgentYAMLConfig).toSequentialAgentConfig`.
pub(crate) async fn to_sequential_agent_config(
    c: &SequentialAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<SequentialAgentConfig, ConfigError> {
    // Go: configurable.go toSequentialAgentConfig
    todo!("Task 3")
}

/// Go `resolveSubAgents`: `failed to resolve agent reference %s: %w`; an inline `code`
/// reference fails with `inline code agent references are not yet supported for %s`.
pub(crate) async fn resolve_sub_agents(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    refs: &[AgentRefConfig],
) -> Result<Vec<Arc<dyn Agent>>, ConfigError> {
    // Go: configurable.go resolveSubAgents
    todo!("Task 3")
}

/// Go `resolveTools`: sets `parentPath` in the context ([`LoadCtx::with_parent_path`]) for each
/// named tool, `failed to resolve tool reference %s: %w`.
pub(crate) async fn resolve_tools(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    tool_configs: &[ToolConfig],
) -> Result<(Vec<Arc<dyn Tool>>, Vec<Arc<dyn Toolset>>), ConfigError> {
    // Go: configurable.go resolveTools
    todo!("Task 3")
}

/// Go `resolveCallbacks[T]`: `failed to resolve callback reference %s: %w`, and
/// `callback %s is of type %T and not %T` when the registered callback is not a `T`.
pub(crate) fn resolve_callbacks<T: CallbackType>(
    registry: &Registry,
    callbacks: &[CodeConfig],
) -> Result<Vec<T::Fn>, ConfigError> {
    // Go: configurable.go resolveCallbacks
    todo!("Task 3")
}

/// Go `newLLMAgent`: `failed to parse LLM agent config: %w`, `'name' is required`,
/// `'model' is required for LlmAgent`, `failed to create LLM agent config: %w`, then
/// `llmagent.New`.
pub(crate) async fn new_llm_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_path: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    // Go: configurable_utils.go newLLMAgent
    todo!("Task 3")
}

/// Go `newLoopAgent`: `failed to parse Loop agent config: %w`, `'name' is required`,
/// `failed to create Loop agent config: %w`, then `loopagent.New`.
pub(crate) async fn new_loop_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_path: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    // Go: configurable_utils.go newLoopAgent
    todo!("Task 3")
}

/// Go `newParallelAgent`: `failed to parse Parallel agent config: %w`, `'name' is required`,
/// `failed to create Parallel agent config: %w`, then `parallelagent.New`.
pub(crate) async fn new_parallel_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_path: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    // Go: configurable_utils.go newParallelAgent
    todo!("Task 3")
}

/// Go `newSequentialAgent`: `failed to parse Sequential agent config: %w`,
/// `'name' is required`, `failed to create Sequential agent config: %w`, then
/// `sequentialagent.New`.
pub(crate) async fn new_sequential_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_path: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    // Go: configurable_utils.go newSequentialAgent
    todo!("Task 3")
}
