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

//! R21: the façade re-exports and prelude are usable end to end without a model.

use futures::StreamExt;
use gadk::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::sync::Arc;

fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: Arc::clone(from),
        to: Arc::clone(to),
        route: None,
    }
}

#[test]
fn facade_modules_are_reexported() {
    // Each path must resolve; the bodies are trivial.
    let _ = gadk::agent::RunConfig::default();
    let _ = gadk::artifact::InMemoryArtifactService::new();
    let _ = gadk::memory::InMemoryMemoryService::new();
    let _ = gadk::engine::runner::RunOptions::default();
    let _: fn() -> Arc<dyn Tool> = gadk::engine::tool::exit_loop;
}

#[test]
fn facade_tools_modules_resolve() {
    // Each gadk-tools module path must resolve through the façade (the modules are filled by
    // later tasks).
    #[allow(unused_imports)]
    use gadk::tools::{
        example, gemini, load_artifacts, load_memory, preload_memory, skills, skills::skill,
        toolsearch,
    };
}

#[tokio::test]
async fn facade_three_node_workflow_runs() {
    let upper = function_node(
        "upper",
        |_ctx: Context, s: String| async move { Ok::<_, AgentError>(s.to_uppercase()) },
        NodeConfig::default(),
    );
    let suffix = function_node(
        "suffix",
        |_ctx: Context, s: String| async move { Ok::<_, AgentError>(format!("{s} IS AWESOME")) },
        NodeConfig::default(),
    );
    let exclaim = function_node(
        "exclaim",
        |_ctx: Context, s: String| async move { Ok::<_, AgentError>(format!("{s}!!!")) },
        NodeConfig::default(),
    );
    let edges = vec![
        edge(&START, &upper),
        edge(&upper, &suffix),
        edge(&suffix, &exclaim),
    ];
    let agent = workflow_agent::new(workflow_agent::WorkflowAgentConfig {
        name: "three_steps".to_string(),
        edges,
        ..Default::default()
    })
    .unwrap();
    let runner = Runner::in_memory("facade_test", agent).unwrap();
    let msg = Content::from_text("hello workflow", gadk::genai::ROLE_USER);
    let events: Vec<Event> = runner
        .run(
            "user",
            "s1",
            Some(msg),
            RunConfig::default(),
            RunOptions::default(),
        )
        .map(|e| e.unwrap())
        .collect()
        .await;
    let outputs: Vec<Value> = events.iter().filter_map(|e| e.output.clone()).collect();
    assert!(outputs.len() >= 3, "outputs: {outputs:?}");
    assert_eq!(
        outputs[..3].to_vec(),
        vec![
            json!("HELLO WORKFLOW"),
            json!("HELLO WORKFLOW IS AWESOME"),
            json!("HELLO WORKFLOW IS AWESOME!!!"),
        ]
    );
    runner.close().await.unwrap();
}
