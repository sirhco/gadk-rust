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

//! Unit tests of operation collection, naming, parameters and skipped operations.

use reqwest::Method;
use serde_json::json;

use super::{Collected, SkippedOperation, collect, is_json, tool_name};
use crate::error::Error;
use crate::spec::OpenApiSpec;
use crate::spec::tests::{ECHO_3_1, PETSTORE};

fn collected(text: &str) -> Collected {
    collect(&text.parse::<OpenApiSpec>().unwrap()).unwrap()
}

fn collect_error(text: &str) -> Error {
    collect(&text.parse::<OpenApiSpec>().unwrap()).unwrap_err()
}

#[test]
fn names_follow_the_snake_case_rules() {
    for (operation_id, method, path, want) in [
        (Some("listPets"), "get", "/pets", "list_pets"),
        (Some("getHTTPResponse"), "get", "/x", "get_http_response"),
        (Some("v2Name"), "get", "/x", "v2_name"),
        (Some("get-user.v2"), "get", "/x", "get_user_v2"),
        (Some("2fa"), "get", "/x", "op_2fa"),
        (Some("--"), "get", "/a", "get_a"),
        (None, "get", "/pets/{petId}", "get_pets_pet_id"),
        (Some("__Already__Snake__"), "get", "/x", "already_snake"),
        (Some("café"), "get", "/x", "caf"),
    ] {
        assert_eq!(
            tool_name(operation_id, method, path),
            want,
            "{operation_id:?}"
        );
    }
}

#[test]
fn long_names_are_cut_to_64_bytes_without_a_trailing_underscore() {
    let long = format!("{}_tail", "a".repeat(63));
    assert_eq!(tool_name(Some(&long), "get", "/x"), "a".repeat(63));
    let exact = "b".repeat(70);
    assert_eq!(tool_name(Some(&exact), "get", "/x"), "b".repeat(64));
}

#[test]
fn the_pet_store_yields_four_operations_in_document_order() {
    let got = collected(PETSTORE);
    let names: Vec<(&str, Method, bool)> = got
        .operations
        .iter()
        .map(|op| (op.name.as_str(), op.method.clone(), op.mutating))
        .collect();
    assert_eq!(
        names,
        vec![
            ("list_pets", Method::GET, false),
            ("create_pet", Method::POST, true),
            ("get_pet", Method::GET, false),
            ("delete_pet", Method::DELETE, true),
        ]
    );
    assert!(got.skipped.is_empty());
}

#[test]
fn parameter_and_request_body_refs_are_resolved() {
    let got = collected(PETSTORE);
    assert_eq!(
        got.operations[1].parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "body": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string"},
                        "tag": {"type": ["string", "null"]}
                    }
                }
            },
            "required": ["body"]
        })
    );
    assert_eq!(
        got.operations[2].parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "petId": {"type": "string", "description": "The pet's id."},
                "X_Request_Id": {"type": "string"}
            },
            "required": ["petId"]
        })
    );
}

#[test]
fn the_3_1_body_keeps_its_type() {
    let got = collected(ECHO_3_1);
    assert_eq!(
        got.operations[0].parameters_schema(),
        json!({
            "type": "object",
            "properties": {"body": {"type": "string"}},
            "required": ["body"]
        })
    );
}

#[test]
fn descriptions_take_summary_and_description() {
    let spec = r#"{"openapi": "3.0.3", "paths": {"/d": {
        "get": {"operationId": "both", "summary": "Short.", "description": "Long."},
        "put": {"operationId": "summaryOnly", "summary": "Short."},
        "post": {"operationId": "descriptionOnly", "description": "Long."},
        "delete": {"operationId": "neither"},
        "patch": {"operationId": "huge", "description": "DESCRIPTION"}
    }}}"#
        .replace("DESCRIPTION", &"é".repeat(1100));
    let got = collected(&spec);
    let descriptions: Vec<&str> = got
        .operations
        .iter()
        .map(|op| op.description.as_str())
        .collect();
    assert_eq!(
        &descriptions[..4],
        ["Short.\n\nLong.", "Short.", "Long.", "DELETE /d"]
    );
    assert_eq!(descriptions[4], "é".repeat(1024));
}

#[test]
fn parameters_merge_collide_and_add_the_body() {
    let spec = r#"
openapi: 3.0.3
paths:
  /items/{id}:
    parameters:
      - {name: id, in: path, schema: {type: string}}
      - {name: verbose, in: query, schema: {type: boolean}, description: From the path item.}
    patch:
      operationId: updateItem
      parameters:
        - {name: verbose, in: query, required: true, schema: {type: integer, description: Own.}}
        - {name: id, in: query, schema: {type: integer}}
        - {name: body, in: header, schema: {type: string}}
        - {name: page-size, in: query, schema: {type: integer, example: 5}}
      requestBody:
        required: false
        content:
          application/merge-patch+json:
            schema: {type: object}
"#;
    let got = collected(spec);
    assert_eq!(
        got.operations[0].parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "id_path": {"type": "string"},
                "verbose": {"type": "integer", "description": "Own."},
                "id_query": {"type": "integer"},
                "body_header": {"type": "string"},
                "page_size": {"type": "integer"},
                "body": {"type": "object"}
            },
            "required": ["id_path", "verbose"]
        })
    );
    let properties: Vec<&str> = got.operations[0]
        .params
        .iter()
        .map(|p| p.property.as_str())
        .collect();
    assert_eq!(
        properties,
        ["id_path", "verbose", "id_query", "body_header", "page_size"]
    );
}

