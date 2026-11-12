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

//! Wire tests of `ReasoningEngineClient` against gadk-model's fakes: Go's generated REST
//! `CreateReasoningEngine` / `UpdateReasoningEngine` requests and the operation wait.
//! `FakeServer` (a real socket) needs real time; the multi-poll test uses `ScriptedClient`
//! under paused time.

use std::collections::HashMap;
use std::time::Duration;

use gadk_model::rest::test_util::{FakeResponse, FakeServer, ScriptedClient, bearer_credentials};
use gadk_platform::env::MapEnv;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::VertexError;

const PARENT: &str = "projects/p/locations/us-central1";
const ENGINE: &str = "projects/p/locations/us-central1/reasoningEngines/123";
const OP: &str = "projects/p/locations/us-central1/reasoningEngines/123/operations/456";

/// The `$alt` pair as Go's `url.Values.Encode` writes it.
const ALT_QUERY: &str = "%24alt=json%3Benum-encoding%3Dint";

/// Options sending to `server` without credentials.
fn fake_options(server: &FakeServer) -> ReasoningEngineOptions {
    ReasoningEngineOptions {
        endpoint: Some(server.url()),
        without_authentication: true,
        ..Default::default()
    }
}

/// Options sending through `scripted` (no socket) without credentials.
fn scripted_options(scripted: &Arc<ScriptedClient>) -> ReasoningEngineOptions {
    ReasoningEngineOptions {
        without_authentication: true,
        http_client: Some(scripted.clone()),
        ..Default::default()
    }
}

/// A done operation whose response is the engine `ENGINE`.
fn done_op() -> Value {
    json!({
        "name": OP,
        "done": true,
        "response": {
            "@type": "type.googleapis.com/google.cloud.aiplatform.v1beta1.ReasoningEngine",
            "name": ENGINE,
            "displayName": "my-agent",
        },
    })
}

/// Go REST `CreateReasoningEngine` then `op.Wait`: `POST /v1beta1/{parent}/reasoningEngines`
/// with the engine as body, then the operation polled at `/ui/{name}`.
#[tokio::test]
async fn create_posts_the_engine_and_wait_returns_the_response() {
    let server = FakeServer::sequence(vec![
        FakeResponse::json(json!({"name": OP}).to_string()),
        FakeResponse::json(done_op().to_string()),
    ]);
    let client = ReasoningEngineClient::new("us-central1", fake_options(&server)).unwrap();
    let engine = json!({
        "displayName": "my-agent",
        "spec": {"agentFramework": "google-adk", "classMethods": [{"name": "query"}]},
    });
    let op = client.create(PARENT, engine.clone()).await.unwrap();
    assert_eq!(op, json!({"name": OP}));
    let got = client.wait(op).await.unwrap();
    assert_eq!(got, json!({"name": ENGINE, "displayName": "my-agent"}));

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let create = &requests[0];
    assert_eq!(create.method, "POST");
    assert_eq!(
        create.path,
        "/v1beta1/projects/p/locations/us-central1/reasoningEngines"
    );
    assert_eq!(create.query.as_deref(), Some(ALT_QUERY));
    // Go `fmt.Sprintf("%s=%v", "parent", url.QueryEscape(req.GetParent()))`.
    assert_eq!(
        create.header("x-goog-request-params"),
        "parent=projects%2Fp%2Flocations%2Fus-central1"
    );
    assert_eq!(create.header("content-type"), "application/json");
    assert_eq!(
        create.header("user-agent"),
        gadk_platform::tracking_header()
    );
    assert!(create.headers.get("authorization").is_none());
    assert_eq!(create.json(), engine);

    let poll = &requests[1];
    assert_eq!(poll.method, "GET");
    assert_eq!(poll.path, format!("/ui/{OP}"));
    assert_eq!(poll.query, None);
    assert_eq!(
        poll.header("x-goog-request-params"),
        "name=projects%2Fp%2Flocations%2Fus-central1%2FreasoningEngines%2F123%2Foperations%2F456"
    );
    assert!(poll.headers.get("authorization").is_none());
    assert!(poll.body.is_empty());
}

