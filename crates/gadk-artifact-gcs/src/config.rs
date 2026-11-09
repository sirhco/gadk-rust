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

//! [`GcsConfig`]: how a [`GcsArtifactService`](crate::GcsArtifactService) reaches GCS.

use std::sync::Arc;

use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;

/// How a [`GcsArtifactService`](crate::GcsArtifactService) reaches Google Cloud Storage (the
/// counterpart of the `option.ClientOption`s Go's `NewService` passes to `storage.NewClient`).
#[derive(Clone)]
pub struct GcsConfig {
    /// The credentials sent with every request (Go `option.WithAuthCredentials`). `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the scopes
    /// `devstorage.full_control` and `cloud-platform` (Go's defaults).
    pub credentials: Option<Credentials>,
    /// The HTTP client (Go `option.WithHTTPClient`); `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// The base URL (Go `option.WithEndpoint`): `scheme://host[:port]`, optionally ending in `/`
    /// or `/storage/v1` (the form Go's endpoint values take). `None` uses
    /// `STORAGE_EMULATOR_HOST` when it is set, else `https://storage.googleapis.com`.
    pub endpoint: Option<String>,
    /// Headers added to every request (Go `option.WithHeaders`).
    pub headers: HeaderMap,
    /// Where `STORAGE_EMULATOR_HOST` and `GOOGLE_APPLICATION_CREDENTIALS` are read.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for GcsConfig {
    fn default() -> Self {
        Self {
            credentials: None,
            http_client: None,
            endpoint: None,
            headers: HeaderMap::new(),
            env: Arc::new(ProcessEnv),
        }
    }
}

impl std::fmt::Debug for GcsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsConfig")
            .field("credentials", &self.credentials.is_some())
            .field("http_client", &self.http_client.is_some())
            .field("endpoint", &self.endpoint)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}
