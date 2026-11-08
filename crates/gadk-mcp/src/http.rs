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

//! The HTTP client of MCP streamable-HTTP sessions (port of ADK-Go
//! `tool/mcptoolset/tracking.go` `trackingTransport` and set.go `httpClient`): the ADK tracking
//! headers on every request and, with auth, the credential's headers with one refresh on an
//! auth rejection.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use gadk_auth::{
    AuthContext, CredentialProvider, apply_credential, find_consent, is_auth_rejected,
};
use http::header::{HeaderMap, USER_AGENT};
use http::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::common::client_side_sse::BoxedSseResponse;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};

/// The header that carries the ADK tracking tokens next to `user-agent` (Go `trackingTransport`).
const X_GOOG_API_CLIENT: HeaderName = HeaderName::from_static("x-goog-api-client");

/// What every delegated `reqwest::Client` call returns on error.
type HttpError = StreamableHttpError<reqwest::Error>;

/// A `reqwest::Client` that adds the tracking headers, the configured headers and (with `auth`)
/// the credential's headers to every MCP request, delegating to rmcp's `reqwest::Client`
/// implementation.
#[derive(Clone)]
pub(crate) struct AdkHttpClient {
    /// The client requests go through.
    pub(crate) inner: reqwest::Client,
    /// The transport's configured headers (`user-agent` and `x-goog-api-client` are merged after
    /// the tracking tokens).
    pub(crate) headers: http::HeaderMap,
    /// The provider and the identity of the session's creator.
    pub(crate) auth: Option<(Arc<dyn CredentialProvider>, AuthContext)>,
}

impl AdkHttpClient {
    /// The request's headers before the credential and the tracking step: `custom_headers`
    /// (rmcp's own), then the configured headers, which replace a name rmcp also set and keep
    /// every value they repeat.
    fn request_headers(&self, custom_headers: HashMap<HeaderName, HeaderValue>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in custom_headers {
            headers.insert(name, value);
        }
        for name in self.headers.keys() {
            headers.remove(name);
            for value in self.headers.get_all(name) {
                headers.append(name.clone(), value.clone());
            }
        }
        headers
    }

    /// Go `trackingTransport.RoundTrip`'s header step: `custom_headers` (rmcp's own) extended
    /// with the configured headers, then `user-agent` and `x-goog-api-client` set to the ADK
    /// tracking tokens followed by the tokens of the request's values (rmcp's and the configured
    /// ones; see [`tracked`]), without duplicates. A name the configured headers repeat is sent
    /// once, its values joined with `", "`. No credential is applied.
    // `send` applies the credential between the two steps, so only tests call this seam.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn tracked_headers(
        &self,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HashMap<HeaderName, HeaderValue> {
        tracked(&self.request_headers(custom_headers))
    }

    /// Sends one request through `call` with, in Go's order (set.go `httpClient` wraps
    /// `auth.Transport` around `trackingTransport`), the credential's headers applied to the
    /// request's headers and then the tracking step, so a `user-agent` or `x-goog-api-client`
    /// the credential writes is merged after the ADK tokens, on the first send and the replay.
    ///
    /// On an auth rejection ([`is_rejection`]) by a refreshing provider, the credential is
    /// refreshed once and the request replayed with the fresh one applied to the request's
    /// headers as they were before the stale credential (Go `applyAndSend`); a second rejection
    /// is returned as is. As in Go `auth.Transport.RoundTrip`, any refresh error degrades to the
    /// original rejection unless it carries interactive consent (then it is returned), and only
    /// an error applying the fresh credential is returned as an apply error. Refresh and apply
    /// run as separate steps, so a provider's refresh error that is itself an
    /// `Error::ApplyCredential` still degrades. Credential errors are [`credential_error`]s.
    async fn send<T, F, Fut>(
        &self,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        call: F,
    ) -> Result<T, HttpError>
    where
        F: Fn(HashMap<HeaderName, HeaderValue>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, HttpError>> + Send,
        T: Send,
    {
        let request = self.request_headers(custom_headers);
        let Some((provider, cx)) = &self.auth else {
            return call(tracked(&request)).await;
        };
        let mut first = request.clone();
        let cred = apply_credential(provider.as_ref(), cx, &mut first)
            .await
            .map_err(credential_error)?;
        let rejection = match call(tracked(&first)).await {
            Err(err) if is_rejection(&err) => err,
            other => return other,
        };
        let Some(refresher) = provider.as_refreshing() else {
            return Err(rejection);
        };
        let fresh = match refresher.refresh(cx, &cred).await {
            Ok(fresh) => fresh,
            // Interactive consent is something the caller can act on; every other refresh
            // failure degrades to the rejection the server actually sent.
            Err(err) if find_consent(&err).is_some() => return Err(credential_error(err)),
            Err(_) => return Err(rejection),
        };
        // Go `applyAndSend` on the fresh credential: its apply error is returned.
        let mut replay = request;
        fresh
            .apply(&mut replay)
            .await
            .map_err(|e| credential_error(gadk_auth::Error::ApplyCredential(Box::new(e))))?;
        call(tracked(&replay)).await
    }
}

impl StreamableHttpClient for AdkHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let inner = &self.inner;
        self.send(custom_headers, move |headers| {
            inner.post_message(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                auth_header.clone(),
                headers,
            )
        })
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let inner = &self.inner;
        self.send(custom_headers, move |headers| {
            inner.post_message_with_max_sse_event_size(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                auth_header.clone(),
                headers,
                max_sse_event_size,
            )
        })
        .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let inner = &self.inner;
        self.send(custom_headers, move |headers| {
            inner.delete_session(
                uri.clone(),
                session_id.clone(),
                auth_header.clone(),
                headers,
            )
        })
        .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        let inner = &self.inner;
        self.send(custom_headers, move |headers| {
            inner.get_stream(
                uri.clone(),
                session_id.clone(),
                last_event_id.clone(),
                auth_header.clone(),
                headers,
            )
        })
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        let inner = &self.inner;
        self.send(custom_headers, move |headers| {
            inner.get_stream_with_max_sse_event_size(
                uri.clone(),
                session_id.clone(),
                last_event_id.clone(),
                auth_header.clone(),
                headers,
                max_sse_event_size,
            )
        })
        .await
    }
}

