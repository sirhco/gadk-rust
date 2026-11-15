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

//! Port of Go tool/functiontool/streaming_function_test.go.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use futures::{StreamExt, executor::block_on, stream};
use gadk_agent::{Context, ContextDelta, Tool, ToolError};
use gadk_session::{EventActions, ToolConfirmation};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{FunctionToolConfig, StreamingFunctionTool};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StreamingArgs {
    value: String,
}

/// Port of Go newStreamingToolContext: function call id "call-id", shared actions, optional confirmation.
fn tool_ctx(actions: &Arc<Mutex<EventActions>>, confirmation: Option<ToolConfirmation>) -> Context {
    Context::builder().build().with_delta(ContextDelta {
        function_call_id: Some("call-id".to_string()),
        tool_confirmation: Some(confirmation),
        actions: Some(actions.clone()),
        ..Default::default()
    })
}

fn cfg(name: &str, require_confirmation: bool) -> FunctionToolConfig {
    FunctionToolConfig {
        name: name.to_string(),
        require_confirmation,
        ..Default::default()
    }
}

fn request() -> Value {
    json!({"value": "request"})
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum WantErr {
    None,
    Rejected,
    Required,
}

#[tokio::test]
async fn test_streaming_function_tool_confirmation() {
    let cases = [
        (
            "rejected",
            Some(ToolConfirmation {
                confirmed: false,
                ..Default::default()
            }),
            WantErr::Rejected,
            false,
            false,
        ),
        (
            "confirmed",
            Some(ToolConfirmation {
                confirmed: true,
                ..Default::default()
            }),
            WantErr::None,
            true,
            false,
        ),
        (
            "confirmation requested",
            None,
            WantErr::Required,
            false,
            true,
        ),
    ];
    for (name, confirmation, want_err, want_handler_call, want_confirmation_call) in cases {
        let actions = Arc::new(Mutex::new(EventActions::default()));
        let handler_called = Arc::new(AtomicBool::new(false));
        let called = handler_called.clone();
        let tool: Arc<dyn Tool> = StreamingFunctionTool::new(
            cfg("streaming_tool", true),
            move |_ctx: Context, args: StreamingArgs| {
                called.store(true, Ordering::SeqCst);
                stream::iter(vec![Ok(format!("handled {}", args.value))])
            },
        )
        .expect("NewStreaming() failed");
        let streaming = tool.as_streaming().expect("want StreamingTool");

        let mut values = Vec::new();
        let mut got_err = None;
        let mut s = streaming.run_stream(tool_ctx(&actions, confirmation), request());
        while let Some(item) = s.next().await {
            match item {
                Ok(v) => values.push(v),
                Err(e) => got_err = Some(e),
            }
        }

        match want_err {
            WantErr::None => assert!(got_err.is_none(), "{name}: {got_err:?}"),
            WantErr::Rejected => assert!(
                matches!(got_err, Some(ToolError::ConfirmationRejected)),
                "{name}: {got_err:?}"
            ),
            WantErr::Required => assert!(
                matches!(got_err, Some(ToolError::ConfirmationRequired)),
                "{name}: {got_err:?}"
            ),
        }
        assert_eq!(
            handler_called.load(Ordering::SeqCst),
            want_handler_call,
            "{name}: handler called"
        );
        if want_handler_call {
            assert_eq!(values, vec!["handled request".to_string()], "{name}");
        }
        let actions = actions.lock().clone();
        let requested = actions.requested_tool_confirmations.get("call-id");
        assert_eq!(
            requested.is_some(),
            want_confirmation_call,
            "{name}: confirmation requested"
        );
        if want_confirmation_call {
            assert!(
                actions.skip_summarization,
                "{name}: SkipSummarization = false, want true"
            );
            assert!(
                requested.unwrap().hint.contains("streaming_tool()"),
                "{name}: hint {:?}",
                requested.unwrap().hint
            );
        }
    }
}

// parity: adapted: Go checks RequireConfirmationProvider's dynamic type at run time; in Rust the provider is a typed
// `P: Fn(&Args) -> bool` parameter, so the three wrong-type cases ("not a function", "wrong argument", "wrong result")
// are compile errors and only "nil" (`new`) and "valid" (`with_confirmation_provider`) remain.
#[test]
fn test_new_streaming_require_confirmation_provider_validation() {
    let handler =
        |_ctx: Context, _args: StreamingArgs| stream::empty::<Result<String, ToolError>>();
    assert!(
        StreamingFunctionTool::new(FunctionToolConfig::default(), handler).is_ok(),
        "nil"
    );
    assert!(
        StreamingFunctionTool::with_confirmation_provider(
            FunctionToolConfig::default(),
            handler,
            |_: &StreamingArgs| true
        )
        .is_ok(),
        "valid"
    );
}

struct Guard(Arc<AtomicBool>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn test_streaming_function_tool_stops_handler_when_consumer_stops() {
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let flag = cleaned_up.clone();
    let tool = StreamingFunctionTool::new(
        cfg("streaming_tool", false),
        move |_ctx: Context, _args: StreamingArgs| {
            stream::unfold(Some((Guard(flag.clone()), 0u8)), |state| async move {
                let (guard, n) = state?;
                match n {
                    0 => Some((Ok("first".to_string()), Some((guard, 1)))),
                    1 => Some((Ok("second".to_string()), None)),
                    _ => None,
                }
            })
        },
    )
    .expect("NewStreaming() failed");
    let actions = Arc::new(Mutex::new(EventActions::default()));
    let mut s = tool
        .as_streaming()
        .unwrap()
        .run_stream(tool_ctx(&actions, None), request());
    let first = s.next().await;
    assert!(
        matches!(first, Some(Ok(ref v)) if v == "first"),
        "{first:?}"
    );
    drop(s);
    assert!(
        cleaned_up.load(Ordering::SeqCst),
        "handler cleanup did not run after the consumer stopped"
    );
}

struct Original;

// parity: adapted: a Rust consumer panic never re-enters the producer, so it propagates by construction; the test checks the first yielded item's error-ness and that the consumer's own panic payload survives catch_unwind
#[test]
fn test_streaming_function_tool_propagates_consumer_panic() {
    /// One Go test case of TestStreamingFunctionToolPropagatesConsumerPanic.
    #[derive(Default)]
    struct Case {
        name: &'static str,
        args: Value,
        confirmation: Option<ToolConfirmation>,
        require_confirmation: bool,
        confirmation_failure: bool,
        handler_error: bool,
        want_error: bool,
    }
    let valid = request();
    let cases = [
        Case {
            name: "chunk",
            args: valid.clone(),
            ..Default::default()
        },
        Case {
            name: "handler error",
            args: valid.clone(),
            handler_error: true,
            want_error: true,
            ..Default::default()
        },
        Case {
            name: "wrong argument type",
            args: json!(42),
            want_error: true,
            ..Default::default()
        },
        Case {
            name: "schema error",
            args: json!({"value": false}),
            want_error: true,
            ..Default::default()
        },
        Case {
            name: "confirmation rejected",
            args: valid.clone(),
            confirmation: Some(ToolConfirmation::default()),
            want_error: true,
            ..Default::default()
        },
        Case {
            name: "confirmation requested",
            args: valid.clone(),
            require_confirmation: true,
            want_error: true,
            ..Default::default()
        },
        Case {
            name: "confirmation request failed",
            args: valid.clone(),
            require_confirmation: true,
            confirmation_failure: true,
            want_error: true,
            ..Default::default()
        },
    ];
    for Case {
        name,
        args,
        confirmation,
        require_confirmation,
        confirmation_failure,
        handler_error,
        want_error,
    } in cases
    {
        let tool = StreamingFunctionTool::new(
            cfg("streaming_tool", require_confirmation),
            move |_ctx: Context, _args: StreamingArgs| {
                let item = if handler_error {
                    Err(ToolError::Execution("synthetic handler error".into()))
                } else {
                    Ok("chunk".to_string())
                };
                stream::iter([item])
            },
        )
        .expect("NewStreaming() failed");
        let actions = Arc::new(Mutex::new(EventActions::default()));
        let ctx = if confirmation_failure {
            // No function call id: request_confirmation fails (Go commonContext with an empty functionCallID).
            Context::builder().build().with_delta(ContextDelta {
                tool_confirmation: Some(None),
                ..Default::default()
            })
        } else {
            tool_ctx(&actions, confirmation)
        };
        let yielded_err = Mutex::new(None::<bool>);
        let got = catch_unwind(AssertUnwindSafe(|| {
            block_on(async {
                let mut s = tool.as_streaming().unwrap().run_stream(ctx, args);
                let first = s.next().await.unwrap();
                *yielded_err.lock() = Some(first.is_err());
                std::panic::panic_any(Original)
            })
        }));
        let payload = got.expect_err(name);
        assert!(
            payload.downcast_ref::<Original>().is_some(),
            "{name}: recovered panic is not the original consumer panic"
        );
        assert_eq!(
            *yielded_err.lock(),
            Some(want_error),
            "{name}: yielded an error"
        );
    }
}

struct CleanupPanic;

struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        std::panic::panic_any(CleanupPanic)
    }
}

