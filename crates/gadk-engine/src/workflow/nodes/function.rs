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

//! Port of Go workflow/function_node.go.

use std::any::Any;
use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{AgentError, Context, EventStream, InvocationContext};
use gadk_genai::{Content, Part};
use gadk_session::Event;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::{CompiledSchema, convert_with_schema, infer_schema};
use crate::workflow::nodes::dynamic::{EmitMsg, make_emit};
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::is_kind;
use crate::workflow::{BaseNode, Node, NodeConfig};

/// Port of the Go `emit func(*session.Event) error` callback: forwards an event to the node's
/// consumer and resolves once the consumer took it (an error once the consumer is gone).
pub type Emit = Arc<dyn Fn(Event) -> BoxFuture<'static, Result<(), AgentError>> + Send + Sync>;

/// What a plain function body produced (Go type-switches on the returned `any`).
pub(crate) enum FnOutput {
    Event(Box<Event>),
    Content(Content),
    Value(Option<Value>),
}

type PlainFn =
    Arc<dyn Fn(Context, Value) -> BoxFuture<'static, Result<FnOutput, AgentError>> + Send + Sync>;
type EmittingFn = Arc<
    dyn Fn(Context, Value, Emit) -> BoxFuture<'static, Result<Option<Value>, AgentError>>
        + Send
        + Sync,
>;

enum Body {
    Plain(PlainFn),
    Emitting(EmittingFn),
}

/// Port of Go workflow.FunctionNode: a node that runs a function. The constructors return it as
/// `Arc<dyn Node>`.
pub struct FunctionNode {
    base: BaseNode,
    body: Body,
    state_field_names: Option<Vec<String>>,
}

/// One step of a node stream that drains emitted events while its body runs.
pub(crate) enum Step<T> {
    Emit(EmitMsg),
    Done(T),
}

/// Failure of [`coerce_input`]; callers format the Go message for their node kind.
pub(crate) enum CoerceError {
    Validation(String),
    Conversion(String),
}

/// The last path segment of a type name (Go `%T` prints "pkg.Type").
pub(crate) fn short_type_name<T>() -> &'static str {
    let full = std::any::type_name::<T>();
    full.rsplit("::").next().unwrap_or(full)
}

/// The Go zero value of `In`: `null` when `In` accepts it, else a value synthesized from the
/// inferred schema ("" / 0 / false / [] / an object of zero-valued properties).
pub(crate) fn zero_input<In: DeserializeOwned + JsonSchema>() -> Result<In, String> {
    if let Ok(v) = serde_json::from_value::<In>(Value::Null) {
        return Ok(v);
    }
    serde_json::from_value::<In>(zero_from_schema(&infer_schema::<In>())).map_err(|e| e.to_string())
}

