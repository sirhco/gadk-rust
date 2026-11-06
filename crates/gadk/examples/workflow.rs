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

//! A three-node function workflow (upper → suffix → exclaim) run through `Runner::in_memory`.
//! It needs no model and no credentials:
//!
//! `cargo run -p gadk --example workflow`
//!
//! Prints one line per node output, e.g. `upper -> HELLO WORKFLOW`.

use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::{AgentError, Context, RunConfig};
use gadk::engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::workflow::{Edge, Node, NodeConfig, START, function_node};
use gadk::genai::{Content, ROLE_USER};
use serde_json::Value;

async fn upper(_ctx: Context, input: String) -> Result<String, AgentError> {
    Ok(input.to_uppercase())
}

async fn suffix(_ctx: Context, input: String) -> Result<String, AgentError> {
    Ok(format!("{input} IS AWESOME"))
}

async fn exclaim(_ctx: Context, input: String) -> Result<String, AgentError> {
    Ok(format!("{input}!!!"))
}

fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: Arc::clone(from),
        to: Arc::clone(to),
        route: None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let upper = function_node("upper", upper, NodeConfig::default());
    let suffix = function_node("suffix", suffix, NodeConfig::default());
    let exclaim = function_node("exclaim", exclaim, NodeConfig::default());
    let edges = vec![
        edge(&START, &upper),
        edge(&upper, &suffix),
        edge(&suffix, &exclaim),
    ];
    let agent = workflow_agent::new(WorkflowAgentConfig {
        name: "three_steps".to_string(),
        description: "Uppercases the input, appends a suffix, then exclaims.".to_string(),
        edges,
        ..Default::default()
    })?;

    let runner = Runner::in_memory("workflow_example", agent)?;
    let msg = Content::from_text("hello workflow", ROLE_USER);
    let mut events = runner.run(
        "user",
        "session-1",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(event) = events.next().await {
        let event = event?;
        let Some(output) = &event.output else {
            continue;
        };
        let node = event
            .node_info
            .as_ref()
            .map(|n| n.path.as_str())
            .unwrap_or(event.author.as_str());
        let node = node.rsplit('/').next().unwrap_or(node);
        let node = node.split('@').next().unwrap_or(node);
        match output {
            Value::String(s) => println!("{node} -> {s}"),
            other => println!("{node} -> {other}"),
        }
    }
    runner.close().await?;
    Ok(())
}
