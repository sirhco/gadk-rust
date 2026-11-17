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

//! The call layer of the BigQuery Storage Write client for the three RPCs the analytics plugin
//! uses — `CreateWriteStream`, `AppendRows`, `FinalizeWriteStream` — over the generated tonic
//! client of [`crate::storagepb`]: per-RPC `x-goog-request-params`, `x-goog-api-client`, bearer
//! `authorization`, the caller's outgoing metadata from the [`Context`], and the standard
//! BigQuery Write call options (timeouts, retry codes and backoffs, applied by `invoke`).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use gadk_model::gcp_auth::{adc_credentials, credential_headers};
use gadk_platform::BoxFuture;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::WaitForCancellationFutureOwned;
use tonic::codegen::tokio_stream::Stream;
use tonic::metadata::{
    AsciiMetadataKey, AsciiMetadataValue, BinaryMetadataKey, MetadataMap, MetadataValue,
};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::config::ClientOptions;
use crate::context::{Context, ContextError, Metadata};
use crate::error::{Code, Error, status_code};
use crate::gax::{Backoff, Pauses};
use crate::storagepb::big_query_write_client::BigQueryWriteClient;
use crate::storagepb::{
    AppendRowsRequest, AppendRowsResponse, CreateWriteStreamRequest, FinalizeWriteStreamRequest,
    FinalizeWriteStreamResponse, WriteStream,
};
use crate::write::{AppendRowsClient, eof, status_error};

/// The default gRPC endpoint (TLS).
pub(crate) const DEFAULT_ENDPOINT: &str = "https://bigquerystorage.googleapis.com:443";

/// The OAuth scopes requested for Application Default Credentials.
pub(crate) const DEFAULT_AUTH_SCOPES: [&str; 3] = [
    "https://www.googleapis.com/auth/bigquery",
    "https://www.googleapis.com/auth/bigquery.insertdata",
    "https://www.googleapis.com/auth/cloud-platform",
];

/// The `x-goog-api-client` key (merged into one value, the client's replacing a caller's).
const API_CLIENT_KEY: &str = "x-goog-api-client";

/// The routing header key of the generated client (also the AppendRows routing header the
/// stream writer adds).
pub(crate) const REQUEST_PARAMS_KEY: &str = "x-goog-request-params";

/// The largest message size sent or received on the channel (`i32::MAX`, the BigQuery Write
/// client's default).
const MAX_MESSAGE_SIZE: usize = i32::MAX as usize;

/// The message of the `Canceled` status of a closed client.
const CONN_CLOSING: &str = "grpc: the client connection is closing";

/// The retry policy of one RPC: which status codes are retried, and the pauses between them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetrySettings {
    /// The status codes that are retried.
    pub(crate) codes: Vec<Code>,
    /// The pauses between attempts.
    pub(crate) backoff: Backoff,
}

/// The settings of one RPC: a timeout (applied only when the call's context has no deadline) and
/// a retry policy.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct CallSettings {
    /// The timeout; `None` = none.
    pub(crate) timeout: Option<Duration>,
    /// The retry policy; `None` = no retries.
    pub(crate) retry: Option<RetrySettings>,
}

/// The call options (timeout, retry) of the RPCs the plugin calls.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WriteCallOptions {
    /// `CreateWriteStream`: 1200s; DeadlineExceeded / Unavailable / ResourceExhausted; 10s → 120s ×1.3.
    pub(crate) create_write_stream: CallSettings,
    /// `AppendRows` (opening the stream): no timeout; Unavailable; 100ms → 60s ×1.3.
    pub(crate) append_rows: CallSettings,
    /// `FinalizeWriteStream`: 600s; DeadlineExceeded / Unavailable / ResourceExhausted; 100ms → 60s ×1.3.
    pub(crate) finalize_write_stream: CallSettings,
}

