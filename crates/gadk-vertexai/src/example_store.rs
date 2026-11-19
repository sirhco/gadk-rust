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

//! A [`Provider`] backed by a Vertex AI Example Store.
//!
//! [`new_provider`] builds a provider for an example tool
//! ([`gadk_tools::example::ExampleToolConfig::provider`]): for each user query it searches the
//! store for the [`TOP_K`] most similar stored-contents examples and keeps those scoring at
//! least [`SIMILARITY_THRESHOLD`].
//!
//! ```no_run
//! # fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use gadk_tools::example::{self, ExampleToolConfig};
//! use gadk_vertexai::example_store::{ProviderConfig, new_provider};
//!
//! let provider = new_provider(ProviderConfig {
//!     example_store_name: "projects/my-project/locations/us-central1/exampleStores/123".into(),
//!     ..Default::default()
//! })?;
//! let tool = example::new(ExampleToolConfig {
//!     provider: Some(provider),
//!     ..Default::default()
//! })?;
//! # let _ = tool;
//! # Ok(()) }
//! ```

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use gadk_agent::{CancelCause, Context, ToolError};
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    Language, Outcome, Part, ROLE_USER, VideoMetadata,
};
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_tools::example::{Example, Provider};
use google_cloud_auth::credentials::Credentials;
use http::Method;
use regex::Regex;
use serde_json::{Value, json};

use crate::client::VertexClient;
use crate::config::{host_url, resolve};
use crate::error::{BoxError, wrap};
use crate::protojson::{language_name, outcome_name, parse_duration};
use crate::session::convert::{
    array_field, bool_field, bytes_field, check_bytes, field, float32_field, str_field,
    struct_as_map,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

/// `testdata/example_store/{stem}.json`, the golden bodies shared by the wire tests.
#[cfg(test)]
#[allow(clippy::panic)]
pub(crate) fn golden(stem: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/example_store")
        .join(format!("{stem}.json"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// How many examples a search asks for.
pub const TOP_K: i64 = 10;

/// Results scoring below this are dropped: below it an example is
/// more likely to mislead the model than help it.
pub const SIMILARITY_THRESHOLD: f32 = 0.5;

/// The shape of a full Example Store resource name.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static EXAMPLE_STORE_NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^projects/([a-zA-Z0-9._:-]+)/locations/([a-zA-Z0-9._-]+)/exampleStores/([a-zA-Z0-9._-]+)$",
    )
    .expect("valid example-store regex")
});

/// How a Vertex AI Example Store provider reaches its store: the store name, plus the
/// `endpoint`, `credentials`, `without_authentication`, `http_client` and `env` settings of
/// the underlying client.
#[derive(Clone)]
pub struct ProviderConfig {
    /// The store's full resource name:
    /// `projects/{project}/locations/{location}/exampleStores/{example_store}`. A bare store ID
    /// is refused (no `GOOGLE_CLOUD_PROJECT` / `GOOGLE_CLOUD_LOCATION` fallback).
    pub example_store_name: String,
    /// The base URL; `None` = `https://{location}-aiplatform.googleapis.com`
    /// (`https://aiplatform.googleapis.com` for the `global` location), the location taken
    /// from [`example_store_name`](Self::example_store_name).
    pub endpoint: Option<String>,
    /// The credentials sent with every request; `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the
    /// `cloud-platform` scope.
    pub credentials: Option<Credentials>,
    /// Send no credentials.
    pub without_authentication: bool,
    /// The HTTP client; `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Where `GOOGLE_APPLICATION_CREDENTIALS` is read.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            example_store_name: String::new(),
            endpoint: None,
            credentials: None,
            without_authentication: false,
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("example_store_name", &self.example_store_name)
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// Why [`new_provider`] failed.
#[non_exhaustive]
#[derive(Debug)]
pub enum ExampleStoreError {
    /// The store name is not a full Example Store resource name.
    InvalidStoreName,
    /// The client could not be built: Application Default Credentials failed.
    CreateClient(BoxError),
}

impl std::fmt::Display for ExampleStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidStoreName => f.write_str(
                "vertexai: example_store_name must match projects/{project}/locations/{location}/exampleStores/{example_store}",
            ),
            Self::CreateClient(e) => write!(f, "vertexai: create example store client: {e}"),
        }
    }
}

impl std::error::Error for ExampleStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidStoreName => None,
            Self::CreateClient(e) => Some(e.as_ref()),
        }
    }
}

