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

//! Unit tests of the base URL, toolset building, filtering and prefixing.

use std::sync::{Arc, Mutex};

use gadk_agent::tool::{Tool, ToolPredicate, Toolset};
use gadk_agent::{Context, ReadonlyContext};
use serde_json::json;
use url::Url;

use super::{OpenApiToolset, base_url};
use crate::config::OpenApiConfig;
use crate::error::Error;
use crate::operation::SkippedOperation;
use crate::spec::OpenApiSpec;
use crate::spec::tests::{ECHO_3_1, PETSTORE};

fn petstore() -> OpenApiSpec {
    PETSTORE.parse().unwrap()
}

async fn names(toolset: &OpenApiToolset) -> Vec<String> {
    let ctx = Context::builder().build();
    toolset
        .tools(&ctx)
        .await
        .unwrap()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect()
}

fn new_error(spec: &OpenApiSpec, config: OpenApiConfig) -> Error {
    match OpenApiToolset::new(spec, config) {
        Err(err) => err,
        Ok(toolset) => panic!("want an error, got {toolset:?}"),
    }
}

#[test]
fn server_variables_take_their_defaults() {
    assert_eq!(
        base_url(&petstore(), None).unwrap(),
        "https://eu.pets.example.com/api/v3"
    );
}

#[test]
fn the_configured_base_url_wins_and_loses_its_trailing_slash() {
    assert_eq!(
        base_url(&petstore(), Some("http://localhost:8080/v2/")).unwrap(),
        "http://localhost:8080/v2"
    );
    assert_eq!(
        base_url(&petstore(), Some("https://h")).unwrap(),
        "https://h"
    );
    assert!(matches!(
        base_url(&petstore(), Some("/relative")),
        Err(Error::NoBaseUrl)
    ));
}

#[test]
fn a_relative_server_url_resolves_against_the_fetch_url() {
    let source = Url::parse("https://api.example.com/specs/echo.json").unwrap();
    let fetched = OpenApiSpec::parse(ECHO_3_1, Some(source.clone())).unwrap();
    assert_eq!(
        base_url(&fetched, None).unwrap(),
        "https://api.example.com/v1"
    );
    let no_servers =
        OpenApiSpec::parse(r#"{"openapi": "3.1.0", "paths": {}}"#, Some(source)).unwrap();
    assert_eq!(
        base_url(&no_servers, None).unwrap(),
        "https://api.example.com"
    );
}

#[test]
fn a_relative_server_url_without_a_fetch_url_has_no_base() {
    let spec: OpenApiSpec = ECHO_3_1.parse().unwrap();
    assert!(matches!(
        new_error(&spec, OpenApiConfig::new()),
        Error::NoBaseUrl
    ));
    assert!(OpenApiToolset::new(&spec, OpenApiConfig::new().base_url("https://h/api")).is_ok());
}

#[tokio::test]
async fn the_pet_store_becomes_four_tools_in_document_order() {
    let toolset = OpenApiToolset::new(&petstore(), OpenApiConfig::new()).unwrap();
    assert_eq!(toolset.name(), "openapi_toolset");
    assert_eq!(
        names(&toolset).await,
        ["list_pets", "create_pet", "get_pet", "delete_pet"]
    );
    let ctx = Context::builder().build();
    let tools = toolset.tools(&ctx).await.unwrap();
    let declaration = tools[0].declaration().unwrap();
    assert_eq!(declaration.name, "list_pets");
    assert_eq!(
        declaration.description,
        "List pets\n\nReturns every pet, newest first."
    );
    assert_eq!(
        declaration.parameters_json_schema,
        Some(json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "How many pets to return."},
                "tag": {"type": "array", "items": {"type": "string"}}
            },
            "required": []
        }))
    );
    assert_eq!(tools[0].description(), declaration.description);
    assert!(tools[0].as_runnable().is_some());
}

#[tokio::test]
async fn the_filter_sees_unprefixed_names_and_the_prefix_renames_tool_and_declaration() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let filter: ToolPredicate = Arc::new(move |_: &ReadonlyContext, tool: &dyn Tool| {
        recorder.lock().unwrap().push(tool.name().to_string());
        tool.name().starts_with("get") || tool.name().starts_with("list")
    });
    let toolset = OpenApiToolset::new(
        &petstore(),
        OpenApiConfig::new().prefix("pets").tool_filter(filter),
    )
    .unwrap();
    let ctx = Context::builder().build();
    let tools = toolset.tools(&ctx).await.unwrap();
    let exposed: Vec<(String, String)> = tools
        .iter()
        .map(|tool| (tool.name().to_string(), tool.declaration().unwrap().name))
        .collect();
    assert_eq!(
        exposed,
        [
            ("pets_list_pets".to_string(), "pets_list_pets".to_string()),
            ("pets_get_pet".to_string(), "pets_get_pet".to_string()),
        ]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        ["list_pets", "create_pet", "get_pet", "delete_pet"]
    );
    // Each call to tools() starts from the unprefixed tools again.
    assert_eq!(names(&toolset).await, ["pets_list_pets", "pets_get_pet"]);
}

#[test]
fn a_prefixed_name_over_64_bytes_is_too_long() {
    // 55 + 1 + 9 (`list_pets`) = 65 bytes.
    let prefix = "p".repeat(55);
    match new_error(&petstore(), OpenApiConfig::new().prefix(prefix.as_str())) {
        Error::NameTooLong { name } => assert_eq!(name, format!("{prefix}_list_pets")),
        other => panic!("want NameTooLong, got {other:?}"),
    }
    // 54 + 1 + 9 = 64 fits; 54 + 1 + 10 (`create_pet`) = 65 does not.
    let prefix = "p".repeat(54);
    match new_error(&petstore(), OpenApiConfig::new().prefix(prefix.as_str())) {
        Error::NameTooLong { name } => assert_eq!(name, format!("{prefix}_create_pet")),
        other => panic!("want NameTooLong, got {other:?}"),
    }
    assert!(OpenApiToolset::new(&petstore(), OpenApiConfig::new().prefix("p".repeat(53))).is_ok());
}

#[tokio::test]
async fn skipped_operations_are_listed_and_never_tools() {
    let spec: OpenApiSpec = r#"
openapi: 3.1.0
servers: [{url: "https://h"}]
paths:
  /login:
    post:
      operationId: login
      requestBody:
        content:
          multipart/form-data: {}
  /me:
    get:
      operationId: me
      parameters:
        - {name: session, in: cookie}
    delete:
      operationId: forgetMe
"#
    .parse()
    .unwrap();
    let toolset = OpenApiToolset::new(&spec, OpenApiConfig::new()).unwrap();
    assert_eq!(
        toolset.skipped_operations(),
        [
            SkippedOperation::new(
                "POST",
                "/login",
                "request body content multipart/form-data is not JSON"
            ),
            SkippedOperation::new("GET", "/me", "cookie parameter session"),
        ]
    );
    assert_eq!(names(&toolset).await, ["forget_me"]);
}
