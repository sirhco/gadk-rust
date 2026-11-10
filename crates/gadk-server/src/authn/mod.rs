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

//! Inbound HTTP authentication for the ADK REST API (port of ADK-Go `server/authn`).
//!
//! An [`Authenticator`] resolves a request's credentials to a [`Caller`]; [`middleware`] gates a
//! route with one, answering 401 when a request carries no valid credentials, 403 when the
//! authenticated principal is refused and 500 on a provider failure, and otherwise inserting the
//! [`Caller`] as a request extension (Go `WithCaller` / `CallerFromContext`). Built-in providers:
//! [`noop`], [`header`], [`identity_aware_proxy`], [`custom`] and [`google_oidc`].

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use futures::future::BoxFuture;
use http::StatusCode;

use crate::rest::http_error::http_error;

mod custom;
mod google_oidc;
mod header;
mod iap;
mod jwks;
mod noop;

pub use custom::custom;
pub use google_oidc::{GoogleOidcConfig, google_oidc};
pub use header::header;
pub use iap::{USER_EMAIL_HEADER, USER_ID_HEADER, identity_aware_proxy};
pub use noop::noop;

/// Port of Go `authn.Caller`: the authenticated principal resolved from a request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caller {
    /// The stable identifier of the caller.
    pub user_id: String,
    /// Optional provider-specific attributes (email, roles, token scopes, ...).
    pub claims: Option<serde_json::Map<String, serde_json::Value>>,
}

/// An authentication failure. The cause is for logs only; it is never sent to the client.
#[derive(Debug, thiserror::Error)]
pub enum AuthnError {
    /// Go `ErrUnauthenticated` (or an error wrapping it): no valid credentials; answered 401.
    #[error("authn: unauthenticated{}", .0.as_ref().map(|c| format!(": {c}")).unwrap_or_default())]
    Unauthenticated(Option<String>),
    /// Go `ErrForbidden` (or an error wrapping it): valid credentials, refused principal;
    /// answered 403.
    #[error("authn: forbidden{}", .0.as_ref().map(|c| format!(": {c}")).unwrap_or_default())]
    Forbidden(Option<String>),
    /// Any other provider failure (Go text); answered 500. Also a construction error of
    /// [`google_oidc`].
    #[error("{0}")]
    Other(String),
}

/// Port of Go `authn.Authenticator`: authenticates an inbound HTTP request.
#[async_trait::async_trait]
pub trait Authenticator: Send + Sync {
    /// Verifies the request's credentials and returns the authenticated identity. `Ok(None)` is a
    /// decline and is answered like [`AuthnError::Unauthenticated`].
    async fn authenticate(&self, req: &http::request::Parts) -> Result<Option<Caller>, AuthnError>;
}

/// Port of Go `authn.Middleware`: a [`tower::Layer`] that authenticates every request before the
/// inner service sees it.
pub fn middleware(authenticator: Arc<dyn Authenticator>) -> AuthnLayer {
    AuthnLayer { authenticator }
}

/// The layer [`middleware`] returns.
#[derive(Clone)]
pub struct AuthnLayer {
    authenticator: Arc<dyn Authenticator>,
}

impl<S> tower::Layer<S> for AuthnLayer {
    type Service = AuthnService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthnService {
            authenticator: self.authenticator.clone(),
            inner,
        }
    }
}

/// The service [`AuthnLayer`] wraps around an inner service.
#[derive(Clone)]
pub struct AuthnService<S> {
    authenticator: Arc<dyn Authenticator>,
    inner: S,
}

impl<S> tower::Service<http::Request<Body>> for AuthnService<S>
where
    S: tower::Service<http::Request<Body>, Response = axum::response::Response, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = axum::response::Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let authenticator = self.authenticator.clone();
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let (mut parts, body) = req.into_parts();
            match authenticator.authenticate(&parts).await {
                Ok(Some(caller)) => {
                    parts.extensions.insert(caller);
                    inner.call(http::Request::from_parts(parts, body)).await
                }
                // A provider that reports neither an identity nor an error declines.
                Ok(None) => Ok(write_auth_error(&AuthnError::Unauthenticated(None))),
                Err(e) => Ok(write_auth_error(&e)),
            }
        })
    }
}

/// Go `writeAuthError`: 403 `forbidden`, 401 `unauthorized`, any other error 500
/// `authentication failed`, each in Go's `http.Error` framing; the cause is logged only.
fn write_auth_error(err: &AuthnError) -> axum::response::Response {
    tracing::debug!(error = %err, "authentication failed");
    match err {
        AuthnError::Forbidden(_) => http_error(StatusCode::FORBIDDEN, "forbidden"),
        AuthnError::Unauthenticated(_) => http_error(StatusCode::UNAUTHORIZED, "unauthorized"),
        AuthnError::Other(_) => {
            http_error(StatusCode::INTERNAL_SERVER_ERROR, "authentication failed")
        }
    }
}
