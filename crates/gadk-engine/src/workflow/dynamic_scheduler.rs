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

//! Runs the children a dynamic node schedules with [`run_node`](crate::workflow::run_node).

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Weak};

use futures::{FutureExt, StreamExt};
use gadk_agent::{AgentError, Context, ContextDelta, DynamicSubScheduler};
use gadk_session::{Event, NodeInfo};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::watch;
use tracing::Instrument;

use crate::error::{NodeRunError, WorkflowError, WorkflowErrorKind, workflow_error};
use crate::tool::utils::{function_calls, function_responses};
use crate::workflow::base_node::validate_and_stamp_output;
use crate::workflow::branch::derive_child_branch;
use crate::workflow::failure_inventory::{self, FailureInventory, record_failed_child_paths};
use crate::workflow::persistence::{
    WORKFLOW_DELEGATED_OUTPUT_KEY, WORKFLOW_FAILED_CHILD_PATHS_KEY, WORKFLOW_NODE_FAILURE_OUTCOME,
    WORKFLOW_NODE_OUTCOME_KEY, completed_delegated_output, workflow_node_outcome,
};
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::{
    child_event_output, is_node_control_flow, record_node_error, start_node_span,
};
use crate::workflow::{Emit, Node, RunNodeOptions};

/// One child run's outcome, shared by every caller that overlapped it.
pub(crate) type RunResult = Result<Value, WorkflowError>;

/// Builds the error of a failed child run, carrying a [`NodeRunError`] (child name, child path,
/// run id and cause). [`WorkflowError::node_run`] gives it the cause's kind and the text
/// "workflow: dynamic child <path>: <cause>"; `node_run_error()` on the result returns the
/// struct. The cause is always a [`WorkflowError`] (see [`to_workflow_error`]) so the kind
/// follows the cause's.
pub(crate) fn node_run_error(
    child_name: &str,
    child_path: &str,
    run_id: &str,
    cause: WorkflowError,
) -> WorkflowError {
    WorkflowError::node_run(NodeRunError {
        child_name: child_name.to_string(),
        child_path: child_path.to_string(),
        run_id: run_id.to_string(),
        cause: Some(Arc::new(AgentError::from(cause))),
    })
}

/// A plain cause as a [`WorkflowError`] of kind `Other` (so a NodeRunError around it matches no
/// workflow sentinel), keeping the original as the source.
fn to_workflow_error(e: AgentError) -> WorkflowError {
    match workflow_error(&e) {
        Some(w) => w.clone(),
        None => WorkflowError::new(WorkflowErrorKind::Other, e.to_string()).with_source(e),
    }
}

/// A `NodeFailed` error with the text `<sentinel text>: <rest>` and `cause` kept as the source.
fn node_failed(rest: String, cause: AgentError) -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorKind::NodeFailed,
        format!(
            "{}: {rest}",
            WorkflowError::sentinel_text(WorkflowErrorKind::NodeFailed)
        ),
    )
    .with_source(cause)
}

/// A `NodeFailed` error with the text `<sentinel text>: child did not complete`.
fn did_not_complete() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorKind::NodeFailed,
        format!(
            "{}: child did not complete",
            WorkflowError::sentinel_text(WorkflowErrorKind::NodeFailed)
        ),
    )
}

/// The at-most-one `use_as_output` delegation of a parent activation. `value: Some(..)` means a
/// value was delegated (`Some(Null)` = a delegated null).
#[derive(Default)]
struct OutputDelegation {
    claimed: bool,
    child_path: String,
    child_name: String,
    value: Option<Value>,
}

impl OutputDelegation {
    /// Reserves the delegation for a child: `Err(existing holder)` on conflict.
    fn reserve(&mut self, child_path: &str, child_name: &str) -> Result<(), String> {
        if self.claimed && self.child_path != child_path {
            return Err(self.child_name.clone());
        }
        self.claimed = true;
        self.child_path = child_path.to_string();
        self.child_name = child_name.to_string();
        Ok(())
    }

    /// Stores the delegated value: a mismatched path is dropped.
    fn commit(&mut self, child_path: &str, value: Value) {
        if !self.claimed || self.child_path != child_path {
            return;
        }
        self.value = Some(value);
    }
}

/// A child run in flight: the leader publishes into `tx`; waiters subscribe.
struct InflightRun {
    tx: watch::Sender<Option<RunResult>>,
}

