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

//! Port of Go `tool/mcptoolset/conversion_test.go` (package `mcptoolset`).
//!
//! gadk-mcp has no edge to gadk-engine, so Go's `functiontool.New` tools are stubs whose
//! declaration carries the schema Go's functiontool infers (jsonschema-go `For[weatherInput]`;
//! `["null", "object"]` for the pointer argument).

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::Tool;
use gadk_genai::{FunctionDeclaration, Schema, Type};
use pretty_assertions::assert_eq;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

use crate::config::default_client_info;
use crate::conversion::gemini_to_json_schema;
use crate::to_mcp_tool;

/// Go `stubToolWithoutDecl` (`decl: None`) and `stubToolWithDecl`.
struct StubTool {
    name: String,
    description: String,
    decl: Option<FunctionDeclaration>,
}

impl Tool for StubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        self.decl.clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn stub(name: &str, description: &str, decl: Option<FunctionDeclaration>) -> StubTool {
    StubTool {
        name: name.to_string(),
        description: description.to_string(),
        decl,
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn json_decl(name: &str, schema: Value) -> Option<FunctionDeclaration> {
    Some(FunctionDeclaration {
        name: name.to_string(),
        parameters_json_schema: Some(schema),
        ..Default::default()
    })
}

fn params_decl(name: &str, description: &str, params: Schema) -> Option<FunctionDeclaration> {
    Some(FunctionDeclaration {
        name: name.to_string(),
        description: description.to_string(),
        parameters: Some(params),
        ..Default::default()
    })
}

fn typed(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Default::default()
    }
}

fn props<const N: usize>(entries: [(&str, Schema); N]) -> BTreeMap<String, Schema> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// jsonschema-go's `For[weatherInput]` as Go's `functiontool.New` puts it in
/// `ParametersJsonSchema`.
fn weather_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "city": {"type": "string", "description": "The city name"},
            "units": {"type": "string", "description": "Temperature units"}
        },
        "required": ["city"],
        "additionalProperties": false
    })
}

/// Go `newWeatherFunctionTool`.
fn new_weather_function_tool() -> StubTool {
    stub(
        "get_weather",
        "Gets weather information",
        Some(FunctionDeclaration {
            name: "get_weather".into(),
            description: "Gets weather information".into(),
            parameters_json_schema: Some(weather_input_schema()),
            ..Default::default()
        }),
    )
}

/// Go `newPointerWeatherFunctionTool`: jsonschema-go infers `["null", "object"]` for `*weatherInput`.
fn new_pointer_weather_function_tool() -> StubTool {
    let mut schema = weather_input_schema();
    schema["type"] = json!(["null", "object"]);
    stub(
        "get_weather_ptr",
        "Gets weather information",
        Some(FunctionDeclaration {
            name: "get_weather_ptr".into(),
            description: "Gets weather information".into(),
            parameters_json_schema: Some(schema),
            ..Default::default()
        }),
    )
}

/// Go `&mcp.Tool{Name, Description, InputSchema}` (an empty description is omitted).
fn mcp_tool(name: &str, description: &str, input_schema: Value) -> rmcp::model::Tool {
    let description =
        (!description.is_empty()).then(|| std::borrow::Cow::Owned(description.to_string()));
    rmcp::model::Tool::new_with_raw(name.to_string(), description, Arc::new(obj(input_schema)))
}

