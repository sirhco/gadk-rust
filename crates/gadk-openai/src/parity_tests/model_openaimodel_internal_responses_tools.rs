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

//! Port of ADK-Go `model/openaimodel/internal/responses/tools_test.go`.
//!
//! Go compares openai-go param structs; these compare the encoded params (Go's probe of the
//! same calls gave the bytes).

use std::collections::BTreeMap;

use gadk_genai::{
    FunctionCallingConfig, FunctionCallingConfigMode, FunctionDeclaration, GenerateContentConfig,
    GoogleSearch, Schema, Tool, ToolConfig, Type,
};
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::{Value, json};

use crate::responses::tools::{convert_function_declaration, convert_tool_choice, convert_tools};

/// Go `json.Marshal` of a param: the bytes openai-go would send, as
/// [`crate::client::encode_body`] writes them. Shared with the `responses/request_test.go`
/// directory target.
pub(super) fn wire<T: Serialize>(value: &T) -> String {
    let body = crate::client::encode_body(value, false).unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

/// [`wire`] decoded back, for reading single fields off the encoded param.
pub(super) fn wire_value<T: Serialize>(value: &T) -> Value {
    serde_json::from_str(&wire(value)).unwrap()
}

fn decl(name: &str) -> FunctionDeclaration {
    FunctionDeclaration {
        name: name.to_string(),
        ..FunctionDeclaration::default()
    }
}

fn typed(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Schema::default()
    }
}

#[test]
fn test_convert_tools() {
    let tests: Vec<(&str, Option<GenerateContentConfig>, usize, bool)> = vec![
        (
            "valid config",
            Some(GenerateContentConfig {
                tools: vec![Tool {
                    function_declarations: vec![decl("fn1")],
                    ..Tool::default()
                }],
                ..GenerateContentConfig::default()
            }),
            1,
            false,
        ),
        ("empty config", None, 0, false),
        (
            "invalid tool",
            Some(GenerateContentConfig {
                tools: vec![Tool {
                    google_search: Some(GoogleSearch::default()),
                    ..Tool::default()
                }],
                ..GenerateContentConfig::default()
            }),
            0,
            true,
        ),
        (
            "invalid function declaration",
            Some(GenerateContentConfig {
                tools: vec![Tool {
                    // Missing name.
                    function_declarations: vec![FunctionDeclaration::default()],
                    ..Tool::default()
                }],
                ..GenerateContentConfig::default()
            }),
            0,
            true,
        ),
    ];
    for (name, cfg, want_len, want_err) in tests {
        let result = convert_tools(cfg.as_ref());
        assert_eq!(
            result.is_err(),
            want_err,
            "{name}: convertTools() = {result:?}, wantErr {want_err}"
        );
        let Ok(tools) = result else {
            continue;
        };
        assert_eq!(tools.len(), want_len, "{name}: tool count");
        let encoded = wire_value(&tools);
        if want_len > 0 {
            assert_eq!(
                encoded[0]["name"], "fn1",
                "{name}: unexpected tool: {encoded}"
            );
        }
        for (i, tool) in encoded
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            assert_eq!(
                tool["type"], "function",
                "{name}: tool {i} is not a function tool"
            );
            assert_eq!(
                tool.get("strict"),
                Some(&json!(false)),
                "{name}: tool {i} strict, want an explicit false"
            );
        }
    }
}

