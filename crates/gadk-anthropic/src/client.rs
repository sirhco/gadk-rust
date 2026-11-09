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

//! The Messages transport: endpoints, request headers, and the request body, sent through
//! [`gadk_model::rest`] with anthropic-sdk-go v1.81.0's retry policy and per-attempt timeouts
//! (port of `RequestConfig.Execute`'s Anthropic specifics and `vertex.WithCredentials`'s URL
//! rules).

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use gadk_model::gcp_auth::credential_headers;
use gadk_model::rest::{
    CallTimeout, RequestHeaders, RestError, RestReply, RestTransport, header_str,
};
use gadk_model::{BodyStream, ModelError};
use google_cloud_auth::credentials::Credentials;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Uri};

use crate::error::{AnthropicError, api_error_from_response, go_quote};
use crate::wire::{MessageNewParams, VertexBody, encode_request};

/// The Anthropic backend's default base URL (SDK `WithEnvironmentProduction`).
pub(crate) const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/";

/// The `Anthropic-Version` header value.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Go's default client `ResponseHeaderTimeout` (`defaultResponseHeaderTimeout`).
pub(crate) use gadk_model::rest::DEFAULT_RESPONSE_HEADER_TIMEOUT;

/// The Messages API path, relative to the base URL (SDK `"v1/messages"`).
const MESSAGES_PATH: &str = "v1/messages";

/// Where a model's requests go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// `POST {base}/v1/messages`.
    Anthropic,
    /// `POST {base}/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:rawPredict`
    /// (`:streamRawPredict` when streaming).
    Vertex {
        /// The Google Cloud project.
        project: String,
        /// The Google Cloud location.
        location: String,
    },
}

/// How requests authenticate.
#[derive(Clone)]
pub(crate) enum Auth {
    /// Anthropic: `x-api-key` and/or `Authorization: Bearer` (both when both are set, as Go's
    /// SDK sends both).
    Anthropic {
        /// The API key.
        api_key: Option<String>,
        /// The bearer token.
        auth_token: Option<String>,
    },
    /// Vertex: the credentials' headers, fetched for every attempt.
    Vertex(Credentials),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic { .. } => f.write_str("Anthropic(<redacted>)"),
            Self::Vertex(_) => f.write_str("Vertex(<credentials>)"),
        }
    }
}

/// SDK `vertex.WithCredentials`'s base URL for `location`: `global` →
/// `https://aiplatform.googleapis.com/`, `us` → `https://aiplatform.us.rep.googleapis.com/`,
/// `eu` → `https://aiplatform.eu.rep.googleapis.com/`, else
/// `https://{location}-aiplatform.googleapis.com/`.
pub(crate) fn vertex_base_url(location: &str) -> String {
    match location {
        "global" => "https://aiplatform.googleapis.com/".to_string(),
        "us" => "https://aiplatform.us.rep.googleapis.com/".to_string(),
        "eu" => "https://aiplatform.eu.rep.googleapis.com/".to_string(),
        region => format!("https://{region}-aiplatform.googleapis.com/"),
    }
}

/// `v1/messages` resolved against `base_url` as the SDK does: `option.WithBaseURL` gives a
/// non-empty path a trailing `/`, then `url.Parse` + `ResolveReference` keep the base's path as
/// a prefix and drop its query and fragment.
fn messages_url(base_url: &str) -> Result<url::Url, AnthropicError> {
    let parse_err = |e: url::ParseError| {
        AnthropicError::request_failed(format!(
            "requestoption: WithBaseURL failed to parse url {e}"
        ))
    };
    let mut base = url::Url::parse(base_url).map_err(parse_err)?;
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base.join(MESSAGES_PATH).map_err(parse_err)
}

/// Whether the SDK's Vertex middleware rewrites a request on `url`: only when its path is
/// exactly `/v1/messages` (`r.URL.Path == "/v1/messages"`), which a base URL with a path prefix
/// never gives.
fn vertex_rewrites(url: &url::Url) -> bool {
    url.path() == "/v1/messages"
}

