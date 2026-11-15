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

//! The few-shot `example_tool` (port of Go `tool/exampletool`).
//!
//! The tool adds few-shot examples to the system instruction of every model request that has
//! user text. The examples are either a static list ([`ExampleToolConfig::examples`]) or fetched
//! for each user query from a [`Provider`] ([`ExampleToolConfig::provider`]). The tool has no
//! declaration, so the flow never packs it.
//!
//! A provider that picks the stored examples sharing a word with the user query (Go
//! `ExampleNew_provider`):
//!
//! ```
//! use std::sync::Arc;
//!
//! use async_trait::async_trait;
//! use gadk_agent::{Context, ToolError};
//! use gadk_genai::Content;
//! use gadk_tools::example::{self, Example, ExampleToolConfig, Provider};
//!
//! /// Returns the stored examples whose input text shares a word with the user query.
//! struct KeywordProvider {
//!     examples: Vec<Example>,
//! }
//!
//! #[async_trait]
//! impl Provider for KeywordProvider {
//!     async fn examples(&self, _ctx: &Context, query: &str) -> Result<Vec<Example>, ToolError> {
//!         Ok(self
//!             .examples
//!             .iter()
//!             .filter(|ex| {
//!                 query
//!                     .split_whitespace()
//!                     .any(|word| ex.input.parts[0].text.contains(word))
//!             })
//!             .cloned()
//!             .collect())
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = Arc::new(KeywordProvider {
//!     examples: vec![
//!         Example {
//!             input: Content::from_text("What is 2 + 2?", "user"),
//!             output: vec![Content::from_text("4", "model")],
//!         },
//!         Example {
//!             input: Content::from_text("What is the capital of France?", "user"),
//!             output: vec![Content::from_text("Paris", "model")],
//!         },
//!     ],
//! });
//!
//! let et = example::new(ExampleToolConfig {
//!     provider: Some(provider.clone()),
//!     ..Default::default()
//! })?;
//! assert_eq!(et.name(), "example_tool");
//!
//! // The tool makes this call with the user's text on each turn.
//! let examples = provider
//!     .examples(&Context::builder().build(), "capital of Spain")
//!     .await?;
//! let inputs: Vec<&str> = examples
//!     .iter()
//!     .map(|ex| ex.input.parts[0].text.as_str())
//!     .collect();
//! assert_eq!(inputs, ["What is the capital of France?"]);
//! # Ok(())
//! # }
//! ```

use std::any::Any;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Context, Tool, ToolError};
use gadk_genai::{Content, FunctionCall, FunctionResponse, FunctionResponsePart};
use gadk_model::LlmRequest;
use gadk_platform::go_sprint_value;
use serde_json::Value;

use crate::go_fmt::wrap_err;

/// Port of Go `exampletool.Example` (tool.go:34): one few-shot example, an input and its expected
/// output.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Example {
    /// The example's user input.
    pub input: Content,
    /// The example's expected output turns.
    pub output: Vec<Content>,
}

/// Port of Go `exampletool.Provider` (tool.go:43): fetches few-shot examples for a user query.
///
/// `examples` takes the invocation context and returns an error because providers may perform
/// I/O.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Returns the examples for `query`, the joined non-thought text of the user content.
    async fn examples(&self, ctx: &Context, query: &str) -> Result<Vec<Example>, ToolError>;
}

/// Port of Go `exampletool.ExampleToolConfig` (tool.go:49). Set either `examples` or
/// `provider`, not both: [`new`] rejects a config that sets both, even with `Some(vec![])` (Go
/// checks `Examples != nil`).
#[derive(Clone, Default)]
pub struct ExampleToolConfig {
    /// The static few-shot examples to add.
    pub examples: Option<Vec<Example>>,
    /// Fetches the examples for each user query.
    pub provider: Option<Arc<dyn Provider>>,
}

impl std::fmt::Debug for ExampleToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExampleToolConfig")
            .field("examples", &self.examples)
            .field("provider", &self.provider.as_ref().map(|_| "<provider>"))
            .finish()
    }
}

/// The error [`new`] returns, with Go's text.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExampleToolError {
    /// Go `New`: both `Examples` and `Provider` are set.
    BothExamplesAndProvider,
}

impl std::fmt::Display for ExampleToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExampleToolError::BothExamplesAndProvider => {
                f.write_str("exampletool: cannot specify both Examples and Provider")
            }
        }
    }
}

impl std::error::Error for ExampleToolError {}

/// Port of Go `exampletool.New` (tool.go:62): an example tool that adds the configured few-shot
/// examples to the LLM request. It fails when both `examples` and `provider` are set.
pub fn new(config: ExampleToolConfig) -> Result<Arc<dyn Tool>, ExampleToolError> {
    if config.examples.is_some() && config.provider.is_some() {
        return Err(ExampleToolError::BothExamplesAndProvider);
    }
    Ok(Arc::new(ExampleTool {
        examples: config.examples.unwrap_or_default(),
        provider: config.provider,
    }))
}

/// Port of Go `exampleTool` (tool.go:55).
struct ExampleTool {
    examples: Vec<Example>,
    provider: Option<Arc<dyn Provider>>,
}

