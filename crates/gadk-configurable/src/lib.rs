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
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use gadk_agent::{
    AfterAgentCallback, Agent, AgentError, BeforeAgentCallback, Context, Tool, Toolset,
};
use gadk_engine::workflow::Node;
use gadk_genai::Content;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::{go_path_clean, go_path_join, go_quote};
use gadk_session::Event;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

use crate::config::BaseAgentConfig;

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
    /// Go's `os.Getenv` source for the `LlmAgent` factory (`GOOGLE_API_KEY`) and the Gemini
    /// client it builds: the process environment ([`ProcessEnv`]) unless a test swaps it.
    pub(crate) env: Arc<dyn EnvProvider>,
}

impl Default for Registries {
    fn default() -> Self {
        Registries {
            agent_classes: HashMap::new(),
            agents: HashMap::new(),
            tools: HashMap::new(),
            callbacks: HashMap::new(),
            node_functions: HashMap::new(),
            nodes: HashMap::new(),
            env: Arc::new(ProcessEnv),
        }
    }
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
        let registry = Registry::with_agent_classes();
        {
            let mut g = registry.inner.write();
            for (name, entry) in builtin_tools::builtin_tool_factories() {
                g.tools.insert(name.to_owned(), entry);
            }
        }
        registry
    }

    /// The agent-class half of Go's `init()` (`Register("LlmAgent", newLLMAgent)` … in Go's
    /// order); [`Registry::new`] adds the built-in tool and toolset factories.
    pub(crate) fn with_agent_classes() -> Registry {
        let llm: AgentFactory = Arc::new(|r, c, d, p| agents::new_llm_agent(r, c, d, p).boxed());
        let looping: AgentFactory =
            Arc::new(|r, c, d, p| agents::new_loop_agent(r, c, d, p).boxed());
        let parallel: AgentFactory =
            Arc::new(|r, c, d, p| agents::new_parallel_agent(r, c, d, p).boxed());
        let sequential: AgentFactory =
            Arc::new(|r, c, d, p| agents::new_sequential_agent(r, c, d, p).boxed());
        let workflow: AgentFactory =
            Arc::new(|r, c, d, p| workflow::new_workflow_agent(r, c, d, p).boxed());
        let registry = Registry::empty();
        {
            let mut g = registry.inner.write();
            for (name, factory) in [
                ("LlmAgent", llm),
                ("LoopAgent", looping),
                ("ParallelAgent", parallel),
                ("SequentialAgent", sequential),
                ("Workflow", workflow),
            ] {
                g.agent_classes.insert(name.to_owned(), factory);
            }
        }
        registry
    }

    /// Replaces the environment the `LlmAgent` factory reads (`GOOGLE_API_KEY` and the Gemini
    /// client's variables): tests use a [`gadk_platform::env::MapEnv`] so model construction is
    /// hermetic and offline.
    #[cfg(test)]
    pub(crate) fn set_env(&self, env: Arc<dyn EnvProvider>) {
        self.inner.write().env = env;
    }

    /// Go `Register`: `Register called twice for agent %s` on a duplicate.
    pub fn register(&self, name: &str, factory: AgentFactory) -> Result<(), ConfigError> {
        // Go: configurable_utils.go Register
        let mut g = self.inner.write();
        if g.agent_classes.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "Register called twice for agent {name}"
            )));
        }
        g.agent_classes.insert(name.to_owned(), factory);
        Ok(())
    }

    /// Go `RegisterToolFactory`: `RegisterToolFactory called twice for tool %s` on a duplicate
    /// (tools and toolsets share one namespace).
    pub fn register_tool_factory(
        &self,
        name: &str,
        factory: ToolFactory,
    ) -> Result<(), ConfigError> {
        // Go: configurable_utils.go RegisterToolFactory
        let mut g = self.inner.write();
        if g.tools.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "RegisterToolFactory called twice for tool {name}"
            )));
        }
        g.tools.insert(name.to_owned(), ToolEntry::Tool(factory));
        Ok(())
    }

    /// Go `RegisterToolsetFactory`: `RegisterToolsetFactory called twice for toolset %s`.
    pub fn register_toolset_factory(
        &self,
        name: &str,
        factory: ToolsetFactory,
    ) -> Result<(), ConfigError> {
        // Go: configurable_utils.go RegisterToolsetFactory
        let mut g = self.inner.write();
        if g.tools.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "RegisterToolsetFactory called twice for toolset {name}"
            )));
        }
        g.tools.insert(name.to_owned(), ToolEntry::Toolset(factory));
        Ok(())
    }

    /// Go `RegisterCallback`: `RegisterCallback called twice for callback %s`.
    pub fn register_callback(&self, name: &str, callback: Callback) -> Result<(), ConfigError> {
        // Go: configurable_utils.go RegisterCallback
        let mut g = self.inner.write();
        if g.callbacks.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "RegisterCallback called twice for callback {name}"
            )));
        }
        g.callbacks.insert(name.to_owned(), callback);
        Ok(())
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
        async move {
            let abs_path = go_abs(config_path).map_err(|e| {
                ConfigError::wrap("failed to resolve absolute path", go_io_error(&e))
            })?;

            // 1. Read the file (Go os.ReadFile: `open` then `read` *PathErrors).
            let data = match read_file(&abs_path) {
                Ok(data) => data,
                Err(e) if e.is_not_exist() => {
                    return Err(ConfigError::msg(format!(
                        "config file not found: {}",
                        abs_path.to_string_lossy()
                    )));
                }
                Err(e) => return Err(e),
            };

            // 2. Peek at the "agent_class" field to know which factory to use.
            let base_config: BaseAgentConfig = config::unmarshal(&data)
                .map_err(|e| ConfigError::wrap("invalid YAML content", e))?;

            // Default fallback similar to Python's handling
            let agent_class = if base_config.agent_class.is_empty() {
                "LlmAgent".to_owned()
            } else {
                base_config.agent_class
            };

            // 3. Resolve the factory (the lock is released before the factory runs).
            let factory = self.inner.read().agent_classes.get(&agent_class).cloned();
            let Some(factory) = factory else {
                return Err(ConfigError::msg(format!(
                    "invalid agent class '{agent_class}': not registered. Ensure the package is imported"
                )));
            };

            // 4. Delegate creation to the specific factory.
            factory(self.clone(), ctx.clone(), data, abs_path).await
        }
        .boxed()
    }

    /// Go `ResolveToolReference`: `(tool, None)` from a tool factory, `(None, toolset)` from a
    /// toolset factory; `tool name cannot be empty`, `tool '%s' not found`.
    // Go's third branch, `tool '%s' is not a tool or toolset factory`, is unreachable: the
    // registry's values are typed (ledger 6c-2 N/A).
    pub async fn resolve_tool_reference(
        &self,
        ctx: &ToolCtx,
        tool_name: &str,
        args: Option<&GoMap>,
    ) -> Result<(Option<Arc<dyn Tool>>, Option<Arc<dyn Toolset>>), ConfigError> {
        // Go: configurable_utils.go ResolveToolReference
        if tool_name.is_empty() {
            return Err(ConfigError::msg("tool name cannot be empty"));
        }
        let entry = self.inner.read().tools.get(tool_name).cloned();
        match entry {
            Some(ToolEntry::Tool(factory)) => {
                let tool = factory(self.clone(), ctx.clone(), args.cloned()).await?;
                Ok((Some(tool), None))
            }
            Some(ToolEntry::Toolset(factory)) => {
                let toolset = factory(self.clone(), ctx.clone(), args.cloned()).await?;
                Ok((None, Some(toolset)))
            }
            None => Err(ConfigError::msg(format!("tool '{tool_name}' not found"))),
        }
    }

    /// Go `ResolveCallbackReference`: `callback name cannot be empty`,
    /// `callback '%s' not found`.
    pub fn resolve_callback_reference(&self, callback_name: &str) -> Result<Callback, ConfigError> {
        // Go: configurable_utils.go ResolveCallbackReference
        if callback_name.is_empty() {
            return Err(ConfigError::msg("callback name cannot be empty"));
        }
        let callback = self.inner.read().callbacks.get(callback_name).cloned();
        callback.ok_or_else(|| ConfigError::msg(format!("callback '{callback_name}' not found")))
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
        async move {
            if ref_path.is_empty() {
                return Err(ConfigError::msg("agent reference path cannot be empty"));
            }

            if go_is_abs(ref_path) {
                return Err(ConfigError::msg(format!(
                    "absolute paths are not allowed in AgentTool config_path: {ref_path}"
                )));
            }

            let parent = parent_path.to_string_lossy();
            let target_path = go_path_join(&[go_dir(&parent).as_str(), ref_path]);

            let abs_path = go_abs(Path::new(&target_path)).map_err(|e| {
                ConfigError::wrap("failed to resolve absolute path", go_io_error(&e))
            })?;

            // Prevent path traversal outside the parent agent's directory. Both sides are
            // made absolute before comparing, and symlinks are resolved where the paths
            // exist, so a symlink inside the agent directory cannot be used to escape it.
            let mut parent_dir = go_abs(Path::new(&go_dir(&parent))).map_err(|e| {
                ConfigError::wrap("failed to resolve agent directory", go_io_error(&e))
            })?;
            if let Some(resolved) = eval_symlinks(&parent_dir) {
                parent_dir = resolved;
            }
            let mut check_path = abs_path.clone();
            if let Some(resolved) = eval_symlinks(&abs_path) {
                check_path = resolved;
            }
            let parent_dir = parent_dir.to_string_lossy();
            let check_path = check_path.to_string_lossy();
            if !check_path.starts_with(&format!("{parent_dir}/")) && check_path != parent_dir {
                return Err(ConfigError::msg(format!(
                    "path traversal detected: config_path {} resolves outside agent directory",
                    go_quote(ref_path)
                )));
            }

            let cached = self.inner.read().agents.get(&abs_path).cloned();
            if let Some(agent) = cached {
                return Ok(agent);
            }

            let agent = self.from_config_with_ctx(ctx, &abs_path).await?;

            let mut g = self.inner.write();
            if let Some(existing) = g.agents.get(&abs_path) {
                return Ok(existing.clone());
            }
            g.agents.insert(abs_path, agent.clone());
            Ok(agent)
        }
        .boxed()
    }
}

