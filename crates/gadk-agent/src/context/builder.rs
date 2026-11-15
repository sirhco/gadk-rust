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

//! The context builder (port of Go `agent/context_mock.go`, `agent/common_context_mock.go` and
//! `internal/context.NewInvocationContext`) and its in-crate default session.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gadk_genai::Content;
use gadk_platform::Platform;
use gadk_session::{Event, Session, SessionError, SessionService, State};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentConfig, CustomAgent};
use crate::context::activation::ActivationId;
use crate::context::cancel::CancelScope;
use crate::context::context::Context;
use crate::context::invocation::InvocationContext;
use crate::context::readonly::{ContextInner, Shared};
use crate::mode::BoundModes;
use crate::parent_map::ParentMap;
use crate::plugin_manager::PluginManager;
use crate::run_config::RunConfig;
use crate::run_config::run_limits::LlmCallLimiter;
use crate::services::{Artifacts, Memory};
use crate::state::{AgentState, AgentType};
use crate::teardown::Teardowns;

/// Builder for contexts (Go `context_mock.go` / `common_context_mock.go` /
/// `internal/context.NewInvocationContext`).
///
/// Defaults: an in-crate in-memory session (app `"app"` / user `"user"` / id `"session"`),
/// `Platform::default()`, a custom agent named `"test_agent"`, invocation id
/// `"e-" + platform.new_uuid()` (Go `NewInvocationContext`), empty branch and isolation scope, a
/// fresh `CancellationToken`, `RunConfig::default()`, no resume inputs, and no artifacts, memory,
/// plugins, parent map, session service, call limiter, compaction runtime or teardown registry.
#[derive(Default)]
pub struct ContextBuilder {
    session: Option<Arc<dyn Session>>,
    session_service: Option<Arc<dyn SessionService>>,
    agent: Option<Arc<dyn Agent>>,
    invocation_id: Option<String>,
    user_content: Option<Content>,
    artifacts: Option<Arc<dyn Artifacts>>,
    memory: Option<Arc<dyn Memory>>,
    run_config: Option<RunConfig>,
    platform: Option<Platform>,
    plugin_manager: Option<Arc<PluginManager>>,
    parent_map: Option<Arc<ParentMap>>,
    branch: Option<String>,
    isolation_scope: Option<String>,
    cancellation_token: Option<CancellationToken>,
    llm_call_limiter: Option<Arc<LlmCallLimiter>>,
    resumed_inputs: Option<Map<String, Value>>,
    compaction: Option<Arc<dyn Any + Send + Sync>>,
    teardowns: Option<Arc<Teardowns>>,
}

impl ContextBuilder {
    /// A builder with every default.
    pub fn new() -> Self {
        Self::default()
    }

    /// The session.
    pub fn session(mut self, s: Arc<dyn Session>) -> Self {
        self.session = Some(s);
        self
    }

    /// The session service.
    pub fn session_service(mut self, s: Arc<dyn SessionService>) -> Self {
        self.session_service = Some(s);
        self
    }

    /// The current agent.
    pub fn agent(mut self, a: Arc<dyn Agent>) -> Self {
        self.agent = Some(a);
        self
    }

    /// The invocation id; an empty id takes the `"e-"` default.
    pub fn invocation_id(mut self, id: impl Into<String>) -> Self {
        self.invocation_id = Some(id.into());
        self
    }

    /// The user content.
    pub fn user_content(mut self, c: Content) -> Self {
        self.user_content = Some(c);
        self
    }

    /// The scoped artifacts handle.
    pub fn artifacts(mut self, a: Arc<dyn Artifacts>) -> Self {
        self.artifacts = Some(a);
        self
    }

    /// The scoped memory handle.
    pub fn memory(mut self, m: Arc<dyn Memory>) -> Self {
        self.memory = Some(m);
        self
    }

    /// The run configuration.
    pub fn run_config(mut self, c: RunConfig) -> Self {
        self.run_config = Some(c);
        self
    }

    /// The platform.
    pub fn platform(mut self, p: Platform) -> Self {
        self.platform = Some(p);
        self
    }

    /// The plugin manager.
    pub fn plugin_manager(mut self, m: Arc<PluginManager>) -> Self {
        self.plugin_manager = Some(m);
        self
    }

    /// The agent tree's parent map.
    pub fn parent_map(mut self, m: Arc<ParentMap>) -> Self {
        self.parent_map = Some(m);
        self
    }

    /// The branch.
    pub fn branch(mut self, b: impl Into<String>) -> Self {
        self.branch = Some(b.into());
        self
    }

    /// The isolation scope.
    pub fn isolation_scope(mut self, s: impl Into<String>) -> Self {
        self.isolation_scope = Some(s.into());
        self
    }

    /// The cancellation token.
    pub fn cancellation_token(mut self, t: CancellationToken) -> Self {
        self.cancellation_token = Some(t);
        self
    }

    /// The invocation's LLM-call limiter.
    pub fn llm_call_limiter(mut self, l: Arc<LlmCallLimiter>) -> Self {
        self.llm_call_limiter = Some(l);
        self
    }

