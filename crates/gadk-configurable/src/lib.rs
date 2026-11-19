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

//! gadk-configurable: the YAML agent configs (the `configurable` loader), the `conformance`
//! support (test callbacks, functions, node functions, the agent loader and the record and
//! replay plugins) and the `adkcli` binary.
//!
//! This file holds the registry: the agent-class, agent, tool, callback, node-function and
//! node registries are the fields of one [`Registry`] value, all under one lock;
//! [`Registry::new`] registers the built-in agent classes and tools. The loading context
//! (the parent path and the sub-agents collector) is carried explicitly by [`LoadCtx`].
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
pub(crate) use gadk_platform::go_filepath_rel;
use gadk_platform::{go_path_clean, go_path_dir, go_path_join, go_quote, safe_subpath};
use gadk_session::Event;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

use crate::config::BaseAgentConfig;

pub use error::ConfigError;
pub use gadk_goyaml::GoValue;
pub use govalue::GoMap;

/// Builds an agent from a config file's bytes and the absolute path of its directory. The
/// [`Registry`] (a cheap handle) and the [`LoadCtx`] stand for the shared registries and the
/// loading context.
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

/// Builds a tool from the loading context and the tool config's args; `None` args is a nil
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

/// Builds a toolset from the loading context and the tool config's args.
pub type ToolsetFactory = Arc<
    dyn Fn(
            Registry,
            ToolCtx,
            Option<GoMap>,
        ) -> BoxFuture<'static, Result<Arc<dyn Toolset>, ConfigError>>
        + Send
        + Sync,
>;

/// A node function taking a dynamic JSON value and returning a dynamic output, which the
/// workflow turns into an event, a content or a plain output ([`NodeOutput`]).
pub type NodeFn =
    Arc<dyn Fn(Context, Value) -> BoxFuture<'static, Result<NodeOutput, AgentError>> + Send + Sync>;

/// A node function taking and returning a string, registered through
/// [`Registry::register_string_node_function`] with a display-text input coercion.
pub type StrNodeFn =
    Arc<dyn Fn(Context, String) -> BoxFuture<'static, Result<String, AgentError>> + Send + Sync>;

/// What a [`NodeFn`] returns: a dynamic result, which the workflow's function node
/// distinguishes (a `session` event is forwarded, a `genai` content becomes the event content,
/// anything else is the event output; nil = `Value(Value::Null)`, no output).
#[derive(Debug, Clone)]
pub enum NodeOutput {
    /// A plain output value.
    Value(Value),
    /// A session event (e.g. one carrying `Routes`).
    Event(Box<Event>),
    /// A genai content.
    Content(Content),
}

/// The values of the callback registry, restricted to the two callback types
/// the resolvers accept.
#[derive(Clone)]
pub enum Callback {
    /// A before-agent callback.
    BeforeAgent(BeforeAgentCallback),
    /// An after-agent callback.
    AfterAgent(AfterAgentCallback),
}

impl Callback {
    /// The type name of the stored callback, as shown in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Callback::BeforeAgent(_) => BeforeAgent::TYPE_NAME,
            Callback::AfterAgent(_) => AfterAgent::TYPE_NAME,
        }
    }
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.type_name())
    }
}

/// The callback type `T` a reference must hold when callbacks are resolved.
pub(crate) trait CallbackType {
    /// The Rust callback type.
    type Fn;
    /// The type name of `T` in error messages (the callback type alias, such as
    /// `BeforeAgentCallback`).
    const TYPE_NAME: &'static str;
    /// The callback when it has this type.
    fn from_callback(c: &Callback) -> Option<Self::Fn>;
}

/// The before-agent callback type.
pub(crate) struct BeforeAgent;

/// The after-agent callback type.
pub(crate) struct AfterAgent;

impl CallbackType for BeforeAgent {
    type Fn = BeforeAgentCallback;
    const TYPE_NAME: &'static str = "BeforeAgentCallback";
    fn from_callback(c: &Callback) -> Option<BeforeAgentCallback> {
        match c {
            Callback::BeforeAgent(f) => Some(f.clone()),
            Callback::AfterAgent(_) => None,
        }
    }
}

