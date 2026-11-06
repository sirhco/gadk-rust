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

//! Port of Go internal/llminternal/agent_transfer.go: the agent-transfer request processor, the
//! `transfer_to_agent` tool and transfer-target selection.

use std::{any::Any, collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gadk_agent::{
    Agent, AgentError, Context, InvocationContext, Mode, RunnableTool, Tool, ToolError,
};
use gadk_genai::{FunctionDeclaration, Schema, Tool as GenaiTool, Type};
use gadk_model::LlmRequest;
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::{
    llm::{
        flow::Flow,
        mode::{is_untransferable_mode, mode_for},
        processors::go_sprint,
        state::reveal,
    },
    tool::utils::{append_instructions, go_type_name},
};

pub(crate) const TRANSFER_TO_AGENT_TOOL_NAME: &str = "transfer_to_agent";

/// Port of agent_transfer.go:69 AgentTransferRequestProcessor.
pub(crate) fn agent_transfer_request_processor<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        if !should_use_auto_flow(&*agent) {
            return Ok(Vec::new());
        }
        let parent = ctx.parent_map().and_then(|pm| pm.parent(agent.name()));
        let targets = transfer_targets(ctx, &agent, parent.as_ref());
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let tool = TransferToAgentTool::new(ctx, &agent, parent.as_ref(), targets);
        // Go appends the (possibly empty) instructions unconditionally. For task/single_turn
        // agents this appends "" and yields a trailing "\n\n" on an existing SI; kept for wire
        // parity.
        append_instructions(req, &[tool.instructions.as_str()]);
        append_tools(req, &[Arc::new(tool) as Arc<dyn Tool>])?;
        Ok(Vec::new())
    })
}

/// Appendix C: Go transferTargets(ctx.Agent(), parents[ctx.Agent().Name()]), the parent looked
/// up in `ctx.parent_map()`.
pub(crate) fn transfer_target_agents(
    ctx: &InvocationContext,
    agent: &Arc<dyn Agent>,
) -> Vec<Arc<dyn Agent>> {
    let parent = ctx.parent_map().and_then(|pm| pm.parent(agent.name()));
    transfer_targets(ctx, agent, parent.as_ref())
}

/// Port of agent_transfer.go:188 transferTargets.
fn transfer_targets(
    ctx: &InvocationContext,
    cur: &Arc<dyn Agent>,
    parent: Option<&Arc<dyn Agent>>,
) -> Vec<Arc<dyn Agent>> {
    let mut targets: Vec<Arc<dyn Agent>> = cur
        .sub_agents()
        .iter()
        .filter(|s| !is_untransferable_mode(ctx, &***s))
        .cloned()
        .collect();
    let Some(parent) = parent.filter(|p| reveal(&***p).is_some()) else {
        return targets;
    };
    // Go dereferences llmAgent.internal() here; a non-LLM current agent would panic in Go. Rust
    // returns the sub-agents only.
    let Some(cur_state) = reveal(&**cur) else {
        return targets;
    };
    if !cur_state.cfg.disallow_transfer_to_parent {
        targets.push(parent.clone());
    }
    // Peer transfer needs DisallowTransferToPeers false and a parent that is also AutoFlow.
    if !cur_state.cfg.disallow_transfer_to_peers && should_use_auto_flow(&**parent) {
        for peer in parent.sub_agents() {
            if peer.name() == cur.name() || is_untransferable_mode(ctx, &**peer) {
                continue;
            }
            targets.push(peer.clone());
        }
    }
    targets
}

/// Port of agent_transfer.go:251 shouldUseAutoFlow.
pub(crate) fn should_use_auto_flow(a: &dyn Agent) -> bool {
    reveal(a).is_some_and(|s| {
        !a.sub_agents().is_empty()
            || !s.cfg.disallow_transfer_to_parent
            || !s.cfg.disallow_transfer_to_peers
    })
}

