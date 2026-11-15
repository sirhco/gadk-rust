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

//! Port of ADK-Go `model/openaimodel/internal/responses/diagnostics_test.go`.
//!
//! Go records the span event `openai.terminal_response_conversion_failed` (attribute
//! `error.type`) on the active OpenTelemetry span when a stream's terminal conversion fails and
//! the streamed turn is returned instead (450b4a54, #2056). The gadk counterpart is a `tracing`
//! event on the current span (the OpenTelemetry layer turns one into a span event): its name is
//! the event's `message` (else its metadata name) and its attributes are its other fields. The
//! capturing layer below reads both, so either spelling of the event satisfies the test.
//!
//! Go's recording / not-recording sampler pair becomes a subscriber installed / not installed:
//! with no subscriber the stream must still return the same turn.

use std::sync::Arc;

use futures::StreamExt;
use gadk_genai::{Content, FinishReason, FunctionCall, Part, ROLE_MODEL};
use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::model_openaimodel_internal_responses_model::{
    EV_COMPLETED, EV_DELTA1, EV_FAILED, EV_MAX_TOKENS,
};
use super::support::{
    TEST_MODEL, TestClientConfig, new_loopback_server, new_test_model, serve_events, user_request,
};
use crate::error::{ErrorKind, OpenAiError};

const EVENT_NAME: &str = "openai.terminal_response_conversion_failed";

/// One captured event: its name and its attributes, in the order recorded.
type CapturedEvent = (String, Vec<(String, String)>);

/// Records every event emitted while it is the subscriber's layer.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<CapturedEvent>>>);

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl FieldVisitor {
    fn record(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }
}

impl<S: tracing::Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let name = visitor
            .message
            .unwrap_or_else(|| event.metadata().name().to_string());
        self.0.lock().push((name, visitor.fields));
    }
}

/// The result of one streaming run.
struct Run {
    err: Option<OpenAiError>,
    finals: Vec<gadk_model::LlmResponse>,
    partials: usize,
}

/// Streams `events` (then `[DONE]`) through a Responses model inside a `generate_content` span.
/// `stop_early` makes the consumer stop after the first response, as Go's `break` does.
async fn run(events: &[&str], stop_early: bool) -> Run {
    let resp = serve_events(events);
    let server = new_loopback_server(move |_| resp.clone());
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(&server));
    let span = tracing::info_span!("generate_content");
    let mut out = Run {
        err: None,
        finals: Vec::new(),
        partials: 0,
    };
    let mut items = llm.generate_content(user_request("private-prompt"), true);
    let _enter = span.enter();
    while let Some(item) = items.next().await {
        match item {
            Err(e) => {
                out.err = Some(super::support::openai_error(e));
                break;
            }
            Ok(r) if r.partial => out.partials += 1,
            Ok(r) => out.finals.push(r),
        }
        if stop_early {
            break;
        }
    }
    out
}