impl CallbackType for AfterAgent {
    type Fn = AfterAgentCallback;
    const TYPE_NAME: &'static str = "AfterAgentCallback";
    fn from_callback(c: &Callback) -> Option<AfterAgentCallback> {
        match c {
            Callback::AfterAgent(f) => Some(f.clone()),
            Callback::BeforeAgent(_) => None,
        }
    }
}

/// The workflow's sub-agent list that YAML agent nodes are appended to.
pub type SubAgentsCollector = Arc<Mutex<Vec<Arc<dyn Agent>>>>;

/// The values carried while loading configs. Every nested `from_config` / factory call inherits
/// the caller's `LoadCtx`.
#[derive(Clone, Default)]
pub struct LoadCtx {
    /// Set by the tool resolver to the config directory of the
    /// agent whose tools are resolved; `None` = not set
    /// (`parentPath not found in context`).
    pub parent_path: Option<PathBuf>,
    /// Set by the workflow factory around edge parsing.
    pub sub_agents: Option<SubAgentsCollector>,
}

impl LoadCtx {
    /// A copy of the context with the parent path set.
    pub fn with_parent_path(&self, parent_path: &Path) -> LoadCtx {
        LoadCtx {
            parent_path: Some(parent_path.to_path_buf()),
            sub_agents: self.sub_agents.clone(),
        }
    }

    /// A copy of the context with the sub-agents collector set.
    pub fn with_sub_agents(&self, collector: SubAgentsCollector) -> LoadCtx {
        LoadCtx {
            parent_path: self.parent_path.clone(),
            sub_agents: Some(collector),
        }
    }
}

/// The context a tool or toolset factory receives (the same loading context).
pub type ToolCtx = LoadCtx;

/// A tool registry value: either factory type.
#[derive(Clone)]
pub(crate) enum ToolEntry {
    /// A [`ToolFactory`].
    Tool(ToolFactory),
    /// A [`ToolsetFactory`].
    Toolset(ToolsetFactory),
}

/// The registries, guarded together by one lock.
pub(crate) struct Registries {
    /// Agent class → factory.
    pub(crate) agent_classes: HashMap<String, AgentFactory>,
    /// Absolute config path → loaded agent.
    pub(crate) agents: HashMap<PathBuf, Arc<dyn Agent>>,
    /// Tool name → tool or toolset factory.
    pub(crate) tools: HashMap<String, ToolEntry>,
    /// Callback name → callback.
    pub(crate) callbacks: HashMap<String, Callback>,
    /// Node-function name → node function.
    pub(crate) node_functions: HashMap<String, NodeFn>,
    /// An absolute YAML path (lossy UTF-8) or a function reference → node.
    pub(crate) nodes: HashMap<String, Arc<dyn Node>>,
    /// The environment source for the `LlmAgent` factory (`GOOGLE_API_KEY`) and the Gemini
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

/// The configurable-agent loader's shared state as a value: a cheap, clonable handle (clones
/// share the registries).
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
    /// A registry with no registrations (not the built-in set): for tests.
    pub(crate) fn empty() -> Registry {
        Registry {
            inner: Arc::new(RwLock::new(Registries::default())),
        }
    }

    /// A registry with the built-ins: agent classes `LlmAgent`, `LoopAgent`, `ParallelAgent`,
    /// `SequentialAgent`, `Workflow`; tools `exit_loop`, `google_search`, `url_context`,
    /// `google_maps_grounding`, `AgentTool`, `LongRunningFunctionTool`, `ExampleTool`;
    /// toolset `McpToolset`. Registration cannot fail here (fresh maps).
    pub fn new() -> Registry {
        let registry = Registry::with_agent_classes();
        {
            let mut g = registry.inner.write();
            for (name, entry) in builtin_tools::builtin_tool_factories() {
                g.tools.insert(name.to_owned(), entry);
            }
        }
        registry
    }

    /// The agent-class half of [`Registry::new`] (`LlmAgent` … in registration
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

