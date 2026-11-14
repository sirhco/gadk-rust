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

//! Port of the call layer of Go's generated `cloud.google.com/go/bigquery/storage/apiv1`
//! `BigQueryWriteClient` (`big_query_write_client.go`, bigquery v1.79.0) for the three RPCs
//! ADK-Go's `plugin/agentanalytics` calls — `CreateWriteStream`, `AppendRows`,
//! `FinalizeWriteStream` — over the generated tonic client of [`crate::storagepb`]: per-RPC
//! `x-goog-request-params` (gax `InsertMetadataIntoOutgoingContext`), `x-goog-api-client`,
//! bearer `authorization`, the caller's outgoing metadata from the [`Context`], and Go's
//! `defaultBigQueryWriteCallOptions` timeouts, retry codes and backoffs (gax `Invoke`).

#![allow(dead_code, unused_variables)] // Task 5 implements the bodies.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use gadk_platform::BoxFuture;
use google_cloud_auth::credentials::Credentials;
use tonic::transport::Channel;

use crate::config::ClientOptions;
use crate::context::Context;
use crate::error::{Code, Error};
use crate::storagepb::big_query_write_client::BigQueryWriteClient;
use crate::storagepb::{
    CreateWriteStreamRequest, FinalizeWriteStreamRequest, FinalizeWriteStreamResponse, WriteStream,
};
use crate::write::AppendRowsClient;

/// Go `defaultBigQueryWriteGRPCClientOptions`' `WithDefaultEndpoint`: the gRPC endpoint (TLS).
pub(crate) const DEFAULT_ENDPOINT: &str = "https://bigquerystorage.googleapis.com:443";

/// Go `DefaultAuthScopes()` (`helpers.go`).
pub(crate) const DEFAULT_AUTH_SCOPES: [&str; 3] = [
    "https://www.googleapis.com/auth/bigquery",
    "https://www.googleapis.com/auth/bigquery.insertdata",
    "https://www.googleapis.com/auth/cloud-platform",
];

/// gax `Backoff`: the pause before retry *n* is `1 + rand(cur)` ns (jittered), then
/// `cur = min(cur * multiplier, max)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Backoff {
    /// gax `Initial`.
    pub(crate) initial: Duration,
    /// gax `Max`.
    pub(crate) max: Duration,
    /// gax `Multiplier`.
    pub(crate) multiplier: f64,
}

/// gax `OnCodes(codes, backoff)`: the retryer of one RPC.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetrySettings {
    /// The status codes that are retried.
    pub(crate) codes: Vec<Code>,
    /// The pauses between attempts.
    pub(crate) backoff: Backoff,
}

/// gax `CallSettings` of one RPC: `WithTimeout` (applied only when the call's context has no
/// deadline) and `WithRetry`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct CallSettings {
    /// gax `WithTimeout`; `None` = none.
    pub(crate) timeout: Option<Duration>,
    /// gax `WithRetry`; `None` = no retries.
    pub(crate) retry: Option<RetrySettings>,
}

/// Go `BigQueryWriteCallOptions` for the RPCs ADK-Go calls.
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
    /// Go `defaultBigQueryWriteCallOptions()`.
    fn default() -> Self {
        todo!("Task 5")
    }
}

/// An AppendRows stream that is about to be opened: awaiting it opens the stream (one attempt,
/// inside the open retry loop).
pub(crate) type OpenAppendRows = BoxFuture<'static, Result<Box<dyn AppendRowsClient>, Error>>;

/// A test hook in the place of grpc-go's `grpc.WithStreamInterceptor` dial option: called for
/// every AppendRows stream the client opens (each attempt), with the call's context and the
/// opening; it may await the opening and wrap the stream, or refuse without opening.
pub(crate) type StreamInterceptor = Arc<
    dyn Fn(Context, OpenAppendRows) -> BoxFuture<'static, Result<Box<dyn AppendRowsClient>, Error>>
        + Send
        + Sync,
>;

