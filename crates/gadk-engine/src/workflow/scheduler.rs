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

//! Port of Go workflow/scheduler.go (plus node_span.go and the childEventOutput/messageText
//! helpers of agent_node.go): the static-graph scheduler.
//!
//! Go runs one goroutine per activation and a single consumer loop on the caller's goroutine.
//! Here each activation is a task in the scheduler's [`JoinSet`], and the consumer loop is the
//! pull state machine [`Scheduler::next`], driven by the workflow's [`EventStream`]: the
//! consumer polling again is Go's `yield` returning true, and dropping the stream (which drops
//! the scheduler, aborting every task and cancelling every per-node token) is Go's `yield`
//! returning false.
//!
//! [`EventStream`]: gadk_agent::EventStream

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{FutureExt, StreamExt};
use gadk_agent::{AgentError, CancelGuard, Context, ContextDelta};
use gadk_genai::Content;
use gadk_session::{Event, NodeInfo};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinSet};
use tracing::Instrument;

use crate::error::{WorkflowError, WorkflowErrorKind, workflow_error};
use crate::tool::utils::panic_message;
use crate::workflow::base_node::validate_and_stamp_output;
use crate::workflow::branch::{common_branch_prefix, derive_sub_branch};
use crate::workflow::failure_inventory::{self, FailureInventory, record_failed_child_paths};
use crate::workflow::graph::Graph;
use crate::workflow::persistence::{
    WORKFLOW_BASE_PATH_PREFIX, WORKFLOW_DELEGATED_OUTPUT_KEY, WORKFLOW_NODE_CANCELLED_OUTCOME,
    WORKFLOW_NODE_COMPLETED_KEY, WORKFLOW_NODE_FAILURE_OUTCOME, WORKFLOW_NODE_OUTCOME_KEY,
    completed_delegated_output,
};
use crate::workflow::route::{go_quote, is_default_route};
use crate::workflow::state::{NodeStatus, RunState};
use crate::workflow::{JoinNode, Node, START, calculate_delay, should_retry};

/// Port of Go defaultEventQueueCapacity.
pub(crate) const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 16;

// ---------- identity, error and output helpers ----------

/// Identity of a node (Go compares Node interface values by pointer): its data address.
pub(crate) fn node_key(n: &Arc<dyn Node>) -> usize {
    Arc::as_ptr(n).cast::<()>() as usize
}

/// Go `n == Start`.
pub(crate) fn is_start(n: &Arc<dyn Node>) -> bool {
    node_key(n) == node_key(&START)
}

/// Go `_, ok := n.(*JoinNode)`.
pub(crate) fn is_join(n: &Arc<dyn Node>) -> bool {
    n.as_any().is::<JoinNode>()
}

/// Go `context.DeadlineExceeded` for a node's own timeout: `AgentError::DeadlineExceeded`.
pub(crate) fn deadline_exceeded() -> AgentError {
    AgentError::DeadlineExceeded
}

/// Go `errors.Is` walk: `err` itself, then the WorkflowError source chain.
pub(crate) fn chain_any(err: &AgentError, pred: &dyn Fn(&AgentError) -> bool) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if pred(e) {
            return true;
        }
        cur = workflow_error(e)
            .and_then(|w| std::error::Error::source(w))
            .and_then(|s| s.downcast_ref::<AgentError>());
    }
    false
}

/// Go `errors.Is(err, context.Canceled)`.
pub(crate) fn is_cancelled(err: &AgentError) -> bool {
    chain_any(err, &|e| matches!(e, AgentError::Cancelled))
}

/// Go `errors.Is(err, context.DeadlineExceeded)`.
pub(crate) fn is_deadline_exceeded(err: &AgentError) -> bool {
    chain_any(err, &|e| matches!(e, AgentError::DeadlineExceeded))
}

/// Go `errors.Is(err, ErrX)` for workflow sentinels.
pub(crate) fn is_kind(err: &AgentError, kind: WorkflowErrorKind) -> bool {
    workflow_error(err).is_some_and(|w| w.is(kind))
}

/// Go `fmt.Errorf("%w<detail>", ErrX)`: the sentinel text immediately followed by `detail`.
pub(crate) fn wf_error(kind: WorkflowErrorKind, detail: impl std::fmt::Display) -> AgentError {
    WorkflowError::new(
        kind,
        format!("{}{detail}", WorkflowError::sentinel_text(kind)),
    )
    .into()
}

/// Port of Go messageText (agent_node.go): the non-thought model text; `None` without model
/// content.
pub(crate) fn message_text(ev: &Event) -> Option<String> {
    let c = ev.response.content.as_ref().filter(|c| c.role == "model")?;
    Some(
        c.parts
            .iter()
            .filter(|p| !p.text.is_empty() && !p.thought)
            .map(|p| p.text.as_str())
            .collect(),
    )
}

/// Port of Go childEventOutput (agent_node.go): the output, or the model text when the event is
/// MessageAsOutput. A `Value::Null` output is Go's nil (no output).
pub(crate) fn child_event_output(ev: &Event) -> Option<Value> {
    if let Some(v) = ev.output.as_ref().filter(|v| !v.is_null()) {
        return Some(v.clone());
    }
    if ev.node_info.as_ref().is_some_and(|i| i.message_as_output) {
        return message_text(ev).map(Value::String);
    }
    None
}

/// Port of Go defaultContentRole: a FunctionResponse part is app-authored (`"user"`);
/// everything else is model-authored.
pub(crate) fn default_content_role(c: &Content) -> &'static str {
    if c.parts.iter().any(|p| p.function_response.is_some()) {
        "user"
    } else {
        "model"
    }
}

// ---------- node_span.go ----------

/// Port of Go startNodeSpan: `None` (Go's noop span) for START and for nodes that emit their
/// own span.
pub(crate) fn start_node_span(ctx: &Context, n: &Arc<dyn Node>) -> Option<tracing::Span> {
    if is_start(n) || n.config().emits_own_span {
        return None;
    }
    Some(crate::telemetry::node_span(n.name(), &ctx.path()))
}

/// Port of Go isNodeControlFlow: cancellation or a HITL / wait-for-output pause.
pub(crate) fn is_node_control_flow(err: &AgentError) -> bool {
    is_cancelled(err) || is_kind(err, WorkflowErrorKind::NodeInterrupted)
}

/// Port of Go nodeSpan.recordError. `control_flow` is the classification of Go's `classifyErr`
/// when the caller has one; `None` classifies on `err`.
pub(crate) fn record_node_error(
    span: Option<&tracing::Span>,
    err: Option<&AgentError>,
    control_flow: Option<bool>,
) {
    let (Some(span), Some(err)) = (span, err) else {
        return;
    };
    if control_flow.unwrap_or_else(|| is_node_control_flow(err)) {
        return;
    }
    span.record("otel.status_code", "ERROR");
    span.record("error.message", tracing::field::display(err));
}

// ---------- queue items and per-activation accumulator ----------

/// Port of Go queueItem (a sealed interface; an enum here).
pub(crate) enum QueueItem {
    /// Go eventItem (boxed: an event is large).
    Event(Box<EventItem>),
    /// Go completionItem.
    Completion(CompletionItem),
    /// Go retryItem.
    Retry(RetryItem),
}

/// Port of Go eventItem. `processed` is the back-pressure handshake (`None` for partial
/// events): the consumer fires it once the event is yielded and the consumer asked for more.
pub(crate) struct EventItem {
    pub(crate) node_name: String,
    pub(crate) ev: Event,
    pub(crate) processed: Option<oneshot::Sender<()>>,
}

/// Port of Go completionItem: exactly one per activation.
pub(crate) struct CompletionItem {
    pub(crate) node_name: String,
    pub(crate) err: Option<AgentError>,
    pub(crate) inventory: FailureInventory,
}

