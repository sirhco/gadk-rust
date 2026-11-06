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
//! Extra FunctionTool tests (spec § Testing): panic message, explicit schema override, non-object args,
//! and the Go functiontool behaviors the `function_test.go` port relies on.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gadk_agent::{Context, ContextDelta, ToolError};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_session::EventActions;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CityArgs {
    city: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct Weather {
    forecast: String,
}

fn boom() -> Result<Weather, ToolError> {
    panic!("boom")
}

#[tokio::test]
async fn function_tool_panic_message() {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("panicky", "panics"),
        |_ctx: Context, _a: CityArgs| async move { boom() },
    )
    .unwrap();
    let err = tool
        .as_runnable()
        .unwrap()
        .run(Context::builder().build(), json!({"city": "Paris"}))
        .await
        .unwrap_err();
    match err {
        ToolError::Panic(msg) => {
            assert!(msg.starts_with("panic in tool \"panicky\": boom"), "{msg}")
        }
        other => panic!("want ToolError::Panic, got {other:?}"),
    }
}

#[tokio::test]
async fn function_tool_explicit_schema_overrides() {
    let explicit = json!({"type": "object", "properties": {"city": {"type": "string", "description": "City name"}}, "required": ["city"]});
    let cfg = FunctionToolConfig {
        input_schema: Some(explicit.clone()),
        ..FunctionToolConfig::new("weather", "gets weather")
    };
    let tool = FunctionTool::new(cfg, |_ctx: Context, a: CityArgs| async move {
        Ok(Weather {
            forecast: format!("sunny in {}", a.city),
        })
    })
    .unwrap();
    let decl = tool.declaration().unwrap();
    assert_eq!(decl.parameters_json_schema, Some(explicit));
    let runnable = tool.as_runnable().unwrap();
    let bad = runnable
        .run(Context::builder().build(), json!({"city": 1}))
        .await;
    assert!(matches!(bad, Err(ToolError::InvalidArgument(_))), "{bad:?}");
    let ok: Map<String, Value> = runnable
        .run(Context::builder().build(), json!({"city": "Paris"}))
        .await
        .unwrap();
    assert_eq!(Value::Object(ok), json!({"forecast": "sunny in Paris"}));
}

#[tokio::test]
async fn function_tool_non_object_args_rejected() {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("weather", ""),
        |_ctx: Context, a: CityArgs| async move { Ok(Weather { forecast: a.city }) },
    )
    .unwrap();
    let err = tool
        .as_runnable()
        .unwrap()
        .run(Context::builder().build(), json!(42))
        .await
        .unwrap_err();
    match err {
        ToolError::InvalidArgument(msg) => assert_eq!(msg, "unexpected args type, got: float64"),
        other => panic!("want InvalidArgument, got {other:?}"),
    }
    // Args that do not infer to an object schema are rejected at construction (Go: "input must be a struct or a map").
    let not_object = FunctionTool::new(
        FunctionToolConfig::new("s", ""),
        |_ctx: Context, s: String| async move { Ok(s) },
    );
    assert!(
        matches!(not_object, Err(ToolError::InvalidArgument(ref m)) if m.starts_with("input must be a struct or a map"))
    );
}

#[test]
fn function_tool_long_running_note() {
    const NOTE: &str = "NOTE: This is a long-running operation. Do not call this tool again if it has already returned some intermediate or pending status.";
    let handler = |_ctx: Context, a: CityArgs| async move { Ok(Weather { forecast: a.city }) };
    let tool = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("t", "desc")
        },
        handler,
    )
    .unwrap();
    assert!(tool.is_long_running());
    assert_eq!(
        tool.declaration().unwrap().description,
        format!("desc\n\n{NOTE}")
    );
    let tool = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("t", "")
        },
        handler,
    )
    .unwrap();
    assert_eq!(tool.declaration().unwrap().description, NOTE);
}

#[tokio::test]
async fn function_tool_wraps_non_object_output() {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("s", ""),
        |_ctx: Context, _a: CityArgs| async move { Ok("hi".to_string()) },
    )
    .unwrap();
    let got = tool
        .as_runnable()
        .unwrap()
        .run(Context::builder().build(), json!({"city": "Paris"}))
        .await
        .unwrap();
    assert_eq!(Value::Object(got), json!({"result": "hi"}));

    let tool = FunctionTool::new(
        FunctionToolConfig::new("w", ""),
        |_ctx: Context, a: CityArgs| async move { Ok(Weather { forecast: a.city }) },
    )
    .unwrap();
    let got = tool
        .as_runnable()
        .unwrap()
        .run(Context::builder().build(), json!({"city": "Paris"}))
        .await
        .unwrap();
    assert_eq!(Value::Object(got), json!({"forecast": "Paris"}));
}

#[test]
fn function_tool_inferred_schema_settings() {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("w", ""),
        |_ctx: Context, a: CityArgs| async move { Ok(Weather { forecast: a.city }) },
    )
    .unwrap();
    let decl = tool.declaration().unwrap();
    assert_eq!(
        decl.parameters_json_schema,
        Some(
            json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]})
        )
    );
    assert_eq!(
        decl.response_json_schema,
        Some(
            json!({"type": "object", "properties": {"forecast": {"type": "string"}}, "required": ["forecast"]})
        )
    );
}

#[tokio::test]
async fn function_tool_confirmation_provider_overrides_flag() {
    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let cfg = FunctionToolConfig {
        require_confirmation: true,
        ..FunctionToolConfig::new("weather", "")
    };
    let tool = FunctionTool::with_confirmation_provider(
        cfg,
        move |_ctx: Context, a: CityArgs| {
            flag.store(true, Ordering::SeqCst);
            async move { Ok(Weather { forecast: a.city }) }
        },
        |a: &CityArgs| a.city == "Paris",
    )
    .unwrap();
    let runnable = tool.as_runnable().unwrap();

    // The provider returns false, overriding the static flag: the handler runs.
    let got = runnable
        .run(Context::builder().build(), json!({"city": "Rome"}))
        .await
        .unwrap();
    assert!(
        called.load(Ordering::SeqCst),
        "handler must run when the provider returns false"
    );
    assert_eq!(Value::Object(got), json!({"forecast": "Rome"}));

    // The provider returns true: confirmation is requested.
    called.store(false, Ordering::SeqCst);
    let actions = Arc::new(Mutex::new(EventActions::default()));
    let ctx = Context::builder().build().with_delta(ContextDelta {
        function_call_id: Some("call-1".to_string()),
        actions: Some(actions),
        ..Default::default()
    });
    let err = runnable
        .run(ctx.clone(), json!({"city": "Paris"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRequired), "{err:?}");
    assert!(
        !called.load(Ordering::SeqCst),
        "handler must not run while confirmation is pending"
    );
    let actions = ctx.actions();
    assert_eq!(
        actions.requested_tool_confirmations["call-1"].hint,
        "Please approve or reject the tool call weather() by responding with a FunctionResponse with an expected ToolConfirmation payload."
    );
    assert!(actions.skip_summarization);
}