/// A provider that retrieves few-shot examples from a Vertex AI Example Store. Synchronous;
/// only Application Default Credentials need a Tokio runtime. The provider has no `close`:
/// the REST client holds no connection.
///
/// # Errors
///
/// [`ExampleStoreError::InvalidStoreName`] when `example_store_name` is not a full Example
/// Store resource name; [`ExampleStoreError::CreateClient`] when Application Default
/// Credentials cannot be built.
pub fn new_provider(config: ProviderConfig) -> Result<Arc<dyn Provider>, ExampleStoreError> {
    let location = parse_location(&config)?;
    // The default endpoint is the location's host, served over HTTPS.
    let default_endpoint = format!("https://{}", host_url(&location));
    let resolved = resolve(
        config.endpoint.as_deref(),
        &default_endpoint,
        config.credentials.clone(),
        config.without_authentication,
        config.http_client.clone(),
        config.env.as_ref(),
    )
    .map_err(ExampleStoreError::CreateClient)?;
    new_provider_with_client(
        &config,
        Arc::new(RestExampleStoreClient {
            client: VertexClient::new(resolved),
        }),
    )
}

/// A provider for `config` searching through `client`.
pub(crate) fn new_provider_with_client(
    config: &ProviderConfig,
    client: Arc<dyn ExampleStoreClient>,
) -> Result<Arc<dyn Provider>, ExampleStoreError> {
    parse_location(config)?;
    Ok(Arc::new(VertexExampleStoreProvider {
        store_name: config.example_store_name.clone(),
        client,
    }))
}

/// The `{location}` of a full Example Store resource name.
pub(crate) fn parse_location(config: &ProviderConfig) -> Result<String, ExampleStoreError> {
    EXAMPLE_STORE_NAME_PATTERN
        .captures(&config.example_store_name)
        .and_then(|c| c.get(2))
        .map(|m| m.as_str().to_owned())
        .ok_or(ExampleStoreError::InvalidStoreName)
}

/// One `SearchExamples` call, taking and returning protojson bodies.
#[async_trait]
pub(crate) trait ExampleStoreClient: Send + Sync {
    /// Searches the store named by `req`'s `exampleStore`.
    async fn search_examples(&self, req: &Value) -> Result<Value, BoxError>;
}

/// The REST `ExampleStoreService.SearchExamples` call.
// Example Store is called over REST v1beta1, not gRPC.
struct RestExampleStoreClient {
    client: VertexClient,
}

#[async_trait]
impl ExampleStoreClient for RestExampleStoreClient {
    /// `POST {base}/v1beta1/{exampleStore}:searchExamples?$alt=json;enum-encoding=int` with
    /// `x-goog-request-params: example_store={exampleStore}`.
    async fn search_examples(&self, req: &Value) -> Result<Value, BoxError> {
        let store = str_field(req, "exampleStore");
        self.client
            .call(
                Method::POST,
                &format!("/v1beta1/{store}:searchExamples"),
                &[],
                ("example_store", store),
                Some(req),
            )
            .await
    }
}

/// The provider that searches the store.
// There is no `close`: REST holds no connection.
struct VertexExampleStoreProvider {
    store_name: String,
    client: Arc<dyn ExampleStoreClient>,
}

#[async_trait]
impl Provider for VertexExampleStoreProvider {
    /// Searches the store with `query` as a user content and the
    /// `lastEntry` search-key method, then maps the results.
    ///
    /// # Errors
    ///
    /// [`ToolError::Execution`] with the text `vertexai: search examples: {err}` when the call
    /// fails or the response holds malformed `bytes` or `Duration` text (protojson decoding of
    /// the response fails), or `vertexai: search examples: {cause}` (wrapping
    /// [`CancelCause::to_agent_error`]) when `ctx` is cancelled before or during the call (the
    /// request is dropped).
    async fn examples(&self, ctx: &Context, query: &str) -> Result<Vec<Example>, ToolError> {
        let req = search_request(&self.store_name, query);
        let token = ctx.cancellation_token();
        let cancelled = || -> BoxError {
            Box::new(
                ctx.cause()
                    .unwrap_or(CancelCause::Canceled)
                    .to_agent_error(),
            )
        };
        let result = if token.is_cancelled() {
            Err(cancelled())
        } else {
            tokio::select! {
                biased;
                () = token.cancelled() => Err(cancelled()),
                resp = self.client.search_examples(&req) => resp,
            }
        };
        result
            .and_then(|resp| examples_from_response(&resp).map_err(BoxError::from))
            .map_err(|e| ToolError::Execution(wrap("vertexai: search examples", e)))
    }
}

/// A `SearchExamplesRequest` in protojson: `topK` is an int64, so a JSON string.
pub(crate) fn search_request(store_name: &str, query: &str) -> Value {
    json!({
        "exampleStore": store_name,
        "topK": TOP_K.to_string(),
        "storedContentsExampleParameters": {
            "contentSearchKey": {
                "contents": [{"role": ROLE_USER, "parts": [{"text": query}]}],
                "searchKeyGenerationMethod": {"lastEntry": {}},
            },
        },
    })
}