impl Default for WriteCallOptions {
    /// The standard BigQuery Write call options.
    fn default() -> Self {
        let unary_codes = vec![
            Code::DeadlineExceeded,
            Code::Unavailable,
            Code::ResourceExhausted,
        ];
        WriteCallOptions {
            create_write_stream: CallSettings {
                timeout: Some(Duration::from_secs(1200)),
                retry: Some(RetrySettings {
                    codes: unary_codes.clone(),
                    backoff: Backoff {
                        initial: Duration::from_secs(10),
                        max: Duration::from_secs(120),
                        multiplier: 1.3,
                    },
                }),
            },
            append_rows: CallSettings {
                timeout: None,
                retry: Some(RetrySettings {
                    codes: vec![Code::Unavailable],
                    backoff: Backoff {
                        initial: Duration::from_millis(100),
                        max: Duration::from_secs(60),
                        multiplier: 1.3,
                    },
                }),
            },
            finalize_write_stream: CallSettings {
                timeout: Some(Duration::from_secs(600)),
                retry: Some(RetrySettings {
                    codes: unary_codes,
                    backoff: Backoff {
                        initial: Duration::from_millis(100),
                        max: Duration::from_secs(60),
                        multiplier: 1.3,
                    },
                }),
            },
        }
    }
}

/// The status of a context error: `Canceled` / `DeadlineExceeded` with the context error's
/// text.
fn context_status(err: ContextError) -> Error {
    Error::Status {
        code: match err {
            ContextError::Canceled => Code::Canceled,
            ContextError::DeadlineExceeded => Code::DeadlineExceeded,
        },
        message: err.to_string(),
    }
}

/// The status of an ended (or ending) context.
fn ended_status(ctx: &Context) -> Error {
    context_status(ctx.err().unwrap_or(ContextError::Canceled))
}

/// `settings.timeout` bounds `ctx` when it has no deadline; a failed attempt whose status code
/// is one of the retry codes is retried after a jittered backoff pause, which an ending context
/// interrupts with the context's error.
async fn invoke<T, F, Fut>(
    ctx: &Context,
    settings: &CallSettings,
    mut attempt: F,
) -> Result<T, Error>
where
    F: FnMut(Context) -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let ctx = match settings.timeout {
        Some(t) if !t.is_zero() && ctx.deadline().is_none() => ctx.with_timeout(t),
        _ => ctx.clone(),
    };
    let mut retry = settings
        .retry
        .as_ref()
        .map(|r| (r.codes.as_slice(), Pauses::new(r.backoff)));
    loop {
        // A failed attempt's error text carries the code and message only: the google.rpc
        // error-details protos (ErrorInfo, DebugInfo, …) are not generated, so no details are
        // appended after `rpc error: code = … desc = …`.
        let err = match attempt(ctx.clone()).await {
            Ok(v) => return Ok(v),
            Err(err) => err,
        };
        let Some((codes, pauses)) = retry.as_mut() else {
            return Err(err);
        };
        // Only gRPC statuses are retried; `status_code` is `Unknown` for
        // anything else, which no retryer lists.
        if !matches!(err, Error::Status { .. }) || !codes.contains(&status_code(&err)) {
            return Err(err);
        }
        let pause = pauses.pause();
        tokio::select! {
            biased;
            () = ctx.done() => {
                return Err(Error::Context(ctx.err().unwrap_or(ContextError::Canceled)));
            }
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// An AppendRows stream that is about to be opened: awaiting it opens the stream (one attempt,
/// inside the open retry loop).
pub(crate) type OpenAppendRows = BoxFuture<'static, Result<Box<dyn AppendRowsClient>, Error>>;

/// A test hook on opening AppendRows streams: called for
/// every AppendRows stream the client opens (each attempt), with the call's context and the
/// opening; it may await the opening and wrap the stream, or refuse without opening.
pub(crate) type StreamInterceptor = Arc<
    dyn Fn(Context, OpenAppendRows) -> BoxFuture<'static, Result<Box<dyn AppendRowsClient>, Error>>
        + Send
        + Sync,
>;

/// A BigQuery Storage Write API client: a cheap `Clone` handle on one tonic channel.
#[derive(Clone)]
pub struct WriteClient {
    /// The generated tonic client.
    pub(crate) inner: BigQueryWriteClient<Channel>,
    /// The bearer-token source; `None` sends no `authorization` (unauthenticated).
    pub(crate) credentials: Option<Credentials>,
    /// Per-RPC timeouts and retries.
    pub(crate) call_options: Arc<WriteCallOptions>,
    /// The `x-goog-api-client` value (`gadk_platform::tracking_header()`).
    pub(crate) api_client_header: String,
    /// Test hook on opening AppendRows streams.
    pub(crate) stream_interceptor: Option<StreamInterceptor>,
    /// [`ClientOptions::headers`] as metadata on every call (header options such as
    /// `x-goog-user-project` travel as gRPC metadata).
    pub(crate) headers: Arc<Metadata>,
    /// Set by [`close`](Self::close) (shared by every clone).
    pub(crate) closed: Arc<AtomicBool>,
}

impl fmt::Debug for WriteClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteClient")
            .field("credentials", &self.credentials.is_some())
            .field("call_options", &self.call_options)
            .field("stream_interceptor", &self.stream_interceptor.is_some())
            .finish_non_exhaustive()
    }
}

