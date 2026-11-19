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

//! Ports of TestToolsAndToolConfig, TestExtendedThinkingAndEffort,
//! TestThinkingAndEffortOmitSamplingParamsAndDefaultMaxTokens, TestPromptCaching.
//!
//! Go's handlers decode each request body into `lastBody map[string]any`; here the fake server
//! records every request and `last_body` decodes the newest one. Go's nil-safe map lookups
//! (`m["k"].(map[string]any)`) become `serde_json::Value` indexing, which yields `Null` for a
//! missing key just as Go's lookups yield `nil`. Go subtests are named blocks inside one test
//! sharing the server and model, as Go's subtests do; table cases loop over named rows and
//! every assertion message names the case.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::str::FromStr;

use bytes::Bytes;
use gadk_anthropic::{
    CACHE_CREATION_INPUT_TOKENS_METADATA_KEY, ClientConfig, DEFAULT_MAX_TOKENS, Effort, ErrorKind,
    PromptCacheConfig, PromptCacheTtl,
};
use gadk_genai::{
    ComputerUse, Content, EnterpriseWebSearch, FileSearch, FunctionCall, FunctionCallingConfig,
    FunctionCallingConfigMode, FunctionDeclaration, FunctionResponse, GenerateContentConfig,
    GenerateContentResponseUsageMetadata, GoogleMaps, GoogleSearch, GoogleSearchRetrieval,
    MCPServer, Part, ROLE_MODEL, ROLE_USER, Retrieval, Schema, ThinkingConfig, ThinkingLevel, Tool,
    ToolCodeExecution, ToolConfig, ToolExaAISearch, ToolParallelAISearch, Type, URLContext,
};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use serde_json::{Map, Value, json};
use tracing::Level;

use crate::harness::{
    FakeResponse, FakeServer, LogCapture, MODEL, anthropic_config, anthropic_error, generate,
    generate_last, generate_ok, new_model, sse_event, text_request,
};

/// Go `map[string]any{...}` literal for `FunctionCall.Args` / `FunctionResponse.Response`.
fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not a JSON object: {other}"),
    }
}

/// Go `lastBody`: the newest request body decoded as JSON.
fn last_body(srv: &FakeServer) -> Value {
    srv.last_request().json()
}

