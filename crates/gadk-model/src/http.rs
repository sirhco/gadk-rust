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

//! The pluggable HTTP transport used by model backends.

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::error::ModelError;

/// A streaming response body.
pub type BodyStream = BoxStream<'static, Result<Bytes, ModelError>>;

/// An HTTP transport. Backends send every request through this trait so tests can
/// substitute a recorder/replayer.
#[async_trait::async_trait]
pub trait HttpClient: Send + Sync {
    /// Sends `req` and returns the response with a streaming body.
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError>;
}

/// [`HttpClient`] backed by `reqwest`.
#[derive(Debug, Clone, Default)]
pub struct ReqwestClient(reqwest::Client);

impl ReqwestClient {
    /// Wraps an existing `reqwest::Client`.
    pub fn new(client: reqwest::Client) -> Self {
        Self(client)
    }
}

#[async_trait::async_trait]
impl HttpClient for ReqwestClient {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let req =
            reqwest::Request::try_from(req).map_err(|e| ModelError::Transport(Box::new(e)))?;
        let resp = self
            .0
            .execute(req)
            .await
            .map_err(|e| ModelError::Transport(Box::new(e)))?;
        let status = resp.status();
        let version = resp.version();
        let headers = resp.headers().clone();
        let body: BodyStream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| ModelError::Transport(Box::new(e))))
            .boxed();
        let mut out = http::Response::new(body);
        *out.status_mut() = status;
        *out.version_mut() = version;
        *out.headers_mut() = headers;
        Ok(out)
    }
}

/// Reads a [`BodyStream`] to the end, concatenating all chunks.
pub async fn collect_body(mut body: BodyStream) -> Result<Bytes, ModelError> {
    let mut buf = BytesMut::new();
    while let Some(chunk) = body.next().await {
        buf.extend_from_slice(&chunk?);
    }
    Ok(buf.freeze())
}
