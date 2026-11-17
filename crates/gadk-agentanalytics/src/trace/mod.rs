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

//! The trace manager places every row of an invocation in a trace — trace and span IDs, parent
//! spans, latencies — and infers from the callbacks it sees which agent activations failed.
//!
//! Split by concern: [`span`] (span records and the places of rows), [`invocation`] (one
//! invocation's spans, its agent tree and the tree queries), [`manager`] (the manager: lookup,
//! eviction, completed runs and their late state, the invocation span and the push / pop of
//! model and tool calls and agent starts), [`settle`] (agent ends, responses, errors, retirement,
//! failures and settling, the end of a run) and [`causes`] (cancellation and failure causes).
//!
//! Callback contexts are [`CallbackCtx`]: the callback's ADK context (`None` for a plain context
//! in direct trace-manager tests), its cancellation and the ADK span it runs in. The callback's
//! activation attribution reads its fiber (`gadk_agent::fiber`; see [`settle`]). Locks are
//! `parking_lot` and never held across an `.await` (no method here is async).

pub(crate) mod causes;
pub(crate) mod invocation;
pub(crate) mod manager;
pub(crate) mod settle;
pub(crate) mod span;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_agent::{ActivationId, Agent, InvocationContext, ParentMap};
use gadk_engine::llm::llm_agent_config;
use gadk_engine::telemetry::d6::SpanIds;
use gadk_session::ReadonlyState;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::context::ContextError;
use crate::error::Error;

// The crate-level names of the trace types (`crate::trace::SpanRef`, …); the second group only
// the tests name.
pub(crate) use invocation::AgentChildren;
pub(crate) use manager::TraceManager;
pub(crate) use span::{CancelSource, Latency, RowSpan, SpanRef, UnclosedAgentSpan};
#[cfg(test)]
pub(crate) use {
    invocation::{InvocationState, InvocationTrace, RecordedError},
    span::{RetiredCause, SpanKind, SpanRecord},
};

/// The number of invocations whose spans are tracked at once.
pub(crate) const MAX_TRACKED_INVOCATIONS: usize = 4096;
/// How long an invocation stays tracked after its last callback.
pub(crate) const INVOCATION_TTL: Duration = Duration::from_secs(3600);
/// The number of new invocations between two sweeps for expired ones.
pub(crate) const SWEEP_INTERVAL: usize = 128;

/// Custom metadata key prefix of the base path of each enclosing workflow on a node's events
/// (redefined here; gadk-engine's keys are crate-private).
pub(crate) const WORKFLOW_BASE_PATH_PREFIX: &str = "adk.workflow.base_path.";
/// Custom metadata key of a workflow node's outcome.
pub(crate) const WORKFLOW_NODE_OUTCOME_KEY: &str = "adk.workflow.node_outcome";
/// Custom metadata key marking a workflow node as completed.
pub(crate) const WORKFLOW_NODE_COMPLETED_KEY: &str = "adk.workflow.node_completed";
/// The node outcome of a failed workflow node.
pub(crate) const WORKFLOW_NODE_FAILED_OUTCOME: &str = "failed";
/// The node outcome of a cancelled workflow node.
pub(crate) const WORKFLOW_NODE_CANCEL_OUTCOME: &str = "cancelled";

/// No activation ran on the current fiber since a frame (`u64::MAX`).
pub(crate) const NO_FRAME: u64 = u64::MAX;

/// An error as the trace manager keeps it (recorded errors, failure causes): shared, compared
/// by kind (`Error::is_context`) or text. A canceled context is
/// `Error::Context(ContextError::Canceled)`.
pub(crate) type TraceError = Arc<Error>;

/// The manager's clock (a field, so tests can replace it).
pub(crate) type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// The manager's log sink (`Config.logf`): receives the formatted line.
pub(crate) type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// The invocation fields of a row (read from a callback's context by
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
    /// The session state (`None` when the context has none).
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

/// Identity of an agent: the address of the agent behind an `Arc<dyn Agent>`. A map keyed by it
/// also holds the `Arc`, so the address stays the agent's for as long as the entry lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AgentKey(usize);

impl AgentKey {
    /// The key of `a` (`Arc::as_ptr`, metadata dropped).
    pub(crate) fn of(a: &Arc<dyn Agent>) -> AgentKey {
        AgentKey(Arc::as_ptr(a).cast::<()>() as usize)
    }
}