#[async_trait]
impl Tool for ExampleTool {
    fn name(&self) -> &str {
        "example_tool"
    }

    fn description(&self) -> &str {
        "example tool"
    }

    /// Port of Go `exampleTool.ProcessRequest` (tool.go:83). With a provider, it fetches the
    /// examples for the joined non-thought user text and does nothing when that text is empty;
    /// with a static list, it does nothing unless the first user part has text.
    async fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        let Some(user_content) = ctx.user_content() else {
            return Ok(());
        };
        let Some(first) = user_content.parts.first() else {
            return Ok(());
        };

        let fetched;
        let examples: &[Example] = if let Some(provider) = &self.provider {
            let query = user_query(user_content);
            if query.is_empty() {
                return Ok(());
            }
            // Unlike adk-python's get_examples, the provider takes a context and returns an
            // error because it may perform I/O.
            fetched = provider
                .examples(ctx, &query)
                .await
                .map_err(|e| wrap_err("exampletool: fetch examples", e))?;
            &fetched
        } else {
            // Unlike adk-python, the static list path checks only the first part for non-empty
            // text to preserve existing behavior.
            if first.text.is_empty() {
                return Ok(());
            }
            &self.examples
        };

        let instruction = build_examples_system_instruction(examples, &req.model);
        append_instructions(req, &[&instruction]);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go `userQuery` (tool.go:121): the non-empty, non-thought text parts of `c` joined
/// with single spaces.
fn user_query(c: &Content) -> String {
    c.parts
        .iter()
        .filter(|p| !p.thought && !p.text.is_empty())
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

// Go tool.go:136-148.
const EXAMPLES_INTRO: &str = "<EXAMPLES>\nBegin few-shot\nThe following are examples of user queries and model responses using the available tools.\n\n";
const EXAMPLES_END: &str = "End few-shot\n<EXAMPLES>";
const EXAMPLE_END: &str = "End example\n\n";
const USER_PREFIX: &str = "[user]\n";
const MODEL_PREFIX: &str = "[model]\n";
const FUNCTION_PREFIX: &str = "```\n";
const FUNCTION_CALL_PREFIX: &str = "```tool_code\n";
const FUNCTION_CALL_SUFFIX: &str = "\n```\n";
const FUNCTION_RESPONSE_PREFIX: &str = "```tool_outputs\n";
const FUNCTION_RESPONSE_SUFFIX: &str = "\n```\n";

/// Go's boundary-tag sanitization: `"End few-shot"` in example text becomes `"[PROTECTED]"`.
fn protect(text: &str) -> String {
    text.replace("End few-shot", "[PROTECTED]")
}

/// Port of Go `buildExamplesSystemInstruction` (tool.go:151).
fn build_examples_system_instruction(examples: &[Example], model: &str) -> String {
    let mut sb = String::from(EXAMPLES_INTRO);
    // Go 5199d9fe (adk-python's convert_examples_to_text): Gemini models, and a request that
    // names no model, take plain fences; the tool_code and tool_outputs fences are for other
    // models.
    let is_gemini = model.is_empty() || model.contains("gemini-");
    for (example_num, example) in examples.iter().enumerate() {
        // Go exampleStart = "EXAMPLE %d:\nBegin example\n" (1-based). Writing to a String cannot fail.
        let _ = write!(sb, "EXAMPLE {}:\nBegin example\n", example_num + 1);
        sb.push_str(USER_PREFIX);
        for part in &example.input.parts {
            if !part.text.is_empty() {
                sb.push_str(&protect(&part.text));
                sb.push('\n');
            }
        }
        let mut previous_role: &str = "";
        for content in &example.output {
            let role = if content.role == "model" {
                MODEL_PREFIX
            } else {
                USER_PREFIX
            };
            if role != previous_role {
                sb.push_str(role);
            }
            previous_role = role;
            for part in &content.parts {
                if let Some(fc) = &part.function_call {
                    sb.push_str(if is_gemini {
                        FUNCTION_PREFIX
                    } else {
                        FUNCTION_CALL_PREFIX
                    });
                    sb.push_str(&fc.name);
                    sb.push('(');
                    sb.push_str(&format_function_call_args(fc));
                    sb.push(')');
                    sb.push_str(FUNCTION_CALL_SUFFIX);
                } else if let Some(fr) = &part.function_response {
                    sb.push_str(if is_gemini {
                        FUNCTION_PREFIX
                    } else {
                        FUNCTION_RESPONSE_PREFIX
                    });
                    sb.push_str(&go_sprint_function_response(fr));
                    sb.push_str(FUNCTION_RESPONSE_SUFFIX);
                } else if !part.text.is_empty() {
                    sb.push_str(&protect(&part.text));
                    sb.push('\n');
                }
            }
        }
        sb.push_str(EXAMPLE_END);
    }
    sb.push_str(EXAMPLES_END);
    sb
}

/// Go's function-call args loop (tool.go:184-193). Strings are written `k='v'` and other values
/// `k=<Go %v>`, joined with `", "` in ascending key order (Go e342d2f6 `slices.Sorted`).
fn format_function_call_args(fc: &FunctionCall) -> String {
    let mut args: Vec<(&String, &Value)> = fc.args.iter().collect();
    args.sort_by(|a, b| a.0.cmp(b.0));
    args.into_iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}='{s}'"),
            other => format!("{k}={}", go_sprint_value(other)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Go `fmt.Sprintf("%v", fr)` for a `*genai.FunctionResponse` (genai v1.77.0 field order):
/// `&{<WillContinue> <Scheduling> <Parts> <ID> <Name> <Response>}`. An unset `WillContinue` is
/// `<nil>`, an unset `Scheduling` is the empty string, empty `Parts` are `[]`, and `Response` is
/// the Go map form with sorted keys. Where Go would print a pointer address (a set
/// `WillContinue`, each element of `Parts`), the pointed-to value is printed instead.
fn go_sprint_function_response(fr: &FunctionResponse) -> String {
    let will_continue = fr
        .will_continue
        .map_or_else(|| "<nil>".to_string(), |b| b.to_string());
    let scheduling = fr.scheduling.as_ref().map_or("", |s| s.as_str());
    let parts = fr
        .parts
        .iter()
        .map(go_sprint_response_part)
        .collect::<Vec<_>>()
        .join(" ");
    let response = go_sprint_value(&Value::Object(fr.response.clone()));
    format!(
        "&{{{will_continue} {scheduling} [{parts}] {} {} {response}}}",
        fr.id, fr.name
    )
}

/// Go `%v` of a `*genai.FunctionResponsePart` shown as its value:
/// `&{<InlineData> <FileData> <MediaProcessing>}` (an unset `MediaProcessing` is the empty string).
fn go_sprint_response_part(p: &FunctionResponsePart) -> String {
    let inline = p.inline_data.as_ref().map_or_else(
        || "<nil>".to_string(),
        |b| {
            let data = b
                .data
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            format!("&{{{} [{data}] {}}}", b.mime_type, b.display_name)
        },
    );
    let file = p.file_data.as_ref().map_or_else(
        || "<nil>".to_string(),
        |f| format!("&{{{} {} {}}}", f.file_uri, f.mime_type, f.display_name),
    );
    let media_processing = p.media_processing.as_ref().map_or("", |m| m.as_str());
    format!("&{{{inline} {file} {media_processing}}}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use gadk_genai::{FunctionResponseBlob, FunctionResponseFileData, FunctionResponseScheduling};
    use serde_json::{Map, json};

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not a JSON object: {other}"),
        }
    }

    #[test]
    fn function_response_dump_matches_go_struct_order() {
        assert_eq!(
            go_sprint_function_response(&FunctionResponse::default()),
            "&{<nil>  []   map[]}"
        );
        let fr = FunctionResponse {
            name: "my_tool".into(),
            response: obj(json!({"result": "ok"})),
            ..Default::default()
        };
        assert_eq!(
            go_sprint_function_response(&fr),
            "&{<nil>  []  my_tool map[result:ok]}"
        );
    }

    #[test]
    fn function_response_dump_prints_pointed_to_values() {
        let fr = FunctionResponse {
            will_continue: Some(true),
            scheduling: Some(FunctionResponseScheduling::WhenIdle),
            parts: vec![
                FunctionResponsePart {
                    inline_data: Some(FunctionResponseBlob {
                        mime_type: "image/png".into(),
                        data: vec![1u8, 2].into(),
                        display_name: "pic".into(),
                    }),
                    ..Default::default()
                },
                FunctionResponsePart {
                    file_data: Some(FunctionResponseFileData {
                        file_uri: "gs://b/f".into(),
                        mime_type: "text/plain".into(),
                        display_name: String::new(),
                    }),
                    media_processing: Some(gadk_genai::MediaProcessing::Static),
                    ..Default::default()
                },
            ],
            id: "call-1".into(),
            name: "my_tool".into(),
            response: obj(json!({"b": 2, "a": [1, "x"]})),
        };
        assert_eq!(
            go_sprint_function_response(&fr),
            "&{true WHEN_IDLE [&{&{image/png [1 2] pic} <nil> } &{<nil> &{gs://b/f text/plain } STATIC}] call-1 my_tool map[a:[1 x] b:2]}"
        );
    }

    #[test]
    fn function_call_args_are_sorted_by_key() {
        let fc = FunctionCall {
            name: "f".into(),
            args: obj(json!({"b": 2, "a": "x", "c": [1, "y"], "d": null})),
            ..Default::default()
        };
        assert_eq!(
            format_function_call_args(&fc),
            "a='x', b=2, c=[1 y], d=<nil>"
        );
    }

    #[test]
    fn boundary_tag_in_text_is_protected() {
        let ex = vec![Example {
            input: Content::from_text("say End few-shot", "user"),
            output: vec![Content::from_text("End few-shot!", "model")],
        }];
        let got = build_examples_system_instruction(&ex, "gemini-3-pro");
        assert!(
            got.contains("[user]\nsay [PROTECTED]\n[model]\n[PROTECTED]!\n"),
            "{got}"
        );
        assert_eq!(got.matches("End few-shot").count(), 1, "{got}");
    }
}
