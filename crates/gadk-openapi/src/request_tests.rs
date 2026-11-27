// Copyright 2026 Chris Olson
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

//! Unit tests of request building from arguments.

use gadk_agent::ToolError;
use serde_json::{Value, json};

use super::{Parts, build};
use crate::operation::{Operation, collect};
use crate::spec::OpenApiSpec;

const SPEC: &str = r#"
openapi: 3.0.3
paths:
  /items/{id}:
    put:
      operationId: putItem
      parameters:
        - {name: id, in: path}
        - {name: tag, in: query}
        - {name: limit, in: query}
        - {name: X-Trace, in: header}
      requestBody:
        content:
          application/json: {}
    get:
      operationId: getItem
      parameters:
        - {name: id, in: path}
        - {name: X-Trace, in: header, required: true}
"#;

fn op(name: &str) -> Operation {
    let spec: OpenApiSpec = SPEC.parse().unwrap();
    collect(&spec)
        .unwrap()
        .operations
        .into_iter()
        .find(|op| op.name == name)
        .unwrap()
}

fn invalid(op: &Operation, args: Value) -> String {
    match build(op, "https://h/api/v3", None, &args) {
        Err(ToolError::InvalidArgument(message)) => message,
        other => panic!("want InvalidArgument, got {other:?}"),
    }
}

#[test]
fn arguments_fill_the_path_query_headers_and_body() {
    let parts = build(
        &op("put_item"),
        "https://h/api/v3",
        Some(&("key".to_string(), "k 1".to_string())),
        &json!({
            "id": "a b/c",
            "tag": ["x", 2, true, null],
            "limit": 10,
            "X_Trace": "t-1",
            "body": {"name": "Rex"},
            "undeclared": "ignored"
        }),
    )
    .unwrap();
    assert_eq!(
        parts,
        Parts {
            url: "https://h/api/v3/items/a%20b%2Fc?tag=x&tag=2&tag=true&limit=10&key=k+1"
                .parse()
                .unwrap(),
            headers: vec![("X-Trace".to_string(), "t-1".to_string())],
            body: Some(json!({"name": "Rex"})),
        }
    );
}

#[test]
fn absent_and_null_optionals_are_left_out() {
    let parts = build(
        &op("put_item"),
        "https://h/api/v3",
        None,
        &json!({"id": 7, "tag": null, "body": null}),
    )
    .unwrap();
    assert_eq!(parts.url.as_str(), "https://h/api/v3/items/7");
    assert!(parts.headers.is_empty());
    assert_eq!(parts.body, None);
}

#[test]
fn body_is_ignored_without_a_request_body() {
    let parts = build(
        &op("get_item"),
        "https://h",
        None,
        &json!({"id": "1", "X_Trace": false, "body": {"x": 1}}),
    )
    .unwrap();
    assert_eq!(parts.url.as_str(), "https://h/items/1");
    assert_eq!(
        parts.headers,
        vec![("X-Trace".to_string(), "false".to_string())]
    );
    assert_eq!(parts.body, None);
}

#[test]
fn argument_errors_name_the_argument() {
    let put = op("put_item");
    let get = op("get_item");
    assert_eq!(
        invalid(&put, json!(["id"])),
        "the arguments must be a JSON object"
    );
    assert_eq!(invalid(&put, json!({})), "missing required argument id");
    assert_eq!(
        invalid(&get, json!({"id": "1", "X_Trace": null})),
        "missing required argument X_Trace"
    );
    assert_eq!(
        invalid(&put, json!({"id": {"a": 1}})),
        "argument id must be a string, number or boolean"
    );
    assert_eq!(
        invalid(&get, json!({"id": "1", "X_Trace": ["a"]})),
        "argument X_Trace must be a string, number or boolean"
    );
    assert_eq!(
        invalid(&put, json!({"id": "1", "limit": {"a": 1}})),
        "argument limit must be a string, number, boolean or an array of them"
    );
    assert_eq!(
        invalid(&put, json!({"id": "1", "tag": ["a", ["b"]]})),
        "argument tag must be a string, number, boolean or an array of them"
    );
    assert_eq!(
        invalid(&put, json!({"id": "1", "tag": [{"b": 1}]})),
        "argument tag must be a string, number, boolean or an array of them"
    );
}

#[test]
fn a_required_body_must_be_given() {
    let spec: OpenApiSpec = r#"
openapi: 3.1.0
paths:
  /pets:
    post:
      operationId: createPet
      requestBody:
        required: true
        content:
          application/json: {}
"#
    .parse()
    .unwrap();
    let create = collect(&spec).unwrap().operations.remove(0);
    match build(&create, "https://h", None, &json!({})) {
        Err(ToolError::InvalidArgument(message)) => {
            assert_eq!(message, "missing required argument body");
        }
        other => panic!("want InvalidArgument, got {other:?}"),
    }
}