impl WriteClient {
    /// Creates a client: a lazily connecting channel to [`ClientOptions::endpoint`] (`http://` =
    /// plaintext, `https://` or a bare `host:port` = TLS) or `bigquerystorage.googleapis.com:443`
    /// (TLS, native roots), with [`ClientOptions::credentials`], none when
    /// [`ClientOptions::without_authentication`], else Application Default Credentials with the
    /// BigQuery scopes. Must run inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// An invalid endpoint, or credentials that cannot be resolved.
    pub async fn new(options: ClientOptions) -> Result<WriteClient, Error> {
        let credentials = resolve_credentials(&options)?;
        let channel = channel_for(options.endpoint.as_deref())?;
        Ok(Self::with_parts(channel, credentials, &options))
    }

    /// A client on an existing channel (the in-process fake's duplex channel), with
    /// `options`' credentials as [`new`](Self::new) resolves them and the default call options.
    ///
    /// # Errors
    ///
    /// Credentials that cannot be resolved.
    #[cfg_attr(not(any(test, feature = "testutil")), allow(dead_code))] // the fake server
    pub(crate) fn from_channel(
        channel: Channel,
        options: &ClientOptions,
    ) -> Result<WriteClient, Error> {
        let credentials = resolve_credentials(options)?;
        Ok(Self::with_parts(channel, credentials, options))
    }

