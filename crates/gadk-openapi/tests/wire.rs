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

//! Wire tests: tool calls against a local test server.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Reply, TestServer};
use gadk_agent::tool::{Tool, Toolset};
use gadk_agent::{Context, ContextDelta, ToolError};
use gadk_openapi::{Error, OpenApiConfig, OpenApiSpec, OpenApiToolset, RequireConfirmation};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

const SPEC: &str = r#"
openapi: 3.0.3
info: {title: Items, version: "1"}
paths:
  /items/{id}:
    get:
      operationId: getItem
      parameters:
        - {name: id, in: path, required: true, schema: {type: string}}
        - {name: tag, in: query, schema: {type: array, items: {type: string}}}
        - {name: limit, in: query, schema: {type: integer}}
        - {name: exact, in: query, schema: {type: boolean}}
        - {name: X-Trace, in: header, schema: {type: string}}
  /items:
    post:
      operationId: createItem
      requestBody:
        required: true
        content:
          application/json:
            schema: {type: object}
"#;

const ECHO_3_1: &str = include_str!("fixtures/echo-3.1.json");

fn ctx_with_token(token: CancellationToken) -> Context {
    Context::builder()
        .invocation_id("inv-1")
        .cancellation_token(token)
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            actions: Some(Default::default()),
            ..Default::default()
        })
}

fn ctx() -> Context {
    ctx_with_token(CancellationToken::new())
}

/// The toolset over `SPEC` at `base`, without confirmation.
fn toolset(base: &str, config: OpenApiConfig) -> OpenApiToolset {
    let spec: OpenApiSpec = SPEC.parse().unwrap();
    OpenApiToolset::new(
        &spec,
        config
            .base_url(base)
            .require_confirmation(RequireConfirmation::Never),
    )
    .unwrap()
}

async fn tool(toolset: &OpenApiToolset, name: &str) -> Arc<dyn Tool> {
    toolset
        .tools(&ctx())
        .await
        .unwrap()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap()
}

async fn call_with(
    tool: &Arc<dyn Tool>,
    ctx: Context,
    args: Value,
) -> Result<Map<String, Value>, ToolError> {
    tool.as_runnable().unwrap().run(ctx, args).await
}

async fn call(tool: &Arc<dyn Tool>, args: Value) -> Result<Map<String, Value>, ToolError> {
    call_with(tool, ctx(), args).await
}

/// The crate error behind a `ToolError::Execution`.
fn execution(err: &ToolError) -> &Error {
    match err {
        ToolError::Execution(source) => source
            .downcast_ref::<Error>()
            .expect("an Execution source is a gadk_openapi::Error"),
        other => panic!("want ToolError::Execution, got {other:?}"),
    }
}