    /// Go `InvocationContextParams.ResumedInputs` (interrupt id → response).
    pub fn resumed_inputs(mut self, m: Map<String, Value>) -> Self {
        self.resumed_inputs = Some(m);
        self
    }

    /// The engine's per-invocation compaction runtime (engine-internal).
    #[doc(hidden)]
    pub fn compaction_runtime(mut self, rt: Arc<dyn Any + Send + Sync>) -> Self {
        self.compaction = Some(rt);
        self
    }

    /// The invocation's teardown registry ([`Teardowns`]): the runner installs one per
    /// invocation and awaits it before the after-run callbacks.
    pub fn teardowns(mut self, t: Arc<Teardowns>) -> Self {
        self.teardowns = Some(t);
        self
    }

    /// Builds the invocation context: no actions, not ended, every node field empty. An empty
    /// explicit invocation id also takes the `"e-"` default, as Go's `params.InvocationID == ""`
    /// check does.
    pub fn build_invocation(self) -> InvocationContext {
        let platform = self.platform.unwrap_or_default();
        let invocation_id = match self.invocation_id {
            Some(id) if !id.is_empty() => id,
            _ => format!("e-{}", platform.new_uuid()),
        };
        let session = self
            .session
            .unwrap_or_else(|| LocalSession::new("app", "user", "session", &platform));
        let agent = self.agent.unwrap_or_else(|| {
            let a: Arc<dyn Agent> = CustomAgent::build(
                AgentConfig {
                    name: "test_agent".into(),
                    ..Default::default()
                },
                AgentState {
                    agent_type: AgentType::CustomAgent,
                    config: None,
                    on_before_agent_callback_event: None,
                    live: None,
                },
            );
            a
        });
        let shared = Shared {
            invocation_id,
            artifacts: self.artifacts,
            memory: self.memory,
            run_config: self.run_config.unwrap_or_default(),
            platform,
            plugin_manager: self.plugin_manager,
            parent_map: self.parent_map,
            session_service: self.session_service,
            llm_call_limiter: self.llm_call_limiter,
            compaction: self.compaction,
            teardowns: self.teardowns,
        };
        InvocationContext::from_inner(ContextInner {
            shared: Arc::new(shared),
            session,
            agent,
            branch: self.branch.unwrap_or_default(),
            isolation_scope: self.isolation_scope.unwrap_or_default(),
            user_content: self.user_content.map(Arc::new),
            ended: Arc::new(AtomicBool::new(false)),
            activation: ActivationId::next(),
            cancel_scope: CancelScope::root(self.cancellation_token.unwrap_or_default()),
            bound_modes: BoundModes::default(),
            live_handle: Arc::new(parking_lot::Mutex::new(String::new())),
            resumed_inputs: self.resumed_inputs.map(Arc::new),
            actions: None,
            function_call_id: String::new(),
            tool_confirmation: None,
            path: String::new(),
            run_id: String::new(),
            sub_scheduler: None,
            output_for_ancestors: None,
            enclosing_path: String::new(),
        })
    }

    /// Builds the full context (Go `Promote` of the built invocation context).
    pub fn build(self) -> Context {
        self.build_invocation().into_context()
    }
}

/// In-memory session for `ContextBuilder`'s default (Go tests build invocation contexts on
/// in-memory sessions). No service, no persistence.
pub(crate) struct LocalSession {
    pub(crate) app: String,
    pub(crate) user: String,
    pub(crate) id: String,
    pub(crate) state: LocalState,
    pub(crate) events: parking_lot::RwLock<Vec<Event>>,
    pub(crate) last_update: chrono::DateTime<chrono::Utc>,
}

impl LocalSession {
    /// A session with the given ids; `last_update` is `platform.now()`.
    pub(crate) fn new(app: &str, user: &str, id: &str, platform: &Platform) -> Arc<Self> {
        Arc::new(Self {
            app: app.to_string(),
            user: user.to_string(),
            id: id.to_string(),
            state: LocalState::default(),
            events: parking_lot::RwLock::new(Vec::new()),
            last_update: platform.now(),
        })
    }
}