/// Port of Go retryItem.
pub(crate) struct RetryItem {
    pub(crate) node: Arc<dyn Node>,
    pub(crate) input: Option<Value>,
    pub(crate) triggered_by: String,
    pub(crate) branch: String,
}

/// Port of Go nodeRun: the per-activation accumulator.
#[derive(Default)]
pub(crate) struct NodeRun {
    routing_event: Option<Event>,
    output: Option<Value>,
    has_output: bool,
    err: Option<AgentError>,
    branch: String,
    pub(crate) node_path: String,
    /// Insertion-ordered set (a Go map; deterministic here).
    interrupt_ids: Vec<String>,
}

impl NodeRun {
    /// An accumulator for an activation at `node_path`.
    pub(crate) fn with_path(node_path: &str) -> Self {
        NodeRun {
            node_path: node_path.to_string(),
            ..NodeRun::default()
        }
    }

    /// Port of Go nodeRun.recordErr: keeps the first error.
    pub(crate) fn record_err(&mut self, err: AgentError) {
        if self.err.is_none() {
            self.err = Some(err);
        }
    }

    /// Port of Go nodeRun.setRoutingEvent.
    fn set_routing_event(&mut self, ev: Event, node_name: &str) {
        if self.routing_event.is_some() {
            self.record_err(wf_error(
                WorkflowErrorKind::MultipleRoutingEvents,
                format!(": node {}", go_quote(node_name)),
            ));
            return;
        }
        self.routing_event = Some(ev);
    }

    /// Port of Go nodeRun.trackInterrupts.
    fn track_interrupts(&mut self, ev: &Event) {
        for id in ev.long_running_tool_ids.iter().filter(|id| !id.is_empty()) {
            if !self.interrupt_ids.contains(id) {
                self.interrupt_ids.push(id.clone());
            }
        }
    }

    /// Port of Go nodeRun.setOutput.
    fn set_output(&mut self, out: Value, node_name: &str) {
        if self.has_output {
            self.record_err(wf_error(
                WorkflowErrorKind::MultipleOutputs,
                format!(": node {}", go_quote(node_name)),
            ));
            return;
        }
        self.output = Some(out);
        self.has_output = true;
    }
}

/// Port of Go pendingActivation.
struct PendingActivation {
    node: Arc<dyn Node>,
    input: Option<Value>,
    triggered_by: String,
    branch: String,
    resume_inputs: Option<Map<String, Value>>,
}

/// Port of Go successor: the per-target dispatch tuple produced by [`find_successors`].
pub(crate) struct Successor {
    pub(crate) node: Arc<dyn Node>,
    pub(crate) input: Option<Value>,
    pub(crate) triggered_by: String,
    pub(crate) branch: String,
}

/// The locals of Go's `run`, kept across pulls.
#[derive(Default)]
struct LoopState {
    pending_err: Option<AgentError>,
    pending_inventory: FailureInventory,
    cancel_err: Option<AgentError>,
    consumer_gone: bool,
    draining: bool,
    /// Go sets `doneChan = nil` after it fires once.
    parent_done_seen: bool,
    /// The handshake of the event most recently handed to the consumer.
    ack: Option<oneshot::Sender<()>>,
    outbox: VecDeque<Result<Event, AgentError>>,
    finished: bool,
}

/// Port of Go scheduler. Owned by the stream that drives it; dropping it aborts every node task
/// and retry timer (the [`JoinSet`]) and cancels every per-node token (the [`CancelGuard`]s).
pub(crate) struct Scheduler {
    pub(crate) state: RunState,
    pub(crate) graph: Arc<Graph>,
    pub(crate) nodes_by_name: HashMap<String, Arc<dyn Node>>,
    terminals: HashSet<String>,
    pub(crate) runs_by_name: HashMap<String, NodeRun>,
    run_cancels: HashMap<String, CancelGuard>,
    pub(crate) retry_timers: HashMap<String, AbortHandle>,
    pub(crate) tx: mpsc::Sender<QueueItem>,
    rx: mpsc::Receiver<QueueItem>,
    tasks: JoinSet<()>,
    pub(crate) parent_ctx: Context,
    pub(crate) workflow_name: String,
    max_concurrency: usize,
    pending_queue: VecDeque<PendingActivation>,
    lp: LoopState,
}

/// Port of Go buildNodesByName: the name → node lookup of every edge endpoint.
pub(crate) fn build_nodes_by_name(g: &Graph) -> HashMap<String, Arc<dyn Node>> {
    let mut m = HashMap::new();
    for e in g.all_edges() {
        m.insert(e.from.name().to_string(), Arc::clone(&e.from));
        m.insert(e.to.name().to_string(), Arc::clone(&e.to));
    }
    m
}

impl Scheduler {
    /// Port of Go newScheduler. Must be called on a tokio runtime (the workflow stream creates
    /// it on its first poll, as Go creates it inside the iterator function).
    pub(crate) fn new(parent: Context, graph: Arc<Graph>, max_concurrency: usize) -> Self {
        let (tx, rx) = mpsc::channel(DEFAULT_EVENT_QUEUE_CAPACITY);
        Scheduler {
            state: RunState::default(),
            nodes_by_name: build_nodes_by_name(&graph),
            terminals: graph.terminal_node_names(),
            graph,
            runs_by_name: HashMap::new(),
            run_cancels: HashMap::new(),
            retry_timers: HashMap::new(),
            tx,
            rx,
            tasks: JoinSet::new(),
            parent_ctx: parent,
            workflow_name: String::new(),
            max_concurrency,
            pending_queue: VecDeque::new(),
            lp: LoopState::default(),
        }
    }

    /// Port of Go scheduleNode.
    pub(crate) fn schedule_node(
        &mut self,
        n: Arc<dyn Node>,
        input: Option<Value>,
        triggered_by: String,
        branch: String,
    ) {
        self.schedule_resumed_node(n, input, triggered_by, branch, None);
    }

    /// Port of Go atConcurrencyLimit.
    fn at_concurrency_limit(&self) -> bool {
        self.max_concurrency > 0 && self.runs_by_name.len() >= self.max_concurrency
    }

    /// Port of Go tryDispatchPending: FIFO over the pending queue while the cap allows.
    fn try_dispatch_pending(&mut self) {
        while !self.at_concurrency_limit() {
            let Some(next) = self.pending_queue.pop_front() else {
                break;
            };
            self.start_node(
                next.node,
                next.input,
                next.triggered_by,
                next.branch,
                next.resume_inputs,
            );
        }
    }

    /// Port of Go scheduleResumedNode: starts the activation, or queues it (NodePending) when
    /// the concurrency cap is reached.
    pub(crate) fn schedule_resumed_node(
        &mut self,
        n: Arc<dyn Node>,
        input: Option<Value>,
        triggered_by: String,
        branch: String,
        resume_inputs: Option<Map<String, Value>>,
    ) {
        if self.at_concurrency_limit() {
            let ns = self.state.ensure_node(n.name());
            ns.status = NodeStatus::Pending;
            ns.input = input.clone();
            ns.triggered_by = triggered_by.clone();
            ns.branch = branch.clone();
            self.pending_queue.push_back(PendingActivation {
                node: n,
                input,
                triggered_by,
                branch,
                resume_inputs,
            });
            return;
        }
        self.start_node(n, input, triggered_by, branch, resume_inputs);
    }