#[tokio::test]
async fn path_query_headers_and_fixed_headers_reach_the_server() {
    let srv = TestServer::replying(Reply::json(r#"{"ok": true}"#)).await;
    let set = toolset(
        srv.url(),
        OpenApiConfig::new().header("X-Client", "gadk-test"),
    );
    let get = tool(&set, "get_item").await;
    let result = call(
        &get,
        json!({
            "id": "a b/c",
            "tag": ["x", "y"],
            "limit": 5,
            "exact": true,
            "X_Trace": "t-1",
            "unknown": "ignored"
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 200, "body": {"ok": true}})
    );
    let req = &srv.requests()[0];
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/items/a%20b%2Fc");
    assert_eq!(req.query.as_deref(), Some("tag=x&tag=y&limit=5&exact=true"));
    assert_eq!(req.header("x-trace"), Some("t-1"));
    assert_eq!(req.header("x-client"), Some("gadk-test"));
    assert_eq!(req.header("authorization"), None);
}

#[tokio::test]
async fn omitted_optionals_are_left_out() {
    let srv = TestServer::replying(Reply::json("{}")).await;
    let set = toolset(srv.url(), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    call(&get, json!({"id": "1", "tag": null})).await.unwrap();
    let req = &srv.requests()[0];
    assert_eq!(req.path, "/items/1");
    assert_eq!(req.query, None);
    assert_eq!(req.header("x-trace"), None);
}

#[tokio::test]
async fn the_body_is_sent_as_json() {
    let srv = TestServer::replying(Reply::status(201, "")).await;
    let set = toolset(&format!("{}/", srv.url()), OpenApiConfig::new());
    let create = tool(&set, "create_item").await;
    let result = call(&create, json!({"body": {"name": "Rex", "age": 3}}))
        .await
        .unwrap();
    // An empty body is `null`.
    assert_eq!(Value::Object(result), json!({"status": 201, "body": null}));
    let req = &srv.requests()[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/items");
    assert_eq!(req.header("content-type"), Some("application/json"));
    assert_eq!(req.json(), json!({"name": "Rex", "age": 3}));
}

#[tokio::test]
async fn a_static_token_and_a_query_key_are_sent() {
    let srv = TestServer::replying(Reply::json("{}")).await;
    let set = toolset(
        srv.url(),
        OpenApiConfig::new()
            .auth(gadk_auth::static_token("tok-1"))
            .query_api_key("key", "k&1"),
    );
    let get = tool(&set, "get_item").await;
    call(&get, json!({"id": "1", "limit": 2})).await.unwrap();
    let req = &srv.requests()[0];
    assert_eq!(req.header("authorization"), Some("Bearer tok-1"));
    assert_eq!(req.query.as_deref(), Some("limit=2&key=k%261"));
}

#[tokio::test]
async fn error_statuses_are_results() {
    let srv = TestServer::replying(
        Reply::status(404, r#"{"error": "no such item"}"#)
            .with_header("Content-Type", "application/problem+json"),
    )
    .await;
    let set = toolset(srv.url(), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    let result = call(&get, json!({"id": "9"})).await.unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 404, "body": {"error": "no such item"}})
    );
}

#[tokio::test]
async fn text_and_unparsable_json_bodies_are_strings() {
    let srv = TestServer::replying(
        Reply::ok("plain words").with_header("Content-Type", "text/plain; charset=utf-8"),
    )
    .await;
    let set = toolset(srv.url(), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    let result = call(&get, json!({"id": "1"})).await.unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 200, "body": "plain words"})
    );

    let srv = TestServer::replying(Reply::json("{not json")).await;
    let set = toolset(srv.url(), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    let result = call(&get, json!({"id": "1"})).await.unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 200, "body": "{not json"})
    );
}

#[tokio::test]
async fn a_long_body_is_truncated() {
    let srv = TestServer::replying(Reply::json(format!("[{}]", "1,".repeat(100) + "1"))).await;
    let set = toolset(srv.url(), OpenApiConfig::new().max_response_bytes(10));
    let get = tool(&set, "get_item").await;
    let result = call(&get, json!({"id": "1"})).await.unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 200, "body": "[1,1,1,1,1", "truncated": true})
    );
    // A body of exactly the limit is kept whole.
    let srv = TestServer::replying(Reply::json("[1,2,3,45]")).await;
    let set = toolset(srv.url(), OpenApiConfig::new().max_response_bytes(10));
    let get = tool(&set, "get_item").await;
    let result = call(&get, json!({"id": "1"})).await.unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"status": 200, "body": [1, 2, 3, 45]})
    );
}

#[tokio::test]
async fn a_redirect_is_a_result_and_not_followed() {
    let srv = TestServer::replying(Reply::redirect("/elsewhere", 307)).await;
    let set = toolset(
        srv.url(),
        OpenApiConfig::new().header("X-API-Key", "secret"),
    );
    let create = tool(&set, "create_item").await;
    let result = call(&create, json!({"body": {"name": "Rex"}}))
        .await
        .unwrap();
    assert_eq!(Value::Object(result), json!({"status": 307, "body": null}));
    assert_eq!(srv.calls(), 1);
    assert_eq!(srv.requests()[0].path, "/items");
}

