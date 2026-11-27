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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Confirmation tests: which calls need confirmation, and that none is sent without it.

mod common;

use std::sync::{Arc, Mutex};

use common::{Reply, TestServer};
use gadk_agent::tool::{ConfirmationProvider, Tool, Toolset};
use gadk_agent::{Context, ContextDelta, ToolError};
use gadk_openapi::{OpenApiConfig, OpenApiSpec, OpenApiToolset, RequireConfirmation};
use gadk_session::ToolConfirmation;
use serde_json::{Map, Value, json};

const SPEC: &str = r#"
openapi: 3.1.0
paths:
  /pets/{id}:
    get:
      operationId: getPet
      parameters:
        - {name: id, in: path, schema: {type: string}}
    delete:
      operationId: deletePet
      parameters:
        - {name: id, in: path, schema: {type: string}}
"#;

const HINT: &str = "Please approve or reject the tool call delete_pet() by responding with a FunctionResponse with an expected ToolConfirmation payload.";

/// A tool context with function call id `fc-1` and the given confirmation.
fn ctx(confirmation: Option<ToolConfirmation>) -> Context {
    Context::builder()
        .invocation_id("inv-1")
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            tool_confirmation: Some(confirmation),
            actions: Some(Default::default()),
            ..Default::default()
        })
}

fn confirmation(confirmed: bool) -> Option<ToolConfirmation> {
    Some(ToolConfirmation {
        confirmed,
        ..Default::default()
    })
}

async fn tools(base: &str, config: OpenApiConfig) -> Vec<Arc<dyn Tool>> {
    let spec: OpenApiSpec = SPEC.parse().unwrap();
    let set = OpenApiToolset::new(&spec, config.base_url(base)).unwrap();
    set.tools(&ctx(None)).await.unwrap()
}

async fn run(tool: &Arc<dyn Tool>, ctx: Context) -> Result<Map<String, Value>, ToolError> {
    tool.as_runnable()
        .unwrap()
        .run(ctx, json!({"id": "7"}))
        .await
}

#[tokio::test]
async fn a_delete_needs_confirmation_by_default_and_sends_nothing() {
    let srv = TestServer::replying(Reply::json("{}")).await;
    let tools = tools(srv.url(), OpenApiConfig::new()).await;
    let ctx = ctx(None);
    let err = run(&tools[1], ctx.clone()).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRequired), "{err:?}");
    let actions = ctx.actions();
    assert_eq!(
        actions
            .requested_tool_confirmations
            .get("fc-1")
            .map(|c| c.hint.as_str()),
        Some(HINT)
    );
    assert!(actions.skip_summarization);
    assert_eq!(srv.calls(), 0);
}

#[tokio::test]
async fn a_get_proceeds_by_default() {
    let srv = TestServer::replying(Reply::json(r#"{"id": "7"}"#)).await;
    let tools = tools(srv.url(), OpenApiConfig::new()).await;
    let result = run(&tools[0], ctx(None)).await.unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 200, "body": {"id": "7"}})
    );
    assert_eq!(srv.calls(), 1);
}

#[tokio::test]
async fn never_and_always_behave_as_named() {
    let srv = TestServer::replying(Reply::json("{}")).await;
    let never = tools(
        srv.url(),
        OpenApiConfig::new().require_confirmation(RequireConfirmation::Never),
    )
    .await;
    run(&never[1], ctx(None)).await.unwrap();
    assert_eq!(srv.calls(), 1);

    let always = tools(
        srv.url(),
        OpenApiConfig::new().require_confirmation(RequireConfirmation::Always),
    )
    .await;
    let err = run(&always[0], ctx(None)).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRequired), "{err:?}");
    assert_eq!(srv.calls(), 1);
}

#[tokio::test]
async fn the_provider_overrides_the_mode_and_sees_the_exposed_name() {
    let srv = TestServer::replying(Reply::json("{}")).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    // Confirm reads, never deletes: the opposite of the default mode.
    let provider: ConfirmationProvider = Arc::new(move |name: &str, args: &Value| {
        recorder
            .lock()
            .unwrap()
            .push((name.to_string(), args.clone()));
        name.ends_with("get_pet")
    });
    let tools = tools(
        srv.url(),
        OpenApiConfig::new()
            .prefix("shop")
            .confirmation_provider(provider),
    )
    .await;
    run(&tools[1], ctx(None)).await.unwrap();
    assert_eq!(srv.calls(), 1);
    let err = run(&tools[0], ctx(None)).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRequired), "{err:?}");
    assert_eq!(srv.calls(), 1);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            ("shop_delete_pet".to_string(), json!({"id": "7"})),
            ("shop_get_pet".to_string(), json!({"id": "7"})),
        ]
    );
}

#[tokio::test]
async fn a_confirmed_call_is_sent_and_a_rejected_one_is_not() {
    let srv = TestServer::replying(Reply::status(204, "")).await;
    let tools = tools(srv.url(), OpenApiConfig::new()).await;
    let result = run(&tools[1], ctx(confirmation(true))).await.unwrap();
    assert_eq!(Value::Object(result), json!({"status": 204, "body": null}));
    let req = &srv.requests()[0];
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("DELETE", "/pets/7")
    );

    let err = run(&tools[1], ctx(confirmation(false))).await.unwrap_err();
    assert!(matches!(err, ToolError::ConfirmationRejected), "{err:?}");
    assert_eq!(srv.calls(), 1);
}
