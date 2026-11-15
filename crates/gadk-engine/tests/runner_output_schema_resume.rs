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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Port of ADK-Go runner/output_schema_resume_test.go.

use std::sync::Arc;

use gadk_agent::{Context, RunConfig};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::workflow::{
    Edge, JoinNode, Node, NodeConfig, START, WORKFLOW_INPUT_FUNCTION_CALL_NAME, function_node,
};
use gadk_genai::{Content, Schema, Type};
use gadk_model::Llm;
use gadk_platform::Platform;
use gadk_session::Event;
use gadk_session_database::DatabaseSessionService;
use serde_json::{Map, Value, json};

mod workflow_support;
use workflow_support::{agent_node, join_resume_agent, resume_content};

fn start() -> Arc<dyn Node> {
    Arc::clone(&START)
}

fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    }
}

/// What the resumed turn showed for the join and the node after it.
#[derive(Default)]
struct ResumedJoin {
    output: Option<Map<String, Value>>,
    saw_join: bool,
    saw_sink: bool,
}

/// Go `runSchemaReplyThroughJoin`: a workflow on the database session service where an LlmAgent
/// with an OutputSchema replies with `reply`, a sibling agent pauses for input, and both feed a
/// join. Returns what the turn that resumes the sibling emitted.
async fn run_schema_reply_through_join(reply: &str) -> ResumedJoin {
    let svc = DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap();
    svc.auto_migrate().await.unwrap();
    let m = MockModel::from_contents(vec![Content::from_text(reply, "model")]);
    let s = llm_agent::new(LlmAgentConfig {
        name: "s".into(),
        model: Some(m as Arc<dyn Llm>),
        output_schema: Some(Schema {
            r#type: Some(Type::Object),
            properties: [(
                "title".to_string(),
                Schema {
                    r#type: Some(Type::String),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let b = join_resume_agent("b", "B", true);
    let (sn, bn) = (agent_node(&s), agent_node(&b));
    let join: Arc<dyn Node> = JoinNode::new("join");
    let sink = function_node::<Map<String, Value>, String, _, _>(
        "sink",
        |_ctx: Context, _in: Map<String, Value>| async { Ok("done".to_string()) },
        NodeConfig::default(),
    );
    let root = workflow_agent::new(WorkflowAgentConfig {
        name: "root".into(),
        sub_agents: vec![s, b],
        edges: vec![
            edge(&start(), &sn),
            edge(&start(), &bn),
            edge(&sn, &join),
            edge(&bn, &join),
            edge(&join, &sink),
        ],
        ..Default::default()
    })
    .unwrap();
    let r = Runner::new(RunnerConfig {
        app_name: "repro".into(),
        agent: root,
        session_service: Arc::new(svc),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();
    let first = r.run(
        "u",
        "s1",
        Some(Content::from_text("start", "user")),
        RunConfig::default(),
        RunOptions::default(),
    );
    futures::TryStreamExt::try_collect::<Vec<Event>>(first)
        .await
        .unwrap();
    let resumed = r.run(
        "u",
        "s1",
        Some(resume_content(
            "b",
            WORKFLOW_INPUT_FUNCTION_CALL_NAME,
            json!("approved"),
        )),
        RunConfig::default(),
        RunOptions::default(),
    );
    let events: Vec<Event> = futures::TryStreamExt::try_collect(resumed).await.unwrap();
    let mut got = ResumedJoin::default();
    for ev in events {
        match ev.output {
            Some(Value::Object(out)) => {
                got.saw_join = true;
                got.output = Some(out);
            }
            Some(Value::String(s)) if s == "done" => got.saw_sink = true,
            _ => {}
        }
    }
    got
}

// parity: adapted: an in-memory SQLite database stands in for Go's SQLite file in t.TempDir()
#[tokio::test]
async fn test_null_schema_reply_survives_resume() {
    let got = run_schema_reply_through_join("null").await;
    assert!(
        got.saw_join && got.saw_sink,
        "resume reached join = {}, sink = {}, want both",
        got.saw_join,
        got.saw_sink
    );
    let want = json!({"s": "null", "b": "approved"});
    assert_eq!(got.output.map(Value::Object), Some(want), "join output");
}

// parity: adapted: an in-memory SQLite database stands in for Go's SQLite file in t.TempDir()
#[tokio::test]
async fn test_parsed_schema_reply_survives_resume_on_database() {
    let got = run_schema_reply_through_join(r#"{"title":"T"}"#).await;
    assert!(
        got.saw_join && got.saw_sink,
        "resume reached join = {}, sink = {}, want both",
        got.saw_join,
        got.saw_sink
    );
    let want = json!({"s": {"title": "T"}, "b": "approved"});
    assert_eq!(got.output.map(Value::Object), Some(want), "join output");
}