/// Port of agent_transfer.go:261 appendTools (NOT pack_tool: different error texts). `req.tools`
/// stores `Arc::new(Arc<dyn Tool>)`, the boxing `pack_tool` uses, so `tool_from_request`
/// recovers it.
pub(crate) fn append_tools(
    req: &mut LlmRequest,
    tools: &[Arc<dyn Tool>],
) -> Result<(), AgentError> {
    let mut decls = Vec::new();
    for (i, t) in tools.iter().enumerate() {
        if t.name().is_empty() {
            return Err(AgentError::other(format!(
                "tools[{i}] tool without name: {}",
                t.name()
            )));
        }
        if req.tools.contains_key(t.name()) {
            return Err(AgentError::other(format!(
                "tools[{i}] duplicate tool: {:?}",
                t.name()
            )));
        }
        req.tools.insert(
            t.name().to_owned(),
            Arc::new(t.clone()) as Arc<dyn Any + Send + Sync>,
        );
        if let Some(d) = t.declaration() {
            decls.push(d);
        }
    }
    if decls.is_empty() {
        return Ok(());
    }
    let cfg = req.config.get_or_insert_with(Default::default);
    match cfg
        .tools
        .iter_mut()
        .find(|gt| !gt.function_declarations.is_empty())
    {
        Some(gt) => gt.function_declarations.extend(decls),
        None => cfg.tools.push(GenaiTool {
            function_declarations: decls,
            ..Default::default()
        }),
    }
    Ok(())
}

/// Port of agent_transfer.go:102 TransferToAgentTool: transfers control to another agent.
#[derive(Clone)]
pub(crate) struct TransferToAgentTool {
    pub(crate) instructions: String,
    supported_agents: Vec<Arc<dyn Agent>>,
}

impl TransferToAgentTool {
    /// Port of NewTransferToAgentTool (line 111) + instructionsForTransferToAgent (line 312).
    /// The declaration ships either way; only the instructions depend on the resolved mode.
    pub(crate) fn new(
        ctx: &InvocationContext,
        cur: &Arc<dyn Agent>,
        parent: Option<&Arc<dyn Agent>>,
        targets: Vec<Arc<dyn Agent>>,
    ) -> Self {
        // RESOLVED mode (Go ModeFor): task / single_turn agents reach callees via FC delegation.
        let instructions = match mode_for(ctx, &**cur) {
            Mode::Task | Mode::SingleTurn => String::new(),
            _ => {
                let parent = if reveal(&**cur).is_some_and(|s| s.cfg.disallow_transfer_to_parent) {
                    None
                } else {
                    parent
                };
                render_transfer_instructions(&targets, parent)
            }
        };
        Self {
            instructions,
            supported_agents: targets,
        }
    }
}

impl Tool for TransferToAgentTool {
    fn name(&self) -> &str {
        TRANSFER_TO_AGENT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Transfer the question to another agent.\nThis tool hands off control to another agent when it's more suitable to answer the user's question according to the agent's description."
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.name().into(),
            description: self.description().into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: BTreeMap::from([(
                    "agent_name".to_owned(),
                    Schema {
                        r#type: Some(Type::String),
                        description: "the agent name to transfer to".into(),
                        r#enum: self
                            .supported_agents
                            .iter()
                            .map(|a| a.name().to_owned())
                            .collect(),
                        ..Default::default()
                    },
                )]),
                required: vec!["agent_name".into()],
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    /// Port of agent_transfer.go:165 ProcessRequest (= appendTools). The flow never calls it
    /// (the processor appends the tool itself, and toolPreprocess walks only the agent's tools);
    /// it is ported so TestAgentTransfer_ProcessRequest is a straight port. The error text is
    /// append_tools' text unchanged.
    fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        append_tools(req, &[Arc::new(self.clone()) as Arc<dyn Tool>])
            .map_err(|e| ToolError::Execution(e.to_string().into()))
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for TransferToAgentTool {
    /// Port of agent_transfer.go:170 Run.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let m = match &args {
            Value::Null => return Err(ToolError::InvalidArgument("missing argument".into())),
            Value::Object(m) => m,
            other => {
                return Err(ToolError::InvalidArgument(format!(
                    "unexpected args type: {}",
                    go_type_name(other)
                )));
            }
        };
        let agent = m
            .get("agent_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidArgument(format!("empty agent_name: {}", go_sprint(&args)))
            })?;
        ctx.update_actions(|a| a.transfer_to_agent = agent.to_owned());
        Ok(Map::new())
    }
}

