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

//! An in-process BigQuery Storage Write server (feature `testutil`), in the place of the
//! `grpc.NewServer` fakes of ADK-Go's `batch_processor_test.go` and
//! `bigquery_agent_analytics_plugin_test.go`'s `fakeBigQueryWriteServer`: the generated tonic
//! `BigQueryWrite` service, served over an in-memory `tokio::io::duplex` connector (no sockets,
//! so Tokio's paused time auto-advances while the client waits), with scriptable RPC hooks
//! (async, so they may block on the test's signals) and recorded requests and metadata.
//!
//! Defaults (no hook installed): `CreateWriteStream` answers `<parent>/streams/committed-<n>`
//! with the requested type; `FinalizeWriteStream` answers an empty response; `AppendRows`
//! answers every request with an empty `AppendRowsResponse` until the client closes the stream
//! (Go's `fakeBigQueryWriteServer`). Every request is recorded before its hook runs; AppendRows
//! requests as the hook receives them through [`AppendRowsServerStream::recv`].

//!
//! A hook that returns ends its RPC (an AppendRows hook: the stream ends with OK or the
//! returned status, as Go's handler returning). The response headers of an AppendRows stream
//! are sent as soon as the stream arrives, before the hook runs (grpc-go sends them with the
//! first response or the status; the client does not wait for them either way).

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gadk_platform::BoxFuture;
use hyper_util::rt::TokioIo;
use tokio::sync::{mpsc, watch};
use tonic::codegen::tokio_stream::StreamExt as _;
use tonic::codegen::tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use tonic::transport::{Endpoint, Server};
use tonic::{Request, Response, Status, Streaming};

use crate::config::ClientOptions;
use crate::context::Metadata;
use crate::error::Error;
use crate::storagepb::big_query_write_server::{BigQueryWrite, BigQueryWriteServer};
use crate::storagepb::{
    AppendRowsRequest, AppendRowsResponse, BatchCommitWriteStreamsRequest,
    BatchCommitWriteStreamsResponse, CreateWriteStreamRequest, FinalizeWriteStreamRequest,
    FinalizeWriteStreamResponse, FlushRowsRequest, FlushRowsResponse, GetWriteStreamRequest,
    WriteStream,
};
use crate::write::gapic::WriteClient;

/// The in-memory pipe size of one client connection.
const DUPLEX_BUFFER: usize = 1 << 20;

/// The authority the fake's channels name (never resolved: the connector ignores it).
const FAKE_URI: &str = "http://fake-bigquery-write.invalid";

/// One unary call as the server saw it.
#[derive(Debug, Clone)]
pub struct UnaryCall<T> {
    /// The request.
    pub request: T,
    /// The incoming metadata (lowercase keys, values in order; tonic's own `te`,
    /// `content-type`, `user-agent`, … included; `grpc-timeout` is [`timeout`](Self::timeout)).
    pub metadata: Metadata,
    /// The call's deadline as the server received it (`grpc-timeout`), if any.
    pub timeout: Option<Duration>,
    /// The 1-based number of this call among the calls of its RPC.
    pub call_no: usize,
}

/// One AppendRows request as the server received it.
#[derive(Debug, Clone)]
pub struct RecordedAppend {
    /// The 1-based number of the stream it arrived on (in opening order).
    pub stream_no: usize,
    /// The request.
    pub request: AppendRowsRequest,
}

/// The server half of one AppendRows stream (Go `storagepb.BigQueryWrite_AppendRowsServer`).
pub struct AppendRowsServerStream {
    /// The 1-based number of this stream (in opening order).
    pub stream_no: usize,
    /// The incoming metadata of the stream (`grpc-timeout` left out, as for [`UnaryCall`]).
    pub metadata: Metadata,
    /// The request stream, the response sender, the recorder.
    pub(crate) inner: StreamInner,
}

/// The private state of an [`AppendRowsServerStream`].
pub(crate) struct StreamInner {
    requests: Streaming<AppendRowsRequest>,
    responses: mpsc::Sender<Result<AppendRowsResponse, Status>>,
    shared: Arc<Shared>,
}

