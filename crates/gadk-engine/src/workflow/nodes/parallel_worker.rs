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

//! `ParallelWorker` (port of Go workflow/parallel_worker.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::fiber::{self, AttemptSite, PARALLEL_WORKER_CREATOR};
use gadk_agent::{AgentError, CancelCause, Context, ContextDelta, EventStream};
use gadk_platform::go_type_name;
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tracing::Instrument;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::llm::flow::context_err;
use crate::workflow::branch::derive_sub_branch;
use crate::workflow::nodes::AgentNode;
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
    /// event carries the per-index outputs. Intermediate wrapped events are suppressed. Each item
    /// task runs on a fresh fiber spawned with [`PARALLEL_WORKER_CREATOR`] (Go's `go
    /// n.runWorker(..)` goroutine; its retries stay on it). Dropping the stream drops the
    /// `JoinSet`, aborting the workers.
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
                };
                // Go `go n.runWorker(..)`: a goroutine started by the one running the node.
                let worker_fiber = fiber::spawn_child(PARALLEL_WORKER_CREATOR);
                set.spawn(worker_fiber.scope(job.run(i, item, permit)).in_current_span());
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

/// Port of Go reportFailure (`sync.Once`): the first error wins and cancels the siblings plainly
/// (Go `cancelFunc()`) through the worker context's cancel scope, which every item context shares.
fn report_failure(first: &Mutex<Option<AgentError>>, ctx: &Context, err: AgentError) {
    let mut g = first.lock();
    if g.is_none() {
        *g = Some(err);
        ctx.cancel_with_cause(CancelCause::Canceled);
    }
}

/// Port of Go (*ParallelWorker).runWrappedOnce: one attempt under its own `invoke_node` span, so
/// each item and each retry attempt is a distinct span. An `AgentNode`'s attempt is marked as a
/// worker attempt on the item's fiber (`with_attempt(Worker, ..)`, Go's `attemptWorker`, which
/// Go's trace manager reads off the goroutine's stack).
async fn run_wrapped_once(
    wrapped: &Arc<dyn Node>,
    ctx: &Context,
    item: Value,
) -> Result<Vec<Value>, AgentError> {
    if wrapped.as_any().is::<AgentNode>() {
        fiber::with_attempt(AttemptSite::Worker, run_wrapped_attempt(wrapped, ctx, item)).await
    } else {
        run_wrapped_attempt(wrapped, ctx, item).await
    }
}