// parity: adapted: Go's "nil tool returns error" and "tool with unmarshalable Parameters returns error" cases have no Rust counterpart (a `&dyn Tool` is never nil; a genai `Schema` holds no channel); functiontool-built tools are stubs carrying functiontool's inferred schema (no gadk-engine edge); each error case also asserts Go's error text
#[test]
fn test_to_mcp_tool() {
    let nested_json_schema = json!({
        "type": "object",
        "properties": {
            "username": {"type": "string"},
            "profile": {
                "type": "object",
                "properties": {
                    "email": {"type": "string"},
                    "age": {"type": "integer"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["email"]
            }
        },
        "required": ["username", "profile"]
    });

    let ok_cases: Vec<(&str, StubTool, rmcp::model::Tool)> = vec![
        (
            "tool without Declaration method defaults to object schema",
            stub("ping", "Pings the service", None),
            mcp_tool("ping", "Pings the service", json!({"type": "object"})),
        ),
        (
            "tool with nil declaration defaults to object schema",
            stub("noop", "Does nothing", None),
            mcp_tool("noop", "Does nothing", json!({"type": "object"})),
        ),
        (
            "tool with declaration but no parameters defaults to object schema",
            stub(
                "clock",
                "Gets the current time",
                Some(FunctionDeclaration {
                    name: "clock".into(),
                    description: "Gets the current time".into(),
                    ..Default::default()
                }),
            ),
            mcp_tool("clock", "Gets the current time", json!({"type": "object"})),
        ),
        (
            "tool with Parameters schema converts to JSON schema",
            stub(
                "get_weather",
                "Gets weather information",
                params_decl(
                    "get_weather",
                    "Gets weather information",
                    Schema {
                        r#type: Some(Type::Object),
                        properties: props([
                            (
                                "location",
                                Schema {
                                    description: "The location to get weather for".into(),
                                    ..typed(Type::String)
                                },
                            ),
                            (
                                "units",
                                Schema {
                                    description: "Temperature units".into(),
                                    ..typed(Type::String)
                                },
                            ),
                        ]),
                        required: vec!["location".into()],
                        property_ordering: vec!["location".into(), "units".into()],
                        ..Default::default()
                    },
                ),
            ),
            mcp_tool(
                "get_weather",
                "Gets weather information",
                json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string", "description": "The location to get weather for"},
                        "units": {"type": "string", "description": "Temperature units"}
                    },
                    "required": ["location"]
                }),
            ),
        ),
        (
            "tool with ParametersJsonSchema uses it directly",
            stub(
                "search",
                "Searches a database",
                json_decl(
                    "search",
                    json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "The search query"},
                            "limit": {"type": "integer", "description": "Maximum number of results"}
                        },
                        "required": ["query"]
                    }),
                ),
            ),
            mcp_tool(
                "search",
                "Searches a database",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "The search query"},
                        "limit": {"type": "integer", "description": "Maximum number of results"}
                    },
                    "required": ["query"]
                }),
            ),
        ),
        (
            "tool prefers ParametersJsonSchema over Parameters",
            stub(
                "prefer_json",
                "Prefers JSON schema",
                Some(FunctionDeclaration {
                    name: "prefer_json".into(),
                    description: "Prefers JSON schema".into(),
                    parameters: Some(Schema {
                        r#type: Some(Type::Object),
                        properties: props([("schema_param", typed(Type::String))]),
                        ..Default::default()
                    }),
                    parameters_json_schema: Some(json!({
                        "type": "object",
                        "properties": {"json_param": {"type": "string"}}
                    })),
                    ..Default::default()
                }),
            ),
            mcp_tool(
                "prefer_json",
                "Prefers JSON schema",
                json!({"type": "object", "properties": {"json_param": {"type": "string"}}}),
            ),
        ),
        (
            "tool with complex nested schema",
            stub(
                "create_user",
                "Creates a new user",
                json_decl("create_user", nested_json_schema.clone()),
            ),
            mcp_tool("create_user", "Creates a new user", nested_json_schema),
        ),
        (
            "tool built with functiontool.New",
            new_weather_function_tool(),
            mcp_tool(
                "get_weather",
                "Gets weather information",
                weather_input_schema(),
            ),
        ),
        (
            "tool built with functiontool.New and a pointer argument gets object type",
            new_pointer_weather_function_tool(),
            mcp_tool(
                "get_weather_ptr",
                "Gets weather information",
                weather_input_schema(),
            ),
        ),
        (
            "tool with typed-nil ParametersJsonSchema defaults to object schema",
            stub(
                "typed_nil",
                "Typed nil schema",
                json_decl("typed_nil", Value::Null),
            ),
            mcp_tool("typed_nil", "Typed nil schema", json!({"type": "object"})),
        ),
        (
            "tool with empty ParametersJsonSchema map defaults to object schema",
            stub(
                "empty_map",
                "Empty schema",
                json_decl("empty_map", json!({})),
            ),
            mcp_tool("empty_map", "Empty schema", json!({"type": "object"})),
        ),
        (
            "tool with ParametersJsonSchema listing object among types gets object type",
            stub(
                "nullable_object",
                "Nullable object",
                json_decl(
                    "nullable_object",
                    json!({"type": ["null", "object"], "properties": {"q": {"type": "string"}}}),
                ),
            ),
            mcp_tool(
                "nullable_object",
                "Nullable object",
                json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            ),
        ),
        (
            "tool with Parameters lacking top-level type gets object type",
            stub(
                "typeless",
                "Typeless parameters",
                Some(FunctionDeclaration {
                    name: "typeless".into(),
                    parameters: Some(Schema {
                        properties: props([("q", typed(Type::String))]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            ),
            mcp_tool(
                "typeless",
                "Typeless parameters",
                json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            ),
        ),
    ];
    for (name, tool, want) in ok_cases {
        let got = to_mcp_tool(&tool).unwrap_or_else(|e| panic!("{name}: ToMCPTool() error = {e}"));
        assert_eq!(got, want, "{name}: ToMCPTool() mismatch");
    }

    let err_cases: Vec<(&str, StubTool, &str)> = vec![
        (
            "tool with non-object ParametersJsonSchema type returns error",
            stub(
                "string_schema",
                "",
                json_decl("string_schema", json!({"type": "string"})),
            ),
            r#"mcptoolset: input schema for tool "string_schema": schema type "string" is not "object""#,
        ),
        (
            "tool with ParametersJsonSchema types excluding object returns error",
            stub(
                "string_or_null",
                "",
                json_decl("string_or_null", json!({"type": ["null", "string"]})),
            ),
            r#"mcptoolset: input schema for tool "string_or_null": schema types ["null","string"] do not include "object""#,
        ),
        (
            "tool with ParametersJsonSchema that is not a JSON object returns error",
            stub(
                "not_object",
                "",
                json_decl("not_object", json!("not a schema")),
            ),
            r#"mcptoolset: input schema for tool "not_object": schema is not a JSON object: expected a JSON object, found a string"#,
        ),
        (
            "tool with non-object Parameters type returns error",
            stub(
                "string_params",
                "",
                params_decl("string_params", "", typed(Type::String)),
            ),
            r#"mcptoolset: input schema for tool "string_params": schema type "string" is not "object""#,
        ),
    ];
    for (name, tool, want) in err_cases {
        match to_mcp_tool(&tool) {
            Ok(got) => panic!("{name}: ToMCPTool() = {got:?}, want an error"),
            Err(e) => assert_eq!(e.to_string(), want, "{name}"),
        }
    }
}

#[test]
fn test_to_mcp_tool_does_not_modify_caller_schema() {
    let schema = json!({
        "type": ["null", "object"],
        "properties": {"q": {"type": "string"}}
    });
    let want = schema.clone();
    let adk_tool = stub("caller_schema", "", json_decl("caller_schema", schema));

    let got = to_mcp_tool(&adk_tool).unwrap();
    assert_eq!(
        got.input_schema.get("type"),
        Some(&json!("object")),
        "ToMCPTool() InputSchema type"
    );
    assert_eq!(
        adk_tool.declaration().unwrap().parameters_json_schema,
        Some(want),
        "ToMCPTool() modified the caller's schema"
    );
}

// parity: adapted: Go's go-sdk Server.AddTool panics on an input schema whose type is not "object"; the rmcp test server accepts any schema, so the port also asserts each converted schema's type is "object" before registering it
#[tokio::test]
async fn test_to_mcp_tool_server_registration() {
    let fn_tool = new_weather_function_tool();
    let param_tool = stub(
        "lookup",
        "Looks up a record",
        params_decl(
            "lookup",
            "Looks up a record",
            Schema {
                r#type: Some(Type::Object),
                properties: props([(
                    "id",
                    Schema {
                        any_of: vec![typed(Type::String), typed(Type::Integer)],
                        ..typed(Type::Object)
                    },
                )]),
                required: vec!["id".into()],
                property_ordering: vec!["id".into()],
                ..Default::default()
            },
        ),
    );
    let no_decl_tool = stub("ping", "Pings the server", None);
    let ptr_tool = new_pointer_weather_function_tool();
    let typed_nil_tool = stub(
        "typed_nil",
        "Typed nil schema",
        json_decl("typed_nil", Value::Null),
    );
    let typeless_tool = stub(
        "typeless",
        "Typeless parameters",
        Some(FunctionDeclaration {
            name: "typeless".into(),
            parameters: Some(Schema {
                properties: props([("q", typed(Type::String))]),
                ..Default::default()
            }),
            ..Default::default()
        }),
    );

    let mut server = crate::testutil::ToolServer::new("test_server", "v1.0.0");
    let adk_tools: [&dyn Tool; 6] = [
        &fn_tool,
        &param_tool,
        &no_decl_tool,
        &ptr_tool,
        &typed_nil_tool,
        &typeless_tool,
    ];
    for adk_tool in adk_tools {
        let mcp_tool = to_mcp_tool(adk_tool)
            .unwrap_or_else(|e| panic!("ToMCPTool({:?}) error = {e}", adk_tool.name()));
        assert_eq!(
            mcp_tool.input_schema.get("type"),
            Some(&json!("object")),
            "{}: go-sdk's AddTool would reject the schema",
            adk_tool.name()
        );
        server = server.tool(mcp_tool, |_: CallToolRequestParams, _| async move {
            Ok(CallToolResult::success(vec![]))
        });
    }

    let session = crate::testutil::in_memory(move || server.clone())
        .connect(default_client_info())
        .await
        .unwrap();
    let listed = session.list_all_tools().await.unwrap();
    session.cancel().await.unwrap();

    let got_by_name: BTreeMap<String, Value> = listed
        .into_iter()
        .map(|t| (t.name.to_string(), Value::Object((*t.input_schema).clone())))
        .collect();
    assert_eq!(got_by_name.len(), 6, "ListTools() returned tools");

    let want_weather_schema = weather_input_schema();
    assert_eq!(
        got_by_name["get_weather"], want_weather_schema,
        "get_weather InputSchema mismatch"
    );
    assert_eq!(
        got_by_name["lookup"],
        json!({
            "type": "object",
            "properties": {"id": {"anyOf": [{"type": "string"}, {"type": "integer"}]}},
            "required": ["id"]
        }),
        "lookup InputSchema mismatch"
    );
    assert_eq!(
        got_by_name["ping"],
        json!({"type": "object"}),
        "ping InputSchema mismatch"
    );
    assert_eq!(
        got_by_name["get_weather_ptr"],
        json!({
            "type": "object",
            "properties": want_weather_schema["properties"],
            "required": ["city"],
            "additionalProperties": false
        }),
        "get_weather_ptr InputSchema mismatch"
    );
    assert_eq!(
        got_by_name["typed_nil"],
        json!({"type": "object"}),
        "typed_nil InputSchema mismatch"
    );
    assert_eq!(
        got_by_name["typeless"],
        json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        "typeless InputSchema mismatch"
    );
}

// parity: adapted: Go's "nil schema returns error" and "unmarshalable schema returns error" cases have no Rust counterpart (the function takes a `&Schema`; a genai `Schema` holds no channel), so every case converts
#[test]
fn test_gemini_to_json_schema() {
    let cases: Vec<(&str, Schema, Value)> = vec![
        (
            "unset type omits type constraint",
            Schema::default(),
            json!({}),
        ),
        (
            "unspecified type omits type constraint",
            typed(Type::Unspecified),
            json!({}),
        ),
        (
            "explicit null type maps to null",
            typed(Type::Null),
            json!({"type": "null"}),
        ),
        (
            "type is lowercased",
            typed(Type::String),
            json!({"type": "string"}),
        ),
        (
            "boolean type is lowercased",
            typed(Type::Boolean),
            json!({"type": "boolean"}),
        ),
        (
            "direct fields are preserved",
            Schema {
                title: "City".into(),
                description: "A city name".into(),
                default: Some(json!("Paris")),
                r#enum: vec!["Paris".into(), "Rome".into()],
                format: "enum".into(),
                example: Some(json!("Rome")),
                ..typed(Type::String)
            },
            json!({
                "type": "string",
                "title": "City",
                "description": "A city name",
                "default": "Paris",
                "enum": ["Paris", "Rome"],
                "format": "enum",
                "example": "Rome"
            }),
        ),
        (
            "nullable true is emitted",
            Schema {
                nullable: Some(true),
                ..typed(Type::String)
            },
            json!({"type": "string", "nullable": true}),
        ),
        (
            "nullable false is omitted",
            Schema {
                nullable: Some(false),
                ..typed(Type::String)
            },
            json!({"type": "string"}),
        ),
        (
            "string constraints are preserved on string type",
            Schema {
                pattern: "^a.*".into(),
                min_length: Some(2),
                max_length: Some(8),
                ..typed(Type::String)
            },
            json!({"type": "string", "pattern": "^a.*", "minLength": 2, "maxLength": 8}),
        ),
        (
            "string constraints are dropped for non-string type",
            Schema {
                pattern: "^a.*".into(),
                min_length: Some(2),
                max_length: Some(8),
                minimum: Some(1.0),
                ..typed(Type::Integer)
            },
            json!({"type": "integer", "minimum": 1.0}),
        ),
        (
            "numeric constraints are dropped for string type",
            Schema {
                minimum: Some(1.0),
                maximum: Some(5.0),
                pattern: "x".into(),
                ..typed(Type::String)
            },
            json!({"type": "string", "pattern": "x"}),
        ),
        (
            "numeric constraints are kept for number type",
            Schema {
                minimum: Some(0.5),
                maximum: Some(9.5),
                ..typed(Type::Number)
            },
            json!({"type": "number", "minimum": 0.5, "maximum": 9.5}),
        ),
        (
            "numeric constraints are kept for integer type",
            Schema {
                minimum: Some(1.0),
                maximum: Some(10.0),
                ..typed(Type::Integer)
            },
            json!({"type": "integer", "minimum": 1.0, "maximum": 10.0}),
        ),
        (
            "array items and constraints are converted recursively",
            Schema {
                items: Some(Box::new(Schema {
                    max_length: Some(4),
                    ..typed(Type::String)
                })),
                min_items: Some(1),
                max_items: Some(3),
                ..typed(Type::Array)
            },
            json!({
                "type": "array",
                "items": {"type": "string", "maxLength": 4},
                "minItems": 1,
                "maxItems": 3
            }),
        ),
        (
            "array without items omits items key",
            typed(Type::Array),
            json!({"type": "array"}),
        ),
        (
            "array constraints are dropped for non-array type",
            Schema {
                items: Some(Box::new(typed(Type::String))),
                min_items: Some(1),
                max_items: Some(3),
                ..typed(Type::String)
            },
            json!({"type": "string"}),
        ),
        (
            "object properties and constraints are converted recursively",
            Schema {
                properties: props([
                    (
                        "name",
                        Schema {
                            max_length: Some(10),
                            ..typed(Type::String)
                        },
                    ),
                    (
                        "tags",
                        Schema {
                            items: Some(Box::new(typed(Type::String))),
                            ..typed(Type::Array)
                        },
                    ),
                ]),
                required: vec!["name".into()],
                min_properties: Some(1),
                max_properties: Some(2),
                ..typed(Type::Object)
            },
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "maxLength": 10},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["name"],
                "minProperties": 1,
                "maxProperties": 2
            }),
        ),
        (
            "object constraints are dropped for non-object type",
            Schema {
                properties: props([("name", typed(Type::String))]),
                required: vec!["name".into()],
                min_properties: Some(1),
                max_properties: Some(2),
                ..typed(Type::String)
            },
            json!({"type": "string"}),
        ),
        (
            "propertyOrdering is stripped at every depth while user property named propertyOrdering is preserved",
            Schema {
                property_ordering: vec!["nested".into(), "propertyOrdering".into(), "list".into()],
                properties: props([
                    ("propertyOrdering", typed(Type::String)),
                    (
                        "nested",
                        Schema {
                            property_ordering: vec!["leaf".into()],
                            properties: props([("leaf", typed(Type::String))]),
                            ..typed(Type::Object)
                        },
                    ),
                    (
                        "list",
                        Schema {
                            items: Some(Box::new(Schema {
                                property_ordering: vec!["item".into()],
                                properties: props([("item", typed(Type::Integer))]),
                                ..typed(Type::Object)
                            })),
                            ..typed(Type::Array)
                        },
                    ),
                ]),
                any_of: vec![Schema {
                    property_ordering: vec!["branch".into()],
                    properties: props([("branch", typed(Type::Boolean))]),
                    ..typed(Type::Object)
                }],
                default: Some(json!({"propertyOrdering": ["keep"], "type": "STRING"})),
                ..typed(Type::Object)
            },
            json!({
                "type": "object",
                "properties": {
                    "propertyOrdering": {"type": "string"},
                    "nested": {"type": "object", "properties": {"leaf": {"type": "string"}}},
                    "list": {
                        "type": "array",
                        "items": {"type": "object", "properties": {"item": {"type": "integer"}}}
                    }
                },
                "anyOf": [{"type": "object", "properties": {"branch": {"type": "boolean"}}}],
                "default": {"propertyOrdering": ["keep"], "type": "STRING"}
            }),
        ),
        (
            "anyOf subschemas are converted recursively",
            Schema {
                any_of: vec![
                    typed(Type::String),
                    Schema {
                        minimum: Some(0.0),
                        ..typed(Type::Integer)
                    },
                ],
                ..Default::default()
            },
            json!({"anyOf": [{"type": "string"}, {"type": "integer", "minimum": 0.0}]}),
        ),
        (
            "explicit non-object type is preserved alongside anyOf",
            Schema {
                any_of: vec![typed(Type::String), typed(Type::Integer)],
                ..typed(Type::String)
            },
            json!({"type": "string", "anyOf": [{"type": "string"}, {"type": "integer"}]}),
        ),
        (
            "object union placeholder with non-object anyOf branches omits outer object type",
            Schema {
                any_of: vec![typed(Type::String), typed(Type::Integer)],
                ..typed(Type::Object)
            },
            json!({"anyOf": [{"type": "string"}, {"type": "integer"}]}),
        ),
        (
            "object union placeholder keeps properties and required",
            Schema {
                properties: props([("name", typed(Type::String))]),
                required: vec!["name".into()],
                any_of: vec![typed(Type::String), typed(Type::Integer)],
                ..typed(Type::Object)
            },
            json!({
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
                "anyOf": [{"type": "string"}, {"type": "integer"}]
            }),
        ),
        (
            "object anyOf of objects and unspecified types keeps object type",
            Schema {
                any_of: vec![
                    Schema {
                        required: vec!["a".into()],
                        ..typed(Type::Object)
                    },
                    typed(Type::Unspecified),
                    Schema::default(),
                ],
                ..typed(Type::Object)
            },
            json!({
                "type": "object",
                "anyOf": [{"type": "object", "required": ["a"]}, {}, {}]
            }),
        ),
    ];
    for (name, schema, want) in cases {
        let got = gemini_to_json_schema(&schema)
            .unwrap_or_else(|e| panic!("{name}: geminiToJSONSchema() error = {e}"));
        assert_eq!(
            Value::Object(got),
            want,
            "{name}: geminiToJSONSchema() mismatch"
        );
    }
}
