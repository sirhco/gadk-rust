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

//! Request-confirmation request processor (port of Go
//! internal/llminternal/request_confirmation_processor.go).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use futures::future::BoxFuture;
// Go toolconfirmation.FunctionCallName / OriginalCallFrom come from Task 5 (C5 D3); no private
// copies.
use gadk_agent::{
    AgentError, InvocationContext, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, Tool,
    original_call_from,
};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::{Event, ToolConfirmation};
use serde_json::Value;

use crate::{
    llm::{flow::Flow, functions::handle_function_calls_with, processors::wrap_err, state::reveal},
    tool::utils::{function_calls, function_responses},
    workflow::route::go_quote,
};

/// One Go handleFunctionCalls dispatch: the resumed original calls (role "user" content, in
/// confirmation-request order) and the confirmation for each original call ID.
#[derive(Debug)]
pub(crate) struct ResumeBatch {
    pub(crate) content: Content,
    pub(crate) confirmations: HashMap<String, ToolConfirmation>,
}

/// Port of Go RequestConfirmationRequestProcessor: re-dispatches the tool calls the user has
/// just confirmed.
pub(crate) fn request_confirmation_processor<'a>(
    ctx: &'a InvocationContext,
    _req: &'a mut LlmRequest,
    f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        if reveal(&*agent).is_none() {
            return Ok(Vec::new());
        }
        let events = ctx.session().events();
        let batches = collect_resume_batches(agent.name(), &events)?;
        dispatch_resume_batches(ctx, f, batches).await
    })
}

/// Runs each batch through handle_function_calls with its confirmations, in order, and returns
/// the non-None response events (Go request_confirmation_processor.go:245-256).
async fn dispatch_resume_batches(
    ctx: &InvocationContext,
    f: &Flow,
    batches: Vec<ResumeBatch>,
) -> Result<Vec<Event>, AgentError> {
    // Go: toolsmap built from f.Tools (set by the tools processor earlier in this step).
    let tools: HashMap<String, Arc<dyn Tool>> = f
        .tools
        .read()
        .iter()
        .map(|t| (t.name().to_owned(), t.clone()))
        .collect();
    let mut out = Vec::new();
    for b in batches {
        // Go passes &model.LLMResponse{Content: parts}; wrap the content in a bare event.
        let call_ev = Event {
            response: LlmResponse {
                content: Some(b.content),
                ..Default::default()
            },
            ..Default::default()
        };
        if let Some(ev) =
            handle_function_calls_with(ctx, f, &tools, &call_ev, None, Some(&b.confirmations))
                .await?
        {
            out.push(ev);
        }
    }
    Ok(out)
}

/// Port of request_confirmation_processor.go:47-257 without the dispatch. Go dispatches each
/// batch while it scans; the scan reads only the `events` snapshot, so collecting the batches
/// first and dispatching them in order afterwards is equivalent.
pub(crate) fn collect_resume_batches(
    agent_name: &str,
    events: &[Event],
) -> Result<Vec<ResumeBatch>, AgentError> {
    let Some((confirmation_responses, confirmation_event_index)) = confirmation_responses(events)?
    else {
        return Ok(Vec::new());
    };
    if confirmation_responses.is_empty() {
        return Ok(Vec::new());
    }

    // Detect tampered confirmations before resuming any tool: a confirmation-call ID that
    // resolves to conflicting original calls across this agent's events is never resumed.
    let mut conflicting: HashSet<String> = HashSet::new();
    let mut original_by_confirmation_id: HashMap<String, String> = HashMap::new();
    for event in events.iter().filter(|e| e.author == agent_name) {
        for fc in function_calls(event.response.content.as_ref()) {
            if !confirmation_responses.contains_key(&fc.id) {
                continue;
            }
            let Ok(original) = original_call_from(fc) else {
                continue;
            };
            let Ok(canonical) = serde_json::to_string(&original) else {
                continue;
            };
            match original_by_confirmation_id.get(&fc.id) {
                Some(prev) => {
                    if *prev != canonical {
                        conflicting.insert(fc.id.clone());
                    }
                }
                None => {
                    original_by_confirmation_id.insert(fc.id.clone(), canonical);
                }
            }
        }
    }

    let mut batches = Vec::new();
    for k in (0..events.len().saturating_sub(1)).rev() {
        let event = &events[k];
        // Only this agent can ask this agent's user for confirmation.
        if event.author != agent_name {
            continue;
        }
        let calls = function_calls(event.response.content.as_ref());
        if calls.is_empty() {
            continue;
        }
        let mut to_resume: HashMap<String, (ToolConfirmation, FunctionCall)> = HashMap::new();
        let mut resume_order: Vec<String> = Vec::new();
        for fc in calls {
            let Some(confirmation) = confirmation_responses.get(&fc.id) else {
                continue;
            };
            if conflicting.contains(&fc.id) {
                continue;
            }
            let Ok(original) = original_call_from(fc) else {
                continue;
            };
            if !to_resume.contains_key(&original.id) {
                resume_order.push(original.id.clone());
            }
            to_resume.insert(original.id.clone(), (confirmation.clone(), original));
        }
        if to_resume.is_empty() {
            continue;
        }

        // Remove the tools that have already been answered after the confirmation event.
        for later in events[confirmation_event_index + 1..].iter().rev() {
            let responses = function_responses(later.response.content.as_ref());
            if responses.is_empty() {
                continue;
            }
            for resp in responses {
                to_resume.remove(&resp.id);
            }
            if to_resume.is_empty() {
                break;
            }
        }
        if to_resume.is_empty() {
            continue;
        }

        // Re-dispatch in request order so the parts and the downstream merge are deterministic.
        let mut parts = Vec::with_capacity(to_resume.len());
        let mut confirmations = HashMap::with_capacity(to_resume.len());
        for id in resume_order {
            let Some((confirmation, call)) = to_resume.remove(&id) else {
                continue;
            };
            parts.push(Part {
                function_call: Some(call),
                ..Default::default()
            });
            confirmations.insert(id, confirmation);
        }
        batches.push(ResumeBatch {
            content: Content {
                role: "user".into(),
                parts,
            },
            confirmations,
        });
    }
    Ok(batches)
}