    fn with_parts(
        channel: Channel,
        credentials: Option<Credentials>,
        options: &ClientOptions,
    ) -> WriteClient {
        let mut headers = Metadata::new();
        for (k, v) in &options.headers {
            headers
                .entry(k.as_str().to_owned())
                .or_default()
                .push(String::from_utf8_lossy(v.as_bytes()).into_owned());
        }
        WriteClient {
            inner: BigQueryWriteClient::new(channel)
                .max_decoding_message_size(MAX_MESSAGE_SIZE)
                .max_encoding_message_size(MAX_MESSAGE_SIZE),
            credentials,
            call_options: Arc::new(WriteCallOptions::default()),
            api_client_header: gadk_platform::tracking_header(),
            stream_interceptor: None,
            headers: Arc::new(headers),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Replaces the per-RPC timeouts and retries (tests shorten the backoffs).
    #[cfg_attr(not(test), allow(dead_code))] // test hooks
    pub(crate) fn with_call_options(mut self, call_options: WriteCallOptions) -> WriteClient {
        self.call_options = Arc::new(call_options);
        self
    }

    /// Installs a hook on opening AppendRows streams (used by tests to wrap the stream).
    #[cfg_attr(not(test), allow(dead_code))] // test hooks
    pub(crate) fn with_stream_interceptor(mut self, interceptor: StreamInterceptor) -> WriteClient {
        self.stream_interceptor = Some(interceptor);
        self
    }

    /// Creates a write stream: sends `x-goog-request-params: parent=<url-escaped parent>`,
    /// retried and bounded per the `create_write_stream` call options and `ctx`.
    ///
    /// # Errors
    ///
    /// The final status ([`Error::Status`]) or the context's error.
    pub async fn create_write_stream(
        &self,
        ctx: &Context,
        req: CreateWriteStreamRequest,
    ) -> Result<WriteStream, Error> {
        let params = format!("parent={}", gadk_platform::go_query_escape(&req.parent));
        invoke(ctx, &self.call_options.create_write_stream, |ctx| {
            self.unary(ctx, &params, req.clone(), |mut c, r| async move {
                c.create_write_stream(r).await
            })
        })
        .await
    }

    /// Opens a bidirectional AppendRows stream that ends when `ctx` ends (its `send` then
    /// returns [`eof`] and its `recv` the context's status), carrying `ctx`'s outgoing metadata
    /// and `x-goog-api-client`; opening is retried on `Unavailable` per the `append_rows` call
    /// options. Opening does not wait for the server: a status the server ends the stream with
    /// surfaces at `recv` (and `send` returns `EOF`).
    ///
    /// # Errors
    ///
    /// The status that ended the opening (closed client, invalid metadata, credentials, a
    /// stream interceptor), or the context's error.
    pub async fn append_rows(&self, ctx: &Context) -> Result<Box<dyn AppendRowsClient>, Error> {
        invoke(ctx, &self.call_options.append_rows, |ctx| {
            let open: OpenAppendRows = Box::pin(self.clone().open_append_rows(ctx.clone()));
            match &self.stream_interceptor {
                Some(interceptor) => interceptor(ctx, open),
                None => open,
            }
        })
        .await
    }

    /// Finalizes a write stream: sends `x-goog-request-params: name=<url-escaped name>`,
    /// retried and bounded per the `finalize_write_stream` call options and `ctx`.
    ///
    /// # Errors
    ///
    /// The final status ([`Error::Status`]) or the context's error.
    pub async fn finalize_write_stream(
        &self,
        ctx: &Context,
        req: FinalizeWriteStreamRequest,
    ) -> Result<FinalizeWriteStreamResponse, Error> {
        let params = format!("name={}", gadk_platform::go_query_escape(&req.name));
        invoke(ctx, &self.call_options.finalize_write_stream, |ctx| {
            self.unary(ctx, &params, req.clone(), |mut c, r| async move {
                c.finalize_write_stream(r).await
            })
        })
        .await
    }

    /// Closes the client: later calls on it (and its clones) fail with `Canceled`
    /// (`grpc: the client connection is closing`); the channel closes once every clone is
    /// dropped. Streams already open are left to their contexts.
    ///
    /// # Errors
    ///
    /// None today.
    pub async fn close(&self) -> Result<(), Error> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// One unary attempt under `ctx`: the request with this call's metadata
    /// and `grpc-timeout`, ended by `ctx` with its status.
    async fn unary<Req, Resp, F, Fut>(
        &self,
        ctx: Context,
        request_params: &str,
        req: Req,
        call: F,
    ) -> Result<Resp, Error>
    where
        F: FnOnce(BigQueryWriteClient<Channel>, tonic::Request<Req>) -> Fut,
        Fut: Future<Output = Result<tonic::Response<Resp>, tonic::Status>>,
    {
        let attempt = async {
            let metadata = self.call_metadata(&ctx, Some(request_params)).await?;
            let mut request = tonic::Request::new(req);
            *request.metadata_mut() = metadata;
            set_deadline(&mut request, &ctx);
            call(self.inner.clone(), request)
                .await
                .map(tonic::Response::into_inner)
                .map_err(|s| status_error(&s))
        };
        self.under(&ctx, attempt).await
    }

    /// Runs one attempt unless the client is closed or `ctx` has ended, ending it with the
    /// context's status when `ctx` ends first.
    async fn under<T>(
        &self,
        ctx: &Context,
        attempt: impl Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Status {
                code: Code::Canceled,
                message: CONN_CLOSING.to_owned(),
            });
        }
        if let Some(err) = ctx.err() {
            return Err(context_status(err));
        }
        tokio::select! {
            biased;
            () = ctx.done() => Err(ended_status(ctx)),
            res = attempt => res,
        }
    }