impl AppendRowsServerStream {
    /// Go `stream.Recv()`: the next request (recorded), `Ok(None)` once the client closed its
    /// side (Go `io.EOF`), or the stream's error.
    ///
    /// # Errors
    ///
    /// The transport's status (e.g. the client canceled the stream).
    pub async fn recv(&mut self) -> Result<Option<AppendRowsRequest>, Status> {
        let req = self.inner.requests.message().await?;
        if let Some(req) = &req {
            let mut appends = lock(&self.inner.shared.appends);
            appends.push(RecordedAppend {
                stream_no: self.stream_no,
                request: req.clone(),
            });
            self.inner.shared.append_count.send_replace(appends.len());
        }
        Ok(req)
    }

    /// Go `stream.Send(res)`.
    ///
    /// # Errors
    ///
    /// The client has gone (`Canceled`).
    pub async fn send(&mut self, res: AppendRowsResponse) -> Result<(), Status> {
        self.inner
            .responses
            .send(Ok(res))
            .await
            .map_err(|_| Status::cancelled("context canceled"))
    }

    /// Go `<-stream.Context().Done()`: resolves once the client has canceled or dropped the
    /// stream.
    pub async fn cancelled(&self) {
        self.inner.responses.closed().await;
    }
}

/// A `CreateWriteStream` hook (Go: a `CreateWriteStream` method or a unary server
/// interceptor).
pub type CreateWriteStreamHook = Arc<
    dyn Fn(UnaryCall<CreateWriteStreamRequest>) -> BoxFuture<'static, Result<WriteStream, Status>>
        + Send
        + Sync,
>;

/// A `FinalizeWriteStream` hook.
pub type FinalizeWriteStreamHook = Arc<
    dyn Fn(
            UnaryCall<FinalizeWriteStreamRequest>,
        ) -> BoxFuture<'static, Result<FinalizeWriteStreamResponse, Status>>
        + Send
        + Sync,
>;

/// An `AppendRows` hook: serves one whole stream (Go's `AppendRows(stream) error` method);
/// returning ends the stream with OK or the error's status.
pub type AppendRowsHook =
    Arc<dyn Fn(AppendRowsServerStream) -> BoxFuture<'static, Result<(), Status>> + Send + Sync>;

