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

//! Port of Go workflow/dynamic_node.go: a node whose body schedules children at run time with
//! [`run_node`](crate::workflow::run_node).

use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use gadk_agent::{AgentError, Context, ContextDelta, DynamicSubScheduler, EventStream};
use gadk_session::{Event, NodeInfo};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::CompiledSchema;
use crate::workflow::dynamic_scheduler::new_dynamic_sub_scheduler;
use crate::workflow::failure_inventory::publish_failure_inventory;
use crate::workflow::nodes::function::{CoerceError, Step, coerce_input, short_type_name};
use crate::workflow::persistence::{WORKFLOW_DELEGATED_OUTPUT_KEY, WORKFLOW_NODE_COMPLETED_KEY};
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::is_kind;
use crate::workflow::{BaseNode, Emit, Node, NodeConfig};

/// One emitted event awaiting the consumer (Go makeEmit's serialized yield).
pub(crate) struct EmitMsg {
    ev: Box<Event>,
    ack: oneshot::Sender<()>,
}

impl EmitMsg {
    /// The event and the acknowledgement to fire once the consumer has resumed.
    pub(crate) fn into_parts(self) -> (Event, oneshot::Sender<()>) {
        (*self.ev, self.ack)
    }
}

/// Port of Go makeEmit. The returned emit resolves after the node's stream yielded the event
/// and the consumer polled again. It fails with `AgentError::Cancelled` once the context is
/// cancelled or the stream (the receiver) is gone. Go's consumerGone() check is implicit: a
/// dropped stream drops the body future.
pub(crate) fn make_emit(ctx: &Context) -> (Emit, mpsc::Receiver<EmitMsg>) {
    let (tx, rx) = mpsc::channel::<EmitMsg>(1);
    let token = ctx.cancellation_token().clone();
    let emit: Emit = Arc::new(move |ev: Event| {
        let (tx, token) = (tx.clone(), token.clone());
        Box::pin(async move {
            if token.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let (ack, done) = oneshot::channel();
            if tx
                .send(EmitMsg {
                    ev: Box::new(ev),
                    ack,
                })
                .await
                .is_err()
            {
                return Err(AgentError::Cancelled);
            }
            done.await.map_err(|_| AgentError::Cancelled)
        })
    });
    (emit, rx)
}

/// Port of Go dynamicNode[IN, OUT].
struct DynamicNode<In, Out, F> {
    base: BaseNode,
    f: Arc<F>,
    input_schema: Option<Arc<CompiledSchema>>,
    _types: PhantomData<fn(In) -> Out>,
}

/// Port of Go applyDynamicDefaults: `rerun_on_resume` defaults to `Some(true)`.
fn apply_dynamic_defaults(mut cfg: NodeConfig) -> NodeConfig {
    if cfg.rerun_on_resume.is_none() {
        cfg.rerun_on_resume = Some(true);
    }
    cfg
}

fn compile(
    schema: Option<Value>,
    what: &str,
) -> Result<Option<Arc<CompiledSchema>>, WorkflowError> {
    schema
        .map(|s| {
            CompiledSchema::new(s).map(Arc::new).map_err(|e| {
                WorkflowError::new(
                    WorkflowErrorKind::Other,
                    format!("resolving {what} schema: {e}"),
                )
            })
        })
        .transpose()
}

/// Port of Go newDynamicNodeWithResolvedSchemas.
fn new_dynamic<In, Out, F, Fut>(
    name: &str,
    f: F,
    input_schema: Option<Arc<CompiledSchema>>,
    output_schema: Option<Arc<CompiledSchema>>,
    cfg: NodeConfig,
) -> Arc<dyn Node>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In, Emit) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    Arc::new(DynamicNode::<In, Out, F> {
        base: BaseNode::from_compiled(name, "", cfg, input_schema.clone(), output_schema),
        f: Arc::new(f),
        input_schema,
        _types: PhantomData,
    })
}

/// Port of Go NewDynamicNode: a node whose body orchestrates children with `run_node`. Its
/// `rerun_on_resume` defaults to true.
pub fn dynamic_node<In, Out, F, Fut>(name: &str, f: F, cfg: NodeConfig) -> Arc<dyn Node>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In, Emit) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    new_dynamic::<In, Out, F, Fut>(name, f, None, None, apply_dynamic_defaults(cfg))
}

