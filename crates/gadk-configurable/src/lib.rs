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

//! gadk-configurable (sub-project 6c-2): a port of ADK-Go's `internal/configurable` (YAML agent
//! configs), its `conformance` package (test callbacks, functions, node functions, the agent
//! loader and the record and replay plugins) and `cmd/internal/adkcli` (the `adkcli` binary).
//!
//! This file ports `configurable_utils.go`'s registry: Go's package-level registries
//! (`registry`, `agentRegistry`, `toolRegistry`, `callbackRegistry` and, from
//! `configurable_workflow.go`, `nodeFunctionRegistry` / `nodeRegistry`, all under one
//! `registryMu`) are the fields of one [`Registry`] value; [`Registry::new`] is Go's `init()`.
//! Go's context values (`parentPathKey`, `subAgentsCollectorKey`) are carried explicitly by
//! [`LoadCtx`].
//!
//! Internal crate (`publish = false`); never a dependency of the `gadk` façade.

#![forbid(unsafe_code)]
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub mod config;
pub mod conformance;
pub mod error;
pub mod govalue;

pub(crate) mod agents;
pub(crate) mod builtin_tools;
pub(crate) mod workflow;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{
    AfterAgentCallback, Agent, AgentError, BeforeAgentCallback, Context, Tool, Toolset,
};
use gadk_engine::workflow::Node;
use gadk_genai::Content;
use gadk_session::Event;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

pub use error::ConfigError;
pub use gadk_goyaml::GoValue;
pub use govalue::GoMap;

/// Go `AgentFactory func(ctx, configBytes []byte, configPath string) (agent.Agent, error)`:
/// builds an agent from a config file's bytes and absolute path. The [`Registry`] (a cheap
/// handle) and the [`LoadCtx`] stand for Go's package globals and `ctx`.
pub type AgentFactory = Arc<
    dyn Fn(
            Registry,
            LoadCtx,
            Vec<u8>,
            PathBuf,
        ) -> BoxFuture<'static, Result<Arc<dyn Agent>, ConfigError>>
        + Send
        + Sync,
>;

/// Go `ToolFactory func(ctx, args map[string]any) (tool.Tool, error)`; `None` args is Go's nil
/// map.
pub type ToolFactory = Arc<
    dyn Fn(
            Registry,
            ToolCtx,
            Option<GoMap>,
        ) -> BoxFuture<'static, Result<Arc<dyn Tool>, ConfigError>>
        + Send
        + Sync,
>;

/// Go `ToolsetFactory func(ctx, args map[string]any) (tool.Toolset, error)`.
pub type ToolsetFactory = Arc<
    dyn Fn(
            Registry,
            ToolCtx,
            Option<GoMap>,
        ) -> BoxFuture<'static, Result<Arc<dyn Toolset>, ConfigError>>
        + Send
        + Sync,
>;

/// Go's node function `func(agent.Context, any) (any, error)` (Go's `any` output type-switched
/// by the workflow into an event, a content or a plain output: [`NodeOutput`]).
pub type NodeFn =
    Arc<dyn Fn(Context, Value) -> BoxFuture<'static, Result<NodeOutput, AgentError>> + Send + Sync>;

/// Go's node function `func(agent.Context, string) (string, error)`, registered through
/// [`Registry::register_string_node_function`] with Go's `fmt.Sprint` input coercion.
pub type StrNodeFn =
    Arc<dyn Fn(Context, String) -> BoxFuture<'static, Result<String, AgentError>> + Send + Sync>;

/// What a [`NodeFn`] returns: Go's `any` result, which the workflow's function node
/// type-switches (`*session.Event` is forwarded, `*genai.Content` becomes the event content,
/// anything else is the event output; Go nil = `Value(Value::Null)`, no output).
#[derive(Debug, Clone)]
pub enum NodeOutput {
    /// A plain output value.
    Value(Value),
    /// A `*session.Event` (e.g. one carrying `Routes`).
    Event(Box<Event>),
    /// A `*genai.Content`.
    Content(Content),
}

/// Go's `callbackRegistry` values: the `any` Go stores, restricted to the two callback types
/// Go's resolvers assert.
#[derive(Clone)]
pub enum Callback {
    /// Go `agent.BeforeAgentCallback`.
    BeforeAgent(BeforeAgentCallback),
    /// Go `agent.AfterAgentCallback`.
    AfterAgent(AfterAgentCallback),
}