fn zero_from_schema(schema: &Value) -> Value {
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if types.contains(&"null") {
        return Value::Null;
    }
    match types.first().copied() {
        Some("string") => json!(""),
        Some("integer") | Some("number") => json!(0),
        Some("boolean") => json!(false),
        Some("array") => json!([]),
        Some("object") => Value::Object(
            schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| {
                    p.iter()
                        .map(|(k, s)| (k.clone(), zero_from_schema(s)))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => Value::Null,
    }
}

/// Port of the input-typing half of Go newFunctionNodeWithResolvedSchemas / coerceInput: nil
/// becomes the zero value; otherwise validate against `schema` (if any), then convert.
pub(crate) fn coerce_input<In: DeserializeOwned + JsonSchema>(
    input: Value,
    schema: Option<&CompiledSchema>,
) -> Result<In, CoerceError> {
    if input.is_null() {
        return zero_input::<In>().map_err(CoerceError::Conversion);
    }
    if let Some(s) = schema {
        s.validate(&input).map_err(CoerceError::Validation)?;
    }
    convert_with_schema::<Value, In>(&input, None).map_err(CoerceError::Conversion)
}

fn function_input_error<In>(name: &str, e: CoerceError) -> AgentError {
    match e {
        CoerceError::Validation(e) => AgentError::other(format!(
            "function node {name}: validation failed for input *{}: {e}",
            short_type_name::<In>()
        )),
        CoerceError::Conversion(e) => AgentError::other(format!(
            "new function node: invalid input type, expected *{}: {e}",
            short_type_name::<In>()
        )),
    }
}

/// The Go type switch on the body's return value: an `Event` is forwarded, a `Content` becomes
/// the event content, anything else is the event output (`null` = Go nil, no output).
pub(crate) fn to_fn_output<Out: Serialize + 'static>(out: Out) -> Result<FnOutput, AgentError> {
    let any: Box<dyn Any> = Box::new(out);
    let any = match any.downcast::<Event>() {
        Ok(ev) => return Ok(FnOutput::Event(ev)),
        Err(a) => a,
    };
    let any = match any.downcast::<Content>() {
        Ok(c) => return Ok(FnOutput::Content(*c)),
        Err(a) => a,
    };
    let Ok(out) = any.downcast::<Out>() else {
        return Err(AgentError::other("function node: output type mismatch"));
    };
    let v = serde_json::to_value(*out)
        .map_err(|e| AgentError::other(format!("function node: marshal output: {e}")))?;
    Ok(FnOutput::Value((!v.is_null()).then_some(v)))
}

/// Go `Schema.Resolve` with the `"resolving <what> schema: %w"` wrap.
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

/// Port of Go newFunctionNodeWithResolvedSchemas.
fn new_plain<In, Out, F, Fut>(
    name: &str,
    f: F,
    in_schema: Option<Arc<CompiledSchema>>,
    out_schema: Option<Arc<CompiledSchema>>,
    cfg: NodeConfig,
) -> Arc<dyn Node>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    let base = BaseNode::from_compiled(name, "", cfg, in_schema.clone(), out_schema);
    let f = Arc::new(f);
    let node_name = name.to_string();
    let body: PlainFn = Arc::new(move |ctx, input| {
        let (f, schema, name) = (Arc::clone(&f), in_schema.clone(), node_name.clone());
        Box::pin(async move {
            let typed: In = coerce_input(input, schema.as_deref())
                .map_err(|e| function_input_error::<In>(&name, e))?;
            to_fn_output(f(ctx, typed).await?)
        })
    });
    Arc::new(FunctionNode {
        base,
        body: Body::Plain(body),
        state_field_names: None,
    })
}

/// Port of Go newEmittingFunctionNodeWithResolvedSchemas.
fn new_emitting<In, Out, F, Fut>(
    name: &str,
    f: F,
    in_schema: Option<Arc<CompiledSchema>>,
    out_schema: Option<Arc<CompiledSchema>>,
    cfg: NodeConfig,
) -> Arc<dyn Node>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In, Emit) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    let base = BaseNode::from_compiled(name, "", cfg, in_schema.clone(), out_schema);
    let f = Arc::new(f);
    let node_name = name.to_string();
    let body: EmittingFn = Arc::new(move |ctx, input, emit| {
        let (f, schema, name) = (Arc::clone(&f), in_schema.clone(), node_name.clone());
        Box::pin(async move {
            let typed: In = coerce_input(input, schema.as_deref())
                .map_err(|e| function_input_error::<In>(&name, e))?;
            let out = f(ctx, typed, emit).await?;
            let v = serde_json::to_value(out)
                .map_err(|e| AgentError::other(format!("function node: marshal output: {e}")))?;
            Ok((!v.is_null()).then_some(v))
        })
    });
    Arc::new(FunctionNode {
        base,
        body: Body::Emitting(body),
        state_field_names: None,
    })
}

/// Port of Go NewFunctionNode: a node that runs `f` on its (converted) input.
pub fn function_node<In, Out, F, Fut>(name: &str, f: F, cfg: NodeConfig) -> Arc<dyn Node>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    new_plain::<In, Out, F, Fut>(name, f, None, None, cfg)
}

