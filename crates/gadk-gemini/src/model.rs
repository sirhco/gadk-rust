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

//! Port of ADK-Go `model/gemini/gemini.go`: the [`Llm`] backed by the Gemini API or
//! Vertex AI.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_genai::{Content, GenerateContentConfig, GenerateContentResponse, LiveConnectConfig};
use gadk_model::{
    ExtrasRequestProvider, HttpClient, Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient,
    StreamingResponseAggregator, llm_response_from_genai,
};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

use crate::config::{Auth, CLOUD_PLATFORM_SCOPE, ClientConfig, ResolvedConfig, resolve};
use crate::continuation::{
    StreamContinuation, merge_responses, prepare_config, should_continue, should_enable,
};
use crate::error::GeminiError;
use crate::live::LiveConnection;
use crate::request::{Target, build_body};
use crate::transport::Transport;

/// The text of the user turn appended when the history ends on a non-user turn
/// (gemini.go `maybeAppendUserContent`).
const CONTINUE_TEXT: &str = "Continue processing previous requests as instructed. Exit or provide a summary if no more outputs are needed.";

/// The `user-agent` / `x-goog-api-client` tracking value (port of ADK-Go
/// `version.TrackingHeader`).
pub(crate) fn tracking_header() -> String {
    format!(
        "gadk-rust/{} rust/{}",
        env!("CARGO_PKG_VERSION"),
        env!("GADK_RUSTC_VERSION")
    )
}

/// A Gemini model served by the Gemini API or Vertex AI over REST.
#[derive(Clone)]
pub struct GeminiModel {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    resolved: Arc<ResolvedConfig>,
    transport: Transport,
}

impl fmt::Debug for GeminiModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiModel")
            .field("name", &self.inner.name)
            .field("resolved", &self.inner.resolved)
            .finish_non_exhaustive()
    }
}

/// A request ready to send: everything but the body, which is built for each hop (so the
/// extras provider runs on every request).
struct Prepared {
    transport: Transport,
    resolved: Arc<ResolvedConfig>,
    model: String,
    contents: Vec<Content>,
    config: GenerateContentConfig,
    extras: Option<ExtrasRequestProvider>,
    headers: HeaderMap,
}

impl Prepared {
    /// The JSON body of one request sent with `config`.
    fn body(&self, config: &GenerateContentConfig) -> Result<Map<String, Value>, ModelError> {
        let target = Target {
            backend: self.resolved.backend,
            project: self.resolved.project.as_deref(),
            location: self.resolved.location.as_deref(),
        };
        build_body(target, &self.contents, config, self.extras.as_ref())
    }

    /// One `generateContent` request (one hop) with `config`.
    async fn generate_hop(
        &self,
        config: &GenerateContentConfig,
    ) -> Result<GenerateContentResponse, ModelError> {
        let body = self.body(config)?;
        self.transport
            .generate(&self.model, body, self.headers.clone())
            .await
    }

    /// One `streamGenerateContent` request (one hop) with `config`.
    fn stream_hop(
        &self,
        config: &GenerateContentConfig,
    ) -> BoxStream<'static, Result<GenerateContentResponse, ModelError>> {
        match self.body(config) {
            Ok(body) => self
                .transport
                .stream(&self.model, body, self.headers.clone()),
            Err(e) => futures::stream::once(async move { Err(e) }).boxed(),
        }
    }
}

impl GeminiModel {
    /// Port of gemini.go `NewModel`: resolves `cfg` (building Application Default
    /// Credentials when needed) and returns the model as an [`Llm`].
    #[allow(clippy::new_ret_no_self)] // mirrors Go NewModel, which returns model.LLM
    pub async fn new(model_name: &str, cfg: ClientConfig) -> Result<Arc<dyn Llm>, GeminiError> {
        Ok(Arc::new(Self::new_concrete(model_name, cfg).await?))
    }