    /// Registers an agent class: `Register called twice for agent {name}` on a duplicate.
    pub fn register(&self, name: &str, factory: AgentFactory) -> Result<(), ConfigError> {
        let mut g = self.inner.write();
        if g.agent_classes.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "Register called twice for agent {name}"
            )));
        }
        g.agent_classes.insert(name.to_owned(), factory);
        Ok(())
    }

    /// Registers a tool factory: `register_tool_factory called twice for tool {name}` on a
    /// duplicate (tools and toolsets share one namespace).
    pub fn register_tool_factory(
        &self,
        name: &str,
        factory: ToolFactory,
    ) -> Result<(), ConfigError> {
        let mut g = self.inner.write();
        if g.tools.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "register_tool_factory called twice for tool {name}"
            )));
        }
        g.tools.insert(name.to_owned(), ToolEntry::Tool(factory));
        Ok(())
    }

    /// Registers a toolset factory: `register_toolset_factory called twice for toolset {name}`.
    pub fn register_toolset_factory(
        &self,
        name: &str,
        factory: ToolsetFactory,
    ) -> Result<(), ConfigError> {
        let mut g = self.inner.write();
        if g.tools.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "register_toolset_factory called twice for toolset {name}"
            )));
        }
        g.tools.insert(name.to_owned(), ToolEntry::Toolset(factory));
        Ok(())
    }

    /// Registers a callback: `register_callback called twice for callback {name}`.
    pub fn register_callback(&self, name: &str, callback: Callback) -> Result<(), ConfigError> {
        let mut g = self.inner.write();
        if g.callbacks.contains_key(name) {
            return Err(ConfigError::msg(format!(
                "register_callback called twice for callback {name}"
            )));
        }
        g.callbacks.insert(name.to_owned(), callback);
        Ok(())
    }

    /// The top-level entry point: loads an agent from a config file (an empty
    /// [`LoadCtx`]).
    pub async fn from_config(&self, config_path: &Path) -> Result<Arc<dyn Agent>, ConfigError> {
        self.from_config_with_ctx(&LoadCtx::default(), config_path)
            .await
    }

    /// Loads an agent from a config file with a loading context: makes the path absolute, reads
    /// the file, peeks `agent_class` (default `LlmAgent`), calls the factory with the file's
    /// bytes and absolute path. Boxed: the factories recurse into it.
    #[allow(clippy::wrong_self_convention)] // a `from_*` name; `self` is the registry.
    pub(crate) fn from_config_with_ctx<'a>(
        &'a self,
        ctx: &'a LoadCtx,
        config_path: &'a Path,
    ) -> BoxFuture<'a, Result<Arc<dyn Agent>, ConfigError>> {
        async move {
            let abs_path = go_abs(config_path)
                .map_err(|e| ConfigError::wrap("failed to resolve absolute path", e))?;

            // 1. Read the file (`open` then `read` path errors).
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

            let agent_dir = PathBuf::from(go_path_dir(&abs_path.to_string_lossy()));
            // 4. Delegate creation to the specific factory.
            factory(self.clone(), ctx.clone(), data, agent_dir).await
        }
        .boxed()
    }

    /// Resolves a tool reference: `(tool, None)` from a tool factory, `(None, toolset)` from a
    /// toolset factory; `tool name cannot be empty`, `tool '{name}' not found`.
    // A third branch, `tool '{name}' is not a tool or toolset factory`, is unreachable: the
    // registry's values are typed.
    pub async fn resolve_tool_reference(
        &self,
        ctx: &ToolCtx,
        tool_name: &str,
        args: Option<&GoMap>,
    ) -> Result<(Option<Arc<dyn Tool>>, Option<Arc<dyn Toolset>>), ConfigError> {
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

    /// Resolves a callback reference: `callback name cannot be empty`,
    /// `callback '{name}' not found`.
    pub fn resolve_callback_reference(&self, callback_name: &str) -> Result<Callback, ConfigError> {
        if callback_name.is_empty() {
            return Err(ConfigError::msg("callback name cannot be empty"));
        }
        let callback = self.inner.read().callbacks.get(callback_name).cloned();
        callback.ok_or_else(|| ConfigError::msg(format!("callback '{callback_name}' not found")))
    }

    /// The top-level entry point for a sub-agent reference (an empty [`LoadCtx`]). `config_dir`
    /// is the directory of the referencing config and `ref_path` the reference as written
    /// there.
    pub async fn resolve_agent_reference(
        &self,
        config_dir: &Path,
        ref_path: &str,
    ) -> Result<Arc<dyn Agent>, ConfigError> {
        self.resolve_agent_reference_with_ctx(&LoadCtx::default(), config_dir, ref_path)
            .await
    }

    /// Resolves a sub-agent reference with a loading context: the containment check
    /// ([`resolve_config_reference`]), then the cached agent for the resolved real path, else
    /// loads it with [`Registry::from_config_with_ctx`] and caches it (double-checked insert:
    /// the first cached agent wins). Boxed: recursive.
    pub(crate) fn resolve_agent_reference_with_ctx<'a>(
        &'a self,
        ctx: &'a LoadCtx,
        config_dir: &'a Path,
        ref_path: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn Agent>, ConfigError>> {
        async move {
            let abs_path = resolve_config_reference(config_dir, ref_path)?;

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

/// The one containment check for every config reference (a
/// `sub_agents` or AgentTool `config_path` and a workflow edge), `config reference {ref:?}:
/// {cause}` around [`gadk_platform::safe_subpath`]. The result is the path the
/// check followed the reference to, links replaced by their targets: it is what gets read and
/// what keys the registries, so the file judged is the file opened.
pub(crate) fn resolve_config_reference(
    config_dir: &Path,
    ref_path: &str,
) -> Result<PathBuf, ConfigError> {
    match safe_subpath(config_dir, ref_path) {
        Ok((abs, _)) => Ok(abs),
        Err(e) => Err(ConfigError::wrap(
            format!("config reference {}", go_quote(ref_path)),
            ConfigError::msg(e.to_string()),
        )),
    }
}

/// Whether a path is absolute (Unix): it starts with `/`.
pub(crate) fn go_is_abs(path: &str) -> bool {
    path.starts_with('/')
}

/// The absolute form (Unix): an absolute path cleaned, a relative one joined to the working
/// directory (its only error) and cleaned. Lexical: `..` is resolved without
/// following symlinks, unlike `std::fs::canonicalize`.
pub(crate) fn go_abs(path: &Path) -> Result<PathBuf, ConfigError> {
    let p = path.to_string_lossy();
    if go_is_abs(&p) {
        return Ok(PathBuf::from(go_path_clean(&p)));
    }
    let cwd = go_getwd()?;
    Ok(PathBuf::from(go_path_join(&[
        cwd.to_string_lossy().as_ref(),
        p.as_ref(),
    ])))
}

/// The conformance plugins' base-directory check (the record and replay plugins repeat it
/// inline): the absolute form of the allowed base and of `case_dir`, the relative path between
/// them, and a `..`-prefixed or absolute result rejected with `<plugin> config error: 'dir' is
/// not within the allowed base directory`; an absolute-form or relative-path failure is
/// `invalid path format: {cause}`. Returns the absolute form of `case_dir`.
pub(crate) fn check_within_base_dir(
    allowed_base_dir: &Path,
    case_dir: &str,
    plugin: &str,
) -> Result<String, String> {
    let invalid = |e: String| format!("invalid path format: {e}");
    let base_path = go_abs(allowed_base_dir).map_err(|e| invalid(e.to_string()))?;
    let requested_abs_path = go_abs(Path::new(case_dir)).map_err(|e| invalid(e.to_string()))?;
    let base_path = base_path.to_string_lossy().into_owned();
    let requested_abs_path = requested_abs_path.to_string_lossy().into_owned();
    let rel = go_filepath_rel(&base_path, &requested_abs_path).map_err(invalid)?;
    if rel.starts_with("..") || go_is_abs(&rel) {
        return Err(format!(
            "{plugin} config error: 'dir' is not within the allowed base directory"
        ));
    }
    Ok(requested_abs_path)
}

/// The working directory (Unix): `$PWD` when it is absolute and names the same file as `.`
/// (so a working directory reached through a symlink keeps the spelling the shell gave it),
/// otherwise the OS working directory (`getcwd`, symlinks resolved). `std::env::current_dir`
/// is only the second half.
///
/// Errors print like ADK's loader: a failed `stat` of `.` (only checked when `$PWD` is
/// absolute) is the path error `stat .: <errno>`, a failed `getcwd` is `getwd: <errno>`
/// (`getwd: no such file or directory` once the directory is deleted).
pub fn go_getwd() -> Result<PathBuf, ConfigError> {
    let stat_dot =
        || std::fs::metadata(".").map_err(|e| ConfigError::path("stat", Path::new("."), &e));
    // Use $PWD when it is set and matches ".".
    let mut dot = None;
    if let Some(dir) = std::env::var_os("PWD")
        && dir.to_string_lossy().starts_with('/')
    {
        let d0 = stat_dot()?;
        if let Ok(d) = std::fs::metadata(&dir)
            && same_file(&d0, &d)
        {
            return Ok(PathBuf::from(dir));
        }
        dot = Some(d0);
    }

    // Otherwise ask the operating system for the working directory.
    let err = match std::env::current_dir() {
        Ok(dir) => return Ok(dir),
        Err(e) => e,
    };
    let getwd_err = ConfigError::msg(format!("getwd: {}", error::go_errno_text(&err)));
    // Only ENAMETOOLONG, EINVAL, ERANGE and ENOMEM are retryable (ERANGE is 34 on Linux and
    // the BSDs; std maps it to no `ErrorKind`).
    let retry = matches!(
        err.kind(),
        io::ErrorKind::InvalidFilename | io::ErrorKind::InvalidInput | io::ErrorKind::OutOfMemory
    ) || err.raw_os_error() == Some(34);
    if !retry {
        return Err(getwd_err);
    }
    // We're trying to find our way back to ".".
    if dot.is_none() {
        stat_dot()?;
    }
    // `getcwd` is Rust std's (a 512-byte buffer grown on ERANGE, where ADK's loader passes
    // 1024 bytes), so where libc answers by buffer size (macOS, for an unlinked or
    // unsearchable working directory) the error, or a success, can differ; and the walk-up
    // fallback after a retryable `getcwd` error is not implemented (gadk reports the error).
    Err(getwd_err)
}

/// The same device and inode.
#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Non-Unix: `$PWD` is never trusted (the OS is asked only).
#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    false
}

/// Reads a file: an `open` path error when the file cannot be opened, a `read` one when it
/// cannot be read (a directory opens but fails with `is a directory`).
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>, ConfigError> {
    let mut file = std::fs::File::open(path).map_err(|e| ConfigError::path("open", path, &e))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| ConfigError::path("read", path, &e))?;
    Ok(data)
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
        let cwd = go_getwd().unwrap();
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

    /// Go `init()`'s agent classes with a hermetic environment (no built-in tools: those are in
    /// `builtin_tools`).
    pub(crate) fn test_registry() -> Registry {
        let r = Registry::with_agent_classes();
        r.set_env(fake_key_env());
        r
    }

    /// What a [`recording_factory`] saw: the context's parent path, the bytes and the path.
    pub(crate) type Seen = Arc<Mutex<Vec<(Option<PathBuf>, Vec<u8>, PathBuf)>>>;

    /// An agent factory that records its inputs and builds a plain agent named after the
    /// config's `name:` line, else after the directory it was given.
    pub(crate) fn recording_factory(seen: Seen) -> AgentFactory {
        Arc::new(move |_r, ctx, data, path| {
            let seen = seen.clone();
            async move {
                let name = String::from_utf8_lossy(&data)
                    .lines()
                    .find_map(|l| l.strip_prefix("name: ").map(str::to_owned))
                    .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned());
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
            "register_tool_factory called twice for tool t"
        );
        // Tools and toolsets share one namespace.
        assert_eq!(
            r.register_toolset_factory("t", test_toolset_factory("t", tseen.clone()))
                .cfg_err()
                .to_string(),
            "register_toolset_factory called twice for toolset t"
        );
        r.register_toolset_factory("ts", test_toolset_factory("ts", tseen.clone()))
            .unwrap();
        assert_eq!(
            r.register_tool_factory("ts", test_tool_factory(tseen))
                .cfg_err()
                .to_string(),
            "register_tool_factory called twice for tool ts"
        );
        assert!(matches!(r.inner.read().tools["t"], ToolEntry::Tool(_)));
        assert!(matches!(r.inner.read().tools["ts"], ToolEntry::Toolset(_)));
        r.register_callback("cb", Callback::BeforeAgent(noop_before()))
            .unwrap();
        assert_eq!(
            r.register_callback("cb", Callback::AfterAgent(noop_before()))
                .cfg_err()
                .to_string(),
            "register_callback called twice for callback cb"
        );
        assert_eq!(
            r.inner.read().callbacks["cb"].type_name(),
            "BeforeAgentCallback"
        );
    }

    #[test]
    fn go_path_helpers_match_go_filepath() {
        assert!(go_is_abs("/x") && !go_is_abs("x") && !go_is_abs("./x"));
        assert_eq!(
            go_abs(Path::new("/a/./b/../c")).unwrap(),
            PathBuf::from("/a/c")
        );
        let cwd = go_getwd().unwrap();
        assert_eq!(go_abs(Path::new("")).unwrap(), cwd);
        assert_eq!(go_abs(Path::new("x/../y")).unwrap(), cwd.join("y"));
    }

    #[test]
    fn go_filepath_rel_matches_go() {
        // Go path/filepath/path_test.go reltests (Unix rows).
        for (root, path, want) in [
            ("a/b", "a/b", Ok(".")),
            ("a/b/.", "a/b", Ok(".")),
            ("a/b", "a/b/.", Ok(".")),
            ("./a/b", "a/b", Ok(".")),
            ("a/b", "./a/b", Ok(".")),
            ("ab/cd", "ab/cde", Ok("../cde")),
            ("ab/cd", "ab/c", Ok("../c")),
            ("a/b", "a/b/c/d", Ok("c/d")),
            ("a/b", "a/b/../c", Ok("../c")),
            ("a/b/../c", "a/b", Ok("../b")),
            ("a/b/c", "a/c/d", Ok("../../c/d")),
            ("a/b", "c/d", Ok("../../c/d")),
            ("a/b/c/d", "a/b", Ok("../..")),
            ("a/b/c/d", "a/b/", Ok("../..")),
            ("a/b/c/d/", "a/b", Ok("../..")),
            ("a/b/c/d/", "a/b/", Ok("../..")),
            ("../../a/b", "../../a/b/c/d", Ok("c/d")),
            ("/a/b", "/a/b", Ok(".")),
            ("/a/b/.", "/a/b", Ok(".")),
            ("/a/b", "/a/b/.", Ok(".")),
            ("/ab/cd", "/ab/cde", Ok("../cde")),
            ("/ab/cd", "/ab/c", Ok("../c")),
            ("/a/b", "/a/b/c/d", Ok("c/d")),
            ("/a/b", "/a/b/../c", Ok("../c")),
            ("/a/b/../c", "/a/b", Ok("../b")),
            ("/a/b/c", "/a/c/d", Ok("../../c/d")),
            ("/a/b", "/c/d", Ok("../../c/d")),
            ("/a/b/c/d", "/a/b", Ok("../..")),
            ("/a/b/c/d", "/a/b/", Ok("../..")),
            ("/a/b/c/d/", "/a/b", Ok("../..")),
            ("/a/b/c/d/", "/a/b/", Ok("../..")),
            ("/../../a/b", "/../../a/b/c/d", Ok("c/d")),
            (".", "a/b", Ok("a/b")),
            (".", "..", Ok("..")),
            // can't do purely lexically
            ("..", ".", Err(())),
            ("..", "a", Err(())),
            ("../..", "..", Err(())),
            ("a", "/a", Err(())),
            ("/a", "a", Err(())),
            // Rows beyond Go's table.
            ("/a/b", "/a/b/c", Ok("c")),
            ("/a/b", "/a", Ok("..")),
            ("/a/b", "/a/c/d", Ok("../c/d")),
            ("/a/b/c", "/x", Ok("../../../x")),
            ("/", "/a/b", Ok("a/b")),
            ("/a", "/", Ok("..")),
            ("/a/b/", "/a/b/c/./d/../e", Ok("c/e")),
            ("/ab", "/abc", Ok("../abc")),
            ("a", "a/b", Ok("b")),
            ("/a", "b", Err(())),
            ("../a", "b", Err(())),
        ] {
            let got = go_filepath_rel(root, path);
            match want {
                Ok(w) => assert_eq!(got.as_deref(), Ok(w), "Rel({root:?}, {path:?})"),
                Err(()) => assert_eq!(
                    got,
                    Err(format!("Rel: can't make {path} relative to {root}")),
                    "Rel({root:?}, {path:?})"
                ),
            }
        }
    }

    /// `go_getwd` names the working directory: `$PWD` (when it is `.`) or `getcwd`. The `$PWD`
    /// spelling through a symlink is covered end to end by `tests/adkcli.rs` (a child process
    /// with its own environment).
    #[test]
    fn go_getwd_names_the_working_directory() {
        let wd = go_getwd().unwrap();
        assert!(wd.is_absolute());
        assert_eq!(
            std::fs::canonicalize(&wd).unwrap(),
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap()
        );
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
            "invalid YAML content: yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into String"
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
        let dir = p.parent().unwrap().to_path_buf();
        assert_eq!(seen[0], (None, content.as_bytes().to_vec(), dir.clone()));
        assert_eq!(
            seen[1],
            (
                Some(PathBuf::from("/outer/agent.yaml")),
                content.as_bytes().to_vec(),
                dir
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
        r.register_tool_factory("bad", failing_tool_factory("args is missing"))
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
            "args is missing"
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
    /// `(guard, base, base/agents/root)`: the agent directory (Go 0990f9e passes the referencing
    /// config's directory) with `root_agent.yaml`, `sub_agent.yaml` and `link.yaml` ->
    /// `base/outside.yaml`.
    fn agent_dir() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let (d, base) = temp_dir();
        let agent_dir = base.join("agents/root");
        write(&agent_dir, "root_agent.yaml", "agent_class: Test\n");
        write(
            &agent_dir,
            "sub_agent.yaml",
            "agent_class: Test\nname: sub_agent\n",
        );
        let outside = write(&base, "outside.yaml", "agent_class: Test\n");
        std::os::unix::fs::symlink(&outside, agent_dir.join("link.yaml")).unwrap();
        (d, base, agent_dir)
    }

    fn counting_registry(seen: &Seen) -> Registry {
        let r = Registry::empty();
        r.register("Test", recording_factory(seen.clone())).unwrap();
        r
    }

    #[tokio::test]
    async fn resolve_agent_reference_rejections_have_go_texts() {
        let (_d, base, agent_dir) = agent_dir();
        let seen = Seen::default();
        let r = counting_registry(&seen);
        let not_valid = "path is not a relative path inside the base directory";
        let cases = [
            (
                "",
                format!("config reference \"\": {not_valid}: rel path is empty"),
            ),
            (
                "/etc/passwd",
                format!("config reference \"/etc/passwd\": {not_valid}: rel path is not local"),
            ),
            (
                "../../outside.yaml",
                format!(
                    "config reference \"../../outside.yaml\": {not_valid}: rel path is not local"
                ),
            ),
            (
                ".",
                format!("config reference \".\": {not_valid}: rel is effectively empty"),
            ),
            (
                "link.yaml",
                format!(
                    "config reference \"link.yaml\": path leads outside the base directory \
                     through a link: it resolves to \"{}\"",
                    base.join("outside.yaml").display()
                ),
            ),
            (
                "../root2/x\"y.yaml",
                format!(
                    "config reference \"../root2/x\\\"y.yaml\": {not_valid}: rel path is not local"
                ),
            ),
        ];
        for (ref_path, want) in cases {
            let err = r
                .resolve_agent_reference(&agent_dir, ref_path)
                .await
                .cfg_err();
            assert_eq!(err.to_string(), want, "{ref_path}");
        }
        // Rejected before loading anything.
        assert!(seen.lock().is_empty());
    }

    #[tokio::test]
    async fn resolve_agent_reference_allows_symlinks_and_parents_inside_the_directory() {
        let (_d, base, agent_dir) = agent_dir();
        let seen = Seen::default();
        let r = counting_registry(&seen);
        // A symlink inside the directory pointing inside it: loaded from (and cached under) the
        // file it points at.
        std::os::unix::fs::symlink(agent_dir.join("sub_agent.yaml"), agent_dir.join("in.yaml"))
            .unwrap();
        let a = r
            .resolve_agent_reference(&agent_dir, "in.yaml")
            .await
            .unwrap();
        assert_eq!(a.name(), "sub_agent");
        // The directory reached through a symlink: the base is trusted and followed.
        std::os::unix::fs::symlink(&agent_dir, base.join("alias")).unwrap();
        let b = r
            .resolve_agent_reference(&base.join("alias"), "sub_agent.yaml")
            .await
            .unwrap();
        // `..` that stays inside is fine.
        let c = r
            .resolve_agent_reference(&agent_dir, "x/../sub_agent.yaml")
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &c));
        // The loader gets the real path, once.
        let paths: Vec<PathBuf> = seen.lock().iter().map(|s| s.2.clone()).collect();
        assert_eq!(paths, std::slice::from_ref(&agent_dir));
        assert!(
            r.inner
                .read()
                .agents
                .contains_key(&agent_dir.join("sub_agent.yaml"))
        );
    }

    #[tokio::test]
    async fn resolve_agent_reference_load_errors_are_returned_unwrapped() {
        let (_d, _base, agent_dir) = agent_dir();
        let r = counting_registry(&Seen::default());
        // A missing file inside the directory passes the containment check.
        assert_eq!(
            r.resolve_agent_reference(&agent_dir, "missing.yaml")
                .await
                .cfg_err()
                .to_string(),
            format!(
                "config file not found: {}",
                agent_dir.join("missing.yaml").display()
            )
        );
        // A directory inside passes the check and fails to read.
        std::fs::create_dir(agent_dir.join("d.yaml")).unwrap();
        assert_eq!(
            r.resolve_agent_reference(&agent_dir, "d.yaml")
                .await
                .cfg_err()
                .to_string(),
            format!(
                "read {}: is a directory",
                agent_dir.join("d.yaml").display()
            )
        );
    }

    #[tokio::test]
    async fn resolve_agent_reference_caches_by_absolute_path() {
        let (_d, _base, parent) = agent_dir();
        let agent_dir = parent.clone();
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
            .resolve_agent_reference(&agent_dir, "x/../sub_agent.yaml")
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
        let agent_dir = parent.clone();
        let r = counting_registry(&Seen::default());
        assert!(
            r.resolve_agent_reference(&parent, "late.yaml")
                .await
                .is_err()
        );
        write(&agent_dir, "late.yaml", "agent_class: Test\nname: late\n");
        let a = r
            .resolve_agent_reference(&parent, "late.yaml")
            .await
            .unwrap();
        assert_eq!(a.name(), "late");
    }

    #[tokio::test]
    async fn resolve_agent_reference_double_checked_insert_keeps_the_first_agent() {
        let (_d, _base, parent) = agent_dir();
        let agent_dir = parent.clone();
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
    fn callback_type_names() {
        let before = Callback::BeforeAgent(noop_before());
        let after = Callback::AfterAgent(noop_before());
        assert_eq!(before.type_name(), "BeforeAgentCallback");
        assert_eq!(after.type_name(), "AfterAgentCallback");
        assert_eq!(format!("{before:?}"), "BeforeAgentCallback");
    }

    #[test]
    fn callback_type_asserts_like_go() {
        let before = Callback::BeforeAgent(noop_before());
        assert!(BeforeAgent::from_callback(&before).is_some());
        assert!(AfterAgent::from_callback(&before).is_none());
        let after = Callback::AfterAgent(noop_before());
        assert!(AfterAgent::from_callback(&after).is_some());
        assert!(BeforeAgent::from_callback(&after).is_none());
        assert_eq!(BeforeAgent::TYPE_NAME, "BeforeAgentCallback");
        assert_eq!(AfterAgent::TYPE_NAME, "AfterAgentCallback");
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
