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

/// `err` and its sources, joined with `": "` (moved from gadk-anthropic in 3b so every REST
/// provider renders a cause chain the same way).
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(e) = cur {
        out.push_str(": ");
        out.push_str(&e.to_string());
        cur = e.source();
    }
    out
}

/// The text of a [`ModelError::Transport`] cause: a `reqwest::Error` loses its URL (Go's
/// provider error texts carry none), then the source chain is joined with `": "`
/// ([`error_chain`]).
pub fn transport_error_detail(err: Box<dyn std::error::Error + Send + Sync>) -> String {
    match err.downcast::<reqwest::Error>() {
        Ok(e) => error_chain(&e.without_url()),
        Err(other) => error_chain(other.as_ref()),
    }
}

/// `url` for `Debug` output: credentials embedded in it (`https://user:secret@host`) are
/// removed; a URL without user info is returned as it is, and one that does not parse, has no
/// host but contains `@`, or whose user info cannot be removed becomes `<redacted>`.
pub fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return "<redacted>".to_string();
    };
    // Without an authority, an `@` may be user info the parser did not recognise (e.g.
    // `user:secret@host` parses as scheme `user`), so the whole value is withheld.
    if !parsed.has_host() && url.contains('@') {
        return "<redacted>".to_string();
    }
    if parsed.username().is_empty() && parsed.password().is_none() {
        return url.to_string();
    }
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return "<redacted>".to_string();
    }
    parsed.to_string()
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn redact_url_removes_user_info() {
        assert_eq!(
            redact_url("https://user:secret@proxy.example/v1"),
            "https://proxy.example/v1"
        );
        assert_eq!(
            redact_url("https://token@proxy.example"),
            "https://proxy.example/"
        );
        assert_eq!(
            redact_url("https://:secret@proxy.example/"),
            "https://proxy.example/"
        );
        // No user info: unchanged (not normalized).
        assert_eq!(redact_url("https://proxy.example"), "https://proxy.example");
        assert_eq!(redact_url("not a url user:secret@"), "<redacted>");
        // Scheme-less user info parses as scheme `user` with no authority.
        assert_eq!(redact_url("user:secret@proxy.example:8080"), "<redacted>");
        assert_eq!(redact_url("user:secret@proxy.example/v1"), "<redacted>");
    }
}