    /// Like [`new`](Self::new), returning the concrete type.
    pub async fn new_concrete(model_name: &str, cfg: ClientConfig) -> Result<Self, GeminiError> {
        let mut resolved = resolve(&cfg)?;
        if resolved.needs_adc {
            let credentials = google_cloud_auth::credentials::Builder::default()
                .with_scopes([CLOUD_PLATFORM_SCOPE])
                .build()
                .map_err(|e| GeminiError::Auth(e.to_string()))?;
            resolved.auth = Some(Auth::Credentials(credentials));
        }
        let http: Arc<dyn HttpClient> = match cfg.http_client {
            Some(h) => h,
            None => Arc::new(ReqwestClient::default()),
        };
        let resolved = Arc::new(resolved);
        let transport = Transport::new(http, resolved.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                name: model_name.to_string(),
                resolved,
                transport,
            }),
        })
    }

    /// The resolved client configuration.
    pub fn resolved(&self) -> &ResolvedConfig {
        &self.inner.resolved
    }

    /// Opens a Live (bidi streaming) connection to this model (genai `Live.Connect` with
    /// the model's name and client configuration).
    pub async fn live_connect(
        &self,
        cfg: LiveConnectConfig,
    ) -> Result<LiveConnection, GeminiError> {
        crate::live::connect(&self.inner.resolved, &self.inner.name, &cfg).await
    }
}

impl Inner {
    /// Clones and prepares `req` (gemini.go `GenerateContent` up to the SDK call). The
    /// caller's request is moved in, so nothing it holds is modified.
    fn prepare(&self, req: LlmRequest) -> Result<Prepared, ModelError> {
        let LlmRequest {
            model,
            mut contents,
            config,
            http_options,
            ..
        } = req;
        maybe_append_user_content(&mut contents);
        let config = config.unwrap_or_default();

        let client_options = &self.resolved.http_options;
        let mut headers = client_options.headers.clone();
        if let Some(o) = &http_options {
            for name in o.headers.keys() {
                headers.remove(name);
            }
            for (name, value) in &o.headers {
                headers.append(name.clone(), value.clone());
            }
        }
        add_tracking_headers(&mut headers)?;

        // gemini.go configPreservingEmptyTextThoughtSignatures: the request provider takes
        // precedence over the client's.
        let extras: Option<ExtrasRequestProvider> = http_options
            .as_ref()
            .and_then(|o| o.extras_request_provider.as_ref())
            .or(client_options.extras_request_provider.as_ref())
            .cloned();

        let model = if model.is_empty() {
            self.name.clone()
        } else {
            model
        };
        Ok(Prepared {
            transport: self.transport.with_request_options(http_options.as_ref()),
            resolved: self.resolved.clone(),
            model,
            contents,
            config,
            extras,
            headers,
        })
    }
}

/// Port of gemini.go `maybeAppendUserContent`.
fn maybe_append_user_content(contents: &mut Vec<Content>) {
    // Contents are left empty to match Python ADK.
    if contents.last().is_some_and(|last| last.role != "user") {
        contents.push(Content::from_text(CONTINUE_TEXT, "user"));
    }
}

/// Sets `x-goog-api-client` and `user-agent` to the tracking value, appended to any
/// existing values (gemini.go `addHeaders` + `mergeHeadersInterceptor`): one value each.
fn add_tracking_headers(headers: &mut HeaderMap) -> Result<(), ModelError> {
    let tracking = tracking_header();
    for name in [
        HeaderName::from_static("x-goog-api-client"),
        http::header::USER_AGENT,
    ] {
        let mut value: Vec<u8> = Vec::new();
        for existing in headers.get_all(&name) {
            value.extend_from_slice(existing.as_bytes());
            value.push(b' ');
        }
        value.extend_from_slice(tracking.as_bytes());
        let value = HeaderValue::from_bytes(&value).map_err(|e| ModelError::Other(Box::new(e)))?;
        headers.insert(name, value);
    }
    Ok(())
}

/// Sends the prepared request and yields the raw responses, with genai's automatic
/// continuation (`models.go` `GenerateContent` / `GenerateContentStream`).
///
/// Unary: while the response carries a continuation token with a resumable finish reason,
/// the request is sent again with that token, and the responses of every hop are yielded
/// merged into one. Stream: the chunks of every hop are yielded in order, then the error
/// that ended the run, if any (see [`StreamContinuation::end_hop`]).
fn run_hops(
    prepared: Prepared,
    stream: bool,
) -> BoxStream<'static, Result<GenerateContentResponse, ModelError>> {
    if stream {
        stream_hops(prepared)
    } else {
        futures::stream::once(generate_hops(prepared)).boxed()
    }
}

/// genai `Models.GenerateContent`.
async fn generate_hops(prepared: Prepared) -> Result<GenerateContentResponse, ModelError> {
    let enabled = should_enable(&prepared.config);
    let mut response = prepared.generate_hop(&prepared.config).await?;
    if !enabled {
        return Ok(response);
    }
    let mut hop_responses = Vec::new();
    while let Some(token) = should_continue(&response) {
        let call_config = prepare_config(&prepared.config, token);
        let next = prepared.generate_hop(&call_config).await?;
        hop_responses.push(std::mem::replace(&mut response, next));
    }
    if hop_responses.is_empty() {
        return Ok(response);
    }
    hop_responses.push(response);
    merge_responses(hop_responses)
}