// parity: adapted: Go's "invalid ParametersJsonSchema" passes a func, which json.Marshal rejects with "json: unsupported type"; a gadk JSON schema is a serde_json::Value, which always marshals, so that case is a JSON array, which the map decode rejects ("openai: unmarshal json schema: json: cannot unmarshal array …")
#[test]
fn test_convert_function_declaration() {
    let tests: Vec<(&str, Option<FunctionDeclaration>, &str)> = vec![
        ("nil declaration", None, "nil function declaration"),
        (
            "missing name",
            Some(FunctionDeclaration::default()),
            "missing name",
        ),
        (
            "valid",
            Some(FunctionDeclaration {
                description: "A test function".to_string(),
                ..decl("test_func")
            }),
            "",
        ),
        (
            "with ParametersJsonSchema",
            Some(FunctionDeclaration {
                parameters_json_schema: Some(json!({"type": "object"})),
                ..decl("test_func")
            }),
            "",
        ),
        (
            "invalid ParametersJsonSchema",
            Some(FunctionDeclaration {
                parameters_json_schema: Some(json!([])),
                ..decl("test_func")
            }),
            "expected a JSON object, found an array",
        ),
    ];
    for (name, declaration, want_err) in tests {
        let result = convert_function_declaration(declaration.as_ref());
        if want_err.is_empty() {
            let fn_param =
                result.unwrap_or_else(|err| panic!("{name}: expected no error, got {err}"));
            let declaration = declaration.unwrap_or_default();
            assert_eq!(fn_param.name, declaration.name, "{name}: unexpected fn");
            assert_eq!(
                wire_value(&fn_param)["parameters"]["type"],
                "object",
                "{name}: expected default object schema"
            );
        } else {
            match result {
                Err(err) if err.to_string().contains(want_err) => {}
                other => panic!("{name}: expected error containing {want_err:?}, got: {other:?}"),
            }
        }
    }
    assert_eq!(
        wire(
            &convert_function_declaration(Some(&FunctionDeclaration {
                description: "A test function".to_string(),
                ..decl("test_func")
            }))
            .unwrap()
        ),
        r#"{"strict":false,"parameters":{"properties":{},"type":"object"},"name":"test_func","description":"A test function","type":"function"}"#
    );
}

// TestConvertFunctionDeclarationPinsStrictOff covers every parameter path, because the API
// decides the validation mode from the schema shape when strict is absent.
#[test]
fn test_convert_function_declaration_pins_strict_off() {
    let tests: Vec<(&str, FunctionDeclaration)> = vec![
        ("no parameters", decl("fn")),
        (
            "genai schema parameters",
            FunctionDeclaration {
                parameters: Some(Schema {
                    r#type: Some(Type::Object),
                    properties: BTreeMap::from([("city".to_string(), typed(Type::String))]),
                    required: vec!["city".to_string()],
                    ..Schema::default()
                }),
                ..decl("fn")
            },
        ),
        // A hand-written strict-compatible schema is exactly where an absent flag would flip
        // the API into strict mode.
        (
            "strict-compatible ParametersJsonSchema",
            FunctionDeclaration {
                parameters_json_schema: Some(json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"],
                    "additionalProperties": false,
                })),
                ..decl("fn")
            },
        ),
    ];
    for (name, declaration) in tests {
        let fn_param = convert_function_declaration(Some(&declaration))
            .unwrap_or_else(|err| panic!("{name}: convertFunctionDeclaration() error = {err}"));
        assert_eq!(
            fn_param.strict,
            Some(false),
            "{name}: Strict unset or true; the API would choose the validation mode"
        );
    }
}

// TestConvertFunctionDeclarationMarshalsStrict guards the wire format: strict false has to be
// sent, not dropped as a zero value.
#[test]
fn test_convert_function_declaration_marshals_strict() {
    let fn_param = convert_function_declaration(Some(&decl("fn"))).unwrap();
    let data = wire(&fn_param);
    let payload = wire_value(&fn_param);
    assert_eq!(
        payload.get("strict"),
        Some(&json!(false)),
        "strict missing or not false in payload {data}"
    );
    assert_eq!(
        data,
        r#"{"strict":false,"parameters":{"properties":{},"type":"object"},"name":"fn","type":"function"}"#
    );
}

