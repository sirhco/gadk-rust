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

//! Port of ADK-Go `plugin/agentanalytics/trace_manager.go` without the goroutine attribution of
//! workflow nodes and parallel branches (sub-project 7b-3b): the trace manager places every row
//! of an invocation in a trace — trace and span IDs, parent spans, latencies — and infers from
//! the callbacks it sees which agent activations failed.
//!
//! Split by concern: [`span`] (span records and the places of rows), [`invocation`] (one
//! invocation's spans, its agent tree and the tree queries), [`manager`] (the manager: lookup,
//! eviction, completed runs and their late state, the invocation span and the push / pop of
//! model and tool calls and agent starts), [`settle`] (agent ends, responses, errors, retirement,
//! failures and settling, the end of a run) and [`causes`] (cancellation and failure causes).
//!
//! Go's `context.Context` arguments are [`CallbackCtx`]: the callback's ADK context (`None` for
//! Go's plain contexts in direct trace-manager tests), its cancellation and the ADK span it runs
//! in. Go's goroutine attribution fields exist on [`span::SpanRecord`] and stay zero (7b-3b).
//! Locks are `parking_lot` and never held across an `.await` (no method here is async).

#![allow(dead_code, unused_variables)] // Task 5 implements the bodies; consumers: Tasks 6-9.

pub(crate) mod causes;
pub(crate) mod invocation;
pub(crate) mod manager;
pub(crate) mod settle;
pub(crate) mod span;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_agent::{ActivationId, Agent, InvocationContext, ParentMap};
use gadk_engine::telemetry::d6::SpanIds;
use gadk_session::ReadonlyState;
use tokio_util::sync::CancellationToken;

use crate::error::Error;

// The crate-level names of the trace types (`crate::trace::SpanRef`, …), for Tasks 6-13.
#[allow(unused_imports)]
pub(crate) use invocation::{
    AgentChildren, AgentEdge, InvocationState, InvocationTrace, RecordedError, RootAgentInput,
};
#[allow(unused_imports)]
pub(crate) use manager::{CompletedTrace, EvictedTrace, ManagerState, TraceManager};
#[allow(unused_imports)]
pub(crate) use span::{
    AttemptSite, CancelSource, GoroutineOrigin, Latency, NodeRunKey, RetiredCause, RowSpan,
    SpanKind, SpanRecord, SpanRef, UnclosedAgentSpan,
};

/// Go `maxTrackedInvocations`: the number of invocations whose spans are tracked at once.
pub(crate) const MAX_TRACKED_INVOCATIONS: usize = 4096;
/// Go `invocationTTL`: how long an invocation stays tracked after its last callback.
pub(crate) const INVOCATION_TTL: Duration = Duration::from_secs(3600);
/// Go `sweepInterval`: the number of new invocations between two sweeps for expired ones.
pub(crate) const SWEEP_INTERVAL: usize = 128;

/// Go `workflowBasePathPrefix`: custom metadata key prefix of the base path of each enclosing
/// workflow on a node's events (redefined like Go; gadk-engine's keys are crate-private).
pub(crate) const WORKFLOW_BASE_PATH_PREFIX: &str = "adk.workflow.base_path.";
/// Go `workflowNodeOutcomeKey`.
pub(crate) const WORKFLOW_NODE_OUTCOME_KEY: &str = "adk.workflow.node_outcome";
/// Go `workflowNodeCompletedKey`.
pub(crate) const WORKFLOW_NODE_COMPLETED_KEY: &str = "adk.workflow.node_completed";
/// Go `workflowNodeFailedOutcome`.
pub(crate) const WORKFLOW_NODE_FAILED_OUTCOME: &str = "failed";
/// Go `workflowNodeCancelOutcome`.
pub(crate) const WORKFLOW_NODE_CANCEL_OUTCOME: &str = "cancelled";

/// Go `noFrame` (`^uint64(0)`): no activation ran on the current goroutine since a frame.
pub(crate) const NO_FRAME: u64 = u64::MAX;
/// Go `maxActivationKeyDepth`: kept for parity; gadk's activation id needs no chain walk.
pub(crate) const MAX_ACTIVATION_KEY_DEPTH: usize = 8;

