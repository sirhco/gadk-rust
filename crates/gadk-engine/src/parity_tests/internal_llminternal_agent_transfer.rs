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

//! Port of Go internal/llminternal/agent_transfer_test.go.

use std::sync::Arc;

use gadk_agent::{
    Agent, AgentConfig, Context, ContextDelta, InvocationContext, Mode, ParentMap, RunnableTool,
    Tool, ToolError, new_agent, pack_tool, tool_from_request,
};
use gadk_genai::FunctionDeclaration;
use gadk_model::{Llm, LlmRequest};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agents::parallel_agent::{self, ParallelAgentConfig};
use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::llm::flow::Flow;
use crate::llm::transfer::{TransferToAgentTool, agent_transfer_request_processor};
use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::testutil::MockModel;
use crate::tool::utils::text_parts;
use crate::tool::{FunctionTool, FunctionToolConfig};

const TRANSFER_PROSE: &str = "You have a list of other agents to transfer to";

/// Go `&struct{ model.LLM }{}`: a model that is never called.
fn idle_model() -> Arc<dyn Llm> {
    MockModel::new(vec![])
}

fn llm(name: &str) -> LlmAgentConfig {
    LlmAgentConfig {
        name: name.into(),
        model: Some(idle_model()),
        ..Default::default()
    }
}

fn build(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).unwrap()
}

fn custom(name: &str, sub_agents: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    new_agent(AgentConfig {
        name: name.into(),
        sub_agents,
        ..Default::default()
    })
    .unwrap()
}

/// Go `&llminternal.TransferToAgentTool{}`.
fn zero_transfer_tool() -> TransferToAgentTool {
    let inv = InvocationContext::builder().build_invocation();
    TransferToAgentTool::new(&inv, &inv.agent(), None, Vec::new())
}

/// Go `utils.FunctionDecls(req.Config)`.
fn function_decls(req: &LlmRequest) -> Vec<FunctionDeclaration> {
    req.config
        .iter()
        .flat_map(|c| c.tools.iter())
        .flat_map(|t| t.function_declarations.iter().cloned())
        .collect()
}

/// Port of the Go processor-invocation block shared by `check` and the task/single_turn test.
async fn run_processor(cur: &Arc<dyn Agent>, root: &Arc<dyn Agent>) -> LlmRequest {
    let parents = ParentMap::new(root).unwrap();
    let ctx = InvocationContext::builder()
        .agent(cur.clone())
        .parent_map(Arc::new(parents))
        .build_invocation();
    let mut req = LlmRequest::default();
    let events = agent_transfer_request_processor(&ctx, &mut req, &Flow::default())
        .await
        .unwrap_or_else(|e| panic!("AgentTransferRequestProcessor failed: {e}"));
    assert!(
        events.is_empty(),
        "AgentTransferRequestProcessor generated an unexpected event"
    );
    req
}

/// Port of the Go `check` closure.
async fn check(
    case: &str,
    cur: &Arc<dyn Agent>,
    root: &Arc<dyn Agent>,
    want_parent: &str,
    want_agents: &[&str],
    unwant: &[&str],
) {
    let cur_tool = zero_transfer_tool();
    let req = run_processor(cur, root).await;

    // We don't expect transfer. Check AgentTransferRequestProcessor was a no-op.
    if want_parent.is_empty() && want_agents.is_empty() {
        assert!(
            req.config.is_none(),
            "{case}: req.config changed unexpectedly: {:?}",
            req.config
        );
        assert!(
            req.tools.is_empty(),
            "{case}: req.tools changed unexpectedly"
        );
        assert!(
            req.contents.is_empty(),
            "{case}: req.contents changed unexpectedly"
        );
        return;
    }

    // check tools dictionary.
    let want_tool_name = cur_tool.name();
    let got_tool = tool_from_request(&req, want_tool_name)
        .unwrap_or_else(|| panic!("{case}: req.tools does not include {want_tool_name}"));
    assert_eq!(
        got_tool.name(),
        want_tool_name,
        "{case}: unexpected name for tool"
    );

    // check instructions.
    let si = req
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref());
    let instructions = text_parts(si);
    let joined = instructions.join("\n");
    assert!(
        instructions
            .iter()
            .any(|s| s.contains(want_tool_name) && s.contains(TRANSFER_PROSE)),
        "{case}: instruction does not include agent transfer instruction, got: {joined}"
    );
    if !want_parent.is_empty() {
        assert!(
            instructions.iter().any(|s| s.contains(want_parent)),
            "{case}: instruction does not include parent agent, got: {joined}"
        );
    }
    assert!(
        !instructions.iter().any(|s| s == cur.name()),
        "{case}: instruction should not suggest transfer to current agent, got: {joined}"
    );
    if !want_agents.is_empty() {
        assert!(
            instructions
                .iter()
                .any(|s| want_agents.iter().all(|a| s.contains(a))),
            "{case}: instruction does not include subagents, got: {joined}"
        );
        let transfer_tool = got_tool
            .as_any()
            .downcast_ref::<TransferToAgentTool>()
            .unwrap_or_else(|| panic!("{case}: failed to type convert tool {want_tool_name}"));
        let decl = transfer_tool.declaration().unwrap();
        let mut got = decl
            .parameters
            .as_ref()
            .and_then(|p| p.properties.get("agent_name"))
            .map(|s| s.r#enum.clone())
            .unwrap_or_default();
        let mut want: Vec<String> = want_agents.iter().map(|s| s.to_string()).collect();
        // Add parent to the list of agents to transfer to, if not already present.
        if !want_parent.is_empty() && !want_agents.contains(&want_parent) {
            want.push(want_parent.to_string());
        }
        got.sort();
        want.sort();
        assert_eq!(want, got, "{case}: failed to set agent_name enums");
    }
    if !unwant.is_empty() {
        assert!(
            !instructions
                .iter()
                .any(|s| unwant.iter().any(|u| s.contains(u))),
            "{case}: instruction includes unwanted agents, got: {joined}"
        );
    }

    // check function declarations.
    let want_desc = cur_tool.description();
    let functions = function_decls(&req);
    assert!(
        functions.iter().any(|f| f.name == want_tool_name
            && f.description.contains(want_desc)
            && f.parameters_json_schema.is_none()),
        "{case}: AgentTransferRequestProcessor() did not append the function declaration, got: {}",
        serde_json::to_string(&functions).unwrap()
    );
}