/// A BigQuery Storage Write API client (Go `*bqstorage.BigQueryWriteClient`): a cheap `Clone`
/// handle on one tonic channel.
#[derive(Clone)]
pub struct WriteClient {
    /// The generated tonic client.
    pub(crate) inner: BigQueryWriteClient<Channel>,
    /// The bearer-token source; `None` sends no `authorization` (Go
    /// `option.WithoutAuthentication`).
    pub(crate) credentials: Option<Credentials>,
    /// Per-RPC timeouts and retries.
    pub(crate) call_options: Arc<WriteCallOptions>,
    /// The `x-goog-api-client` value (`gadk_platform::tracking_header()`).
    pub(crate) api_client_header: String,
    /// Test hook on opening AppendRows streams.
    pub(crate) stream_interceptor: Option<StreamInterceptor>,
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
    /// Go `bqstorage.NewBigQueryWriteClient(ctx, opts...)`: a lazily connecting channel to
    /// [`ClientOptions::endpoint`] (`http://` = plaintext) or `bigquerystorage.googleapis.com:443`
    /// (TLS, native roots), with [`ClientOptions::credentials`], none when
    /// [`ClientOptions::without_authentication`], else Application Default Credentials with
    /// Go's `DefaultAuthScopes()`. Must run inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// An invalid endpoint, or credentials that cannot be resolved.
    pub async fn new(options: ClientOptions) -> Result<WriteClient, Error> {
        todo!("Task 5")
    }

    /// A client on an existing channel (the in-process fake's duplex channel), with
    /// `options`' credentials as [`new`](Self::new) resolves them and the default call options.
    ///
    /// # Errors
    ///
    /// Credentials that cannot be resolved.
    pub(crate) fn from_channel(
        channel: Channel,
        options: &ClientOptions,
    ) -> Result<WriteClient, Error> {
        todo!("Task 5")
    }

    /// Replaces the per-RPC timeouts and retries (tests shorten the backoffs).
    pub(crate) fn with_call_options(self, call_options: WriteCallOptions) -> WriteClient {
        todo!("Task 5")
    }

    /// Installs a hook on opening AppendRows streams (Go tests' `grpc.WithStreamInterceptor`).
    pub(crate) fn with_stream_interceptor(self, interceptor: StreamInterceptor) -> WriteClient {
        todo!("Task 5")
    }

    /// Go `CreateWriteStream(ctx, req)`: `x-goog-request-params: parent=<QueryEscape(parent)>`,
    /// retried and bounded per [`WriteCallOptions::create_write_stream`] and `ctx`.
    ///
    /// # Errors
    ///
    /// The final status ([`Error::Status`]) or the context's error.
    pub async fn create_write_stream(
        &self,
        ctx: &Context,
        req: CreateWriteStreamRequest,
    ) -> Result<WriteStream, Error> {
        todo!("Task 5")
    }

    /// Go `AppendRows(ctx)`: opens a bidirectional AppendRows stream that ends when `ctx` ends
    /// (its `send` / `recv` then fail with the context's status), carrying `ctx`'s outgoing
    /// metadata and `x-goog-api-client`; opening is retried on `Unavailable` per
    /// [`WriteCallOptions::append_rows`]. Unlike grpc-go, tonic waits for the response headers
    /// while opening, so a server that fails at stream start fails here.
    ///
    /// # Errors
    ///
    /// The status that ended the opening, or the context's error.
    pub async fn append_rows(&self, ctx: &Context) -> Result<Box<dyn AppendRowsClient>, Error> {
        todo!("Task 5")
    }

    /// Go `FinalizeWriteStream(ctx, req)`: `x-goog-request-params: name=<QueryEscape(name)>`,
    /// retried and bounded per [`WriteCallOptions::finalize_write_stream`] and `ctx`.
    ///
    /// # Errors
    ///
    /// The final status ([`Error::Status`]) or the context's error.
    pub async fn finalize_write_stream(
        &self,
        ctx: &Context,
        req: FinalizeWriteStreamRequest,
    ) -> Result<FinalizeWriteStreamResponse, Error> {
        todo!("Task 5")
    }

    /// Go `Close()`: releases the client (the channel closes once every clone is dropped).
    ///
    /// # Errors
    ///
    /// None today (Go's `Close` reports the connection pool's error).
    pub async fn close(&self) -> Result<(), Error> {
        todo!("Task 5")
    }
}