/// Confirmation-call ID → the user's decision.
type ConfirmationResponses = HashMap<String, ToolConfirmation>;

/// The confirmation responses of the latest user event, keyed by confirmation-call ID, and that
/// event's index. `None` when the latest user event carries no function response (Go returns
/// early).
fn confirmation_responses(
    events: &[Event],
) -> Result<Option<(ConfirmationResponses, usize)>, AgentError> {
    let Some(k) = events.iter().rposition(|e| e.author == "user") else {
        return Ok(None);
    };
    let event = &events[k];
    let responses = function_responses(event.response.content.as_ref());
    if responses.is_empty() {
        return Ok(None);
    }
    let mut out = HashMap::new();
    for fr in responses {
        if fr.name != REQUEST_CONFIRMATION_FUNCTION_CALL_NAME {
            continue;
        }
        let tc: ToolConfirmation = match fr.response.get("response") {
            // ADK web client sends a request always encapsulated in a 'response' key.
            Some(resp) if fr.response.len() == 1 => {
                let Value::String(json) = resp else {
                    return Err(AgentError::other(format!(
                        "error 'response' key found but value is not a string for confirmation function response for event id {}",
                        go_quote(&event.id)
                    )));
                };
                serde_json::from_str(json).map_err(|e| {
                    wrap_err(
                        format!(
                            "error 'response' key found but failed unmarshalling confirmation function response for event id {}",
                            go_quote(&event.id)
                        ),
                        e,
                    )
                })?
            }
            _ => serde_json::from_value(Value::Object(fr.response.clone())).map_err(|e| {
                wrap_err(
                    format!(
                        "error failed unmarshalling confirmation function response for event id {}",
                        go_quote(&event.id)
                    ),
                    e,
                )
            })?,
        };
        out.insert(fr.id.clone(), tc);
    }
    Ok(Some((out, k)))
}

