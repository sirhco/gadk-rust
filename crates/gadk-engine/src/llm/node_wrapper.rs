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

//! Port of ADK-Go agent/llmagent/llm_agent_wrapper.go (wrappedSession lives in llm/agent.rs).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentError, Context, ContextDelta, EventStream, InvocationContext, Mode, Tool,
};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{Event, EventActions, NodeInfo, Session};
use serde_json::{Map, Value};

use crate::llm::agent::WrappedSession;
use crate::llm::mode::{mode_for, resolve_mode, with_bound_mode};
use crate::llm::processors::go_json_marshal;
use crate::llm::state::reveal;
use crate::tool::finish_task::{FINISH_TASK_SUCCESS_RESULT, FINISH_TASK_TOOL_NAME, FinishTaskTool};
use crate::tool::task_agent_tool::TaskAgentTool;
use crate::tool::utils::validate_output_schema;
use crate::workflow::route::go_quote;
use crate::workflow::{AgentNode, Node, NodeConfig, RunNodeOptions, run_node};
use crate::{WorkflowErrorKind, workflow_error};

fn error_stream(err: AgentError) -> EventStream {
    Box::pin(futures::stream::once(async move { Err(err) }))
}

/// Port of Go `llmagent.RunLLMAgentAsNode`, the entry point a custom node wrapping an LLM agent
/// calls to run it as a workflow node (re-exported as `gadk_engine::llm::run_llm_agent_as_node`).
/// Not async (C4): every branch returns a lazy stream.
///
/// The mode is the one this invocation bound for the agent, else its declaration, else chat.
/// single_turn seeds `input` as a user event (never persisted) and post-processes the model reply
/// into the output; task promotes the successful `finish_task` args; chat ignores `input` and runs
/// the outer task-delegation dispatch loop. Go's nil-context, nil-session and "tool or callback
/// context" error branches have no Rust counterpart: a `Context` is never null, `ctx.session()`
/// always returns a session, and every `Context` supports `with_delta`.
pub fn run_llm_agent_as_node(ctx: Context, agent: Arc<dyn Agent>, input: Value) -> EventStream {
    if reveal(agent.as_ref()).is_none() {
        return error_stream(AgentError::other(format!(
            "RunLLMAgentAsNode: {} is not an LlmAgent",
            go_quote(agent.name())
        )));
    }
    // A placement binds the mode it resolved for this agent; without one the agent is a plain
    // conversational callee (a transfer target, say).
    let mode = resolve_mode(mode_for(ctx.as_invocation(), agent.as_ref()), Mode::Chat);
    // Unreachable after resolve_mode(_, Chat) (Rust's Mode cannot hold Go's arbitrary strings);
    // kept with Go's text for parity.
    if !matches!(mode, Mode::Task | Mode::SingleTurn | Mode::Chat) {
        return error_stream(AgentError::other(format!(
            "RunLLMAgentAsNode: LlmAgent {} only supports task, single_turn, and chat mode, got {}",
            go_quote(agent.name()),
            go_quote(mode.as_str())
        )));
    }
    // Re-bind under this agent's own key so the request processors agree with the branch taken
    // here, and a mode resolved for this agent never governs a peer, child or same-named agent.
    let bound = with_bound_mode(ctx.as_invocation(), agent.name(), agent.agent_state(), mode);
    match mode {
        Mode::Chat => {
            // Go ctx.WithAgentContext(bound): keeps the node Context (its sub-scheduler is what
            // task dispatch runs through) and only swaps in the binding.
            let chat_ctx = ctx.with_delta(ContextDelta {
                bound_modes: Some(bound.bound_modes().clone()),
                ..Default::default()
            });
            run_chat(agent, chat_ctx)
        }
        _ => {
            let user_content = if input.is_null() {
                ctx.user_content().cloned()
            } else {
                node_input_to_content(&input)
            };
            let mut session = ctx.session();
            if let Some(seed) = prepare_llm_agent_input(agent.as_ref(), ctx.as_invocation(), &input)
            {
                session = Arc::new(WrappedSession::new(session, seed));
            }
            let ic =
                node_invocation_context(&ctx, &bound, Arc::clone(&agent), session, user_content);
            if mode == Mode::SingleTurn {
                run_single_turn(agent, ic)
            } else {
                run_task(agent, ic)
            }
        }
    }
}

/// Port of the `icontext.NewInvocationContext(bound, params{…})` call in RunLLMAgentAsNode: a
/// FRESH invocation context (`fresh_invocation`: own end-invocation flag starting NOT ended, as Go
/// passes no `EndInvocation`; path/run_id/sub_scheduler/output_for_ancestors/resumed_inputs/
/// tool_confirmation/function_call_id reset) around the possibly wrapped session, with the
/// params Go sets (agent, user content, the parent's isolation scope) and fresh actions, as Task
/// 12's `AgentNode::run`. Derived with `with_delta` rather than rebuilt through the builder, so
/// everything Go inherits through the parent context.Context (bound modes, cancellation,
/// platform, plugin manager, parent map, session service, artifacts, memory, run config,
/// invocation ID, branch) AND the per-invocation LLM-call limiter carry over unchanged.
fn node_invocation_context(
    ctx: &Context,
    bound: &InvocationContext,
    agent: Arc<dyn Agent>,
    session: Arc<dyn Session>,
    user_content: Option<Content>,
) -> InvocationContext {
    ctx.as_invocation().with_delta(ContextDelta {
        fresh_invocation: true,
        session: Some(session),
        agent: Some(agent),
        user_content: Some(user_content),
        isolation_scope: Some(ctx.isolation_scope()),
        bound_modes: Some(bound.bound_modes().clone()),
        actions: Some(Arc::new(parking_lot::Mutex::new(EventActions::default()))),
        ..Default::default()
    })
}