/// A callback's context as the trace manager reads it: the ADK context's `InvocationContext`
/// (activation, `Ended`, agent, parent map), its cancellation and cause, and the OpenTelemetry
/// span the callback runs in.
#[derive(Clone, Default)]
pub(crate) struct CallbackCtx {
    /// The callback's ADK context; `None` for a plain background context (direct trace-manager
    /// tests), which shows no activation, no `Ended` and no agent.
    pub(crate) adk: Option<gadk_agent::Context>,
    /// The context's cancellation: the ADK context's token and cause (read when asked), or a
    /// test's token and cause.
    pub(crate) cancel: Option<CancelSource>,
    /// The ADK span the callback runs in.
    pub(crate) span: Option<SpanIds>,
}

impl CallbackCtx {
    /// A context with no ADK context, never cancelled, no span.
    #[cfg(test)]
    pub(crate) fn background() -> CallbackCtx {
        CallbackCtx::default()
    }

    /// The context of an agent, model or tool callback: `adk` = a clone of `ctx`, `cancel` =
    /// its cancellation token, `span` = `gadk_engine::telemetry::current_span_ids()` (the
    /// nearest enclosing ADK span).
    ///
    /// Only spans gadk-telemetry's bridge marks as ADK spans are read, and the bridge starts a
    /// new trace under a non-ADK span, so a caller's own span is never inherited: rows of a run
    /// started under a caller's span do not join that trace (`attributes.otel`, the adopted
    /// trace ID).
    pub(crate) fn from_context(ctx: &gadk_agent::Context) -> CallbackCtx {
        CallbackCtx {
            adk: Some(ctx.clone()),
            cancel: Some(CancelSource::of_context(ctx)),
            span: gadk_engine::telemetry::current_span_ids(),
        }
    }

    /// The context of an invocation callback (user message, run, event): as
    /// [`from_context`](Self::from_context) over `ctx.clone().into_context()`.
    pub(crate) fn from_invocation(ctx: &InvocationContext) -> CallbackCtx {
        CallbackCtx::from_context(&ctx.clone().into_context())
    }

    /// For tests: `cancel` = `token` (cause `Canceled`).
    #[cfg(test)]
    pub(crate) fn with_cancel(mut self, token: CancellationToken) -> CallbackCtx {
        self.cancel = Some(CancelSource::of_token(token, None));
        self
    }

    /// For tests: the cause the token set by [`with_cancel`](Self::with_cancel) ends with.
    #[cfg(test)]
    pub(crate) fn with_cancel_cause(mut self, cause: TraceError) -> CallbackCtx {
        if let Some(c) = self.cancel.as_mut() {
            c.cause = Some(cause);
            c.adk = None;
        }
        self
    }

    /// For tests: the span the callback runs in.
    #[cfg(test)]
    pub(crate) fn with_span(mut self, span: SpanIds) -> CallbackCtx {
        self.span = Some(span);
        self
    }

    /// The span, if valid.
    pub(crate) fn ambient_span_context(&self) -> Option<SpanIds> {
        self.span.filter(SpanIds::is_valid)
    }

    /// The span's trace ID as 32 lowercase hex digits, or "".
    pub(crate) fn ambient_trace_id(&self) -> String {
        self.ambient_span_context()
            .map(|sc| hex(&sc.trace_id))
            .unwrap_or_default()
    }

    /// The activation of the ADK context, or `None`.
    pub(crate) fn activation_key(&self) -> Option<ActivationId> {
        self.adk.as_ref().map(|c| c.activation_id())
    }

    /// The ADK context whose `ended()` a span record reads later, or `None` for a plain
    /// context.
    pub(crate) fn ended_func(&self) -> Option<gadk_agent::Context> {
        self.adk.clone()
    }

    /// What a span record reads later to tell how its context ended, or `None` when the context
    /// cannot be cancelled (a context that never ends reports "not cancelled" forever, as
    /// `None` does).
    pub(crate) fn canceled_func(&self) -> Option<CancelSource> {
        self.cancel.clone()
    }

    /// The ADK context's `ended()`; false for a plain context.
    pub(crate) fn invocation_ended(&self) -> bool {
        self.adk.as_ref().is_some_and(|c| c.ended())
    }

    /// The run's own end (e.g. in the after-run callback): `None` while live, else the cause
    /// read now (the ADK context's `cause()` or the test cause; `Canceled` without one).
    pub(crate) fn err(&self) -> Option<TraceError> {
        self.cancel.as_ref()?.cause()
    }

    /// The parent map the runner attached to the context.
    pub(crate) fn parent_map(&self) -> Option<Arc<ParentMap>> {
        self.adk.as_ref().and_then(|c| c.parent_map())
    }