/// One attempt of [`run_wrapped_once`].
async fn run_wrapped_attempt(
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
                            () = self.ctx.cancellation_token().cancelled() => {
                                // Cancellation while parked in the retry delay is a real failure
                                // (Go `reportFailure(ctx.Err())`).
                                report_failure(&self.first_err, &self.ctx, context_err(&self.ctx));
                                return (idx, None);
                            }
                        }
                    }
                    // Cannot retry or exhausted attempts.
                    report_failure(&self.first_err, &self.ctx, run_err);
                    return (idx, None);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use futures::StreamExt;
    use gadk_agent::fiber::{self, AttemptSite, Fiber, PARALLEL_WORKER_CREATOR};
    use gadk_agent::{
        AgentConfig, AgentError, BeforeAgentCallback, CancelCause, Context, InvocationContext,
    };
    use gadk_session::Event;
    use parking_lot::Mutex;
    use serde_json::{Value, json};

    use super::ParallelWorker;
    use crate::workflow::nodes::AgentNode;
    use crate::workflow::{Node, NodeConfig, RetryConfig, default_retry_config, function_node};

    fn ctx() -> Context {
        InvocationContext::builder().build()
    }

    fn retry(max_attempts: i32, delay: Duration) -> NodeConfig {
        NodeConfig {
            retry_config: Some(RetryConfig {
                max_attempts,
                initial_delay: delay,
                max_delay: delay,
                jitter: 0.0,
                ..default_retry_config()
            }),
            ..NodeConfig::default()
        }
    }

    async fn run(
        node: &Arc<ParallelWorker>,
        ctx: Context,
        input: Value,
    ) -> Vec<Result<Event, AgentError>> {
        node.run(ctx, input).collect().await
    }

    /// R2/R3: each item of a wrapped `AgentNode` runs on its own `PARALLEL_WORKER_CREATOR` fiber
    /// started by the fiber running the worker, and every attempt (retries included, on the same
    /// fiber) is claimed by the agent as a `Worker` attempt.
    #[tokio::test]
    async fn agent_node_attempts_are_worker_attempts_on_the_item_fiber() {
        type Seen = Arc<Mutex<Vec<(Fiber, Option<AttemptSite>)>>>;
        let seen: Seen = Arc::default();
        let seen_cb = seen.clone();
        let before: BeforeAgentCallback = Arc::new(move |_ctx: Context| {
            seen_cb
                .lock()
                .push((fiber::current(), fiber::current_attempt()));
            Box::pin(async { Ok(None) })
        });
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_agent = runs.clone();
        let agent = gadk_agent::new_agent(AgentConfig {
            name: "flaky".into(),
            before_agent_callbacks: vec![before],
            run: Some(Arc::new(move |_ctx: InvocationContext| {
                let n = runs_agent.fetch_add(1, Ordering::SeqCst);
                Box::pin(async_stream::stream! {
                    if n == 0 {
                        yield Err(AgentError::other("flaky"));
                    }
                })
            })),
            ..Default::default()
        })
        .unwrap();
        let wrapped: Arc<dyn Node> = AgentNode::new(agent, NodeConfig::default()).unwrap();
        let worker = ParallelWorker::new("pw", wrapped, 0, retry(3, Duration::ZERO)).unwrap();
        let outer = Fiber::root();
        let outer_id = outer.id;
        let items = outer.scope(run(&worker, ctx(), json!(["a"]))).await;
        assert_eq!(items.len(), 1);
        assert!(items[0].is_ok(), "{:?}", items[0]);
        let seen = seen.lock().clone();
        assert_eq!(seen.len(), 2, "one record per attempt");
        for (f, attempt) in &seen {
            assert_eq!(*attempt, Some(AttemptSite::Worker));
            assert_eq!(f.origin.creator, PARALLEL_WORKER_CREATOR);
            assert_eq!(f.origin.parent, outer_id);
            assert_ne!(f.id, outer_id);
        }
        assert_eq!(
            seen[0].0.id, seen[1].0.id,
            "a retry stays on the item's fiber"
        );
    }

    /// R7: the first failure cancels the in-flight siblings plainly (Go `cancelFunc()`).
    #[tokio::test]
    async fn the_first_failure_cancels_the_siblings_plainly() {
        let causes: Arc<Mutex<Vec<Option<CancelCause>>>> = Arc::default();
        let causes_node = causes.clone();
        let wrapped = function_node::<String, String, _, _>(
            "item",
            move |c: Context, input: String| {
                let causes = causes_node.clone();
                async move {
                    if input == "fail" {
                        return Err(AgentError::other("boom"));
                    }
                    c.cancellation_token().cancelled().await;
                    causes.lock().push(c.cause());
                    Ok(input)
                }
            },
            NodeConfig::default(),
        );
        let worker = ParallelWorker::new("pw", wrapped, 0, NodeConfig::default()).unwrap();
        let items = run(&worker, ctx(), json!(["wait", "fail"])).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_ref().unwrap_err().to_string(), "boom");
        let causes = causes.lock();
        assert_eq!(causes.len(), 1);
        assert!(
            matches!(causes[0], Some(CancelCause::Canceled)),
            "{causes:?}"
        );
    }

    /// Go `reportFailure(ctx.Err())`: a worker parked in its retry delay when the enclosing
    /// node's timeout fires fails with `context deadline exceeded`, not `context canceled`.
    #[tokio::test(start_paused = true)]
    async fn a_deadline_during_the_retry_delay_is_the_workers_error() {
        let wrapped = function_node::<String, String, _, _>(
            "item",
            |_c: Context, _input: String| async move { Err(AgentError::other("boom")) },
            NodeConfig::default(),
        );
        let worker =
            ParallelWorker::new("pw", wrapped, 0, retry(5, Duration::from_secs(1))).unwrap();
        let (tctx, _guard) = ctx().with_agent_timeout(Duration::from_millis(10));
        let items = run(&worker, tctx, json!(["a"])).await;
        assert_eq!(items.len(), 1);
        assert!(
            matches!(items[0], Err(AgentError::DeadlineExceeded)),
            "{:?}",
            items[0]
        );
    }
}