/// Go REST `UpdateReasoningEngine`: `PATCH /v1beta1/{engine.name}`, `updateMask` = the
/// protojson field mask (`spec.source_code_spec` → `spec.sourceCodeSpec`, comma-joined) in a
/// sorted `url.Values` query, routing header `reasoning_engine.name`.
#[tokio::test]
async fn update_patches_the_engine_with_the_update_mask() {
    let server = FakeServer::sequence(vec![
        FakeResponse::json(json!({"name": OP, "done": false}).to_string()),
        FakeResponse::json(done_op().to_string()),
    ]);
    let client = ReasoningEngineClient::new("us-central1", fake_options(&server)).unwrap();
    let engine = json!({
        "name": ENGINE,
        "spec": {"sourceCodeSpec": {"imageSpec": {}}, "classMethods": []},
    });
    let op = client
        .update(
            engine.clone(),
            &["spec.sourceCodeSpec", "spec.classMethods"],
        )
        .await
        .unwrap();
    assert_eq!(op, json!({"name": OP, "done": false}));
    let got = client.wait(op).await.unwrap();
    assert_eq!(got, json!({"name": ENGINE, "displayName": "my-agent"}));

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let update = &requests[0];
    assert_eq!(update.method, "PATCH");
    assert_eq!(
        update.path,
        "/v1beta1/projects/p/locations/us-central1/reasoningEngines/123"
    );
    assert_eq!(
        update.query.as_deref(),
        Some(
            "%24alt=json%3Benum-encoding%3Dint&updateMask=spec.sourceCodeSpec%2Cspec.classMethods"
        )
    );
    assert_eq!(
        update.header("x-goog-request-params"),
        "reasoning_engine.name=projects%2Fp%2Flocations%2Fus-central1%2FreasoningEngines%2F123"
    );
    assert_eq!(update.header("content-type"), "application/json");
    assert!(update.headers.get("authorization").is_none());
    assert_eq!(update.json(), engine);
    assert_eq!(requests[1].path, format!("/ui/{OP}"));
}

/// `adkgo deploy agentengine --mem_deploy` appends `spec.context_spec` (protojson
/// `spec.contextSpec`) to the mask.
#[tokio::test]
async fn update_joins_every_mask_path() {
    let server = FakeServer::json(done_op().to_string());
    let client = ReasoningEngineClient::new("us-central1", fake_options(&server)).unwrap();
    client
        .update(
            json!({"name": ENGINE}),
            &[
                "spec.sourceCodeSpec",
                "spec.classMethods",
                "spec.contextSpec",
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        server.last_request().query.as_deref(),
        Some(
            "%24alt=json%3Benum-encoding%3Dint\
             &updateMask=spec.sourceCodeSpec%2Cspec.classMethods%2Cspec.contextSpec"
        )
    );
}

/// The default endpoint is Go adkgo's `{region}-aiplatform.googleapis.com`, over HTTPS.
#[tokio::test]
async fn the_default_endpoint_is_the_regional_host() {
    let scripted = ScriptedClient::json(json!({"name": OP}).to_string());
    let client = ReasoningEngineClient::new("europe-west4", scripted_options(&scripted)).unwrap();
    client
        .create("projects/p/locations/europe-west4", json!({}))
        .await
        .unwrap();
    assert_eq!(
        scripted.requests()[0].url,
        format!(
            "https://europe-west4-aiplatform.googleapis.com/v1beta1/projects/p/locations/europe-west4/reasoningEngines?{ALT_QUERY}"
        )
    );
}

/// Supplied credentials are sent on the call and on every poll.
#[tokio::test]
async fn supplied_credentials_are_sent() {
    let server = FakeServer::sequence(vec![
        FakeResponse::json(json!({"name": OP}).to_string()),
        FakeResponse::json(done_op().to_string()),
    ]);
    let client = ReasoningEngineClient::new(
        "us-central1",
        ReasoningEngineOptions {
            endpoint: Some(server.url()),
            credentials: Some(bearer_credentials("test-token")),
            ..Default::default()
        },
    )
    .unwrap();
    let op = client.create(PARENT, json!({})).await.unwrap();
    client.wait(op).await.unwrap();
    for req in server.requests() {
        assert_eq!(req.header("authorization"), "Bearer test-token");
    }
}

/// A non-2xx create is a `VertexError` with Go's gRPC status text.
#[tokio::test]
async fn a_failed_call_is_a_vertex_error() {
    let server = FakeServer::always(FakeResponse::json_status(
        403,
        r#"{"error":{"code":403,"message":"denied","status":"PERMISSION_DENIED"}}"#,
    ));
    let client = ReasoningEngineClient::new("us-central1", fake_options(&server)).unwrap();
    let err = client.create(PARENT, json!({})).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "rpc error: code = PermissionDenied desc = denied"
    );
    assert!(err.downcast_ref::<VertexError>().is_some());
}

