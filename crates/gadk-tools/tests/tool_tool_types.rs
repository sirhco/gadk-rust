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

//! The geminitool and loadartifactstool cases of Go tool/tool_test.go `TestTypes`. The 1b port
//! (`crates/gadk-engine/src/parity_tests/tool_tool.rs::test_types`) covers the `FunctionTool` and
//! `AgentTool` cases and left these out. This file is not a ledger target; the 2a ledger header
//! names it.

use std::sync::Arc;

use gadk_agent::{Context, Tool};
use gadk_model::LlmRequest;
use gadk_tools::{gemini, load_artifacts};

const FUNCTION_TOOL: &str = "FunctionTool";
const REQUEST_PROC: &str = "RequestProcessor";

/// The Rust stand-in for Go's `tool.(toolinternal.RequestProcessor)` assertion. Every Rust tool
/// has `process_request` (a no-op by default), so a tool counts as a request processor when
/// processing a Gemini 2 request, in a context without an artifact service, either changes the
/// request or fails.
async fn processes_request(tool: &dyn Tool) -> bool {
    let ctx = Context::builder().build();
    let mut req = LlmRequest {
        model: "gemini-2.5-flash".into(),
        ..Default::default()
    };
    let before = format!("{req:?}");
    match tool.process_request(&ctx, &mut req).await {
        Err(_) => true,
        Ok(()) => format!("{req:?}") != before,
    }
}

// parity: adapted: Go's interface assertions become capabilities (FunctionTool = as_runnable() and declaration() are both Some; RequestProcessor = process_request changes the request or fails); geminitool.New's nil *genai.Tool becomes genai::Tool::default(); the FunctionTool and AgentTool cases are in the 1b port
#[tokio::test]
async fn test_types() {
    let cases: Vec<(&str, Arc<dyn Tool>, Vec<&str>)> = vec![
        (
            "geminitool",
            gemini::new("", "", gadk_genai::Tool::default()),
            vec![REQUEST_PROC],
        ),
        (
            "geminitool.GoogleSearch{}",
            Arc::new(gemini::GoogleSearch),
            vec![REQUEST_PROC],
        ),
        (
            "LoadArtifactsTool",
            load_artifacts::new(),
            vec![REQUEST_PROC, FUNCTION_TOOL],
        ),
        (
            "LoadArtifactsTool",
            load_artifacts::new(),
            vec![REQUEST_PROC, FUNCTION_TOOL],
        ),
    ];
    for (name, tool, expected_types) in cases {
        for s in expected_types {
            match s {
                FUNCTION_TOOL => assert!(
                    tool.as_runnable().is_some() && tool.declaration().is_some(),
                    "Expected {name} to implement toolinternal.FunctionTool"
                ),
                REQUEST_PROC => assert!(
                    processes_request(tool.as_ref()).await,
                    "Expected {name} to implement toolinternal.RequestProcessor"
                ),
                other => panic!("Unknown expected type: {other}"),
            }
        }
    }
}