/// Go `trackingTransport` for one header name: the ADK tracking tokens, then the whitespace-split
/// tokens of `existing` (the request's values, in order) not already present, joined by one
/// space.
pub(crate) fn merge_tracking_tokens(existing: &[&str]) -> String {
    let tracking = gadk_platform::tracking_header();
    let mut tokens: Vec<&str> = tracking.split_whitespace().collect();
    for token in existing.iter().flat_map(|v| v.split_whitespace()) {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens.join(" ")
}

/// Whether a delegated call's error is an auth rejection (401 or 403) in any of the shapes rmcp's
/// `reqwest::Client` gives it: `AuthRequired`/`InsufficientScope` (with `WWW-Authenticate`),
/// `Client` from `error_for_status` (`get_stream`, `delete_session`), or a POST's
/// `UnexpectedServerResponse("HTTP 401 …")` without a challenge.
pub(crate) fn is_rejection(err: &HttpError) -> bool {
    match err {
        StreamableHttpError::AuthRequired(_) | StreamableHttpError::InsufficientScope(_) => true,
        StreamableHttpError::Client(e) => e.status().is_some_and(is_auth_rejected),
        StreamableHttpError::UnexpectedServerResponse(msg) => {
            msg.starts_with("HTTP 401") || msg.starts_with("HTTP 403")
        }
        _ => false,
    }
}

/// A credential error as the transport reports it: `StreamableHttpError::Io` whose message is
/// gadk-auth's text (`auth: resolve credential: …`, `auth: apply credential: …`). Consent stays
/// findable along the `source()` chain.
fn credential_error(err: gadk_auth::Error) -> HttpError {
    StreamableHttpError::Io(std::io::Error::other(err))
}

/// Go `trackingTransport.RoundTrip` over a request's final headers: each name sent once, its
/// values joined with `", "`, and `user-agent`/`x-goog-api-client` set to the ADK tracking tokens
/// followed by the tokens of the request's values (configured or credential-written), without
/// duplicates.
fn tracked(headers: &HeaderMap) -> HashMap<HeaderName, HeaderValue> {
    let mut out = HashMap::new();
    for name in headers.keys() {
        if *name == USER_AGENT || *name == X_GOOG_API_CLIENT {
            continue;
        }
        if let Some(value) = joined(headers.get_all(name).iter()) {
            out.insert(name.clone(), value);
        }
    }
    for name in [USER_AGENT, X_GOOG_API_CLIENT] {
        let existing: Vec<&str> = headers
            .get_all(&name)
            .iter()
            .filter_map(|v| std::str::from_utf8(v.as_bytes()).ok())
            .collect();
        if let Ok(value) = HeaderValue::from_str(&merge_tracking_tokens(&existing)) {
            out.insert(name, value);
        }
    }
    out
}

/// One header value from `values`: the value itself when there is one, else all of them joined
/// with `", "` (RFC 9110 field-line combination). `None` for no values.
fn joined<'a>(values: impl Iterator<Item = &'a HeaderValue>) -> Option<HeaderValue> {
    let values: Vec<&HeaderValue> = values.collect();
    match values.as_slice() {
        [] => None,
        [one] => Some((*one).clone()),
        many => {
            let bytes = many
                .iter()
                .map(|v| v.as_bytes())
                .collect::<Vec<_>>()
                .join(&b", "[..]);
            HeaderValue::from_bytes(&bytes)
                .ok()
                .or_else(|| many.first().map(|v| (*v).clone()))
        }
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
