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

//! Port of Go internal/workflowinternal/finish_task_tool_test.go.

use std::sync::Arc;

use gadk_agent::{Agent, Context, Tool, pack_tool};
use gadk_genai::{Schema, Type};
use gadk_model::LlmRequest;
use serde_json::{Map, Value, json};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::tool::finish_task::{FINISH_TASK_SUCCESS_RESULT, FINISH_TASK_TOOL_NAME, FinishTaskTool};
use crate::tool::utils::{function_decls, text_parts};

fn schema(v: Value) -> Schema {
    serde_json::from_value(v).unwrap()
}

/// Go sampleOutputSchema.
fn sample_output_schema() -> Schema {
    schema(json!({
        "type": "OBJECT",
        "properties": {"result": {"type": "STRING"}, "count": {"type": "INTEGER"}},
        "required": ["result", "count"]
    }))
}

fn s(t: &str) -> Schema {
    schema(json!({ "type": t }))
}

fn arr(items: Schema) -> Schema {
    Schema {
        r#type: Some(Type::Array),
        items: Some(Box::new(items)),
        ..Default::default()
    }
}

/// Port of Go makeTaskAgent.
fn make_task_agent(output_schema: Option<Schema>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "test_agent".into(),
        output_schema,
        ..Default::default()
    })
    .expect("llmagent.New failed")
}

