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

//! `ParallelWorker` (struct by Task 8, `Node::run` by Task 12; port of Go
//! workflow/parallel_worker.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, ContextDelta, EventStream};
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::branch::derive_sub_branch;
use crate::workflow::scheduler::{record_node_error, start_node_span};
use crate::workflow::{BaseNode, Node, NodeConfig, RetryConfig, calculate_delay, should_retry};

/// Port of Go workflow.ParallelWorker: runs a wrapped node once per item of a list input.
pub struct ParallelWorker {
    pub(crate) base: BaseNode,
    pub(crate) wrapped: Arc<dyn Node>,
    pub(crate) max_concurrency: usize,
    pub(crate) retry_cfg: Option<RetryConfig>,
}

impl ParallelWorker {
    /// Port of Go NewParallelWorker. The wrapped node may not carry its own RetryConfig; the
    /// worker's own RetryConfig is taken out of `cfg` (hidden from the scheduler) and applied per
    /// item.
    pub fn new(
        name: &str,
        wrapped: Arc<dyn Node>,
        max_concurrency: usize,
        mut cfg: NodeConfig,
    ) -> Result<Arc<ParallelWorker>, WorkflowError> {
        if wrapped.config().retry_config.is_some() {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "ParallelWorker {name}: wrapped node {} cannot have RetryConfig",
                    wrapped.name()
                ),
            ));
        }
        let retry_cfg = cfg.retry_config.take();
        Ok(Arc::new(ParallelWorker {
            base: BaseNode::new(name, "", cfg),
            wrapped,
            max_concurrency,
            retry_cfg,
        }))
    }
}

