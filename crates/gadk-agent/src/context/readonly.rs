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

//! The read-only context view and the shared context state.

use std::any::Any;
use std::collections::HashMap;
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
use crate::context::activation::ActivationId;
use crate::context::cancel::CancelScope;
use crate::context::identity::Identity;
use crate::context::prompt_use::PromptUse;
use crate::mode::BoundModes;
use crate::parent_map::ParentMap;
use crate::plugin_manager::PluginManager;
use crate::run_config::RunConfig;
use crate::run_config::run_limits::LlmCallLimiter;
use crate::scheduler::DynamicSubScheduler;
use crate::services::{Artifacts, Memory};

/// Invocation-wide values, fixed when the context is built (the invocation fields that no
/// delta changes, plus the runner's parent map, run config and plugin manager).
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
    /// The engine's per-invocation compaction runtime. Opaque here:
    /// only the engine downcasts it. Every context derived within one invocation shares it.
    pub(crate) compaction: Option<Arc<dyn Any + Send + Sync>>,
    /// The invocation's teardown registry ([`crate::teardown::Teardowns`]); the runner installs
    /// one per invocation.
    pub(crate) teardowns: Option<Arc<crate::teardown::Teardowns>>,
    /// The prompt version each agent's instruction was last rendered from, by agent name
    /// ([`ReadonlyContext::record_prompt`]). Created empty with the invocation; every derived
    /// context (deltas included) shares it.
    pub(crate) prompts: parking_lot::Mutex<HashMap<String, PromptUse>>,
}

/// One snapshot of every context field; deltas clone it (cheap: `Arc`s and short strings).
#[derive(Clone)]
pub(crate) struct ContextInner {
    pub(crate) shared: Arc<Shared>,
    // Invocation level (a delta touching any of these forks `ended`).
    pub(crate) session: Arc<dyn Session>,
    pub(crate) agent: Arc<dyn Agent>,
    pub(crate) branch: String,
    pub(crate) isolation_scope: String,
    pub(crate) user_content: Option<Arc<Content>>,
    pub(crate) ended: Arc<AtomicBool>,
    /// The identity of `ended`: replaced together with it, never without it.
    pub(crate) activation: ActivationId,
    /// The context's cancel scope (cancellation and cause); its token is
    /// the context's cancellation token.
    pub(crate) cancel_scope: Arc<CancelScope>,
    pub(crate) bound_modes: BoundModes,
    /// The live session resumption handle. One slot per invocation, shared by
    /// every derived context (the live reader and the reconnect loop run on different tasks);
    /// `ContextDelta::fresh_invocation` installs a new empty slot.
    pub(crate) live_handle: Arc<parking_lot::Mutex<String>>,
    // Node / tool / callback level.
    pub(crate) resumed_inputs: Option<Arc<Map<String, Value>>>,
    pub(crate) actions: Option<Arc<parking_lot::Mutex<EventActions>>>,
    pub(crate) function_call_id: String,
    pub(crate) tool_confirmation: Option<ToolConfirmation>,
    pub(crate) path: String,
    pub(crate) run_id: String,
    pub(crate) sub_scheduler: Option<Arc<dyn DynamicSubScheduler>>,
    pub(crate) output_for_ancestors: Option<Vec<String>>,
    /// The walk past a fresh invocation context: the path of the context a
    /// `fresh_invocation` / `fresh_end_flag` delta forked from (its own path, else its enclosing
    /// path); every other delta copies it unchanged.
    pub(crate) enclosing_path: String,
    /// The plugin manager a plugin-less
    /// runner started with this context keeps (`None`: no mark, or a cleared one). Every
    /// derivation copies it.
    pub(crate) inherited_plugins: Option<Arc<PluginManager>>,
    /// The graph-resume-done mark (`InvocationContext::with_graph_resume_done`): resuming a
    /// nested graph workflow is done for this context and every context derived from it, so
    /// composite agents run all their sub-agents. Every derivation copies it.
    pub(crate) graph_resume_done: bool,
}