#[test]
fn a_collision_that_remains_is_a_duplicate_name() {
    let spec = r#"
openapi: 3.0.3
paths:
  /s:
    get:
      operationId: search
      parameters:
        - {name: a-b, in: query}
        - {name: a.b, in: query}
"#;
    match collect_error(spec) {
        Error::DuplicateName { name } => assert_eq!(name, "search.a_b_query"),
        other => panic!("want DuplicateName, got {other:?}"),
    }
}

#[test]
fn duplicate_tool_names_are_errors() {
    let same = r#"{"openapi": "3.0.3", "paths": {
        "/a": {"get": {"operationId": "listThings"}},
        "/b": {"get": {"operationId": "list_things"}}
    }}"#;
    match collect_error(same) {
        Error::DuplicateName { name } => assert_eq!(name, "list_things"),
        other => panic!("want DuplicateName, got {other:?}"),
    }
    let long = format!(
        r#"{{"openapi": "3.0.3", "paths": {{
        "/a": {{"get": {{"operationId": "{0}A"}}}},
        "/b": {{"get": {{"operationId": "{0}B"}}}}
    }}}}"#,
        "x".repeat(64)
    );
    match collect_error(&long) {
        Error::DuplicateName { name } => assert_eq!(name, "x".repeat(64)),
        other => panic!("want DuplicateName, got {other:?}"),
    }
}

#[test]
fn cookie_form_and_path_item_ref_operations_are_skipped() {
    let spec = r#"
openapi: 3.0.3
paths:
  /login:
    post:
      operationId: login
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema: {$ref: '#/components/schemas/Missing'}
          multipart/form-data: {}
  /me:
    get:
      operationId: me
      parameters:
        - {name: session, in: cookie, schema: {$ref: '#/components/schemas/Missing'}}
    put:
      operationId: updateMe
  /shared:
    $ref: '#/components/pathItems/Shared'
"#;
    let got = collected(spec);
    assert_eq!(
        got.skipped,
        vec![
            SkippedOperation::new(
                "POST",
                "/login",
                "request body content application/x-www-form-urlencoded, multipart/form-data is not JSON"
            ),
            SkippedOperation::new("GET", "/me", "cookie parameter session"),
            SkippedOperation::new("*", "/shared", "path item $ref is not supported"),
        ]
    );
    let names: Vec<&str> = got.operations.iter().map(|op| op.name.as_str()).collect();
    assert_eq!(names, ["update_me"]);
}

#[test]
fn refs_outside_parameters_and_bodies_are_never_read() {
    let spec = r#"
openapi: 3.0.3
paths:
  /pets:
    get:
      operationId: listPets
      responses:
        200:
          $ref: 'https://example.com/responses.yaml#/Ok'
        404:
          $ref: '#/components/responses/Missing'
"#;
    assert_eq!(collected(spec).operations.len(), 1);
}

#[test]
fn bad_refs_in_parameters_and_bodies_are_errors() {
    let remote = r#"
openapi: 3.0.3
paths:
  /pets:
    get:
      parameters:
        - $ref: 'common.yaml#/components/parameters/Limit'
"#;
    match collect_error(remote) {
        Error::Ref { reference, at } => {
            assert_eq!(reference, "common.yaml#/components/parameters/Limit");
            assert_eq!(at, "GET /pets");
        }
        other => panic!("want Ref, got {other:?}"),
    }
    let dangling = r#"
openapi: 3.0.3
paths:
  /pets:
    post:
      requestBody:
        content:
          application/json:
            schema: {$ref: '#/components/schemas/Pet'}
"#;
    match collect_error(dangling) {
        Error::Ref { reference, at } => {
            assert_eq!(reference, "#/components/schemas/Pet");
            assert_eq!(at, "POST /pets");
        }
        other => panic!("want Ref, got {other:?}"),
    }
}

#[test]
fn json_media_types() {
    for media in [
        "application/json",
        "application/json; charset=utf-8",
        "Application/JSON",
        "application/problem+json",
    ] {
        assert!(is_json(media), "{media}");
    }
    for media in ["text/json", "application/xml", "application/jsonl", "*/*"] {
        assert!(!is_json(media), "{media}");
    }
}