    /// One AppendRows open attempt: the stream starts on a spawned driver task and is returned
    /// at once (tonic's call would wait for the response headers, so it runs on the driver
    /// instead).
    // A channel that cannot connect fails the stream at its first `recv` (`Unavailable`)
    // instead of failing the open, so the open retry on `Unavailable` does not cover connection
    // failures; the stream writer's own retries apply.
    async fn open_append_rows(self, ctx: Context) -> Result<Box<dyn AppendRowsClient>, Error> {
        let metadata = self.under(&ctx, self.call_metadata(&ctx, None)).await?;
        let (req_tx, req_rx) = mpsc::channel::<AppendRowsRequest>(1);
        let ended = CancellationToken::new();
        let mut request = tonic::Request::new(RequestStream {
            requests: req_rx,
            stop: Box::pin(ended.clone().cancelled_owned()),
        });
        *request.metadata_mut() = metadata;
        set_deadline(&mut request, &ctx);
        let (resp_tx, resp_rx) = mpsc::channel(1);
        let driver = tokio::spawn(drive_append_rows(
            self.inner.clone(),
            request,
            ctx.clone(),
            resp_tx,
            ended.clone(),
        ));
        Ok(Box::new(TonicAppendRows {
            ctx,
            requests: Some(req_tx),
            responses: resp_rx,
            ended,
            end: None,
            driver,
        }))
    }

    /// The request metadata: the caller's
    /// outgoing metadata (a caller `x-goog-api-client` replaced by the client's), the client's
    /// header options, `x-goog-api-client`, the routing header appended after any the caller
    /// set, then the credentials' headers. Validated as gRPC validates outgoing metadata.
    async fn call_metadata(
        &self,
        ctx: &Context,
        request_params: Option<&str>,
    ) -> Result<MetadataMap, Error> {
        let mut md = ctx.outgoing_metadata().unwrap_or_default();
        for (k, vals) in self.headers.iter() {
            md.entry(k.clone())
                .or_default()
                .extend(vals.iter().cloned());
        }
        // `x-goog-api-client` is gadk's tracking header (`gadk_platform::tracking_header()`).
        md.insert(
            API_CLIENT_KEY.to_owned(),
            vec![self.api_client_header.clone()],
        );
        if let Some(params) = request_params {
            md.entry(REQUEST_PARAMS_KEY.to_owned())
                .or_default()
                .push(params.to_owned());
        }
        validate_metadata(&md)?;
        let mut map = metadata_map(&md)?;
        if let Some(credentials) = &self.credentials {
            let headers = credential_headers(credentials)
                .await
                .map_err(|e| Error::Status {
                    code: Code::Unauthenticated,
                    message: format!("transport: per-RPC creds failed due to error: {e}"),
                })?;
            for (k, v) in &headers {
                let key = AsciiMetadataKey::from_bytes(k.as_str().as_bytes())
                    .map_err(|e| internal(e.to_string()))?;
                let value = AsciiMetadataValue::try_from(v.as_bytes())
                    .map_err(|e| internal(e.to_string()))?;
                map.append(key, value);
            }
        }
        Ok(map)
    }
}

/// An `Internal` status for an RPC the client refuses to start.
fn internal(message: String) -> Error {
    Error::Status {
        code: Code::Internal,
        message,
    }
}

/// Validates outgoing metadata as gRPC does when a stream starts: keys in `[0-9a-z-_.]`,
/// values of non-`-bin` keys printable ASCII; pseudo-headers (`:…`) are skipped.
fn validate_metadata(md: &Metadata) -> Result<(), Error> {
    for (k, vals) in md {
        if k.is_empty() {
            return Err(internal("there is an empty key in the header".to_owned()));
        }
        if k.starts_with(':') {
            continue;
        }
        if !k.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        }) {
            return Err(internal(format!(
                "header key {} contains illegal characters not in [0-9a-z-_.]",
                gadk_platform::go_quote(k)
            )));
        }
        if k.ends_with("-bin") {
            continue;
        }
        if vals
            .iter()
            .any(|v| v.bytes().any(|b| !(0x20..=0x7e).contains(&b)))
        {
            return Err(internal(format!(
                "header key {} contains value with non-printable ASCII characters",
                gadk_platform::go_quote(k)
            )));
        }
    }
    Ok(())
}

