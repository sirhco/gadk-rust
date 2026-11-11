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

//! The identity header bridge: the authenticated user reaches the executor through the
//! reserved `x-gadk-a2a-user` header (spec R5, R6). Bodies: plan 5b Task 5.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::task::{Context, Poll};

use super::executor::UserResolver;

/// The reserved request header that carries the authenticated user to the executor.
pub const IDENTITY_HEADER: &str = "x-gadk-a2a-user";

/// The default user resolver: `ctx.user.name` when it is non-empty; it never reads headers.
pub fn default_user_resolver() -> UserResolver {
    todo!("plan 5b Task 5")
}

/// The first non-empty [`IDENTITY_HEADER`] service param, else [`default_user_resolver`].
/// Install it only together with [`a2a_identity_layer`] inside the authentication layer.
pub fn identity_header_resolver() -> UserResolver {
    todo!("plan 5b Task 5")
}

/// A tower layer that removes any inbound [`IDENTITY_HEADER`] and, when `extract` finds a user
/// in the request extensions, sets the header to it.
pub fn a2a_identity_layer<F>(extract: F) -> IdentityLayer<F>
where
    F: Fn(&http::Extensions) -> Option<String> + Clone + Send + Sync + 'static,
{
    IdentityLayer { extract }
}

/// The layer [`a2a_identity_layer`] returns.
#[derive(Clone)]
pub struct IdentityLayer<F> {
    extract: F,
}

impl<S, F: Clone> tower::Layer<S> for IdentityLayer<F> {
    type Service = IdentityService<S, F>;

    fn layer(&self, inner: S) -> Self::Service {
        IdentityService {
            inner,
            extract: self.extract.clone(),
        }
    }
}

/// The service [`IdentityLayer`] wraps around the A2A router.
#[derive(Clone)]
pub struct IdentityService<S, F> {
    inner: S,
    extract: F,
}

impl<S, F, B> tower::Service<http::Request<B>> for IdentityService<S, F>
where
    S: tower::Service<http::Request<B>>,
    F: Fn(&http::Extensions) -> Option<String> + Clone + Send + Sync + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!("plan 5b Task 5")
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        todo!("plan 5b Task 5")
    }
}

/// The user ID gadk-server's authentication layer stored (`gadk_server::authn::Caller`).
#[cfg(feature = "gadk-server")]
pub fn caller_user(ext: &http::Extensions) -> Option<String> {
    todo!("plan 5b Task 5")
}