/// Port of Go llmagent.PrepareLLMAgentInput: the seeded user-role event for a single_turn agent's
/// first turn (the mode `ctx` bound for `agent`, else its declaration), `None` for any other.
pub(crate) fn prepare_llm_agent_input(
    agent: &dyn Agent,
    ctx: &InvocationContext,
    node_input: &Value,
) -> Option<Event> {
    if node_input.is_null() {
        return None;
    }
    reveal(agent)?;
    if mode_for(ctx, agent) != Mode::SingleTurn {
        return None;
    }
    let content = node_input_to_content(node_input)?;
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.author = "user".into();
    ev.response = LlmResponse {
        content: Some(content),
        ..Default::default()
    };
    let iso = ctx.isolation_scope();
    if !iso.is_empty() {
        ev.isolation_scope = iso;
    }
    Some(ev)
}

/// Port of Go llmagent.ProcessLLMAgentOutput: on the agent's final text turn (no function calls,
/// not partial, role model) parse the merged non-thought text against the output schema (else
/// keep the raw text), record it under the output key, and set it as the event output with
/// `node_info.message_as_output`. Errors only when schema validation fails on a non-empty reply.
pub(crate) fn process_llm_agent_output(
    agent: &dyn Agent,
    ev: &mut Event,
) -> Result<(), AgentError> {
    // Go checks function calls, then partial, then nil/role content; all return early with no
    // change, so the combined check is equivalent.
    let Some(content) = ev.response.content.as_ref() else {
        return Ok(());
    };
    if content.parts.iter().any(|p| p.function_call.is_some())
        || ev.response.partial
        || content.role != "model"
    {
        return Ok(());
    }
    let Some(state) = reveal(agent) else {
        return Ok(());
    };
    // Merge non-thought text parts (adk-python's `p.text for p in parts if p.text and not p.thought`).
    let text: String = content
        .parts
        .iter()
        .filter(|p| !p.thought)
        .map(|p| p.text.as_str())
        .collect();
    // Go's `parsed` map is non-nil only for a reply that parsed to a JSON object; a literal
    // `null` reply validates as a nil map (gadk's validator maps it to an empty map).
    let mut parsed = false;
    let output = match &state.cfg.output_schema {
        Some(_) if text.trim().is_empty() => None,
        Some(schema) => {
            let map = validate_output_schema(&text, schema).map_err(|e| {
                AgentError::other(format!(
                    "LlmAgent {} output validation failed: {e}",
                    go_quote(agent.name())
                ))
            })?;
            parsed = !matches!(serde_json::from_str::<Value>(&text), Ok(Value::Null));
            Some(Value::Object(map))
        }
        None => Some(Value::String(text)),
    };
    if !state.cfg.output_key.is_empty()
        && let Some(o) = &output
    {
        ev.actions
            .state_delta
            .get_or_insert_with(Map::new)
            .insert(state.cfg.output_key.clone(), o.clone());
    }
    ev.output = output;
    // Go #2291 (67fc01b1): the runner drops the output of an event flagged MessageAsOutput, which
    // would discard a parsed value, so a parsed reply skips the flag. An unparsed (no schema or
    // blank) or null reply keeps it, so the reply text stays the node output once stored.
    if parsed {
        return Ok(());
    }
    ev.node_info
        .get_or_insert_with(NodeInfo::default)
        .message_as_output = true;
    Ok(())
}

fn function_calls(ev: &Event) -> impl Iterator<Item = &FunctionCall> {
    ev.response
        .content
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_call.as_ref())
}

/// Port of Go extractFinishTaskFC.
fn extract_finish_task_fc(ev: &Event) -> Option<&FunctionCall> {
    function_calls(ev).find(|fc| fc.name == FINISH_TASK_TOOL_NAME)
}

/// Port of Go isFinishTaskSuccessFR: the FIRST finish_task FR decides; a non-success FR (e.g. a
/// validation error) lets the model retry.
fn is_finish_task_success_fr(ev: &Event) -> bool {
    let mut frs = ev
        .response
        .content
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_response.as_ref());
    frs.find(|fr| fr.name == FINISH_TASK_TOOL_NAME)
        .is_some_and(|fr| {
            fr.response.get("result").and_then(Value::as_str) == Some(FINISH_TASK_SUCCESS_RESULT)
        })
}

/// Port of Go isTaskDelegationTool.
fn is_task_delegation_tool(tools: &HashMap<String, Arc<dyn Tool>>, name: &str) -> bool {
    tools
        .get(name)
        .is_some_and(|t| t.as_any().is::<TaskAgentTool>())
}

/// Port of Go extractTaskDelegationFCs: FCs with a non-empty ID whose tool is a TaskAgentTool.
fn extract_task_delegation_fcs(
    ev: &Event,
    tools: &HashMap<String, Arc<dyn Tool>>,
) -> Vec<FunctionCall> {
    function_calls(ev)
        .filter(|fc| !fc.id.is_empty() && is_task_delegation_tool(tools, &fc.name))
        .cloned()
        .collect()
}