/// Go safehtml HTMLEscaped: control chars (U+0000-0008, U+000B, U+000E-001F, U+007F-009F) and
/// Unicode noncharacters (U+FDD0-FDEF, U+xFFFE/U+xFFFF) → U+FFFD, then html.EscapeString
/// (& < > ' ").
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        let bad = matches!(u, 0x00..=0x08 | 0x0B | 0x0E..=0x1F | 0x7F..=0x9F | 0xFDD0..=0xFDEF)
            || (u & 0xFFFE) == 0xFFFE;
        match if bad { '\u{FFFD}' } else { c } {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&#39;"),
            '"' => out.push_str("&#34;"),
            other => out.push(other),
        }
    }
    out
}

/// Go `transferToAgentPromptTmpl` (agentTransferInstructionTemplate, line 361) rendered by
/// `safehtml/template`, byte for byte.
fn render_transfer_instructions(
    targets: &[Arc<dyn Agent>],
    parent: Option<&Arc<dyn Agent>>,
) -> String {
    let mut s = String::from("\nYou have a list of other agents to transfer to:\n\n");
    for t in targets {
        s.push_str(&format!(
            "\nAgent name: {}\nAgent description: {}\n\n",
            html_escape(t.name()),
            html_escape(t.description())
        ));
    }
    s.push_str("\nIf you are the best to answer the question according to your description,\nyou can answer it.\n\nIf another agent is better for answering the question according to its\ndescription, call `transfer_to_agent` function to transfer the question to that\nagent. When transferring, do not generate any text other than the function\ncall.\n\n**NOTE**: the only available agents for `transfer_to_agent` function are\n");
    s.push_str(&html_escape(&format_targets(targets)));
    s.push_str(".\n");
    if let Some(p) = parent {
        s.push_str(&format!(
            "\nIf neither you nor the other agents are best for the question, transfer to your parent agent {}.\n",
            html_escape(p.name())
        ));
    }
    s
}