    /// Port of Go startNode: the per-node context, the bookkeeping, and the runner task.
    fn start_node(
        &mut self,
        n: Arc<dyn Node>,
        input: Option<Value>,
        triggered_by: String,
        branch: String,
        resume_inputs: Option<Map<String, Value>>,
    ) {
        let name = n.name().to_string();
        let parent_path = self.parent_ctx.path();
        let node_path = if !parent_path.is_empty() {
            format!("{parent_path}/{name}@1")
        } else if self.graph.is_root_wrapper {
            String::new()
        } else {
            format!("{name}@1")
        };
        let ofa = self.terminal_ancestors(&name);
        let per_node = self.parent_ctx.with_delta(ContextDelta {
            resumed_inputs: Some(Arc::new(resume_inputs.unwrap_or_default())),
            branch: Some(branch.clone()),
            path: Some(node_path.clone()),
            run_id: Some("1".to_string()),
            sub_scheduler: Some(None),
            output_for_ancestors: Some(ofa),
            ..ContextDelta::default()
        });
        let (per_node, guard) = per_node.with_agent_cancel();
        let ns = self.state.ensure_node(&name);
        ns.status = NodeStatus::Running;
        ns.output = None;
        ns.input = input.clone();
        ns.triggered_by = triggered_by;
        ns.branch = branch.clone();
        self.runs_by_name.insert(
            name.clone(),
            NodeRun {
                branch,
                node_path,
                ..NodeRun::default()
            },
        );
        self.run_cancels.insert(name.clone(), guard);
        // Go `cfg.Timeout > 0`: a zero timeout means "no timeout".
        let timeout = n.config().timeout.filter(|d| !d.is_zero());
        // `.in_current_span()`: a spawned task starts with no span, so without it node/agent
        // spans opened inside the task would lose the runner/workflow span as their parent (Go
        // passes the span through ctx to the goroutine).
        self.tasks.spawn(
            run_node_task(self.tx.clone(), name, n, per_node, input, timeout).in_current_span(),
        );
    }

    /// Port of Go scheduleRetry (time.AfterFunc → an abortable spawned sleep).
    fn schedule_retry(
        &mut self,
        n: Arc<dyn Node>,
        input: Option<Value>,
        triggered_by: String,
        branch: String,
        delay: Duration,
    ) {
        let tx = self.tx.clone();
        let token = self.parent_ctx.cancellation_token().clone();
        let name = n.name().to_string();
        // in_current_span: keep the workflow span as the parent of the timer task (see above).
        let handle = self.tasks.spawn(
            async move {
                tokio::time::sleep(delay).await;
                let item = QueueItem::Retry(RetryItem {
                    node: n,
                    input,
                    triggered_by,
                    branch,
                });
                tokio::select! {
                    _ = tx.send(item) => {}
                    () = token.cancelled() => {}
                }
            }
            .in_current_span(),
        );
        self.retry_timers.insert(name, handle);
    }

    /// Port of Go cancelAll: cancels every running activation, stops every retry timer and
    /// drops the pending queue (their NodeState stays NodePending).
    pub(crate) fn cancel_all(&mut self) {
        for guard in self.run_cancels.values() {
            guard.cancel();
        }
        for (_, h) in self.retry_timers.drain() {
            h.abort();
        }
        self.pending_queue.clear();
    }

    /// Port of Go `(*scheduler).run(yield, false)` as a pull state machine: each call resumes
    /// the loop until one item is ready for the consumer (Go's `yield`). `None` means the loop
    /// returned. Calling `next` again is Go's `yield` returning true, so the previous event's
    /// producer handshake is released first.
    pub(crate) async fn next(&mut self) -> Option<Result<Event, AgentError>> {
        if let Some(ack) = self.lp.ack.take() {
            let _ = ack.send(());
        }
        loop {
            if let Some(item) = self.lp.outbox.pop_front() {
                return Some(item);
            }
            if self.lp.finished {
                return None;
            }
            if self.runs_by_name.is_empty() && self.retry_timers.is_empty() {
                self.finish();
                continue;
            }
            while self.tasks.try_join_next().is_some() {}
            let token = self.parent_ctx.cancellation_token().clone();
            let item = tokio::select! {
                biased;
                it = self.rx.recv() => it,
                () = token.cancelled(), if !self.lp.parent_done_seen => {
                    self.lp.parent_done_seen = true;
                    None
                }
            };
            // External cancellation is recorded and starts draining whichever case won the
            // select (Go lines 576-591): a queued completion can be picked ahead of it.
            if self.lp.cancel_err.is_none() && token.is_cancelled() {
                self.lp.cancel_err = Some(AgentError::Cancelled);
                if !self.lp.draining {
                    self.lp.draining = true;
                    self.cancel_all();
                }
            }
            match item {
                Some(QueueItem::Event(mut it)) => {
                    self.handle_event(&mut it);
                    let processed = it.processed.take();
                    if !self.lp.draining && !self.lp.consumer_gone {
                        self.lp.outbox.push_back(Ok(it.ev));
                        // Released on the next pull (Go: after yield returned true).
                        self.lp.ack = processed;
                    } else if let Some(p) = processed {
                        // Always release, even when draining, so a blocked producer finishes.
                        let _ = p.send(());
                    }
                }
                Some(QueueItem::Completion(mut it)) => {
                    let nr = self.runs_by_name.remove(&it.node_name);
                    let nr_path = nr.as_ref().map(|r| r.node_path.clone());
                    let had_err = it.err.is_some();
                    let inventory = std::mem::take(&mut it.inventory);
                    let schedule_new_work = !self.lp.draining;
                    let err =
                        self.handle_completion(&it.node_name, it.err.take(), nr, schedule_new_work);
                    if !self.lp.consumer_gone
                        && let Some(ev) =
                            self.completion_event(&it.node_name, nr_path.as_deref(), &inventory)
                    {
                        self.lp.outbox.push_back(Ok(ev));
                    }
                    if let Some(err) = err
                        && self.lp.pending_err.is_none()
                    {
                        self.lp.pending_err = Some(err);
                        if had_err {
                            self.lp.pending_inventory = inventory;
                        }
                        if !self.lp.draining {
                            self.lp.draining = true;
                            self.cancel_all();
                        }
                    }
                    // A slot just freed up: promote queued activations.
                    if !self.lp.draining {
                        self.try_dispatch_pending();
                    }
                }
                Some(QueueItem::Retry(it)) => {
                    self.retry_timers.remove(it.node.name());
                    if !self.lp.draining {
                        self.schedule_node(it.node, it.input, it.triggered_by, it.branch);
                    }
                }
                None => {}
            }
        }
    }

    /// Go `run(nil, true)`: the consumer already left; never yields, never finalizes.
    pub(crate) async fn drain(&mut self) {
        self.lp.consumer_gone = true;
        self.lp.draining = true;
        while self.next().await.is_some() {}
    }

    /// Go run's epilogue: surface the first error (a node's own error outranks the cancellation
    /// cause), or finalize a normally completed run.
    fn finish(&mut self) {
        self.lp.finished = true;
        if let Some(err) = self
            .lp
            .pending_err
            .take()
            .or_else(|| self.lp.cancel_err.take())
        {
            if !self.lp.consumer_gone {
                // A WorkflowNode forwards this error unchanged; bridge its recovery evidence to
                // the enclosing activation without wrapping the error.
                failure_inventory::publish_failure_inventory(
                    failure_inventory::graph_key(&self.graph),
                    &self.lp.pending_inventory,
                );
                self.lp.outbox.push_back(Err(err));
            }
            return;
        }
        // Skip finalize when draining: the run did not complete normally.
        if !self.lp.draining
            && let Err(e) = self.finalize()
        {
            self.lp.outbox.push_back(Err(e));
        }
    }

