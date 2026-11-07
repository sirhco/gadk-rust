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

//! The read-only context view and the shared context state (port of Go
//! `internal/context/readonly_context.go` and the `invocationContext`/`commonContext` fields).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gadk_genai::Content;
use gadk_platform::Platform;
use gadk_session::{
    EventActions, ReadonlyState, Session, SessionError, SessionService, ToolConfirmation,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::context::identity::Identity;
use crate::mode::BoundModes;
use crate::parent_map::ParentMap;
use crate::plugin_manager::PluginManager;
use crate::run_config::RunConfig;
use crate::run_config::run_limits::LlmCallLimiter;
use crate::scheduler::DynamicSubScheduler;
use crate::services::{Artifacts, Memory};

/// Invocation-wide values, fixed when the context is built (Go `invocationContext` fields that no
/// delta changes, plus the runner's `context.Context` values parentmap/runconfig/plugin manager).
/// Every derivation shares this `Arc`, so a `with_delta` context keeps the same `llm_call_limiter`.
pub(crate) struct Shared {
    pub(crate) invocation_id: String,
    pub(crate) artifacts: Option<Arc<dyn Artifacts>>,
    pub(crate) memory: Option<Arc<dyn Memory>>,
    pub(crate) run_config: RunConfig,
    pub(crate) platform: Platform,
    pub(crate) plugin_manager: Option<Arc<PluginManager>>,
    pub(crate) parent_map: Option<Arc<ParentMap>>,
    pub(crate) session_service: Option<Arc<dyn SessionService>>,
    pub(crate) llm_call_limiter: Option<Arc<LlmCallLimiter>>,
    /// The engine's per-invocation compaction runtime (Go `compactionctx.ToContext`). Opaque here:
    /// only the engine downcasts it. Every context derived within one invocation shares it.
    pub(crate) compaction: Option<Arc<dyn Any + Send + Sync>>,
}

/// One snapshot of every context field; deltas clone it (cheap: `Arc`s and short strings).
#[derive(Clone)]
pub(crate) struct ContextInner {
    pub(crate) shared: Arc<Shared>,
    // Invocation level (Go invocationContext; a delta touching any of these forks `ended`).
    pub(crate) session: Arc<dyn Session>,
    pub(crate) agent: Arc<dyn Agent>,
    pub(crate) branch: String,
    pub(crate) isolation_scope: String,
    pub(crate) user_content: Option<Arc<Content>>,
    pub(crate) ended: Arc<AtomicBool>,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) bound_modes: BoundModes,
    /// Go `InvocationContextParams.LiveSessionResumptionHandle`. One slot per invocation, shared by
    /// every derived context (the live reader and the reconnect loop run on different tasks);
    /// `ContextDelta::fresh_invocation` installs a new empty slot.
    pub(crate) live_handle: Arc<parking_lot::Mutex<String>>,
    // Node / tool / callback level (Go commonContext).
    pub(crate) resumed_inputs: Option<Arc<Map<String, Value>>>,
    pub(crate) actions: Option<Arc<parking_lot::Mutex<EventActions>>>,
    pub(crate) function_call_id: String,
    pub(crate) tool_confirmation: Option<ToolConfirmation>,
    pub(crate) path: String,
    pub(crate) run_id: String,
    pub(crate) sub_scheduler: Option<Arc<dyn DynamicSubScheduler>>,
    pub(crate) output_for_ancestors: Option<Vec<String>>,
}

/// Go `agent.ReadonlyContext`: read-only access to the invocation's values.
#[derive(Clone)]
pub struct ReadonlyContext {
    pub(crate) inner: Arc<ContextInner>,
}

impl ReadonlyContext {
    /// The user content that started the invocation.
    pub fn user_content(&self) -> Option<&Content> {
        self.inner.user_content.as_deref()
    }

    /// The invocation id.
    pub fn invocation_id(&self) -> &str {
        &self.inner.shared.invocation_id
    }

    /// The name of the current agent.
    pub fn agent_name(&self) -> String {
        self.inner.agent.name().to_string()
    }

    /// Go `ReadonlyState()`: the session's state, without the actions overlay.
    pub fn readonly_state(&self) -> Arc<dyn ReadonlyState> {
        Arc::new(SessionReadonlyState(self.inner.session.clone()))
    }

    /// The session's user id, read from the session on every call, never cached (Go
    /// `IdentityFromContext`'s "read live" rule).
    pub fn user_id(&self) -> &str {
        self.inner.session.user_id()
    }

    /// The session's app name, read from the session on every call.
    pub fn app_name(&self) -> &str {
        self.inner.session.app_name()
    }

    /// The session id, read from the session on every call.
    pub fn session_id(&self) -> &str {
        self.inner.session.id()
    }

    /// Go `identityOf` (agent/common_context.go:83): the invocation's identity from one
    /// `session` read, then one accessor call per field, so the identity cannot be torn.
    pub fn identity(&self) -> Identity {
        let s = self.inner.session.clone();
        Identity {
            user_id: s.user_id().to_string(),
            app_name: s.app_name().to_string(),
            session_id: s.id().to_string(),
        }
    }

    /// The branch, like `agent_1.agent_2.agent_3`.
    pub fn branch(&self) -> String {
        self.inner.branch.clone()
    }

    /// The invocation's cancellation token (Go's embedded `context.Context` cancellation): the
    /// same token [`InvocationContext::cancellation_token`](crate::InvocationContext::cancellation_token)
    /// returns, so a toolset's `tools` can stop when the invocation is cancelled.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.inner.cancellation_token
    }
}

/// The session's state seen read-only (Go `ReadonlyContext.ReadonlyState`).
pub(crate) struct SessionReadonlyState(pub(crate) Arc<dyn Session>);

impl ReadonlyState for SessionReadonlyState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.0.state().get(key)
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.0.state().all()
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use crate::context::{ContextDelta, InvocationContext};

    /// R18: the read-only view hands out the invocation's own token, on every context shape.
    #[test]
    fn readonly_cancellation_token_is_the_invocations() {
        let token = CancellationToken::new();
        let ic = InvocationContext::builder()
            .cancellation_token(token.clone())
            .build_invocation();
        let ctx = ic.clone().into_context();
        let tool_ctx = ctx.with_delta(ContextDelta {
            function_call_id: Some("fc".into()),
            ..Default::default()
        });
        let views = [
            ("invocation", (*ic).clone()),
            ("context", (**ctx.as_invocation()).clone()),
            ("tool context", (**tool_ctx.as_invocation()).clone()),
        ];
        for (name, ro) in &views {
            assert!(!ro.cancellation_token().is_cancelled(), "{name}");
        }
        token.cancel();
        for (name, ro) in &views {
            assert!(ro.cancellation_token().is_cancelled(), "{name}");
        }
        assert!(ic.cancellation_token().is_cancelled());
    }
}
