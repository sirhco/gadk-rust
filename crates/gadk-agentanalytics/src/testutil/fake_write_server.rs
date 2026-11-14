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

#![allow(dead_code, unused_variables)] // Task 5 implements the bodies.

use std::sync::Arc;
use std::time::Duration;

use gadk_platform::BoxFuture;

use crate::config::ClientOptions;
use crate::context::Metadata;
use crate::error::Error;
use crate::storagepb::{
    AppendRowsRequest, AppendRowsResponse, CreateWriteStreamRequest, FinalizeWriteStreamRequest,
    FinalizeWriteStreamResponse, WriteStream,
};
use crate::write::gapic::WriteClient;

/// One unary call as the server saw it.
#[derive(Debug, Clone)]
pub struct UnaryCall<T> {
    /// The request.
    pub request: T,
    /// The incoming metadata (lowercase keys, values in order; tonic's own `te`,
    /// `content-type`, `user-agent`, … included).
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
    /// The incoming metadata of the stream.
    pub metadata: Metadata,
    /// Task 5: the request stream, the response sender, the recorder, cancellation.
    pub(crate) inner: Box<dyn std::any::Any + Send>,
}

impl AppendRowsServerStream {
    /// Go `stream.Recv()`: the next request (recorded), `Ok(None)` once the client closed its
    /// side (Go `io.EOF`), or the stream's error.
    ///
    /// # Errors
    ///
    /// The transport's status (e.g. the client canceled the stream).
    pub async fn recv(&mut self) -> Result<Option<AppendRowsRequest>, tonic::Status> {
        todo!("Task 5")
    }

    /// Go `stream.Send(res)`.
    ///
    /// # Errors
    ///
    /// The client has gone.
    pub async fn send(&mut self, res: AppendRowsResponse) -> Result<(), tonic::Status> {
        todo!("Task 5")
    }

    /// Go `<-stream.Context().Done()`: resolves once the client has canceled or dropped the
    /// stream.
    pub async fn cancelled(&self) {
        todo!("Task 5")
    }
}

/// A `CreateWriteStream` hook (Go: a `CreateWriteStream` method or a unary server
/// interceptor).
pub type CreateWriteStreamHook = Arc<
    dyn Fn(
            UnaryCall<CreateWriteStreamRequest>,
        ) -> BoxFuture<'static, Result<WriteStream, tonic::Status>>
        + Send
        + Sync,
>;

/// A `FinalizeWriteStream` hook.
pub type FinalizeWriteStreamHook = Arc<
    dyn Fn(
            UnaryCall<FinalizeWriteStreamRequest>,
        ) -> BoxFuture<'static, Result<FinalizeWriteStreamResponse, tonic::Status>>
        + Send
        + Sync,
>;

/// An `AppendRows` hook: serves one whole stream (Go's `AppendRows(stream) error` method);
/// returning ends the stream with OK or the error's status.
pub type AppendRowsHook = Arc<
    dyn Fn(AppendRowsServerStream) -> BoxFuture<'static, Result<(), tonic::Status>> + Send + Sync,
>;

/// The in-process fake BigQuery Write server. Cheap to clone (shared state).
#[derive(Clone)]
pub struct FakeWriteServer {
    /// Task 5: hooks, recordings, the incoming-connection sender.
    pub(crate) inner: Arc<dyn std::any::Any + Send + Sync>,
}

impl Default for FakeWriteServer {
    fn default() -> Self {
        FakeWriteServer::new()
    }
}

impl FakeWriteServer {
    /// A server with the default behaviour, serving from now on (needs a Tokio runtime).
    pub fn new() -> FakeWriteServer {
        todo!("Task 5")
    }

    /// Replaces the `CreateWriteStream` hook (takes effect for the next call).
    pub fn on_create_write_stream(&self, hook: CreateWriteStreamHook) {
        todo!("Task 5")
    }

    /// Replaces the `FinalizeWriteStream` hook.
    pub fn on_finalize_write_stream(&self, hook: FinalizeWriteStreamHook) {
        todo!("Task 5")
    }

    /// Replaces the `AppendRows` hook (takes effect for the next stream).
    pub fn on_append_rows(&self, hook: AppendRowsHook) {
        todo!("Task 5")
    }

    /// A [`WriteClient`] connected to this server over a new in-memory connection, without
    /// authentication and with the default call options.
    ///
    /// # Errors
    ///
    /// Building the channel failed.
    pub fn client(&self) -> Result<WriteClient, Error> {
        todo!("Task 5")
    }

    /// [`client`](Self::client) with `options`' credentials (to observe `authorization`).
    ///
    /// # Errors
    ///
    /// Building the channel or resolving the credentials failed.
    pub fn client_with_options(&self, options: &ClientOptions) -> Result<WriteClient, Error> {
        todo!("Task 5")
    }

    /// Every `CreateWriteStream` call so far, in order.
    pub fn create_calls(&self) -> Vec<UnaryCall<CreateWriteStreamRequest>> {
        todo!("Task 5")
    }

    /// Every `FinalizeWriteStream` call so far, in order.
    pub fn finalize_calls(&self) -> Vec<UnaryCall<FinalizeWriteStreamRequest>> {
        todo!("Task 5")
    }

    /// Every AppendRows request received so far, in order.
    pub fn append_requests(&self) -> Vec<RecordedAppend> {
        todo!("Task 5")
    }

    /// The incoming metadata of every AppendRows stream opened so far, in opening order.
    pub fn append_stream_metadata(&self) -> Vec<Metadata> {
        todo!("Task 5")
    }

    /// How many AppendRows streams were opened (Go `streamsOpened`).
    pub fn streams_opened(&self) -> usize {
        todo!("Task 5")
    }

    /// Resolves once at least `n` AppendRows requests have been received (Go tests reading
    /// the `requests` channel).
    pub async fn wait_for_append_requests(&self, n: usize) {
        todo!("Task 5")
    }
}
