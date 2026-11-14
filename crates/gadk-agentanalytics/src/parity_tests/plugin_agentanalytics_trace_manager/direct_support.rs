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

//! Helpers of the direct trace-manager test ports.
//!
//! Go → gadk: `spanCounts` / `openSpans` = [`SpanCounts`] / [`open_spans`];
//! `trackedInvocations` = [`tracked_invocations`]; `setAgentSpan` = [`set_agent_span`];
//! `tm.invocations[id]` = [`invocation_of`]; `contextRunning` = [`context_running`]; the plugin
//! test file's `fakeTool` = [`FakeTool`]; `clockedPlugin.firstTokenTimed` = [`first_token_timed`].
//!
//! Go's `mockInvocationContext` shows the trace manager no activation (it has no `WithICDelta`), so
//! every callback of one `mCtx` reads as the same run. gadk's ADK contexts always carry one, and
//! each `MockCtx::context()` call builds a new one; [`MockRun`] builds the context once and derives
//! the tool and invocation contexts from it (`with_delta` keeps the activation), so the callbacks
//! of one `mCtx` share one activation, as one agent run's callbacks do.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(dead_code)] // Helpers used by some of direct_1 … direct_4 only.

use std::sync::Arc;

use gadk_agent::{Agent, Context, ContextDelta, InvocationContext};

use crate::error::Error;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    ClockedPlugin, MockCtx,
};
pub(crate) use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    fake_tool, text_content, tracked_invocations,
};
use crate::plugin::rows::identity_of;
use crate::trace::{
    CallbackCtx, InvocationTrace, SpanRecord, TraceError, TraceManager, llm_key,
};

/// Go `spanCounts`: the spans an invocation tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SpanCounts {
    pub(crate) agents: usize,
    pub(crate) models: usize,
    pub(crate) tools: usize,
    pub(crate) errored_tools: usize,
}

/// Go `tm.invocations[invocationID]` (panics when the invocation is not tracked).
pub(crate) fn invocation_of(tm: &TraceManager, invocation_id: &str) -> Arc<InvocationTrace> {
    tm.mu
        .lock()
        .invocations
        .get(invocation_id)
        .cloned()
        .unwrap_or_else(|| panic!("invocation {invocation_id:?} is not tracked"))
}

/// Go `context.Background()`.
pub(crate) fn bg() -> CallbackCtx {
    CallbackCtx::background()
}

/// Go `errors.New(text)` as the trace manager keeps errors.
pub(crate) fn err(text: &str) -> TraceError {
    Arc::new(Error::msg(text))
}

/// Go `openSpans(t, tm, invocationID)`: the running agents (without the invocation span), model
/// calls and tool calls, and the tool calls that `OnToolErrorCallback` ended.
pub(crate) fn open_spans(tm: &TraceManager, invocation_id: &str) -> SpanCounts {
    let inv = invocation_of(tm, invocation_id);
    let st = inv.mu.lock();
    SpanCounts {
        agents: st.agent_stack.len() - 1,
        models: st.llm_spans.len(),
        tools: st.tool_spans.len(),
        errored_tools: st.completed_tools.len(),
    }
}

/// Go `setAgentSpan(t, tm, invocationID, name, set)`: applies `set` to the open agent spans
/// named `name`.
pub(crate) fn set_agent_span(
    tm: &TraceManager,
    invocation_id: &str,
    name: &str,
    mut set: impl FnMut(&mut SpanRecord),
) {
    let inv = invocation_of(tm, invocation_id);
    let st = inv.mu.lock();
    for rec in &st.agent_stack {
        let mut r = rec.lock();
        if r.name == name {
            set(&mut r);
        }
    }
}

/// Go `contextRunning(a)`: a callback context that knows only the running agent `a`.
pub(crate) fn context_running(a: &Arc<dyn Agent>) -> CallbackCtx {
    let ctx = Context::builder().agent(a.clone()).build();
    CallbackCtx::from_context(&ctx)
}

/// The ADK contexts of one Go `mockInvocationContext` (see the module docs): every context of a
/// `MockRun` carries the activation of its one base context.
#[derive(Clone)]
pub(crate) struct MockRun {
    pub(crate) mock: MockCtx,
    base: Context,
}

impl MockRun {
    /// The run of `mock` (its function call ID, if any, is ignored: see [`tool`](Self::tool)).
    pub(crate) fn new(mock: &MockCtx) -> MockRun {
        let mock = MockCtx {
            function_call_id: String::new(),
            ..mock.clone()
        };
        let base = mock.context();
        MockRun { mock, base }
    }

    /// The agent / model callback context (Go `mCtx`).
    pub(crate) fn ctx(&self) -> Context {
        self.base.clone()
    }

    /// The invocation callback context (Go `mCtx` passed to `BeforeRun`, `OnEvent`, …).
    pub(crate) fn inv(&self) -> InvocationContext {
        self.base.as_invocation().clone()
    }

    /// Go `withFunctionCall(mCtx, id)`: the tool context of call `id`.
    pub(crate) fn tool(&self, id: &str) -> Context {
        self.base.with_delta(ContextDelta {
            function_call_id: Some(id.to_owned()),
            ..ContextDelta::default()
        })
    }

    /// The trace manager's view of [`ctx`](Self::ctx).
    pub(crate) fn cb(&self) -> CallbackCtx {
        CallbackCtx::from_context(&self.base)
    }

    /// The trace manager's view of [`tool`](Self::tool).
    pub(crate) fn tool_cb(&self, id: &str) -> CallbackCtx {
        CallbackCtx::from_context(&self.tool(id))
    }
}

/// Go `clockedPlugin.firstTokenTimed(t, ctx)`: whether the running model call of `ctx`'s agent
/// has a time to first token (model calls are keyed by the activation: `call_key`).
pub(crate) fn first_token_timed(c: &ClockedPlugin, ctx: &CallbackCtx) -> bool {
    let id = identity_of(ctx);
    let traces = c.plugin.traces.as_ref().expect("trace manager");
    let inv = traces.invocation(ctx, &id);
    let st = inv.mu.lock();
    let rec = st
        .llm_spans
        .get(&ctx.call_key(&llm_key(&id)))
        .unwrap_or_else(|| panic!("no model call is running"));
    rec.lock().first_token.is_some()
}
