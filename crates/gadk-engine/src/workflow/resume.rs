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

//! Port of Go workflow/resume.go.

use std::collections::HashSet;
use std::sync::Arc;

use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::error::WorkflowErrorKind;
use crate::tool::utils::{CompiledSchema, convert_with_schema};
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::{Scheduler, find_successors, wf_error};
use crate::workflow::state::{NodeStatus, RunState};
use crate::workflow::{Node, Workflow};

/// Port of Go resumeOutput: one matched response is forwarded as itself, several as the map.
pub(crate) fn resume_output(matched: Map<String, Value>) -> Value {
    if matched.len() == 1 {
        return matched
            .into_iter()
            .map(|(_, v)| v)
            .next()
            .unwrap_or(Value::Null);
    }
    Value::Object(matched)
}

/// Port of Go validateResumeResponse: coerces `resp` through `schema`, resolved per call (Go
/// does not cache: persisted schemas round-trip through JSON).
pub(crate) fn validate_resume_response(resp: Value, schema: &Value) -> Result<Value, String> {
    let compiled =
        CompiledSchema::new(schema.clone()).map_err(|e| format!("resolve schema: {e}"))?;
    convert_with_schema::<Value, Value>(&resp, Some(&compiled))
}

/// Port of Go (*Workflow).Resume. Pass 1 acts on the nodes whose interrupts this turn's
/// responses answer (re-entry, pending or failed → Pending and re-run with the responses;
/// handoff → Completed with the response as output). Pass 2 schedules the handoffs' successors,
/// skipping ones already completed in history. A response failing its interrupt's schema yields
/// `InvalidResumeResponse` and leaves that node alone. No match yields `NothingToResume`;
/// otherwise failed siblings are retried and the scheduler runs. Empty `responses` is a clean
/// no-op (Go's nil state is the caller's `None` from `reconstruct_run_state`).
///
/// Go's deferred cleanup (`cancelAll` + drain + `wg.Wait`) needs no code: dropping the stream
/// drops the scheduler, which cancels every node token and hands the drain of the still-running
/// node tasks to the invocation's teardown registry, so no successor is dispatched after the
/// consumer leaves.
pub(crate) fn resume_stream(
    wf: Arc<Workflow>,
    ctx: Context,
    mut state: RunState,
    responses: Map<String, Value>,
) -> EventStream {
    Box::pin(async_stream::stream! {
        if responses.is_empty() {
            return;
        }
        let completed = std::mem::take(&mut state.completed);
        let mut s = Scheduler::new(ctx.clone(), Arc::clone(&wf.graph), wf.max_concurrency);
        s.workflow_name = wf.name.clone();
        s.state = state;
        let mut deferred: Vec<(Arc<dyn Node>, Option<Value>)> = Vec::new();
        let mut scheduled = 0usize;
        let mut invalid: HashSet<String> = HashSet::new();
        let mut names: Vec<String> = s.state.nodes.keys().cloned().collect();
        // Go iterates a map; sorted order makes the first schema error deterministic.
        names.sort();
        for name in &names {
            let Some(node) = s.nodes_by_name.get(name).cloned() else { continue };
            let Some(ns) = s.state.nodes.get(name) else { continue };
            let answered_now = ns
                .resumed_inputs
                .as_ref()
                .is_some_and(|m| m.keys().any(|id| responses.contains_key(id)));
            let status = ns.status;
            // Direct callers may supply a new or corrected response not yet in history:
            // validate it before a waiting handoff or merging it into re-entry inputs.
            let mut fresh = Map::new();
            let mut schema_err = None;
            if matches!(status, NodeStatus::Waiting | NodeStatus::Pending | NodeStatus::Failed) {
                let mut ids = ns.interrupts.clone();
                if status != NodeStatus::Waiting {
                    ids.extend(ns.resumed_inputs.iter().flat_map(|m| m.keys().cloned()));
                }
                for id in ids {
                    let Some(resp) = responses.get(&id) else { continue };
                    let resp = match ns.interrupt_schemas.get(&id) {
                        Some(sc) => match validate_resume_response(resp.clone(), sc) {
                            Ok(v) => v,
                            Err(e) => {
                                schema_err = Some(wf_error(
                                    WorkflowErrorKind::InvalidResumeResponse,
                                    format!(": node {}: {e}", go_quote(name)),
                                ));
                                break;
                            }
                        },
                        None => resp.clone(),
                    };
                    fresh.insert(id, resp);
                }
            }
            if let Some(e) = schema_err {
                yield Err::<Event, AgentError>(e);
                invalid.insert(name.clone());
                continue;
            }
            if !answered_now && fresh.is_empty() {
                continue;
            }
            let reenter = node.config().rerun_on_resume == Some(true);
            let Some(ns) = s.state.nodes.get_mut(name) else { continue };
            if reenter || status == NodeStatus::Pending || status == NodeStatus::Failed {
                ns.resumed_inputs.get_or_insert_with(Map::new).extend(fresh);
                ns.status = NodeStatus::Pending;
                let (input, tb, br, ri) = (
                    ns.input.clone(),
                    ns.triggered_by.clone(),
                    ns.branch.clone(),
                    ns.resumed_inputs.clone(),
                );
                s.schedule_resumed_node(node, input, tb, br, ri);
                scheduled += 1;
            } else {
                // Handoff: the response is the asker's output; the asker does not re-run.
                let had_fresh = !fresh.is_empty();
                let out = if had_fresh { Some(resume_output(fresh)).filter(|v| !v.is_null()) } else { ns.output.clone() };
                ns.status = NodeStatus::Completed;
                ns.output = out.clone();
                ns.interrupts.clear();
                // A matched asker is an effective resume even when terminal; a duplicate
                // resume (response already counted in history) stays a no-op.
                if ns.answered_this_turn || had_fresh {
                    scheduled += 1;
                }
                deferred.push((node, out));
            }
        }
        // Pass 2: every matched asker is now Completed, so a downstream join sees a settled
        // predecessor set. Successors reached only via a concrete route do not fire (no event).
        for (node, resp) in deferred {
            let parent_branch = s
                .state
                .nodes
                .get(node.name())
                .map(|n| n.branch.clone())
                .unwrap_or_default();
            let succs = find_successors(&s.graph, &s.state, &node, resp, None, &parent_branch);
            for succ in succs {
                // Skip a successor that already produced output on a prior turn.
                if completed.contains(succ.node.name()) {
                    continue;
                }
                s.schedule_node(succ.node, succ.input, succ.triggered_by, succ.branch);
                scheduled += 1;
            }
        }
        if scheduled == 0 {
            yield Err(wf_error(WorkflowErrorKind::NothingToResume, ""));
            return;
        }
        // A valid resume also retries siblings whose latest attempt failed.
        for name in &names {
            let Some(ns) = s.state.nodes.get(name) else { continue };
            if ns.status != NodeStatus::Failed || invalid.contains(name) {
                continue;
            }
            let Some(node) = s.nodes_by_name.get(name).cloned() else { continue };
            let (input, tb, br, ri) = (
                ns.input.clone(),
                ns.triggered_by.clone(),
                ns.branch.clone(),
                ns.resumed_inputs.clone(),
            );
            s.schedule_resumed_node(node, input, tb, br, ri);
        }
        while let Some(item) = s.next().await {
            yield item;
        }
    })
}
