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

//! Parity target for Go examples/toolsearch/main_test.go: the example's agent, driven through the
//! runner by a scripted model (no network).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../examples/toolsearch_catalog.rs"]
mod toolsearch_catalog;

use std::any::Any;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk::agent::RunConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk::model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk::session::InMemorySessionService;
use serde_json::{Value, json};

/// Go `scriptedModel`: replays fixed responses and records the tools declared in each request.
struct ScriptedModel {
    responses: Vec<Part>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut requests = self.requests.lock().unwrap();
        if requests.len() == self.responses.len() {
            let n = requests.len() + 1;
            return futures::stream::iter([Err(ModelError::Other(
                format!("unexpected model call {n}").into(),
            ))])
            .boxed();
        }
        let part = self.responses[requests.len()].clone();
        requests.push(req);
        let resp = LlmResponse {
            content: Some(Content {
                role: ROLE_MODEL.to_string(),
                parts: vec![part],
            }),
            ..Default::default()
        };
        futures::stream::iter([Ok(resp)]).boxed()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `declaredNames`: the sorted function-declaration names of a request.
fn declared_names(req: &LlmRequest) -> Vec<String> {
    let mut names: Vec<String> = req
        .config
        .iter()
        .flat_map(|c| c.tools.iter())
        .flat_map(|t| t.function_declarations.iter())
        .map(|d| d.name.clone())
        .collect();
    names.sort();
    names
}

/// Go `call`.
fn call(name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: name.to_string(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `TestDiscoveryThroughRunner`: a gated tool is declared only after search_tools returns it,
/// the model can then call it in the same invocation, and it stays declared in the next turn.
#[tokio::test]
async fn test_discovery_through_runner() {
    let m = Arc::new(ScriptedModel {
        responses: vec![
            call("search_tools", json!({"query": "multiply"})),
            call("multiply_numbers", json!({"a": 6, "b": 7})),
            Part::from_text("42"),
            Part::from_text("hello"),
        ],
        requests: Mutex::new(Vec::new()),
    });
    let a = toolsearch_catalog::new_agent(m.clone()).expect("newAgent() error");
    let r = Runner::new(RunnerConfig {
        app_name: "app".to_string(),
        agent: a,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Default::default(),
        compaction: None,
    })
    .expect("runner.New() error");

    let mut results: Vec<FunctionResponse> = Vec::new();
    for prompt in ["Multiply 6 by 7.", "Hi"] {
        let mut events = r.run(
            "user",
            "session",
            Some(Content::from_text(prompt, ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(event) = events.next().await {
            let event = event.unwrap_or_else(|e| panic!("Run({prompt:?}) error = {e}"));
            let Some(content) = &event.response.content else {
                continue;
            };
            for p in &content.parts {
                if let Some(fr) = &p.function_response {
                    results.push(fr.clone());
                }
            }
        }
    }

    let requests = m.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        m.responses.len(),
        "model called {} times, want {}",
        requests.len(),
        m.responses.len()
    );
    let initial = vec!["get_current_time", "search_tools"];
    let discovered = vec!["get_current_time", "multiply_numbers", "search_tools"];
    for (i, want) in [&initial, &discovered, &discovered, &discovered]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            &declared_names(&requests[i]),
            want,
            "request {} declared tools mismatch",
            i + 1
        );
    }
    assert!(
        results.len() == 2
            && results[1].name == "multiply_numbers"
            && results[1].response.get("value").and_then(Value::as_f64) == Some(42.0),
        "function responses = {results:?}, want search_tools then multiply_numbers with value 42"
    );
}
