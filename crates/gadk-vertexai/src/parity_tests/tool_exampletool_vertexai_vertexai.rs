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

//! Ports of ADK-Go `tool/exampletool/vertexai/vertexai_test.go`, driven by the Go-generated
//! goldens in `testdata/example_store/`: each golden holds the
//! `SearchExamplesRequest` Go built, the response its fake returned (protojson, enum numbers)
//! and the examples Go mapped it to (`json.Marshal`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{CancelCause, Context, ToolError};
use gadk_model::rest::test_util::ScriptedClient;
use gadk_platform::env::MapEnv;
use gadk_tools::example::Example;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::error::BoxError;
use crate::example_store::{
    ExampleStoreClient, ExampleStoreError, ProviderConfig, golden, new_provider,
    new_provider_with_client, parse_location,
};

/// Go `testStoreName`.
const TEST_STORE_NAME: &str = "projects/p/locations/l/exampleStores/s";

/// The `TestProvider_Examples` goldens, in Go's subtest order.
const EXAMPLES_GOLDENS: [&str; 16] = [
    "searches_the_configured_store",
    "returns_empty_slice_without_results",
    "tolerates_unset_results",
    "converts_text_part",
    "filters_results_below_similarity_threshold",
    "converts_function_call_part",
    "converts_function_response_part",
    "preserves_inline_data_part",
    "converts_file_data_video_metadata_executable_code_code_execution_result_and_thought_fields",
    "keeps_data_less_parts_with_thought_signature_or_video_metadata_and_drops_empty_parts",
    "preserves_multi_step_expected_output",
    "skips_expected_contents_without_content",
    "skips_result_without_example",
    "skips_result_without_stored_contents_example",
    "tolerates_unset_contents_example",
    "propagates_search_error",
];

/// Go's `errors.New("rpc failed")` sentinel.
#[derive(Debug)]
struct SearchError(String);

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SearchError {}

/// Go `fakeExampleStoreClient`: records the request, answers with `resp` or fails with `err`.
#[derive(Default)]
struct FakeExampleStoreClient {
    got_req: Mutex<Option<Value>>,
    resp: Value,
    err: Option<String>,
}

#[async_trait]
impl ExampleStoreClient for FakeExampleStoreClient {
    async fn search_examples(&self, req: &Value) -> Result<Value, BoxError> {
        *self.got_req.lock() = Some(req.clone());
        match &self.err {
            Some(err) => Err(Box::new(SearchError(err.clone()))),
            None => Ok(self.resp.clone()),
        }
    }
}

fn config(store_name: &str) -> ProviderConfig {
    ProviderConfig {
        example_store_name: store_name.to_owned(),
        without_authentication: true,
        env: Arc::new(MapEnv(HashMap::new())),
        ..Default::default()
    }
}

/// Go `json.Marshal([]*exampletool.Example)`: `{"input": Content, "output": [Content…]}`.
fn examples_json(examples: &[Example]) -> Value {
    Value::Array(
        examples
            .iter()
            .map(|e| json!({"input": e.input, "output": e.output}))
            .collect(),
    )
}

/// Go `errors.Is(err, wantErr)`: whether a [`SearchError`] with text `want` is in the chain
/// (stepping into the transparent `ToolError::Execution` box).
fn wraps(err: &ToolError, want: &str) -> bool {
    let ToolError::Execution(inner) = err else {
        return false;
    };
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&**inner);
    while let Some(e) = cur {
        if e.downcast_ref::<SearchError>().is_some_and(|s| s.0 == want) {
            return true;
        }
        cur = e.source();
    }
    false
}

