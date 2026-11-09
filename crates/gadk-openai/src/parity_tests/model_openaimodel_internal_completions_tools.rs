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

//! Port of ADK-Go `model/openaimodel/internal/completions/tools_test.go`.

use gadk_genai::{
    FunctionCallingConfig, FunctionCallingConfigMode, FunctionDeclaration, GenerateContentConfig,
    GoogleSearch, Schema, Tool, ToolConfig, Type,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::model_openaimodel_internal_completions_request::{
    build_err, field, hi_req, request_wire,
};

/// Go `toolReq(cfg)`: one user "hi" turn and `cfg`.
fn tool_req(cfg: GenerateContentConfig) -> gadk_model::LlmRequest {
    hi_req(cfg)
}

/// Go `weatherTool`.
fn weather_tool() -> Tool {
    Tool {
        function_declarations: vec![FunctionDeclaration {
            name: "get_weather".to_string(),
            description: "look up the weather".to_string(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: [(
                    "city".to_string(),
                    Schema {
                        r#type: Some(Type::String),
                        ..Schema::default()
                    },
                )]
                .into(),
                ..Schema::default()
            }),
            ..FunctionDeclaration::default()
        }],
        ..Tool::default()
    }
}

/// A config of `tools` and a function-calling config of `mode` and `names`.
fn choice_cfg(
    tools: Vec<Tool>,
    mode: Option<FunctionCallingConfigMode>,
    names: &[&str],
) -> GenerateContentConfig {
    GenerateContentConfig {
        tools,
        tool_config: Some(ToolConfig {
            function_calling_config: Some(FunctionCallingConfig {
                mode,
                allowed_function_names: names.iter().map(|name| name.to_string()).collect(),
                ..FunctionCallingConfig::default()
            }),
            ..ToolConfig::default()
        }),
        ..GenerateContentConfig::default()
    }
}

/// The `function` object of the first tool on the wire.
fn first_function(wire: &serde_json::Map<String, Value>) -> Value {
    field(wire, "tools")[0]["function"].clone()
}

/// The extra "function" level Chat Completions requires and the flat Responses shape does not
/// have.
#[test]
fn test_convert_tools_nested_function_shape() {
    let wire = request_wire(&tool_req(GenerateContentConfig {
        tools: vec![weather_tool()],
        ..GenerateContentConfig::default()
    }));
    let tools = field(&wire, "tools");
    let Some(tools) = tools.as_array().filter(|tools| tools.len() == 1) else {
        panic!("tools = {tools:?}, want one");
    };
    let tool = &tools[0];
    assert_eq!(tool["type"], json!("function"), "type");
    let function = &tool["function"];
    assert!(function.is_object(), "no nested function object: {tool:?}");
    assert!(
        function["name"] == json!("get_weather")
            && function["description"] == json!("look up the weather"),
        "function = {function:?}"
    );
    // The flat form would have put these on the tool itself.
    assert!(
        tool.get("name").is_none(),
        "name on the tool; that is the Responses shape"
    );
    assert_eq!(
        function["parameters"]["type"],
        json!("object"),
        "parameters type, want lowercase object"
    );
}

#[test]
fn test_convert_tools_no_parameters_defaults_to_empty_object() {
    let wire = request_wire(&tool_req(GenerateContentConfig {
        tools: vec![Tool {
            function_declarations: vec![FunctionDeclaration {
                name: "ping".to_string(),
                ..FunctionDeclaration::default()
            }],
            ..Tool::default()
        }],
        ..GenerateContentConfig::default()
    }));
    let params = &first_function(&wire)["parameters"];
    assert_eq!(
        params["type"],
        json!("object"),
        "parameters = {params:?}, want an empty object schema"
    );
}

/// The schema functiontool.New emits, whose optional properties must stay optional: the
/// strict rewrite that response schemas get would list every property as required.
#[test]
fn test_convert_tools_parameters_json_schema() {
    let wire = request_wire(&tool_req(GenerateContentConfig {
        tools: vec![Tool {
            function_declarations: vec![FunctionDeclaration {
                name: "get_weather".to_string(),
                parameters_json_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"},
                        "units": {"type": "string"},
                    },
                    "required": ["city"],
                    "additionalProperties": false,
                })),
                ..FunctionDeclaration::default()
            }],
            ..Tool::default()
        }],
        ..GenerateContentConfig::default()
    }));
    let params = first_function(&wire)["parameters"].clone();
    let props = params["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    assert!(
        props.contains_key("units") && props.len() == 2,
        "properties = {:?}, want city and units",
        params["properties"]
    );
    assert_eq!(
        params["required"],
        json!(["city"]),
        "required, want only city"
    );
    assert_eq!(
        params["additionalProperties"],
        json!(false),
        "additionalProperties"
    );
}