/// The state the server and every handle share.
pub(crate) struct Shared {
    create_hook: Mutex<Option<CreateWriteStreamHook>>,
    finalize_hook: Mutex<Option<FinalizeWriteStreamHook>>,
    append_hook: Mutex<Option<AppendRowsHook>>,
    create_calls: Mutex<Vec<UnaryCall<CreateWriteStreamRequest>>>,
    finalize_calls: Mutex<Vec<UnaryCall<FinalizeWriteStreamRequest>>>,
    appends: Mutex<Vec<RecordedAppend>>,
    append_count: watch::Sender<usize>,
    stream_metadata: Mutex<Vec<Metadata>>,
    /// New client connections (the server halves of in-memory pipes).
    connections: mpsc::UnboundedSender<tokio::io::DuplexStream>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The in-process fake BigQuery Write server. Cheap to clone (shared state).
#[derive(Clone)]
pub struct FakeWriteServer {
    /// Hooks, recordings, the incoming-connection sender.
    pub(crate) inner: Arc<Shared>,
}

impl Default for FakeWriteServer {
    fn default() -> Self {
        FakeWriteServer::new()
    }
}

impl FakeWriteServer {
    /// A server with the default behaviour, serving from now on (needs a Tokio runtime; the
    /// server task ends with the runtime).
    pub fn new() -> FakeWriteServer {
        let (connections, incoming) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            create_hook: Mutex::new(None),
            finalize_hook: Mutex::new(None),
            append_hook: Mutex::new(None),
            create_calls: Mutex::new(Vec::new()),
            finalize_calls: Mutex::new(Vec::new()),
            appends: Mutex::new(Vec::new()),
            append_count: watch::Sender::new(0),
            stream_metadata: Mutex::new(Vec::new()),
            connections,
        });
        let service = BigQueryWriteServer::new(Service {
            shared: Arc::clone(&shared),
        });
        let incoming = UnboundedReceiverStream::new(incoming).map(Ok::<_, io::Error>);
        tokio::spawn(async move {
            let _ = Server::builder()
                .add_service(service)
                .serve_with_incoming(incoming)
                .await;
        });
        FakeWriteServer { inner: shared }
    }

    /// Replaces the `CreateWriteStream` hook (takes effect for the next call).
    pub fn on_create_write_stream(&self, hook: CreateWriteStreamHook) {
        *lock(&self.inner.create_hook) = Some(hook);
    }

    /// Replaces the `FinalizeWriteStream` hook.
    pub fn on_finalize_write_stream(&self, hook: FinalizeWriteStreamHook) {
        *lock(&self.inner.finalize_hook) = Some(hook);
    }

    /// Replaces the `AppendRows` hook (takes effect for the next stream).
    pub fn on_append_rows(&self, hook: AppendRowsHook) {
        *lock(&self.inner.append_hook) = Some(hook);
    }

    /// A [`WriteClient`] connected to this server over a new in-memory connection, without
    /// authentication and with the default call options.
    ///
    /// # Errors
    ///
    /// Building the channel failed.
    pub fn client(&self) -> Result<WriteClient, Error> {
        self.client_with_options(&ClientOptions {
            without_authentication: true,
            ..ClientOptions::default()
        })
    }

    /// [`client`](Self::client) with `options`' credentials (to observe `authorization`) and
    /// header options; `options.endpoint` and `options.http_client` are ignored.
    ///
    /// # Errors
    ///
    /// Building the channel or resolving the credentials failed.
    pub fn client_with_options(&self, options: &ClientOptions) -> Result<WriteClient, Error> {
        let connections = self.inner.connections.clone();
        let connector = tower::service_fn(move |_: http::Uri| {
            let connections = connections.clone();
            async move {
                let (client, server) = tokio::io::duplex(DUPLEX_BUFFER);
                connections
                    .send(server)
                    .map_err(|_| io::Error::other("the fake BigQuery Write server has stopped"))?;
                Ok::<_, io::Error>(TokioIo::new(client))
            }
        });
        let channel = Endpoint::from_static(FAKE_URI).connect_with_connector_lazy(connector);
        WriteClient::from_channel(channel, options)
    }

    /// Every `CreateWriteStream` call so far, in order.
    pub fn create_calls(&self) -> Vec<UnaryCall<CreateWriteStreamRequest>> {
        lock(&self.inner.create_calls).clone()
    }

    /// Every `FinalizeWriteStream` call so far, in order.
    pub fn finalize_calls(&self) -> Vec<UnaryCall<FinalizeWriteStreamRequest>> {
        lock(&self.inner.finalize_calls).clone()
    }

    /// Every AppendRows request received so far, in order.
    pub fn append_requests(&self) -> Vec<RecordedAppend> {
        lock(&self.inner.appends).clone()
    }

    /// The incoming metadata of every AppendRows stream opened so far, in opening order.
    pub fn append_stream_metadata(&self) -> Vec<Metadata> {
        lock(&self.inner.stream_metadata).clone()
    }

    /// How many AppendRows streams were opened (Go `streamsOpened`).
    pub fn streams_opened(&self) -> usize {
        lock(&self.inner.stream_metadata).len()
    }

    /// Resolves once at least `n` AppendRows requests have been received (Go tests reading
    /// the `requests` channel).
    pub async fn wait_for_append_requests(&self, n: usize) {
        let mut count = self.inner.append_count.subscribe();
        // The sender lives in `self.inner`, so the watch cannot close while we wait.
        let _ = count.wait_for(|got| *got >= n).await;
    }
}

/// The incoming metadata of a call (`grpc-timeout` split off as the deadline).
fn incoming(md: &tonic::metadata::MetadataMap) -> (Metadata, Option<Duration>) {
    let mut out = Metadata::new();
    let mut timeout = None;
    for (k, v) in &md.clone().into_headers() {
        if k.as_str() == "grpc-timeout" {
            timeout = v.to_str().ok().and_then(parse_grpc_timeout);
            continue;
        }
        out.entry(k.as_str().to_owned())
            .or_default()
            .push(String::from_utf8_lossy(v.as_bytes()).into_owned());
    }
    (out, timeout)
}

/// A `grpc-timeout` value: at most 8 digits and a unit (`H`, `M`, `S`, `m`, `u`, `n`).
fn parse_grpc_timeout(v: &str) -> Option<Duration> {
    let unit = v.chars().last()?;
    let n: u64 = v.get(..v.len() - unit.len_utf8())?.parse().ok()?;
    Some(match unit {
        'H' => Duration::from_secs(n * 3600),
        'M' => Duration::from_secs(n * 60),
        'S' => Duration::from_secs(n),
        'm' => Duration::from_millis(n),
        'u' => Duration::from_micros(n),
        'n' => Duration::from_nanos(n),
        _ => return None,
    })
}