    /// Port of Go terminalAncestors: the ancestor paths a terminal node's output also counts
    /// for; empty for a non-terminal node or a workflow running at the root.
    fn terminal_ancestors(&self, node_name: &str) -> Vec<String> {
        let parent_path = self.parent_ctx.path();
        if !self.terminals.contains(node_name) || parent_path.is_empty() {
            return Vec::new();
        }
        std::iter::once(parent_path)
            .chain(self.parent_ctx.output_for_ancestors())
            .collect()
    }

    /// Port of Go handleEvent: updates the activation's accumulator and stamps the event (base
    /// path, branch, content role, node path, output attribution) before it is yielded.
    fn handle_event(&mut self, it: &mut EventItem) {
        let name = it.node_name.clone();
        let ancestors = self.terminal_ancestors(&name);
        let node = self.nodes_by_name.get(&name).cloned();
        let base_path_key = (!self.graph.is_root_wrapper)
            .then(|| format!("{WORKFLOW_BASE_PATH_PREFIX}{}", self.workflow_name));
        let parent_path = self.parent_ctx.path();
        // Defensive: the completion was already processed for this node.
        let Some(nr) = self.runs_by_name.get_mut(&name) else {
            return;
        };
        let ev = &mut it.ev;
        if let Some(key) = base_path_key {
            ev.response
                .custom_metadata
                .insert(key, Value::String(parent_path));
        }
        // Stamp the activation's branch onto events that left it empty.
        if ev.branch.is_empty() && !nr.branch.is_empty() {
            ev.branch = nr.branch.clone();
        }
        // Default Content.Role for nodes that left it empty, before the descendant
        // short-circuit so dynamic children are covered too.
        if let Some(c) = ev.response.content.as_mut()
            && c.role.is_empty()
        {
            c.role = default_content_role(c).to_string();
        }
        let expected_path = if nr.node_path.is_empty() {
            name.clone()
        } else {
            nr.node_path.clone()
        };
        let mut path = ev
            .node_info
            .as_ref()
            .map(|i| i.path.clone())
            .unwrap_or_default();
        let is_descendant = !path.is_empty() && path != expected_path && path != name;
        // Track interrupts before the descendant short-circuit so a dynamic child's pause is
        // promoted to the parent node.
        nr.track_interrupts(ev);
        if is_descendant {
            return;
        }
        if path.is_empty() {
            ev.node_info.get_or_insert_with(NodeInfo::default).path = expected_path.clone();
            path = expected_path;
        }
        let not_found = || {
            wf_error(
                WorkflowErrorKind::NodeFailed,
                format!(
                    ": output validation: node {} not found in graph",
                    go_quote(&name)
                ),
            )
        };
        if let Some(value) = completed_delegated_output(ev) {
            if value.is_null() {
                return;
            }
            // The dynamic body completed with a delegated (possibly cached) result.
            let Some(n) = node else {
                nr.record_err(not_found());
                return;
            };
            match validate_and_stamp_output(n.as_ref(), value, ev) {
                Err(e) => nr.record_err(e),
                Ok(validated) => {
                    ev.response
                        .custom_metadata
                        .insert(WORKFLOW_DELEGATED_OUTPUT_KEY.to_string(), validated.clone());
                    nr.set_output(validated, &name);
                }
            }
            return;
        }
        if !ev.routes.is_empty() {
            nr.set_routing_event(ev.clone(), &name);
        }
        if let Some(out) = child_event_output(ev) {
            let Some(n) = node else {
                // A miss means the registry is out of sync: fail rather than forward
                // unvalidated.
                nr.record_err(not_found());
                return;
            };
            let validated = match validate_and_stamp_output(n.as_ref(), out, ev) {
                Ok(v) => v,
                Err(e) => {
                    nr.record_err(e);
                    return;
                }
            };
            nr.set_output(validated, &name);
            let output_for = std::iter::once(path).chain(ancestors).collect();
            ev.node_info
                .get_or_insert_with(NodeInfo::default)
                .output_for = output_for;
        }
    }

    /// Port of Go handleCompletion: transitions the node's status and (when
    /// `schedule_new_work`) schedules a retry or the successors. Returns the node's own error;
    /// `None` on success, sibling cancellation or a pause.
    fn handle_completion(
        &mut self,
        name: &str,
        err: Option<AgentError>,
        mut nr: Option<NodeRun>,
        schedule_new_work: bool,
    ) -> Option<AgentError> {
        self.run_cancels.remove(name);
        // A dead invocation context: the cancellation came from outside the scheduler. A node
        // that failed for its own reason keeps that error (Go echoesCancellation; the parent's
        // cause is always Cancelled here, as tokens carry no cause).
        if self.parent_ctx.cancellation_token().is_cancelled() {
            let ns = self.state.ensure_node(name);
            if let Some(e) = err
                && !is_cancelled(&e)
            {
                ns.status = NodeStatus::Failed;
                return Some(e);
            }
            ns.status = NodeStatus::Cancelled;
            return None;
        }
        if let Some(e) = &err {
            if is_cancelled(e) {
                self.state.ensure_node(name).status = NodeStatus::Cancelled;
                return None; // sibling cancellation; not the original error
            }
            // A WaitForOutput park: a pause, not a failure, with no interrupt id.
            if is_kind(e, WorkflowErrorKind::NodeWaitingForOutput) {
                self.state.ensure_node(name).status = NodeStatus::Waiting;
                return None;
            }
        }
        if let Some(e) = err {
            let node = self.nodes_by_name.get(name).cloned();
            if schedule_new_work
                && let Some(n) = node
                && let Some(cfg) = n.config().retry_config.clone()
            {
                let ns = self.state.ensure_node(name);
                ns.attempt += 1;
                let attempt = ns.attempt;
                if should_retry(&cfg, &e, attempt) {
                    let delay = calculate_delay(&cfg, attempt, self.parent_ctx.platform());
                    let ns = self.state.ensure_node(name);
                    ns.status = NodeStatus::Pending;
                    let (input, triggered_by, branch) =
                        (ns.input.clone(), ns.triggered_by.clone(), ns.branch.clone());
                    self.schedule_retry(n, input, triggered_by, branch, delay);
                    // Successors are scheduled only when a retry eventually succeeds.
                    return None;
                }
            }
            self.state.ensure_node(name).status = NodeStatus::Failed;
            return Some(e);
        }
        if let Some(nr_err) = nr.as_mut().and_then(|r| r.err.take()) {
            self.state.ensure_node(name).status = NodeStatus::Failed;
            return Some(nr_err);
        }
        // An interrupt that survived the run parks the node, regardless of schedule_new_work.
        if let Some(r) = nr.as_ref()
            && !r.interrupt_ids.is_empty()
        {
            let ns = self.state.ensure_node(name);
            ns.status = NodeStatus::Waiting;
            ns.interrupts = r.interrupt_ids.clone();
            return None;
        }
        let ns = self.state.ensure_node(name);
        ns.status = NodeStatus::Completed;
        ns.attempt = 0;
        if let Some(r) = nr.as_ref()
            && r.has_output
        {
            ns.output = r.output.clone();
        }
        // Release the accumulated re-entry response history.
        ns.resumed_inputs = None;
        if !schedule_new_work {
            return None;
        }
        let current = self.nodes_by_name.get(name).cloned()?;
        let (mut input, routing_ev) = match nr {
            Some(r) => (r.output, r.routing_event),
            None => (None, None),
        };
        let ns = self.state.ensure_node(name);
        // START's own output is empty by definition; it propagates the workflow's seed input.
        if is_start(&current) {
            input = ns.input.clone();
        }
        let parent_branch = ns.branch.clone();
        let succs = find_successors(
            &self.graph,
            &self.state,
            &current,
            input,
            routing_ev.as_ref(),
            &parent_branch,
        );
        for s in succs {
            self.schedule_node(s.node, s.input, s.triggered_by, s.branch);
        }
        None
    }