/// Go `error` as the trace manager keeps it (recorded errors, failure causes): shared, compared
/// by kind (`Error::is_context`) or text. Go's `context.Canceled` is
/// `Error::Context(ContextError::Canceled)`.
pub(crate) type TraceError = Arc<Error>;

/// Go `traceManager.now`: the manager's clock (a field, so tests can replace it).
pub(crate) type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Go `traceManager.logf` (`Config.logf`): receives the formatted line.
pub(crate) type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Go `identity`: the invocation fields of a row (read from a callback's context by
/// `plugin::rows::identity_of*`).
#[derive(Clone, Default)]
pub(crate) struct Identity {
    pub(crate) agent_name: String,
    pub(crate) session_id: String,
    pub(crate) invocation_id: String,
    pub(crate) user_id: String,
    pub(crate) app_name: String,
    pub(crate) branch: String,
    pub(crate) path: String,
    /// Go `state session.ReadonlyState` (nil when the context has none).
    pub(crate) state: Option<Arc<dyn ReadonlyState>>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("agent_name", &self.agent_name)
            .field("session_id", &self.session_id)
            .field("invocation_id", &self.invocation_id)
            .field("user_id", &self.user_id)
            .field("app_name", &self.app_name)
            .field("branch", &self.branch)
            .field("path", &self.path)
            .field("state", &self.state.is_some())
            .finish()
    }
}

/// Go's agent interface identity (`map[agent.Agent]…` keys, `a == b`): the address of the
/// agent behind an `Arc<dyn Agent>`. A map keyed by it also holds the `Arc`, so the address
/// stays the agent's for as long as the entry lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AgentKey(usize);

impl AgentKey {
    /// The key of `a` (`Arc::as_ptr`, metadata dropped).
    pub(crate) fn of(a: &Arc<dyn Agent>) -> AgentKey {
        AgentKey(Arc::as_ptr(a).cast::<()>() as usize)
    }
}

/// Go's `context.Context` as the trace manager reads it. Go reads the ADK context's
/// `InvocationContext` (activation, `Ended`, agent, parent map), `ctx.Err` / `context.Cause`,
/// and the OpenTelemetry span the callback runs in.
#[derive(Clone, Default)]
pub(crate) struct CallbackCtx {
    /// The callback's ADK context; `None` for Go's plain `context.Background()` (direct
    /// trace-manager tests), which shows no activation, no `Ended` and no agent.
    pub(crate) adk: Option<gadk_agent::Context>,
    /// The context's cancellation (Go `ctx.Err`): the ADK context's token, or a test's own.
    pub(crate) cancel: Option<CancellationToken>,
    /// Go `context.Cause(ctx)` when `cancel` is cancelled; `None` means Go's
    /// `context.Canceled`. Divergence (ledger 7b-3a): ADK contexts carry no cause in gadk
    /// (`CancellationToken`), so it is always `None` for them; only tests set it (Go
    /// `context.WithCancelCause`).
    pub(crate) cancel_cause: Option<TraceError>,
    /// Go `trace.SpanContextFromContext(ctx)`: the ADK span the callback runs in.
    pub(crate) span: Option<SpanIds>,
}

impl CallbackCtx {
    /// Go `context.Background()`: no ADK context, never cancelled, no span.
    pub(crate) fn background() -> CallbackCtx {
        CallbackCtx::default()
    }

    /// The context of an agent, model or tool callback: `adk` = a clone of `ctx`, `cancel` =
    /// its cancellation token, `span` = `gadk_engine::telemetry::current_span_ids()` (the
    /// nearest enclosing ADK span; R1(d)).
    pub(crate) fn from_context(ctx: &gadk_agent::Context) -> CallbackCtx {
        todo!("Task 5")
    }

    /// The context of an invocation callback (user message, run, event): as
    /// [`from_context`](Self::from_context) over `ctx.clone().into_context()`.
    pub(crate) fn from_invocation(ctx: &InvocationContext) -> CallbackCtx {
        todo!("Task 5")
    }

