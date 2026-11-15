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

//! The invocation context (port of Go `agent.InvocationContext`, `invocationContext` in
//! `agent/agent.go`, `internal/context/invocation_context.go`, and `trackedArtifacts` in
//! `agent/common_context.go`).

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gadk_artifact::{ListResponse, LoadResponse, SaveResponse};
use gadk_genai::Part;
use gadk_platform::Platform;
use gadk_session::{EventActions, Session, SessionService};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::context::activation::ActivationId;
use crate::context::builder::ContextBuilder;
use crate::context::cancel::CancelCause;
use crate::context::context::Context;
use crate::context::delta::ContextDelta;
use crate::context::readonly::{ContextInner, ReadonlyContext};
use crate::error::AgentError;
use crate::mode::BoundModes;
use crate::parent_map::ParentMap;
use crate::plugin_manager::PluginManager;
use crate::run_config::RunConfig;
use crate::run_config::run_limits::LlmCallLimiter;
use crate::services::{ArtifactBacking, Artifacts, Memory};

/// Go `agent.InvocationContext`: the context of one agent invocation.
#[derive(Clone)]
pub struct InvocationContext {
    pub(crate) ro: ReadonlyContext,
}

impl Deref for InvocationContext {
    type Target = ReadonlyContext;

    fn deref(&self) -> &ReadonlyContext {
        &self.ro
    }
}

impl InvocationContext {
    /// A [`ContextBuilder`] with documented defaults.
    pub fn builder() -> ContextBuilder {
        ContextBuilder::new()
    }

    pub(crate) fn from_inner(inner: ContextInner) -> Self {
        Self {
            ro: ReadonlyContext {
                inner: Arc::new(inner),
            },
        }
    }

    pub(crate) fn inner(&self) -> &ContextInner {
        &self.ro.inner
    }

    /// The current agent.
    pub fn agent(&self) -> Arc<dyn Agent> {
        self.inner().agent.clone()
    }

    /// The artifacts of the session: tracked (records saves into the actions' `artifact_delta`) when
    /// the context has actions, as Go `NewCallbackContextWithArtifactTracking`/`NewToolContext` do;
    /// the raw handle otherwise; `None` without an artifact service.
    pub fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        let raw = self.inner().shared.artifacts.clone()?;
        Some(match &self.inner().actions {
            Some(a) => Arc::new(TrackedArtifacts::new(raw, a.clone())),
            None => raw,
        })
    }

    /// The memory of the user, if a memory service is configured.
    pub fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.inner().shared.memory.clone()
    }

    /// The session.
    pub fn session(&self) -> Arc<dyn Session> {
        self.inner().session.clone()
    }

    /// The isolation scope; events with a different non-empty scope are not visible.
    pub fn isolation_scope(&self) -> String {
        self.inner().isolation_scope.clone()
    }

    /// The run configuration.
    pub fn run_config(&self) -> &RunConfig {
        &self.inner().shared.run_config
    }

    /// Ends the invocation: the agent stops at its next check.
    pub fn end_invocation(&self) {
        self.inner().ended.store(true, Ordering::SeqCst)
    }

    /// Whether the invocation has ended.
    pub fn ended(&self) -> bool {
        self.inner().ended.load(Ordering::SeqCst)
    }

    /// The identity of this context's end flag (see [`ActivationId`]): equal for every callback
    /// and tool context of one agent run, distinct across runs and forks, never reused.
    pub fn activation_id(&self) -> ActivationId {
        self.inner().activation
    }

    /// Go `commonContext.ResumedInput`: this context's resume input for `interrupt_id`.
    pub fn resumed_input(&self, interrupt_id: &str) -> Option<Value> {
        self.inner()
            .resumed_inputs
            .as_ref()?
            .get(interrupt_id)
            .cloned()
    }

    /// The cancellation token (Go's embedded `context.Context` cancellation).
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.inner().cancel_scope.token
    }

    /// Go `cancel(cause)` of a `context.WithCancelCause` context: cancels this context's own
    /// cancel scope with `cause`, unless it is already cancelled (first cause wins). Contexts
    /// sharing the scope (every delta but a raw `cancellation_token`, and contexts derived by
    /// [`Context::with_agent_cancel`](crate::Context::with_agent_cancel) from it) are cancelled
    /// with it.
    pub fn cancel_with_cause(&self, cause: CancelCause) {
        self.inner().cancel_scope.cancel_with_cause(cause);
    }

    /// Go `context.Cause`: `None` while the cancellation token is live; else the cause recorded
    /// by the nearest cancelled scope walking outward from this context's own (a scope cancelled
    /// only by its parent defers to it), else `Canceled` (e.g. a raw `token.cancel()`).
    pub fn cause(&self) -> Option<CancelCause> {
        self.inner().cancel_scope.cause()
    }

    /// The platform (time, UUIDs, task runner).
    pub fn platform(&self) -> &Platform {
        &self.inner().shared.platform
    }

    /// The plugin manager, if plugins are configured.
    pub fn plugin_manager(&self) -> Option<Arc<PluginManager>> {
        self.inner().shared.plugin_manager.clone()
    }

    /// Go `plugininternal.WithInheritedPlugins` (#1777): a derived context marked so that the
    /// next runner started with it (`Runner::run_with_context`) keeps this context's plugin
    /// manager ([`InvocationContext::plugin_manager`], Go `FromContext`) when that runner has no
    /// plugins of its own. Only the agent tool sets this mark, for the sub-runner it starts; any
    /// other runner uses only its own plugins. The receiver is unchanged.
    pub fn with_inherited_plugins(&self) -> InvocationContext {
        let mut n = self.inner().clone();
        n.inherited_plugins = self.plugin_manager();
        Self::from_inner(n)
    }

    /// Go `plugininternal.TakeInheritedPlugins` (#1777): the plugin manager marked by
    /// [`InvocationContext::with_inherited_plugins`] (`None` without a mark) and a derived context
    /// with the mark cleared, so that the mark applies to one runner only and not to runners
    /// started further down.
    pub fn take_inherited_plugins(&self) -> (InvocationContext, Option<Arc<PluginManager>>) {
        let Some(pm) = self.inner().inherited_plugins.clone() else {
            return (self.clone(), None);
        };
        let mut n = self.inner().clone();
        n.inherited_plugins = None;
        (Self::from_inner(n), Some(pm))
    }

    /// The agent tree's parent map, if set.
    pub fn parent_map(&self) -> Option<Arc<ParentMap>> {
        self.inner().shared.parent_map.clone()
    }

    /// The session service, if set.
    pub fn session_service(&self) -> Option<Arc<dyn SessionService>> {
        self.inner().shared.session_service.clone()
    }

    /// The invocation's LLM-call limiter, shared by every agent in it.
    #[doc(hidden)]
    pub fn llm_call_limiter(&self) -> Option<Arc<LlmCallLimiter>> {
        self.inner().shared.llm_call_limiter.clone()
    }

    /// The engine's per-invocation compaction runtime, if compaction is configured
    /// (engine-internal; Go `compactionctx.FromContext`).
    #[doc(hidden)]
    pub fn compaction_runtime(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.inner().shared.compaction.clone()
    }

    /// The bound agent modes.
    pub fn bound_modes(&self) -> &BoundModes {
        &self.inner().bound_modes
    }

    /// Go `InvocationContext.LiveSessionResumptionHandle`: the active live session's resumption
    /// handle, used to resume a bidirectional streaming session after a reconnect.
    pub fn live_session_resumption_handle(&self) -> String {
        self.inner().live_handle.lock().clone()
    }

    /// Go `InvocationContext.SetLiveSessionResumptionHandle`: stores the latest handle received
    /// from the model, visible to every context of this invocation.
    pub fn set_live_session_resumption_handle(&self, handle: &str) {
        *self.inner().live_handle.lock() = handle.to_owned();
    }

    /// A derived context with `d` applied; the receiver is unchanged.
    pub fn with_delta(&self, d: ContextDelta) -> InvocationContext {
        Self::from_inner(self.inner().apply(d))
    }

    /// Go `agent.Promote`: the same fields, seen as a [`Context`].
    pub fn into_context(self) -> Context {
        Context { ic: self }
    }

    /// The read-only view.
    pub fn as_readonly(&self) -> &ReadonlyContext {
        &self.ro
    }
}