// TestConvertFunctionDeclarationKeepsOptionalParameters: the strict-schema rewrite would make
// optional tool arguments mandatory, so it is pinned out of all three parameter paths.
#[test]
fn test_convert_function_declaration_keeps_optional_parameters() {
    let tests: Vec<(&str, FunctionDeclaration, Option<Value>, Option<Value>)> = vec![
        // genai.Schema carries no additionalProperties; it must not be injected.
        (
            "genai schema parameters",
            FunctionDeclaration {
                parameters: Some(Schema {
                    r#type: Some(Type::Object),
                    properties: BTreeMap::from([
                        ("city".to_string(), typed(Type::String)),
                        ("units".to_string(), typed(Type::String)),
                    ]),
                    required: vec!["city".to_string()],
                    ..Schema::default()
                }),
                ..decl("fn")
            },
            Some(json!(["city"])),
            None,
        ),
        // What functiontool.New emits for an omitempty/omitzero field.
        (
            "ParametersJsonSchema with an optional property",
            FunctionDeclaration {
                parameters_json_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"},
                        "units": {"type": "string"},
                    },
                    "required": ["city"],
                    "additionalProperties": false,
                })),
                ..decl("fn")
            },
            Some(json!(["city"])),
            Some(json!(false)),
        ),
        // The default schema for a tool that takes no arguments.
        ("no parameters", decl("fn"), None, None),
    ];
    for (name, declaration, want_required, want_additional) in tests {
        let fn_param = convert_function_declaration(Some(&declaration))
            .unwrap_or_else(|err| panic!("{name}: convertFunctionDeclaration() error = {err}"));
        let parameters = wire_value(&fn_param)["parameters"].clone();
        // An absent key is what the None expectations mean.
        assert_eq!(
            parameters.get("required").cloned(),
            want_required,
            "{name}: required, in {parameters}"
        );
        assert_eq!(
            parameters.get("additionalProperties").cloned(),
            want_additional,
            "{name}: additionalProperties, in {parameters}"
        );
    }
}

fn calling(mode: Option<FunctionCallingConfigMode>, names: &[&str]) -> Option<ToolConfig> {
    Some(ToolConfig {
        function_calling_config: Some(FunctionCallingConfig {
            mode,
            allowed_function_names: names.iter().map(ToString::to_string).collect(),
            ..FunctionCallingConfig::default()
        }),
        ..ToolConfig::default()
    })
}

#[test]
fn test_convert_tool_choice() {
    use FunctionCallingConfigMode as Mode;
    let allowed = |mode: &str| {
        format!(
            r#"{{"mode":"{mode}","tools":[{{"name":"fn1","type":"function"}}],"type":"allowed_tools"}}"#
        )
    };
    let tests: Vec<(&str, Option<ToolConfig>, bool, Option<String>)> = vec![
        ("nil cfg", None, false, None),
        (
            "mode none",
            calling(Some(Mode::None), &[]),
            false,
            Some(r#""none""#.to_string()),
        ),
        (
            "mode unspecified empty",
            calling(Some(Mode::Unspecified), &[]),
            false,
            None,
        ),
        (
            "mode unspecified with names",
            calling(Some(Mode::Unspecified), &["fn1"]),
            false,
            Some(allowed("auto")),
        ),
        (
            "mode auto empty",
            calling(Some(Mode::Auto), &[]),
            false,
            None,
        ),
        (
            "mode auto with names",
            calling(Some(Mode::Auto), &["fn1"]),
            false,
            Some(allowed("auto")),
        ),
        (
            "mode any empty",
            calling(Some(Mode::Any), &[]),
            false,
            Some(r#""required""#.to_string()),
        ),
        (
            "mode any with names",
            calling(Some(Mode::Any), &["fn1", ""]),
            false,
            Some(allowed("required")),
        ),
        (
            "invalid mode",
            calling(Some("invalid".parse::<Mode>().unwrap()), &[]),
            true,
            None,
        ),
    ];
    for (name, tool_cfg, want_err, want) in tests {
        let result = convert_tool_choice(tool_cfg.as_ref());
        assert_eq!(
            result.is_err(),
            want_err,
            "{name}: convertToolChoice() = {result:?}, wantErr {want_err}"
        );
        let got = result.ok().flatten().map(|choice| wire(&choice));
        assert_eq!(got, want, "{name}: convertToolChoice()");
    }
    let err = convert_tool_choice(calling(Some("invalid".parse::<Mode>().unwrap()), &[]).as_ref())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"openai: unsupported tool calling mode "invalid""#
    );
}