/// Port of Go NewFunctionNodeWithSchema: as [`function_node`], with input and output schemas.
pub fn function_node_with_schema<In, Out, F, Fut>(
    name: &str,
    f: F,
    input: Option<Value>,
    output: Option<Value>,
    cfg: NodeConfig,
) -> Result<Arc<dyn Node>, WorkflowError>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    let in_schema = compile(input, "input")?;
    let out_schema = compile(output, "output")?;
    Ok(new_plain::<In, Out, F, Fut>(
        name, f, in_schema, out_schema, cfg,
    ))
}

/// Port of Go NewEmittingFunctionNode: a function node whose body can emit intermediate events
/// (progress, a RequestInput) through [`Emit`] before returning its output.
pub fn emitting_function_node<In, Out, F, Fut>(name: &str, f: F, cfg: NodeConfig) -> Arc<dyn Node>
where
    In: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + Send + 'static,
    F: Fn(Context, In, Emit) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    new_emitting::<In, Out, F, Fut>(name, f, None, None, cfg)
}

/// Port of Go NewEmittingFunctionNodeWithSchema.
pub fn emitting_function_node_with_schema<In, Out, F, Fut>(
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
    let in_schema = compile(input, "input")?;
    let out_schema = compile(output, "output")?;
    Ok(new_emitting::<In, Out, F, Fut>(
        name, f, in_schema, out_schema, cfg,
    ))
}

/// The serde field names of struct `T` in declaration order (renames applied), read from the
/// `fields` serde hands to `deserialize_struct`; `None` when `T` does not deserialize as a struct.
/// Go reflects over the struct fields; serde exposes their names (and order) only this way.
fn struct_field_names<T: DeserializeOwned>() -> Option<&'static [&'static str]> {
    struct Probe(Option<&'static [&'static str]>);

    #[derive(Debug)]
    struct Stop;
    impl std::fmt::Display for Stop {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("probe")
        }
    }
    impl std::error::Error for Stop {}
    impl serde::de::Error for Stop {
        fn custom<M: std::fmt::Display>(_: M) -> Self {
            Stop
        }
    }

    impl<'de> serde::Deserializer<'de> for &mut Probe {
        type Error = Stop;
        fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Stop> {
            Err(Stop)
        }
        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _name: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Stop> {
            self.0 = Some(fields);
            Err(Stop)
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
            option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier
            ignored_any
        }
    }

    let mut probe = Probe(None);
    let _ = T::deserialize(&mut probe);
    probe.0
}

/// The Go `state:"node_input"` binding: the property that receives the node's input.
const NODE_INPUT_FIELD: &str = "node_input";