    /// The sub-agents of the context's agent if it is named `name`, else the default value.
    /// `name()` and `sub_agents()` are agent code, so no lock may be held by the caller. A
    /// panicking agent yields the default value, and a panicking agent type reads as "".
    pub(crate) fn running_agent_children(&self, name: &str) -> AgentChildren {
        let Some(adk) = self.adk.as_ref() else {
            return AgentChildren::default();
        };
        if name.is_empty() {
            return AgentChildren::default();
        }
        catch_unwind(AssertUnwindSafe(|| {
            let a = adk.agent();
            if a.name() != name {
                return AgentChildren::default();
            }
            let cfg = llm_agent_config(&*a);
            let ag_type = catch_unwind(AssertUnwindSafe(|| invocation::agent_type_of(&*a)))
                .unwrap_or_default();
            AgentChildren {
                parent: name.to_string(),
                non_llm: cfg.is_none(),
                propagates_child_error: invocation::is_built_in_workflow_agent(&*a),
                is_parallel: ag_type == "ParallelAgent",
                is_workflow: ag_type == "WorkflowAgent",
                task_mode: cfg.is_some_and(|c| c.mode == gadk_agent::Mode::Task),
                names: a
                    .sub_agents()
                    .iter()
                    .map(|sub| sub.name().to_string())
                    .filter(|n| !n.is_empty())
                    .collect(),
            }
        }))
        .unwrap_or_default()
    }

    /// `key` prefixed with the activation (`"<id>\x00<key>"`), or `key` itself when the context
    /// shows none.
    pub(crate) fn call_key(&self, key: &str) -> String {
        match self.activation_key() {
            Some(act) => format!("{}\x00{key}", act.get()),
            None => key.to_string(),
        }
    }
}

/// The key of an agent's model call: `path \x00 agent_name \x00 branch`.
pub(crate) fn llm_key(id: &Identity) -> String {
    format!("{}\x00{}\x00{}", id.path, id.agent_name, id.branch)
}

/// The key of a tool call: the function call ID scoped to the activation
/// (`path \x00 agent_name \x00 branch \x00 call`), or the tool name without an ID.
pub(crate) fn tool_key(id: &Identity, function_call_id: &str, tool_name: &str) -> String {
    let call = if function_call_id.is_empty() {
        tool_name
    } else {
        function_call_id
    };
    format!(
        "{}\x00{}\x00{}\x00{call}",
        id.path, id.agent_name, id.branch
    )
}

/// What follows the last NUL of a tool-call `key`.
pub(crate) fn tool_key_call(key: &str) -> &str {
    match key.rfind('\0') {
        Some(i) => &key[i + 1..],
        None => key,
    }
}

/// 32 random lowercase hex digits (a v4 UUID's bytes).
pub(crate) fn new_trace_id() -> String {
    hex(uuid::Uuid::new_v4().as_bytes())
}

/// 16 random lowercase hex digits (the first 8 bytes of a v4 UUID).
pub(crate) fn new_span_id() -> String {
    hex(&uuid::Uuid::new_v4().as_bytes()[..8])
}

/// Lowercase hex digits of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Builders shared by the trace manager's unit tests: agents, ADK contexts and a manual clock.
#[cfg(test)]
pub(crate) mod testkit {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use chrono::{DateTime, Duration, TimeZone, Utc};
    use gadk_agent::{Agent, AgentConfig, Mode, ParentMap};
    use gadk_engine::llm::LlmAgentConfig;
    use gadk_engine::{llm_agent, parallel_agent, sequential_agent};
    use parking_lot::Mutex;

    use super::{Clock, Identity};

    /// A custom agent named `name` with `subs`.
    pub(crate) fn custom(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        gadk_agent::new_agent(AgentConfig {
            name: name.into(),
            sub_agents: subs,
            ..Default::default()
        })
        .unwrap()
    }

    /// A sequential agent named `name` with `subs`.
    pub(crate) fn sequential(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        sequential_agent::new(sequential_agent::SequentialAgentConfig {
            agent: AgentConfig {
                name: name.into(),
                sub_agents: subs,
                ..Default::default()
            },
        })
        .unwrap()
    }

    /// A parallel agent named `name` with `subs`.
    pub(crate) fn parallel(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        parallel_agent::new(parallel_agent::ParallelAgentConfig {
            agent: AgentConfig {
                name: name.into(),
                sub_agents: subs,
                ..Default::default()
            },
        })
        .unwrap()
    }

    /// An LLM agent named `name` with `subs` in `mode`.
    pub(crate) fn llm(name: &str, subs: Vec<Arc<dyn Agent>>, mode: Mode) -> Arc<dyn Agent> {
        llm_agent::new(LlmAgentConfig {
            name: name.into(),
            sub_agents: subs,
            mode,
            ..Default::default()
        })
        .unwrap()
    }