    /// Port of Go completionEvent: a history record for an activation that failed, was
    /// cancelled, or completed without output (output events alone cannot tell these apart).
    pub(crate) fn completion_event(
        &self,
        name: &str,
        node_path: Option<&str>,
        inventory: &FailureInventory,
    ) -> Option<Event> {
        if name == START.name() || self.graph.is_root_wrapper {
            return None;
        }
        let ns = self.state.nodes.get(name)?;
        let node_path = node_path?;
        let outcome = match ns.status {
            NodeStatus::Failed | NodeStatus::Pending => Some(WORKFLOW_NODE_FAILURE_OUTCOME),
            NodeStatus::Cancelled => Some(WORKFLOW_NODE_CANCELLED_OUTCOME),
            NodeStatus::Completed if ns.output.is_none() => None,
            _ => return None,
        };
        let mut ev = Event::new(self.parent_ctx.platform(), self.parent_ctx.invocation_id());
        ev.branch = ns.branch.clone();
        let path = if node_path.is_empty() {
            name
        } else {
            node_path
        };
        ev.node_info = Some(NodeInfo {
            path: path.to_string(),
            ..NodeInfo::default()
        });
        let md = &mut ev.response.custom_metadata;
        md.insert(
            format!("{WORKFLOW_BASE_PATH_PREFIX}{}", self.workflow_name),
            Value::String(self.parent_ctx.path()),
        );
        match outcome {
            Some(o) => {
                // A failed attempt can still recover through retries or a parent fallback;
                // ErrorCode would terminate AgentTool/A2A consumers.
                md.insert(
                    WORKFLOW_NODE_OUTCOME_KEY.to_string(),
                    Value::String(o.to_string()),
                );
                record_failed_child_paths(md, inventory);
            }
            None => {
                md.insert(WORKFLOW_NODE_COMPLETED_KEY.to_string(), Value::Bool(true));
            }
        }
        Some(ev)
    }

    /// Port of Go finalize: errors when more than one terminal node produced output. A no-op
    /// while a node is waiting (the run has not finished).
    pub(crate) fn finalize(&self) -> Result<(), AgentError> {
        if self
            .state
            .nodes
            .values()
            .any(|ns| ns.status == NodeStatus::Waiting)
        {
            return Ok(());
        }
        let mut producers: Vec<&str> = self
            .terminals
            .iter()
            .filter(|name| {
                self.state
                    .nodes
                    .get(name.as_str())
                    .is_some_and(|ns| ns.status == NodeStatus::Completed && ns.output.is_some())
            })
            .map(String::as_str)
            .collect();
        if producers.len() > 1 {
            producers.sort_unstable();
            return Err(wf_error(
                WorkflowErrorKind::MultipleTerminalOutputs,
                format!(": {}", producers.join(", ")),
            ));
        }
        Ok(())
    }
}

/// Port of Go runNode: drives one activation and always ends with exactly one
/// [`CompletionItem`]. A panic in the node is reported as the completion's error.
pub(crate) async fn run_node_task(
    out: mpsc::Sender<QueueItem>,
    name: String,
    node: Arc<dyn Node>,
    ctx: Context,
    input: Option<Value>,
    timeout: Option<Duration>,
) {
    let span = start_node_span(&ctx, &node);
    let body = drive_activation(out.clone(), name.clone(), node, ctx, input, timeout);
    let body = match &span {
        Some(s) => body.instrument(s.clone()).boxed(),
        None => body.boxed(),
    };
    let (err, inventory) = match AssertUnwindSafe(body).catch_unwind().await {
        Ok(r) => r,
        Err(panic) => (
            Some(AgentError::other(format!(
                "node {} panicked: {}",
                go_quote(&name),
                panic_message(&*panic)
            ))),
            FailureInventory::default(),
        ),
    };
    record_node_error(span.as_ref(), err.as_ref(), None);
    // Unconditional, as in Go: the consumer's bookkeeping relies on exactly one completion.
    let _ = out
        .send(QueueItem::Completion(CompletionItem {
            node_name: name,
            err,
            inventory,
        }))
        .await;
    drop(span); // Go: span.end() after the completion is queued
}

/// How an activation's event loop ended.
enum Outcome {
    Done,
    Ctx,
    NodeError(AgentError),
}

/// The body of Go runNode: input validation, then the node's events into the queue with the
/// back-pressure handshake, honoring the per-node token and timeout.
async fn drive_activation(
    out: mpsc::Sender<QueueItem>,
    name: String,
    node: Arc<dyn Node>,
    ctx: Context,
    input: Option<Value>,
    timeout: Option<Duration>,
) -> (Option<AgentError>, FailureInventory) {
    let validated = match node.validate_input(input.unwrap_or(Value::Null)) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!(
                "{} for node {}: {e}",
                WorkflowError::sentinel_text(WorkflowErrorKind::InputValidation),
                go_quote(&name)
            );
            let err =
                WorkflowError::new(WorkflowErrorKind::InputValidation, msg).with_source(e.into());
            return (Some(err.into()), FailureInventory::default());
        }
    };
    let recorder = failure_inventory::with_failure_inventory(&node);
    let token = ctx.cancellation_token().clone();
    let timed_out = Arc::new(AtomicBool::new(false));
    let drive = failure_inventory::scope(Arc::clone(&recorder), async {
        let mut stream = node.run(ctx.clone(), validated);
        while let Some(item) = stream.next().await {
            let ev = match item {
                Ok(ev) => ev,
                Err(e) => return Outcome::NodeError(e),
            };
            // Non-partial events block until the consumer has persisted them; partial events
            // are fire-and-forget.
            let (processed, wait) = if ev.response.partial {
                (None, None)
            } else {
                let (a, b) = oneshot::channel();
                (Some(a), Some(b))
            };
            tokio::select! {
                biased;
                sent = out.send(QueueItem::Event(Box::new(EventItem { node_name: name.clone(), ev, processed }))) => {
                    if sent.is_err() {
                        return Outcome::Ctx;
                    }
                }
                () = token.cancelled() => return Outcome::Ctx,
            }
            if let Some(wait) = wait {
                tokio::select! {
                    biased;
                    _ = wait => {}
                    () = token.cancelled() => return Outcome::Ctx,
                }
            }
        }
        // The node returned cleanly but its context died: it most likely observed it.
        if token.is_cancelled() {
            Outcome::Ctx
        } else {
            Outcome::Done
        }
        // `stream` drops here, inside catch_unwind: a cleanup panic supersedes the outcome.
    });
    let mut drive = std::pin::pin!(drive);
    let outcome = match timeout {
        None => drive.await,
        Some(d) => tokio::select! {
            o = &mut drive => o,
            () = tokio::time::sleep(d) => {
                timed_out.store(true, Ordering::SeqCst);
                token.cancel();
                drive.await
            }
        },
    };
    let ctx_err = || {
        if timed_out.load(Ordering::SeqCst) {
            deadline_exceeded()
        } else {
            AgentError::Cancelled
        }
    };
    match outcome {
        Outcome::Done => (None, FailureInventory::default()),
        Outcome::Ctx => (Some(ctx_err()), FailureInventory::default()),
        Outcome::NodeError(e) => {
            // A timed-out node that forwards its context's cancellation reports the deadline.
            let e = if timed_out.load(Ordering::SeqCst) && is_cancelled(&e) {
                deadline_exceeded()
            } else {
                e
            };
            (Some(e), recorder.snapshot())
        }
    }
}