/// Port of agent_transfer.go:345 formatTargets: names sorted, backticked, ", "-joined.
fn format_targets(targets: &[Arc<dyn Agent>]) -> String {
    let mut names: Vec<&str> = targets.iter().map(|t| t.name()).collect();
    names.sort_unstable();
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use gadk_agent::ParentMap;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::llm::{config::LlmAgentConfig, mode::with_bound_mode, processors::test_llm_agent};

    fn llm(name: &str, description: &str, mode: Mode, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        test_llm_agent(LlmAgentConfig {
            name: name.into(),
            description: description.into(),
            mode,
            sub_agents: subs,
            ..Default::default()
        })
    }

    fn names(agents: &[Arc<dyn Agent>]) -> Vec<String> {
        agents.iter().map(|a| a.name().to_owned()).collect()
    }

    #[test]
    fn transfer_instructions_match_go_template() {
        let targets = vec![
            llm("a<b", "it's \"x\" & y + z\u{1}", Mode::Unset, vec![]),
            llm("c", "", Mode::Unset, vec![]),
        ];
        let root = llm("root", "", Mode::Unset, vec![]);
        assert_eq!(
            render_transfer_instructions(&targets, Some(&root)),
            "\nYou have a list of other agents to transfer to:\n\n\nAgent name: a&lt;b\nAgent description: it&#39;s &#34;x&#34; &amp; y + z\u{FFFD}\n\n\nAgent name: c\nAgent description: \n\n\nIf you are the best to answer the question according to your description,\nyou can answer it.\n\nIf another agent is better for answering the question according to its\ndescription, call `transfer_to_agent` function to transfer the question to that\nagent. When transferring, do not generate any text other than the function\ncall.\n\n**NOTE**: the only available agents for `transfer_to_agent` function are\n`a&lt;b`, `c`.\n\nIf neither you nor the other agents are best for the question, transfer to your parent agent root.\n"
        );
    }

    fn tool(targets: Vec<Arc<dyn Agent>>) -> TransferToAgentTool {
        TransferToAgentTool {
            instructions: String::new(),
            supported_agents: targets,
        }
    }

    #[test]
    fn append_tools_rejects_duplicate() {
        let t: Arc<dyn Tool> = Arc::new(tool(vec![]));
        let mut req = LlmRequest::default();
        append_tools(&mut req, std::slice::from_ref(&t)).unwrap();
        let err = append_tools(&mut req, &[t]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "tools[0] duplicate tool: \"transfer_to_agent\""
        );
    }

    #[test]
    fn process_request_appends_declaration() {
        let t = tool(vec![llm("x", "", Mode::Unset, vec![])]);
        let ctx = Context::builder().build();
        let mut req = LlmRequest::default();
        t.process_request(&ctx, &mut req).unwrap();
        assert!(req.tools.contains_key(TRANSFER_TO_AGENT_TOOL_NAME));
        assert!(gadk_agent::tool_from_request(&req, TRANSFER_TO_AGENT_TOOL_NAME).is_some());
        let decls: Vec<_> = req
            .config
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .flat_map(|g| g.function_declarations.iter())
            .collect();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, TRANSFER_TO_AGENT_TOOL_NAME);
        let err = t.process_request(&ctx, &mut req).unwrap_err();
        assert_eq!(
            err.to_string(),
            "tools[0] duplicate tool: \"transfer_to_agent\""
        );
    }

    #[test]
    fn targets_parent_and_peers_filtered_by_declared_mode() {
        let d = llm("d", "", Mode::Unset, vec![]);
        let a = llm("a", "", Mode::Chat, vec![d]);
        let b = llm("b", "", Mode::Task, vec![]);
        let c = llm("c", "", Mode::SingleTurn, vec![]);
        let root = llm("root", "", Mode::Unset, vec![a.clone(), b, c]);
        let pm = Arc::new(ParentMap::new(&root).unwrap());
        let ctx = InvocationContext::builder()
            .agent(a.clone())
            .parent_map(pm)
            .build_invocation();
        assert_eq!(names(&transfer_target_agents(&ctx, &a)), ["d", "root"]);
    }

    #[tokio::test]
    async fn single_turn_agent_gets_declaration_but_no_instructions() {
        let child = llm("child", "", Mode::Unset, vec![]);
        let agent = llm("worker", "", Mode::Unset, vec![child]);
        let ctx = InvocationContext::builder()
            .agent(agent.clone())
            .build_invocation();
        let ctx = with_bound_mode(&ctx, agent.name(), agent.agent_state(), Mode::SingleTurn);
        let mut req = LlmRequest::default();
        agent_transfer_request_processor(&ctx, &mut req, &Flow::default())
            .await
            .unwrap();
        assert!(req.tools.contains_key(TRANSFER_TO_AGENT_TOOL_NAME));
        let si = req.config.unwrap().system_instruction.unwrap();
        assert_eq!(si.parts.len(), 1);
        assert_eq!(si.parts[0].text, "");
    }

    #[tokio::test]
    async fn run_sets_transfer_action() {
        let t = tool(vec![]);
        let ctx = Context::builder()
            .build()
            .with_delta(gadk_agent::ContextDelta {
                actions: Some(Arc::new(parking_lot::Mutex::new(
                    gadk_session::EventActions::default(),
                ))),
                ..Default::default()
            });
        let runnable = t.as_runnable().unwrap();
        runnable
            .run(ctx.clone(), json!({"agent_name": "x"}))
            .await
            .unwrap();
        assert_eq!(ctx.actions().transfer_to_agent, "x");
        let err = runnable
            .run(ctx.clone(), json!({"agent_name": ""}))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "empty agent_name: map[agent_name:]");
        let err = runnable.run(ctx, Value::Null).await.unwrap_err();
        assert_eq!(err.to_string(), "missing argument");
    }
}
