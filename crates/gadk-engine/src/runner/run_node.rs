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

//! Port of Go runner/run_node.go: drives an agent through a single-node root workflow
//! (START -> agent node), sharing the agent path's session/plugin/persist pipeline.

use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentError, AgentType, EventStream, InvocationContext, Mode, RunConfig, run_limits,
};
use gadk_genai::{Content, FunctionResponse};
use gadk_session::{Event, Session};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::agent_node::new_agent_node;
use super::compaction::InvocationGuard;
use super::{Processed, RunOptions, RunnerInner, wrap};
use crate::artifacts::ScopedArtifacts;
use crate::llm::mode::with_bound_mode;
use crate::memory::ScopedMemory;
use crate::tool::utils::{function_calls, function_responses};
use crate::workflow::{Edge, Node, NodeStatus, RunState, START, Workflow, WorkflowOptions};

/// Port of Go runner/run_node.go `isLlmAgent`.
pub(crate) fn is_llm_agent(a: &dyn Agent) -> bool {
    a.agent_state().agent_type == AgentType::LlmAgent
}

/// Port of Go runner/run_node.go `buildRunnerNode`: wraps any agent as the node the node runtime
/// drives (Go's nil check has no Rust counterpart).
pub(crate) fn build_runner_node(a: &Arc<dyn Agent>) -> Result<Arc<dyn Node>, AgentError> {
    Ok(new_agent_node(a.clone()))
}

/// Port of Go runner/run_node.go `rootWorkflowName`: stable across turns and unique per agent.
fn root_workflow_name(app_name: &str, a: &dyn Agent) -> String {
    format!("{app_name}/{}", a.name())
}

impl RunnerInner {
    /// Port of Go runner/run_node.go `(*Runner).newNodeInvocationContext`; the agent path
    /// (runner.go:678-706) builds the same context inline and uses it too.
    pub(super) fn new_invocation_context(
        &self,
        stored: &Arc<dyn Session>,
        agent: Arc<dyn Agent>,
        msg: Option<Content>,
        cfg: &RunConfig,
        token: &CancellationToken,
        compaction: bool,
    ) -> InvocationContext {
        // Go runconfig.ToContext(ctx, &RunConfig{MaxLLMCalls: ResolveMaxLLMCalls(..)}): one
        // budget for the whole invocation.
        let limiter = Arc::new(run_limits::LlmCallLimiter::new(
            run_limits::resolve_max_llm_calls(cfg.max_llm_calls),
        ));
        let mut invocation_id = resolve_invocation_id(Some(&**stored), msg.as_ref());
        if invocation_id.is_empty() {
            invocation_id = format!("e-{}", self.platform.new_uuid());
        }
        let mut b = InvocationContext::builder()
            .session(stored.clone())
            .session_service(self.session_service.clone())
            .agent(agent)
            .invocation_id(invocation_id)
            .run_config(cfg.clone())
            .llm_call_limiter(limiter)
            .platform(self.platform.clone())
            .plugin_manager(self.plugin_manager.clone())
            .parent_map(self.parents.clone())
            .cancellation_token(token.clone());
        if let Some(m) = msg {
            b = b.user_content(m);
        }
        if let Some(svc) = &self.artifact_service {
            b = b.artifacts(Arc::new(ScopedArtifacts::new(
                svc.clone(),
                stored.app_name(),
                stored.user_id(),
                stored.id(),
            )));
        }
        if let Some(svc) = &self.memory_service {
            b = b.memory(Arc::new(ScopedMemory::new(
                svc.clone(),
                stored.app_name(),
                stored.user_id(),
            )));
        }
        // Go compactionctx.ToContext(ctx, r.compactionRuntime()): one runtime for the whole
        // invocation, shared by every context derived from it. Go RunLive attaches none.
        if compaction && let Some(cfg) = &self.compaction {
            b = crate::compaction::runtime::attach(
                b,
                crate::compaction::runtime::CompactionRuntime::new(
                    cfg.clone(),
                    Some(self.session_service.clone()),
                ),
            );
        }
        b.build_invocation()
    }
}