/// Go `filepath.IsAbs` (Unix): a path starting with `/`.
pub(crate) fn go_is_abs(path: &str) -> bool {
    path.starts_with('/')
}

/// Go `filepath.Dir` (Unix): everything up to the last `/`, cleaned; `.` without a `/`.
pub(crate) fn go_dir(path: &str) -> String {
    let end = path.rfind('/').map_or(0, |i| i + 1);
    go_path_clean(&path[..end])
}

/// Go `filepath.Abs` (Unix): an absolute path cleaned, a relative one joined to the working
/// directory (`os.Getwd`, its only error) and cleaned. Lexical: `..` is resolved without
/// following symlinks, unlike `std::fs::canonicalize`.
pub(crate) fn go_abs(path: &Path) -> io::Result<PathBuf> {
    let p = path.to_string_lossy();
    if go_is_abs(&p) {
        return Ok(PathBuf::from(go_path_clean(&p)));
    }
    let cwd = std::env::current_dir()?;
    Ok(PathBuf::from(go_path_join(&[
        cwd.to_string_lossy().as_ref(),
        p.as_ref(),
    ])))
}

/// Go `filepath.EvalSymlinks` for an absolute path: the path with every symlink resolved, or
/// `None` when that fails (Go's callers then keep the unresolved path).
fn eval_symlinks(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// Go `os.ReadFile`: an `open` *PathError when the file cannot be opened, a `read` one when it
/// cannot be read (a directory opens but fails with `is a directory`).
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>, ConfigError> {
    let mut file = std::fs::File::open(path).map_err(|e| ConfigError::path("open", path, &e))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| ConfigError::path("read", path, &e))?;
    Ok(data)
}