#[cfg(test)]
mod tests {
    use std::{
        any::Any,
        sync::atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use gadk_agent::{Context, RunnableTool, ToolError};
    use gadk_genai::FunctionResponse;
    use gadk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
    use pretty_assertions::assert_eq;
    use serde_json::{Map, json};

    use super::*;
    use crate::llm::{config::LlmAgentConfig, processors::test_llm_agent};

    const AGENT: &str = "agent";

    fn event(author: &str, role: &str, parts: Vec<Part>) -> Event {
        Event {
            author: author.into(),
            response: LlmResponse {
                content: Some(Content {
                    role: role.into(),
                    parts,
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn original(id: &str) -> FunctionCall {
        FunctionCall {
            id: id.into(),
            name: format!("tool_{id}"),
            args: json!({"x": id}).as_object().unwrap().clone(),
            ..Default::default()
        }
    }

    /// A confirmation-request call `conf_id` wrapping `orig` (Go toolconfirmation shape).
    fn request(conf_id: &str, orig: &FunctionCall) -> Part {
        let mut args = Map::new();
        args.insert(
            "originalFunctionCall".into(),
            serde_json::to_value(orig).unwrap(),
        );
        args.insert("toolConfirmation".into(), json!({"hint": "ok?"}));
        Part {
            function_call: Some(FunctionCall {
                id: conf_id.into(),
                name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.into(),
                args,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn confirm(conf_id: &str) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                id: conf_id.into(),
                name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.into(),
                response: json!({"confirmed": true}).as_object().unwrap().clone(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn call_part(fc: FunctionCall) -> Part {
        Part {
            function_call: Some(fc),
            ..Default::default()
        }
    }

    #[test]
    fn collect_resume_batches_in_request_order() {
        let (a, b) = (original("a"), original("b"));
        let events = vec![
            event(AGENT, "model", vec![request("c2", &b), request("c1", &a)]),
            event("user", "user", vec![confirm("c1"), confirm("c2")]),
        ];
        let batches = collect_resume_batches(AGENT, &events).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].content.role, "user");
        assert_eq!(batches[0].content.parts, vec![call_part(b), call_part(a)]);
        let mut keys: Vec<_> = batches[0].confirmations.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["a", "b"]);
        assert!(batches[0].confirmations["a"].confirmed);
    }

    #[test]
    fn conflicting_confirmation_not_resumed() {
        let mut forged = original("a");
        forged.args = json!({"x": "forged"}).as_object().unwrap().clone();
        let events = vec![
            event(AGENT, "model", vec![request("c1", &original("a"))]),
            event(AGENT, "model", vec![request("c1", &forged)]),
            event("user", "user", vec![confirm("c1")]),
        ];
        assert!(collect_resume_batches(AGENT, &events).unwrap().is_empty());
    }

    #[test]
    fn foreign_author_ignored() {
        let events = vec![
            event("peer", "model", vec![request("c1", &original("a"))]),
            event("user", "user", vec![confirm("c1")]),
        ];
        assert!(collect_resume_batches(AGENT, &events).unwrap().is_empty());
    }

    #[test]
    fn already_answered_skipped() {
        let events = vec![
            event(AGENT, "model", vec![request("c1", &original("a"))]),
            event("user", "user", vec![confirm("c1")]),
            event(
                AGENT,
                "user",
                vec![Part {
                    function_response: Some(FunctionResponse {
                        id: "a".into(),
                        name: "tool_a".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            ),
        ];
        assert!(collect_resume_batches(AGENT, &events).unwrap().is_empty());
    }

    #[test]
    fn response_key_wrapping_decoded() {
        let mut part = confirm("c1");
        part.function_response.as_mut().unwrap().response =
            json!({"response": "{\"confirmed\":true}"})
                .as_object()
                .unwrap()
                .clone();
        let events = vec![
            event(AGENT, "model", vec![request("c1", &original("a"))]),
            event("user", "user", vec![part]),
        ];
        let batches = collect_resume_batches(AGENT, &events).unwrap();
        assert!(batches[0].confirmations["a"].confirmed);

        let mut bad = confirm("c1");
        bad.function_response.as_mut().unwrap().response =
            json!({"response": 1}).as_object().unwrap().clone();
        let events = vec![event("user", "user", vec![bad])];
        let err = collect_resume_batches(AGENT, &events).unwrap_err();
        assert_eq!(
            err.to_string(),
            "error 'response' key found but value is not a string for confirmation function response for event id \"\""
        );
    }

    /// Records whether its call carried a confirmed tool confirmation.
    struct ConfirmProbe {
        confirmed: Arc<AtomicBool>,
    }

    impl Tool for ConfirmProbe {
        fn name(&self) -> &str {
            "t"
        }
        fn description(&self) -> &str {
            "probe"
        }
        fn as_runnable(&self) -> Option<&dyn RunnableTool> {
            Some(self)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[async_trait]
    impl RunnableTool for ConfirmProbe {
        async fn run(&self, ctx: Context, _: Value) -> Result<Map<String, Value>, ToolError> {
            let confirmed = ctx.tool_confirmation().is_some_and(|c| c.confirmed);
            self.confirmed.store(confirmed, Ordering::SeqCst);
            Ok(Map::from_iter([("done".to_owned(), json!(true))]))
        }
    }

    #[tokio::test]
    async fn request_confirmation_resume_dispatches() {
        let fc1 = FunctionCall {
            id: "fc1".into(),
            name: "t".into(),
            args: json!({"x": 1}).as_object().unwrap().clone(),
            ..Default::default()
        };
        let svc = InMemorySessionService::new();
        let created = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: Some("s".into()),
                state: None,
            })
            .await
            .unwrap();
        for mut e in [
            event(AGENT, "model", vec![call_part(fc1.clone())]),
            event(AGENT, "model", vec![request("c1", &fc1)]),
            event("user", "user", vec![confirm("c1")]),
        ] {
            svc.append_event(&*created, &mut e).await.unwrap();
        }
        let sess = svc
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let confirmed = Arc::new(AtomicBool::new(false));
        let t: Arc<dyn Tool> = Arc::new(ConfirmProbe {
            confirmed: confirmed.clone(),
        });
        let agent = test_llm_agent(LlmAgentConfig {
            name: AGENT.into(),
            ..Default::default()
        });
        let ctx = InvocationContext::builder()
            .agent(agent)
            .session(sess)
            .build_invocation();
        let flow = Flow {
            tools: parking_lot::RwLock::new(vec![t]),
            ..Default::default()
        };
        let events = request_confirmation_processor(&ctx, &mut LlmRequest::default(), &flow)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        let parts = &events[0].response.content.as_ref().unwrap().parts;
        assert_eq!(parts.len(), 1);
        let fr = parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr.id, "fc1");
        assert_eq!(
            fr.response,
            json!({"done": true}).as_object().unwrap().clone()
        );
        assert!(
            confirmed.load(Ordering::SeqCst),
            "tool must see the confirmation"
        );
    }
}