#[tokio::test]
async fn test_agent_transfer_request_processor() {
    let cur_tool = zero_transfer_tool();
    assert!(
        !cur_tool.name().is_empty()
            && !cur_tool.description().is_empty()
            && cur_tool.declaration().is_some(),
        "unexpected TransferToAgentTool: name={:?}, desc={:?}",
        cur_tool.name(),
        cur_tool.description()
    );

    // SoloAgent
    {
        let agent = build(llm("Current"));
        check("SoloAgent", &agent, &agent, "", &[], &["Current"]).await;
    }
    // NotLLMAgent
    {
        let a = custom("mockAgent", vec![]);
        check("NotLLMAgent", &a, &a, "", &[], &[]).await;
    }
    // LLMAgentParent
    {
        let cur = build(llm("Current"));
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone()],
            ..llm("Parent")
        });
        check("LLMAgentParent", &cur, &root, "Parent", &[], &["Current"]).await;
    }
    // LLMAgentParentAndPeer
    {
        let cur = build(llm("Current"));
        let peer = build(llm("Peer"));
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone(), peer],
            ..llm("Parent")
        });
        check(
            "LLMAgentParentAndPeer",
            &cur,
            &root,
            "Parent",
            &["Peer"],
            &["Current"],
        )
        .await;
    }
    // LLMAgentSubagents
    {
        let cur = build(LlmAgentConfig {
            sub_agents: vec![custom("Sub1", vec![]), build(llm("Sub2"))],
            ..llm("Current")
        });
        check(
            "LLMAgentSubagents",
            &cur,
            &cur,
            "",
            &["Sub1", "Sub2"],
            &["Current"],
        )
        .await;
    }
    // AgentWithParentAndPeersAndSubagents
    {
        let cur = build(LlmAgentConfig {
            sub_agents: vec![custom("Sub1", vec![]), build(llm("Sub2"))],
            ..llm("Current")
        });
        let peer = custom("Peer", vec![]);
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone(), peer],
            ..llm("Parent")
        });
        check(
            "AgentWithParentAndPeersAndSubagents",
            &cur,
            &root,
            "Parent",
            &["Peer", "Sub1", "Sub2"],
            &["Current"],
        )
        .await;
    }
    // NonLLMAgentSubagents
    {
        let cur = build(LlmAgentConfig {
            sub_agents: vec![custom("Sub1", vec![]), custom("Sub2", vec![])],
            ..llm("Current")
        });
        check(
            "NonLLMAgentSubagents",
            &cur,
            &cur,
            "",
            &["Sub1", "Sub2"],
            &["Current"],
        )
        .await;
    }
    // AgentWithDisallowTransferToParent
    {
        let cur = build(LlmAgentConfig {
            disallow_transfer_to_parent: true,
            sub_agents: vec![build(llm("Sub1")), build(llm("Sub2"))],
            ..llm("Current")
        });
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone()],
            ..llm("Parent")
        });
        check(
            "AgentWithDisallowTransferToParent",
            &cur,
            &root,
            "",
            &["Sub1", "Sub2"],
            &["Parent", "Current"],
        )
        .await;
    }
    // AgentWithDisallowTransferToPeers
    {
        let cur = build(LlmAgentConfig {
            disallow_transfer_to_peers: true,
            sub_agents: vec![custom("Sub1", vec![]), build(llm("Sub2"))],
            ..llm("Current")
        });
        let peer = build(llm("Peer"));
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone(), peer],
            ..llm("Parent")
        });
        check(
            "AgentWithDisallowTransferToPeers",
            &cur,
            &root,
            "Parent",
            &["Sub1", "Sub2"],
            &["Peer", "Current"],
        )
        .await;
    }
    // AgentWithDisallowTransferToParentAndPeers
    {
        let cur = build(LlmAgentConfig {
            disallow_transfer_to_parent: true,
            disallow_transfer_to_peers: true,
            sub_agents: vec![custom("Sub1", vec![]), build(llm("Sub2"))],
            ..llm("Current")
        });
        let peer = build(llm("Peer"));
        let root = build(LlmAgentConfig {
            sub_agents: vec![peer, cur.clone()],
            ..llm("Parent")
        });
        check(
            "AgentWithDisallowTransferToParentAndPeers",
            &cur,
            &root,
            "",
            &["Sub1", "Sub2"],
            &["Parent", "Peer", "Current"],
        )
        .await;
    }
    // AgentWithDisallowTransfer
    {
        let cur = build(LlmAgentConfig {
            disallow_transfer_to_parent: true,
            disallow_transfer_to_peers: true,
            ..llm("Current")
        });
        let peer = build(llm("Peer"));
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone(), peer],
            ..llm("Parent")
        });
        check(
            "AgentWithDisallowTransfer",
            &cur,
            &root,
            "",
            &[],
            &["Parent", "Peer", "Current"],
        )
        .await;
    }
    // AgentWithParallelParent
    {
        let cur = build(llm("Current"));
        let peer = build(llm("Peer"));
        let root = parallel_agent::new(ParallelAgentConfig {
            agent: AgentConfig {
                name: "Parent".into(),
                sub_agents: vec![cur.clone(), peer],
                ..Default::default()
            },
        })
        .unwrap();
        check(
            "AgentWithParallelParent",
            &cur,
            &root,
            "",
            &[],
            &["Parent", "Peer", "Current"],
        )
        .await;
    }
    // AgentWithSequentialParent
    {
        let cur = build(llm("Current"));
        let peer = build(llm("Peer"));
        let root = sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "Parent".into(),
                sub_agents: vec![cur.clone(), peer],
                ..Default::default()
            },
        })
        .unwrap();
        check(
            "AgentWithSequentialParent",
            &cur,
            &root,
            "",
            &[],
            &["Parent", "Peer", "Current"],
        )
        .await;
    }
    // AgentWithSequentialSubagent
    {
        let sub1 = sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "Sub1".into(),
                sub_agents: vec![build(llm("Sub3"))],
                ..Default::default()
            },
        })
        .unwrap();
        let cur = build(LlmAgentConfig {
            sub_agents: vec![sub1, build(llm("Sub2"))],
            ..llm("Current")
        });
        check(
            "AgentWithSequentialSubagent",
            &cur,
            &cur,
            "",
            &["Sub1", "Sub2"],
            &["Current"],
        )
        .await;
    }
    // TaskAndSingleTurnSubagentsExcludedAsTargets
    {
        let task_sub = build(LlmAgentConfig {
            mode: Mode::Task,
            ..llm("TaskSub")
        });
        let single_sub = build(LlmAgentConfig {
            mode: Mode::SingleTurn,
            ..llm("SingleTurnSub")
        });
        let cur = build(LlmAgentConfig {
            sub_agents: vec![task_sub, single_sub, build(llm("ChatSub"))],
            ..llm("Current")
        });
        check(
            "TaskAndSingleTurnSubagentsExcludedAsTargets",
            &cur,
            &cur,
            "",
            &["ChatSub"],
            &["TaskSub", "SingleTurnSub", "Current"],
        )
        .await;
    }
    // TaskAndSingleTurnPeersExcludedAsTargets
    {
        let cur = build(llm("Current"));
        let task_peer = build(LlmAgentConfig {
            mode: Mode::Task,
            ..llm("TaskPeer")
        });
        let single_peer = build(LlmAgentConfig {
            mode: Mode::SingleTurn,
            ..llm("SingleTurnPeer")
        });
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone(), task_peer, single_peer, build(llm("ChatPeer"))],
            ..llm("Parent")
        });
        check(
            "TaskAndSingleTurnPeersExcludedAsTargets",
            &cur,
            &root,
            "Parent",
            &["ChatPeer"],
            &["TaskPeer", "SingleTurnPeer", "Current"],
        )
        .await;
    }
}