/// Maps a protojson `SearchExamplesResponse` to examples: results below
/// [`SIMILARITY_THRESHOLD`] or without a `storedContentsExample` are skipped; the input is
/// the search key as a user text content, the output each expected content that has a
/// `content`, converted. An empty response gives no examples.
///
/// # Errors
///
/// protojson's text for a malformed `bytes` field (`thoughtSignature`, `inlineData.data`) or
/// `Duration` (`videoMetadata` offsets) in any content of any result, skipped ones included:
/// the whole response is decoded before any result is examined.
pub(crate) fn examples_from_response(resp: &Value) -> Result<Vec<Example>, String> {
    let mut examples = Vec::new();
    for result in array_field(resp, "results") {
        let stored = result
            .get("example")
            .and_then(|e| field(e, "storedContentsExample"));
        let mut output = Vec::new();
        if let Some(stored) = stored {
            let contents_example = &stored["contentsExample"];
            for content in array_field(contents_example, "contents") {
                convert_content(content)?;
            }
            for expected in array_field(contents_example, "expectedContents") {
                if let Some(content) = field(expected, "content") {
                    output.push(convert_content(content)?);
                }
            }
        }
        if float32_field(result, "similarityScore") < SIMILARITY_THRESHOLD {
            continue;
        }
        let Some(stored) = stored else {
            continue;
        };
        examples.push(Example {
            input: Content::from_text(str_field(stored, "searchKey"), ROLE_USER),
            output,
        });
    }
    Ok(examples)
}

/// The role, and each part converted by [`convert_part`].
fn convert_content(src: &Value) -> Result<Content, String> {
    let mut parts = Vec::new();
    for part in array_field(src, "parts") {
        if let Some(part) = convert_part(part)? {
            parts.push(part);
        }
    }
    Ok(Content {
        role: str_field(src, "role").to_owned(),
        parts,
    })
}

/// Converts one part: thought, thought signature and video metadata, then the one set data
/// field (enums by their protobuf names). A part with no data is dropped unless it has a
/// thought, a thought signature or video metadata; a `null` part is dropped. Malformed
/// `bytes` and `Duration` text fails with protojson's text.
fn convert_part(src: &Value) -> Result<Option<Part>, String> {
    if src.is_null() {
        return Ok(None);
    }
    check_bytes(src, "thoughtSignature")?;
    let mut part = Part {
        thought: bool_field(src, "thought"),
        thought_signature: bytes_field(src, "thoughtSignature").into(),
        ..Default::default()
    };
    if let Some(vm) = field(src, "videoMetadata") {
        part.video_metadata = Some(VideoMetadata {
            start_offset: duration_field(vm, "startOffset")?,
            end_offset: duration_field(vm, "endOffset")?,
            fps: None,
        });
    }
    if let Some(text) = field(src, "text") {
        part.text = text.as_str().unwrap_or_default().to_owned();
    } else if let Some(fc) = field(src, "functionCall") {
        part.function_call = Some(FunctionCall {
            id: str_field(fc, "id").to_owned(),
            name: str_field(fc, "name").to_owned(),
            args: struct_as_map(field(fc, "args")),
            ..Default::default()
        });
    } else if let Some(fr) = field(src, "functionResponse") {
        part.function_response = Some(FunctionResponse {
            id: str_field(fr, "id").to_owned(),
            name: str_field(fr, "name").to_owned(),
            response: struct_as_map(field(fr, "response")),
            ..Default::default()
        });
    } else if let Some(blob) = field(src, "inlineData") {
        check_bytes(blob, "data")?;
        part.inline_data = Some(Blob {
            mime_type: str_field(blob, "mimeType").to_owned(),
            data: bytes_field(blob, "data").into(),
            ..Default::default()
        });
    } else if let Some(fd) = field(src, "fileData") {
        part.file_data = Some(FileData {
            mime_type: str_field(fd, "mimeType").to_owned(),
            file_uri: str_field(fd, "fileUri").to_owned(),
            ..Default::default()
        });
    } else if let Some(ec) = field(src, "executableCode") {
        let name = language_name(ec.get("language").unwrap_or(&Value::Null));
        part.executable_code = Some(ExecutableCode {
            code: str_field(ec, "code").to_owned(),
            language: Some(name.parse::<Language>().unwrap_or_else(|e| match e {})),
            ..Default::default()
        });
    } else if let Some(cer) = field(src, "codeExecutionResult") {
        let name = outcome_name(cer.get("outcome").unwrap_or(&Value::Null));
        part.code_execution_result = Some(CodeExecutionResult {
            outcome: Some(name.parse::<Outcome>().unwrap_or_else(|e| match e {})),
            output: str_field(cer, "output").to_owned(),
            ..Default::default()
        });
    } else if !part.thought && part.thought_signature.is_empty() && part.video_metadata.is_none() {
        return Ok(None);
    }
    Ok(Some(part))
}

/// A protojson `google.protobuf.Duration` field as a [`chrono::TimeDelta`]; an absent or `null`
/// one is zero.
fn duration_field(v: &Value, key: &str) -> Result<chrono::TimeDelta, String> {
    field(v, key).map_or(Ok(chrono::TimeDelta::zero()), parse_duration)
}