    /// Go `context.WithCancel(ctx)` in tests: `cancel` = `token`.
    pub(crate) fn with_cancel(mut self, token: CancellationToken) -> CallbackCtx {
        self.cancel = Some(token);
        self
    }

    /// Go `context.WithCancelCause(ctx)` in tests: the cause [`cancel`](Self::cancel) ends
    /// with.
    pub(crate) fn with_cancel_cause(mut self, cause: TraceError) -> CallbackCtx {
        self.cancel_cause = Some(cause);
        self
    }

    /// Go `trace.ContextWithSpanContext(ctx, sc)` in tests.
    pub(crate) fn with_span(mut self, span: SpanIds) -> CallbackCtx {
        self.span = Some(span);
        self
    }

    /// Go `ambientSpanContext(ctx)`: the span, if valid.
    pub(crate) fn ambient_span_context(&self) -> Option<SpanIds> {
        todo!("Task 5")
    }

    /// Go `ambientTraceID(ctx)`: the span's trace ID as 32 lowercase hex digits, or "".
    pub(crate) fn ambient_trace_id(&self) -> String {
        todo!("Task 5")
    }

    /// Go `activationKey(ctx)`: the activation of the ADK context (R1(a)), or `None`.
    pub(crate) fn activation_key(&self) -> Option<ActivationId> {
        todo!("Task 5")
    }

    /// Go `endedFunc(ctx)`: the ADK context whose `ended()` a span record reads later, or
    /// `None` for a plain context.
    pub(crate) fn ended_func(&self) -> Option<gadk_agent::Context> {
        todo!("Task 5")
    }

    /// Go `canceledFunc(ctx)`: what a span record reads later to tell how its context ended,
    /// or `None` when the context cannot be cancelled.
    pub(crate) fn canceled_func(&self) -> Option<CancelSource> {
        todo!("Task 5")
    }

    /// Go `invocationEnded(ctx)`: the ADK context's `ended()`; false for a plain context.
    pub(crate) fn invocation_ended(&self) -> bool {
        todo!("Task 5")
    }

    /// Go `ctx.Err() != nil` / `context.Cause(ctx)` for the run's own end (e.g. `afterRun`):
    /// `None` while live, else Go's `context.Canceled` or the test cause.
    pub(crate) fn err(&self) -> Option<TraceError> {
        todo!("Task 5")
    }

    /// Go `contextParentMap(ctx)`: the parent map the runner attached to the context.
    pub(crate) fn parent_map(&self) -> Option<Arc<ParentMap>> {
        todo!("Task 5")
    }

    /// Go `runningAgentChildren(ctx, name)`: the sub-agents of the context's agent if it is
    /// named `name`, else the zero value.
    pub(crate) fn running_agent_children(&self, name: &str) -> AgentChildren {
        todo!("Task 5")
    }

    /// Go `callKey(ctx, key)`: `key` prefixed with the activation (`"<id>\x00<key>"`), or
    /// `key` itself when the context shows none.
    pub(crate) fn call_key(&self, key: &str) -> String {
        todo!("Task 5")
    }
}

/// Go `llmKey(id)`: `path \x00 agentName \x00 branch`.
pub(crate) fn llm_key(id: &Identity) -> String {
    todo!("Task 5")
}

/// Go `toolKey(id, functionCallID, toolName)`.
pub(crate) fn tool_key(id: &Identity, function_call_id: &str, tool_name: &str) -> String {
    todo!("Task 5")
}

/// Go `toolKeyCall(key)`: what follows the last NUL of `key`.
pub(crate) fn tool_key_call(key: &str) -> &str {
    todo!("Task 5")
}

/// Go `newTraceID()`: 32 random lowercase hex digits (a v4 UUID's bytes).
pub(crate) fn new_trace_id() -> String {
    todo!("Task 5")
}

/// Go `newSpanID()`: 16 random lowercase hex digits (the first 8 bytes of a v4 UUID).
pub(crate) fn new_span_id() -> String {
    todo!("Task 5")
}