/// Go `WaitWithInterval(ctx, resp, time.Minute)`: the first poll is immediate, then full-jitter
/// pauses of at most 1 s, 2 s, ….
#[tokio::test(start_paused = true)]
async fn wait_polls_a_pending_operation_until_done() {
    let scripted = ScriptedClient::sequence(vec![
        FakeResponse::json(json!({"name": OP}).to_string()),
        FakeResponse::json(json!({"name": OP, "done": false}).to_string()),
        FakeResponse::json(done_op().to_string()),
    ]);
    let client = ReasoningEngineClient::new("us-central1", scripted_options(&scripted)).unwrap();
    let got = client.wait(json!({"name": OP})).await.unwrap();
    assert_eq!(got, json!({"name": ENGINE, "displayName": "my-agent"}));
    let requests = scripted.requests();
    assert_eq!(requests.len(), 3);
    for req in &requests {
        assert_eq!(
            req.url,
            format!("https://us-central1-aiplatform.googleapis.com/ui/{OP}")
        );
    }
    assert!(requests[1].at - requests[0].at <= Duration::from_secs(1));
    assert!(requests[2].at - requests[1].at <= Duration::from_secs(2));
}

/// Go longrunning `Poll`: a done operation with `error` is `status.ErrorProto(op.Error)`.
#[tokio::test(start_paused = true)]
async fn an_operation_done_with_an_error_fails() {
    let scripted = ScriptedClient::json(
        json!({"name": OP, "done": true, "error": {"code": 9, "message": "build failed"}})
            .to_string(),
    );
    let client = ReasoningEngineClient::new("us-central1", scripted_options(&scripted)).unwrap();
    let err = client.wait(json!({"name": OP})).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "rpc error: code = FailedPrecondition desc = build failed"
    );
    assert!(err.downcast_ref::<VertexError>().is_some());
    assert_eq!(scripted.requests().len(), 1);
}

/// Go longrunning `Poll`'s `default` case: a done operation without a result.
#[tokio::test(start_paused = true)]
async fn an_operation_done_without_a_response_is_unsupported() {
    let scripted = ScriptedClient::json("{}");
    let client = ReasoningEngineClient::new("us-central1", scripted_options(&scripted)).unwrap();
    let err = client
        .wait(json!({"name": OP, "done": true}))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "unsupported result type <nil>: <nil>");
    assert!(
        scripted.requests().is_empty(),
        "a done operation is not polled"
    );
}

/// `new` resolves Application Default Credentials through the injected environment and returns
/// the ADC error unwrapped, without a request.
#[test]
fn new_returns_the_adc_error_from_the_injected_environment() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("creds.json");
    std::fs::write(&file, r#"{"type":"bogus"}"#).unwrap();
    let env: Arc<dyn EnvProvider> = Arc::new(MapEnv(HashMap::from([(
        "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
        file.to_string_lossy().into_owned(),
    )])));
    let err = ReasoningEngineClient::new(
        "us-central1",
        ReasoningEngineOptions {
            env: Some(env),
            ..Default::default()
        },
    )
    .err()
    .unwrap();
    assert_eq!(err.to_string(), "unknown credential type: bogus");
    assert!(
        err.downcast_ref::<gadk_model::gcp_auth::AdcError>()
            .is_some()
    );
}

#[test]
fn options_debug_hides_secrets() {
    let opts = ReasoningEngineOptions {
        endpoint: Some("http://127.0.0.1:1".into()),
        credentials: Some(bearer_credentials("secret")),
        ..Default::default()
    };
    let text = format!("{opts:?}");
    assert_eq!(
        text,
        "ReasoningEngineOptions { endpoint: Some(\"http://127.0.0.1:1\"), credentials: true, \
         without_authentication: false, http_client: false, env: false }"
    );
}
