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

//! gadk-auth: credentials, credential providers, the credential store and the refresh-and-retry
//! transport (port of ADK-Go `auth/`), plus per-user Google Cloud credentials (`gcp`, port of
//! `auth/gcp`).
//!
//! A small authentication layer for gadk. It models the two things a consumer needs: a resolved
//! [`Credential`] that writes itself onto an outbound HTTP request, and a [`CredentialProvider`]
//! seam that resolves a credential for the current invocation. Built-in providers cover the
//! common cases: a static bearer token ([`static_token`]), a header API key ([`api_key`]), any
//! [`TokenSource`] ([`token_source`]), Application Default Credentials ([`adc`]) and service
//! accounts ([`service_account`]). A [`Transport`] middleware applies a provider to every
//! outgoing request ([`client`]).
//!
//! A provider that has to reach the network to resolve a credential can cache what it resolved
//! in a [`CredentialStore`], keyed per app, per end user and per whatever else decides which
//! credential comes back; [`InMemoryCredentialStore`] is the process-local implementation.
//! Providers backed by a token source refresh themselves and need none.
//!
//! Token exchange and refresh are delegated to `google-cloud-auth` rather than reimplemented
//! here. The GCP agent-identity integration lives in the `gcp` module (feature `gcp`, on by
//! default).

pub mod credential;
pub mod error;
#[cfg(feature = "gcp")]
pub mod gcp;
pub mod provider;
pub mod providers;
pub mod store;
pub mod transport;

pub(crate) mod go_fmt;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
#[cfg(test)]
pub(crate) mod testserver;

pub use credential::{
    ApiKey, Basic, Bearer, Credential, OAuth2, StaticTokenSource, Token, TokenSource, with_headers,
};
pub use error::{ConsentRequiredError, Error, SharedError, find_consent};
pub use gadk_agent::Identity;
pub use provider::{
    AuthContext, CredentialFuture, CredentialProvider, ProviderFn, RefreshingProvider,
};
pub use providers::{
    ServiceAccountConfig, adc, api_key, service_account, static_token, token_source,
};
pub use store::{CredentialKey, CredentialStore, EXPIRY_SKEW, InMemoryCredentialStore};
pub use transport::{Transport, client, find_consent_in};