/// The request URL: the endpoint's API path joined to `base_url` with exactly one `/`
/// (a base URL's own path is kept as a prefix).
///
/// On Vertex the SDK sends `v1/messages` through its middleware, which rewrites the path to
/// `/v1/projects/{p}/locations/{l}/publishers/anthropic/models/{model}:rawPredict`
/// (`:streamRawPredict` when streaming) only when the resolved path is exactly `/v1/messages`;
/// with a base URL that carries a path prefix the request stays at `{prefix}/v1/messages`
/// (and [`Transport::encode_body`] keeps `model` in the body), as in Go.
pub(crate) fn request_url(
    base_url: &str,
    endpoint: &Endpoint,
    model_id: &str,
    stream: bool,
) -> Result<String, AnthropicError> {
    let mut url = messages_url(base_url)?;
    if let Endpoint::Vertex { project, location } = endpoint
        && vertex_rewrites(&url)
    {
        let specifier = if stream {
            "streamRawPredict"
        } else {
            "rawPredict"
        };
        url.set_path(&format!(
            "/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model_id}:{specifier}"
        ));
    }
    Ok(url.into())
}

/// A header value marked sensitive; an invalid one fails the attempt as Go's `net/http` does
/// (a transport error).
fn sensitive_value(name: &str, value: &str) -> Result<HeaderValue, AnthropicError> {
    let mut v = HeaderValue::from_str(value).map_err(|_| {
        AnthropicError::transport(format!(
            "net/http: invalid header field value for {}",
            go_quote(name)
        ))
    })?;
    v.set_sensitive(true);
    Ok(v)
}

/// A streaming body's [`RestError`] (the attempt's deadline passing) becomes the
/// [`AnthropicError`] the rest of the crate expects: `ModelError::Other(AnthropicError)`.
fn anthropic_body_error(err: ModelError) -> ModelError {
    match err {
        ModelError::Other(boxed) => match boxed.downcast::<RestError>() {
            Ok(rest) => AnthropicError::from(*rest).into(),
            Err(other) => ModelError::Other(other),
        },
        other => other,
    }
}

/// A model's transport: everything needed to send one Messages request.
#[derive(Clone)]
pub(crate) struct Transport {
    /// The HTTP client, the retry policy ([`gadk_model::rest::RetryPolicy::ANTHROPIC`] with
    /// `ClientConfig.max_retries`), and the response-header timeout
    /// ([`DEFAULT_RESPONSE_HEADER_TIMEOUT`] when gadk built the client, `None` for a custom
    /// one; the credential fetch before each send is outside it, as in Go).
    pub(crate) rest: RestTransport,
    /// The effective base URL (custom, `ANTHROPIC_BASE_URL`, or the backend default).
    pub(crate) base_url: String,
    /// The endpoint shape.
    pub(crate) endpoint: Endpoint,
    /// The authentication.
    pub(crate) auth: Auth,
    /// `ClientConfig.headers`, added after the headers gadk sets.
    pub(crate) headers: HeaderMap,
}

/// Every attempt's headers are [`Transport::request_headers`]; its error travels boxed in
/// [`RestError::Headers`] and comes back unchanged.
#[async_trait::async_trait]
impl RequestHeaders for Transport {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        self.request_headers()
            .await
            .map_err(|e| RestError::Headers(Box::new(e)))
    }
}

impl Transport {
    /// The headers of one attempt, in Go's precedence (a later step replaces an earlier one's
    /// values for the same name):
    /// 1. `Accept: application/json`, `Anthropic-Version`, `Content-Type: application/json`;
    /// 2. `user-agent` set to `gadk_platform::tracking_header()` (the Go SDK sends
    ///    `Anthropic/Go 1.81.0`; ADK-Go overrides it only on Vertex); Anthropic: `x-api-key` and/or `Authorization: Bearer` (SDK
    ///    `WithAPIKey` / `WithAuthToken`); Vertex: `x-goog-api-client` set to the tracking value
    ///    too;
    /// 3. [`headers`](Self::headers) (Go's `option.WithHeader`, applied after the above);
    /// 4. Vertex: the credentials' headers, fetched now (Go's `oauth2.Transport` sets
    ///    `Authorization` on the outgoing request last).
    pub(crate) async fn request_headers(&self) -> Result<HeaderMap, AnthropicError> {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let tracking = HeaderValue::from_str(&gadk_platform::tracking_header())
            .map_err(|e| AnthropicError::transport(e.to_string()))?;
        h.insert(USER_AGENT, tracking.clone());
        let mut user = self.headers.clone();
        for v in user.values_mut() {
            v.set_sensitive(true);
        }
        match &self.auth {
            Auth::Anthropic {
                api_key,
                auth_token,
            } => {
                if let Some(key) = api_key {
                    h.insert("x-api-key", sensitive_value("X-Api-Key", key)?);
                }
                if let Some(token) = auth_token {
                    h.insert(
                        AUTHORIZATION,
                        sensitive_value("Authorization", &format!("Bearer {token}"))?,
                    );
                }
                h.extend(user);
            }
            Auth::Vertex(credentials) => {
                h.insert("x-goog-api-client", tracking);
                h.extend(user);
                // A failure is a transport error whose detail is google-cloud-auth's error
                // text (Go's is `oauth2`'s), a ledgered divergence.
                h.extend(
                    credential_headers(credentials)
                        .await
                        .map_err(AnthropicError::transport)?,
                );
            }
        }
        Ok(h)
    }

