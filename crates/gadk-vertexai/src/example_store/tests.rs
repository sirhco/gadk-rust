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

//! Tests of the Example Store provider's REST wire against gadk-model's 127.0.0.1 fake server.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{Context, ToolError};
use gadk_genai::Content;
use gadk_model::rest::test_util::{FakeResponse, FakeServer};
use gadk_platform::env::MapEnv;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::{ExampleStoreError, ProviderConfig, golden, new_provider};
use crate::error::find_vertex_error;

/// The store every test uses (Go `testStoreName`).
const STORE: &str = "projects/p/locations/l/exampleStores/s";

/// Go `url.QueryEscape(store)` in the routing header.
const ROUTING: &str = "example_store=projects%2Fp%2Flocations%2Fl%2FexampleStores%2Fs";

/// The `$alt` query Go's REST client adds.
const ALT_QUERY: &str = "%24alt=json%3Benum-encoding%3Dint";

/// A config for a fake at `url`: no credentials, an empty environment.
fn config(url: &str) -> ProviderConfig {
    ProviderConfig {
        example_store_name: STORE.to_owned(),
        endpoint: Some(url.to_owned()),
        without_authentication: true,
        env: Arc::new(MapEnv(HashMap::new())),
        ..Default::default()
    }
}

/// Go's REST `SearchExamples`: `POST /v1beta1/{store}:searchExamples` with the `$alt` query,
/// the `example_store` routing header and the golden's body; the golden's response maps to
/// the golden's examples.
#[tokio::test]
async fn search_examples_sends_gos_rest_request() {
    let g = golden("converts_text_part");
    let server = FakeServer::json(g["response"].to_string());
    let p = new_provider(config(&server.url())).unwrap();
    let examples = p
        .examples(&Context::builder().build(), "query")
        .await
        .unwrap();

    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path_and_query(),
        format!("/v1beta1/{STORE}:searchExamples?{ALT_QUERY}")
    );
    assert_eq!(req.header("x-goog-request-params"), ROUTING);
    assert_eq!(req.header("content-type"), "application/json");
    assert!(req.headers.get("authorization").is_none());
    assert_eq!(req.json(), g["request"]);

    assert_eq!(examples.len(), 1);
    assert_eq!(
        examples[0].input,
        Content::from_text("what is the weather?", "user")
    );
    assert_eq!(
        examples[0].output,
        vec![Content::from_text("it is sunny", "model")]
    );
}

/// An endpoint with a path prefix and trailing `/` keeps the prefix (as the other services).
#[tokio::test]
async fn endpoint_prefix_is_kept() {
    let server = FakeServer::json("{}");
    let p = new_provider(config(&format!("{}/prefix/", server.url()))).unwrap();
    let examples = p.examples(&Context::builder().build(), "q").await.unwrap();
    assert!(examples.is_empty());
    assert_eq!(
        server.requests()[0].path_and_query(),
        format!("/prefix/v1beta1/{STORE}:searchExamples?{ALT_QUERY}")
    );
}

/// An HTTP error is Go's `vertexai: search examples: rpc error: …`, with the
/// [`VertexError`](crate::VertexError) in the source chain.
#[tokio::test]
async fn http_errors_are_wrapped_with_gos_prefix() {
    let server = FakeServer::always(FakeResponse::json_status(
        404,
        json!({"error": {"code": 404, "message": "store not found", "status": "NOT_FOUND"}})
            .to_string(),
    ));
    let p = new_provider(config(&server.url())).unwrap();
    let err = p
        .examples(&Context::builder().build(), "q")
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "vertexai: search examples: rpc error: code = NotFound desc = store not found"
    );
    let ToolError::Execution(inner) = &err else {
        panic!("want ToolError::Execution, got {err:?}");
    };
    assert!(find_vertex_error(inner.as_ref()).is_some_and(crate::VertexError::is_not_found));
}

/// Go `fmt.Errorf("vertexai: create example store client: %w", err)` when Application Default
/// Credentials cannot be built; the store name is checked first.
#[test]
fn adc_errors_are_gos_create_client_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("creds.json");
    std::fs::write(&file, r#"{"type":"bogus"}"#).unwrap();
    let env = Arc::new(MapEnv(HashMap::from([(
        "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
        file.to_string_lossy().into_owned(),
    )])));
    let cfg = ProviderConfig {
        example_store_name: STORE.to_owned(),
        env,
        ..Default::default()
    };
    let err = new_provider(cfg.clone()).err().unwrap();
    assert!(matches!(err, ExampleStoreError::CreateClient(_)));
    assert_eq!(
        err.to_string(),
        "vertexai: create example store client: unknown credential type: bogus"
    );
    assert!(std::error::Error::source(&err).is_some());

    let err = new_provider(ProviderConfig {
        example_store_name: "s".to_owned(),
        ..cfg
    })
    .err()
    .unwrap();
    assert!(matches!(err, ExampleStoreError::InvalidStoreName));
}