/// Read-only access to the invocation's values.
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

    /// The session's state, without the actions overlay.
    pub fn readonly_state(&self) -> Arc<dyn ReadonlyState> {
        Arc::new(SessionReadonlyState(self.inner.session.clone()))
    }

    /// The session's user id, read from the session on every call, never cached.
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

    /// The invocation's identity from one
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

    /// The invocation's cancellation token: the
    /// same token [`InvocationContext::cancellation_token`](crate::InvocationContext::cancellation_token)
    /// returns, so a toolset's `tools` can stop when the invocation is cancelled.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.inner.cancel_scope.token
    }

    /// Records `used` as the prompt version this context's agent rendered its instruction from,
    /// replacing an earlier record of the same agent. Every context of the invocation (including
    /// `fresh_invocation` deltas such as parallel branches) sees it; each runner invocation starts
    /// with none. Records are keyed by agent name, so two agents with the same name in one
    /// invocation share one record.
    pub fn record_prompt(&self, used: PromptUse) {
        self.inner
            .shared
            .prompts
            .lock()
            .insert(self.agent_name(), used);
    }

    /// Removes the prompt version recorded for this context's agent.
    pub fn clear_recorded_prompt(&self) {
        self.inner.shared.prompts.lock().remove(&self.agent_name());
    }

    /// The prompt version recorded for this context's agent, if any.
    pub fn recorded_prompt(&self) -> Option<PromptUse> {
        self.inner
            .shared
            .prompts
            .lock()
            .get(&self.agent_name())
            .cloned()
    }
}

/// The session's state seen read-only.
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

    use crate::context::builder::test_support::named_agent;
    use crate::context::prompt_use::PromptUse;
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

    /// R2: records are per agent name, shared by every context of the invocation (fresh
    /// invocation deltas and tool contexts included), replaced by a later record and removed by
    /// a clear.
    #[test]
    fn recorded_prompts_are_per_agent_and_shared_by_the_invocation() {
        let inv = InvocationContext::builder()
            .agent(named_agent("a"))
            .build_invocation();
        assert_eq!(inv.recorded_prompt(), None);
        inv.record_prompt(PromptUse::new("triage", 2).with_label("production"));

        let branch_b = inv.with_delta(ContextDelta {
            fresh_invocation: true,
            agent: Some(named_agent("b")),
            ..Default::default()
        });
        assert_eq!(branch_b.recorded_prompt(), None, "b has no record");
        branch_b.record_prompt(PromptUse::new("summary", 1));

        let a_again = branch_b.with_delta(ContextDelta {
            agent: Some(named_agent("a")),
            ..Default::default()
        });
        assert_eq!(
            a_again.recorded_prompt(),
            Some(PromptUse::new("triage", 2).with_label("production")),
            "a fresh-invocation delta sees the invocation's records"
        );
        let tool_ctx = inv.clone().into_context().with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            ..Default::default()
        });
        assert_eq!(
            tool_ctx.recorded_prompt(),
            Some(PromptUse::new("triage", 2).with_label("production"))
        );

        tool_ctx.record_prompt(PromptUse::new("triage", 3));
        assert_eq!(
            inv.recorded_prompt(),
            Some(PromptUse::new("triage", 3)),
            "replaced"
        );

        a_again.clear_recorded_prompt();
        assert_eq!(inv.recorded_prompt(), None, "cleared");
        assert_eq!(
            branch_b.recorded_prompt(),
            Some(PromptUse::new("summary", 1)),
            "b's record is untouched"
        );
    }

    /// R2: each built invocation starts with an empty slot.
    #[test]
    fn each_built_invocation_starts_empty() {
        let first = InvocationContext::builder()
            .agent(named_agent("a"))
            .build_invocation();
        first.record_prompt(PromptUse::new("triage", 1));
        let second = InvocationContext::builder()
            .agent(named_agent("a"))
            .build_invocation();
        assert_eq!(second.recorded_prompt(), None);
        assert_eq!(first.recorded_prompt(), Some(PromptUse::new("triage", 1)));
    }
}