impl Callback {
    /// Go `%T` of the stored value.
    pub fn go_type_name(&self) -> &'static str {
        match self {
            Callback::BeforeAgent(_) => BeforeAgent::GO_TYPE_NAME,
            Callback::AfterAgent(_) => AfterAgent::GO_TYPE_NAME,
        }
    }
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.go_type_name())
    }
}

/// Go's type parameter `T` of `resolveCallbacks[T]`: the callback type a reference must hold.
pub(crate) trait CallbackType {
    /// The Rust callback type.
    type Fn;
    /// Go `%T` of the zero `T` (`agent.BeforeAgentCallback`).
    const GO_TYPE_NAME: &'static str;
    /// Go `c.(T)`: the callback when it has this type.
    fn from_callback(c: &Callback) -> Option<Self::Fn>;
}

/// `T = agent.BeforeAgentCallback`.
pub(crate) struct BeforeAgent;

/// `T = agent.AfterAgentCallback`.
pub(crate) struct AfterAgent;

impl CallbackType for BeforeAgent {
    type Fn = BeforeAgentCallback;
    const GO_TYPE_NAME: &'static str = "agent.BeforeAgentCallback";
    fn from_callback(c: &Callback) -> Option<BeforeAgentCallback> {
        match c {
            Callback::BeforeAgent(f) => Some(f.clone()),
            Callback::AfterAgent(_) => None,
        }
    }
}

impl CallbackType for AfterAgent {
    type Fn = AfterAgentCallback;
    const GO_TYPE_NAME: &'static str = "agent.AfterAgentCallback";
    fn from_callback(c: &Callback) -> Option<AfterAgentCallback> {
        match c {
            Callback::AfterAgent(f) => Some(f.clone()),
            Callback::BeforeAgent(_) => None,
        }
    }
}

/// Go's `subAgentsCollectorKey` value (`*[]agent.Agent`): the workflow's sub-agent list that
/// YAML agent nodes are appended to.
pub type SubAgentsCollector = Arc<Mutex<Vec<Arc<dyn Agent>>>>;

/// Go's `ctx` values while loading configs. Every nested `from_config` / factory call inherits
/// the caller's `LoadCtx`, as Go's `ctx` is inherited.
#[derive(Clone, Default)]
pub struct LoadCtx {
    /// Go `ctx.Value(parentPathKey)`: set by `resolveTools` to the config path of the agent
    /// whose tools are resolved; `None` = not set (`parentPath not found in context`).
    pub parent_path: Option<PathBuf>,
    /// Go `ctx.Value(subAgentsCollectorKey)`: set by the workflow factory around `parseEdges`.
    pub sub_agents: Option<SubAgentsCollector>,
}

impl LoadCtx {
    /// Go `context.WithValue(ctx, parentPathKey, parentPath)`.
    pub fn with_parent_path(&self, parent_path: &Path) -> LoadCtx {
        LoadCtx {
            parent_path: Some(parent_path.to_path_buf()),
            sub_agents: self.sub_agents.clone(),
        }
    }

    /// Go `context.WithValue(ctx, subAgentsCollectorKey, &subAgents)`.
    pub fn with_sub_agents(&self, collector: SubAgentsCollector) -> LoadCtx {
        LoadCtx {
            parent_path: self.parent_path.clone(),
            sub_agents: Some(collector),
        }
    }
}

/// The context a tool or toolset factory receives (Go passes the same `ctx`).
pub type ToolCtx = LoadCtx;

/// A `toolRegistry` value: Go stores either factory type as `any` and type-switches on it.
#[derive(Clone)]
pub(crate) enum ToolEntry {
    /// A [`ToolFactory`].
    Tool(ToolFactory),
    /// A [`ToolsetFactory`].
    Toolset(ToolsetFactory),
}

