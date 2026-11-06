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

//! Port of ADK-Go workflow/agent_node_modes_test.go (external→internal).

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{Agent, AgentConfig, Context, Mode, RunConfig};
use gadk_genai::Content;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use serde_json::Value;

use crate::agents::parallel_agent::{self, ParallelAgentConfig};
use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::workflow::{AgentNode, Node, NodeConfig};

/// Go `for ev, err := range seq { if err != nil { t.Fatal(err) } }`: every event (with or
/// without content), stopping at the first error.
async fn drain_events(s: gadk_agent::EventStream) -> Result<Vec<Event>, gadk_agent::AgentError> {
    futures::TryStreamExt::try_collect(s).await
}

/// Port of Go `scriptedLLM`: one response per call, then a terminal "done".
struct ScriptedLlm {
    turns: Vec<LlmResponse>,
    call_idx: parking_lot::Mutex<usize>,
}

impl ScriptedLlm {
    fn text(text: &str) -> Arc<Self> {
        Arc::new(Self {
            turns: vec![LlmResponse {
                content: Some(Content::from_text(text, "model")),
                ..Default::default()
            }],
            call_idx: parking_lot::Mutex::new(0),
        })
    }
}

impl Llm for ScriptedLlm {
    fn name(&self) -> &str {
        "scripted-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut idx = self.call_idx.lock();
        let resp = self
            .turns
            .get(*idx)
            .cloned()
            .unwrap_or_else(|| LlmResponse {
                content: Some(Content::from_text("done", "model")),
                ..Default::default()
            });
        *idx += 1;
        futures::stream::iter([Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go `newRunnableNodeContext`: in-memory session, agent, invocation "inv-test",
/// StreamingMode none, no user content.
async fn new_runnable_node_context(a: &Arc<dyn Agent>) -> Context {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let session = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .expect("create session");
    Context::builder()
        .session_service(svc)
        .session(session)
        .agent(a.clone())
        .invocation_id("inv-test")
        .run_config(RunConfig::default())
        .build()
}

/// Port of Go `fixedTextAgent`.
fn fixed_text_agent(name: &str, text: &str) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        model: Some(ScriptedLlm::text(text) as Arc<dyn Llm>),
        ..Default::default()
    })
    .expect("llmagent.New")
}

/// Port of Go `assertSubAgentOutputsNotSynthesized`: a composite's sub-agent events must not
/// be promoted to node outputs (each would trip the one-output-per-node rule).
async fn assert_sub_agent_outputs_not_synthesized(
    node: Arc<dyn Node>,
    node_name: &str,
    ctx: Context,
    want_authors: &[&str],
) {
    let events = drain_events(node.run(ctx, Value::Null))
        .await
        .expect("node.Run yielded err");
    let mut seen = HashSet::new();
    for ev in events.iter().filter(|e| !e.response.partial) {
        if !ev.author.is_empty() && ev.author != node_name {
            seen.insert(ev.author.clone());
            assert_eq!(
                ev.output, None,
                "sub-agent {:?} event Output, want None",
                ev.author
            );
            assert!(
                !ev.node_info.as_ref().is_some_and(|n| n.message_as_output),
                "sub-agent {:?} event MessageAsOutput = true, want false",
                ev.author
            );
        }
    }
    for a in want_authors {
        assert!(
            seen.contains(*a),
            "missing final event from sub-agent {a:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_new_agent_node_name_inherited_from_agent() {
    const AGENT_NAME: &str = "my_inner_agent";
    let a = llm_agent::new(LlmAgentConfig {
        name: AGENT_NAME.into(),
        mode: Mode::Chat,
        ..Default::default()
    })
    .expect("llmagent.New");
    let node = AgentNode::new(a, NodeConfig::default()).expect("NewAgentNode");
    assert_eq!(
        node.name(),
        AGENT_NAME,
        "node.Name() must inherit from wrapped agent"
    );
}

// Task-mode agents' natural-language text must NOT be promoted to the node Output: their
// authoritative output is set via finish_task. Promoting plain text would make the chat
// coordinator close the delegation prematurely.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_task_mode_does_not_promote_model_text() {
    const USER_FACING_TEXT: &str = "Please tell me what you'd like to order.";
    let task_agent = llm_agent::new(LlmAgentConfig {
        name: "order_collector".into(),
        mode: Mode::Task,
        model: Some(ScriptedLlm::text(USER_FACING_TEXT) as Arc<dyn Llm>),
        ..Default::default()
    })
    .expect("llmagent.New");
    let node: Arc<dyn Node> =
        AgentNode::new(task_agent.clone(), NodeConfig::default()).expect("NewAgentNode");
    let ctx = new_runnable_node_context(&task_agent).await;

    let events = drain_events(node.run(ctx, Value::Null))
        .await
        .expect("node.Run yielded err");
    let last = events
        .iter()
        .rfind(|e| !e.response.partial)
        .expect("no non-partial events yielded");
    assert_eq!(last.output, None, "task-mode text must not be promoted");
    assert!(
        !last.node_info.as_ref().is_some_and(|n| n.message_as_output),
        "NodeInfo.MessageAsOutput = true, want false/unset for task-mode text"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_sequential_agent_as_node_no_multiple_outputs() {
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq_node".into(),
            sub_agents: vec![
                fixed_text_agent("seq_a", "SEQ_A"),
                fixed_text_agent("seq_b", "SEQ_B"),
            ],
            ..Default::default()
        },
    })
    .expect("sequentialagent.New");
    let node: Arc<dyn Node> =
        AgentNode::new(seq.clone(), NodeConfig::default()).expect("NewAgentNode");
    let ctx = new_runnable_node_context(&seq).await;
    assert_sub_agent_outputs_not_synthesized(node, "seq_node", ctx, &["seq_a", "seq_b"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_parallel_agent_as_node_no_multiple_outputs() {
    let par = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "par_node".into(),
            sub_agents: vec![
                fixed_text_agent("par_a", "PAR_A"),
                fixed_text_agent("par_b", "PAR_B"),
            ],
            ..Default::default()
        },
    })
    .expect("parallelagent.New");
    let node: Arc<dyn Node> =
        AgentNode::new(par.clone(), NodeConfig::default()).expect("NewAgentNode");
    let ctx = new_runnable_node_context(&par).await;
    assert_sub_agent_outputs_not_synthesized(node, "par_node", ctx, &["par_a", "par_b"]).await;
}