/// Port of Go findSuccessors: routing, the Default fallback, de-duplication, and fan-out
/// sub-branches.
pub(crate) fn find_successors(
    g: &Graph,
    state: &RunState,
    current: &Arc<dyn Node>,
    input: Option<Value>,
    event: Option<&Event>,
    parent_branch: &str,
) -> Vec<Successor> {
    let succs = g.successors_of(current);
    if succs.is_empty() {
        return Vec::new();
    }
    let from = current.name().to_string();
    let mut concrete_matched = false;
    let mut out: Vec<Successor> = Vec::new();
    let mut added: HashSet<usize> = HashSet::new();
    let mut default_node: Option<Arc<dyn Node>> = None;
    for e in succs {
        let key = node_key(&e.to);
        if added.contains(&key) {
            continue;
        }
        match &e.route {
            None => {
                append_successor(
                    &mut out,
                    g,
                    state,
                    &e.to,
                    input.clone(),
                    &from,
                    parent_branch,
                );
                added.insert(key);
            }
            Some(r) if is_default_route(r) => default_node = Some(Arc::clone(&e.to)),
            Some(r) => {
                if event.is_some_and(|ev| r.matches(ev)) {
                    append_successor(
                        &mut out,
                        g,
                        state,
                        &e.to,
                        input.clone(),
                        &from,
                        parent_branch,
                    );
                    added.insert(key);
                    concrete_matched = true;
                }
            }
        }
    }
    if !concrete_matched && let Some(d) = default_node {
        append_successor(&mut out, g, state, &d, input, &from, parent_branch);
    }
    // A fan-out to more than one successor gives each non-join entry still on the inherited
    // branch its own sub-branch; joins keep their common-prefix branch.
    if out.len() > 1 {
        for s in &mut out {
            if is_join(&s.node) || s.branch != parent_branch {
                continue;
            }
            s.branch = derive_sub_branch(parent_branch, &format!("{}@1", s.node.name()));
        }
    }
    out
}

/// Port of Go appendSuccessor: a join is recorded only once every predecessor completed, with
/// the aggregated predecessor outputs as input and their common branch prefix as branch.
fn append_successor(
    out: &mut Vec<Successor>,
    g: &Graph,
    state: &RunState,
    target: &Arc<dyn Node>,
    input: Option<Value>,
    triggered_by: &str,
    parent_branch: &str,
) {
    if !is_join(target) {
        out.push(Successor {
            node: Arc::clone(target),
            input,
            triggered_by: triggered_by.to_string(),
            branch: parent_branch.to_string(),
        });
        return;
    }
    let Some(aggregated) = aggregate_predecessor_outputs(g, state, target) else {
        return;
    };
    let branch = common_branch_prefix(&aggregate_predecessor_branches(g, state, target));
    out.push(Successor {
        node: Arc::clone(target),
        input: Some(Value::Object(aggregated)),
        triggered_by: triggered_by.to_string(),
        branch,
    });
}

/// Port of Go aggregatePredecessorOutputs: `None` unless every predecessor is Completed; a
/// predecessor without output contributes `null`.
fn aggregate_predecessor_outputs(
    g: &Graph,
    state: &RunState,
    target: &Arc<dyn Node>,
) -> Option<Map<String, Value>> {
    let mut aggregated = Map::new();
    for e in g.predecessors_of(target) {
        let name = e.from.name();
        let ns = state
            .nodes
            .get(name)
            .filter(|ns| ns.status == NodeStatus::Completed)?;
        aggregated.insert(name.to_string(), ns.output.clone().unwrap_or(Value::Null));
    }
    Some(aggregated)
}