/// Port of Go findUnresolvedTaskDelegations: task FCs authored by `owner` (or "user") with no FR
/// of the same ID, in discovery order. Deliberately not filtered by isolation scope: a chat
/// coordinator's conversation spans turns, each with a fresh scope.
fn find_unresolved_task_delegations(
    session: &dyn Session,
    owner: &str,
    tools: &HashMap<String, Arc<dyn Tool>>,
) -> Vec<FunctionCall> {
    let mut pending: Vec<FunctionCall> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut resolved: HashSet<String> = HashSet::new();
    for ev in session.events() {
        let Some(content) = ev.response.content.as_ref() else {
            continue;
        };
        if ev.author != owner && ev.author != "user" {
            continue;
        }
        for p in &content.parts {
            if let Some(fc) = &p.function_call
                && !fc.id.is_empty()
                && is_task_delegation_tool(tools, &fc.name)
                && seen.insert(fc.id.clone())
            {
                pending.push(fc.clone());
            }
            if let Some(fr) = &p.function_response
                && !fr.id.is_empty()
            {
                resolved.insert(fr.id.clone());
            }
        }
    }
    pending
        .into_iter()
        .filter(|fc| !resolved.contains(&fc.id))
        .collect()
}

/// Port of Go findFinishTaskTool, reduced to its one use: the tool's WrapperKey ("" when absent).
fn finish_task_wrapper_key(agent: &dyn Agent) -> String {
    reveal(agent)
        .and_then(|s| {
            s.tools.read().iter().find_map(|t| {
                t.as_any()
                    .downcast_ref::<FinishTaskTool>()
                    .map(|f| f.wrapper_key_str().to_string())
            })
        })
        .unwrap_or_default()
}

/// Port of Go safeCanonicalToolsDict.
fn safe_canonical_tools_dict(agent: &dyn Agent) -> HashMap<String, Arc<dyn Tool>> {
    let Some(state) = reveal(agent) else {
        return HashMap::new();
    };
    state
        .tools
        .read()
        .iter()
        .filter(|t| !t.name().is_empty())
        .map(|t| (t.name().to_string(), Arc::clone(t)))
        .collect()
}

/// Port of Go dispatchTaskFC: runs the target as an AgentNode under a stable run id and
/// isolation scope (both the FC id), pausing the parent on an unresolved long-running call.
async fn dispatch_task_fc(
    parent: &Arc<dyn Agent>,
    fc: &FunctionCall,
    ctx: &Context,
) -> Result<Value, AgentError> {
    let target = parent.find_agent(&fc.name).ok_or_else(|| {
        AgentError::other(format!(
            "dispatchTaskFC: task target agent {} not found",
            go_quote(&fc.name)
        ))
    })?;
    let node = AgentNode::new(target, NodeConfig::default()).map_err(|e| {
        AgentError::other(format!(
            "dispatchTaskFC: build node for {}: {e}",
            go_quote(&fc.name)
        ))
    })?;
    let node: Arc<dyn Node> = node;
    run_node(
        ctx,
        node,
        Value::Object(fc.args.clone()),
        RunNodeOptions {
            run_id: Some(fc.id.clone()),
            isolation_scope: Some(fc.id.clone()),
            raise_on_wait: true,
            ..Default::default()
        },
    )
    .await
}