/// Go's package-level registries, guarded together like Go's single `registryMu`.
#[derive(Default)]
pub(crate) struct Registries {
    /// Go `registry`: agent class → factory.
    pub(crate) agent_classes: HashMap<String, AgentFactory>,
    /// Go `agentRegistry`: absolute config path → loaded agent.
    pub(crate) agents: HashMap<PathBuf, Arc<dyn Agent>>,
    /// Go `toolRegistry`: tool name → tool or toolset factory.
    pub(crate) tools: HashMap<String, ToolEntry>,
    /// Go `callbackRegistry`.
    pub(crate) callbacks: HashMap<String, Callback>,
    /// Go `nodeFunctionRegistry`.
    pub(crate) node_functions: HashMap<String, NodeFn>,
    /// Go `nodeRegistry`: an absolute YAML path (lossy UTF-8) or a function reference → node.
    pub(crate) nodes: HashMap<String, Arc<dyn Node>>,
}

/// Go `internal/configurable`'s global state as a value: a cheap, clonable handle (clones share
/// the registries, as Go's globals are shared).
#[derive(Clone)]
pub struct Registry {
    pub(crate) inner: Arc<RwLock<Registries>>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new()
    }
}

impl Registry {
    /// A registry with no registrations (not Go's `init()` set): for tests.
    pub(crate) fn empty() -> Registry {
        Registry {
            inner: Arc::new(RwLock::new(Registries::default())),
        }
    }

    /// Go `init()`: agent classes `LlmAgent`, `LoopAgent`, `ParallelAgent`,
    /// `SequentialAgent`, `Workflow`; tools `exit_loop`, `google_search`, `url_context`,
    /// `google_maps_grounding`, `AgentTool`, `LongRunningFunctionTool`, `ExampleTool`;
    /// toolset `McpToolset`. Go's `init` panics are unreachable (fresh maps).
    pub fn new() -> Registry {
        // Go: configurable_utils.go init
        todo!("Task 3")
    }

    /// Go `Register`: `Register called twice for agent %s` on a duplicate.
    pub fn register(&self, name: &str, factory: AgentFactory) -> Result<(), ConfigError> {
        // Go: configurable_utils.go Register
        todo!("Task 3")
    }

    /// Go `RegisterToolFactory`: `RegisterToolFactory called twice for tool %s` on a duplicate
    /// (tools and toolsets share one namespace).
    pub fn register_tool_factory(
        &self,
        name: &str,
        factory: ToolFactory,
    ) -> Result<(), ConfigError> {
        // Go: configurable_utils.go RegisterToolFactory
        todo!("Task 3")
    }

    /// Go `RegisterToolsetFactory`: `RegisterToolsetFactory called twice for toolset %s`.
    pub fn register_toolset_factory(
        &self,
        name: &str,
        factory: ToolsetFactory,
    ) -> Result<(), ConfigError> {
        // Go: configurable_utils.go RegisterToolsetFactory
        todo!("Task 3")
    }

    /// Go `RegisterCallback`: `RegisterCallback called twice for callback %s`.
    pub fn register_callback(&self, name: &str, callback: Callback) -> Result<(), ConfigError> {
        // Go: configurable_utils.go RegisterCallback
        todo!("Task 3")
    }

    /// Go `FromConfig(context.Background(), configPath)`: the top-level entry point (an empty
    /// [`LoadCtx`]).
    pub async fn from_config(&self, config_path: &Path) -> Result<Arc<dyn Agent>, ConfigError> {
        self.from_config_with_ctx(&LoadCtx::default(), config_path)
            .await
    }