/// Port of Go aggregatePredecessorBranches: predecessor branches in predecessor-edge order.
fn aggregate_predecessor_branches(
    g: &Graph,
    state: &RunState,
    target: &Arc<dyn Node>,
) -> Vec<String> {
    g.predecessors_of(target)
        .iter()
        .map(|e| {
            state
                .nodes
                .get(e.from.name())
                .map(|ns| ns.branch.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// Shared fixtures for the ported `package workflow` tests (Go test helpers).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod test_support {
    use std::any::Any;
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::{DateTime, Utc};
    use futures::StreamExt;
    use gadk_agent::{AgentError, Context, EventStream, InvocationContext};
    use gadk_genai::{Content, Part};
    use gadk_platform::Platform;
    use gadk_session::{Event, NodeInfo, RequestInput, Session, SessionError, State};
    use parking_lot::Mutex;
    use serde_json::Value;

    use crate::error::{WorkflowError, WorkflowErrorKind, workflow_error};
    use crate::workflow::{
        BaseNode, Edge, Emit, Node, NodeConfig, Route, START, Workflow, WorkflowOptions,
    };

    /// Go `MockInvocationContext.InvocationID()`.
    pub(crate) const TEST_INVOCATION_ID: &str = "test-invocation-id";

    /// Port of Go newMockCtx: root branch, in-memory session, deterministic platform.
    pub(crate) fn mock_ctx() -> InvocationContext {
        InvocationContext::builder()
            .invocation_id(TEST_INVOCATION_ID)
            .platform(Platform::deterministic(1))
            .build_invocation()
    }

    /// Port of Go newSeededMockCtx: user content "seed".
    pub(crate) fn seeded_mock_ctx() -> InvocationContext {
        InvocationContext::builder()
            .invocation_id(TEST_INVOCATION_ID)
            .platform(Platform::deterministic(1))
            .user_content(Content {
                parts: vec![Part::from_text("seed")],
                ..Content::default()
            })
            .build_invocation()
    }

    /// Port of Go newTopLevelCtx / agent.Promote(newMockCtx(t)).
    pub(crate) fn top_level_ctx() -> Context {
        mock_ctx().into_context()
    }

    /// Port of Go `ctx.sess = &eventsSession{events}` / `fakeSession{events}`.
    pub(crate) fn ctx_with_events(events: Vec<Event>) -> Context {
        InvocationContext::builder()
            .invocation_id(TEST_INVOCATION_ID)
            .platform(Platform::deterministic(1))
            .session(Arc::new(EventsSession::new(events)))
            .build()
    }

    /// Port of Go mustNew: anonymous workflow.
    pub(crate) fn must_new(edges: Vec<Edge>) -> Arc<Workflow> {
        Workflow::new("", edges, WorkflowOptions::default()).expect("Workflow::new")
    }

    pub(crate) fn start() -> Arc<dyn Node> {
        Arc::clone(&START)
    }

    pub(crate) fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
        Edge {
            from: Arc::clone(from),
            to: Arc::clone(to),
            route: None,
        }
    }

    pub(crate) fn route_edge(
        from: &Arc<dyn Node>,
        to: &Arc<dyn Node>,
        route: Arc<dyn Route>,
    ) -> Edge {
        Edge {
            from: Arc::clone(from),
            to: Arc::clone(to),
            route: Some(route),
        }
    }

    /// Port of Go workflow.Chain: consecutive unconditional edges.
    pub(crate) fn chain(nodes: &[&Arc<dyn Node>]) -> Vec<Edge> {
        nodes.windows(2).map(|w| edge(w[0], w[1])).collect()
    }

    /// Port of Go drain: every event; the first error fails the test.
    pub(crate) async fn drain(s: EventStream) -> Vec<Event> {
        let (events, err) = collect(s).await;
        if let Some(err) = err {
            panic!("unexpected error: {err}");
        }
        events
    }

    /// Port of Go drainErr: the first error; fails the test if none.
    pub(crate) async fn drain_err(s: EventStream) -> AgentError {
        collect(s)
            .await
            .1
            .expect("expected an error from Run, got none")
    }

    /// Drains to the end like Go's `for ev, err := range seq`, keeping the first error.
    pub(crate) async fn collect(mut s: EventStream) -> (Vec<Event>, Option<AgentError>) {
        let mut events = Vec::new();
        let mut first = None;
        while let Some(item) = s.next().await {
            match item {
                Ok(ev) => events.push(ev),
                Err(e) => {
                    if first.is_none() {
                        first = Some(e);
                    }
                }
            }
        }
        (events, first)
    }

    /// Port of Go outputsOf: `fmt.Sprint(ev.Output)` of each output event, sorted.
    pub(crate) fn outputs_of(events: &[Event]) -> Vec<String> {
        let mut out: Vec<String> = events
            .iter()
            .filter_map(|ev| ev.output.as_ref())
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        out.sort();
        out
    }

    /// Go `errors.Is(err, ErrX)` for workflow sentinels.
    pub(crate) fn is_kind(err: &AgentError, kind: WorkflowErrorKind) -> bool {
        super::is_kind(err, kind)
    }

    /// Go `errors.Is(err, target)` / `errors.As` for a test-defined error type: walks
    /// WorkflowError sources and downcasts `AgentError::Other` payloads.
    pub(crate) fn find_cause<T: std::error::Error + 'static>(err: &AgentError) -> Option<&T> {
        let mut cur = Some(err);
        while let Some(e) = cur {
            if let AgentError::Other(b) = e
                && let Some(t) = b.downcast_ref::<T>()
            {
                return Some(t);
            }
            cur = workflow_error(e)
                .and_then(|w| std::error::Error::source(w))
                .and_then(|s| s.downcast_ref::<AgentError>());
        }
        None
    }

    /// A test-defined business error (Go `errors.New("...")` sentinels compared by identity).
    #[derive(Debug, thiserror::Error, PartialEq, Eq)]
    #[error("{0}")]
    pub(crate) struct TestError(pub(crate) &'static str);

    pub(crate) fn test_err(msg: &'static str) -> AgentError {
        AgentError::Other(Box::new(TestError(msg)))
    }

    pub(crate) fn once(ev: Event) -> EventStream {
        futures::stream::once(async move { Ok(ev) }).boxed()
    }

    pub(crate) fn fail(err: AgentError) -> EventStream {
        futures::stream::once(async move { Err(err) }).boxed()
    }

    pub(crate) fn output_event(ctx: &Context, out: Value) -> Event {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.output = Some(out);
        ev
    }

    type RunFn = Arc<dyn Fn(Context, Value) -> EventStream + Send + Sync>;

    /// A node whose `run` is a closure (replaces Go's per-test node structs).
    pub(crate) struct TestNode {
        base: BaseNode,
        run: RunFn,
    }

    impl TestNode {
        pub(crate) fn arc(
            name: &str,
            cfg: NodeConfig,
            run: impl Fn(Context, Value) -> EventStream + Send + Sync + 'static,
        ) -> Arc<dyn Node> {
            Arc::new(TestNode {
                base: BaseNode::new(name, "", cfg),
                run: Arc::new(run),
            })
        }

        pub(crate) fn with_schemas(
            name: &str,
            input: Option<Value>,
            output: Option<Value>,
            cfg: NodeConfig,
            run: impl Fn(Context, Value) -> EventStream + Send + Sync + 'static,
        ) -> Arc<dyn Node> {
            let base = BaseNode::with_schemas(name, "", input, output, cfg).expect("schemas");
            Arc::new(TestNode {
                base,
                run: Arc::new(run),
            })
        }
    }

    impl Node for TestNode {
        fn name(&self) -> &str {
            self.base.name()
        }
        fn description(&self) -> &str {
            self.base.description()
        }
        fn config(&self) -> &NodeConfig {
            self.base.config()
        }
        fn input_schema(&self) -> Option<&Value> {
            self.base.input_schema()
        }
        fn output_schema(&self) -> Option<&Value> {
            self.base.output_schema()
        }
        fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
            self.base.validate_input(v)
        }
        fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
            self.base.validate_output(v)
        }
        fn run(&self, ctx: Context, input: Value) -> EventStream {
            (self.run)(ctx, input)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Port of Go newDummyNode: a named node that emits nothing.
    pub(crate) fn dummy_node(name: &str) -> Arc<dyn Node> {
        TestNode::arc(name, NodeConfig::default(), |_, _| {
            futures::stream::empty().boxed()
        })
    }

    /// Port of a Go `chan struct{}` closed once (release / started signals).
    pub(crate) struct Gate(tokio::sync::watch::Sender<bool>);

    impl Gate {
        pub(crate) fn new() -> Arc<Gate> {
            Arc::new(Gate(tokio::sync::watch::channel(false).0))
        }
        pub(crate) fn release(&self) {
            self.0.send_replace(true);
        }
        pub(crate) async fn wait(&self) {
            let mut rx = self.0.subscribe();
            let _ = rx.wait_for(|v| *v).await;
        }
        pub(crate) fn is_released(&self) -> bool {
            *self.0.borrow()
        }
    }

    /// Port of Go newRecordingNode: waits for release, emits output "<input>:<name>".
    pub(crate) fn recording_node(name: &str) -> (Arc<dyn Node>, Arc<Gate>) {
        let gate = Gate::new();
        let g = Arc::clone(&gate);
        let n = name.to_string();
        let node = TestNode::arc(name, NodeConfig::default(), move |ctx, input| {
            let g = Arc::clone(&g);
            let n = n.clone();
            async_stream::stream! {
                g.wait().await;
                let shown = match &input { Value::String(s) => s.clone(), Value::Null => "<nil>".into(), v => v.to_string() };
                yield Ok(output_event(&ctx, Value::String(format!("{shown}:{n}"))));
            }
            .boxed()
        });
        (node, gate)
    }

    /// Port of Go newErroringNode.
    pub(crate) fn erroring_node(name: &str, msg: &'static str) -> Arc<dyn Node> {
        TestNode::arc(name, NodeConfig::default(), move |_, _| fail(test_err(msg)))
    }

    pub(crate) struct CancelObserver {
        pub(crate) started: Arc<Gate>,
        pub(crate) observed: Arc<AtomicBool>,
    }

    /// Port of Go newCancelObservingNode: signals start, waits for its token, records it.
    pub(crate) fn cancel_observing_node(
        name: &str,
        cfg: NodeConfig,
    ) -> (Arc<dyn Node>, CancelObserver) {
        let started = Gate::new();
        let observed = Arc::new(AtomicBool::new(false));
        let (s, o) = (Arc::clone(&started), Arc::clone(&observed));
        let node = TestNode::arc(name, cfg, move |ctx, _| {
            let (s, o) = (Arc::clone(&s), Arc::clone(&o));
            futures::stream::once(async move {
                s.release();
                ctx.cancellation_token().cancelled().await;
                o.store(true, Ordering::SeqCst);
            })
            .filter_map(|()| async { None::<Result<Event, AgentError>> })
            .boxed()
        });
        (node, CancelObserver { started, observed })
    }

    #[derive(Debug, Default, Clone)]
    pub(crate) struct Seen {
        pub(crate) path: String,
        pub(crate) branch: String,
        pub(crate) scope: String,
    }

    /// Port of Go newStubNode: records path/branch/scope, emits `Event{Output: out}`.
    pub(crate) fn stub_node(name: &str, out: Value) -> (Arc<dyn Node>, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let s = Arc::clone(&seen);
        let node = TestNode::arc(name, NodeConfig::default(), move |ctx, _| {
            *s.lock() = Seen {
                path: ctx.path(),
                branch: ctx.branch(),
                scope: ctx.isolation_scope(),
            };
            let out = out.clone();
            let ev = Event {
                output: (!out.is_null()).then_some(out),
                ..Event::default()
            };
            once(ev)
        });
        (node, seen)
    }

    /// Port of Go newMessageAsOutputNode.
    pub(crate) fn message_as_output_node(name: &str, text: &str) -> Arc<dyn Node> {
        let text = text.to_string();
        TestNode::arc(name, NodeConfig::default(), move |_, _| {
            let mut ev = Event {
                node_info: Some(NodeInfo {
                    message_as_output: true,
                    ..NodeInfo::default()
                }),
                ..Event::default()
            };
            ev.response.content = Some(Content::from_text(text.clone(), "model"));
            once(ev)
        })
    }

    /// Port of Go newRequestInputNode: one RequestedInput (InterruptID "iid-1") event.
    pub(crate) fn request_input_node(name: &str, msg: &str) -> Arc<dyn Node> {
        let msg = msg.to_string();
        TestNode::arc(name, NodeConfig::default(), move |_, _| {
            once(Event {
                requested_input: Some(RequestInput {
                    interrupt_id: "iid-1".into(),
                    message: msg.clone(),
                    ..RequestInput::default()
                }),
                ..Event::default()
            })
        })
    }

    /// Port of Go noopEmit.
    pub(crate) fn noop_emit() -> Emit {
        Arc::new(|_ev| Box::pin(async { Ok(()) }))
    }

    /// An emit that appends to a shared history (Go `forwarded = append(forwarded, ev)`).
    pub(crate) fn recording_emit() -> (Emit, Arc<Mutex<Vec<Event>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = Arc::clone(&log);
        let emit: Emit = Arc::new(move |ev| {
            l.lock().push(ev);
            Box::pin(async { Ok(()) })
        });
        (emit, log)
    }

    /// Port of Go drainDynamicWithErr: runs `n` directly under a top-level context.
    pub(crate) async fn drain_dynamic_with_err(
        n: &Arc<dyn Node>,
        input: Value,
    ) -> (Vec<Event>, Option<AgentError>) {
        let mut s = n.run(top_level_ctx(), input);
        let mut events = Vec::new();
        while let Some(item) = s.next().await {
            match item {
                Ok(ev) => events.push(ev),
                Err(e) => return (events, Some(e)),
            }
        }
        (events, None)
    }

    /// Port of Go drainDynamic.
    pub(crate) async fn drain_dynamic(n: &Arc<dyn Node>, input: Value) -> Vec<Event> {
        let (events, err) = drain_dynamic_with_err(n, input).await;
        if let Some(err) = err {
            panic!("Run error: {err}");
        }
        events
    }

    /// Clones an error for re-returning while keeping the original (AgentError is not Clone).
    pub(crate) fn clone_err(e: &AgentError) -> AgentError {
        match workflow_error(e) {
            Some(w) => w.clone().into(),
            None => AgentError::other(e.to_string()),
        }
    }

    /// Port of Go runInOrchestratorWithErr: runs `f` as the body of a dynamic node "orch"
    /// (so `run_node` has a sub-scheduler) and returns (run error ?? body result).
    pub(crate) async fn run_in_orchestrator_with_err<F, Fut>(f: F) -> Result<Value, AgentError>
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AgentError>> + Send + 'static,
    {
        let slot: Arc<Mutex<Option<Result<Value, AgentError>>>> = Arc::new(Mutex::new(None));
        let s = Arc::clone(&slot);
        let f = Arc::new(f);
        let n = crate::workflow::dynamic_node::<Value, Value, _, _>(
            "orch",
            move |ctx, _input: Value, _emit| {
                let (f, s) = (Arc::clone(&f), Arc::clone(&s));
                async move {
                    let res = f(ctx).await;
                    let ret = match &res {
                        Ok(v) => Ok(v.clone()),
                        Err(e) => Err(clone_err(e)),
                    };
                    *s.lock() = Some(res);
                    ret
                }
            },
            NodeConfig::default(),
        );
        let (_, run_err) = drain_dynamic_with_err(&n, Value::String(String::new())).await;
        if let Some(e) = run_err {
            return Err(e);
        }
        slot.lock().take().unwrap_or(Ok(Value::Null))
    }

    /// Port of Go runInOrchestrator.
    pub(crate) async fn run_in_orchestrator<F, Fut>(f: F) -> Value
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AgentError>> + Send + 'static,
    {
        run_in_orchestrator_with_err(f)
            .await
            .expect("orchestrator error")
    }

    /// Port of Go eventsSession / fakeSession: a session with fixed history.
    pub(crate) struct EventsSession {
        events: Vec<Event>,
        state: MapState,
    }

    impl EventsSession {
        pub(crate) fn new(events: Vec<Event>) -> Self {
            Self {
                events,
                state: MapState::default(),
            }
        }
    }

    #[derive(Default)]
    pub(crate) struct MapState(Mutex<serde_json::Map<String, Value>>);

    impl State for MapState {
        fn get(&self, key: &str) -> Result<Value, SessionError> {
            self.0
                .lock()
                .get(key)
                .cloned()
                .ok_or(SessionError::StateKeyNotExist)
        }
        fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
            self.0.lock().insert(key.to_string(), value);
            Ok(())
        }
        fn all(&self) -> Vec<(String, Value)> {
            self.0
                .lock()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
    }

    impl Session for EventsSession {
        fn id(&self) -> &str {
            "test-session"
        }
        fn app_name(&self) -> &str {
            "test-app"
        }
        fn user_id(&self) -> &str {
            "test-user"
        }
        fn state(&self) -> &dyn State {
            &self.state
        }
        fn events(&self) -> Vec<Event> {
            self.events.clone()
        }
        fn last_update_time(&self) -> DateTime<Utc> {
            DateTime::<Utc>::default()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Captures spans for the adapted OTel span tests (Go tracetest.InMemoryExporter).
    #[derive(Clone, Default)]
    pub(crate) struct SpanCapture {
        pub(crate) spans: Arc<Mutex<Vec<CapturedSpan>>>,
    }

    #[derive(Clone, Debug, Default)]
    pub(crate) struct CapturedSpan {
        pub(crate) id: u64,
        pub(crate) name: &'static str,
        pub(crate) otel_name: String,
        pub(crate) status: String,
    }

    struct FieldVisitor<'a>(&'a mut CapturedSpan);

    impl tracing::field::Visit for FieldVisitor<'_> {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            match field.name() {
                "otel.name" => self.0.otel_name = value.to_string(),
                "otel.status_code" => self.0.status = value.to_string(),
                _ => {}
            }
        }
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.record_str(field, format!("{value:?}").trim_matches('"'));
        }
    }

    impl<S> tracing_subscriber::Layer<S> for SpanCapture
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut span = CapturedSpan {
                id: id.into_u64(),
                name: attrs.metadata().name(),
                ..CapturedSpan::default()
            };
            attrs.record(&mut FieldVisitor(&mut span));
            self.spans.lock().push(span);
        }
        fn on_record(
            &self,
            id: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut spans = self.spans.lock();
            if let Some(span) = spans.iter_mut().find(|s| s.id == id.into_u64()) {
                values.record(&mut FieldVisitor(span));
            }
        }
    }

    impl SpanCapture {
        /// Installs a thread-default subscriber; use with a current-thread `#[tokio::test]`.
        pub(crate) fn install() -> (SpanCapture, tracing::subscriber::DefaultGuard) {
            use tracing_subscriber::layer::SubscriberExt;
            crate::llm::flow::span_capture::pin_tracing_interest();
            let cap = SpanCapture::default();
            let guard =
                tracing::subscriber::set_default(tracing_subscriber::registry().with(cap.clone()));
            (cap, guard)
        }
        /// `invoke_node` spans in creation order.
        pub(crate) fn node_spans(&self) -> Vec<CapturedSpan> {
            self.spans
                .lock()
                .iter()
                .filter(|s| s.name == "invoke_node")
                .cloned()
                .collect()
        }
    }
}