/// Port of Go NewDynamicNodeWithSchema: as [`dynamic_node`], with input and output schemas.
pub fn dynamic_node_with_schema<In, Out, F, Fut>(
    name: &str,
    f: F,
    input: Option<Value>,
    output: Option<Value>,
    cfg: NodeConfig,
) -> Result<Arc<dyn Node>, WorkflowError>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In, Emit) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    let input_schema = compile(input, "input")?;
    let output_schema = compile(output, "output")?;
    Ok(new_dynamic::<In, Out, F, Fut>(
        name,
        f,
        input_schema,
        output_schema,
        apply_dynamic_defaults(cfg),
    ))
}

impl<In, Out, F, Fut> Node for DynamicNode<In, Out, F>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In, Emit) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
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
    /// Port of Go (*dynamicNode).Run.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let (f, schema, name) = (
            Arc::clone(&self.f),
            self.input_schema.clone(),
            self.base.name().to_string(),
        );
        let waits = self.base.config().wait_for_output == Some(true);
        // The data address of the Arc holding this node: equal to its node_producer_key.
        let self_key = std::ptr::from_ref(self).cast::<()>() as usize;
        Box::pin(async_stream::stream! {
            let typed: In = match coerce_input(input, schema.as_deref()) {
                Ok(v) => v,
                Err(CoerceError::Validation(e) | CoerceError::Conversion(e)) => {
                    yield Err(AgentError::other(format!(
                        "dynamic node {}: invalid input type, expected {}: {e}",
                        go_quote(&name),
                        short_type_name::<In>()
                    )));
                    return;
                }
            };
            let (emit, mut rx) = make_emit(&ctx);
            // Go composePath.
            let parent_path = {
                let p = ctx.path();
                if p.is_empty() { name.clone() } else { p }
            };
            let sub = new_dynamic_sub_scheduler(ctx.clone(), parent_path.clone(), Arc::clone(&emit));
            let orchestrator_ctx = ctx.with_delta(ContextDelta {
                run_id: Some(String::new()),
                sub_scheduler: Some(Some(Arc::clone(&sub) as Arc<dyn DynamicSubScheduler>)),
                output_for_ancestors: Some(sub.output_for_ancestors()),
                ..ContextDelta::default()
            });
            let mut body = Box::pin(f(orchestrator_ctx, typed, emit));
            let result = loop {
                let step = tokio::select! {
                    biased;
                    Some(msg) = rx.recv() => Step::Emit(msg),
                    r = &mut body => Step::Done(r),
                };
                match step {
                    Step::Emit(msg) => {
                        let (ev, ack) = msg.into_parts();
                        yield Ok(ev);
                        let _ = ack.send(());
                    }
                    Step::Done(r) => break r,
                }
            };
            drop(body);
            match result {
                Err(e) => {
                    if is_kind(&e, WorkflowErrorKind::NodeWaitingForOutput) {
                        yield Err(e);
                    } else if !is_kind(&e, WorkflowErrorKind::NodeInterrupted) {
                        publish_failure_inventory(self_key, &sub.child_failures());
                        yield Err(e);
                    }
                }
                Ok(out) => {
                    if let Some(value) = sub.delegated_output() {
                        if !(value.is_null() && waits) {
                            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                            ev.node_info = Some(NodeInfo { path: parent_path.clone(), ..NodeInfo::default() });
                            ev.response.custom_metadata.insert(WORKFLOW_NODE_COMPLETED_KEY.into(), Value::Bool(true));
                            ev.response.custom_metadata.insert(WORKFLOW_DELEGATED_OUTPUT_KEY.into(), value);
                            yield Ok(ev);
                        }
                    } else {
                        match serde_json::to_value(out) {
                            Err(e) => yield Err(AgentError::other(format!(
                                "dynamic node {}: marshal output: {e}",
                                go_quote(&name)
                            ))),
                            Ok(Value::Null) => {}
                            Ok(v) => {
                                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                                ev.output = Some(v);
                                ev.node_info = Some(NodeInfo { path: parent_path.clone(), ..NodeInfo::default() });
                                yield Ok(ev);
                            }
                        }
                    }
                }
            }
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