    /// An ADK context of `agent` in invocation `inv`, with the parent map of `root` if given.
    pub(crate) fn adk_ctx(
        agent: &Arc<dyn Agent>,
        inv: &str,
        root: Option<&Arc<dyn Agent>>,
    ) -> gadk_agent::Context {
        let mut b = gadk_agent::Context::builder()
            .agent(agent.clone())
            .invocation_id(inv);
        if let Some(root) = root {
            b = b.parent_map(Arc::new(ParentMap::new(root).unwrap()));
        }
        b.build()
    }

    /// The identity of `agent` on `branch` and `path` in invocation `inv`.
    pub(crate) fn ident(inv: &str, agent: &str, branch: &str, path: &str) -> Identity {
        Identity {
            agent_name: agent.into(),
            invocation_id: inv.into(),
            branch: branch.into(),
            path: path.into(),
            ..Identity::default()
        }
    }

    /// A manual clock starting at 2026-01-01T00:00:00Z.
    #[derive(Clone)]
    pub(crate) struct ManualClock(Arc<Mutex<DateTime<Utc>>>);

    impl ManualClock {
        pub(crate) fn new() -> ManualClock {
            ManualClock(Arc::new(Mutex::new(
                Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            )))
        }

        /// Moves the clock forward by `ms` milliseconds.
        pub(crate) fn advance_ms(&self, ms: i64) {
            *self.0.lock() += Duration::milliseconds(ms);
        }

        pub(crate) fn now(&self) -> DateTime<Utc> {
            *self.0.lock()
        }