impl Node for ParallelWorker {
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
    /// Port of Go (*ParallelWorker).Run: runs the wrapped node once per list item (at most
    /// `max_concurrency` at a time; 0 = unlimited), each item under its own sub-branch
    /// `<wrapped>@<i+1>` and retried independently under the worker's RetryConfig. The first
    /// non-retryable failure cancels the in-flight siblings and is the node's error; otherwise one
    /// event carries the per-index outputs. Intermediate wrapped events are suppressed. Dropping
    /// the stream drops the `JoinSet`, aborting the workers.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let name = self.base.name().to_string();
        let wrapped = Arc::clone(&self.wrapped);
        let max_c = self.max_concurrency;
        let retry = self.retry_cfg.clone();
        Box::pin(async_stream::stream! {
            let (worker_ctx, guard) = ctx.with_agent_cancel();
            let items = match input {
                Value::Array(items) => items,
                other => {
                    yield Err(AgentError::other(format!(
                        "parallel worker {name} expects a slice input, got {}",
                        go_type_name(&other)
                    )));
                    return;
                }
            };
            if items.is_empty() {
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.output = Some(Value::Array(Vec::new()));
                yield Ok(ev);
                return;
            }
            let n = items.len();
            let sem = (max_c > 0).then(|| Arc::new(Semaphore::new(max_c)));
            let first_err: Arc<Mutex<Option<AgentError>>> = Arc::default();
            let token = worker_ctx.cancellation_token().clone();
            // Branch isolation: items 0..N-1 run under sub-branches <wrapped>@1..<wrapped>@N.
            let parent_branch = worker_ctx.branch();
            let mut set: JoinSet<(usize, Option<Value>)> = JoinSet::new();
            for (i, item) in items.into_iter().enumerate() {
                let permit = match &sem {
                    Some(sem) => {
                        let acquired = tokio::select! {
                            biased;
                            () = token.cancelled() => None,
                            p = Arc::clone(sem).acquire_owned() => p.ok(),
                        };
                        // Re-check after acquiring: cancellation may have raced the slot.
                        match acquired {
                            Some(p) if !token.is_cancelled() => Some(p),
                            _ => continue,
                        }
                    }
                    None => {
                        if token.is_cancelled() {
                            continue;
                        }
                        None
                    }
                };
                let item_branch =
                    derive_sub_branch(&parent_branch, &format!("{}@{}", wrapped.name(), i + 1));
                let item_ctx = worker_ctx.with_delta(ContextDelta {
                    branch: Some(item_branch),
                    ..ContextDelta::default()
                });
                let job = Worker {
                    wrapped: Arc::clone(&wrapped),
                    ctx: item_ctx,
                    retry: retry.clone(),
                    first_err: Arc::clone(&first_err),
                    token: token.clone(),
                };
                set.spawn(job.run(i, item, permit).in_current_span());
            }
            let mut outputs = vec![Value::Null; n];
            while let Some(res) = set.join_next().await {
                match res {
                    Ok((i, Some(out))) => {
                        if let Some(slot) = outputs.get_mut(i) {
                            *slot = out;
                        }
                    }
                    Ok((_, None)) => {}
                    // A panicking worker re-raises into the node's task, where the scheduler turns
                    // it into the node's failure (Go would crash the process).
                    Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                    Err(_) => {}
                }
            }
            drop(guard);
            let err = first_err.lock().take();
            if let Some(e) = err {
                yield Err(e);
                return;
            }
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(Value::Array(outputs));
            yield Ok(ev);
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `%T` of a JSON-decoded value.
fn go_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "<nil>",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float64",
        Value::String(_) => "string",
        Value::Array(_) => "[]interface {}",
        Value::Object(_) => "map[string]interface {}",
    }
}

/// Port of Go extractOutput: the event's output (a `Value::Null` output is Go's nil).
fn extract_output(ev: &Event) -> Option<Value> {
    ev.output.clone().filter(|v| !v.is_null())
}

/// Port of Go makeWorkerOutputEvent, as the output value it would carry: none, the single
/// output, or the list of outputs.
fn make_worker_output(mut outputs: Vec<Value>) -> Option<Value> {
    match outputs.len() {
        0 => None,
        1 => outputs.pop(),
        _ => Some(Value::Array(outputs)),
    }
}

/// Port of Go reportFailure (`sync.Once`): the first error wins and cancels the siblings.
fn report_failure(first: &Mutex<Option<AgentError>>, token: &CancellationToken, err: AgentError) {
    let mut g = first.lock();
    if g.is_none() {
        *g = Some(err);
        token.cancel();
    }
}

/// Port of Go (*ParallelWorker).runWrappedOnce: one attempt under its own `invoke_node` span, so
/// each item and each retry attempt is a distinct span.
async fn run_wrapped_once(
    wrapped: &Arc<dyn Node>,
    ctx: &Context,
    item: Value,
) -> Result<Vec<Value>, AgentError> {
    let span = start_node_span(ctx, wrapped);
    let attempt = async {
        let mut outputs = Vec::new();
        let mut s = wrapped.run(ctx.clone(), item);
        while let Some(r) = s.next().await {
            match r {
                Err(e) => return Err(e),
                Ok(ev) => outputs.extend(extract_output(&ev)),
            }
        }
        Ok(outputs)
    };
    let res = match &span {
        Some(sp) => attempt.instrument(sp.clone()).await,
        None => attempt.await,
    };
    record_node_error(span.as_ref(), res.as_ref().err(), None);
    res
}

/// One item's worker (Go runWorker's captured state).
struct Worker {
    wrapped: Arc<dyn Node>,
    ctx: Context,
    retry: Option<RetryConfig>,
    first_err: Arc<Mutex<Option<AgentError>>>,
    token: CancellationToken,
}

impl Worker {
    /// Port of Go (*ParallelWorker).runWorker. Failures are reported before the semaphore permit
    /// is released (the permit drops when this returns).
    async fn run(
        self,
        idx: usize,
        item: Value,
        _permit: Option<OwnedSemaphorePermit>,
    ) -> (usize, Option<Value>) {
        let mut failed_attempts = 0;
        loop {
            match run_wrapped_once(&self.wrapped, &self.ctx, item.clone()).await {
                Ok(outputs) => return (idx, make_worker_output(outputs)),
                Err(run_err) => {
                    failed_attempts += 1;
                    if let Some(cfg) = self
                        .retry
                        .as_ref()
                        .filter(|c| should_retry(c, &run_err, failed_attempts))
                    {
                        let delay = calculate_delay(cfg, failed_attempts, self.ctx.platform());
                        tokio::select! {
                            () = tokio::time::sleep(delay) => continue,
                            () = self.token.cancelled() => {
                                // Cancellation while parked in the retry delay is a real failure.
                                report_failure(&self.first_err, &self.token, AgentError::Cancelled);
                                return (idx, None);
                            }
                        }
                    }
                    // Cannot retry or exhausted attempts.
                    report_failure(&self.first_err, &self.token, run_err);
                    return (idx, None);
                }
            }
        }
    }
}
