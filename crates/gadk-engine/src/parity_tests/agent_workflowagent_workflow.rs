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

//! Port of Go agent/workflowagent/workflow_test.go.

use futures::StreamExt;
use gadk_agent::{AgentError, Context};
use gadk_genai::{Content, FunctionResponse, Part};
use serde_json::{Value, json};

use super::agent_workflowagent_hitl::{FakeSession, chain, new_mock_ctx, start};
use crate::agents::workflow_agent::{self, WorkflowAgentConfig, decode_workflow_input_response};
use crate::workflow::{NodeConfig, function_node};

#[tokio::test]
async fn test_workflow_agent() {
    let upper = function_node::<Value, String, _, _>(
        "upper",
        |_ctx: Context, input: Value| async move {
            match input {
                Value::String(s) => Ok(s.to_uppercase()),
                _ => Err(AgentError::other("expected string input")),
            }
        },
        NodeConfig::default(),
    );
    let suffix = function_node::<String, String, _, _>(
        "suffix",
        |_ctx: Context, input: String| async move { Ok(format!("{input} done")) },
        NodeConfig::default(),
    );
    let my_workflow = workflow_agent::new(WorkflowAgentConfig {
        name: "test_workflow".into(),
        edges: chain(&[start(), upper, suffix]),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create workflow agent: {e}"));

    // Go MockSession: a session with no persisted state (FakeSession here).
    let sess = FakeSession::new();
    let msg = Content {
        role: String::new(),
        parts: vec![Part::from_text("hello")],
    };
    let mut events = my_workflow.run(new_mock_ctx(&sess, &my_workflow, Some(msg)));

    let mut last_output = None;
    let mut node_events = 0;
    while let Some(item) = events.next().await {
        let ev = item.unwrap_or_else(|e| panic!("unexpected error: {e}"));
        if let Some(out) = ev.output {
            last_output = Some(out);
            node_events += 1;
        }
    }
    // One output event per function node (upper, suffix); START emits nothing.
    assert_eq!(node_events, 2, "node-output events");
    assert_eq!(last_output, Some(json!("HELLO done")));
}

#[test]
fn test_decode_workflow_input_response() {
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "ResponseShape_JSONObject",
            json!({"response": "{\"approved\":true}"}),
            json!({"approved": true}),
        ),
        // Go decodes 42 as float64(42); serde_json as an integer. Both are the JSON number 42.
        (
            "ResponseShape_JSONScalar",
            json!({"response": "42"}),
            json!(42),
        ),
        (
            "ResponseShape_InvalidJSONFallsBackToRawString",
            json!({"response": "not valid json"}),
            json!("not valid json"),
        ),
        (
            "ResponseShape_NonStringValueReturnedVerbatim",
            json!({"response": {"already": "decoded"}}),
            json!({"already": "decoded"}),
        ),
        (
            "PayloadShape_StringPayload",
            json!({"payload": "yes"}),
            json!("yes"),
        ),
        (
            "PayloadShape_StructuredPayload",
            json!({"payload": {"k": "v"}}),
            json!({"k": "v"}),
        ),
        (
            "PriorityOrder_ResponseWinsOverPayload",
            json!({"response": "\"from-response\"", "payload": "from-payload"}),
            json!("from-response"),
        ),
        (
            "Fallback_NeitherKey_ReturnsRawMap",
            json!({"custom": "shape"}),
            json!({"custom": "shape"}),
        ),
        ("Fallback_EmptyResponseMap", json!({}), json!({})),
    ];
    for (name, response, want) in cases {
        let Value::Object(response) = response else {
            unreachable!()
        };
        let fr = FunctionResponse {
            response,
            ..Default::default()
        };
        assert_eq!(decode_workflow_input_response(&fr), want, "{name}");
    }
}