/// The default endpoint is the store location's host (Go `aiplatform.HostURL`): the `global`
/// location uses `aiplatform.googleapis.com`.
#[tokio::test]
async fn default_endpoint_follows_the_store_location() {
    for (store, host) in [
        (
            "projects/p/locations/us-central1/exampleStores/s",
            "https://us-central1-aiplatform.googleapis.com",
        ),
        (
            "projects/p/locations/global/exampleStores/s",
            "https://aiplatform.googleapis.com",
        ),
    ] {
        let scripted = gadk_model::rest::test_util::ScriptedClient::json("{}");
        let p = new_provider(ProviderConfig {
            example_store_name: store.to_owned(),
            endpoint: None,
            http_client: Some(scripted.clone()),
            ..config("")
        })
        .unwrap();
        p.examples(&Context::builder().build(), "q").await.unwrap();
        assert_eq!(
            scripted.last_request().url,
            format!("{host}/v1beta1/{store}:searchExamples?{ALT_QUERY}")
        );
    }
}

/// `ProviderConfig`'s `Debug` hides the credentials and HTTP client.
#[test]
fn config_debug_hides_secrets() {
    let text = format!("{:?}", config("http://x"));
    assert!(text.contains("example_store_name: \"projects/p/locations/l/exampleStores/s\""));
    assert!(text.contains("credentials: false"));
}

/// A response whose one result holds `part` in an expected content (score `score`).
fn response_with_part(part: Value, score: f64) -> String {
    json!({"results": [{
        "similarityScore": score,
        "example": {"storedContentsExample": {
            "searchKey": "k",
            "contentsExample": {"expectedContents": [
                {"content": {"role": "model", "parts": [part]}},
            ]},
        }},
    }]})
    .to_string()
}

/// The error `Examples` returns for `body`, or a panic.
async fn search_error(body: String) -> ToolError {
    let server = FakeServer::json(body);
    let p = new_provider(config(&server.url())).unwrap();
    p.examples(&Context::builder().build(), "q")
        .await
        .err()
        .unwrap()
}

/// Go's protojson decode of the response fails on malformed base64 in `inlineData.data`, so
/// `Examples` fails with `vertexai: search examples: …`.
#[tokio::test]
async fn malformed_inline_data_bytes_fail_the_search() {
    let err = search_error(response_with_part(
        json!({"inlineData": {"mimeType": "image/png", "data": "not base64!"}}),
        1.0,
    ))
    .await;
    assert_eq!(
        err.to_string(),
        r#"vertexai: search examples: invalid value for bytes field data: "not base64!""#
    );
}

/// The same for `thoughtSignature`, also in a result below the similarity threshold: Go decodes
/// the whole response before filtering.
#[tokio::test]
async fn malformed_thought_signature_bytes_fail_the_search() {
    for score in [1.0, 0.1] {
        let err = search_error(response_with_part(
            json!({"thought": true, "thoughtSignature": "%%%"}),
            score,
        ))
        .await;
        assert_eq!(
            err.to_string(),
            r#"vertexai: search examples: invalid value for bytes field thoughtSignature: "%%%""#,
            "score {score}"
        );
    }
    let err = search_error(response_with_part(json!({"thoughtSignature": 7}), 1.0)).await;
    assert_eq!(
        err.to_string(),
        "vertexai: search examples: invalid value for bytes field thoughtSignature: 7"
    );
}

/// The same for a `videoMetadata` offset that is not protojson `Duration` text.
#[tokio::test]
async fn malformed_video_metadata_offset_fails_the_search() {
    let err = search_error(response_with_part(
        json!({"videoMetadata": {"startOffset": "5s", "endOffset": "1m"}}),
        1.0,
    ))
    .await;
    assert_eq!(
        err.to_string(),
        r#"vertexai: search examples: invalid google.protobuf.Duration value "1m""#
    );
    let err = search_error(response_with_part(
        json!({"videoMetadata": {"startOffset": "315576000001s"}}),
        1.0,
    ))
    .await;
    assert_eq!(
        err.to_string(),
        r#"vertexai: search examples: google.protobuf.Duration value out of range: "315576000001s""#
    );
}

/// A `SearchExamples` call that never answers.
struct PendingClient;

#[async_trait::async_trait]
impl super::ExampleStoreClient for PendingClient {
    async fn search_examples(&self, _req: &Value) -> Result<Value, crate::error::BoxError> {
        std::future::pending().await
    }
}

/// Go passes the invocation context to `SearchExamples`: cancelling it mid-call aborts the call
/// with `vertexai: search examples: {cause}`.
#[tokio::test]
async fn cancelling_the_context_aborts_the_search() {
    let p =
        super::new_provider_with_client(&config("http://unused"), Arc::new(PendingClient)).unwrap();
    let ctx = Context::builder().build();
    let (got, ()) = tokio::join!(p.examples(&ctx, "query"), async {
        tokio::task::yield_now().await;
        ctx.cancel_with_cause(gadk_agent::CancelCause::DeadlineExceeded);
    });
    let err = got.unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "vertexai: search examples: context deadline exceeded"
    );
}