/// Port of Go runner/run_node.go `(*Runner).runNode`: wraps `agent_to_run`
/// in a START -> node workflow and runs or resumes it.
pub(super) fn run_node(
    inner: Arc<RunnerInner>,
    stored: Arc<dyn Session>,
    agent_to_run: Arc<dyn Agent>,
    root_mode: Mode,
    msg: Option<Content>,
    cfg: RunConfig,
    opts: RunOptions,
) -> EventStream {
    Box::pin(async_stream::stream! {
        let node = match build_runner_node(&agent_to_run) {
            Ok(n) => n,
            Err(e) => { yield Err(e); return; }
        };
        // A synthetic START -> node workflow; the root wrapper keeps "app/agent@1" off event
        // paths.
        let edges = vec![Edge { from: START.clone(), to: node, route: None }];
        let wf = match Workflow::new(
            &root_workflow_name(&inner.app_name, &*agent_to_run),
            edges,
            WorkflowOptions { root_wrapper: true, ..Default::default() },
        ) {
            Ok(w) => w,
            Err(e) => { yield Err(wrap("failed to build node workflow", e)); return; }
        };

        let token = CancellationToken::new();
        let cancel_on_drop = token.clone().drop_guard();
        // The user content is the workflow's seed input.
        let ictx = inner.new_invocation_context(&stored, agent_to_run.clone(), msg.clone(), &cfg, &token, true);
        // Go runner.go:610 binds the ROOT agent's placement on the context handed to runNode.
        let ictx = with_bound_mode(
            &ictx,
            inner.root_agent.name(),
            inner.root_agent.agent_state(),
            root_mode,
        );

        // Append the user message (also runs on_user_message), as the agent path does.
        let appended = inner
            .append_message_to_session(
                ictx.into_context(),
                &stored,
                msg.clone(),
                cfg.save_input_blobs_as_artifacts,
                opts.state_delta.clone(),
            )
            .await;
        let (ctx, user_event) = match appended {
            Ok(v) => v,
            Err(e) => { yield Err(e); return; }
        };
        // Go's deferred compactOnce is registered before the user message is yielded, the
        // deferred after-run callbacks after it.
        let ictx = ctx.as_invocation().clone();
        let mut guard = InvocationGuard::new(inner.clone(), stored.clone(), ictx.clone());
        guard.cancel_on_drop(cancel_on_drop);
        if opts.yield_user_message && let Some(ev) = user_event {
            guard.at_yield(true);
            yield Ok(ev);
            guard.at_yield(false);
        }

        // Plugin lifecycle: defer after_run, run before_run, honor an early exit.
        guard.defer_after_run();
        let mut early = false;
        'run: {
            match inner.run_before_run_callback(&ictx).await {
                Err(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); early = true; break 'run; }
                Ok(Some(ev)) => { guard.at_yield(true); yield Ok(ev); guard.at_yield(false); early = true; break 'run; }
                Ok(None) => {}
            }

            // Resume (HITL continuation) vs fresh run: the paused state comes from session
            // history (None = Go nil *RunState).
            let state: Option<RunState> = match wf.reconstruct_run_state(&*stored, ictx.invocation_id()) {
                Ok(s) => s,
                Err(e) => {
                    guard.mark_failed();
                    guard.at_yield(true);
                    yield Err(wrap("failed to reconstruct workflow run state", e));
                    guard.at_yield(false);
                    early = true;
                    break 'run;
                }
            };
            let responses = build_resume_responses(msg.as_ref(), state.as_ref(), &*stored);
            let mut events: EventStream = match (responses.is_empty(), state) {
                (true, _) => wf.run(ictx.clone()),
                (false, Some(state)) => wf.resume(ctx.clone(), state, responses),
                // Go Workflow.Resume with a nil state yields nothing.
                (false, None) => Box::pin(futures::stream::empty()),
            };

            while let Some(item) = events.next().await {
                let mut event = match item {
                    Ok(ev) => ev,
                    Err(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); continue; }
                };
                // Engine control events have no author; a later find_agent_to_run would warn
                // about the empty author.
                if event.author.is_empty() {
                    event.author = agent_to_run.name().to_string();
                }
                match inner.process_event(&ictx, &*stored, event).await {
                    Processed::Event(ev) => { guard.at_yield(true); yield Ok(*ev); guard.at_yield(false); }
                    Processed::PluginError(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); }
                    Processed::AppendError(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); early = true; break 'run; }
                }
            }
        }
        if early {
            guard.finish_early().await;
        } else {
            // Compact once every event of the invocation is persisted; the consumer drained the
            // stream, so a failure can still be reported.
            if let Some(e) = guard.compact_inline().await {
                guard.at_yield(true);
                yield Err(e);
                guard.at_yield(false);
            }
            guard.finish().await;
        }
    })
}

