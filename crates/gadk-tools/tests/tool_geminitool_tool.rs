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

//! Port of Go tool/geminitool/tool_test.go.

use gadk_agent::Context;
use gadk_genai::{GenerateContentConfig, GoogleMaps, GoogleSearch, Tool as GenaiTool};
use gadk_model::LlmRequest;
use gadk_tools::gemini;

fn search() -> GenaiTool {
    GenaiTool {
        google_search: Some(GoogleSearch::default()),
        ..Default::default()
    }
}

fn maps() -> GenaiTool {
    GenaiTool {
        google_maps: Some(GoogleMaps::default()),
        ..Default::default()
    }
}

// parity: adapted: the "error on nil request" case is dropped because a &mut LlmRequest cannot be null; Go's nil agent.Context becomes a default built Context
#[tokio::test]
async fn test_gemini_tool_process_request() {
    struct Case {
        name: &'static str,
        input_tool: GenaiTool,
        req: LlmRequest,
        want_tools: Vec<GenaiTool>,
    }
    let cases = vec![
        Case {
            name: "add to empty request",
            input_tool: search(),
            req: LlmRequest::default(),
            want_tools: vec![search()],
        },
        Case {
            name: "add to existing tools",
            input_tool: search(),
            req: LlmRequest {
                config: Some(GenerateContentConfig {
                    tools: vec![maps()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            want_tools: vec![maps(), search()],
        },
    ];
    let ctx = Context::builder().build();
    for mut c in cases {
        let gemini_tool = gemini::new("test_tool", "test_description", c.input_tool);
        gemini_tool
            .process_request(&ctx, &mut c.req)
            .await
            .unwrap_or_else(|e| panic!("{}: ProcessRequest() error = {e}", c.name));
        let got = c.req.config.map(|cfg| cfg.tools).unwrap_or_default();
        assert_eq!(
            got, c.want_tools,
            "{}: ProcessRequest returned unexpected tools",
            c.name
        );
    }
}