/// Go `len(x.([]any))`: 0 when `v` is not an array.
fn array_len(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// Go `&model.LLMRequest{Contents: genai.Text(text), Config: cfg}`.
fn text_request_with(text: &str, cfg: GenerateContentConfig) -> LlmRequest {
    LlmRequest {
        config: Some(cfg),
        ..text_request(text)
    }
}

/// Go's `ClientConfig{APIKey: "test-key", BaseURL: srv.URL, Effort: eff}`.
fn effort_config(base_url: &str, effort: Effort) -> ClientConfig {
    ClientConfig {
        effort: Some(effort),
        ..anthropic_config(base_url)
    }
}

/// Go `for _, err := range m.GenerateContent(...) { if err != nil { t.Fatalf(...) } }`, with
/// the case named in the panic message.
async fn generate_ok_named(m: &dyn Llm, req: LlmRequest, stream: bool, what: &str) {
    for item in generate(m, req, stream).await {
        if let Err(e) = item {
            panic!("{what} error = {e}");
        }
    }
}

/// Go's rejected-config loop: `for _, err := range m.GenerateContent(...)` with
/// `errors.Is(err, ErrInvalidConfig)` and `strings.Contains(err.Error(), wantSub)`. Beyond Go,
/// the stream must yield exactly one item and the whole text must be Go's `Error()`.
fn assert_invalid_config(
    items: &[Result<LlmResponse, ModelError>],
    name: &str,
    want_sub: &str,
    want_text: &str,
) {
    assert_eq!(items.len(), 1, "{name}: items = {items:?}, want one error");
    for item in items {
        let err = item
            .as_ref()
            .expect_err(&format!("{name}: error = <nil>, want ErrInvalidConfig"));
        assert!(
            anthropic_error(err).is(ErrorKind::InvalidConfig),
            "{name}: error = {err}, want ErrInvalidConfig"
        );
        let text = err.to_string();
        assert!(
            text.contains(want_sub),
            "{name}: error = {text:?}, want substring {want_sub:?}"
        );
        assert_eq!(text, want_text, "{name}");
    }
}

/// Go `weatherTools`: two function declarations, one with a genai `Schema` and one with a
/// `ParametersJsonSchema` in upper-case genai type names.
fn weather_tools() -> Vec<Tool> {
    vec![Tool {
        function_declarations: vec![
            FunctionDeclaration {
                name: "get_weather".to_string(),
                description: "Look up weather.".to_string(),
                parameters: Some(Schema {
                    r#type: Some(Type::Object),
                    properties: [
                        (
                            "city".to_string(),
                            Schema {
                                r#type: Some(Type::String),
                                description: "City name".to_string(),
                                ..Default::default()
                            },
                        ),
                        (
                            "days".to_string(),
                            Schema {
                                r#type: Some(Type::Array),
                                items: Some(Box::new(Schema {
                                    r#type: Some(Type::Integer),
                                    ..Default::default()
                                })),
                                ..Default::default()
                            },
                        ),
                    ]
                    .into_iter()
                    .collect(),
                    required: vec!["city".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            FunctionDeclaration {
                name: "get_time".to_string(),
                description: "Look up time.".to_string(),
                parameters_json_schema: Some(json!({
                    "type": "OBJECT",
                    "properties": {
                        "tz": {"type": ["STRING", "NULL"]},
                    },
                })),
                ..Default::default()
            },
        ],
        ..Default::default()
    }]
}

/// Go `&genai.ToolConfig{FunctionCallingConfig: fcc}`.
fn fc_tool_config(fcc: FunctionCallingConfig) -> Option<ToolConfig> {
    Some(ToolConfig {
        function_calling_config: Some(fcc),
        ..Default::default()
    })
}

/// A tool with one declaration named `fn` (Go `[]*genai.FunctionDeclaration{{Name: "fn"}}`).
fn fn_decl() -> Vec<FunctionDeclaration> {
    vec![FunctionDeclaration {
        name: "fn".to_string(),
        ..Default::default()
    }]
}

#[tokio::test]
async fn test_tools_and_tool_config() {
    let srv = FakeServer::json(
        r#"{"id":"msg_t","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"Paris"}},{"type":"tool_use","id":"toolu_2","name":"get_time","input":{"tz":"UTC"}}],"usage":{"input_tokens":15,"output_tokens":12}}"#,
    );
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    // ---- schema_normalization_and_parallel_tool_calls_and_id_sanitization
    {
        let req = LlmRequest {
            contents: vec![
                Content::from_text("Weather and time in Paris?", ROLE_USER),
                Content::from_parts(
                    vec![
                        Part {
                            function_call: Some(FunctionCall {
                                id: "bad id#1".to_string(),
                                name: "get_weather".to_string(),
                                args: obj(json!({"city": "Paris"})),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        Part {
                            function_call: Some(FunctionCall {
                                id: String::new(),
                                name: "get_time".to_string(),
                                args: obj(json!({"tz": "UTC"})),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                    ROLE_MODEL,
                ),
                Content::from_parts(
                    vec![
                        Part {
                            function_response: Some(FunctionResponse {
                                id: "bad id#1".to_string(),
                                name: "get_weather".to_string(),
                                response: obj(json!({"result": "Sunny 22C"})),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        Part {
                            function_response: Some(FunctionResponse {
                                id: String::new(),
                                name: "get_time".to_string(),
                                response: obj(
                                    json!({"content": [{"type": "text", "text": "12:00 UTC"}]}),
                                ),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                    ROLE_USER,
                ),
            ],
            config: Some(GenerateContentConfig {
                tools: weather_tools(),
                tool_config: fc_tool_config(FunctionCallingConfig {
                    mode: Some(FunctionCallingConfigMode::Any),
                    allowed_function_names: vec!["get_weather".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let got = generate_last(&*m, req, false).await;
        let body = last_body(&srv);

        // Check normalized tool schemas in captured request body.
        let tools = &body["tools"];
        assert_eq!(
            array_len(tools),
            2,
            "sent {} tools, want 2",
            array_len(tools)
        );
        let input_schema = &tools[0]["input_schema"];
        assert_eq!(
            input_schema["type"],
            json!("object"),
            "input_schema.type = {}, want \"object\"",
            input_schema["type"]
        );
        let props = &input_schema["properties"];
        assert_eq!(
            props["city"]["type"],
            json!("string"),
            "city.type = {}, want \"string\"",
            props["city"]["type"]
        );
        assert_eq!(
            props["days"]["type"],
            json!("array"),
            "days.type = {}, want \"array\"",
            props["days"]["type"]
        );

        // Check tool_choice mapped to {"type":"tool","name":"get_weather"}.
        let tc = &body["tool_choice"];
        assert!(
            tc["type"] == json!("tool") && tc["name"] == json!("get_weather"),
            "tool_choice = {tc}, want type=tool name=get_weather"
        );

        // Check deterministic tool_use_id sanitization paired across assistant and user turns.
        let msgs = &body["messages"];
        assert_eq!(
            array_len(msgs),
            3,
            "len(messages) = {}, want 3",
            array_len(msgs)
        );
        let asst = &msgs[1]["content"];
        let user = &msgs[2]["content"];
        let (tu0, tu1, tr0, tr1) = (&asst[0], &asst[1], &user[0], &user[1]);
        assert!(
            tu0["id"] == json!("toolu_fallback_0")
                && tr0["tool_use_id"] == json!("toolu_fallback_0"),
            "first pair IDs = ({}, {}), want toolu_fallback_0",
            tu0["id"],
            tr0["tool_use_id"]
        );
        assert!(
            tu1["id"] == json!("toolu_fallback_1")
                && tr1["tool_use_id"] == json!("toolu_fallback_1"),
            "second pair IDs = ({}, {}), want toolu_fallback_1",
            tu1["id"],
            tr1["tool_use_id"]
        );

        // Check response returned both parallel FunctionCalls.
        let parts = got
            .content
            .as_ref()
            .map(|c| c.parts.as_slice())
            .unwrap_or_default();
        assert_eq!(parts.len(), 2, "got {got:?}, want 2 FunctionCall parts");
        assert_eq!(
            parts[0].function_call.as_ref().map(|f| f.name.as_str()),
            Some("get_weather"),
            "part[0] = {:?}, want get_weather FunctionCall",
            parts[0]
        );
        assert_eq!(
            parts[1].function_call.as_ref().map(|f| f.name.as_str()),
            Some("get_time"),
            "part[1] = {:?}, want get_time FunctionCall",
            parts[1]
        );
    }

    // ---- tool_config_modes_auto_any_none
    {
        let modes = [
            (FunctionCallingConfigMode::Auto, "auto"),
            (FunctionCallingConfigMode::Any, "any"),
            (FunctionCallingConfigMode::None, "none"),
        ];
        for (mode, want_type) in modes {
            let req = text_request_with(
                "hi",
                GenerateContentConfig {
                    tools: weather_tools(),
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        mode: Some(mode.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
            generate_ok_named(&*m, req, false, &format!("mode {mode}")).await;
            let tc = last_body(&srv)["tool_choice"].clone();
            assert_eq!(
                tc["type"],
                json!(want_type),
                "mode {mode}: tool_choice.type = {}, want {want_type:?}",
                tc["type"]
            );
        }
    }

    // ---- rejected_tools_and_tool_configs
    {
        let budget = 1024;
        // (name, config, Go's wantSub, Go's whole error text)
        let cases: Vec<(&str, GenerateContentConfig, &str, &str)> = vec![
            (
                "google_search",
                GenerateContentConfig {
                    tools: vec![Tool {
                        google_search: Some(GoogleSearch::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "google_search",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.google_search is not supported by Claude",
            ),
            (
                "google_search_retrieval",
                GenerateContentConfig {
                    tools: vec![Tool {
                        google_search_retrieval: Some(GoogleSearchRetrieval::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "google_search_retrieval",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.google_search_retrieval is not supported by Claude",
            ),
            (
                "retrieval",
                GenerateContentConfig {
                    tools: vec![Tool {
                        retrieval: Some(Retrieval::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "retrieval",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.retrieval is not supported by Claude",
            ),
            (
                "code_execution",
                GenerateContentConfig {
                    tools: vec![Tool {
                        code_execution: Some(ToolCodeExecution::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "code_execution",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.code_execution is not supported by Claude",
            ),
            (
                "enterprise_web_search",
                GenerateContentConfig {
                    tools: vec![Tool {
                        enterprise_web_search: Some(EnterpriseWebSearch::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "enterprise_web_search",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.enterprise_web_search is not supported by Claude",
            ),
            (
                "google_maps",
                GenerateContentConfig {
                    tools: vec![Tool {
                        google_maps: Some(GoogleMaps::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "google_maps",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.google_maps is not supported by Claude",
            ),
            (
                "computer_use",
                GenerateContentConfig {
                    tools: vec![Tool {
                        computer_use: Some(ComputerUse::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "computer_use",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.computer_use is not supported by Claude",
            ),
            (
                "url_context",
                GenerateContentConfig {
                    tools: vec![Tool {
                        url_context: Some(URLContext::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "url_context",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.url_context is not supported by Claude",
            ),
            (
                "file_search",
                GenerateContentConfig {
                    tools: vec![Tool {
                        file_search: Some(FileSearch::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "file_search",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.file_search is not supported by Claude",
            ),
            (
                "parallel_ai_search_with_function_declarations",
                GenerateContentConfig {
                    tools: vec![Tool {
                        function_declarations: fn_decl(),
                        parallel_ai_search: Some(ToolParallelAISearch::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "parallel_ai_search",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.parallel_ai_search is not supported by Claude",
            ),
            (
                "mcp_servers_with_function_declarations",
                GenerateContentConfig {
                    tools: vec![Tool {
                        function_declarations: fn_decl(),
                        mcp_servers: vec![MCPServer {
                            name: "srv".to_string(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "mcp_servers",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.mcp_servers is not supported by Claude",
            ),
            (
                "exa_ai_search_with_function_declarations",
                GenerateContentConfig {
                    tools: vec![Tool {
                        function_declarations: fn_decl(),
                        exa_ai_search: Some(ToolExaAISearch::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                "exa_ai_search",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.exa_ai_search is not supported by Claude",
            ),
            (
                "empty_tool",
                GenerateContentConfig {
                    tools: vec![Tool::default()],
                    ..Default::default()
                },
                "function_declarations",
                "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools entry contains no function_declarations",
            ),
            (
                "include_server_side_tool_invocations",
                GenerateContentConfig {
                    tools: weather_tools(),
                    tool_config: Some(ToolConfig {
                        include_server_side_tool_invocations: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "include_server_side_tool_invocations",
                "anthropicmodel: invalid configuration: tool_config.include_server_side_tool_invocations is not supported",
            ),
            (
                "validated_mode",
                GenerateContentConfig {
                    tools: weather_tools(),
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Validated),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "VALIDATED",
                "anthropicmodel: invalid configuration: unsupported tool_config.function_calling_config.mode \"VALIDATED\"",
            ),
            (
                "any_with_multiple_allowed_names",
                GenerateContentConfig {
                    tools: weather_tools(),
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Any),
                        allowed_function_names: vec![
                            "get_weather".to_string(),
                            "get_time".to_string(),
                        ],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "allowed_function_names",
                "anthropicmodel: invalid configuration: tool_config.function_calling_config.allowed_function_names with multiple names is not supported by Claude",
            ),
            (
                "any_without_tools",
                GenerateContentConfig {
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Any),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "ANY",
                "anthropicmodel: invalid configuration: tool_config.function_calling_config.mode ANY requires at least one declared function tool",
            ),
            (
                "any_with_thinking_enabled",
                GenerateContentConfig {
                    tools: weather_tools(),
                    thinking_config: Some(ThinkingConfig {
                        thinking_budget: Some(budget),
                        ..Default::default()
                    }),
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Any),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "thinking_config",
                "anthropicmodel: invalid configuration: tool_config.function_calling_config.mode ANY cannot be combined with an enabled thinking_config on Claude",
            ),
            (
                "auto_with_allowed_names",
                GenerateContentConfig {
                    tools: weather_tools(),
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Auto),
                        allowed_function_names: vec!["get_weather".to_string()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "allowed_function_names",
                "anthropicmodel: invalid configuration: tool_config.function_calling_config.allowed_function_names is not supported with mode AUTO",
            ),
            (
                "stream_function_call_arguments",
                GenerateContentConfig {
                    tools: weather_tools(),
                    tool_config: fc_tool_config(FunctionCallingConfig {
                        stream_function_call_arguments: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "stream_function_call_arguments",
                "anthropicmodel: invalid configuration: tool_config.function_calling_config.stream_function_call_arguments is not supported",
            ),
        ];

        for (name, cfg, want_sub, want_text) in cases {
            let hits_before = srv.hits();
            let items = generate(&*m, text_request_with("hi", cfg), false).await;
            assert_invalid_config(&items, name, want_sub, want_text);
            // Beyond Go: nothing is sent.
            assert_eq!(srv.hits(), hits_before, "{name}: a request was sent");
        }
    }
}

/// Go `TestExtendedThinkingAndEffort`'s streaming reply, one write-and-`Flush` per event
/// (`event: <name>\ndata: <data>\n\n`).
fn thinking_stream_events() -> Vec<String> {
    [
        ("message_start", r#"{"type":"message_start","message":{"id":"msg_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"usage":{"input_tokens":20,"output_tokens":1}}}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Step 1. "}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Step 2."}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_stream_123"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"enc_blob_456"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_calc","name":"calc","input":{}}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"expr\":"}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"6*7\"}"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":2}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":30,"output_tokens_details":{"thinking_tokens":18}}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .into_iter()
    .map(|(name, data)| sse_event(name, data))
    .collect()
}

#[tokio::test]
async fn test_extended_thinking_and_effort() {
    let srv = FakeServer::start(|req| {
        if req.is_stream() {
            return FakeResponse::sse_chunks(thinking_stream_events());
        }
        FakeResponse::json(
            r#"{"id":"msg_th","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"Deep thought","signature":"sig_abc"},{"type":"redacted_thinking","data":"redacted_xyz"},{"type":"text","text":"42"}],"usage":{"input_tokens":10,"output_tokens":25,"output_tokens_details":{"thinking_tokens":20}}}"#,
        )
    });
    let m = new_model(MODEL, effort_config(&srv.url(), Effort::High)).await;

    // ---- thinking_config_table
    {
        let zero = 0;
        let pos = 512; // intentionally < 1024 to verify we do NOT clamp
        let neg = -1;
        let thinking = |tc: ThinkingConfig| {
            text_request_with(
                "q",
                GenerateContentConfig {
                    thinking_config: Some(tc),
                    ..Default::default()
                },
            )
        };

        // 1. nil ThinkingConfig -> thinking omitted, effort sent.
        generate_ok_named(&*m, text_request("q"), false, "nil ThinkingConfig").await;
        let body = last_body(&srv);
        assert!(
            body.get("thinking").is_none(),
            "expected thinking to be omitted when ThinkingConfig is nil, got {}",
            body["thinking"]
        );
        assert_eq!(
            body["output_config"]["effort"],
            json!("high"),
            "output_config.effort = {}, want \"high\"",
            body["output_config"]["effort"]
        );

        // 2. ThinkingBudget == 0 -> disabled.
        let req = thinking(ThinkingConfig {
            thinking_budget: Some(zero),
            ..Default::default()
        });
        generate_ok_named(&*m, req, false, "thinking_budget=0").await;
        let th = last_body(&srv)["thinking"].clone();
        assert_eq!(
            th["type"],
            json!("disabled"),
            "thinking = {th}, want type=disabled"
        );

        // 3. ThinkingBudget > 0 -> enabled with unclamped budget_tokens.
        let req = thinking(ThinkingConfig {
            thinking_budget: Some(pos),
            include_thoughts: false,
            ..Default::default()
        });
        generate_ok_named(&*m, req, false, "thinking_budget>0").await;
        let th = last_body(&srv)["thinking"].clone();
        assert!(
            th["type"] == json!("enabled") && th["budget_tokens"] == json!(512),
            "thinking = {th}, want type=enabled budget_tokens=512"
        );

        // 4. ThinkingBudget < 0 -> adaptive.
        let req = thinking(ThinkingConfig {
            thinking_budget: Some(neg),
            ..Default::default()
        });
        generate_ok_named(&*m, req, false, "thinking_budget<0").await;
        let th = last_body(&srv)["thinking"].clone();
        assert_eq!(
            th["type"],
            json!("adaptive"),
            "thinking = {th}, want type=adaptive"
        );

        // 5. ThinkingBudget == nil -> error naming ThinkingBudget.
        let req = thinking(ThinkingConfig {
            include_thoughts: true,
            ..Default::default()
        });
        let items = generate(&*m, req, false).await;
        assert_invalid_config(
            &items,
            "nil thinking_budget",
            "thinking_budget",
            "anthropicmodel: invalid configuration: thinking_config.thinking_budget must be set explicitly when thinking_config is set (0 to disable, >0 for manual budget, <0 for adaptive thinking)",
        );

        // 6. ThinkingLevel set -> error naming ThinkingLevel.
        let req = thinking(ThinkingConfig {
            thinking_budget: Some(pos),
            thinking_level: Some(ThinkingLevel::High),
            ..Default::default()
        });
        let items = generate(&*m, req, false).await;
        assert_invalid_config(
            &items,
            "thinking_level",
            "thinking_level",
            "anthropicmodel: invalid configuration: thinking_config.thinking_level is not supported on Claude; configure ClientConfig.effort instead",
        );
    }

    // ---- streaming_thinking_redacted_and_tool_loop_roundtrip
    {
        let mut partial_thoughts: Vec<String> = Vec::new();
        let mut final_resp: Option<LlmResponse> = None;
        for resp in generate_ok(&*m, text_request("Compute 6*7"), true).await {
            if resp.partial {
                for p in resp.content.iter().flat_map(|c| &c.parts) {
                    assert!(
                        p.function_call.is_none(),
                        "partial event must not carry FunctionCall: {p:?}"
                    );
                    if p.thought {
                        partial_thoughts.push(p.text.clone());
                    }
                }
            } else {
                final_resp = Some(resp);
            }
        }
        assert_eq!(
            partial_thoughts.concat(),
            "Step 1. Step 2.",
            "partialThoughts = {:?}, want \"Step 1. Step 2.\"",
            partial_thoughts.concat()
        );
        let final_resp = final_resp.expect("finalResp = <nil>, want 3 parts");
        let parts = final_resp
            .content
            .as_ref()
            .map(|c| c.parts.clone())
            .unwrap_or_default();
        assert_eq!(
            parts.len(),
            3,
            "finalResp parts = {final_resp:?}, want 3 parts (thinking, redacted_thinking, tool_use)"
        );
        assert!(
            parts[0].thought && parts[0].thought_signature.as_ref() == b"sig_stream_123",
            "part[0] = {:?}, want signed thinking part",
            parts[0]
        );
        assert!(
            parts[1].thought
                && parts[1].text.is_empty()
                && parts[1].thought_signature.as_ref() == b"enc_blob_456",
            "part[1] = {:?}, want redacted_thinking part",
            parts[1]
        );
        assert_eq!(
            parts[2]
                .function_call
                .as_ref()
                .and_then(|f| f.args.get("expr")),
            Some(&json!("6*7")),
            "part[2] = {:?}, want FunctionCall expr=6*7",
            parts[2]
        );
        let usage = final_resp.usage_metadata.clone().unwrap_or_default();
        assert!(
            usage.thoughts_token_count == 18 && usage.candidates_token_count == 12,
            "UsageMetadata = {usage:?}, want ThoughtsTokenCount=18 CandidatesTokenCount=12"
        );

        // Replay the turn in history alongside an unsigned thinking block and a foreign
        // signature, plus text placed before thinking in the input Content, to verify ordering
        // and stripping.
        let replayed_assistant = Content::from_parts(
            vec![
                Part::from_text("I will calculate that."),
                Part {
                    thought: true,
                    text: "unsigned thought that must be stripped".to_string(),
                    ..Default::default()
                },
                Part {
                    thought_signature: Bytes::from_static(
                        b"foreign_gemini_sig_without_thought_flag",
                    ),
                    ..Default::default()
                },
                parts[0].clone(), // signed thinking
                parts[1].clone(), // redacted_thinking
                parts[2].clone(), // tool_use
            ],
            ROLE_MODEL,
        );
        let follow_up = LlmRequest {
            contents: vec![
                Content::from_text("Compute 6*7", ROLE_USER),
                replayed_assistant,
                Content::from_parts(
                    vec![Part {
                        function_response: Some(FunctionResponse {
                            id: "toolu_calc".to_string(),
                            name: "calc".to_string(),
                            response: obj(json!({"result": 42})),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ROLE_USER,
                ),
            ],
            ..Default::default()
        };
        generate_ok_named(&*m, follow_up, false, "followUp GenerateContent()").await;

        let body = last_body(&srv);
        let asst_blocks = &body["messages"][1]["content"];
        assert_eq!(
            array_len(asst_blocks),
            4,
            "replayed assistant blocks = {} ({asst_blocks}), want 4 (thinking, redacted_thinking, text, tool_use)",
            array_len(asst_blocks)
        );
        assert!(
            asst_blocks[0]["type"] == json!("thinking")
                && asst_blocks[1]["type"] == json!("redacted_thinking"),
            "thinking blocks were not ordered first on assistant turn: {asst_blocks}"
        );
    }
}

// parity: adapted: ClientConfig.Logger is not ported (P4); the WARN is captured per model call with a tracing LogCapture
#[tokio::test]
async fn test_thinking_and_effort_omit_sampling_params_and_default_max_tokens() {
    let srv = FakeServer::json(
        r#"{"id":"msg_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":5,"output_tokens":1}}"#,
    );

    // ---- default_max_tokens_is_8192_and_all_effort_levels_accepted
    {
        assert_eq!(
            DEFAULT_MAX_TOKENS, 8192,
            "DefaultMaxTokens = {DEFAULT_MAX_TOKENS}, want 8192"
        );
        let efforts = [
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::XHigh,
            Effort::Max,
        ];
        for eff in efforts {
            let m = new_model(MODEL, effort_config(&srv.url(), eff)).await;
            generate_ok_named(
                &*m,
                text_request("hi"),
                false,
                &format!("GenerateContent(Effort={:?})", eff.as_str()),
            )
            .await;
            let body = last_body(&srv);
            assert_eq!(
                body["max_tokens"],
                json!(8192),
                "max_tokens = {}, want 8192",
                body["max_tokens"]
            );
            assert_eq!(
                body["output_config"]["effort"],
                json!(eff.as_str()),
                "output_config.effort = {}, want {:?}",
                body["output_config"]["effort"],
                eff.as_str()
            );
        }
    }

    // ---- sampling_params_omitted_with_warn_when_thinking_or_effort_active
    {
        let budget = 2048;
        let temp = 0.0_f32;
        let top_p = 0.9_f32;
        let top_k = 40.0_f32;
        let thinking = || {
            Some(ThinkingConfig {
                thinking_budget: Some(budget),
                ..Default::default()
            })
        };
        // The one WARN record Go's handler sees (message and attrs, D10).
        let want_warning = format!(
            "omitting sampling parameters when thinking or effort is active backend=anthropic model={MODEL}"
        );
        let assert_one_warning = |logs: &LogCapture, what: &str| {
            let records = logs.events();
            assert!(
                records.len() == 1 && records[0].level == Level::WARN,
                "logged records{what} = {records:?}, want 1 WARN record for omitted sampling params"
            );
            assert_eq!(logs.rendered(), vec![want_warning.clone()], "{what}");
        };
        let assert_omitted = |what: &str| {
            let body = last_body(&srv);
            for key in ["temperature", "top_p", "top_k"] {
                assert!(
                    body.get(key).is_none(),
                    "{key} was sent when {what} is active: {}",
                    body[key]
                );
            }
        };

        // Thinking enabled with Temperature, TopP and TopK.
        let logs = LogCapture::new();
        let m = new_model(MODEL, anthropic_config(&srv.url())).await;
        let req = text_request_with(
            "solve",
            GenerateContentConfig {
                temperature: Some(temp),
                top_p: Some(top_p),
                top_k: Some(top_k),
                thinking_config: thinking(),
                ..Default::default()
            },
        );
        {
            let _guard = logs.set_default();
            generate_ok_named(&*m, req, false, "GenerateContent()").await;
        }
        assert_omitted("thinking");
        assert_one_warning(&logs, "");

        // TopP alone with thinking enabled must also be omitted with a warning.
        let top_p_only_logs = LogCapture::new();
        let m_top_p_thinking = new_model(MODEL, anthropic_config(&srv.url())).await;
        let req = text_request_with(
            "solve with top_p only",
            GenerateContentConfig {
                top_p: Some(top_p),
                thinking_config: thinking(),
                ..Default::default()
            },
        );
        {
            let _guard = top_p_only_logs.set_default();
            generate_ok_named(
                &*m_top_p_thinking,
                req,
                false,
                "GenerateContent(top_p + ThinkingConfig)",
            )
            .await;
        }
        let body = last_body(&srv);
        assert!(
            body.get("top_p").is_none(),
            "top_p was sent when thinking is enabled: {}",
            body["top_p"]
        );
        assert_one_warning(&top_p_only_logs, " for top_p + ThinkingConfig");

        // Effort active without ThinkingConfig must also omit Temperature, TopP, and TopK and
        // log a warning.
        let effort_logs = LogCapture::new();
        let m_effort = new_model(MODEL, effort_config(&srv.url(), Effort::High)).await;
        let req = text_request_with(
            "solve with effort",
            GenerateContentConfig {
                temperature: Some(temp),
                top_p: Some(top_p),
                top_k: Some(top_k),
                ..Default::default()
            },
        );
        {
            let _guard = effort_logs.set_default();
            generate_ok_named(&*m_effort, req, false, "GenerateContent(Effort)").await;
        }
        assert_omitted("effort");
        assert_one_warning(&effort_logs, " for effort");

        // TopP alone with Effort active must also be omitted with a warning.
        let top_p_effort_logs = LogCapture::new();
        let m_top_p_effort = new_model(MODEL, effort_config(&srv.url(), Effort::Medium)).await;
        let req = text_request_with(
            "solve with effort and top_p only",
            GenerateContentConfig {
                top_p: Some(top_p),
                ..Default::default()
            },
        );
        {
            let _guard = top_p_effort_logs.set_default();
            generate_ok_named(
                &*m_top_p_effort,
                req,
                false,
                "GenerateContent(top_p + Effort)",
            )
            .await;
        }
        let body = last_body(&srv);
        assert!(
            body.get("top_p").is_none(),
            "top_p was sent when effort is active: {}",
            body["top_p"]
        );
        assert_one_warning(&top_p_effort_logs, " for top_p + Effort");
    }
}

#[tokio::test]
async fn test_prompt_caching() {
    let srv = FakeServer::json(
        r#"{"id":"msg_c","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"cached reply"}],"usage":{"input_tokens":50,"cache_read_input_tokens":1024,"cache_creation_input_tokens":256,"output_tokens":10}}"#,
    );

    // Go's subtests `ttl_`, `ttl_5m`, `ttl_1h`; Go's empty TTL is `PromptCacheTtl::from_str("")`.
    for ttl in ["", "5m", "1h"] {
        let case = format!("ttl_{ttl}");
        let cfg = ClientConfig {
            prompt_cache: Some(PromptCacheConfig {
                ttl: PromptCacheTtl::from_str(ttl).unwrap(),
            }),
            ..anthropic_config(&srv.url())
        };
        let m = new_model(MODEL, cfg).await;

        let req = LlmRequest {
            contents: vec![
                Content::from_text("First user message", ROLE_USER),
                Content::from_parts(
                    vec![
                        Part::from_text("Assistant text before thinking"),
                        Part {
                            thought: true,
                            text: "Trailing thinking".to_string(),
                            thought_signature: Bytes::from_static(b"sig_1"),
                            ..Default::default()
                        },
                    ],
                    ROLE_MODEL,
                ),
            ],
            config: Some(GenerateContentConfig {
                system_instruction: Some(Content::from_text("System prompt", ROLE_USER)),
                tools: vec![Tool {
                    function_declarations: vec![
                        FunctionDeclaration {
                            name: "tool_a".to_string(),
                            ..Default::default()
                        },
                        FunctionDeclaration {
                            name: "tool_b".to_string(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };

        let got = generate_last(&*m, req, false).await;
        let body = last_body(&srv);

        let want_ttl = if ttl == "1h" { "1h" } else { "5m" };

        // 1. Breakpoint on last tool only.
        let tools = &body["tools"];
        assert!(
            tools[0].get("cache_control").is_none(),
            "{case}: first tool should not have cache_control"
        );
        let last_tool_cc = &tools[1]["cache_control"];
        assert!(
            last_tool_cc["type"] == json!("ephemeral") && last_tool_cc["ttl"] == json!(want_ttl),
            "{case}: last tool cache_control = {last_tool_cc}, want ephemeral/{want_ttl}"
        );

        // 2. Breakpoint on system block.
        let sys_cc = &body["system"][0]["cache_control"];
        assert!(
            sys_cc["type"] == json!("ephemeral") && sys_cc["ttl"] == json!(want_ttl),
            "{case}: system cache_control = {sys_cc}, want ephemeral/{want_ttl}"
        );

        // 3. Breakpoint on last non-thinking block in messages (assistant text block, skipping
        // thinking block). asstBlocks[0] is thinking, asstBlocks[1] is text.
        let asst_blocks = &body["messages"][1]["content"];
        assert!(
            asst_blocks[0].get("cache_control").is_none(),
            "{case}: thinking block must never carry cache_control"
        );
        let text_cc = &asst_blocks[1]["cache_control"];
        assert!(
            text_cc["type"] == json!("ephemeral") && text_cc["ttl"] == json!(want_ttl),
            "{case}: last non-thinking message block cache_control = {text_cc}, want ephemeral/{want_ttl}"
        );

        // 4. Verify UsageMetadata and CustomMetadata.
        let want_usage = GenerateContentResponseUsageMetadata {
            prompt_token_count: 50 + 1024 + 256,
            candidates_token_count: 10,
            total_token_count: 50 + 1024 + 256 + 10,
            cached_content_token_count: 1024,
            thoughts_token_count: 0,
            ..Default::default()
        };
        pretty_assertions::assert_eq!(
            Some(want_usage),
            got.usage_metadata,
            "{case}: UsageMetadata diff (-want +got)"
        );
        assert_eq!(
            got.custom_metadata
                .get(CACHE_CREATION_INPUT_TOKENS_METADATA_KEY),
            Some(&json!(256)),
            "{case}: CustomMetadata[{CACHE_CREATION_INPUT_TOKENS_METADATA_KEY:?}] = {:?}, want 256",
            got.custom_metadata
                .get(CACHE_CREATION_INPUT_TOKENS_METADATA_KEY)
        );
    }
}