// parity: adapted: Go's deferred cleanup is the handler stream's Drop
#[test]
fn test_streaming_function_tool_propagates_cleanup_panic_after_stop() {
    let tool = StreamingFunctionTool::new(
        cfg("streaming_tool", false),
        |_ctx: Context, _args: StreamingArgs| {
            let guard = PanicOnDrop;
            stream::iter([Ok("chunk".to_string())]).map(move |item| {
                let _ = &guard;
                item
            })
        },
    )
    .expect("NewStreaming() failed");
    let actions = Arc::new(Mutex::new(EventActions::default()));
    let got = catch_unwind(AssertUnwindSafe(|| {
        block_on(async {
            let mut s = tool
                .as_streaming()
                .unwrap()
                .run_stream(tool_ctx(&actions, None), request());
            let first = s.next().await;
            assert!(
                matches!(first, Some(Ok(_))),
                "RunStream() error = {first:?}"
            );
            drop(s);
        })
    }));
    let payload = got.expect_err("cleanup panic must propagate");
    assert!(
        payload.downcast_ref::<CleanupPanic>().is_some(),
        "recovered panic is not the original cleanup panic"
    );
}

#[tokio::test]
async fn test_streaming_function_tool_recovers_handler_panic() {
    #[derive(Clone, Copy)]
    enum Mode {
        OnCall,
        BeforeFirstYield,
        AfterYield,
    }
    for (name, mode, want_chunks) in [
        ("calling the handler", Mode::OnCall, 0),
        ("before first yield", Mode::BeforeFirstYield, 0),
        ("after a yield", Mode::AfterYield, 1),
    ] {
        let tool = StreamingFunctionTool::new(
            cfg("streaming_tool", false),
            move |_ctx: Context, _args: StreamingArgs| {
                let panicking = stream::poll_fn(|_| -> Poll<Option<Result<String, ToolError>>> {
                    panic!("synthetic handler panic")
                });
                match mode {
                    Mode::OnCall => panic!("synthetic handler panic"),
                    Mode::BeforeFirstYield => panicking.boxed(),
                    Mode::AfterYield => stream::iter([Ok("chunk".to_string())])
                        .chain(panicking)
                        .boxed(),
                }
            },
        )
        .expect("NewStreaming() failed");
        let actions = Arc::new(Mutex::new(EventActions::default()));
        let items: Vec<_> = tool
            .as_streaming()
            .unwrap()
            .run_stream(tool_ctx(&actions, None), request())
            .collect()
            .await;
        let chunks = items.iter().filter(|i| i.is_ok()).count();
        let errs: Vec<_> = items.iter().filter_map(|i| i.as_ref().err()).collect();
        assert_eq!(chunks, want_chunks, "{name}: chunks");
        assert_eq!(errs.len(), 1, "{name}: errors {errs:?}");
        assert!(
            matches!(errs[0], ToolError::Panic(msg) if msg.contains("panic in tool")),
            "{name}: {:?}",
            errs[0]
        );
    }
}