#[test]
fn test_convert_tools_rejects_non_function_tools() {
    let err = build_err(&tool_req(GenerateContentConfig {
        tools: vec![Tool {
            google_search: Some(GoogleSearch::default()),
            ..Tool::default()
        }],
        ..GenerateContentConfig::default()
    }));
    assert!(
        err.to_string().contains("non-function tools"),
        "err = {err}, want a non-function-tool error"
    );
}

#[test]
fn test_convert_tool_choice_modes() {
    // (name, mode, want; None = absent)
    let tests = [
        (
            "auto leaves it to the provider",
            FunctionCallingConfigMode::Auto,
            None,
        ),
        ("none", FunctionCallingConfigMode::None, Some("none")),
        (
            "any becomes required",
            FunctionCallingConfigMode::Any,
            Some("required"),
        ),
    ];
    for (name, mode, want) in tests {
        let wire = request_wire(&tool_req(choice_cfg(vec![weather_tool()], Some(mode), &[])));
        let got = wire.get("tool_choice");
        match want {
            None => assert!(
                got.is_none(),
                "{name}: tool_choice = {got:?}, want it absent"
            ),
            Some(want) => assert_eq!(got, Some(&json!(want)), "{name}: tool_choice"),
        }
    }
}

#[test]
fn test_convert_tool_choice_unsupported_mode() {
    let err = build_err(&tool_req(choice_cfg(
        vec![weather_tool()],
        Some(FunctionCallingConfigMode::Unknown("TELEPATHY".to_string())),
        &[],
    )));
    assert!(
        err.to_string().contains("TELEPATHY"),
        "err = {err}, want one naming the mode"
    );
}

/// The nesting again: an allowed tool names its function one level deeper than the Responses
/// form.
#[test]
fn test_convert_tool_choice_allowed_tools() {
    let tests: [(&str, FunctionCallingConfigMode, &[&str], &str); 2] = [
        (
            "auto with one name",
            FunctionCallingConfigMode::Auto,
            &["get_weather"],
            "auto",
        ),
        (
            "any with several names",
            FunctionCallingConfigMode::Any,
            &["get_weather", "get_time"],
            "required",
        ),
    ];
    for (name, mode, names, want_mode) in tests {
        let wire = request_wire(&tool_req(choice_cfg(
            vec![weather_tool()],
            Some(mode),
            names,
        )));
        let choice = field(&wire, "tool_choice");
        assert!(
            choice.is_object(),
            "{name}: tool_choice = {choice:?}, want an allowed-tools object"
        );
        assert_eq!(choice["type"], json!("allowed_tools"), "{name}: type");
        let allowed = &choice["allowed_tools"];
        assert_eq!(allowed["mode"], json!(want_mode), "{name}: mode");
        let entries = allowed["tools"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            entries.len(),
            names.len(),
            "{name}: allowed tools = {entries:?}"
        );
        for (i, raw) in entries.iter().enumerate() {
            assert_eq!(
                raw["function"]["name"],
                json!(names[i]),
                "{name}: allowed tool {i} = {raw:?}, want {:?} nested under function",
                names[i]
            );
        }
    }
}

/// The older named-function form for the one case it can express, which compatible providers
/// accept where many reject allowed_tools.
#[test]
fn test_convert_tool_choice_any_with_one_name_names_the_function() {
    let wire = request_wire(&tool_req(choice_cfg(
        vec![weather_tool()],
        Some(FunctionCallingConfigMode::Any),
        &["", "get_weather"],
    )));
    let choice = field(&wire, "tool_choice");
    assert!(
        choice.is_object(),
        "tool_choice = {choice:?}, want a named-function object"
    );
    assert_eq!(choice["type"], json!("function"), "type");
    assert_eq!(
        choice["function"]["name"],
        json!("get_weather"),
        "tool_choice = {choice:?}, want get_weather named under function"
    );
}

/// tool_choice travels only with tools: the API rejects one sent without them, "none"
/// included, which a turn whose toolset came up empty would otherwise hit.
#[test]
fn test_build_params_tool_choice_needs_tools() {
    for mode in [
        FunctionCallingConfigMode::None,
        FunctionCallingConfigMode::Any,
    ] {
        let name = mode.to_string();
        let wire = request_wire(&tool_req(choice_cfg(Vec::new(), Some(mode), &[])));
        let got = wire.get("tool_choice");
        assert!(
            got.is_none(),
            "{name}: tool_choice = {got:?}, want it absent without tools"
        );
    }
}

#[test]
fn test_convert_tool_choice_all_blank_names_falls_back() {
    let wire = request_wire(&tool_req(choice_cfg(
        vec![weather_tool()],
        Some(FunctionCallingConfigMode::Any),
        &[""],
    )));
    // Nothing nameable was allowed, so no choice is sent rather than an empty allowed-tools
    // object the API would reject.
    let got = wire.get("tool_choice");
    assert!(got.is_none(), "tool_choice = {got:?}, want it absent");
}