    /// Go `FromConfig(ctx, configPath)`: `filepath.Abs`, read the file, peek `agent_class`
    /// (default `LlmAgent`), call the factory with the file's bytes and absolute path. Boxed:
    /// the factories recurse into it.
    #[allow(clippy::wrong_self_convention)] // Go's `FromConfig` name; `self` is the registry.
    pub(crate) fn from_config_with_ctx<'a>(
        &'a self,
        ctx: &'a LoadCtx,
        config_path: &'a Path,
    ) -> BoxFuture<'a, Result<Arc<dyn Agent>, ConfigError>> {
        // Go: configurable_utils.go FromConfig
        todo!("Task 3")
    }

    /// Go `ResolveToolReference`: `(tool, None)` from a tool factory, `(None, toolset)` from a
    /// toolset factory; `tool name cannot be empty`, `tool '%s' not found`.
    pub async fn resolve_tool_reference(
        &self,
        ctx: &ToolCtx,
        tool_name: &str,
        args: Option<&GoMap>,
    ) -> Result<(Option<Arc<dyn Tool>>, Option<Arc<dyn Toolset>>), ConfigError> {
        // Go: configurable_utils.go ResolveToolReference
        todo!("Task 3")
    }

    /// Go `ResolveCallbackReference`: `callback name cannot be empty`,
    /// `callback '%s' not found`.
    pub fn resolve_callback_reference(&self, callback_name: &str) -> Result<Callback, ConfigError> {
        // Go: configurable_utils.go ResolveCallbackReference
        todo!("Task 3")
    }

    /// Go `ResolveAgentReference(context.Background(), parentPath, refPath)`: the top-level
    /// entry point (an empty [`LoadCtx`]).
    pub async fn resolve_agent_reference(
        &self,
        parent_path: &Path,
        ref_path: &str,
    ) -> Result<Arc<dyn Agent>, ConfigError> {
        self.resolve_agent_reference_with_ctx(&LoadCtx::default(), parent_path, ref_path)
            .await
    }

    /// Go `ResolveAgentReference(ctx, parentPath, refPath)`: rejects empty and absolute
    /// references and paths escaping the parent's directory (after `EvalSymlinks`), returns the
    /// cached agent for the absolute path, else loads it with [`Registry::from_config_with_ctx`]
    /// and caches it (double-checked insert: the first cached agent wins). Boxed: recursive.
    pub(crate) fn resolve_agent_reference_with_ctx<'a>(
        &'a self,
        ctx: &'a LoadCtx,
        parent_path: &'a Path,
        ref_path: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn Agent>, ConfigError>> {
        // Go: configurable_utils.go ResolveAgentReference
        todo!("Task 3")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use futures::FutureExt;

    use super::*;

    fn noop_before() -> BeforeAgentCallback {
        Arc::new(|_ctx| async { Ok(None) }.boxed())
    }

    #[test]
    fn callback_go_type_names() {
        let before = Callback::BeforeAgent(noop_before());
        let after = Callback::AfterAgent(noop_before());
        assert_eq!(before.go_type_name(), "agent.BeforeAgentCallback");
        assert_eq!(after.go_type_name(), "agent.AfterAgentCallback");
        assert_eq!(format!("{before:?}"), "agent.BeforeAgentCallback");
    }

    #[test]
    fn callback_type_asserts_like_go() {
        let before = Callback::BeforeAgent(noop_before());
        assert!(BeforeAgent::from_callback(&before).is_some());
        assert!(AfterAgent::from_callback(&before).is_none());
        let after = Callback::AfterAgent(noop_before());
        assert!(AfterAgent::from_callback(&after).is_some());
        assert!(BeforeAgent::from_callback(&after).is_none());
        assert_eq!(BeforeAgent::GO_TYPE_NAME, "agent.BeforeAgentCallback");
        assert_eq!(AfterAgent::GO_TYPE_NAME, "agent.AfterAgentCallback");
    }

    #[test]
    fn load_ctx_values_are_inherited() {
        let collector: SubAgentsCollector = Arc::default();
        let base = LoadCtx::default();
        assert!(base.parent_path.is_none() && base.sub_agents.is_none());
        let with_c = base.with_sub_agents(collector.clone());
        let with_both = with_c.with_parent_path(Path::new("/a/root_agent.yaml"));
        assert_eq!(
            with_both.parent_path.as_deref(),
            Some(Path::new("/a/root_agent.yaml"))
        );
        assert!(Arc::ptr_eq(
            with_both.sub_agents.as_ref().unwrap(),
            &collector
        ));
        let replaced = with_both.with_parent_path(Path::new("/b/x.yaml"));
        assert_eq!(
            replaced.parent_path.as_deref(),
            Some(Path::new("/b/x.yaml"))
        );
        assert!(Arc::ptr_eq(
            replaced.sub_agents.as_ref().unwrap(),
            &collector
        ));
    }

    #[test]
    fn empty_registry_has_no_registrations_and_clones_share_state() {
        let r = Registry::empty();
        let clone = r.clone();
        clone
            .inner
            .write()
            .callbacks
            .insert("x".into(), Callback::BeforeAgent(noop_before()));
        assert!(r.inner.read().callbacks.contains_key("x"));
        let g = r.inner.read();
        assert!(g.agent_classes.is_empty() && g.tools.is_empty() && g.agents.is_empty());
        assert!(g.node_functions.is_empty() && g.nodes.is_empty());
    }
}