/// The mutex-guarded state of [`DynSubScheduler`].
#[derive(Default)]
pub(crate) struct DynState {
    run_count_by_child: HashMap<String, u64>,
    pub(crate) result_by_path: HashMap<String, Value>,
    inflight_by_path: HashMap<String, Arc<InflightRun>>,
    delegation: OutputDelegation,
    pub(crate) failed_paths: HashSet<String>,
}

/// Runs the children of one dynamic-node activation.
pub(crate) struct DynSubScheduler {
    me: Weak<DynSubScheduler>,
    parent_path: String,
    parent_ctx: Context,
    emit_up: Emit,
    output_for_ancestors: Vec<String>,
    /// Never held across `.await` (nor across a child's run).
    inner: Mutex<DynState>,
}

/// Creates the sub-scheduler, rehydrating the result cache from session history.
pub(crate) fn new_dynamic_sub_scheduler(
    parent: Context,
    parent_path: String,
    emit_up: Emit,
) -> Arc<DynSubScheduler> {
    let s = Arc::new_cyclic(|me| DynSubScheduler {
        me: me.clone(),
        output_for_ancestors: parent.output_for_ancestors(),
        parent_path,
        parent_ctx: parent,
        emit_up,
        inner: Mutex::new(DynState::default()),
    });
    s.rehydrate_cache();
    s
}

/// Whether the node opts into `wait_for_output`.
pub(crate) fn waits_for_output(node: &dyn Node) -> bool {
    node.config().wait_for_output == Some(true)
}

/// Validates a custom run id: rejects empty ids, purely numeric ids (they collide with the
/// auto-counter), and ids containing the path separators `/` and `@`.
pub(crate) fn validate_custom_run_id(id: &str) -> Result<(), WorkflowError> {
    let s = WorkflowError::sentinel_text(WorkflowErrorKind::InvalidRunId);
    let err = |m: String| Err(WorkflowError::new(WorkflowErrorKind::InvalidRunId, m));
    if id.is_empty() {
        return err(format!("{s}: empty"));
    }
    // ASCII digits only, as the auto-counter emits.
    if id.bytes().all(|b| b.is_ascii_digit()) {
        return err(format!(
            "{s}: {} is purely numeric (reserved for auto-counter)",
            go_quote(id)
        ));
    }
    if id.contains(['/', '@']) {
        return err(format!(
            "{s}: {} contains reserved separator (/ or @)",
            go_quote(id)
        ));
    }
    Ok(())
}

/// Publishes "child did not complete" if the leader future is dropped before it published
/// (so waiters never hang).
struct LeaderGuard<'a> {
    sched: &'a DynSubScheduler,
    child_name: String,
    child_path: String,
    run_id: String,
    armed: bool,
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let res = Err(node_run_error(
                &self.child_name,
                &self.child_path,
                &self.run_id,
                did_not_complete(),
            ));
            self.sched
                .finish_run(&self.child_path, res, false, &FailureInventory::default());
        }
    }
}

/// Removes every cached result at or below `path` (called with the state locked).
fn invalidate_cache_path(st: &mut DynState, path: &str) {
    let sub = format!("{path}/");
    st.result_by_path
        .retain(|p, _| !(p == path || p.starts_with(&sub)));
}

