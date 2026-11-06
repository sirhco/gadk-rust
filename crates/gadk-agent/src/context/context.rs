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

//! The full agent context used by callbacks, tools and workflow nodes (port of Go `agent.Context`,
//! `commonContext` in `agent/common_context.go`, and `internal/context/callback_context.go`).

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use gadk_memory::SearchResponse;
use gadk_session::{EventActions, Session, SessionError, State, ToolConfirmation};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::context::builder::ContextBuilder;
use crate::context::delta::ContextDelta;
use crate::context::invocation::InvocationContext;
use crate::error::{AgentError, ToolError};
use crate::scheduler::DynamicSubScheduler;

/// Go `agent.Context`: an invocation context plus the node, tool and callback fields.
#[derive(Clone)]
pub struct Context {
    pub(crate) ic: InvocationContext,
}

impl Deref for Context {
    type Target = InvocationContext;

    fn deref(&self) -> &InvocationContext {
        &self.ic
    }
}

impl Context {
    /// A [`ContextBuilder`] with documented defaults.
    pub fn builder() -> ContextBuilder {
        ContextBuilder::new()
    }

    /// Go `callbackContextState`: `get` reads the actions' `state_delta` first, then the session;
    /// `set` writes the `state_delta` (when actions exist) and the session state.
    pub fn state(&self) -> Arc<dyn State> {
        Arc::new(CallbackState {
            actions: self.inner().actions.clone(),
            session: self.inner().session.clone(),
        })
    }

    /// The id of the function call this tool context runs; empty outside a tool call.
    pub fn function_call_id(&self) -> String {
        self.inner().function_call_id.clone()
    }

    /// A snapshot of the context's actions (`EventActions::default()` when it has none).
    pub fn actions(&self) -> EventActions {
        self.inner()
            .actions
            .as_ref()
            .map(|a| a.lock().clone())
            .unwrap_or_default()
    }

    /// Runs `f` on the context's actions under their lock; a no-op on a context without actions.
    pub fn update_actions(&self, f: impl FnOnce(&mut EventActions)) {
        match &self.inner().actions {
            Some(a) => f(&mut a.lock()),
            None => tracing::debug!("update_actions on a context without actions is ignored"),
        }
    }

    /// Go `commonContext.SearchMemory`.
    pub async fn search_memory(&self, query: &str) -> Result<SearchResponse, AgentError> {
        let Some(m) = self.memory() else {
            return Err(AgentError::other("memory service is not set"));
        };
        m.search_memory(query).await
    }

    /// The Human-in-the-Loop confirmation of the current tool call, if any.
    pub fn tool_confirmation(&self) -> Option<ToolConfirmation> {
        self.inner().tool_confirmation.clone()
    }

    /// Go `commonContext.RequestConfirmation`: records a pending confirmation for the current tool
    /// call and sets `skip_summarization` so the agent loop halts until the user responds.
    pub fn request_confirmation(
        &self,
        hint: &str,
        payload: Option<Value>,
    ) -> Result<(), ToolError> {
        let id = self.inner().function_call_id.clone();
        if id.is_empty() {
            return Err(ToolError::InvalidArgument(
                "error function call id not set when requesting confirmation for tool".into(),
            ));
        }
        self.update_actions(|a| {
            a.requested_tool_confirmations.insert(
                id,
                ToolConfirmation {
                    hint: hint.to_string(),
                    confirmed: false,
                    payload,
                },
            );
            // SkipSummarization stops the agent loop after this tool call (Go comment).
            a.skip_summarization = true;
        });
        Ok(())
    }

    /// The workflow node path.
    pub fn path(&self) -> String {
        self.inner().path.clone()
    }

    /// The workflow node run id.
    pub fn run_id(&self) -> String {
        self.inner().run_id.clone()
    }

    /// The dynamic sub-scheduler of the current workflow node, if any.
    pub fn sub_scheduler(&self) -> Option<Arc<dyn DynamicSubScheduler>> {
        self.inner().sub_scheduler.clone()
    }

    /// The node paths whose output this context also reports.
    pub fn output_for_ancestors(&self) -> Vec<String> {
        self.inner()
            .output_for_ancestors
            .clone()
            .unwrap_or_default()
    }

    /// A derived context with `d` applied; the receiver is unchanged.
    pub fn with_delta(&self, d: ContextDelta) -> Context {
        Context {
            ic: self.ic.with_delta(d),
        }
    }

    /// The invocation-context view.
    pub fn as_invocation(&self) -> &InvocationContext {
        &self.ic
    }

    /// Go `WithAgentCancel`: the same fields (the end flag shared) with a child cancellation token.
    pub fn with_agent_cancel(&self) -> (Context, CancelGuard) {
        let token = self.cancellation_token().child_token();
        let mut inner = self.inner().clone();
        inner.cancellation_token = token.clone();
        (
            InvocationContext::from_inner(inner).into_context(),
            CancelGuard { token, timer: None },
        )
    }

    /// Go `WithAgentTimeout`: as [`Context::with_agent_cancel`], plus a timer task that cancels the
    /// child after `timeout` (immediately for a zero duration). Outside a Tokio runtime the timeout is
    /// not enforced (a `tracing::warn!` says so).
    pub fn with_agent_timeout(&self, timeout: Duration) -> (Context, CancelGuard) {
        let (ctx, mut guard) = self.with_agent_cancel();
        if timeout.is_zero() {
            guard.token.cancel();
            return (ctx, guard);
        }
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                let t = guard.token.clone();
                guard.timer = Some(h.spawn(async move {
                    tokio::time::sleep(timeout).await;
                    t.cancel();
                }));
            }
            Err(_) => tracing::warn!(
                "with_agent_timeout outside a Tokio runtime: the timeout is not enforced"
            ),
        }
        (ctx, guard)
    }
}

/// Cancels its child token when dropped or when [`CancelGuard::cancel`] is called (Go
/// `context.CancelFunc`); keep it alive while the derived context is used.
pub struct CancelGuard {
    token: CancellationToken,
    timer: Option<tokio::task::JoinHandle<()>>,
}

impl CancelGuard {
    /// Cancels the derived context's token.
    pub fn cancel(&self) {
        self.token.cancel()
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.token.cancel();
        if let Some(t) = self.timer.take() {
            t.abort();
        }
    }
}

impl std::fmt::Debug for CancelGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelGuard")
            .field("cancelled", &self.token.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Go `callbackContextState`.
struct CallbackState {
    actions: Option<Arc<parking_lot::Mutex<EventActions>>>,
    session: Arc<dyn Session>,
}

impl State for CallbackState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        let pending = self.actions.as_ref().and_then(|a| {
            a.lock()
                .state_delta
                .as_ref()
                .and_then(|d| d.get(key).cloned())
        });
        if let Some(v) = pending {
            return Ok(v);
        }
        self.session.state().get(key)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        if let Some(a) = &self.actions
            && let Some(d) = a.lock().state_delta.as_mut()
        {
            d.insert(key.to_string(), value.clone());
        }
        self.session.state().set(key, value)
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.session.state().all()
    }
}
