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

//! The per-user credential provider (port of ADK-Go `auth/gcp/provider.go`). Task 1 writes the
//! public types; Task 9 replaces the `todo!` bodies and adds its own seams.

use std::sync::Arc;

use super::Error;
use super::client::Client;

/// Go `ProviderScheme` (provider.go:33).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderScheme {
    /// The connector or auth-provider resource name.
    pub name: String,
    /// The OAuth scopes requested.
    pub scopes: Vec<String>,
    /// Where the consent flow returns to.
    pub continue_uri: String,
}

/// Go `ProviderConfig` (provider.go:51). `client: None` builds the ADC client lazily on first
/// use; `store: None` gives the provider a private `InMemoryCredentialStore`.
#[derive(Clone, Default)]
pub struct ProviderConfig {
    /// What to retrieve.
    pub scheme: ProviderScheme,
    /// The client to retrieve with; `None` builds the ADC client lazily.
    pub client: Option<Arc<Client>>,
    /// Where retrieved credentials are cached; `None` is a private in-memory store.
    pub store: Option<Arc<dyn crate::CredentialStore>>,
}

impl std::fmt::Debug for ProviderConfig {
    #[allow(unused_variables, clippy::todo)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("Task 9")
    }
}

/// Go `NewProvider` (provider.go:181) without the wiring context. Errors: "gcp: NewProvider
/// requires a scheme Name"; "gcp: NewProvider: <validate_resource error>"; "gcp: NewProvider:
/// scheme Name %q is neither projects/*/locations/*/connectors/* nor
/// projects/*/locations/*/authProviders/*". The provider's `as_refreshing()` is `Some`.
#[allow(unused_variables, clippy::todo)]
pub fn new_provider(cfg: ProviderConfig) -> Result<Arc<dyn crate::CredentialProvider>, Error> {
    todo!("Task 9")
}