impl DynSubScheduler {
    /// Test access to the guarded state.
    #[cfg(test)]
    pub(crate) fn state(&self) -> parking_lot::MutexGuard<'_, DynState> {
        self.inner.lock()
    }

    /// Repopulates the result cache from this invocation's history so
    /// a resumed orchestrator serves completed children from cache instead of re-running them.
    fn rehydrate_cache(&self) {
        let invocation_id = self.parent_ctx.invocation_id().to_string();
        let prefix = format!("{}/", self.parent_path);
        let events = self.parent_ctx.session().events();
        let mut st = self.inner.lock();
        for ev in &events {
            let Some(info) = ev.node_info.as_ref() else {
                continue;
            };
            if ev.response.partial {
                continue;
            }
            if !invocation_id.is_empty() && ev.invocation_id != invocation_id {
                continue;
            }
            // The parent records child failures even when cancellation stops a child from
            // emitting its own marker.
            let mut known_paths = false;
            if let Some(Value::Array(paths)) = ev
                .response
                .custom_metadata
                .get(WORKFLOW_FAILED_CHILD_PATHS_KEY)
            {
                // An explicit empty array records a completed inventory with no uncertain
                // children.
                known_paths = paths.is_empty();
                for p in paths {
                    if let Value::String(p) = p
                        && !p.is_empty()
                    {
                        known_paths = true;
                        invalidate_cache_path(&mut st, p);
                    }
                }
            }
            let has_output = child_event_output(ev).is_some();
            if !known_paths && !has_output && !workflow_node_outcome(ev).is_empty() {
                // No completion evidence for that attempt: conservatively retry.
                invalidate_cache_path(&mut st, &info.path);
            }
            if !info.path.starts_with(&prefix) {
                continue;
            }
            // A partial result followed by failure or a pause is not a cached success; the
            // last outcome wins.
            if !ev.long_running_tool_ids.is_empty() || ev.requested_input.is_some() {
                st.result_by_path.remove(&info.path);
            } else if let Some(out) = child_event_output(ev) {
                st.result_by_path.insert(info.path.clone(), out);
            } else if let Some(out) = completed_delegated_output(ev) {
                if !out.is_null() || !st.result_by_path.contains_key(&info.path) {
                    st.result_by_path.insert(info.path.clone(), out);
                }
            } else if !workflow_node_outcome(ev).is_empty() {
                st.result_by_path.remove(&info.path);
            }
        }
    }

    /// Runs `child` once (or shares/replays its
    /// outcome) and classifies it: HITL → NodeInterrupted, runtime failure → NodeFailed.
    pub(crate) async fn run_child(
        &self,
        child: Arc<dyn Node>,
        input: Value,
        opts: RunNodeOptions,
    ) -> Result<Value, AgentError> {
        let name = child.name().to_string();
        let run_id = self
            .resolve_run_id(&name, opts.run_id.as_deref().unwrap_or(""))
            .map_err(|cause| AgentError::from(node_run_error(&name, "", "", cause)))?;
        let child_path = format!("{}/{}@{}", self.parent_path, name, run_id);
        // Claim before running so a sibling WithUseAsOutput fails fast.
        self.claim_delegation(&child_path, &name, opts.use_as_output)?;
        let child_branch = derive_child_branch(
            &self.parent_ctx.branch(),
            &name,
            &run_id,
            opts.use_sub_branch,
            opts.override_branch.as_deref().unwrap_or(""),
        );
        // A delegating child extends the chain for its own delegating children.
        let child_ancestors = if opts.use_as_output {
            std::iter::once(self.parent_path.clone())
                .chain(self.output_for_ancestors.iter().cloned())
                .collect()
        } else {
            Vec::new()
        };
        // An explicit scope wins over the node-path default; absent both, inherit.
        let child_scope = match opts.isolation_scope.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None if opts.isolation_scope_from_node_path => child_path.clone(),
            None => self.parent_ctx.isolation_scope(),
        };
        let me: Option<Arc<dyn DynamicSubScheduler>> =
            self.me.upgrade().map(|a| a as Arc<dyn DynamicSubScheduler>);
        let child_ctx = self.parent_ctx.with_delta(ContextDelta {
            path: Some(child_path.clone()),
            run_id: Some(run_id.clone()),
            sub_scheduler: Some(me),
            output_for_ancestors: Some(child_ancestors),
            branch: Some(child_branch),
            isolation_scope: Some(child_scope.clone()),
            ..ContextDelta::default()
        });
        // The span opens before the cache lookup, so a replayed hit is still its own span.
        let span = start_node_span(&child_ctx, &child);
        let fut = self.lead_or_share(
            child,
            child_ctx,
            input,
            &opts,
            &name,
            &child_path,
            &run_id,
            &child_scope,
        );
        let (res, control_flow) = match &span {
            Some(s) => fut.instrument(s.clone()).await,
            None => fut.await,
        };
        record_node_error(span.as_ref(), res.as_ref().err(), control_flow);
        res
    }

    /// The cache / single-flight half of [`run_child`](Self::run_child), then the leader's run
    /// and publication. Returns the outcome and the span's control-flow classification of the
    /// raw error.
    #[allow(clippy::too_many_arguments)]
    async fn lead_or_share(
        &self,
        child: Arc<dyn Node>,
        child_ctx: Context,
        input: Value,
        opts: &RunNodeOptions,
        name: &str,
        child_path: &str,
        run_id: &str,
        child_scope: &str,
    ) -> (Result<Value, AgentError>, Option<bool>) {
        if let Some(res) = self.await_or_lead(child_path).await {
            return match res {
                Ok(v) => {
                    self.commit_delegation(child_path, v.clone());
                    (Ok(v), None)
                }
                Err(e) => (Err(e), None),
            };
        }
        // This caller leads: every exit path publishes to the waiters.
        let mut guard = LeaderGuard {
            sched: self,
            child_name: name.into(),
            child_path: child_path.into(),
            run_id: run_id.into(),
            armed: true,
        };
        let recorder = failure_inventory::with_failure_inventory(&child);
        let drive = failure_inventory::scope(
            Arc::clone(&recorder),
            self.drive_child(
                &child,
                child_ctx.clone(),
                input,
                opts,
                name,
                child_path,
                run_id,
                child_scope,
            ),
        );
        match AssertUnwindSafe(drive).catch_unwind().await {
            Ok((res, control_flow, child_failed)) => {
                // The iterator finished unwinding: its failure evidence can be trusted.
                let inventory = if child_failed {
                    recorder.snapshot()
                } else {
                    FailureInventory::default()
                };
                self.publish_outcome(&child_ctx, child_path, &res, inventory)
                    .await;
                guard.armed = false;
                (res.map_err(AgentError::from), control_flow)
            }
            Err(panic) => {
                // A panic publishes "child did not complete" to the waiters, then propagates.
                let res: RunResult =
                    Err(node_run_error(name, child_path, run_id, did_not_complete()));
                self.publish_outcome(&child_ctx, child_path, &res, FailureInventory::default())
                    .await;
                guard.armed = false;
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// The closing step of a child run: the child-failure marker, then
    /// [`finish_run`](Self::finish_run).
    async fn publish_outcome(
        &self,
        child_ctx: &Context,
        child_path: &str,
        res: &RunResult,
        inventory: FailureInventory,
    ) {
        let mut failure_recorded = false;
        if let Err(e) = res
            && !e.is(WorkflowErrorKind::NodeInterrupted)
        {
            // The parent's failure record cannot invalidate this child's path-keyed cache on a
            // later re-entry; record the child failure too, without its error text.
            let mut ev = Event::new(child_ctx.platform(), child_ctx.invocation_id());
            ev.node_info = Some(NodeInfo {
                path: child_path.to_string(),
                ..NodeInfo::default()
            });
            ev.response.custom_metadata.insert(
                WORKFLOW_NODE_OUTCOME_KEY.into(),
                Value::String(WORKFLOW_NODE_FAILURE_OUTCOME.into()),
            );
            record_failed_child_paths(&mut ev.response.custom_metadata, &inventory);
            failure_recorded = (self.emit_up)(ev).await.is_ok();
        }
        self.finish_run(child_path, res.clone(), failure_recorded, &inventory);
    }

    /// The child loop of [`run_child`](Self::run_child). Returns the outcome, the span's
    /// control-flow classification of the raw error, and whether the child stream yielded an error
    /// (so the failure inventory must be snapshotted).
    #[allow(clippy::too_many_arguments)]
    async fn drive_child(
        &self,
        child: &Arc<dyn Node>,
        child_ctx: Context,
        input: Value,
        opts: &RunNodeOptions,
        name: &str,
        child_path: &str,
        run_id: &str,
        child_scope: &str,
    ) -> (RunResult, Option<bool>, bool) {
        let nre = |cause: WorkflowError| node_run_error(name, child_path, run_id, cause);
        let mut out: Option<Value> = None;
        let mut interrupted = false;
        let mut child_failure: Option<AgentError> = None;
        // Long-running FunctionCall ids not yet answered by a FunctionResponse (WithRaiseOnWait).
        let mut pending_long_running: HashSet<String> = HashSet::new();
        let mut stream = child.run(child_ctx.clone(), input);
        while let Some(item) = stream.next().await {
            let mut ev = match item {
                Err(e) => {
                    // A child error wins over any prior interrupt.
                    child_failure = Some(e);
                    break;
                }
                Ok(ev) => ev,
            };
            // Stamp the path so the top scheduler scopes the child's output to the child; a
            // nested dynamic node that already set its own path keeps it.
            let info = ev.node_info.get_or_insert_with(NodeInfo::default);
            if info.path.is_empty() {
                info.path = child_path.to_string();
            }
            let ev_path = info.path.clone();
            if !child_scope.is_empty() && ev.isolation_scope.is_empty() {
                ev.isolation_scope = child_scope.to_string();
            }
            if ev.requested_input.is_some() {
                interrupted = true;
            }
            if opts.raise_on_wait {
                if !ev.long_running_tool_ids.is_empty() {
                    for fc in function_calls(ev.response.content.as_ref()) {
                        if !fc.id.is_empty() && ev.long_running_tool_ids.contains(&fc.id) {
                            pending_long_running.insert(fc.id.clone());
                        }
                    }
                }
                if !pending_long_running.is_empty() {
                    for fr in function_responses(ev.response.content.as_ref()) {
                        pending_long_running.remove(&fr.id);
                    }
                }
            }
            let mut child_out = child_event_output(&ev);
            let mut delegated = false;
            if child_out.is_none()
                && ev_path == child_path
                && let Some(v) = completed_delegated_output(&ev)
            {
                delegated = true;
                child_out = (!v.is_null()).then_some(v);
            }
            if let Some(co) = child_out {
                let validated = match validate_and_stamp_output(child.as_ref(), co, &mut ev) {
                    Ok(v) => v,
                    // The validation error itself is the NodeRunError cause.
                    Err(e) => return (Err(nre(to_workflow_error(e))), None, false),
                };
                out = Some(validated.clone());
                if delegated {
                    ev.response
                        .custom_metadata
                        .insert(WORKFLOW_DELEGATED_OUTPUT_KEY.to_string(), validated);
                } else if let Some(info) = ev.node_info.as_mut()
                    && info.output_for.is_empty()
                {
                    // The emitter's own path plus, under delegation, this parent and its
                    // ancestors; a nested child that already stamped its chain keeps it.
                    let mut output_for = vec![info.path.clone()];
                    if opts.use_as_output {
                        output_for.push(self.parent_path.clone());
                        output_for.extend(self.output_for_ancestors.iter().cloned());
                    }
                    info.output_for = output_for;
                }
            }
            if let Err(e) = (self.emit_up)(ev).await {
                let control_flow = is_node_control_flow(&e);
                return (
                    Err(nre(node_failed(format!("emitUp: {e}"), e))),
                    Some(control_flow),
                    false,
                );
            }
        }
        // The iterator must finish unwinding before its failure evidence is trusted: a cleanup
        // panic supersedes the error it yielded earlier.
        drop(stream);
        if let Some(f) = child_failure {
            let control_flow = is_node_control_flow(&f);
            return (
                Err(nre(node_failed(f.to_string(), f))),
                Some(control_flow),
                true,
            );
        }
        if child_ctx.cancellation_token().is_cancelled() {
            let cause = AgentError::Cancelled;
            return (Err(nre(node_failed(cause.to_string(), cause))), None, false);
        }
        if opts.raise_on_wait && !pending_long_running.is_empty() {
            interrupted = true;
        }
        // HITL (and raise-on-wait): not cached, so resume re-runs and re-invokes RunNode.
        if interrupted {
            return (
                Err(nre(WorkflowError::sentinel(
                    WorkflowErrorKind::NodeInterrupted,
                ))),
                None,
                false,
            );
        }
        // A WaitForOutput child that produced no output is not done: park the parent.
        if out.is_none() && waits_for_output(child.as_ref()) {
            return (
                Err(nre(WorkflowError::sentinel(
                    WorkflowErrorKind::NodeWaitingForOutput,
                ))),
                None,
                false,
            );
        }
        let out = out.unwrap_or(Value::Null);
        self.commit_delegation(child_path, out.clone());
        (Ok(out), None, false)
    }

    /// `Some(outcome)` (cached, shared from the in-flight leader, or the
    /// parent's cancellation) or `None` when this caller now leads.
    pub(crate) async fn await_or_lead(
        &self,
        child_path: &str,
    ) -> Option<Result<Value, AgentError>> {
        let mut rx = {
            let mut st = self.inner.lock();
            if let Some(out) = st.result_by_path.get(child_path) {
                return Some(Ok(out.clone()));
            }
            match st.inflight_by_path.get(child_path) {
                Some(leader) => leader.tx.subscribe(),
                None => {
                    let (tx, _rx) = watch::channel(None);
                    st.inflight_by_path
                        .insert(child_path.to_string(), Arc::new(InflightRun { tx }));
                    return None;
                }
            }
        };
        let token = self.parent_ctx.cancellation_token().clone();
        tokio::select! {
            r = rx.wait_for(Option::is_some) => Some(match r.ok().and_then(|v| (*v).clone()) {
                Some(Ok(v)) => Ok(v),
                Some(Err(e)) => Err(e.into()),
                None => Err(AgentError::Cancelled),
            }),
            () = token.cancelled() => Some(Err(AgentError::Cancelled)),
        }
    }

    /// Publishes `res` to the waiters, caches a success, and tracks the
    /// failed paths.
    pub(crate) fn finish_run(
        &self,
        child_path: &str,
        res: RunResult,
        failure_recorded: bool,
        inventory: &FailureInventory,
    ) {
        let leader = {
            let mut st = self.inner.lock();
            let leader = st.inflight_by_path.remove(child_path);
            let sub_prefix = format!("{child_path}/");
            let clear = |st: &mut DynState| {
                st.failed_paths
                    .retain(|p| !(p == child_path || p.starts_with(&sub_prefix)))
            };
            match &res {
                Ok(v) => {
                    st.result_by_path.insert(child_path.to_string(), v.clone());
                    clear(&mut st);
                }
                Err(e) if !e.is(WorkflowErrorKind::NodeInterrupted) => {
                    if failure_recorded && inventory.known {
                        // The child's marker already invalidates its own result: keep its
                        // latest inventory.
                        clear(&mut st);
                        st.failed_paths.extend(inventory.paths.iter().cloned());
                    } else {
                        // Without a recorded child outcome its partial result may remain in
                        // history: the conservative fallback.
                        st.failed_paths.insert(child_path.to_string());
                    }
                }
                Err(_) => {}
            }
            leader
        };
        if let Some(l) = leader {
            l.tx.send_replace(Some(res));
        }
    }

    /// A known inventory of the failed and still in-flight child paths (sorted for
    /// determinism).
    pub(crate) fn child_failures(&self) -> FailureInventory {
        let st = self.inner.lock();
        let mut paths: Vec<String> = st
            .failed_paths
            .iter()
            .chain(st.inflight_by_path.keys())
            .cloned()
            .collect();
        paths.sort();
        FailureInventory { known: true, paths }
    }

    /// Reserves the output delegation when `use_as_output`.
    fn claim_delegation(
        &self,
        child_path: &str,
        child_name: &str,
        use_as_output: bool,
    ) -> Result<(), AgentError> {
        if !use_as_output {
            return Ok(());
        }
        let mut st = self.inner.lock();
        st.delegation
            .reserve(child_path, child_name)
            .map_err(|existing| {
                let cause = WorkflowError::new(
                    WorkflowErrorKind::OutputAlreadyDelegated,
                    format!(
                        "{}: {} already delegates to {existing}",
                        WorkflowError::sentinel_text(WorkflowErrorKind::OutputAlreadyDelegated),
                        self.parent_path
                    ),
                );
                node_run_error(child_name, child_path, "", cause).into()
            })
    }

    /// Commits the delegated value of the child at `child_path`.
    fn commit_delegation(&self, child_path: &str, value: Value) {
        self.inner.lock().delegation.commit(child_path, value);
    }

    /// A validated custom id, or the next per-child counter value.
    fn resolve_run_id(&self, child_name: &str, custom: &str) -> Result<String, WorkflowError> {
        if !custom.is_empty() {
            validate_custom_run_id(custom)?;
            return Ok(custom.to_string());
        }
        let mut st = self.inner.lock();
        let n = st
            .run_count_by_child
            .entry(child_name.to_string())
            .or_insert(0);
        *n += 1;
        Ok(n.to_string())
    }
}

#[async_trait::async_trait]
impl DynamicSubScheduler for DynSubScheduler {
    /// Runs a child: `node` is an `Arc<dyn Node>` and `opts` a
    /// [`RunNodeOptions`] (boxed as `Any` by [`run_node`](crate::workflow::run_node)).
    async fn run_node(
        &self,
        node: Arc<dyn Any + Send + Sync>,
        input: Value,
        opts: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Result<Value, AgentError> {
        let Ok(child) = node.downcast::<Arc<dyn Node>>() else {
            return Err(AgentError::Config(
                "got child of a non-Node type, want Node".into(),
            ));
        };
        let opts = match opts {
            None => RunNodeOptions::default(),
            Some(o) => match o.downcast::<RunNodeOptions>() {
                Ok(o) => (*o).clone(),
                Err(_) => {
                    return Err(AgentError::Config(
                        "got opts of a non-RunNodeOptions type, want RunNodeOptions".into(),
                    ));
                }
            },
        };
        self.run_child(Arc::clone(&*child), input, opts).await
    }
    fn parent_path(&self) -> String {
        self.parent_path.clone()
    }
    fn output_for_ancestors(&self) -> Vec<String> {
        self.output_for_ancestors.clone()
    }
    fn delegated_output(&self) -> Option<Value> {
        self.inner.lock().delegation.value.clone()
    }
    fn resolve_by_run_id(&self, child_name: &str, custom: &str) -> Result<String, AgentError> {
        self.resolve_run_id(child_name, custom)
            .map_err(AgentError::from)
    }
}
