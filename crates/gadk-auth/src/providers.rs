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

//! The built-in providers and the lazy token source (port of ADK-Go `auth/providers.go:124-291`).
//! Task 1 placeholder: Task 2 replaces every `todo!` body.

use std::sync::Arc;

use crate::credential::TokenSource;
use crate::error::Error;
use crate::provider::CredentialProvider;

/// Go `StaticToken` (providers.go:125): a fresh `Bearer { token }` per call.
#[allow(unused_variables, clippy::todo)]
pub fn static_token(token: impl Into<String>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `APIKey` (providers.go:133).
#[allow(unused_variables, clippy::todo)]
pub fn api_key(name: impl Into<String>, value: impl Into<String>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `TokenSourceProvider` (providers.go:142): `OAuth2 { token_source }` per call.
#[allow(unused_variables, clippy::todo)]
pub fn token_source(ts: Arc<dyn TokenSource>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `ADC(scopes...)` (providers.go:168): lazy, scopes default to cloud-platform when empty.
#[allow(unused_variables, clippy::todo)]
pub fn adc(scopes: &[&str]) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `ServiceAccount` (providers.go:193): lazy; audience → ID token; key → access token
/// (scopes required); neither → ADC with `scopes`.
#[allow(unused_variables, clippy::todo)]
pub fn service_account(cfg: ServiceAccountConfig) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `ServiceAccountConfig` (providers.go:177). An empty `json_key` is Go's `len(JSONKey) == 0`.
#[derive(Clone, Default)]
pub struct ServiceAccountConfig {
    /// A service-account key in JSON form; empty means Application Default Credentials.
    pub json_key: Vec<u8>,
    /// Scopes for an OAuth2 access token; ignored when `audience` is set.
    pub scopes: Vec<String>,
    /// When non-empty, an ID token for this audience instead of an access token.
    pub audience: String,
}

impl std::fmt::Debug for ServiceAccountConfig {
    #[allow(unused_variables, clippy::todo)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("Task 2")
    }
}

/// providers.go:152 `scopeCloudPlatform`.
#[allow(dead_code)]
pub(crate) const SCOPE_CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";
/// providers.go:227 `initTimeout` (a parameter in Rust, so tests pass a short bound).
#[allow(dead_code)]
pub(crate) const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Where ADC is read from. `Discover` is google-cloud-auth's real lookup. `Json` stands in for a
/// `GOOGLE_APPLICATION_CREDENTIALS` file, because tests cannot set the variable
/// (`env::set_var` is `unsafe`). Supported `"type"`s: `authorized_user`, `service_account`.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum AdcSource {
    Discover,
    Json(serde_json::Value),
}

/// Go `adcTokenSource` (providers.go:154): an access-token source for `scopes` (cloud-platform
/// when empty). Builder errors are exactly `Error::FindDefaultCredentials(_)`. Synchronous.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn adc_token_source(
    source: &AdcSource,
    scopes: &[String],
) -> Result<Arc<dyn TokenSource>, Error> {
    todo!("Task 2")
}

/// `adc` with an injected source.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn adc_with(source: AdcSource, scopes: Vec<String>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// `service_account` with an injected ADC source for its fallback branch.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn service_account_with(
    cfg: ServiceAccountConfig,
    source: AdcSource,
) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

#[allow(dead_code)]
pub(crate) type InitFuture =
    futures::future::BoxFuture<'static, Result<Arc<dyn TokenSource>, Error>>;

/// Go `lazyTokenSource` (providers.go:232). One shared attempt, `tokio::spawn`ed. The bound is a
/// timer inside the attempt: when it fires the token is cancelled, and the attempt's result
/// becomes `Error::InitTimeout { bound, init_error }` (the init's text when it failed); a
/// source landing past the bound is discarded. A failure resets the slot. Each success is an
/// `OAuth2 { token_source }` over the memoized source.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn lazy_token_source<F>(
    init: F,
    bound: std::time::Duration,
) -> Arc<dyn CredentialProvider>
where
    F: Fn(tokio_util::sync::CancellationToken) -> InitFuture + Send + Sync + 'static,
{
    todo!("Task 2")
}
