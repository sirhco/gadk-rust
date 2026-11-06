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

//! Parity port of Go internal/llminternal/outputschema_processor_test.go.

use std::collections::BTreeMap;

use gadk_agent::{AgentConfig, Context, InvocationContext, Tool, agent};
use gadk_genai::{Content, FunctionResponse, Part, Schema, Type};
use gadk_model::{Backend, LlmRequest, LlmResponse};
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::{
    llm::{
        config::LlmAgentConfig,
        flow::Flow,
        processors::{
            TestLlm, TestTool,
            output_schema::{
                SetModelResponseTool, create_final_model_response_event,
                output_schema_request_processor, retrieve_structured_model_response,
            },
            test_llm_agent,
        },
    },
    tool::utils::text_parts,
};

fn answer_schema() -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([(
            "answer".into(),
            Schema {
                r#type: Some(Type::String),
                ..Default::default()
            },
        )]),
        required: vec!["answer".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn test_output_schema_request_processor() {
    let f = Flow::default();
    // InjectsToolAndInstructions
    let a = test_llm_agent(LlmAgentConfig {
        name: "SchemaAgent".into(),
        model: Some(TestLlm::named("gemini-2.5-flash")),
        output_schema: Some(answer_schema()),
        tools: vec![TestTool::named("other_tool")],
        ..Default::default()
    });
    let ctx = InvocationContext::builder().agent(a).build_invocation();
    let mut req = LlmRequest::default();
    output_schema_request_processor(&ctx, &mut req, &f)
        .await
        .unwrap();
    assert!(
        req.tools.contains_key("set_model_response"),
        "req.tools['set_model_response'] missing"
    );
    let si = text_parts(
        req.config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref()),
    );
    assert!(
        si.iter()
            .any(|s| s.contains("set_model_response") && s.contains("required structured format")),
        "instructions: {si:?}"
    );

    for (name, cfg) in [
        (
            "NoOpWhenNoTools",
            LlmAgentConfig {
                name: "SchemaAgentNoTools".into(),
                model: Some(TestLlm::named("gemini-2.5-flash")),
                output_schema: Some(answer_schema()),
                ..Default::default()
            },
        ),
        (
            "NoOpWhenNoSchema",
            LlmAgentConfig {
                name: "NoSchemaAgent".into(),
                model: Some(TestLlm::named("gemini-2.5-flash")),
                tools: vec![TestTool::named("other_tool")],
                ..Default::default()
            },
        ),
        (
            "NoOpWhenNativeSupportAvailable",
            LlmAgentConfig {
                name: "VertexGemini2Agent".into(),
                model: Some(TestLlm::with_variant("gemini-2.5-flash", Backend::VertexAi)),
                output_schema: Some(answer_schema()),
                tools: vec![TestTool::named("other_tool")],
                ..Default::default()
            },
        ),
    ] {
        let ctx = InvocationContext::builder()
            .agent(test_llm_agent(cfg))
            .build_invocation();
        let mut req = LlmRequest::default();
        output_schema_request_processor(&ctx, &mut req, &f)
            .await
            .unwrap();
        assert!(
            !req.tools.contains_key("set_model_response"),
            "{name}: set_model_response must not be added"
        );
    }
}

#[tokio::test]
async fn test_create_final_model_response_event() {
    let a = agent::new(AgentConfig {
        name: "TestAgent".into(),
        ..Default::default()
    })
    .unwrap();
    let ctx = InvocationContext::builder().agent(a).build_invocation();
    let json_resp = r#"{"answer": "value"}"#;
    let ev = create_final_model_response_event(&ctx, json_resp);
    assert_eq!(ev.author, "TestAgent");
    let c = ev.response.content.as_ref().expect("content");
    assert_eq!(c.role, "model");
    assert_eq!(ev.branch, ctx.branch());
    assert_eq!(ev.invocation_id, ctx.invocation_id());
    assert_eq!(c.parts.len(), 1);
    assert_eq!(c.parts[0].text, json_resp);
}

fn response_event(name: &str, response: Value) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content {
                role: String::new(),
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        name: name.into(),
                        response: response.as_object().unwrap().clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn test_get_structured_model_response() {
    // ExtractsResponse
    let event = response_event("set_model_response", json!({"result": 123.0}));
    let got = retrieve_structured_model_response(Some(&event)).unwrap();
    let got_map: Value = serde_json::from_str(&got).expect("result is not valid JSON");
    assert_eq!(got_map, json!({"result": 123.0}));

    // NoResponseWhenNameMismatch
    let event = response_event("other_tool", json!({"x": 1}));
    assert_eq!(
        retrieve_structured_model_response(Some(&event)).unwrap(),
        ""
    );

    // NilEvent
    assert_eq!(retrieve_structured_model_response(None).unwrap(), "");
}

#[tokio::test]
async fn test_set_model_response_tool() {
    let schema = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([(
            "count".into(),
            Schema {
                r#type: Some(Type::Integer),
                ..Default::default()
            },
        )]),
        required: vec!["count".into()],
        ..Default::default()
    };
    let tool = SetModelResponseTool { schema };
    assert!(
        tool.description().contains("outputting text directly"),
        "Description should contain explicit instruction"
    );
    let runnable = tool.as_runnable().unwrap();

    // RunSuccess
    let input = json!({"count": 10.0});
    let got = runnable
        .run(Context::builder().build(), input.clone())
        .await
        .unwrap();
    assert_eq!(Value::Object(got), input);

    // RunValidationFailure_Type
    let res = runnable
        .run(Context::builder().build(), json!({"count": "not a number"}))
        .await;
    assert!(res.is_err(), "Expected validation error for invalid type");

    // RunValidationFailure_MissingRequired
    let res = runnable
        .run(Context::builder().build(), json!({"other": 123}))
        .await;
    assert!(
        res.is_err(),
        "Expected validation error for missing required field"
    );
}
