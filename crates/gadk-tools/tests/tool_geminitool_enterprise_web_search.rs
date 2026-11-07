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

//! Port of Go tool/geminitool/enterprise_web_search_test.go.

use std::sync::Arc;

use gadk_agent::{Context, Tool};
use gadk_genai::{GenerateContentConfig, GoogleSearch, Tool as GenaiTool};
use gadk_model::LlmRequest;
use gadk_tools::gemini::EnterpriseWebSearch;

fn enterprise() -> GenaiTool {
    GenaiTool {
        enterprise_web_search: Some(gadk_genai::EnterpriseWebSearch::default()),
        ..Default::default()
    }
}

fn search() -> GenaiTool {
    GenaiTool {
        google_search: Some(GoogleSearch::default()),
        ..Default::default()
    }
}

fn bare(model: &str) -> LlmRequest {
    LlmRequest {
        model: model.into(),
        ..Default::default()
    }
}

fn with_tools(model: &str, tools: Vec<GenaiTool>) -> LlmRequest {
    LlmRequest {
        model: model.into(),
        config: Some(GenerateContentConfig {
            tools,
            ..Default::default()
        }),
        ..Default::default()
    }
}

// parity: adapted: the "nil request errors" case is dropped because a &mut LlmRequest cannot be null; Go's nil agent.Context becomes a default built Context
#[tokio::test]
async fn test_enterprise_web_search_process_request() {
    // (name, req, want_tools, want_err_text); an empty want_err_text means no error expected.
    let cases: Vec<(&str, LlmRequest, Vec<GenaiTool>, &str)> = vec![
        (
            "gemini 2 adds tool to empty request",
            bare("gemini-2.5-flash"),
            vec![enterprise()],
            "",
        ),
        (
            "gemini 2 adds tool alongside existing tools",
            with_tools("gemini-2.5-flash", vec![search()]),
            vec![search(), enterprise()],
            "",
        ),
        (
            "gemini 1 adds tool when alone",
            bare("gemini-1.5-flash"),
            vec![enterprise()],
            "",
        ),
        (
            "gemini 1 fails with other tools",
            with_tools("gemini-1.5-flash", vec![search()]),
            vec![],
            "cannot be used with other tools in Gemini 1.x",
        ),
        (
            "path-based gemini model resolves and adds tool",
            bare("projects/p/locations/global/publishers/google/models/gemini-2.5-flash"),
            vec![enterprise()],
            "",
        ),
        (
            "non-gemini model is unsupported",
            bare("gpt-4o"),
            vec![],
            "Enterprise Web Search tool is not supported for model gpt-4o",
        ),
        (
            "bare gemini is unsupported",
            bare("gemini"),
            vec![],
            "Enterprise Web Search tool is not supported for model gemini",
        ),
    ];
    let ctx = Context::builder().build();
    for (name, mut req, want_tools, want_err_text) in cases {
        let rp: Arc<dyn Tool> = Arc::new(EnterpriseWebSearch);
        let res = rp.process_request(&ctx, &mut req).await;
        if !want_err_text.is_empty() {
            let err = res.expect_err(&format!(
                "{name}: ProcessRequest() error = nil, want error containing {want_err_text:?}"
            ));
            assert!(
                err.to_string().contains(want_err_text),
                "{name}: ProcessRequest() error = {err:?}, want it to contain {want_err_text:?}"
            );
            continue;
        }
        res.unwrap_or_else(|e| panic!("{name}: ProcessRequest() unexpected error = {e}"));
        let got = req.config.map(|cfg| cfg.tools).unwrap_or_default();
        assert_eq!(got, want_tools, "{name}: ProcessRequest() tools mismatch");
    }
}