/// The validated metadata as tonic's map (`-bin` values are sent base64-encoded, as gRPC
/// does; pseudo-headers are dropped).
fn metadata_map(md: &Metadata) -> Result<MetadataMap, Error> {
    let mut map = MetadataMap::new();
    for (k, vals) in md {
        if k.starts_with(':') {
            continue;
        }
        if k.ends_with("-bin") {
            let key =
                BinaryMetadataKey::from_bytes(k.as_bytes()).map_err(|e| internal(e.to_string()))?;
            for v in vals {
                map.append_bin(key.clone(), MetadataValue::from_bytes(v.as_bytes()));
            }
        } else {
            let key =
                AsciiMetadataKey::from_bytes(k.as_bytes()).map_err(|e| internal(e.to_string()))?;
            for v in vals {
                let value = AsciiMetadataValue::try_from(v.as_str())
                    .map_err(|e| internal(e.to_string()))?;
                map.append(key.clone(), value);
            }
        }
    }
    Ok(map)
}

/// The client sends the remaining time of the call's deadline as `grpc-timeout`.
fn set_deadline<T>(request: &mut tonic::Request<T>, ctx: &Context) {
    if let Some(at) = ctx.deadline() {
        request.set_timeout(at.saturating_duration_since(Instant::now()));
    }
}

/// Resolves the credentials of `options`: explicit credentials, none, or Application Default
/// Credentials (`GOOGLE_APPLICATION_CREDENTIALS` first) with [`DEFAULT_AUTH_SCOPES`].
fn resolve_credentials(options: &ClientOptions) -> Result<Option<Credentials>, Error> {
    if options.without_authentication && options.credentials.is_some() {
        return Err(Error::msg(
            "options.WithoutAuthentication is incompatible with any option that provides \
             credentials",
        ));
    }
    if options.without_authentication {
        return Ok(None);
    }
    if let Some(credentials) = &options.credentials {
        return Ok(Some(credentials.clone()));
    }
    let file = ProcessEnv.var("GOOGLE_APPLICATION_CREDENTIALS");
    adc_credentials(file.as_deref(), &DEFAULT_AUTH_SCOPES)
        .map(Some)
        .map_err(Error::other)
}

/// The lazily connecting channel to `endpoint`: a URL as given (`http://` = plaintext), a bare
/// `host[:port]` over TLS, none = [`DEFAULT_ENDPOINT`]. TLS uses the platform's native roots
/// (rustls with aws-lc).
fn channel_for(endpoint: Option<&str>) -> Result<Channel, Error> {
    let url = match endpoint.filter(|e| !e.is_empty()) {
        None => DEFAULT_ENDPOINT.to_owned(),
        Some(e) if e.contains("://") => e.to_owned(),
        Some(e) => format!("https://{e}"),
    };
    let mut endpoint = Endpoint::from_shared(url.clone()).map_err(Error::other)?;
    if url.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(Error::other)?;
    }
    Ok(endpoint.connect_lazy())
}

/// The request side of an AppendRows call: the client's sends, ended once the stream has
/// ended or been abandoned. Ending it lets hyper drop its half of the HTTP/2 stream, so with
/// the response side dropped too, h2 resets the stream (`RST_STREAM(CANCEL)`) and the server
/// sees the call canceled (after the end of the requests), as a canceled context does
/// resets the stream.
struct RequestStream {
    requests: mpsc::Receiver<AppendRowsRequest>,
    stop: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl Stream for RequestStream {
    type Item = AppendRowsRequest;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<AppendRowsRequest>> {
        if self.stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        self.requests.poll_recv(cx)
    }
}

/// How an AppendRows stream ended.
#[derive(Debug, Clone)]
enum StreamEnd {
    /// The server ended it with OK.
    Eof,
    /// A status: the server's, the transport's, or the context's.
    Status { code: Code, message: String },
}

impl StreamEnd {
    fn from_status(status: &tonic::Status) -> StreamEnd {
        match status_error(status) {
            Error::Status { code, message } => StreamEnd::Status { code, message },
            _ => StreamEnd::Eof,
        }
    }

    fn from_context(ctx: &Context) -> StreamEnd {
        match ended_status(ctx) {
            Error::Status { code, message } => StreamEnd::Status { code, message },
            _ => StreamEnd::Eof,
        }
    }