// parity: adapted: no nil config or nil client in Rust (those cases are dropped); there is no gRPC dial or Close (ledger 2a divergences), so the regional-endpoint subtest checks the REST URL a ScriptedClient receives (https://{host_url(location)}) and the Close subtest checks the provider releases its client when dropped; new_provider takes no context, so the detached client-init context check is dropped; the canceled per-call context is a Context cancelled with CancelCause::Canceled and the error text is also checked
#[tokio::test]
async fn test_provider_config_validation() {
    // extracts location from valid store resource name
    // reference: test_get_examples_derives_project_and_location_from_the_store_name
    for store_name in [
        "projects/my-proj_1/locations/us-central1/exampleStores/store.1",
        "projects/example.com:my-proj/locations/us-central1/exampleStores/store.1",
    ] {
        match parse_location(&config(store_name)) {
            Ok(loc) => assert_eq!(loc, "us-central1", "parseLocation({store_name:?})"),
            Err(e) => panic!("parseLocation({store_name:?}) error = {e}"),
        }
    }

    // NewProvider builds client for regional endpoint
    let scripted = ScriptedClient::json(
        json!({"results": [{
            "similarityScore": 0.9,
            "example": {"storedContentsExample": {
                "searchKey": "hello",
                "contentsExample": {"expectedContents": [
                    {"content": {"role": "model", "parts": [{"text": "world"}]}},
                ]},
            }},
        }]})
        .to_string(),
    );
    let store_name = "projects/example.com:my-proj/locations/asia-east1/exampleStores/store-1";
    let p = new_provider(ProviderConfig {
        http_client: Some(scripted.clone()),
        ..config(store_name)
    })
    .unwrap_or_else(|e| panic!("NewProvider() error = {e}"));
    let examples = p
        .examples(&Context::builder().build(), "hi")
        .await
        .unwrap_or_else(|e| panic!("Examples() error = {e}"));
    let req = scripted.last_request();
    assert_eq!(
        req.url,
        format!(
            "https://asia-east1-aiplatform.googleapis.com/v1beta1/{store_name}:searchExamples?%24alt=json%3Benum-encoding%3Dint"
        ),
        "request URL"
    );
    assert_eq!(
        req.json()["exampleStore"],
        store_name,
        "SearchExamples ExampleStore"
    );
    assert!(
        examples.len() == 1 && examples[0].output[0].parts[0].text == "world",
        "Examples() = {examples:?}, want 1 example with output 'world'"
    );

    // A canceled per-call context fails SearchExamples.
    let canceled_ctx = Context::builder().build();
    canceled_ctx.cancel_with_cause(CancelCause::Canceled);
    match p.examples(&canceled_ctx, "hi").await {
        Err(e @ ToolError::Execution(_)) => assert_eq!(
            e.to_string(),
            "vertexai: search examples: context canceled",
            "Examples() with canceled context"
        ),
        Err(e) => panic!("Examples() with canceled context error = {e}, want Execution"),
        Ok(got) => panic!("Examples() with canceled context = {got:?}, want error"),
    }

    // Close delegates to underlying client → dropping the provider releases the client.
    let client = Arc::new(FakeExampleStoreClient::default());
    let p = new_provider_with_client(&config(TEST_STORE_NAME), client.clone())
        .unwrap_or_else(|e| panic!("newProvider() error = {e}"));
    assert_eq!(Arc::strong_count(&client), 2);
    drop(p);
    assert_eq!(Arc::strong_count(&client), 1, "client released");

    // rejects invalid config or resource name
    for (name, store_name) in [
        ("empty store name", ""),
        ("bare store ID", "exampleStores/s"),
        ("missing exampleStores segment", "projects/p/locations/l"),
        (
            "extra trailing segment",
            "projects/p/locations/l/exampleStores/s/extra",
        ),
        (
            "invalid characters in location",
            "projects/p/locations/l#bad/exampleStores/s",
        ),
        ("empty project", "projects//locations/l/exampleStores/s"),
    ] {
        let want = "vertexai: example_store_name must match projects/{project}/locations/{location}/exampleStores/{example_store}";
        match new_provider(config(store_name)) {
            Err(e @ ExampleStoreError::InvalidStoreName) => assert_eq!(e.to_string(), want),
            Err(e) => panic!("{name}: NewProvider() error = {e}, want InvalidStoreName"),
            Ok(_) => panic!("{name}: NewProvider() succeeded, want error"),
        }
        let client = Arc::new(FakeExampleStoreClient::default());
        match new_provider_with_client(&config(store_name), client) {
            Err(e @ ExampleStoreError::InvalidStoreName) => assert_eq!(e.to_string(), want),
            Err(e) => panic!("{name}: newProvider() error = {e}, want InvalidStoreName"),
            Ok(_) => panic!("{name}: newProvider() succeeded, want error"),
        }
    }
}

// parity: adapted: each subtest runs from its Go-generated golden: the fake client answers with the golden's protojson response (Go's nil response is {}, nil list elements are {}), and the captured request and the mapped examples are compared with the golden's request and examples as JSON values; errors.Is is a source-chain search for the injected error, and the wrapped text is also checked
#[tokio::test]
async fn test_provider_examples() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/example_store");
    let on_disk = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .count();
    assert_eq!(on_disk, EXAMPLES_GOLDENS.len(), "every golden is ported");

    for stem in EXAMPLES_GOLDENS {
        let g = golden(stem);
        let client = Arc::new(FakeExampleStoreClient {
            resp: g.get("response").cloned().unwrap_or(Value::Null),
            err: g
                .get("searchError")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Default::default()
        });
        let p = new_provider_with_client(&config(TEST_STORE_NAME), client.clone())
            .unwrap_or_else(|e| panic!("{stem}: newProvider() error = {e}"));
        let query = g["query"].as_str().unwrap();
        let result = p.examples(&Context::builder().build(), query).await;

        assert_eq!(
            client.got_req.lock().clone(),
            Some(g["request"].clone()),
            "{stem}: SearchExamplesRequest"
        );
        match (result, g.get("error")) {
            (Ok(examples), None) => {
                assert_eq!(examples_json(&examples), g["examples"], "{stem}: examples");
            }
            (Err(err), Some(want)) => {
                assert_eq!(Value::String(err.to_string()), *want, "{stem}: error");
                let search_error = g["searchError"].as_str().unwrap();
                assert!(wraps(&err, search_error), "{stem}: errors.Is");
            }
            (Ok(examples), Some(want)) => {
                panic!("{stem}: Examples() = {examples:?}, want error {want}")
            }
            (Err(err), None) => panic!("{stem}: Examples() error = {err}"),
        }
    }
}