#[tokio::test]
async fn a_missing_required_argument_sends_nothing() {
    let srv = TestServer::replying(Reply::json("{}")).await;
    let set = toolset(srv.url(), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    match call(&get, json!({"limit": 1})).await {
        Err(ToolError::InvalidArgument(message)) => {
            assert_eq!(message, "missing required argument id");
        }
        other => panic!("want InvalidArgument, got {other:?}"),
    }
    assert_eq!(srv.calls(), 0);
}

#[tokio::test]
async fn a_server_that_never_answers_times_out() {
    let srv = TestServer::start(|_req| std::future::pending::<Reply>()).await;
    let set = toolset(
        srv.url(),
        OpenApiConfig::new().timeout(Duration::from_millis(200)),
    );
    let get = tool(&set, "get_item").await;
    let err = call(&get, json!({"id": "1"})).await.unwrap_err();
    match execution(&err) {
        Error::Timeout { tool } => assert_eq!(tool, "get_item"),
        other => panic!("want Timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn a_cancelled_invocation_stops_the_call() {
    let srv = TestServer::start(|_req| std::future::pending::<Reply>()).await;
    let set = toolset(srv.url(), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let err = call_with(&get, ctx_with_token(token), json!({"id": "1"}))
        .await
        .unwrap_err();
    match execution(&err) {
        Error::Cancelled { tool } => assert_eq!(tool, "get_item"),
        other => panic!("want Cancelled, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_connection_is_a_request_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let set = toolset(&format!("http://{addr}"), OpenApiConfig::new());
    let get = tool(&set, "get_item").await;
    let err = call(&get, json!({"id": "1"})).await.unwrap_err();
    match execution(&err) {
        Error::Request { tool, .. } => assert_eq!(tool, "get_item"),
        other => panic!("want Request, got {other:?}"),
    }
}

#[tokio::test]
async fn a_fetched_spec_resolves_its_relative_server_url() {
    let srv = TestServer::start(|req| async move {
        if req.path == "/specs/echo.json" {
            Reply::json(ECHO_3_1)
        } else {
            Reply::json(req.body.clone())
        }
    })
    .await;
    let url = format!("{}/specs/echo.json", srv.url());
    let spec = OpenApiSpec::fetch(&url).await.unwrap();
    assert_eq!(spec.source_url(), Some(url.as_str()));
    let set = OpenApiToolset::new(
        &spec,
        OpenApiConfig::new().require_confirmation(RequireConfirmation::Never),
    )
    .unwrap();
    let echo = tool(&set, "echo").await;
    let result = call(&echo, json!({"body": "hi"})).await.unwrap();
    assert_eq!(Value::Object(result), json!({"status": 200, "body": "hi"}));
    let req = &srv.requests()[1];
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("POST", "/v1/echo")
    );
}

#[tokio::test]
async fn a_failed_or_oversized_fetch_is_an_error() {
    let srv = TestServer::replying(Reply::status(404, "missing")).await;
    let url = format!("{}/openapi.yaml", srv.url());
    match OpenApiSpec::fetch(&url).await {
        Err(Error::Fetch { url: got, message }) => {
            assert_eq!(got, url);
            assert_eq!(message, "HTTP status 404 Not Found");
        }
        other => panic!("want Fetch, got {other:?}"),
    }

    let big = format!("openapi: 3.1.0\n#{}\n", "x".repeat(10 * 1024 * 1024));
    let srv = TestServer::replying(Reply::ok(big)).await;
    match OpenApiSpec::fetch(&format!("{}/openapi.yaml", srv.url())).await {
        Err(Error::Fetch { message, .. }) => {
            assert_eq!(message, "the document is larger than 10485760 bytes");
        }
        other => panic!("want Fetch, got {other:?}"),
    }
}