/// Port of Go NewFunctionNodeFromState: a node whose `Params` struct is filled from session
/// state, one field per state key (Rust names the key with `#[serde(rename = "key")]` where Go
/// uses the `state:"key"` tag); a field named `node_input` receives the node's input instead.
/// The output schema is inferred from `Out`.
pub fn function_node_from_state<Params, Out, F, Fut>(
    name: &str,
    f: F,
    cfg: NodeConfig,
) -> Result<Arc<dyn Node>, WorkflowError>
where
    Params: DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + JsonSchema + Send + 'static,
    F: Fn(InvocationContext, Params) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, AgentError>> + Send + 'static,
{
    let other = |m: String| WorkflowError::new(WorkflowErrorKind::Other, m);
    let schema = infer_schema::<Params>();
    let fields = match struct_field_names::<Params>() {
        Some(f) if schema.get("type") == Some(&json!("object")) => f,
        _ => return Err(other("params must be a struct type".into())),
    };
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // Each value is checked against its field's schema, so a mismatch names the field (Go reads
    // the field from json.UnmarshalTypeError).
    let mut field_schemas: Vec<(String, Option<Arc<CompiledSchema>>)> = Vec::new();
    for field in fields {
        let compiled = compile(properties.get(*field).cloned(), "input")?;
        field_schemas.push(((*field).to_string(), compiled));
    }
    let has_node_input = fields.contains(&NODE_INPUT_FIELD);
    let state_field_names: Vec<String> = fields
        .iter()
        .filter(|f| **f != NODE_INPUT_FIELD)
        .map(|f| (*f).to_string())
        .collect();
    let in_schema = if has_node_input {
        compile(properties.get(NODE_INPUT_FIELD).cloned(), "input")?
    } else {
        None
    };
    let out_schema = compile(Some(infer_schema::<Out>()), "output")?;
    let f = Arc::new(f);
    let field_schemas = Arc::new(field_schemas);
    let body: PlainFn = Arc::new(move |ctx: Context, input: Value| {
        let (f, field_schemas) = (Arc::clone(&f), Arc::clone(&field_schemas));
        Box::pin(async move {
            let session = ctx.session();
            let mut state_map = Map::new();
            for (key, schema) in field_schemas.iter() {
                let val = if key == NODE_INPUT_FIELD {
                    input.clone()
                } else {
                    session.state().get(key).map_err(|e| {
                        AgentError::other(format!(
                            "missing state value for required field {} (state key {}): {e}",
                            go_quote(key),
                            go_quote(key)
                        ))
                    })?
                };
                if let Some(s) = schema
                    && let Err(e) = s.validate(&val)
                {
                    return Err(AgentError::other(if key == NODE_INPUT_FIELD {
                        format!(
                            "new function node from state: invalid input type for node_input: {e}"
                        )
                    } else {
                        format!(
                            "failed to convert state value to field {}: {e}",
                            go_quote(key)
                        )
                    }));
                }
                state_map.insert(key.clone(), val);
            }
            let params: Params = serde_json::from_value(Value::Object(state_map)).map_err(|e| {
                AgentError::other(format!("failed to convert state to Params: {e}"))
            })?;
            to_fn_output(f(ctx.as_invocation().clone(), params).await?)
        })
    });
    Ok(Arc::new(FunctionNode {
        base: BaseNode::from_compiled(name, "", cfg, in_schema, out_schema),
        body: Body::Plain(body),
        state_field_names: Some(state_field_names),
    }))
}

impl Node for FunctionNode {
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
    fn state_field_names(&self) -> Option<&[String]> {
        self.state_field_names.as_deref()
    }
    /// Port of Go (*FunctionNode).Run / runEmitting.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        match &self.body {
            Body::Plain(f) => {
                let f = Arc::clone(f);
                Box::pin(async_stream::stream! {
                    match f(ctx.clone(), input).await {
                        Err(e) => yield Err(e),
                        Ok(FnOutput::Event(mut ev)) => {
                            if ev.invocation_id.is_empty() {
                                ev.invocation_id = ctx.invocation_id().to_string();
                            }
                            yield Ok(*ev);
                        }
                        Ok(FnOutput::Content(c)) => {
                            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                            ev.response.content = Some(c);
                            yield Ok(ev);
                        }
                        Ok(FnOutput::Value(v)) => {
                            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                            ev.output = v;
                            yield Ok(ev);
                        }
                    }
                })
            }
            Body::Emitting(f) => {
                let f = Arc::clone(f);
                Box::pin(async_stream::stream! {
                    // Port of Go runEmitting + makeEmit: the body runs concurrently with the
                    // drain of its emitted events; each emit completes after the consumer took
                    // the event and asked for the next one.
                    let (emit, mut rx) = make_emit(&ctx);
                    let mut body = Box::pin(f(ctx.clone(), input, emit));
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
                    match result {
                        Err(e) if is_kind(&e, WorkflowErrorKind::NodeInterrupted) => {}
                        Err(e) => yield Err(e),
                        Ok(None) => {}
                        Ok(Some(out)) => {
                            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                            if let Value::String(s) = &out {
                                ev.response.content = Some(Content {
                                    parts: vec![Part::from_text(s.clone())],
                                    ..Content::default()
                                });
                            }
                            ev.output = Some(out);
                            yield Ok(ev);
                        }
                    }
                })
            }
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