struct HopsState {
    prepared: Prepared,
    continuation: StreamContinuation,
    /// The current hop's chunks; `None` between hops.
    hop: Option<BoxStream<'static, Result<GenerateContentResponse, ModelError>>>,
    done: bool,
}

/// genai `Models.GenerateContentStream`: within a hop the transport ends its stream at
/// the first error, which ends the hop.
fn stream_hops(
    prepared: Prepared,
) -> BoxStream<'static, Result<GenerateContentResponse, ModelError>> {
    let enabled = should_enable(&prepared.config);
    let state = HopsState {
        continuation: StreamContinuation::new(prepared.config.clone(), enabled),
        prepared,
        hop: None,
        done: false,
    };
    futures::stream::unfold(state, |mut s| async move {
        loop {
            if s.done {
                return None;
            }
            let hop = match &mut s.hop {
                Some(hop) => hop,
                None => {
                    let call_config = s.continuation.start_hop();
                    s.hop.insert(s.prepared.stream_hop(&call_config))
                }
            };
            let hop_err = match hop.next().await {
                Some(Ok(chunk)) => match s.continuation.observe(chunk) {
                    Ok(chunk) => return Some((Ok(chunk), s)),
                    Err(e) => {
                        s.done = true;
                        return Some((Err(e), s));
                    }
                },
                Some(Err(e)) => Some(e),
                None => None,
            };
            s.hop = None;
            match s.continuation.end_hop(hop_err) {
                Ok(true) => {}
                Ok(false) => s.done = true,
                Err(e) => {
                    s.done = true;
                    return Some((Err(e), s));
                }
            }
        }
    })
    .boxed()
}

/// gemini.go `generate`: the first candidate of the single response.
fn generate(prepared: Prepared) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    run_hops(prepared, false)
        .take(1)
        .map(|item| {
            let resp = item?;
            if resp.candidates.is_empty() {
                return Err(ModelError::EmptyResponse);
            }
            Ok(llm_response_from_genai(&resp))
        })
        .boxed()
}

struct StreamState {
    chunks: BoxStream<'static, Result<GenerateContentResponse, ModelError>>,
    aggregator: StreamingResponseAggregator,
    pending: VecDeque<Result<LlmResponse, ModelError>>,
    done: bool,
}

/// gemini.go `generateStream`: every chunk through the aggregator, then its final
/// aggregated response; a transport error is yielded and ends the stream.
fn generate_stream(prepared: Prepared) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    let state = StreamState {
        chunks: run_hops(prepared, true),
        aggregator: StreamingResponseAggregator::new(),
        pending: VecDeque::new(),
        done: false,
    };
    futures::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(item) = s.pending.pop_front() {
                return Some((item, s));
            }
            if s.done {
                return None;
            }
            match s.chunks.next().await {
                Some(Ok(resp)) => s.pending.extend(s.aggregator.process_response(resp)),
                Some(Err(e)) => {
                    s.done = true;
                    return Some((Err(e), s));
                }
                None => {
                    s.done = true;
                    if let Some(last) = s.aggregator.close() {
                        s.pending.push_back(Ok(last));
                    }
                }
            }
        }
    })
    .boxed()
}

impl Llm for GeminiModel {
    fn name(&self) -> &str {
        &self.inner.name
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let prepared = match self.inner.prepare(req) {
            Ok(p) => p,
            Err(e) => return futures::stream::once(async move { Err(e) }).boxed(),
        };
        if stream {
            generate_stream(prepared)
        } else {
            generate(prepared)
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn google_variant(&self) -> Option<gadk_model::Backend> {
        Some(self.inner.resolved.backend)
    }

    /// Opens a Live connection (genai `Live.Connect` with this model's name and client
    /// configuration) as the model-agnostic trait object. Failures convert to `ModelError`.
    fn connect_live<'a>(
        &'a self,
        cfg: LiveConnectConfig,
    ) -> BoxFuture<'a, Result<Box<dyn gadk_model::LiveConnection>, ModelError>> {
        Box::pin(async move {
            let conn = self.live_connect(cfg).await?;
            Ok(Box::new(conn) as Box<dyn gadk_model::LiveConnection>)
        })
    }
}