    /// The request body the SDK sends for `params` (whose `model` is set): [`encode_request`],
    /// on Vertex dropping `model` only when the middleware rewrites the path (see
    /// [`request_url`]); with a base URL whose path it does not rewrite `model` stays and only
    /// `,"anthropic_version":"vertex-2023-10-16"` is appended, as the SDK's middleware does.
    pub(crate) fn encode_body(
        &self,
        params: MessageNewParams,
        stream: bool,
    ) -> Result<Bytes, AnthropicError> {
        let vertex = match &self.endpoint {
            Endpoint::Anthropic => None,
            Endpoint::Vertex { .. } if vertex_rewrites(&messages_url(&self.base_url)?) => {
                Some(VertexBody::DropModel)
            }
            Endpoint::Vertex { .. } => Some(VertexBody::KeepModel),
        };
        encode_request(params, stream, vertex)
            .map(Bytes::from)
            .map_err(AnthropicError::request_failed)
    }

    /// Sends `body` with anthropic-sdk-go's retry policy ([`RestTransport::send`] with
    /// [`CallTimeout::PerAttempt`] for `timeout`) and returns the response that ends the call.
    ///
    /// - A connection error (including a credential failure and the response-header timeout),
    ///   or `timeout` (> 0, per attempt) expiring, is retried like a connection error;
    ///   [`AnthropicError::deadline_exceeded`] surfaces only when the last attempt timed out.
    /// - A body that fails to read is retried per `shouldRetryDroppedBody`, else returned as
    ///   `anthropicmodel: request failed: error reading response body: …`.
    /// - A status ≥ 400 that ends the call is returned as `Err` with
    ///   [`crate::error::api_error_from_response`] (header `request-id`).
    /// - A non-streaming 2xx is returned with its whole body as one chunk; a streaming 2xx with
    ///   its live body, still under the attempt's `timeout` (an expiry yields
    ///   `ModelError::Other(AnthropicError::deadline_exceeded())`).
    /// - Every await is cancelled by dropping the future (no task is spawned).
    pub(crate) async fn send(
        &self,
        model_id: &str,
        body: Bytes,
        stream: bool,
        timeout: Option<Duration>,
    ) -> Result<http::Response<BodyStream>, AnthropicError> {
        let url = request_url(&self.base_url, &self.endpoint, model_id, stream)?;
        let uri: Uri = url.parse().map_err(AnthropicError::request_failed)?;
        let timeout = timeout.map_or(CallTimeout::None, CallTimeout::PerAttempt);
        match self.rest.send(&uri, body, stream, self, timeout).await? {
            RestReply::Read(parts, bytes) => {
                if parts.status.as_u16() >= 400 {
                    return Err(AnthropicError::api(api_error_from_response(
                        parts.status.as_u16(),
                        header_str(&parts.headers, "request-id"),
                        &bytes,
                    )));
                }
                let body: BodyStream = futures::stream::iter([Ok(bytes)]).boxed();
                Ok(http::Response::from_parts(parts, body))
            }
            RestReply::Streaming(res) => {
                Ok(res.map(|b| b.map(|chunk| chunk.map_err(anthropic_body_error)).boxed()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gadk_model::rest::RetryPolicy;
    use gadk_model::rest::test_util::{FakeResponse, ScriptedClient as Fake};
    use google_cloud_auth::credentials::{CacheableResource, CredentialsProvider, EntityTag};
    use google_cloud_auth::errors::CredentialsError;
    use http::StatusCode;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::error::ErrorKind;

    // ------------------------------------------------------------ scripted fake HttpClient

    /// A JSON reply (`gadk_model::rest::test_util`'s scripted fake).
    fn reply(status: u16, body: &'static str) -> FakeResponse {
        FakeResponse::json_status(status, body)
    }

    /// A credentials provider returning fixed headers, or failing; counts its calls.
    #[derive(Debug)]
    struct TestCreds {
        headers: Option<HeaderMap>,
        calls: Arc<AtomicUsize>,
    }

    impl CredentialsProvider for TestCreds {
        async fn headers(
            &self,
            _extensions: http::Extensions,
        ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.headers {
                Some(h) => Ok(CacheableResource::New {
                    entity_tag: EntityTag::new(),
                    data: h.clone(),
                }),
                None => Err(CredentialsError::from_msg(false, "boom")),
            }
        }

        async fn universe_domain(&self) -> Option<String> {
            None
        }
    }

    fn vertex_creds(headers: Option<HeaderMap>) -> (Credentials, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let creds = Credentials::from(TestCreds {
            headers,
            calls: calls.clone(),
        });
        (creds, calls)
    }

    fn bearer(token: &'static str) -> HeaderMap {
        HeaderMap::from_iter([(AUTHORIZATION, HeaderValue::from_static(token))])
    }

    fn rest(http: Arc<Fake>, max_retries: u32) -> RestTransport {
        RestTransport {
            http,
            policy: RetryPolicy {
                max_retries,
                ..RetryPolicy::ANTHROPIC
            },
            response_header_timeout: None,
        }
    }

    fn anthropic(http: Arc<Fake>, max_retries: u32) -> Transport {
        Transport {
            rest: rest(http, max_retries),
            base_url: ANTHROPIC_DEFAULT_BASE_URL.to_string(),
            endpoint: Endpoint::Anthropic,
            auth: Auth::Anthropic {
                api_key: Some("sk-test".to_string()),
                auth_token: None,
            },
            headers: HeaderMap::new(),
        }
    }

    fn vertex(http: Arc<Fake>, credentials: Credentials, max_retries: u32) -> Transport {
        Transport {
            rest: rest(http, max_retries),
            base_url: vertex_base_url("us-east5"),
            endpoint: Endpoint::Vertex {
                project: "p".to_string(),
                location: "us-east5".to_string(),
            },
            auth: Auth::Vertex(credentials),
            headers: HeaderMap::new(),
        }
    }

    const BODY: &[u8] = br#"{"max_tokens":16,"model":"claude-x"}"#;
    const OK: &str = r#"{"id":"msg_1"}"#;
    const BAD: &str =
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad thing"}}"#;
    const BOOM: &str = r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#;

    async fn send(
        t: &Transport,
        stream: bool,
        timeout: Option<Duration>,
    ) -> Result<Bytes, AnthropicError> {
        let res = t
            .send("claude-x", Bytes::from_static(BODY), stream, timeout)
            .await?;
        Ok(gadk_model::collect_body(res.into_body()).await.unwrap())
    }

    fn sorted(h: &HeaderMap) -> Vec<(String, String)> {
        let mut v: Vec<_> = h
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect();
        v.sort();
        v
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<_> = p
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        v.sort();
        v
    }

    // ------------------------------------------------------------ endpoints

    #[test]
    fn vertex_base_url_hosts() {
        let cases = [
            ("global", "https://aiplatform.googleapis.com/"),
            ("us", "https://aiplatform.us.rep.googleapis.com/"),
            ("eu", "https://aiplatform.eu.rep.googleapis.com/"),
            ("us-east5", "https://us-east5-aiplatform.googleapis.com/"),
            (
                "europe-west1",
                "https://europe-west1-aiplatform.googleapis.com/",
            ),
        ];
        for (location, want) in cases {
            assert_eq!(vertex_base_url(location), want, "{location}");
        }
    }

    #[test]
    fn request_url_cases() {
        let vertex = Endpoint::Vertex {
            project: "proj".to_string(),
            location: "us-east5".to_string(),
        };
        let global = Endpoint::Vertex {
            project: "proj".to_string(),
            location: "global".to_string(),
        };
        let cases: [(&str, &str, &Endpoint, bool, &str); 13] = [
            (
                "anthropic_default",
                ANTHROPIC_DEFAULT_BASE_URL,
                &Endpoint::Anthropic,
                false,
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "anthropic_streaming_same_path",
                ANTHROPIC_DEFAULT_BASE_URL,
                &Endpoint::Anthropic,
                true,
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "anthropic_host_without_slash",
                "http://127.0.0.1:8080",
                &Endpoint::Anthropic,
                false,
                "http://127.0.0.1:8080/v1/messages",
            ),
            (
                "anthropic_prefix_without_slash",
                "https://proxy.example/anthropic",
                &Endpoint::Anthropic,
                false,
                "https://proxy.example/anthropic/v1/messages",
            ),
            (
                "anthropic_prefix_with_slash",
                "https://proxy.example/anthropic/",
                &Endpoint::Anthropic,
                false,
                "https://proxy.example/anthropic/v1/messages",
            ),
            (
                "anthropic_query_and_fragment_dropped",
                "https://proxy.example/a?x=1#f",
                &Endpoint::Anthropic,
                false,
                "https://proxy.example/a/v1/messages",
            ),
            (
                "vertex_regional",
                "https://us-east5-aiplatform.googleapis.com/",
                &vertex,
                false,
                "https://us-east5-aiplatform.googleapis.com/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:rawPredict",
            ),
            (
                "vertex_regional_streaming",
                "https://us-east5-aiplatform.googleapis.com/",
                &vertex,
                true,
                "https://us-east5-aiplatform.googleapis.com/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict",
            ),
            (
                "vertex_global",
                "https://aiplatform.googleapis.com/",
                &global,
                false,
                "https://aiplatform.googleapis.com/v1/projects/proj/locations/global/publishers/anthropic/models/claude-x:rawPredict",
            ),
            (
                "vertex_custom_host",
                "http://127.0.0.1:9000",
                &vertex,
                false,
                "http://127.0.0.1:9000/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:rawPredict",
            ),
            (
                "vertex_custom_host_with_slash",
                "http://127.0.0.1:9000/",
                &vertex,
                true,
                "http://127.0.0.1:9000/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict",
            ),
            // R-a: the SDK's Vertex middleware rewrites only the path `/v1/messages`, so a
            // prefixed base URL keeps `{prefix}/v1/messages` (and `model` in the body).
            (
                "vertex_prefixed_base_url_is_not_rewritten",
                "https://proxy.example/vertex",
                &vertex,
                false,
                "https://proxy.example/vertex/v1/messages",
            ),
            (
                "vertex_prefixed_base_url_streaming_is_not_rewritten",
                "https://proxy.example/vertex/",
                &vertex,
                true,
                "https://proxy.example/vertex/v1/messages",
            ),
        ];
        for (name, base, endpoint, stream, want) in cases {
            let got = request_url(base, endpoint, "claude-x", stream).unwrap();
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn request_url_rejects_an_unparsable_base_url() {
        // The text is gadk's (shaped like the SDK's `WithBaseURL` error): Go's url.Parse accepts
        // "not a url" as a relative URL, so Go has no counterpart for this input.
        let err = request_url("not a url", &Endpoint::Anthropic, "m", false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: requestoption: WithBaseURL failed to parse url relative URL without a base"
        );
    }

    #[test]
    fn encode_body_per_endpoint() {
        let params = MessageNewParams {
            max_tokens: 16,
            model: "claude-x".to_string(),
            ..Default::default()
        };
        let fake = Fake::sequence(vec![reply(200, OK)]);
        let (creds, _) = vertex_creds(Some(HeaderMap::new()));
        let anthropic_t = anthropic(fake.clone(), 0);
        let vertex_t = vertex(fake.clone(), creds.clone(), 0);
        let mut custom_host = vertex(fake.clone(), creds.clone(), 0);
        custom_host.base_url = "http://127.0.0.1:9000".to_string();
        let mut prefixed = vertex(fake, creds, 0);
        prefixed.base_url = "https://proxy.example/vertex".to_string();
        let cases: [(&str, &Transport, bool, &str); 7] = [
            (
                "anthropic",
                &anthropic_t,
                false,
                r#"{"max_tokens":16,"model":"claude-x"}"#,
            ),
            (
                "anthropic_stream",
                &anthropic_t,
                true,
                r#"{"max_tokens":16,"model":"claude-x","stream":true}"#,
            ),
            (
                "vertex",
                &vertex_t,
                false,
                r#"{"max_tokens":16,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_stream",
                &vertex_t,
                true,
                r#"{"max_tokens":16,"stream":true,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_custom_host",
                &custom_host,
                false,
                r#"{"max_tokens":16,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_prefixed_keeps_model",
                &prefixed,
                false,
                r#"{"max_tokens":16,"model":"claude-x","anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_prefixed_stream_keeps_model",
                &prefixed,
                true,
                r#"{"max_tokens":16,"model":"claude-x","stream":true,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
        ];
        for (name, t, stream, want) in cases {
            let got = t.encode_body(params.clone(), stream).unwrap();
            assert_eq!(std::str::from_utf8(&got).unwrap(), want, "{name}");
        }
    }

    // ------------------------------------------------------------ headers

    /// Header name/value pairs.
    type Pairs = &'static [(&'static str, &'static str)];

    const BASE_HEADERS: [(&str, &str); 3] = [
        ("accept", "application/json"),
        ("anthropic-version", "2023-06-01"),
        ("content-type", "application/json"),
    ];

    #[tokio::test]
    async fn request_headers_anthropic_auth() {
        let cases: [(&str, Option<&str>, Option<&str>, Pairs); 4] = [
            ("api_key", Some("sk-1"), None, &[("x-api-key", "sk-1")]),
            (
                "auth_token",
                None,
                Some("tok"),
                &[("authorization", "Bearer tok")],
            ),
            (
                "both_headers_when_both_set",
                Some("sk-1"),
                Some("tok"),
                &[("x-api-key", "sk-1"), ("authorization", "Bearer tok")],
            ),
            ("neither", None, None, &[]),
        ];
        for (name, api_key, auth_token, auth) in cases {
            let mut t = anthropic(Fake::sequence(vec![reply(200, OK)]), 0);
            t.auth = Auth::Anthropic {
                api_key: api_key.map(str::to_string),
                auth_token: auth_token.map(str::to_string),
            };
            let got = t.request_headers().await.unwrap();
            let tracking = gadk_platform::tracking_header();
            let mut want = BASE_HEADERS.to_vec();
            want.extend_from_slice(auth);
            // gadk's User-Agent (the Go SDK sends `Anthropic/Go 1.81.0` here).
            want.push(("user-agent", tracking.as_str()));
            assert_eq!(sorted(&got), pairs(&want), "{name}");
            for name in ["x-api-key", "authorization"] {
                if let Some(v) = got.get(name) {
                    assert!(v.is_sensitive(), "{name} must be sensitive");
                }
            }
        }
    }

    #[tokio::test]
    async fn request_headers_user_headers_replace_and_add() {
        let mut t = anthropic(Fake::sequence(vec![reply(200, OK)]), 0);
        t.headers
            .append("anthropic-version", HeaderValue::from_static("2099-01-01"));
        t.headers.append("x-extra", HeaderValue::from_static("a"));
        t.headers.append("x-extra", HeaderValue::from_static("b"));
        t.headers
            .append("x-api-key", HeaderValue::from_static("override"));
        t.headers
            .insert(USER_AGENT, HeaderValue::from_static("mine"));
        let got = t.request_headers().await.unwrap();
        assert_eq!(
            sorted(&got),
            pairs(&[
                ("accept", "application/json"),
                ("anthropic-version", "2099-01-01"),
                ("content-type", "application/json"),
                ("user-agent", "mine"),
                ("x-api-key", "override"),
                ("x-extra", "a"),
                ("x-extra", "b"),
            ])
        );
        assert!(
            got.get_all("x-extra").iter().all(HeaderValue::is_sensitive),
            "config header values are sensitive"
        );
    }

    #[tokio::test]
    async fn request_headers_invalid_api_key_is_a_transport_error() {
        let mut t = anthropic(Fake::sequence(vec![reply(200, OK)]), 0);
        t.auth = Auth::Anthropic {
            api_key: Some("a\nb".to_string()),
            auth_token: None,
        };
        let err = t.request_headers().await.unwrap_err();
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            r#"anthropicmodel: transport error: net/http: invalid header field value for "X-Api-Key""#
        );
    }

    #[tokio::test]
    async fn request_headers_vertex() {
        let mut creds_headers = bearer("Bearer vertex-token");
        creds_headers.insert("x-goog-user-project", HeaderValue::from_static("quota"));
        let (creds, calls) = vertex_creds(Some(creds_headers));
        let mut t = vertex(Fake::sequence(vec![reply(200, OK)]), creds, 0);
        t.headers.insert("x-extra", HeaderValue::from_static("1"));
        let got = t.request_headers().await.unwrap();
        let tracking = gadk_platform::tracking_header();
        let mut want = BASE_HEADERS.to_vec();
        want.extend_from_slice(&[
            ("authorization", "Bearer vertex-token"),
            ("x-goog-user-project", "quota"),
            ("x-goog-api-client", tracking.as_str()),
            ("user-agent", tracking.as_str()),
            ("x-extra", "1"),
        ]);
        assert_eq!(sorted(&got), pairs(&want));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!got.contains_key("x-api-key"));
    }

    #[tokio::test]
    async fn request_headers_vertex_precedence_follows_go() {
        // User headers replace the tracking headers (Go's WithHeader order), and the
        // credentials' Authorization replaces a user one (Go's oauth2.Transport runs last).
        let (creds, _) = vertex_creds(Some(bearer("Bearer vertex-token")));
        let mut t = vertex(Fake::sequence(vec![reply(200, OK)]), creds, 0);
        t.headers
            .insert(USER_AGENT, HeaderValue::from_static("mine"));
        t.headers
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer user"));
        let got = t.request_headers().await.unwrap();
        assert_eq!(got.get(USER_AGENT).unwrap(), "mine");
        assert_eq!(got.get(AUTHORIZATION).unwrap(), "Bearer vertex-token");
        assert_eq!(
            got.get("x-goog-api-client").unwrap(),
            gadk_platform::tracking_header().as_str()
        );
    }

    #[tokio::test]
    async fn request_headers_vertex_credential_failure() {
        let (creds, _) = vertex_creds(None);
        let t = vertex(Fake::sequence(vec![reply(200, OK)]), creds, 0);
        let err = t.request_headers().await.unwrap_err();
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: boom and future attempts will not succeed"
        );
    }

    // ------------------------------------------------------------ send: Go texts over gadk_model::rest
    //
    // The retry loop, backoff, retry-after, timeouts, dropped bodies and cancellation are
    // tested in `gadk_model::rest`; these pin the Anthropic wiring and error texts.

    #[tokio::test(start_paused = true)]
    async fn send_first_attempt_success_wire() {
        let fake = Fake::sequence(vec![reply(200, OK)]);
        let t = anthropic(fake.clone(), 2);
        let res = t
            .send("claude-x", Bytes::from_static(BODY), false, None)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get(CONTENT_TYPE).unwrap(), "application/json");
        assert_eq!(
            gadk_model::collect_body(res.into_body()).await.unwrap(),
            OK.as_bytes()
        );
        let seen = fake.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].url, "https://api.anthropic.com/v1/messages");
        assert_eq!(seen[0].body, BODY);
        assert_eq!(seen[0].headers.get("x-api-key").unwrap(), "sk-test");
        assert_eq!(
            seen[0].headers.get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        for h in seen[0].headers.keys() {
            assert!(
                !h.as_str().starts_with("x-stainless"),
                "{h} must not be sent"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_vertex_wire() {
        let (creds, calls) = vertex_creds(Some(bearer("Bearer vertex-token")));
        let fake = Fake::sequence(vec![reply(500, BOOM), reply(200, OK)]);
        let t = vertex(fake.clone(), creds, 2);
        send(&t, true, None).await.unwrap();
        assert_eq!(fake.hits(), 2);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "credentials fetched per attempt"
        );
        let seen = fake.requests();
        assert_eq!(
            seen[1].url,
            "https://us-east5-aiplatform.googleapis.com/v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict"
        );
        assert_eq!(
            seen[1].headers.get(AUTHORIZATION).unwrap(),
            "Bearer vertex-token"
        );
        assert_eq!(
            seen[1].headers.get(USER_AGENT).unwrap(),
            gadk_platform::tracking_header().as_str()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_does_not_retry_other_statuses() {
        let cases = [
            (400u16, ErrorKind::BadRequest),
            (401, ErrorKind::Authentication),
            (403, ErrorKind::Authentication),
            (404, ErrorKind::ApiStatus),
            (422, ErrorKind::ApiStatus),
        ];
        // The error type is neutral, so the kind comes from the status alone.
        let body = r#"{"type":"error","error":{"type":"some_error","message":"bad thing"}}"#;
        for (status, kind) in cases {
            let fake = Fake::sequence(vec![reply(status, body), reply(200, OK)]);
            let err = send(&anthropic(fake.clone(), 2), false, None)
                .await
                .unwrap_err();
            assert_eq!(fake.hits(), 1, "{status}");
            assert!(err.is(kind), "{status}: {err}");
            let api = err.api_error().unwrap();
            assert_eq!(api.status_code, status);
            assert_eq!(api.message, "bad thing");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_max_retries_0_1_2() {
        for max_retries in [0u32, 1, 2] {
            let fake = Fake::sequence(vec![reply(500, BOOM)]);
            let err = send(&anthropic(fake.clone(), max_retries), false, None)
                .await
                .unwrap_err();
            assert_eq!(
                fake.hits(),
                max_retries as usize + 1,
                "max_retries {max_retries}"
            );
            assert_eq!(err.api_error().unwrap().status_code, 500);
            assert_eq!(
                err.to_string(),
                "anthropicmodel: api error (status 500) [type=api_error]: boom"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_request_id_header_wins() {
        let fake = Fake::sequence(vec![reply(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"},"request_id":"req_body"}"#,
        )
        .header("request-id", "req_header")]);
        let err = send(&anthropic(fake, 2), false, None).await.unwrap_err();
        assert_eq!(err.api_error().unwrap().request_id, "req_header");
    }

    #[tokio::test(start_paused = true)]
    async fn send_connection_error_text() {
        let fake = Fake::sequence(vec![FakeResponse::reset("connection refused")]);
        let err = send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.hits(), 3);
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: connection refused"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_retries_credential_failures() {
        let (creds, calls) = vertex_creds(None);
        let fake = Fake::sequence(vec![reply(200, OK)]);
        let err = send(&vertex(fake.clone(), creds, 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(fake.hits(), 0);
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: boom and future attempts will not succeed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_response_header_timeout_text() {
        let fake = Fake::sequence(vec![FakeResponse::never()]);
        let mut t = anthropic(fake.clone(), 0);
        t.rest.response_header_timeout = Some(DEFAULT_RESPONSE_HEADER_TIMEOUT);
        let err = send(&t, false, None).await.unwrap_err();
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: net/http: timeout awaiting response headers"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_attempt_timeout_on_last_attempt_is_deadline_exceeded() {
        for max_retries in [0u32, 2] {
            let fake = Fake::sequence(vec![FakeResponse::never()]);
            let err = send(
                &anthropic(fake.clone(), max_retries),
                false,
                Some(Duration::from_secs(1)),
            )
            .await
            .unwrap_err();
            assert_eq!(fake.hits(), max_retries as usize + 1);
            assert!(err.is_deadline_exceeded());
            assert_eq!(err.to_string(), "context deadline exceeded");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_attempt_timeout_keeps_running_on_a_streaming_body() {
        let fake = Fake::sequence(vec![FakeResponse::sse("data: a\n\n").hang()]);
        let t = anthropic(fake, 2);
        let start = tokio::time::Instant::now();
        let res = t
            .send(
                "claude-x",
                Bytes::from_static(BODY),
                true,
                Some(Duration::from_secs(5)),
            )
            .await
            .unwrap();
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        let err = body.next().await.unwrap().unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        let ModelError::Other(boxed) = err else {
            panic!("want ModelError::Other, got {err:?}");
        };
        let err = boxed.downcast::<AnthropicError>().unwrap();
        assert!(err.is_deadline_exceeded());
        assert!(
            body.next().await.is_none(),
            "the body ends after the deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_dropped_body_not_retried() {
        let want = "anthropicmodel: request failed: error reading response body: unexpected EOF";
        // x-should-retry: false.
        let fake = Fake::sequence(vec![
            reply(200, "{")
                .fail("unexpected EOF")
                .header("x-should-retry", "false"),
            reply(200, OK),
        ]);
        let err = send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.hits(), 1);
        assert_eq!(err.to_string(), want);
        // The last attempt.
        let fake = Fake::sequence(vec![reply(200, "{").fail("unexpected EOF")]);
        let err = send(&anthropic(fake.clone(), 1), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.hits(), 2);
        assert_eq!(err.to_string(), want);
        // An error status whose body drops on the last attempt.
        let fake = Fake::sequence(vec![reply(500, "{").fail("unexpected EOF")]);
        let err = send(&anthropic(fake.clone(), 0), false, None)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), want);
    }

    #[tokio::test(start_paused = true)]
    async fn send_streaming_error_status_reads_the_body() {
        let fake = Fake::sequence(vec![reply(400, BAD)]);
        let Err(err) = anthropic(fake, 2)
            .send("claude-x", Bytes::from_static(BODY), true, None)
            .await
        else {
            panic!("want an error");
        };
        assert!(err.is(ErrorKind::BadRequest));
        assert_eq!(err.api_error().unwrap().message, "bad thing");
    }

    #[test]
    fn rest_errors_map_to_gos_texts() {
        let io = std::io::Error::other("connection refused");
        let cases: [(&str, RestError, &str); 4] = [
            (
                "transport",
                RestError::Transport(ModelError::Transport(Box::new(io))),
                "anthropicmodel: transport error: connection refused",
            ),
            (
                "headers_keep_the_anthropic_error",
                RestError::Headers(Box::new(AnthropicError::transport("boom"))),
                "anthropicmodel: transport error: boom",
            ),
            (
                "response_header_timeout",
                RestError::ResponseHeaderTimeout,
                "anthropicmodel: transport error: net/http: timeout awaiting response headers",
            ),
            (
                "deadline_exceeded",
                RestError::DeadlineExceeded,
                "context deadline exceeded",
            ),
        ];
        for (name, rest, want) in cases {
            assert_eq!(AnthropicError::from(rest).to_string(), want, "{name}");
        }
        let (parts, ()) = http::Response::new(()).into_parts();
        let dropped = RestError::DroppedBody {
            parts: Box::new(parts),
            source: ModelError::Other("unexpected EOF".into()),
        };
        assert_eq!(
            AnthropicError::from(dropped).to_string(),
            "anthropicmodel: request failed: error reading response body: unexpected EOF"
        );
    }
}