        pub(crate) fn clock(&self) -> Clock {
            let c = self.0.clone();
            Arc::new(move || *c.lock())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::testkit::*;
    use super::*;

    #[test]
    fn keys_scope_calls_to_the_activation() {
        let id = ident("inv", "agent", "root.agent", "wf/node@1");
        assert_eq!(llm_key(&id), "wf/node@1\x00agent\x00root.agent");
        assert_eq!(
            tool_key(&id, "call_0", "lookup"),
            "wf/node@1\x00agent\x00root.agent\x00call_0"
        );
        assert_eq!(
            tool_key(&id, "", "lookup"),
            "wf/node@1\x00agent\x00root.agent\x00lookup"
        );
        assert_eq!(tool_key_call(&tool_key(&id, "call_0", "lookup")), "call_0");
        assert_eq!(tool_key_call("plain"), "plain");
    }

    #[test]
    fn new_ids_are_random_lowercase_hex() {
        let (t1, t2, s1) = (new_trace_id(), new_trace_id(), new_span_id());
        assert_eq!(t1.len(), 32);
        assert_eq!(s1.len(), 16);
        assert_ne!(t1, t2);
        for id in [&t1, &s1] {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
        assert_eq!(hex(&[0x00, 0xab, 0x7f]), "00ab7f");
    }

    #[test]
    fn background_context_shows_nothing() {
        let ctx = CallbackCtx::background();
        assert_eq!(ctx.activation_key(), None);
        assert_eq!(ctx.call_key("k"), "k");
        assert!(ctx.err().is_none());
        assert!(!ctx.invocation_ended());
        assert!(ctx.canceled_func().is_none());
        assert!(ctx.ended_func().is_none());
        assert!(ctx.parent_map().is_none());
        assert_eq!(ctx.running_agent_children("a"), AgentChildren::default());
        assert_eq!(ctx.ambient_trace_id(), "");
    }

    #[test]
    fn cancellation_reports_canceled_or_the_cause() {
        let token = CancellationToken::new();
        let ctx = CallbackCtx::background().with_cancel(token.clone());
        assert!(ctx.err().is_none());
        assert!(!ctx.canceled_func().unwrap().check().0);
        token.cancel();
        let err = ctx.err().unwrap();
        assert!(matches!(*err, Error::Context(ContextError::Canceled)));
        let (canceled, cause) = ctx.canceled_func().unwrap().check();
        assert!(canceled);
        assert!(cause.is_none());

        let token = CancellationToken::new();
        let cause: TraceError = Arc::new(Error::Context(ContextError::DeadlineExceeded));
        let ctx = CallbackCtx::background()
            .with_cancel(token.clone())
            .with_cancel_cause(cause.clone());
        token.cancel();
        assert!(Arc::ptr_eq(&ctx.err().unwrap(), &cause));
        let (canceled, got) = ctx.canceled_func().unwrap().check();
        assert!(!canceled);
        assert!(Arc::ptr_eq(&got.unwrap(), &cause));
    }

    #[test]
    fn ambient_span_needs_a_valid_span() {
        let sc = SpanIds {
            trace_id: [0xab; 16],
            span_id: [0x01; 8],
        };
        let ctx = CallbackCtx::background().with_span(sc);
        assert_eq!(ctx.ambient_trace_id(), "ab".repeat(16));
        assert_eq!(ctx.ambient_span_context(), Some(sc));
        let ctx = CallbackCtx::background().with_span(SpanIds::default());
        assert_eq!(ctx.ambient_trace_id(), "");
        assert_eq!(ctx.ambient_span_context(), None);
    }

    #[test]
    fn adk_context_gives_activation_end_cancellation_and_parent_map() {
        let child = custom("child", vec![]);
        let root = sequential("root", vec![child.clone()]);
        let adk = adk_ctx(&child, "inv", Some(&root));
        let ctx = CallbackCtx::from_context(&adk);
        assert_eq!(ctx.activation_key(), Some(adk.activation_id()));
        assert_eq!(
            ctx.call_key("k"),
            format!("{}\x00k", adk.activation_id().get())
        );
        assert!(ctx.parent_map().unwrap().parent("child").is_some());
        assert!(!ctx.invocation_ended());
        adk.end_invocation();
        assert!(ctx.invocation_ended());
        assert!(ctx.ended_func().unwrap().ended());
        assert!(ctx.err().is_none());
        adk.cancellation_token().cancel();
        assert!(ctx.err().is_some());

        let ictx = adk.as_invocation().clone();
        let ctx = CallbackCtx::from_invocation(&ictx);
        assert_eq!(ctx.activation_key(), Some(adk.activation_id()));
    }

    /// The ADK context's cause is read when asked (R9): a scope cancelled with an `Error` or
    /// `DeadlineExceeded` after the callback context was made reports that cause.
    #[test]
    fn adk_context_cause_is_read_lazily() {
        let base = adk_ctx(&custom("a", vec![]), "inv", None);
        let (child, _guard) = base.with_agent_cancel();
        let ctx = CallbackCtx::from_context(&child);
        let src = ctx.canceled_func().unwrap();
        assert!(ctx.err().is_none());
        let boom: Arc<dyn std::error::Error + Send + Sync> =
            Arc::new(std::io::Error::other("boom"));
        child.cancel_with_cause(gadk_agent::CancelCause::Error(boom));
        assert_eq!(ctx.err().unwrap().to_string(), "boom");
        let (canceled, cause) = src.check();
        assert!(!canceled);
        assert_eq!(cause.unwrap().to_string(), "boom");

        let (child, _guard) = base.with_agent_timeout(std::time::Duration::ZERO);
        let ctx = CallbackCtx::from_context(&child);
        assert!(
            ctx.err()
                .unwrap()
                .is_context(ContextError::DeadlineExceeded)
        );
        let (canceled, cause) = ctx.canceled_func().unwrap().check();
        assert!(!canceled);
        assert!(cause.unwrap().is_context(ContextError::DeadlineExceeded));
    }

    #[test]
    fn running_agent_children_reads_the_agent_kind() {
        let (a, b) = (custom("a", vec![]), custom("b", vec![]));
        let seq = sequential("seq", vec![a.clone(), b.clone()]);
        let ctx = CallbackCtx::from_context(&adk_ctx(&seq, "inv", None));
        let got = ctx.running_agent_children("seq");
        assert_eq!(
            got,
            AgentChildren {
                parent: "seq".into(),
                non_llm: true,
                propagates_child_error: true,
                is_parallel: false,
                is_workflow: false,
                task_mode: false,
                names: vec!["a".into(), "b".into()],
            }
        );
        assert_eq!(
            ctx.running_agent_children("other"),
            AgentChildren::default()
        );
        assert_eq!(ctx.running_agent_children(""), AgentChildren::default());

        let par = parallel("par", vec![custom("x", vec![])]);
        let got =
            CallbackCtx::from_context(&adk_ctx(&par, "inv", None)).running_agent_children("par");
        assert!(got.is_parallel && got.non_llm && got.propagates_child_error);

        let task = llm("task", vec![], gadk_agent::Mode::Task);
        let got =
            CallbackCtx::from_context(&adk_ctx(&task, "inv", None)).running_agent_children("task");
        assert!(got.task_mode && !got.non_llm && !got.propagates_child_error);

        let plain = custom("plain", vec![]);
        let got = CallbackCtx::from_context(&adk_ctx(&plain, "inv", None))
            .running_agent_children("plain");
        assert!(got.non_llm && !got.propagates_child_error && !got.task_mode);
    }
}