/// Port of Go synthesizeTaskFREvent: a user-role FR event closing the delegation; a non-object
/// output is wrapped under `output`.
fn synthesize_task_fr_event(
    platform: &Platform,
    invocation_id: &str,
    fc: &FunctionCall,
    output: Value,
) -> Event {
    let response = match output {
        Value::Object(m) => m,
        other => Map::from_iter([("output".to_string(), other)]),
    };
    let mut ev = Event::new(platform, invocation_id);
    ev.author = "user".into();
    ev.response = LlmResponse {
        content: Some(Content {
            role: "user".into(),
            parts: vec![Part {
                function_response: Some(FunctionResponse {
                    id: fc.id.clone(),
                    name: fc.name.clone(),
                    response,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    ev
}

/// Port of Go nodeInputToContent. Go switches on `*genai.Content` / `genai.Content`; workflow
/// inputs are `serde_json::Value` here, so a Content is recognised as an object whose keys are
/// only "parts" (an array) and optionally "role", and which deserializes as Content. Role is
/// forced to "user"; parts are reused verbatim. A string is user text; anything else is its Go
/// `json.Marshal` text.
pub(crate) fn node_input_to_content(input: &Value) -> Option<Content> {
    let user = |parts| Content {
        role: "user".into(),
        parts,
    };
    match input {
        Value::Null => None,
        Value::String(s) => Some(user(vec![Part::from_text(s.clone())])),
        Value::Object(m)
            if m.get("parts").is_some_and(Value::is_array)
                && m.keys().all(|k| k == "parts" || k == "role") =>
        {
            match serde_json::from_value::<Content>(input.clone()) {
                Ok(c) => Some(user(c.parts)),
                Err(_) => Some(user(vec![Part::from_text(go_json_marshal(input))])),
            }
        }
        other => Some(user(vec![Part::from_text(go_json_marshal(other))])),
    }
}

/// Port of Go runSingleTurn: one Agent.Run, each event post-processed so the model reply carries
/// the terminal output.
fn run_single_turn(agent: Arc<dyn Agent>, ic: InvocationContext) -> EventStream {
    Box::pin(async_stream::stream! {
        let mut inner = agent.run(ic);
        while let Some(item) = inner.next().await {
            let mut ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            if let Err(e) = process_llm_agent_output(agent.as_ref(), &mut ev) {
                yield Err(e);
                return;
            }
            yield Ok(ev);
        }
    })
}

/// Outcome of Go runChat's dispatchAndYield closure.
enum Dispatch {
    Emit(Box<Event>),
    Stop,
    Fail(AgentError),
}

async fn dispatch_step(agent: &Arc<dyn Agent>, fc: &FunctionCall, ctx: &Context) -> Dispatch {
    match dispatch_task_fc(agent, fc, ctx).await {
        // The task sub-agent paused (HITL): leave the delegation unresolved, so the next user
        // turn re-dispatches it into the same scope, and stop both loops.
        Err(e) if workflow_error(&e).is_some_and(|w| w.is(WorkflowErrorKind::NodeInterrupted)) => {
            Dispatch::Stop
        }
        Err(e) => Dispatch::Fail(e),
        // The task drained without finish_task (e.g. it asked the user a question): not complete,
        // leave the delegation unresolved.
        Ok(Value::Null) => Dispatch::Stop,
        Ok(out) => Dispatch::Emit(Box::new(synthesize_task_fr_event(
            ctx.platform(),
            ctx.invocation_id(),
            fc,
            out,
        ))),
    }
}

/// Port of Go runChat: a pre-LLM scan replays unresolved delegations from prior turns, then
/// Agent.Run is re-entered after each dispatch round until the model finishes without
/// delegating. Model events are yielded before their task FCs dispatch, sequentially, in the
/// order the model emitted them.
fn run_chat(agent: Arc<dyn Agent>, ctx: Context) -> EventStream {
    Box::pin(async_stream::stream! {
        let tools = safe_canonical_tools_dict(agent.as_ref());
        // Step 1: replay unresolved task FCs from prior turns.
        let session = ctx.session();
        for fc in find_unresolved_task_delegations(session.as_ref(), agent.name(), &tools) {
            match dispatch_step(&agent, &fc, &ctx).await {
                Dispatch::Emit(ev) => yield Ok(*ev),
                Dispatch::Stop => return,
                Dispatch::Fail(e) => {
                    yield Err(e);
                    return;
                }
            }
        }
        // Step 2: run; on fresh task FCs dispatch, then re-enter so the model sees the FRs.
        loop {
            let mut had_task_fc = false;
            let mut inner = agent.run(ctx.as_invocation().clone());
            while let Some(item) = inner.next().await {
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                // Only finalized events dispatch: a partial is never persisted, so an FR for it
                // would dangle in the next round's history.
                let task_fcs = if ev.response.partial {
                    Vec::new()
                } else {
                    extract_task_delegation_fcs(&ev, &tools)
                };
                yield Ok(ev);
                for fc in &task_fcs {
                    match dispatch_step(&agent, fc, &ctx).await {
                        Dispatch::Emit(fr) => yield Ok(*fr),
                        Dispatch::Stop => return,
                        Dispatch::Fail(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
                if !task_fcs.is_empty() {
                    had_task_fc = true;
                    // Dropping `inner` below stops the flow, as Go's `break` out of the range.
                    break;
                }
            }
            drop(inner);
            if !had_task_fc {
                return;
            }
        }
    })
}

/// Port of Go runTask: remember the latest finish_task FC's args; on FinishTaskTool's success FR,
/// promote them (or the wrapped value) as the output and stop. A non-success FR lets the model
/// see the error and retry. Rust's `FunctionCall.args` cannot express Go's nil map, so an FC
/// without args is pending with an empty map.
fn run_task(agent: Arc<dyn Agent>, ic: InvocationContext) -> EventStream {
    Box::pin(async_stream::stream! {
        let wrapper_key = finish_task_wrapper_key(agent.as_ref());
        let mut pending: Option<Map<String, Value>> = None;
        let mut inner = agent.run(ic);
        while let Some(item) = inner.next().await {
            let mut ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            if let Some(fc) = extract_finish_task_fc(&ev) {
                pending = Some(fc.args.clone());
                yield Ok(ev);
                continue;
            }
            if let Some(args) = pending.as_ref()
                && is_finish_task_success_fr(&ev)
            {
                let output = if wrapper_key.is_empty() {
                    Value::Object(args.clone())
                } else {
                    args.get(&wrapper_key)
                        .cloned()
                        .unwrap_or_else(|| Value::Object(args.clone()))
                };
                // Go 8c2e2e2a (#1977): the accepted finish_task output is the task agent's
                // result, so it goes to output_key (Go outputKeyOf: "" for a non-LLM agent; a
                // null output, Go's nil, is not written).
                if let Some(key) = reveal(agent.as_ref())
                    .map(|s| s.cfg.output_key.clone())
                    .filter(|k| !k.is_empty() && !output.is_null())
                {
                    ev.actions
                        .state_delta
                        .get_or_insert_with(Map::new)
                        .insert(key, output.clone());
                }
                ev.output = Some(output);
                yield Ok(ev);
                return;
            }
            yield Ok(ev);
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use futures::StreamExt;
    use gadk_agent::{
        AgentConfig, AgentError, BeforeModelCallback, Context, InvocationContext, Mode, ParentMap,
    };
    use gadk_genai::{Content, FunctionCall, Schema};
    use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
    use gadk_platform::Platform;
    use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
    use parking_lot::Mutex;
    use serde_json::{Value, json};

    use super::*;
    use crate::llm::agent as llm_agent;
    use crate::llm::config::LlmAgentConfig;
    use crate::llm::mode::bound_mode;
    use crate::testutil::{MockModel, collect_events};
    use crate::workflow::{
        AgentNode, Edge, Emit, Node, NodeConfig, START, Workflow, WorkflowOptions, dynamic_node,
    };

    fn text_resp(t: &str) -> Result<LlmResponse, ModelError> {
        Ok(LlmResponse {
            content: Some(Content::from_text(t, "model")),
            ..Default::default()
        })
    }
    fn fc_resp(id: &str, name: &str, args: Value) -> Result<LlmResponse, ModelError> {
        let Value::Object(args) = args else {
            panic!("args must be an object")
        };
        let fc = FunctionCall {
            id: id.into(),
            name: name.into(),
            args,
            ..Default::default()
        };
        Ok(LlmResponse {
            content: Some(Content::from_function_call(fc, "model")),
            ..Default::default()
        })
    }
    fn agent(name: &str, model: &Arc<MockModel>, mode: Mode) -> Arc<dyn Agent> {
        llm_agent::new(LlmAgentConfig {
            name: name.into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            mode,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn node_invocation_context_is_a_fresh_invocation() {
        // Go RunLLMAgentAsNode: icontext.NewInvocationContext(bound, params{…, IsolationScope:
        // ctx.IsolationScope(), …}) with no EndInvocation, so the agent's context starts not
        // ended even under an ended parent, keeps the parent's isolation scope and resets the
        // node-level fields.
        let model = MockModel::new(vec![]);
        let a = agent("doer", &model, Mode::Task);
        let mut resumed = serde_json::Map::new();
        resumed.insert("ask".into(), json!("yes"));
        let parent = Context::builder()
            .agent(a.clone())
            .isolation_scope("scope-1")
            .build()
            .with_delta(ContextDelta {
                path: Some("wf@1/doer@1".into()),
                run_id: Some("1".into()),
                output_for_ancestors: Some(vec!["wf".into()]),
                resumed_inputs: Some(Arc::new(resumed)),
                function_call_id: Some("fc-1".into()),
                ..Default::default()
            });
        parent.end_invocation();
        let ic = node_invocation_context(
            &parent,
            parent.as_invocation(),
            a.clone(),
            parent.session(),
            None,
        );
        assert!(
            !ic.ended(),
            "Go passes no EndInvocation: the flag starts false"
        );
        assert!(parent.ended());
        assert_eq!(ic.isolation_scope(), "scope-1");
        assert_eq!(ic.agent_name(), "doer");
        let c = ic.into_context();
        assert_eq!(c.path(), "");
        assert_eq!(c.run_id(), "");
        assert_eq!(c.function_call_id(), "");
        assert!(c.output_for_ancestors().is_empty());
        assert_eq!(c.resumed_input("ask"), None);
    }

    #[tokio::test]
    async fn rejects_non_llm_agent() {
        let a = gadk_agent::agent::new(AgentConfig {
            name: "custom".into(),
            ..Default::default()
        })
        .unwrap();
        let err = collect_events(run_llm_agent_as_node(
            Context::builder().build(),
            a,
            Value::Null,
        ))
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "RunLLMAgentAsNode: \"custom\" is not an LlmAgent"
        );
    }

    #[tokio::test]
    async fn single_turn_seeds_node_input_and_sets_output() {
        let model = MockModel::new(vec![text_resp("answer")]);
        let a = llm_agent::new(LlmAgentConfig {
            name: "worker".into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            mode: Mode::SingleTurn,
            output_key: "out".into(),
            ..Default::default()
        })
        .unwrap();
        let ctx = Context::builder().agent(a.clone()).build();
        let events = collect_events(run_llm_agent_as_node(ctx.clone(), a, json!("question")))
            .await
            .unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.output, Some(json!("answer")));
        assert!(last.node_info.as_ref().unwrap().message_as_output);
        assert_eq!(
            last.actions.state_delta.as_ref().unwrap().get("out"),
            Some(&json!("answer"))
        );
        let req = &model.requests()[0];
        assert!(
            req.contents
                .iter()
                .any(|c| c.role == "user" && c.parts.iter().any(|p| p.text == "question")),
            "seed must reach the model"
        );
        assert!(
            ctx.session().events().is_empty(),
            "seed must not be persisted"
        );
    }

    // Rust counterpart of Go TestRunLLMAgentAsNode_DeclaredSingleTurn_BindsForTheRequestProcessors (the Go test itself is ported in the parity tests).
    #[tokio::test]
    async fn declared_single_turn_hides_history() {
        let model = MockModel::new(vec![text_resp("answer")]);
        let a = agent("worker", &model, Mode::SingleTurn);
        let svc = Arc::new(InMemorySessionService::new());
        let created = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        for (author, text, role) in [
            ("user", "EARLIER_TURN", "user"),
            ("worker", "earlier answer", "model"),
            ("user", "current question", "user"),
        ] {
            let mut ev = Event::new(&Platform::default(), "inv-old");
            ev.author = author.into();
            ev.response.content = Some(Content::from_text(text, role));
            svc.append_event(created.as_ref(), &mut ev).await.unwrap();
        }
        let sess = svc
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: created.id().to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        let ctx = Context::builder()
            .session(sess)
            .session_service(svc.clone())
            .agent(a.clone())
            .user_content(Content::from_text("current question", "user"))
            .invocation_id("inv-declared-single-turn")
            .build();
        collect_events(run_llm_agent_as_node(ctx, a, Value::Null))
            .await
            .unwrap();
        let req = &model.requests()[0];
        assert!(
            !req.contents
                .iter()
                .flat_map(|c| c.parts.iter())
                .any(|p| p.text.contains("EARLIER_TURN")),
            "declared single_turn agent saw history: {:?}",
            req.contents
        );
    }

    #[tokio::test]
    async fn task_promotes_finish_task_args_on_success() {
        let model = MockModel::new(vec![fc_resp(
            "finish-1",
            "finish_task",
            json!({"result": "done"}),
        )]);
        let a = agent("doer", &model, Mode::Task);
        let events = collect_events(run_llm_agent_as_node(
            Context::builder().agent(a.clone()).build(),
            a,
            json!("do it"),
        ))
        .await
        .unwrap();
        let last = events.last().unwrap();
        assert_eq!(
            last.output,
            Some(json!({"result": "done"})),
            "default finish_task schema is an object, so the whole args map is the output"
        );
        let fr = last.response.content.as_ref().unwrap().parts[0]
            .function_response
            .as_ref()
            .unwrap();
        assert_eq!(
            (fr.name.as_str(), fr.response.get("result")),
            ("finish_task", Some(&json!("Task completed.")))
        );
        assert_eq!(
            model.requests().len(),
            1,
            "the wrapper returns right after the success FR"
        );
    }

    #[tokio::test]
    async fn task_run_keeps_parent_llm_call_limiter() {
        // C5 D3: the node context is derived with ContextDelta { session, fresh_invocation }, so the
        // invocation's limiter is shared, not reset (the runner/max_llm_calls_test.go port relies on it).
        let model = MockModel::new(vec![
            fc_resp("f1", "noop", json!({})),
            fc_resp("f2", "noop", json!({})),
        ]);
        let a = agent("doer", &model, Mode::Task);
        let limiter = Arc::new(gadk_agent::run_limits::LlmCallLimiter::new(1));
        let ctx = Context::builder()
            .agent(a.clone())
            .llm_call_limiter(limiter)
            .build();
        let err = collect_events(run_llm_agent_as_node(ctx, a, json!("do it")))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "max number of llm calls exceeded: limit is 1"
        );
    }

    #[tokio::test]
    async fn task_validation_error_lets_the_model_retry() {
        let schema: Schema = serde_json::from_value(json!({"type": "OBJECT", "properties": {"count": {"type": "INTEGER"}}, "required": ["count"]})).unwrap();
        let model = MockModel::new(vec![
            fc_resp("f1", "finish_task", json!({})),
            fc_resp("f2", "finish_task", json!({"count": 3})),
        ]);
        let a = llm_agent::new(LlmAgentConfig {
            name: "doer".into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            mode: Mode::Task,
            output_schema: Some(schema),
            ..Default::default()
        })
        .unwrap();
        let events = collect_events(run_llm_agent_as_node(
            Context::builder().agent(a.clone()).build(),
            a,
            json!("count"),
        ))
        .await
        .unwrap();
        assert!(
            events.iter().any(|e| e
                .response
                .content
                .as_ref()
                .is_some_and(|c| c.parts.iter().any(|p| p
                    .function_response
                    .as_ref()
                    .is_some_and(|fr| fr.response.contains_key("error")))))
        );
        assert_eq!(events.iter().filter(|e| e.output.is_some()).count(), 1);
        assert_eq!(events.last().unwrap().output, Some(json!({"count": 3})));
        assert_eq!(model.requests().len(), 2);
    }

    #[test]
    fn node_input_to_content_matches_go_marshal() {
        let c = node_input_to_content(&json!({"task": "summarize", "limit": 5, "html": "<a&b>"}))
            .unwrap();
        assert_eq!(c.role, "user");
        assert_eq!(
            c.parts[0].text,
            r#"{"html":"\u003ca\u0026b\u003e","limit":5,"task":"summarize"}"#
        );
        let c = node_input_to_content(
            &json!({"role": "model", "parts": [{"text": "part one"}, {"text": "part two"}]}),
        )
        .unwrap();
        assert_eq!(
            (c.role.as_str(), c.parts.len(), c.parts[1].text.as_str()),
            ("user", 2, "part two")
        );
        assert!(node_input_to_content(&Value::Null).is_none());
    }

    #[test]
    fn process_output_validates_schema_and_skips_non_final() {
        let schema: Schema = serde_json::from_value(
            json!({"type": "OBJECT", "properties": {"n": {"type": "INTEGER"}}, "required": ["n"]}),
        )
        .unwrap();
        let a = llm_agent::new(LlmAgentConfig {
            name: "w".into(),
            output_schema: Some(schema),
            output_key: "k".into(),
            ..Default::default()
        })
        .unwrap();
        let mut ok = Event {
            author: "w".into(),
            ..Default::default()
        };
        ok.response.content = Some(Content::from_text(r#"{"n": 1}"#, "model"));
        process_llm_agent_output(a.as_ref(), &mut ok).unwrap();
        assert_eq!(ok.output, Some(json!({"n": 1})));
        assert_eq!(
            ok.actions.state_delta.as_ref().unwrap().get("k"),
            Some(&json!({"n": 1}))
        );
        let mut bad = Event {
            author: "w".into(),
            ..Default::default()
        };
        bad.response.content = Some(Content::from_text("not json", "model"));
        let err = process_llm_agent_output(a.as_ref(), &mut bad)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("LlmAgent \"w\" output validation failed: "),
            "{err}"
        );
        let mut partial = ok.clone();
        partial.output = None;
        partial.response.partial = true;
        process_llm_agent_output(a.as_ref(), &mut partial).unwrap();
        assert!(partial.output.is_none());
    }

    // Go base_flow.go:904-913 runs an LlmAgent transfer target through RunNode → RunLLMAgentAsNode, which binds chat for it.
    #[tokio::test]
    async fn transfer_to_llm_agent_runs_through_the_node_wrapper() {
        let seen: Arc<Mutex<Option<Option<Mode>>>> = Arc::new(Mutex::new(None));
        let s2 = seen.clone();
        let record: BeforeModelCallback = Arc::new(move |ctx: Context, req: LlmRequest| {
            let target = ctx.agent();
            *s2.lock() = Some(bound_mode(ctx.as_invocation(), target.as_ref()));
            Box::pin(async move { (req, Ok(None)) })
        });
        let target_model = MockModel::new(vec![text_resp("hi from target")]);
        let target = llm_agent::new(LlmAgentConfig {
            name: "target".into(),
            model: Some(target_model.clone() as Arc<dyn Llm>),
            before_model_callbacks: vec![record],
            ..Default::default()
        })
        .unwrap();
        let coord_model = MockModel::new(vec![fc_resp(
            "t1",
            "transfer_to_agent",
            json!({"agent_name": "target"}),
        )]);
        let coord = llm_agent::new(LlmAgentConfig {
            name: "coord".into(),
            model: Some(coord_model.clone() as Arc<dyn Llm>),
            sub_agents: vec![target],
            ..Default::default()
        })
        .unwrap();
        let ic = InvocationContext::builder()
            .agent(coord.clone())
            .parent_map(Arc::new(ParentMap::new(&coord).unwrap()))
            .build_invocation();
        let events = collect_events(coord.run(ic)).await.unwrap();
        assert!(events.iter().any(|e| e.author == "target"));
        assert_eq!(
            *seen.lock(),
            Some(Some(Mode::Chat)),
            "the wrapper must bind chat for the transfer target"
        );
    }

    #[tokio::test]
    async fn chat_coordinator_dispatches_task_delegation() {
        let doer_model = MockModel::new(vec![fc_resp(
            "finish-1",
            "finish_task",
            json!({"result": "done"}),
        )]);
        let doer = llm_agent::new(LlmAgentConfig {
            name: "doer".into(),
            description: "Does things.".into(),
            model: Some(doer_model.clone() as Arc<dyn Llm>),
            mode: Mode::Task,
            ..Default::default()
        })
        .unwrap();
        let coord_model = MockModel::new(vec![
            fc_resp("fc-1", "doer", json!({"request": "x"})),
            text_resp("all done"),
        ]);
        let coord = llm_agent::new(LlmAgentConfig {
            name: "coordinator".into(),
            model: Some(coord_model.clone() as Arc<dyn Llm>),
            sub_agents: vec![doer],
            ..Default::default()
        })
        .unwrap();
        let body_agent = coord.clone();
        let node = dynamic_node(
            "coordinator",
            move |ctx: Context, input: Value, emit: Emit| {
                let a = body_agent.clone();
                async move {
                    let mut s = run_llm_agent_as_node(ctx, a, input);
                    while let Some(ev) = s.next().await {
                        emit(ev?).await?;
                    }
                    Ok::<Value, AgentError>(Value::Null)
                }
            },
            NodeConfig::default(),
        );
        let wf = Workflow::new(
            "wf",
            vec![Edge {
                from: Arc::clone(&START),
                to: node,
                route: None,
            }],
            WorkflowOptions::default(),
        )
        .unwrap();
        // Not collect_events: the workflow also emits content-less node-completion events.
        let events: Vec<Event> = wf
            .run_node(Context::builder().agent(coord.clone()).build(), json!("go"))
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert!(
            events
                .iter()
                .any(|e| e.author == "doer" && e.isolation_scope == "fc-1"),
            "task sub-agent events carry the FC id as isolation scope"
        );
        let fr = events
            .iter()
            .find_map(|e| {
                e.response.content.as_ref()?.parts.iter().find_map(|p| {
                    p.function_response
                        .as_ref()
                        .filter(|fr| fr.id == "fc-1")
                        .cloned()
                })
            })
            .expect("synthesized delegation FR");
        assert_eq!(fr.name, "doer");
        assert_eq!(Value::Object(fr.response), json!({"result": "done"}));
        assert_eq!(
            coord_model.requests().len(),
            2,
            "coordinator re-entered once after the dispatch"
        );
        assert_eq!(doer_model.requests().len(), 1);
    }

    // Review follow-ups: the AgentNode → LlmAgent branch now that the real wrapper exists.

    #[tokio::test]
    async fn agent_node_binds_single_turn_for_an_undeclared_llm_agent() {
        let seen: Arc<Mutex<Option<Option<Mode>>>> = Arc::new(Mutex::new(None));
        let s2 = seen.clone();
        let record: BeforeModelCallback = Arc::new(move |ctx: Context, req: LlmRequest| {
            let target = ctx.agent();
            *s2.lock() = Some(bound_mode(ctx.as_invocation(), target.as_ref()));
            Box::pin(async move { (req, Ok(None)) })
        });
        let model = MockModel::new(vec![text_resp("answer")]);
        let a = llm_agent::new(LlmAgentConfig {
            name: "worker".into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            before_model_callbacks: vec![record],
            ..Default::default()
        })
        .unwrap();
        let node = AgentNode::new(a, NodeConfig::default()).unwrap();
        let ctx = Context::builder().build();
        let events = collect_events(node.run(ctx.clone(), json!("question")))
            .await
            .unwrap();
        assert_eq!(*seen.lock(), Some(Some(Mode::SingleTurn)));
        let req = &model.requests()[0];
        assert!(
            req.contents
                .iter()
                .any(|c| c.role == "user" && c.parts.iter().any(|p| p.text == "question")),
            "the single_turn seed must reach the model: {:?}",
            req.contents
        );
        assert_eq!(events.last().unwrap().output, Some(json!("answer")));
        assert!(
            ctx.session().events().is_empty(),
            "seed must not be persisted"
        );
    }

    #[tokio::test]
    async fn agent_node_isolates_end_invocation_and_clears_function_call_id() {
        let seen_call_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let s2 = seen_call_id.clone();
        let short_circuit: gadk_agent::BeforeAgentCallback = Arc::new(move |ctx: Context| {
            *s2.lock() = Some(ctx.function_call_id());
            Box::pin(async move { Ok(Some(Content::from_text("short-circuit", "model"))) })
        });
        let model = MockModel::new(vec![]);
        let a = llm_agent::new(LlmAgentConfig {
            name: "worker".into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            mode: Mode::SingleTurn,
            before_agent_callbacks: vec![short_circuit],
            ..Default::default()
        })
        .unwrap();
        // The node runs under a tool context (as SingleTurnTool's caller would be).
        let parent = Context::builder().build().with_delta(ContextDelta {
            function_call_id: Some("call-1".into()),
            ..Default::default()
        });
        assert_eq!(parent.function_call_id(), "call-1");
        let node = AgentNode::new(a, NodeConfig::default()).unwrap();
        let events = collect_events(node.run(parent.clone(), json!("q")))
            .await
            .unwrap();
        assert_eq!(
            seen_call_id.lock().as_deref(),
            Some(""),
            "a fresh invocation has no call id"
        );
        assert!(
            !parent.ended(),
            "the callback's end_invocation must not reach the parent"
        );
        assert_eq!(events.last().unwrap().output, Some(json!("short-circuit")));
        assert!(model.requests().is_empty());
    }

    #[tokio::test]
    async fn single_turn_tool_runs_the_sub_agent_end_to_end() {
        let worker_model = MockModel::new(vec![text_resp("worker answer")]);
        let worker = llm_agent::new(LlmAgentConfig {
            name: "worker".into(),
            description: "Answers.".into(),
            model: Some(worker_model.clone() as Arc<dyn Llm>),
            mode: Mode::SingleTurn,
            ..Default::default()
        })
        .unwrap();
        let coord_model = MockModel::new(vec![
            fc_resp("c1", "worker", json!({"request": "q"})),
            text_resp("final"),
        ]);
        let coord = llm_agent::new(LlmAgentConfig {
            name: "coordinator".into(),
            model: Some(coord_model.clone() as Arc<dyn Llm>),
            sub_agents: vec![worker],
            ..Default::default()
        })
        .unwrap();
        let body_agent = coord.clone();
        let node = dynamic_node(
            "coordinator",
            move |ctx: Context, input: Value, emit: Emit| {
                let a = body_agent.clone();
                async move {
                    let mut s = run_llm_agent_as_node(ctx, a, input);
                    while let Some(ev) = s.next().await {
                        emit(ev?).await?;
                    }
                    Ok::<Value, AgentError>(Value::Null)
                }
            },
            NodeConfig::default(),
        );
        let wf = Workflow::new(
            "wf",
            vec![Edge {
                from: Arc::clone(&START),
                to: node,
                route: None,
            }],
            WorkflowOptions::default(),
        )
        .unwrap();
        let events: Vec<Event> = wf
            .run_node(Context::builder().agent(coord.clone()).build(), json!("go"))
            .map(|r| r.unwrap())
            .collect()
            .await;
        let fr = events
            .iter()
            .find_map(|e| {
                e.response.content.as_ref()?.parts.iter().find_map(|p| {
                    p.function_response
                        .as_ref()
                        .filter(|fr| fr.id == "c1")
                        .cloned()
                })
            })
            .expect("single_turn tool FR");
        assert_eq!(fr.name, "worker");
        assert_eq!(
            Value::Object(fr.response),
            json!({"result": "worker answer"})
        );
        let wreq = &worker_model.requests()[0];
        assert!(
            wreq.contents
                .iter()
                .any(|c| c.role == "user" && c.parts.iter().any(|p| p.text == r#"{"request":"q"}"#)),
            "the tool args reach the sub-agent as its seeded input: {:?}",
            wreq.contents
        );
        assert_eq!(coord_model.requests().len(), 2);
    }
}
