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

//! Port of Go workflow/base_node_test.go.

use std::sync::Arc;
use std::time::Duration;

use gadk_genai::{Content, Part};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

use crate::error::WorkflowErrorKind as K;
use crate::tool::utils::CompiledSchema;
use crate::workflow::base_node::{default_validate_content_output, default_validate_output};
use crate::workflow::{
    AgentNode, BaseNode, JoinNode, Node, NodeConfig, ParallelWorker, RetryConfig, WorkflowNode,
};

// Compile-time assertions: the built-in node kinds satisfy Node (Go's `var _ Node = (*X)(nil)`
// block). FunctionNode/ToolNode are asserted with their own tests.
const _: fn() = || {
    fn is_node<T: Node>() {}
    is_node::<AgentNode>();
    is_node::<JoinNode>();
    is_node::<ParallelWorker>();
    is_node::<WorkflowNode>();
};

/// Go jsonschema.For[testSchemaInput] (struct { Value string `json:"value"` }).
fn test_schema_input() -> Value {
    json!({"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"], "additionalProperties": false})
}

fn compiled(v: Value) -> Arc<CompiledSchema> {
    Arc::new(CompiledSchema::new(v).expect("schema compiles"))
}

fn content(parts: Vec<Part>) -> Content {
    Content {
        parts,
        ..Default::default()
    }
}

type CfgView = (
    bool,
    Option<bool>,
    Option<bool>,
    Option<i32>,
    Option<Duration>,
    bool,
);
fn cfg_view(c: &NodeConfig) -> CfgView {
    (
        c.parallel_worker,
        c.rerun_on_resume,
        c.wait_for_output,
        c.retry_config.as_ref().map(|r| r.max_attempts),
        c.timeout,
        c.emits_own_span,
    )
}

#[rstest]
#[case::zero_config("n", "desc", NodeConfig::default())]
#[case::wait_for_output_true("join", "fan-in", NodeConfig { wait_for_output: Some(true), ..Default::default() })]
#[case::parallel_worker_true("mapper", "data parallel", NodeConfig { parallel_worker: true, ..Default::default() })]
#[case::empty_name_and_description("", "", NodeConfig::default())]
#[case::fully_populated("full_node", "Node with all config fields set", NodeConfig {
    parallel_worker: true, rerun_on_resume: Some(false), wait_for_output: Some(true),
    retry_config: Some(RetryConfig { max_attempts: 3, ..Default::default() }), timeout: Some(Duration::from_secs(10)), emits_own_span: false })]
fn test_new_base_node_round_trip(#[case] name: &str, #[case] desc: &str, #[case] cfg: NodeConfig) {
    let want = cfg_view(&cfg);
    let b = BaseNode::new(name, desc, cfg);
    assert_eq!(b.name(), name);
    assert_eq!(b.description(), desc);
    assert_eq!(cfg_view(b.config()), want);
}

#[test]
fn test_base_node_nil_schemas() {
    let b = BaseNode::new(
        "nil_schema_node",
        "no validation node",
        NodeConfig::default(),
    );
    assert!(b.input_schema().is_none());
    assert!(b.output_schema().is_none());
    let input = json!({"value": "hello"});
    assert_eq!(b.validate_input(input.clone()).expect("passthrough"), input);
    let output = json!({"value": "world"});
    assert_eq!(
        b.validate_output(output.clone()).expect("passthrough"),
        output
    );
}

#[test]
fn test_base_node_with_schemas() {
    let b = BaseNode::with_schemas(
        "schema_node",
        "validation node",
        Some(test_schema_input()),
        Some(test_schema_input()),
        NodeConfig::default(),
    )
    .expect("schemas compile");
    assert_eq!(b.input_schema(), Some(&test_schema_input()));
    assert_eq!(b.output_schema(), Some(&test_schema_input()));

    // ValidateInput: valid
    let got = b
        .validate_input(json!({"value": "hello"}))
        .expect("valid input");
    assert_eq!(got["value"], "hello");
    // ValidateInput: invalid
    let e = b
        .validate_input(json!({"value": 123}))
        .expect_err("invalid input");
    assert_eq!(e.kind(), K::InputValidation);

    // ValidateOutput: valid
    let out = json!({"value": "world"});
    assert_eq!(b.validate_output(out.clone()).expect("valid output"), out);
    // ValidateOutput: invalid
    let e = b
        .validate_output(json!({"value": 123}))
        .expect_err("invalid output");
    assert_eq!(e.kind(), K::OutputValidation);
}

