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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go tool/exampletool/tool_test.go (package exampletool, white-box).

use std::sync::Arc;

use gadk_agent::{Context, Tool};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmRequest;
use serde_json::{Map, Value, json};

use crate::example::{Example, ExampleToolConfig, new};

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not a JSON object: {other}"),
    }
}

/// The `{Input: "hi", Output: ["hello"]}` example shared by the four no-op cases.
fn hi_hello() -> Vec<Example> {
    vec![Example {
        input: Content::from_text("hi", "user"),
        output: vec![Content::from_text("hello", "model")],
    }]
}

/// The FunctionCallExample_* example: a model function call, then a user function response.
fn function_call_example() -> Vec<Example> {
    vec![Example {
        input: Content::from_text("call func", "user"),
        output: vec![
            Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        name: "my_tool".into(),
                        args: obj(json!({"arg1": "val1"})),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            },
            Content {
                // Function response is usually from user/tool role
                role: "user".into(),
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        name: "my_tool".into(),
                        response: obj(json!({"result": "ok"})),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            },
        ],
    }]
}

struct Case {
    name: &'static str,
    examples: Vec<Example>,
    user_content: Option<Content>,
    model: &'static str,
    want_instruct: &'static str,
}

// parity: adapted: Go's mockToolContext becomes a built Context carrying only the user content; NilFirstPart uses an empty Part because a Rust part list cannot hold nil; New's never-returned error is dropped
#[tokio::test]
async fn test_example_tool_process_request() {
    let cases = vec![
        Case {
            name: "NoUserContent",
            examples: hi_hello(),
            user_content: Some(Content {
                parts: vec![],
                role: String::new(),
            }),
            model: "gemini-1.5-pro",
            want_instruct: "",
        },
        Case {
            name: "NilUserContent",
            examples: hi_hello(),
            user_content: None,
            model: "gemini-1.5-pro",
            want_instruct: "",
        },
        Case {
            name: "NilFirstPart",
            examples: hi_hello(),
            user_content: Some(Content {
                parts: vec![Part::default()],
                role: String::new(),
            }),
            model: "gemini-1.5-pro",
            want_instruct: "",
        },
        Case {
            name: "EmptyUserContentString",
            examples: hi_hello(),
            user_content: Some(Content {
                parts: vec![Part::from_text("")],
                role: String::new(),
            }),
            model: "gemini-1.5-pro",
            want_instruct: "",
        },
        Case {
            name: "SimpleExample",
            examples: vec![Example {
                input: Content::from_text("input1", "user"),
                output: vec![Content::from_text("output1", "model")],
            }],
            user_content: Some(Content::from_text("user query", "user")),
            model: "gemini-1.5-pro",
            want_instruct: concat!(
                "<EXAMPLES>\n",
                "Begin few-shot\n",
                "The following are examples of user queries and model responses using the available tools.\n",
                "\n",
                "EXAMPLE 1:\n",
                "Begin example\n",
                "[user]\n",
                "input1\n",
                "[model]\n",
                "output1\n",
                "End example\n",
                "\n",
                "End few-shot\n",
                "<EXAMPLES>",
            ),
        },
        Case {
            name: "MultipleExamples",
            examples: vec![
                Example {
                    input: Content::from_text("in1", "user"),
                    output: vec![Content::from_text("out1", "model")],
                },
                Example {
                    input: Content::from_text("in2", "user"),
                    output: vec![Content::from_text("out2", "model")],
                },
            ],
            user_content: Some(Content::from_text("query", "user")),
            model: "gemini-1.5-pro",
            want_instruct: concat!(
                "<EXAMPLES>\n",
                "Begin few-shot\n",
                "The following are examples of user queries and model responses using the available tools.\n",
                "\n",
                "EXAMPLE 1:\n",
                "Begin example\n",
                "[user]\n",
                "in1\n",
                "[model]\n",
                "out1\n",
                "End example\n",
                "\n",
                "EXAMPLE 2:\n",
                "Begin example\n",
                "[user]\n",
                "in2\n",
                "[model]\n",
                "out2\n",
                "End example\n",
                "\n",
                "End few-shot\n",
                "<EXAMPLES>",
            ),
        },
        Case {
            name: "FunctionCallExample_Gemini1.5",
            examples: function_call_example(),
            user_content: Some(Content::from_text("query", "user")),
            model: "gemini-1.5-pro",
            want_instruct: concat!(
                "<EXAMPLES>\n",
                "Begin few-shot\n",
                "The following are examples of user queries and model responses using the available tools.\n",
                "\n",
                "EXAMPLE 1:\n",
                "Begin example\n",
                "[user]\n",
                "call func\n",
                "[model]\n",
                "```\n",
                "my_tool(arg1='val1')\n",
                "```\n",
                "[user]\n",
                "```\n",
                "&{<nil>  []  my_tool map[result:ok]}\n",
                "```\n",
                "End example\n",
                "\n",
                "End few-shot\n",
                "<EXAMPLES>",
            ),
        },
        Case {
            name: "FunctionCallExample_Gemini2",
            examples: function_call_example(),
            user_content: Some(Content::from_text("query", "user")),
            model: "gemini-2.0-flash",
            want_instruct: concat!(
                "<EXAMPLES>\n",
                "Begin few-shot\n",
                "The following are examples of user queries and model responses using the available tools.\n",
                "\n",
                "EXAMPLE 1:\n",
                "Begin example\n",
                "[user]\n",
                "call func\n",
                "[model]\n",
                "```tool_code\n",
                "my_tool(arg1='val1')\n",
                "```\n",
                "[user]\n",
                "```tool_outputs\n",
                "&{<nil>  []  my_tool map[result:ok]}\n",
                "```\n",
                "End example\n",
                "\n",
                "End few-shot\n",
                "<EXAMPLES>",
            ),
        },
    ];

    for tc in cases {
        let name = tc.name;
        let et = new(ExampleToolConfig {
            examples: tc.examples,
        });
        let mut req = LlmRequest {
            model: tc.model.into(),
            ..Default::default()
        };
        let mut builder = Context::builder();
        if let Some(uc) = tc.user_content {
            builder = builder.user_content(uc);
        }
        let ctx = builder.build();

        if let Err(e) = et.process_request(&ctx, &mut req).await {
            panic!("{name}: ProcessRequest() error = {e}");
        }

        let si = req
            .config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref());
        if tc.want_instruct.is_empty() {
            assert!(
                si.is_none(),
                "{name}: ProcessRequest() unexpected system instruction: got {si:?}, want nil/empty"
            );
            continue;
        }
        let si = si.unwrap_or_else(|| {
            panic!("{name}: ProcessRequest() expected system instruction, got nil")
        });
        assert_eq!(
            si.parts[0].text, tc.want_instruct,
            "{name}: System instruction mismatch"
        );
    }
}

#[test]
fn test_example_tool_interface() {
    let et: Arc<dyn Tool> = new(ExampleToolConfig::default());
    assert_eq!(et.name(), "example_tool", "Name()");
    assert!(!et.is_long_running(), "IsLongRunning() = true, want false");
}