/// Port of Go newFinishTaskTool.
fn new_finish_task_tool(output_schema: Option<Schema>) -> Arc<FinishTaskTool> {
    FinishTaskTool::new(make_task_agent(output_schema).as_ref()).expect("NewFinishTaskTool failed")
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

#[test]
fn test_new_finish_task_tool() {
    for (name, output_schema, want_has_output_doc) in [
        ("without output schema", None, false),
        ("with output schema", Some(sample_output_schema()), true),
    ] {
        let ft = new_finish_task_tool(output_schema);
        assert_eq!(ft.name(), FINISH_TASK_TOOL_NAME, "{name}: Name()");
        assert!(
            ft.description()
                .contains("Signal that this agent has completed"),
            "{name}: Description() = {:?}",
            ft.description()
        );
        assert_eq!(
            ft.description().contains("output data"),
            want_has_output_doc,
            "{name}: Description() = {:?}",
            ft.description()
        );
    }
}

#[test]
fn test_finish_task_tool_declaration() {
    let cases: Vec<(&str, Option<Schema>, Vec<&str>)> = vec![
        ("without output schema", None, vec!["result"]),
        (
            "with object output schema",
            Some(sample_output_schema()),
            vec!["result", "count"],
        ),
        (
            "wrapped string output schema",
            Some(s("STRING")),
            vec!["result"],
        ),
        (
            "wrapped integer output schema",
            Some(s("INTEGER")),
            vec!["result"],
        ),
        (
            "wrapped boolean output schema",
            Some(s("BOOLEAN")),
            vec!["result"],
        ),
        (
            "wrapped number output schema",
            Some(s("NUMBER")),
            vec!["result"],
        ),
        (
            "wrapped array of strings output schema",
            Some(arr(s("STRING"))),
            vec!["result"],
        ),
        (
            "wrapped array of integers output schema",
            Some(arr(s("INTEGER"))),
            vec!["result"],
        ),
        (
            "wrapped array of objects output schema",
            Some(arr(sample_output_schema())),
            vec!["result"],
        ),
    ];
    for (name, output_schema, want_props) in cases {
        let ft = new_finish_task_tool(output_schema);
        let decl = ft
            .declaration()
            .unwrap_or_else(|| panic!("{name}: Declaration() = None, want Some"));
        assert_eq!(
            decl.name, FINISH_TASK_TOOL_NAME,
            "{name}: Declaration().Name"
        );
        let params = decl
            .parameters
            .unwrap_or_else(|| panic!("{name}: Declaration().Parameters = None"));
        assert_eq!(
            params.r#type,
            Some(Type::Object),
            "{name}: Declaration().Parameters.Type"
        );
        for p in want_props {
            assert!(
                params.properties.contains_key(p),
                "{name}: Declaration().Parameters.Properties is missing {p:?}, got keys {:?}",
                params.properties.keys().collect::<Vec<_>>()
            );
        }
    }
}

fn nullable(mut schema: Schema) -> Schema {
    schema.nullable = Some(true);
    schema
}

/// The declaration must never offer the model a value Run rejects. A wrapped result is rejected
/// when null, so the declared result must not be nullable.
#[tokio::test]
async fn test_finish_task_tool_declaration_result_matches_validation() {
    let cases: Vec<(&str, Schema)> = vec![
        ("nullable string", nullable(s("STRING"))),
        ("nullable integer", nullable(s("INTEGER"))),
        ("nullable array", nullable(arr(s("STRING")))),
        ("string", s("STRING")),
        (
            "nullable string enum with description",
            nullable(Schema {
                r#type: Some(Type::String),
                description: "The verdict.".into(),
                r#enum: vec!["yes".into(), "no".into()],
                ..Default::default()
            }),
        ),
    ];
    for (name, output_schema) in cases {
        let agent = make_task_agent(Some(output_schema.clone()));
        let ft = FinishTaskTool::new(agent.as_ref()).expect("NewFinishTaskTool failed");
        let mut want_declared = output_schema.clone();
        want_declared.nullable = None;

        let key = ft.wrapper_key_str().to_owned();
        let params = ft
            .declaration()
            .and_then(|d| d.parameters)
            .unwrap_or_else(|| panic!("{name}: Declaration().Parameters = None"));
        let declared = params
            .properties
            .get(&key)
            .unwrap_or_else(|| panic!("{name}: Declaration() has no {key:?} property"));
        assert_eq!(
            declared, &want_declared,
            "{name}: declared {key:?} schema mismatch"
        );

        let got = ft
            .as_runnable()
            .expect("runnable")
            .run(Context::builder().build(), json!({ key.clone(): null }))
            .await
            .unwrap_or_else(|e| panic!("{name}: Run() unexpected error: {e}"));
        assert!(
            got.contains_key("error"),
            "{name}: Run() with null {key:?} = {got:?}, want a validation error"
        );

        // The agent's own OutputSchema keeps the Nullable the user set.
        let agent_schema = crate::llm::state::reveal(agent.as_ref())
            .and_then(|state| state.cfg.output_schema.clone());
        assert_eq!(
            agent_schema.as_ref(),
            Some(&output_schema),
            "{name}: agent OutputSchema changed"
        );
    }
}

#[test]
fn test_finish_task_tool_wrapper_key() {
    let cases: Vec<(&str, Option<Schema>, &str)> = vec![
        ("default (no user schema)", None, ""),
        ("object", Some(sample_output_schema()), ""),
        ("string", Some(s("STRING")), "result"),
        ("integer", Some(s("INTEGER")), "result"),
        ("boolean", Some(s("BOOLEAN")), "result"),
        ("number", Some(s("NUMBER")), "result"),
        ("array of strings", Some(arr(s("STRING"))), "result"),
        ("array of integers", Some(arr(s("INTEGER"))), "result"),
        (
            "array of objects",
            Some(arr(sample_output_schema())),
            "result",
        ),
    ];
    for (name, output_schema, want) in cases {
        let ft = new_finish_task_tool(output_schema);
        assert_eq!(ft.wrapper_key_str(), want, "{name}: wrapperKey");
        // Go "" ↔ Rust None on the trait method.
        assert_eq!(
            ft.wrapper_key(),
            (!want.is_empty()).then_some(want),
            "{name}: Tool::wrapper_key"
        );
    }
}

#[tokio::test]
#[allow(clippy::approx_constant)] // Go's literal 3.14
async fn test_finish_task_tool_run() {
    let want_success = obj(json!({ "result": FINISH_TASK_SUCCESS_RESULT }));
    // (name, output schema, args, want error, want error substrings)
    type Case = (&'static str, Option<Schema>, Value, bool, Vec<&'static str>);
    let cases: Vec<Case> = vec![
        (
            "default schema, valid args",
            None,
            json!({"result": "done"}),
            false,
            vec![],
        ),
        (
            "object schema, valid args",
            Some(sample_output_schema()),
            json!({"result": "success", "count": 42.0}),
            false,
            vec![],
        ),
        (
            "string wrapper, valid args",
            Some(s("STRING")),
            json!({"result": "hello"}),
            false,
            vec![],
        ),
        (
            "integer wrapper, valid args",
            Some(s("INTEGER")),
            json!({"result": 42.0}),
            false,
            vec![],
        ),
        (
            "boolean wrapper, valid args",
            Some(s("BOOLEAN")),
            json!({"result": true}),
            false,
            vec![],
        ),
        (
            "number wrapper, valid args",
            Some(s("NUMBER")),
            json!({"result": 3.14}),
            false,
            vec![],
        ),
        (
            "array of strings wrapper, valid args",
            Some(arr(s("STRING"))),
            json!({"result": ["a", "b", "c"]}),
            false,
            vec![],
        ),
        (
            "array of integers wrapper, valid args",
            Some(arr(s("INTEGER"))),
            json!({"result": [1.0, 2.0, 3.0]}),
            false,
            vec![],
        ),
        (
            "array of strings wrapper, empty args",
            Some(arr(s("STRING"))),
            json!({"result": []}),
            false,
            vec![],
        ),
        (
            "nullable string wrapper, non-null args",
            Some(nullable(s("STRING"))),
            json!({"result": "hello"}),
            false,
            vec![],
        ),
        (
            "validation error: nullable string wrapper, null result",
            Some(nullable(s("STRING"))),
            json!({"result": null}),
            false,
            vec!["finish_task", "validation errors", "result"],
        ),
        (
            "validation error: string wrapper, null result",
            Some(s("STRING")),
            json!({"result": null}),
            false,
            vec!["finish_task", "validation errors"],
        ),
        (
            "validation error: missing required field",
            Some(sample_output_schema()),
            json!({"result": "success"}),
            false,
            vec!["finish_task", "validation errors", "count"],
        ),
        (
            "validation error: wrong type",
            Some(sample_output_schema()),
            json!({"result": "success", "count": "not_an_int"}),
            false,
            vec!["finish_task", "validation errors"],
        ),
        ("no args", None, Value::Null, true, vec![]),
        ("non-map args", None, json!("not a map"), true, vec![]),
    ];
    for (name, output_schema, args, want_err, want_err_substr) in cases {
        let ft = new_finish_task_tool(output_schema);
        let got = ft
            .as_runnable()
            .expect("runnable")
            .run(Context::builder().build(), args)
            .await;
        if want_err {
            assert!(
                got.is_err(),
                "{name}: Run() error = nil, want non-nil; result = {got:?}"
            );
            continue;
        }
        let got = got.unwrap_or_else(|e| panic!("{name}: Run() unexpected error: {e}"));
        if !want_err_substr.is_empty() {
            let got_err = got.get("error").and_then(Value::as_str).unwrap_or_else(|| {
                panic!("{name}: Run() result missing \"error\" key or wrong type, got {got:?}")
            });
            for sub in want_err_substr {
                assert!(
                    got_err.contains(sub),
                    "{name}: Run() error {got_err:?} does not contain substring {sub:?}"
                );
            }
            continue;
        }
        assert_eq!(got, want_success, "{name}: Run() result mismatch");
    }
}

// parity: adapted: Go ProcessRequest packs the tool itself; the Rust flow calls pack_tool before process_request (spec § Tools), so the port does both in the flow's order
#[tokio::test]
async fn test_finish_task_tool_process_request() {
    let ft = new_finish_task_tool(None);
    let mut req = LlmRequest::default();
    let t: Arc<dyn Tool> = ft.clone();
    pack_tool(&mut req, &t).expect("pack_tool");
    ft.process_request(&Context::builder().build(), &mut req)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest() failed: {e}"));

    // The tool should be registered on the request.
    assert!(
        req.tools.contains_key(FINISH_TASK_TOOL_NAME),
        "req.Tools does not include {FINISH_TASK_TOOL_NAME:?}; got keys {:?}",
        req.tools.keys().collect::<Vec<_>>()
    );

    // The function declaration should also be present.
    let decls = function_decls(req.config.as_ref());
    assert!(
        decls.iter().any(|d| d.name == FINISH_TASK_TOOL_NAME),
        "req.Config function declarations do not include {FINISH_TASK_TOOL_NAME:?}; got {decls:?}"
    );

    // The finish_task instruction should have been appended to the system instruction.
    let instructions = text_parts(
        req.config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref()),
    );
    let want = [
        "finish_task",
        "Do NOT call 'finish_task' prematurely",
        "call 'finish_task' by itself with",
    ];
    assert!(
        instructions
            .iter()
            .any(|s| want.iter().all(|sub| s.contains(sub))),
        "system instruction does not contain finish_task guidance phrases\n  want all substrings: {want:?}\n  got instructions: {instructions:?}"
    );
}

#[test]
fn test_finish_task_tool_constants() {
    assert_eq!(FINISH_TASK_TOOL_NAME, "finish_task", "FinishTaskToolName");
    assert_eq!(
        FINISH_TASK_SUCCESS_RESULT, "Task completed.",
        "FinishTaskSuccessResult"
    );
}