// parity: skipped TestDefaultValidateOutput_PassthroughTypes: Go-only — Rust node outputs are serde_json::Value, so framework control values (*session.Event, *session.RequestInput) never ride in an output and there is no typed value to pass through

// parity: adapted: *genai.Content outputs go through default_validate_content_output (a Value cannot carry the Content type); schemas are JSON literals
#[test]
fn test_default_validate_output_content_fallback() {
    let string_schema = compiled(json!({"type": "string"}));
    let object_schema = compiled(test_schema_input());

    // string_schema_returns_text
    let c = content(vec![Part::from_text("hello "), Part::from_text("world")]);
    assert_eq!(
        default_validate_content_output(&c, Some(&string_schema)).expect("string schema"),
        json!("hello world")
    );

    // object_schema_parses_json
    let c = content(vec![Part::from_text(r#"{"value":"hello"}"#)]);
    assert_eq!(
        default_validate_content_output(&c, Some(&object_schema)).expect("object schema"),
        json!({"value": "hello"})
    );

    // invalid_json_returns_original_error
    let c = content(vec![Part::from_text("not valid json")]);
    assert!(default_validate_content_output(&c, Some(&object_schema)).is_err());

    // empty_text_returns_original_error
    let c = content(vec![Part::from_text("   ")]);
    assert!(default_validate_content_output(&c, Some(&object_schema)).is_err());

    // thought_parts_are_skipped
    let c = content(vec![
        Part {
            text: "internal reasoning, not for the user".into(),
            thought: true,
            ..Default::default()
        },
        Part::from_text(r#"{"value":"hello"}"#),
    ]);
    assert_eq!(
        default_validate_content_output(&c, Some(&object_schema)).expect("thought part skipped"),
        json!({"value": "hello"})
    );

    // string_schema_returns_whitespace_text
    let c = content(vec![Part::from_text("   ")]);
    assert_eq!(
        default_validate_content_output(&c, Some(&string_schema)).expect("whitespace"),
        json!("   ")
    );

    // string_schema_min_length_violation_fails
    let min_len = compiled(json!({"type": "string", "minLength": 5}));
    let c = content(vec![Part::from_text("hi")]);
    assert!(default_validate_content_output(&c, Some(&min_len)).is_err());

    // string_schema_pattern_violation_fails
    let pattern = compiled(json!({"type": "string", "pattern": "^[0-9]+$"}));
    let c = content(vec![Part::from_text("not-digits")]);
    assert!(default_validate_content_output(&c, Some(&pattern)).is_err());
}

// parity: adapted: Go nil output is Value::Null
#[test]
fn test_default_validate_output_nil_passthrough() {
    let got = default_validate_output(Value::Null, Some(&compiled(test_schema_input())))
        .expect("nil passthrough");
    assert_eq!(got, Value::Null);
}

#[test]
fn test_default_validate_output_string_fallback() {
    let object_schema = compiled(test_schema_input());

    // object_schema_parses_json
    let got = default_validate_output(json!(r#"{"value":"hello"}"#), Some(&object_schema))
        .expect("parses json");
    assert_eq!(got, json!({"value": "hello"}));

    // non_json_returns_original_error
    assert!(default_validate_output(json!("not json"), Some(&object_schema)).is_err());

    // plain_string_passes_any_schema
    let any_schema = compiled(json!({}));
    assert_eq!(
        default_validate_output(json!("plain text"), Some(&any_schema)).expect("any schema"),
        json!("plain text")
    );

    // string_schema_min_length_violation_fails
    let min_len = compiled(json!({"type": "string", "minLength": 5}));
    assert!(default_validate_output(json!("hi"), Some(&min_len)).is_err());
}

// parity: adapted: Content path via default_validate_content_output; Go float64(42) is json!(42)
#[test]
fn test_default_validate_output_multi_type_root() {
    // string_or_null_returns_projected_text
    let schema = compiled(json!({"type": ["string", "null"]}));
    let c = content(vec![Part::from_text("plain text")]);
    assert_eq!(
        default_validate_content_output(&c, Some(&schema)).expect("string or null"),
        json!("plain text")
    );

    // string_or_integer_returns_projected_text
    let schema = compiled(json!({"type": ["string", "integer"]}));
    let c = content(vec![Part::from_text("42")]);
    assert_eq!(
        default_validate_content_output(&c, Some(&schema)).expect("string or integer"),
        json!("42")
    );

    // non_string_members_still_parse_json
    let schema = compiled(json!({"type": ["integer", "null"]}));
    let c = content(vec![Part::from_text("42")]);
    assert_eq!(
        default_validate_content_output(&c, Some(&schema)).expect("integer or null"),
        json!(42)
    );
}