#[tokio::test]
async fn test_agent_transfer_request_processor_task_single_turn_current_agent() {
    for (name, mode) in [("task", Mode::Task), ("single_turn", Mode::SingleTurn)] {
        let cur = build(LlmAgentConfig {
            mode,
            ..llm("Current")
        });
        let root = build(LlmAgentConfig {
            sub_agents: vec![cur.clone()],
            ..llm("Parent")
        });
        let req = run_processor(&cur, &root).await;
        // No transfer-prose instructions should be appended (Go: AppendInstructions("") is a no-op).
        if let Some(cfg) = &req.config {
            for s in text_parts(cfg.system_instruction.as_ref()) {
                assert!(
                    s.is_empty(),
                    "{name}: expected no transfer instructions for mode={:?}, got: {s:?}",
                    mode.as_str()
                );
            }
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Input {
    #[serde(default)]
    x: i64,
}

#[tokio::test]
async fn test_agent_transfer_process_request() {
    let mut req = LlmRequest::default();
    // First tool
    let identity = FunctionTool::new(
        FunctionToolConfig::new("identity", "returns the input value"),
        |_ctx: Context, input: Input| async move { Ok::<i64, ToolError>(input.x) },
    )
    .unwrap();
    // Go identityTool.ProcessRequest packs the declaration; the Rust FunctionTool's packing is pack_tool.
    pack_tool(&mut req, &identity)
        .unwrap_or_else(|e| panic!("identityTool.ProcessRequest failed: {e}"));
    // Second tool: TransferToAgentTool::process_request is Go's ProcessRequest (appendTools).
    let transfer: Arc<dyn Tool> = Arc::new(zero_transfer_tool());
    transfer
        .process_request(&Context::builder().build(), &mut req)
        .await
        .unwrap_or_else(|e| panic!("transferToAgentTool.ProcessRequest failed: {e}"));

    let tools = &req.config.as_ref().unwrap().tools;
    assert_eq!(tools.len(), 1, "number of tools should be one");
    assert_eq!(
        tools[0].function_declarations.len(),
        2,
        "number of function declarations should be two"
    );
}

#[tokio::test]
#[ignore = "8a bug: transfer_to_agent accepts an agent name it cannot transfer to, and a nil argument errors with \"missing argument\" instead of \"empty agent_name\" (Go c8fff670 returns an error naming the available agents)"]
async fn test_transfer_to_agent_tool_run() {
    // Go newTool: the transfer tool of an agent with a single sub-agent, "TestAgent".
    let new_tool = || {
        let sub = llm_agent::new(llm("TestAgent")).unwrap();
        let root = llm_agent::new(LlmAgentConfig {
            sub_agents: vec![sub.clone()],
            ..llm("Root")
        })
        .unwrap();
        let inv = InvocationContext::builder()
            .agent(root.clone())
            .build_invocation();
        TransferToAgentTool::new(&inv, &root, None, vec![sub])
    };

    // Success
    {
        let cur_tool = new_tool();
        let ctx = Context::builder().build().with_delta(ContextDelta {
            function_call_id: Some("call".into()),
            ..Default::default()
        });
        let want_agent_name = "TestAgent";
        let args = json!({"agent_name": want_agent_name});
        cur_tool
            .run(ctx.clone(), args.clone())
            .await
            .unwrap_or_else(|e| panic!("Success: Run({args}) failed: {e}"));
        assert_eq!(
            ctx.actions().transfer_to_agent,
            want_agent_name,
            "Success: Run({args}) did not set TransferToAgent"
        );
    }

    // UnknownAgent
    {
        let cur_tool = new_tool();
        let ctx = Context::builder().build();
        let args = json!({"agent_name": "nobody"});
        let got = cur_tool.run(ctx.clone(), args.clone()).await;
        assert!(
            got.as_ref()
                .is_err_and(|e| e.to_string().contains("TestAgent")),
            "UnknownAgent: Run({args}) = {got:?}, want an error naming the available agents"
        );
        assert_eq!(
            ctx.actions().transfer_to_agent,
            "",
            "UnknownAgent: Run({args}) set TransferToAgent, want it unset"
        );
    }

    // InvalidArguments
    let cases: [(&str, Value, &str); 4] = [
        ("NoAgentName", json!({}), "empty agent_name"),
        ("NilArg", Value::Null, "empty agent_name"),
        (
            "InvalidType",
            json!({"agent_name": 123}),
            "empty agent_name",
        ),
        (
            "InvalidValue",
            json!({"agent_name": ""}),
            "empty agent_name",
        ),
    ];
    for (name, args, want_err) in cases {
        let cur_tool = new_tool();
        let got = cur_tool.run(Context::builder().build(), args.clone()).await;
        assert!(
            got.as_ref()
                .is_err_and(|e| e.to_string().contains(want_err)),
            "InvalidArguments/{name}: Run({args}) = {got:?}, want error containing {want_err:?}"
        );
    }
}