#[ignore = "8a bug: responses stream finalization recovers from a failed terminal conversion silently; Go records the span event openai.terminal_response_conversion_failed {error.type} (gadk-openai has no tracing; every other assertion of the table passes) (450b4a54 #2056)"]
#[tokio::test]
async fn test_model_generate_stream_terminal_conversion_diagnostic() {
    const MESSAGE: &str = r#"{"type":"message","content":[{"type":"output_text","text":"hello"}]}"#;
    const UNSUPPORTED: &str = r#"{"type":"private-output-type"}"#;
    const UNSUPPORTED_CONTENT: &str =
        r#"{"type":"message","content":[{"type":"private-content-type"}]}"#;
    const ADDED: &str = r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"private-call-id","name":"private-tool-name"}}"#;
    const ARGS_DONE: &str = r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"value\":\"private-argument\"}"}"#;
    const BAD_CALL: &str = r#"{"type":"function_call","name":"private-tool-name","call_id":"private-call-id","arguments":"private-invalid-arguments"}"#;
    let completed = |output: &str| {
        format!(
            r#"{{"type":"response.completed","response":{{"id":"private-response-id","model":"private-model-name","status":"completed","output":{output}}}}}"#
        )
    };
    let two = |a: &str, b: &str| format!("[{a},{b}]");

    struct Case {
        name: &'static str,
        events: Vec<String>,
        want_type: &'static str,
        want_err: Option<ErrorKind>,
        want_text: &'static str,
        want_call: bool,
        want_finish: Option<FinishReason>,
        stop_early: bool,
    }
    let case = |name: &'static str, events: Vec<String>| Case {
        name,
        events,
        want_type: "",
        want_err: None,
        want_text: "",
        want_call: false,
        want_finish: None,
        stop_early: false,
    };
    let s = |v: &str| v.to_string();
    let tests = vec![
        Case {
            want_type: "unsupported_output_item",
            want_text: "hel",
            ..case(
                "unsupported output retains streamed text",
                vec![s(EV_DELTA1), completed(&two(MESSAGE, UNSUPPORTED))],
            )
        },
        Case {
            want_type: "unsupported_output_item",
            want_text: "hel",
            ..case(
                "server tool output prevents terminal conversion",
                vec![
                    s(EV_DELTA1),
                    completed(&two(
                        MESSAGE,
                        r#"{"type":"web_search_call","id":"search_1","status":"completed"}"#,
                    )),
                ],
            )
        },
        Case {
            want_type: "unsupported_message_content",
            want_text: "hel",
            ..case(
                "unsupported message content retains streamed text",
                vec![s(EV_DELTA1), completed(&two(MESSAGE, UNSUPPORTED_CONTENT))],
            )
        },
        Case {
            want_type: "invalid_function_call_arguments",
            want_text: "hel",
            want_call: true,
            ..case(
                "invalid arguments recover the streamed call",
                vec![
                    s(EV_DELTA1),
                    s(ADDED),
                    s(ARGS_DONE),
                    completed(&two(MESSAGE, BAD_CALL)),
                ],
            )
        },
        Case {
            want_type: "unsupported_output_item",
            want_text: "hel",
            ..case(
                "later terminal does not duplicate diagnostic",
                vec![
                    s(EV_DELTA1),
                    completed(&format!("[{UNSUPPORTED}]")),
                    s(EV_COMPLETED),
                ],
            )
        },
        Case {
            want_text: "hello",
            ..case(
                "successful terminal conversion",
                vec![s(EV_DELTA1), s(EV_COMPLETED)],
            )
        },
        Case {
            want_text: "hel",
            ..case(
                "terminal omits streamed text",
                vec![
                    s(EV_DELTA1),
                    completed(
                        r#"[{"type":"message","content":[{"type":"output_text","text":"unrelated"}]}]"#,
                    ),
                ],
            )
        },
        Case {
            want_text: "hel",
            ..case("empty output", vec![s(EV_DELTA1), completed("[]")])
        },
        Case {
            want_text: "hel",
            ..case(
                "empty message",
                vec![
                    s(EV_DELTA1),
                    completed(r#"[{"type":"message","content":[]}]"#),
                ],
            )
        },
        Case {
            want_err: Some(ErrorKind::UnsupportedOutputItemType),
            ..case(
                "no aggregate returns conversion error",
                vec![completed(&format!("[{UNSUPPORTED}]"))],
            )
        },
        Case {
            want_err: Some(ErrorKind::FunctionCallArgs),
            ..case(
                "invalid arguments without a streamed call return an error",
                vec![s(EV_DELTA1), completed(&format!("[{BAD_CALL}]"))],
            )
        },
        Case {
            want_err: Some(ErrorKind::FunctionCallArgs),
            ..case(
                "later call failure suppresses recovery diagnostic",
                vec![s(EV_DELTA1), completed(&two(UNSUPPORTED, BAD_CALL))],
            )
        },
        Case {
            want_err: Some(ErrorKind::ResponseFailed),
            ..case(
                "server failure is returned",
                vec![s(EV_DELTA1), s(EV_FAILED)],
            )
        },
        Case {
            want_text: "hel",
            want_finish: Some(FinishReason::MaxTokens),
            ..case(
                "incomplete terminal does not attempt recovery",
                vec![
                    s(EV_DELTA1),
                    s(EV_MAX_TOKENS),
                    completed(&format!("[{UNSUPPORTED}]")),
                ],
            )
        },
        Case {
            want_text: "hel",
            want_call: true,
            want_finish: Some(FinishReason::MaxTokens),
            ..case(
                "incomplete terminal recovers invalid arguments without diagnostic",
                vec![
                    s(EV_DELTA1),
                    s(ADDED),
                    s(ARGS_DONE),
                    format!(
                        r#"{{"type":"response.incomplete","response":{{"status":"incomplete","incomplete_details":{{"reason":"max_output_tokens"}},"output":[{BAD_CALL}]}}}}"#
                    ),
                ],
            )
        },
        Case {
            stop_early: true,
            ..case(
                "consumer stops before terminal",
                vec![s(EV_DELTA1), completed(&format!("[{UNSUPPORTED}]"))],
            )
        },
    ];

    for tc in tests {
        for recording in [true, false] {
            let label = format!(
                "{}/{}",
                tc.name,
                if recording {
                    "recording"
                } else {
                    "not recording"
                }
            );
            let capture = Capture::default();
            let events: Vec<&str> =
                std::iter::once(super::model_openaimodel_internal_responses_model::EV_CREATED)
                    .chain(tc.events.iter().map(String::as_str))
                    .collect();
            let got = if recording {
                let subscriber = Registry::default().with(capture.clone());
                let _guard = tracing::subscriber::set_default(subscriber);
                run(&events, tc.stop_early).await
            } else {
                run(&events, tc.stop_early).await
            };

            assert_eq!(
                got.err
                    .as_ref()
                    .map(|e| tc.want_err.is_some_and(|k| e.is(k))),
                tc.want_err.map(|_| true),
                "{label}: stream error does not match expected sentinel (got {:?})",
                got.err
            );
            if tc.stop_early || tc.want_err.is_some() {
                assert!(
                    got.finals.is_empty(),
                    "{label}: stream emitted final responses, want none"
                );
            } else {
                assert!(
                    got.finals.len() == 1 && got.partials > 0,
                    "{label}: stream emitted {} finals and {} partials, want one final after partial content",
                    got.finals.len(),
                    got.partials
                );
                let final_resp = &got.finals[0];
                let mut want_parts = vec![Part {
                    text: tc.want_text.to_string(),
                    ..Part::default()
                }];
                if tc.want_call {
                    want_parts.push(Part {
                        function_call: Some(FunctionCall {
                            name: "private-tool-name".to_string(),
                            id: "private-call-id".to_string(),
                            args: serde_json::json!({"value": "private-argument"})
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                            ..FunctionCall::default()
                        }),
                        ..Part::default()
                    });
                }
                assert_eq!(
                    final_resp.content,
                    Some(Content {
                        role: ROLE_MODEL.to_string(),
                        parts: want_parts
                    }),
                    "{label}: final content changed during fallback"
                );
                let finish = tc.want_finish.clone().unwrap_or(FinishReason::Stop);
                assert!(
                    final_resp.finish_reason == Some(finish)
                        && final_resp.turn_complete
                        && final_resp.error_code.is_empty()
                        && final_resp.error_message.is_empty(),
                    "{label}: fallback changed the turn completion or error fields"
                );
            }
            if !recording {
                continue;
            }
            let events = capture.0.lock();
            let diagnostics: Vec<&CapturedEvent> = events
                .iter()
                .filter(|(name, _)| name == EVENT_NAME)
                .collect();
            if tc.want_type.is_empty() {
                assert!(
                    diagnostics.is_empty(),
                    "{label}: recorded {} diagnostic events, want none",
                    diagnostics.len()
                );
                continue;
            }
            assert_eq!(
                diagnostics.len(),
                1,
                "{label}: recorded {} diagnostic events, want 1",
                diagnostics.len()
            );
            // Exact attributes reject response data and wrapped error text.
            assert_eq!(
                diagnostics[0].1,
                vec![("error.type".to_string(), tc.want_type.to_string())],
                "{label}: diagnostic must contain only the expected error category"
            );
        }
    }
}