/// A bare OS error as Go prints a `syscall.Errno` (`os.Getwd`'s error).
pub(crate) fn go_io_error(err: &io::Error) -> ConfigError {
    ConfigError::msg(error::go_errno_text(err))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::FutureExt;
    use gadk_agent::{AgentConfig, ReadonlyContext, ToolError};
    use gadk_platform::env::MapEnv;

    use super::*;

    /// `unwrap_err` for results whose `Ok` type (`Arc<dyn Agent>`, tools) has no `Debug`.
    pub(crate) trait CfgErr {
        fn cfg_err(self) -> ConfigError;
    }

    impl<T> CfgErr for Result<T, ConfigError> {
        fn cfg_err(self) -> ConfigError {
            match self {
                Ok(_) => panic!("expected an error"),
                Err(e) => e,
            }
        }
    }

    pub(crate) fn noop_before() -> BeforeAgentCallback {
        Arc::new(|_ctx| async { Ok(None) }.boxed())
    }

    /// Writes `content` to `dir/name` (creating parent directories) and returns the path.
    pub(crate) fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    /// A temporary directory with symlinks resolved (macOS reaches `/var` through
    /// `/private/var`), as Go's tests resolve `t.TempDir()`.
    pub(crate) fn temp_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        (dir, base)
    }

    /// `path` spelled relative to the current working directory (`../../…/path`), so tests
    /// exercise Go's `filepath.Abs` cwd join without changing the process-wide cwd.
    pub(crate) fn relative_to_cwd(path: &Path) -> String {
        let cwd = std::env::current_dir().unwrap();
        let ups = cwd.components().count() - 1;
        let rest = path.to_string_lossy();
        format!("{}{}", "../".repeat(ups), rest.trim_start_matches('/'))
    }

    /// An environment with a fake Gemini API key: `GeminiModel::new` resolves it without any
    /// network access.
    pub(crate) fn fake_key_env() -> Arc<dyn EnvProvider> {
        Arc::new(MapEnv(
            [("GOOGLE_API_KEY".to_owned(), "fake-key".to_owned())]
                .into_iter()
                .collect(),
        ))
    }

    /// Go `init()`'s agent classes with a hermetic environment (no built-in tools: those are
    /// Task 4's).
    pub(crate) fn test_registry() -> Registry {
        let r = Registry::with_agent_classes();
        r.set_env(fake_key_env());
        r
    }

    /// What a [`recording_factory`] saw: the context's parent path, the bytes and the path.
    pub(crate) type Seen = Arc<Mutex<Vec<(Option<PathBuf>, Vec<u8>, PathBuf)>>>;

    /// An agent factory that records its inputs and builds a plain agent named after the
    /// file stem.
    pub(crate) fn recording_factory(seen: Seen) -> AgentFactory {
        Arc::new(move |_r, ctx, data, path| {
            let seen = seen.clone();
            async move {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                seen.lock().push((ctx.parent_path.clone(), data, path));
                gadk_agent::new_agent(AgentConfig {
                    name,
                    ..Default::default()
                })
                .map_err(ConfigError::other)
            }
            .boxed()
        })
    }

    /// A toolset with no tools.
    pub(crate) struct TestToolset(pub(crate) String);

    impl Toolset for TestToolset {
        fn name(&self) -> &str {
            &self.0
        }

        // The `async_trait` expansion of `async fn tools` (gadk-configurable does not depend on
        // `async-trait`).
        fn tools<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _ctx: &'life1 ReadonlyContext,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<Arc<dyn Tool>>, ToolError>> + Send + 'async_trait>,
        >
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// What a test tool or toolset factory saw: the context's parent path and the args.
    pub(crate) type ToolSeen = Arc<Mutex<Vec<(Option<PathBuf>, Option<GoMap>)>>>;

    /// A tool factory returning gadk-engine's `exit_loop` tool and recording its inputs.
    pub(crate) fn test_tool_factory(seen: ToolSeen) -> ToolFactory {
        Arc::new(move |_r, ctx, args| {
            seen.lock().push((ctx.parent_path.clone(), args));
            async { Ok(gadk_engine::tool::exit_loop()) }.boxed()
        })
    }

    /// A toolset factory returning a [`TestToolset`] and recording its inputs.
    pub(crate) fn test_toolset_factory(name: &str, seen: ToolSeen) -> ToolsetFactory {
        let name = name.to_owned();
        Arc::new(move |_r, ctx, args| {
            seen.lock().push((ctx.parent_path.clone(), args));
            let ts: Arc<dyn Toolset> = Arc::new(TestToolset(name.clone()));
            async move { Ok(ts) }.boxed()
        })
    }

    fn failing_tool_factory(text: &'static str) -> ToolFactory {
        Arc::new(move |_r, _c, _a| async move { Err(ConfigError::msg(text)) }.boxed())
    }

    fn sorted_keys<V>(m: &HashMap<String, V>) -> Vec<String> {
        let mut keys: Vec<String> = m.keys().cloned().collect();
        keys.sort();
        keys
    }

    #[test]
    fn with_agent_classes_registers_go_init_agent_classes() {
        let r = Registry::with_agent_classes();
        let g = r.inner.read();
        assert_eq!(
            sorted_keys(&g.agent_classes),
            [
                "LlmAgent",
                "LoopAgent",
                "ParallelAgent",
                "SequentialAgent",
                "Workflow"
            ]
        );
        assert!(g.tools.is_empty() && g.callbacks.is_empty() && g.agents.is_empty());
        assert!(g.node_functions.is_empty() && g.nodes.is_empty());
    }

    #[test]
    fn new_registers_go_init_set() {
        let r = Registry::new();
        let g = r.inner.read();
        assert_eq!(
            sorted_keys(&g.agent_classes),
            [
                "LlmAgent",
                "LoopAgent",
                "ParallelAgent",
                "SequentialAgent",
                "Workflow"
            ]
        );
        assert_eq!(
            sorted_keys(&g.tools),
            [
                "AgentTool",
                "ExampleTool",
                "LongRunningFunctionTool",
                "McpToolset",
                "exit_loop",
                "google_maps_grounding",
                "google_search",
                "url_context"
            ]
        );
        for (name, entry) in &g.tools {
            let is_toolset = matches!(entry, ToolEntry::Toolset(_));
            assert_eq!(is_toolset, name == "McpToolset", "{name}");
        }
        assert!(g.callbacks.is_empty() && g.agents.is_empty());
    }

    #[test]
    fn duplicate_registrations_return_go_texts() {
        let r = Registry::empty();
        let seen = Seen::default();
        let tseen = ToolSeen::default();
        r.register("A", recording_factory(seen.clone())).unwrap();
        assert_eq!(
            r.register("A", recording_factory(seen))
                .cfg_err()
                .to_string(),
            "Register called twice for agent A"
        );
        r.register_tool_factory("t", test_tool_factory(tseen.clone()))
            .unwrap();
        assert_eq!(
            r.register_tool_factory("t", test_tool_factory(tseen.clone()))
                .cfg_err()
                .to_string(),
            "RegisterToolFactory called twice for tool t"
        );
        // Tools and toolsets share one namespace.
        assert_eq!(
            r.register_toolset_factory("t", test_toolset_factory("t", tseen.clone()))
                .cfg_err()
                .to_string(),
            "RegisterToolsetFactory called twice for toolset t"
        );
        r.register_toolset_factory("ts", test_toolset_factory("ts", tseen.clone()))
            .unwrap();
        assert_eq!(
            r.register_tool_factory("ts", test_tool_factory(tseen))
                .cfg_err()
                .to_string(),
            "RegisterToolFactory called twice for tool ts"
        );
        assert!(matches!(r.inner.read().tools["t"], ToolEntry::Tool(_)));
        assert!(matches!(r.inner.read().tools["ts"], ToolEntry::Toolset(_)));
        r.register_callback("cb", Callback::BeforeAgent(noop_before()))
            .unwrap();
        assert_eq!(
            r.register_callback("cb", Callback::AfterAgent(noop_before()))
                .cfg_err()
                .to_string(),
            "RegisterCallback called twice for callback cb"
        );
        assert_eq!(
            r.inner.read().callbacks["cb"].go_type_name(),
            "agent.BeforeAgentCallback"
        );
    }

    #[test]
    fn go_path_helpers_match_go_filepath() {
        assert_eq!(go_dir("/a/b/root_agent.yaml"), "/a/b");
        assert_eq!(go_dir("root_agent.yaml"), ".");
        assert_eq!(go_dir("/root_agent.yaml"), "/");
        assert_eq!(go_dir("a//b/../c/x.yaml"), "a/c");
        assert_eq!(go_dir("/a/b/"), "/a/b");
        assert!(go_is_abs("/x") && !go_is_abs("x") && !go_is_abs("./x"));
        assert_eq!(
            go_abs(Path::new("/a/./b/../c")).unwrap(),
            PathBuf::from("/a/c")
        );
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(go_abs(Path::new("")).unwrap(), cwd);
        assert_eq!(go_abs(Path::new("x/../y")).unwrap(), cwd.join("y"));
    }

    #[tokio::test]
    async fn from_config_missing_file_is_config_file_not_found() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let missing = base.join("a/../missing.yaml");
        let err = r.from_config(&missing).await.cfg_err();
        assert_eq!(
            err.to_string(),
            format!(
                "config file not found: {}",
                base.join("missing.yaml").display()
            )
        );
    }

    #[tokio::test]
    async fn from_config_directory_is_go_read_path_error() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let err = r.from_config(&base).await.cfg_err();
        assert_eq!(
            err.to_string(),
            format!("read {}: is a directory", base.display())
        );
        assert!(!err.is_not_exist());
    }

    #[tokio::test]
    async fn from_config_invalid_yaml_is_wrapped() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let p = write(&base, "bad.yaml", "agent_class: [a]\n");
        assert_eq!(
            r.from_config(&p).await.cfg_err().to_string(),
            "invalid YAML content: yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into string"
        );
        let p = write(&base, "bad2.yaml", "name: [unclosed\n");
        let err = r.from_config(&p).await.cfg_err().to_string();
        assert!(err.starts_with("invalid YAML content: yaml: "), "{err}");
    }

    #[tokio::test]
    async fn from_config_unknown_class() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let p = write(&base, "x.yaml", "agent_class: Nope\nname: x\n");
        assert_eq!(
            r.from_config(&p).await.cfg_err().to_string(),
            "invalid agent class 'Nope': not registered. Ensure the package is imported"
        );
    }

    #[tokio::test]
    async fn from_config_defaults_to_llm_agent_and_passes_bytes_abs_path_and_ctx() {
        let (_d, base) = temp_dir();
        let r = Registry::empty();
        let seen = Seen::default();
        r.register("LlmAgent", recording_factory(seen.clone()))
            .unwrap();
        let content = "name: root\n";
        let p = write(&base, "agents/root.yaml", content);
        // A relative spelling with `..` elements: Go's `filepath.Abs` joins and cleans it.
        let rel = relative_to_cwd(&base.join("agents/sub/../root.yaml"));
        let a = r.from_config(Path::new(&rel)).await.unwrap();
        assert_eq!(a.name(), "root");
        let ctx = LoadCtx::default().with_parent_path(Path::new("/outer/agent.yaml"));
        r.from_config_with_ctx(&ctx, &p).await.unwrap();
        let seen = seen.lock();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], (None, content.as_bytes().to_vec(), p.clone()));
        assert_eq!(
            seen[1],
            (
                Some(PathBuf::from("/outer/agent.yaml")),
                content.as_bytes().to_vec(),
                p.clone()
            )
        );
    }

    #[tokio::test]
    async fn from_config_returns_factory_errors_unwrapped() {
        let (_d, base) = temp_dir();
        let r = Registry::empty();
        r.register(
            "Failing",
            Arc::new(|_r, _c, _d, _p| async { Err(ConfigError::msg("boom")) }.boxed()),
        )
        .unwrap();
        let p = write(&base, "f.yaml", "agent_class: Failing\n");
        assert_eq!(r.from_config(&p).await.cfg_err().to_string(), "boom");
    }

    #[tokio::test]
    async fn resolve_tool_reference_tools_toolsets_and_errors() {
        let r = Registry::empty();
        let seen = ToolSeen::default();
        r.register_tool_factory("t", test_tool_factory(seen.clone()))
            .unwrap();
        r.register_toolset_factory("ts", test_toolset_factory("my_set", seen.clone()))
            .unwrap();
        r.register_tool_factory("bad", failing_tool_factory("args is nil"))
            .unwrap();
        let ctx = LoadCtx::default();

        assert_eq!(
            r.resolve_tool_reference(&ctx, "", None)
                .await
                .cfg_err()
                .to_string(),
            "tool name cannot be empty"
        );
        assert_eq!(
            r.resolve_tool_reference(&ctx, "nope", None)
                .await
                .cfg_err()
                .to_string(),
            "tool 'nope' not found"
        );
        assert_eq!(
            r.resolve_tool_reference(&ctx, "bad", None)
                .await
                .cfg_err()
                .to_string(),
            "args is nil"
        );

        let (tool, toolset) = r.resolve_tool_reference(&ctx, "t", None).await.unwrap();
        assert_eq!(tool.unwrap().name(), "exit_loop");
        assert!(toolset.is_none());

        let mut args = GoMap::new();
        args.insert("k".into(), GoValue::String("v".into()));
        let with_parent = ctx.with_parent_path(Path::new("/p/root.yaml"));
        let (tool, toolset) = r
            .resolve_tool_reference(&with_parent, "ts", Some(&args))
            .await
            .unwrap();
        assert!(tool.is_none());
        assert_eq!(toolset.unwrap().name(), "my_set");

        let seen = seen.lock();
        assert_eq!(seen[0], (None, None));
        assert_eq!(
            seen[1],
            (Some(PathBuf::from("/p/root.yaml")), Some(args.clone()))
        );
    }

    #[test]
    fn resolve_callback_reference_returns_the_registered_callback() {
        let r = Registry::empty();
        let cb = noop_before();
        r.register_callback("cb", Callback::BeforeAgent(cb.clone()))
            .unwrap();
        assert_eq!(
            r.resolve_callback_reference("").cfg_err().to_string(),
            "callback name cannot be empty"
        );
        assert_eq!(
            r.resolve_callback_reference("x").cfg_err().to_string(),
            "callback 'x' not found"
        );
        match r.resolve_callback_reference("cb").unwrap() {
            Callback::BeforeAgent(f) => assert!(Arc::ptr_eq(&f, &cb)),
            Callback::AfterAgent(_) => panic!("wrong kind"),
        }
    }

    /// Go's `newAgentDir` layout plus a plain test agent class (`agent_class: Test`).
    fn agent_dir() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let (d, base) = temp_dir();
        let agent_dir = base.join("agents/root");
        let parent = write(&agent_dir, "root_agent.yaml", "agent_class: Test\n");
        write(&agent_dir, "sub_agent.yaml", "agent_class: Test\n");
        let outside = write(&base, "outside.yaml", "agent_class: Test\n");
        std::os::unix::fs::symlink(&outside, agent_dir.join("link.yaml")).unwrap();
        (d, base, parent)
    }

    fn counting_registry(seen: &Seen) -> Registry {
        let r = Registry::empty();
        r.register("Test", recording_factory(seen.clone())).unwrap();
        r
    }

    #[tokio::test]
    async fn resolve_agent_reference_rejections_have_go_texts() {
        let (_d, _base, parent) = agent_dir();
        let seen = Seen::default();
        let r = counting_registry(&seen);
        let cases = [
            ("", "agent reference path cannot be empty".to_owned()),
            (
                "/etc/passwd",
                "absolute paths are not allowed in AgentTool config_path: /etc/passwd".to_owned(),
            ),
            (
                "../../outside.yaml",
                "path traversal detected: config_path \"../../outside.yaml\" resolves outside agent directory".to_owned(),
            ),
            (
                "link.yaml",
                "path traversal detected: config_path \"link.yaml\" resolves outside agent directory".to_owned(),
            ),
            (
                "../root2/x\"y.yaml",
                "path traversal detected: config_path \"../root2/x\\\"y.yaml\" resolves outside agent directory".to_owned(),
            ),
        ];
        for (ref_path, want) in cases {
            let err = r.resolve_agent_reference(&parent, ref_path).await.cfg_err();
            assert_eq!(err.to_string(), want, "{ref_path}");
        }
        // Rejected before loading anything.
        assert!(seen.lock().is_empty());
    }

    #[tokio::test]
    async fn resolve_agent_reference_allows_symlinks_and_parents_inside_the_directory() {
        let (_d, base, parent) = agent_dir();
        let agent_dir = parent.parent().unwrap().to_path_buf();
        let seen = Seen::default();
        let r = counting_registry(&seen);
        // A symlink inside the directory pointing inside it.
        std::os::unix::fs::symlink(agent_dir.join("sub_agent.yaml"), agent_dir.join("in.yaml"))
            .unwrap();
        assert_eq!(
            r.resolve_agent_reference(&parent, "in.yaml")
                .await
                .unwrap()
                .name(),
            "in"
        );
        // The parent reached through a symlinked directory: both sides are resolved.
        std::os::unix::fs::symlink(&agent_dir, base.join("alias")).unwrap();
        let via_alias = base.join("alias/root_agent.yaml");
        r.resolve_agent_reference(&via_alias, "sub_agent.yaml")
            .await
            .unwrap();
        // `..` that stays inside is fine.
        r.resolve_agent_reference(&parent, "x/../sub_agent.yaml")
            .await
            .unwrap();
        // The loader gets the unresolved absolute path.
        let paths: Vec<PathBuf> = seen.lock().iter().map(|s| s.2.clone()).collect();
        assert_eq!(
            paths,
            [
                agent_dir.join("in.yaml"),
                base.join("alias/sub_agent.yaml"),
                agent_dir.join("sub_agent.yaml")
            ]
        );
    }

    #[tokio::test]
    async fn resolve_agent_reference_load_errors_are_returned_unwrapped() {
        let (_d, _base, parent) = agent_dir();
        let agent_dir = parent.parent().unwrap().to_path_buf();
        let r = counting_registry(&Seen::default());
        // A missing file inside the directory passes the containment check.
        assert_eq!(
            r.resolve_agent_reference(&parent, "missing.yaml")
                .await
                .cfg_err()
                .to_string(),
            format!(
                "config file not found: {}",
                agent_dir.join("missing.yaml").display()
            )
        );
        // `.` is the directory itself (Go's `checkPath == parentDir`), which fails to read.
        assert_eq!(
            r.resolve_agent_reference(&parent, ".")
                .await
                .cfg_err()
                .to_string(),
            format!("read {}: is a directory", agent_dir.display())
        );
    }

    #[tokio::test]
    async fn resolve_agent_reference_caches_by_absolute_path() {
        let (_d, _base, parent) = agent_dir();
        let agent_dir = parent.parent().unwrap().to_path_buf();
        let seen = Seen::default();
        let r = counting_registry(&seen);
        let a = r
            .resolve_agent_reference(&parent, "sub_agent.yaml")
            .await
            .unwrap();
        let b = r
            .resolve_agent_reference(&parent, "./sub_agent.yaml")
            .await
            .unwrap();
        let c = r
            .clone()
            .resolve_agent_reference(&agent_dir.join("other.yaml"), "x/../sub_agent.yaml")
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &c));
        assert_eq!(seen.lock().len(), 1);
        assert!(
            r.inner
                .read()
                .agents
                .contains_key(&agent_dir.join("sub_agent.yaml"))
        );
        // A fresh registry has its own cache.
        let other = counting_registry(&seen);
        let d = other
            .resolve_agent_reference(&parent, "sub_agent.yaml")
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &d));
    }

    #[tokio::test]
    async fn resolve_agent_reference_does_not_cache_failures() {
        let (_d, _base, parent) = agent_dir();
        let agent_dir = parent.parent().unwrap().to_path_buf();
        let r = counting_registry(&Seen::default());
        assert!(
            r.resolve_agent_reference(&parent, "late.yaml")
                .await
                .is_err()
        );
        write(&agent_dir, "late.yaml", "agent_class: Test\n");
        let a = r
            .resolve_agent_reference(&parent, "late.yaml")
            .await
            .unwrap();
        assert_eq!(a.name(), "late");
    }

    #[tokio::test]
    async fn resolve_agent_reference_double_checked_insert_keeps_the_first_agent() {
        let (_d, _base, parent) = agent_dir();
        let agent_dir = parent.parent().unwrap().to_path_buf();
        let calls = Arc::new(AtomicUsize::new(0));
        let r = Registry::empty();
        // The factory caches a different agent under the same path while it runs, as a
        // concurrent load would: the cached one wins.
        let calls2 = calls.clone();
        let target = agent_dir.join("sub_agent.yaml");
        let target2 = target.clone();
        r.register(
            "Test",
            Arc::new(move |reg, _c, _d, _p| {
                let calls = calls2.clone();
                let target = target2.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let first = gadk_agent::new_agent(AgentConfig {
                        name: "first".into(),
                        ..Default::default()
                    })
                    .map_err(ConfigError::other)?;
                    reg.inner.write().agents.insert(target, first);
                    gadk_agent::new_agent(AgentConfig {
                        name: "second".into(),
                        ..Default::default()
                    })
                    .map_err(ConfigError::other)
                }
                .boxed()
            }),
        )
        .unwrap();
        let a = r
            .resolve_agent_reference(&parent, "sub_agent.yaml")
            .await
            .unwrap();
        assert_eq!(a.name(), "first");
        assert_eq!(r.inner.read().agents[&target].name(), "first");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_agent_reference_with_ctx_passes_the_ctx_to_the_loader() {
        let (_d, _base, parent) = agent_dir();
        let seen = Seen::default();
        let r = counting_registry(&seen);
        let ctx = LoadCtx::default().with_parent_path(Path::new("/outer/root.yaml"));
        r.resolve_agent_reference_with_ctx(&ctx, &parent, "sub_agent.yaml")
            .await
            .unwrap();
        assert_eq!(
            seen.lock()[0].0.as_deref(),
            Some(Path::new("/outer/root.yaml"))
        );
    }

    #[test]
    fn registry_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}
        let r = Registry::empty();
        let ctx = LoadCtx::default();
        assert_send(r.from_config(Path::new("x")));
        assert_send(r.resolve_agent_reference(Path::new("x"), "y"));
        assert_send(r.resolve_tool_reference(&ctx, "t", None));
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
