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

//! Go `internal/configurable/configurable_workflow.go`: node-function registration, the
//! `Workflow` agent factory, edge parsing and node resolution. The config structs live in
//! [`crate::config`]; the node-function and node registries are fields of [`Registry`].

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_agent::Agent;
use gadk_engine::workflow::{Edge, Node, NodeConfig};
use gadk_goyaml::Document;

use crate::{ConfigError, LoadCtx, NodeFn, Registry, StrNodeFn};

impl Registry {
    /// Go `RegisterNodeFunction` with a `func(agent.Context, any) (any, error)`: overwrites an
    /// existing registration silently, as Go does. (Go's bad-signature panic cannot happen with
    /// the typed API.)
    pub fn register_node_function(&self, name: &str, f: NodeFn) {
        // Go: configurable_workflow.go RegisterNodeFunction
        todo!("Task 8")
    }

    /// Go `RegisterNodeFunction` with a `func(agent.Context, string) (string, error)`:
    /// registered through [`cast_node_function`]'s `fmt.Sprint` coercion; overwrites.
    pub fn register_string_node_function(&self, name: &str, f: StrNodeFn) {
        // Go: configurable_workflow.go RegisterNodeFunction
        todo!("Task 8")
    }
}

/// Go `newWorkflowAgent`: `failed to unmarshal Workflow config: %w`,
/// `workflow 'name' is required`, `failed to parse workflow edges: %w`,
/// `failed to build workflow: %w`; the agent is `agent.New` with the collected sub-agents and
/// the workflow's `Run`.
pub(crate) async fn new_workflow_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_path: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    // Go: configurable_workflow.go newWorkflowAgent
    todo!("Task 8")
}

/// Go `parseEdges`: each edge node must be a sequence of scalars (nodes) and mappings (route
/// maps, `invalid route map format: %w`); chains without routes need two nodes, routed chains
/// a source; route keys sorted, `default` (case-insensitive) is the default route.
pub(crate) async fn parse_edges(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    nodes: &[Document],
) -> Result<Vec<Edge>, ConfigError> {
    // Go: configurable_workflow.go parseEdges
    todo!("Task 8")
}

/// Go `resolveNodeLike`: `START`; a `.yaml`/`.yml` reference (absolute, or relative to the
/// parent's directory) through the node cache and [`resolve_node_from_yaml`]; otherwise a
/// registered node function (never cached: a new function node per use), else
/// `node or function reference %q not found in registries`.
pub(crate) async fn resolve_node_like(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    node_ref: &str,
) -> Result<Arc<dyn Node>, ConfigError> {
    // Go: configurable_workflow.go resolveNodeLike
    todo!("Task 8")
}

/// Go `resolveNodeFromYAML`: `FunctionNode`, `JoinNode`, `ToolNode`, else an agent reference
/// wrapped in an agent node and appended to the context's sub-agent collector.
pub(crate) async fn resolve_node_from_yaml(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    node_ref: &str,
    abs_path: &Path,
) -> Result<Arc<dyn Node>, ConfigError> {
    // Go: configurable_workflow.go resolveNodeFromYAML
    todo!("Task 8")
}

/// Go `castNodeFunction`'s string branch: a nil input is `""`, a string input itself, any other
/// input `fmt.Sprint(input)`; the string result is the node's output.
pub(crate) fn cast_node_function(f: StrNodeFn) -> NodeFn {
    // Go: configurable_workflow.go castNodeFunction
    todo!("Task 8")
}

/// Go `workflow.NewFunctionNode(name, fn, cfg)` for a registered [`NodeFn`] (input `any`), with
/// Go's output type switch on the returned [`crate::NodeOutput`].
pub(crate) fn new_function_node(name: &str, f: NodeFn, cfg: NodeConfig) -> Arc<dyn Node> {
    // Go: workflow NewFunctionNode (as configurable_workflow.go calls it)
    todo!("Task 8")
}