/// Port of Go runner/run_node.go `buildResumeResponses`: `msg`'s function responses that answer
/// a still-pending interrupt (waiting in `state`, or an open long-running call in history),
/// keyed by interrupt ID. Empty = Go nil (a fresh turn).
fn build_resume_responses(
    msg: Option<&Content>,
    state: Option<&RunState>,
    sess: &dyn Session,
) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(msg) = msg else {
        return out;
    };
    let mut pending: HashSet<String> = HashSet::new();
    if let Some(state) = state {
        pending.extend(waiting_interrupt_ids(state));
    }
    pending.extend(open_long_running_call_ids(Some(sess)));
    if pending.is_empty() {
        return out;
    }
    for fr in function_responses(Some(msg)) {
        if fr.id.is_empty() || !pending.contains(&fr.id) {
            continue;
        }
        // Opaque payload; Workflow::resume validates it against any schema.
        out.insert(fr.id.clone(), decode_resume_response(fr));
    }
    out
}

/// Port of Go runner/run_node.go `openLongRunningCallIDs`: long-running call IDs in history
/// without a matching function response.
fn open_long_running_call_ids(sess: Option<&dyn Session>) -> HashSet<String> {
    let mut open = HashSet::new();
    let Some(sess) = sess else {
        return open;
    };
    let mut answered = HashSet::new();
    for ev in sess.events() {
        open.extend(ev.long_running_tool_ids.iter().cloned());
        for fr in function_responses(ev.response.content.as_ref()) {
            if !fr.id.is_empty() {
                answered.insert(fr.id.clone());
            }
        }
    }
    open.retain(|id| !answered.contains(id));
    open
}

/// Port of Go runner/run_node.go `decodeResumeResponse`: `{"response": v}` (a string `v` is
/// JSON-parsed when possible), else `{"payload": v}`, else the whole map. Go's nil `Response`
/// has no Rust counterpart.
fn decode_resume_response(fr: &FunctionResponse) -> Value {
    if let Some(raw) = fr.response.get("response") {
        if let Value::String(s) = raw {
            return serde_json::from_str::<Value>(s).unwrap_or_else(|_| raw.clone());
        }
        return raw.clone();
    }
    if let Some(payload) = fr.response.get("payload") {
        return payload.clone();
    }
    Value::Object(fr.response.clone())
}

/// Port of Go runner/run_node.go `waitingInterruptIDs`: the non-empty interrupt IDs of every
/// waiting node.
fn waiting_interrupt_ids(state: &RunState) -> HashSet<String> {
    state
        .nodes
        .values()
        .filter(|ns| ns.status == NodeStatus::Waiting)
        .flat_map(|ns| ns.interrupts.iter())
        .filter(|id| !id.is_empty())
        .cloned()
        .collect()
}

/// Port of Go runner/run_node.go `resolveInvocationID`: the invocation ID of the event holding the
/// call that the first answerable function response in `msg` answers, so a HITL resume shares
/// its pause's invocation; `""` otherwise.
pub(super) fn resolve_invocation_id(sess: Option<&dyn Session>, msg: Option<&Content>) -> String {
    let (Some(sess), Some(msg)) = (sess, msg) else {
        return String::new();
    };
    for fr in function_responses(Some(msg)) {
        if fr.id.is_empty() {
            continue;
        }
        if let Some(ev) = find_event_by_function_call_id(Some(sess), &fr.id) {
            return ev.invocation_id;
        }
    }
    String::new()
}

/// Port of Go runner/run_node.go `findEventByFunctionCallID`: the most recent event carrying a
/// function call with `id` (reverse scan).
fn find_event_by_function_call_id(sess: Option<&dyn Session>, id: &str) -> Option<Event> {
    let sess = sess?;
    if id.is_empty() {
        return None;
    }
    sess.events().into_iter().rev().find(|ev| {
        function_calls(ev.response.content.as_ref())
            .iter()
            .any(|fc| fc.id == id)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fr(response: Value) -> FunctionResponse {
        let Value::Object(m) = response else {
            unreachable!()
        };
        FunctionResponse {
            id: "c".into(),
            name: "ask".into(),
            response: m,
            ..Default::default()
        }
    }

    #[test]
    fn decode_resume_response_matches_go() {
        assert_eq!(
            decode_resume_response(&fr(json!({"response": "{\"a\":1}"}))),
            json!({"a": 1})
        );
        assert_eq!(
            decode_resume_response(&fr(json!({"response": "plain"}))),
            json!("plain")
        );
        assert_eq!(
            decode_resume_response(&fr(json!({"response": 3}))),
            json!(3)
        );
        assert_eq!(
            decode_resume_response(&fr(json!({"payload": "yes"}))),
            json!("yes")
        );
        assert_eq!(
            decode_resume_response(&fr(json!({"x": 1}))),
            json!({"x": 1})
        );
    }
}