    fn error(&self) -> Error {
        match self {
            StreamEnd::Eof => eof(),
            StreamEnd::Status { code, message } => Error::Status {
                code: *code,
                message: message.clone(),
            },
        }
    }
}

/// Runs one AppendRows call: opens it, forwards the responses, and reports how it ended; an
/// ending `ctx` drops the call (the server sees the stream canceled).
async fn drive_append_rows(
    client: BigQueryWriteClient<Channel>,
    request: tonic::Request<RequestStream>,
    ctx: Context,
    responses: mpsc::Sender<Result<AppendRowsResponse, StreamEnd>>,
    ended: CancellationToken,
) {
    let end = tokio::select! {
        biased;
        () = ctx.done() => StreamEnd::from_context(&ctx),
        end = pump_append_rows(client, request, &responses) => end,
    };
    ended.cancel();
    let _ = responses.send(Err(end)).await;
}

async fn pump_append_rows(
    mut client: BigQueryWriteClient<Channel>,
    request: tonic::Request<RequestStream>,
    responses: &mpsc::Sender<Result<AppendRowsResponse, StreamEnd>>,
) -> StreamEnd {
    let mut stream = match client.append_rows(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => return StreamEnd::from_status(&status),
    };
    loop {
        match stream.message().await {
            Ok(Some(response)) => {
                if responses.send(Ok(response)).await.is_err() {
                    return StreamEnd::Eof;
                }
            }
            Ok(None) => return StreamEnd::Eof,
            Err(status) => return StreamEnd::from_status(&status),
        }
    }
}

/// The client half of a tonic AppendRows stream with gRPC client-stream semantics.
struct TonicAppendRows {
    ctx: Context,
    /// `None` after `close_send`.
    requests: Option<mpsc::Sender<AppendRowsRequest>>,
    responses: mpsc::Receiver<Result<AppendRowsResponse, StreamEnd>>,
    /// Canceled once the stream has ended.
    ended: CancellationToken,
    /// How the stream ended, once `recv` has seen it (repeated by later `recv`s).
    end: Option<StreamEnd>,
    driver: JoinHandle<()>,
}

impl Drop for TonicAppendRows {
    fn drop(&mut self) {
        self.ended.cancel();
        self.driver.abort();
    }
}

#[async_trait::async_trait]
impl AppendRowsClient for TonicAppendRows {
    async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error> {
        let Some(requests) = &self.requests else {
            return Err(internal("SendMsg called after CloseSend".to_owned()));
        };
        if self.ended.is_cancelled() || self.ctx.err().is_some() {
            return Err(eof());
        }
        tokio::select! {
            biased;
            () = self.ended.cancelled() => Err(eof()),
            () = self.ctx.done() => Err(eof()),
            sent = requests.send(req) => sent.map_err(|_| eof()),
        }
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        if let Some(end) = &self.end {
            return Err(end.error());
        }
        match self.responses.recv().await {
            Some(Ok(response)) => Ok(response),
            Some(Err(end)) => {
                let err = end.error();
                self.end = Some(end);
                Err(err)
            }
            None => {
                self.end = Some(StreamEnd::Eof);
                Err(eof())
            }
        }
    }