impl Session for LocalSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        &self.app
    }

    fn user_id(&self) -> &str {
        &self.user
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn events(&self) -> Vec<Event> {
        self.events.read().clone()
    }

    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        self.last_update
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go session state map: `get` → `SessionError::StateKeyNotExist` when absent; `set` overwrites;
/// `all` snapshots. No lock is held past the call.
#[derive(Default)]
pub(crate) struct LocalState(parking_lot::RwLock<Map<String, Value>>);

impl State for LocalState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.0
            .read()
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.0.write().insert(key.to_string(), value);
        Ok(())
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.0
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Shared helpers for the internal parity tests.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod test_support {
    use std::any::Any;
    use std::sync::Arc;

    use futures::future::BoxFuture;
    use futures::{FutureExt, StreamExt};
    use gadk_artifact::{ListResponse, LoadResponse, SaveResponse};
    use gadk_genai::Part;
    use gadk_platform::Platform;
    use gadk_session::{Event, Session, State};
    use serde_json::Value;

    use super::{LocalSession, LocalState};
    use crate::agent::{Agent, AgentConfig};
    use crate::error::AgentError;
    use crate::scheduler::DynamicSubScheduler;
    use crate::services::Artifacts;

    /// An in-memory session with the given ids (empty strings allowed: Go's userless session).
    pub(crate) fn session(app: &str, user: &str, id: &str) -> Arc<dyn Session> {
        LocalSession::new(app, user, id, &Platform::default())
    }

    /// Go `matrixSession` / `mappingSession`: fixed ids, empty state, no events, zero time.
    pub(crate) struct TestSession {
        app: String,
        user: String,
        id: String,
        state: LocalState,
    }

    impl TestSession {
        pub(crate) fn shared(app: &str, user: &str, id: &str) -> Arc<dyn Session> {
            Arc::new(Self {
                app: app.into(),
                user: user.into(),
                id: id.into(),
                state: LocalState::default(),
            })
        }
    }

    impl Session for TestSession {
        fn id(&self) -> &str {
            &self.id
        }
        fn app_name(&self) -> &str {
            &self.app
        }
        fn user_id(&self) -> &str {
            &self.user
        }
        fn state(&self) -> &dyn State {
            &self.state
        }
        fn events(&self) -> Vec<Event> {
            Vec::new()
        }
        fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
            gadk_session::zero_time()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Go `swappingSession`: every id accessor advances one shared generation under a mutex and
    /// returns `"<prefix>-<generation>"` (`"sid"`, `"app"`, `"user"`). The strings are leaked
    /// (`Box::leak`) because `Session` returns `&str`; a test reads at most a few thousand of them.
    #[derive(Default)]
    pub(crate) struct SwappingSession {
        generation: parking_lot::Mutex<u64>,
        state: LocalState,
    }

    impl SwappingSession {
        pub(crate) fn swap(&self) {
            *self.generation.lock() += 1;
        }
        pub(crate) fn now(&self) -> u64 {
            *self.generation.lock()
        }
        fn tag(&self, prefix: &str) -> &'static str {
            let mut g = self.generation.lock();
            *g += 1;
            Box::leak(format!("{prefix}-{}", *g).into_boxed_str())
        }
    }

    impl Session for SwappingSession {
        fn id(&self) -> &str {
            self.tag("sid")
        }
        fn app_name(&self) -> &str {
            self.tag("app")
        }
        fn user_id(&self) -> &str {
            self.tag("user")
        }
        fn state(&self) -> &dyn State {
            &self.state
        }
        fn events(&self) -> Vec<Event> {
            Vec::new()
        }
        fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
            gadk_session::zero_time()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// A custom agent with an empty run.
    pub(crate) fn named_agent(name: &str) -> Arc<dyn Agent> {
        crate::agent::new(AgentConfig {
            name: name.into(),
            run: Some(Arc::new(|_| futures::stream::empty().boxed())),
            ..Default::default()
        })
        .unwrap()
    }

    pub(crate) type SaveFn =
        Arc<dyn Fn(String) -> BoxFuture<'static, Result<SaveResponse, AgentError>> + Send + Sync>;

    /// Go `saveOnlyArtifacts` / `savingArtifacts`: `save` delegates to the closure; the rest return
    /// `AgentError::other("not implemented")`.
    pub(crate) struct SaveOnlyArtifacts(pub(crate) SaveFn);

    impl SaveOnlyArtifacts {
        pub(crate) fn version(v: i64) -> Self {
            Self(Arc::new(move |_| {
                async move { Ok(SaveResponse { version: v }) }.boxed()
            }))
        }
    }

    #[async_trait::async_trait]
    impl Artifacts for SaveOnlyArtifacts {
        async fn save(&self, name: &str, _data: Part) -> Result<SaveResponse, AgentError> {
            (self.0)(name.to_string()).await
        }
        async fn list(&self) -> Result<ListResponse, AgentError> {
            Err(AgentError::other("not implemented"))
        }
        async fn load(&self, _name: &str) -> Result<LoadResponse, AgentError> {
            Err(AgentError::other("not implemented"))
        }
        async fn load_version(
            &self,
            _name: &str,
            _version: i64,
        ) -> Result<LoadResponse, AgentError> {
            Err(AgentError::other("not implemented"))
        }
    }

    /// Go `stubSubScheduler`: a non-nil `DynamicSubScheduler` returning empty values.
    pub(crate) struct StubSubScheduler;

    #[async_trait::async_trait]
    impl DynamicSubScheduler for StubSubScheduler {
        async fn run_node(
            &self,
            _node: Arc<dyn Any + Send + Sync>,
            _input: Value,
            _opts: Option<Arc<dyn Any + Send + Sync>>,
        ) -> Result<Value, AgentError> {
            Ok(Value::Null)
        }
        fn parent_path(&self) -> String {
            String::new()
        }
        fn output_for_ancestors(&self) -> Vec<String> {
            Vec::new()
        }
        fn delegated_output(&self) -> Option<Value> {
            None
        }
        fn resolve_by_run_id(
            &self,
            _child_name: &str,
            _custom: &str,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }
    }
}