/// Go `trackedArtifacts`: records each successful save, keeping the highest version per name.
pub(crate) struct TrackedArtifacts {
    inner: Arc<dyn Artifacts>,
    actions: Arc<parking_lot::Mutex<EventActions>>,
}

impl TrackedArtifacts {
    /// Go `newTrackedArtifacts`.
    pub(crate) fn new(
        inner: Arc<dyn Artifacts>,
        actions: Arc<parking_lot::Mutex<EventActions>>,
    ) -> Self {
        Self { inner, actions }
    }
}

#[async_trait::async_trait]
impl Artifacts for TrackedArtifacts {
    async fn save(&self, name: &str, data: Part) -> Result<SaveResponse, AgentError> {
        let resp = self.inner.save(name, data).await?;
        let mut a = self.actions.lock();
        let delta = a.artifact_delta.get_or_insert_with(BTreeMap::new);
        if delta.get(name).is_none_or(|v| resp.version > *v) {
            delta.insert(name.to_string(), resp.version);
        }
        Ok(resp)
    }

    async fn list(&self) -> Result<ListResponse, AgentError> {
        self.inner.list().await
    }

    async fn load(&self, name: &str) -> Result<LoadResponse, AgentError> {
        self.inner.load(name).await
    }

    async fn load_version(&self, name: &str, version: i64) -> Result<LoadResponse, AgentError> {
        self.inner.load_version(name, version).await
    }

    fn backing_service(&self) -> Option<ArtifactBacking> {
        self.inner.backing_service()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;

    use crate::context::delta::ContextDelta;

    use super::*;

    fn slot(ctx: &InvocationContext) -> Option<Arc<dyn Any + Send + Sync>> {
        ctx.compaction_runtime()
    }

    #[test]
    fn compaction_runtime_slot_round_trips_and_is_shared() {
        let rt: Arc<dyn Any + Send + Sync> = Arc::new(7u32);
        let ctx = InvocationContext::builder()
            .compaction_runtime(rt.clone())
            .build_invocation();
        let got = slot(&ctx).expect("slot set");
        assert!(Arc::ptr_eq(&got, &rt));
        assert_eq!(got.downcast_ref::<u32>(), Some(&7));

        // Every derivation inside the invocation sees the same Arc.
        let derived = [
            ctx.with_delta(ContextDelta {
                branch: Some("a.b".into()),
                ..Default::default()
            }),
            ctx.with_delta(ContextDelta {
                fresh_invocation: true,
                ..Default::default()
            }),
            ctx.clone()
                .into_context()
                .with_delta(ContextDelta {
                    function_call_id: Some("fc".into()),
                    ..Default::default()
                })
                .as_invocation()
                .clone(),
            ctx.clone()
                .into_context()
                .with_agent_cancel()
                .0
                .as_invocation()
                .clone(),
        ];
        for d in &derived {
            assert!(Arc::ptr_eq(&slot(d).expect("derived slot"), &rt));
        }
    }

    #[test]
    fn compaction_runtime_slot_defaults_to_none() {
        assert!(
            InvocationContext::builder()
                .build_invocation()
                .compaction_runtime()
                .is_none()
        );
    }
}