    fn close_send(&mut self) {
        self.requests = None;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    /// gax retries an AppendRows open that fails with `Unavailable` (here refused by a stream
    /// interceptor, as Go tests' `grpc.WithStreamInterceptor` can), pausing within the
    /// jittered bounds; other codes are returned at once.
    #[tokio::test(start_paused = true)]
    async fn append_rows_open_retries_unavailable() {
        use std::sync::Mutex;

        use crate::testutil::fake_write_server::FakeWriteServer;

        for (code, refusals, want_attempts) in [
            (Code::Unavailable, 3, 4),
            (Code::PermissionDenied, 1, 1),
            (Code::ResourceExhausted, 1, 1),
        ] {
            let server = FakeWriteServer::new();
            let attempts = Arc::new(Mutex::new(Vec::<Instant>::new()));
            let seen = Arc::clone(&attempts);
            let interceptor: StreamInterceptor = Arc::new(move |_ctx, open| {
                let n = {
                    let mut seen = seen.lock().unwrap();
                    seen.push(Instant::now());
                    seen.len()
                };
                Box::pin(async move {
                    if n <= refusals {
                        return Err(Error::Status {
                            code,
                            message: "scripted".into(),
                        });
                    }
                    open.await
                })
            });
            let client = server
                .client()
                .unwrap()
                .with_stream_interceptor(interceptor);
            let res = client.append_rows(&Context::background()).await;
            let attempts = attempts.lock().unwrap().clone();
            assert_eq!(attempts.len(), want_attempts, "{code}");
            if code == Code::Unavailable {
                let mut stream = res.unwrap();
                stream.close_send();
                assert!(crate::write::is_eof(&stream.recv().await.unwrap_err()));
                assert_eq!(server.streams_opened(), 1);
                let mut cur = Duration::from_millis(100);
                for pair in attempts.windows(2) {
                    let pause = pair[1] - pair[0];
                    assert!(
                        pause >= Duration::from_nanos(1) && pause <= cur,
                        "{pause:?}"
                    );
                    cur = Duration::from_nanos((cur.as_nanos() as f64 * 1.3) as u64);
                }
            } else {
                let err = res.err().unwrap();
                assert_eq!(
                    err.to_string(),
                    format!("rpc error: code = {code} desc = scripted")
                );
                assert_eq!(server.streams_opened(), 0);
            }
        }
    }

    /// Shortened call options (tests) replace the defaults.
    #[tokio::test(start_paused = true)]
    async fn call_options_can_be_replaced() {
        use crate::testutil::fake_write_server::FakeWriteServer;

        let server = FakeWriteServer::new();
        server.on_finalize_write_stream(Arc::new(|_| {
            Box::pin(async { Err(tonic::Status::unavailable("down")) })
        }));
        let mut options = WriteCallOptions::default();
        options.finalize_write_stream.timeout = Some(Duration::from_secs(1));
        let client = server.client().unwrap().with_call_options(options);
        let start = Instant::now();
        let err = client
            .finalize_write_stream(
                &Context::background(),
                FinalizeWriteStreamRequest::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert_eq!(err.to_string(), "context deadline exceeded");
        assert_eq!(
            server.finalize_calls()[0].timeout,
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn metadata_is_validated_like_grpc_go() {
        let md = |k: &str, v: &str| Metadata::from([(k.to_owned(), vec![v.to_owned()])]);
        assert!(validate_metadata(&md("x-ok_1.a", "v v")).is_ok());
        assert!(validate_metadata(&md("x-raw-bin", "\u{1}")).is_ok());
        assert_eq!(
            validate_metadata(&md("X-Up", "v")).unwrap_err().to_string(),
            "rpc error: code = Internal desc = header key \"X-Up\" contains illegal characters \
             not in [0-9a-z-_.]"
        );
        assert_eq!(
            validate_metadata(&md("x-a", "é")).unwrap_err().to_string(),
            "rpc error: code = Internal desc = header key \"x-a\" contains value with \
             non-printable ASCII characters"
        );
        assert_eq!(
            validate_metadata(&md("", "v")).unwrap_err().to_string(),
            "rpc error: code = Internal desc = there is an empty key in the header"
        );
    }

    /// `gapic/call_options.json`: Go's `defaultBigQueryWriteCallOptions()` as the probe read
    /// them from a new client.
    #[test]
    fn default_call_options_match_the_probe() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/gapic/call_options.json"))
                .unwrap();
        let settings = |rpc: &str| {
            let f = &fixture[rpc];
            let timeout_ns = f["timeout_ns"].as_u64().unwrap();
            let r = &f["retry"];
            CallSettings {
                timeout: (timeout_ns != 0).then(|| Duration::from_nanos(timeout_ns)),
                retry: Some(RetrySettings {
                    codes: r["codes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| {
                            (0..17)
                                .filter_map(Code::from_u32)
                                .find(|code| code.to_string() == c.as_str().unwrap())
                                .unwrap()
                        })
                        .collect(),
                    backoff: Backoff {
                        initial: Duration::from_nanos(r["initial_ns"].as_u64().unwrap()),
                        max: Duration::from_nanos(r["max_ns"].as_u64().unwrap()),
                        multiplier: r["multiplier"].as_f64().unwrap(),
                    },
                }),
            }
        };
        assert_eq!(
            WriteCallOptions::default(),
            WriteCallOptions {
                create_write_stream: settings("CreateWriteStream"),
                append_rows: settings("AppendRows"),
                finalize_write_stream: settings("FinalizeWriteStream"),
            }
        );
    }
}