/// Records a unary call and returns it with its number.
fn record<T: Clone>(calls: &Mutex<Vec<UnaryCall<T>>>, request: Request<T>) -> UnaryCall<T> {
    let (metadata, timeout) = incoming(request.metadata());
    let mut calls = lock(calls);
    let call = UnaryCall {
        request: request.into_inner(),
        metadata,
        timeout,
        call_no: calls.len() + 1,
    };
    calls.push(call.clone());
    call
}

/// Go's default `AppendRows` of `fakeBigQueryWriteServer`: an empty response per request
/// until the client closes its side.
async fn default_append_rows(mut stream: AppendRowsServerStream) -> Result<(), Status> {
    while stream.recv().await?.is_some() {
        stream.send(AppendRowsResponse::default()).await?;
    }
    Ok(())
}

/// The tonic service over the shared state.
struct Service {
    shared: Arc<Shared>,
}

/// grpc-go `UnimplementedBigQueryWriteServer`'s status.
fn unimplemented(method: &str) -> Status {
    Status::unimplemented(format!("method {method} not implemented"))
}

#[tonic::async_trait]
impl BigQueryWrite for Service {
    async fn create_write_stream(
        &self,
        request: Request<CreateWriteStreamRequest>,
    ) -> Result<Response<WriteStream>, Status> {
        let call = record(&self.shared.create_calls, request);
        let hook = lock(&self.shared.create_hook).clone();
        let stream = match hook {
            Some(hook) => hook(call).await?,
            None => WriteStream {
                name: format!("{}/streams/committed-{}", call.request.parent, call.call_no),
                r#type: call.request.write_stream.as_ref().map_or(0, |w| w.r#type),
                ..WriteStream::default()
            },
        };
        Ok(Response::new(stream))
    }

    type AppendRowsStream = ReceiverStream<Result<AppendRowsResponse, Status>>;

    async fn append_rows(
        &self,
        request: Request<Streaming<AppendRowsRequest>>,
    ) -> Result<Response<Self::AppendRowsStream>, Status> {
        let (metadata, _) = incoming(request.metadata());
        let stream_no = {
            let mut all = lock(&self.shared.stream_metadata);
            all.push(metadata.clone());
            all.len()
        };
        let (responses, rx) = mpsc::channel(16);
        let stream = AppendRowsServerStream {
            stream_no,
            metadata,
            inner: StreamInner {
                requests: request.into_inner(),
                responses: responses.clone(),
                shared: Arc::clone(&self.shared),
            },
        };
        let hook = lock(&self.shared.append_hook).clone();
        tokio::spawn(async move {
            let res = match hook {
                Some(hook) => hook(stream).await,
                None => default_append_rows(stream).await,
            };
            if let Err(status) = res {
                let _ = responses.send(Err(status)).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn get_write_stream(
        &self,
        _request: Request<GetWriteStreamRequest>,
    ) -> Result<Response<WriteStream>, Status> {
        Err(unimplemented("GetWriteStream"))
    }

    async fn finalize_write_stream(
        &self,
        request: Request<FinalizeWriteStreamRequest>,
    ) -> Result<Response<FinalizeWriteStreamResponse>, Status> {
        let call = record(&self.shared.finalize_calls, request);
        let hook = lock(&self.shared.finalize_hook).clone();
        let response = match hook {
            Some(hook) => hook(call).await?,
            None => FinalizeWriteStreamResponse::default(),
        };
        Ok(Response::new(response))
    }

    async fn batch_commit_write_streams(
        &self,
        _request: Request<BatchCommitWriteStreamsRequest>,
    ) -> Result<Response<BatchCommitWriteStreamsResponse>, Status> {
        Err(unimplemented("BatchCommitWriteStreams"))
    }

    async fn flush_rows(
        &self,
        _request: Request<FlushRowsRequest>,
    ) -> Result<Response<FlushRowsResponse>, Status> {
        Err(unimplemented("FlushRows"))
    }
}
