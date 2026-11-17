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

use gadk_artifact::ArtifactError;
use gadk_model::HttpClient;
use gadk_model::gcp_auth::adc_credentials;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::go_quote;
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;

use crate::error::backend;

/// The OAuth scopes requested for Application Default Credentials.
pub(crate) const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/devstorage.full_control",
    "https://www.googleapis.com/auth/cloud-platform",
];

/// The base URL when neither an endpoint nor `STORAGE_EMULATOR_HOST` is set.
pub(crate) const DEFAULT_BASE_URL: &str = "https://storage.googleapis.com";

/// How a [`GcsArtifactService`](crate::GcsArtifactService) reaches Google Cloud Storage.
#[derive(Clone)]
pub struct GcsConfig {
    /// The credentials sent with every request. `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the scopes
    /// `devstorage.full_control` and `cloud-platform`.
    pub credentials: Option<Credentials>,
    /// The HTTP client; `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// The base URL: `scheme://host[:port]`, optionally ending in `/` or `/storage/v1`. `None` uses
    /// `STORAGE_EMULATOR_HOST` when it is set, else `https://storage.googleapis.com`.
    pub endpoint: Option<String>,
    /// Headers added to every request.
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

/// What a [`GcsConfig`] resolves to.
pub(crate) struct Resolved {
    /// `scheme://host[:port]`, without a trailing `/`.
    pub(crate) base_url: String,
    /// The credentials sent with every request; `None` turns authentication off.
    pub(crate) credentials: Option<Credentials>,
}

impl GcsConfig {
    /// The base URL and the credentials: the
    /// endpoint, else `STORAGE_EMULATOR_HOST`, else [`DEFAULT_BASE_URL`]; no authentication
    /// when `STORAGE_EMULATOR_HOST` is non-empty, else [`credentials`](Self::credentials) or
    /// Application Default Credentials with [`SCOPES`].
    pub(crate) fn resolve(&self) -> Result<Resolved, ArtifactError> {
        let emulator = self
            .env
            .var("STORAGE_EMULATOR_HOST")
            .filter(|host| !host.is_empty());
        let endpoint = self.endpoint.as_deref().filter(|e| !e.is_empty());
        let base_url = match (endpoint, &emulator) {
            (Some(endpoint), _) => endpoint_base_url(endpoint)?,
            (None, Some(host)) => emulator_base_url(host),
            (None, None) => DEFAULT_BASE_URL.to_owned(),
        };
        let credentials = match (&emulator, &self.credentials) {
            (Some(_), _) => None,
            (None, Some(credentials)) => Some(credentials.clone()),
            (None, None) => {
                let file = self.env.var("GOOGLE_APPLICATION_CREDENTIALS");
                Some(
                    adc_credentials(file.as_deref(), &SCOPES)
                        .map_err(|e| backend("failed to create gcs service", e))?,
                )
            }
        };
        Ok(Resolved {
            base_url,
            credentials,
        })
    }
}

/// The base URL of an endpoint value: `http`/`https`, a host, and no path other than `/`,
/// `/storage/v1` or `/storage/v1/`.
fn endpoint_base_url(endpoint: &str) -> Result<String, ArtifactError> {
    let invalid = || {
        ArtifactError::InvalidRequest(format!(
            "invalid GCS endpoint {}: want http(s)://host[:port], optionally ending in /storage/v1",
            go_quote(endpoint)
        ))
    };
    let uri: http::Uri = endpoint.parse().map_err(|_| invalid())?;
    let (Some(scheme @ ("http" | "https")), Some(authority)) = (uri.scheme_str(), uri.authority())
    else {
        return Err(invalid());
    };
    let path = uri.path().trim_end_matches('/');
    if uri.query().is_some() || !(path.is_empty() || path == "/storage/v1") {
        return Err(invalid());
    }
    Ok(format!("{scheme}://{authority}"))
}

/// The base URL of a `STORAGE_EMULATOR_HOST` value: `http://{host}`, or the value's own scheme
/// and host when it has a scheme (any path, query or fragment is dropped).
fn emulator_base_url(host: &str) -> String {
    match host.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            format!("{scheme}://{authority}")
        }
        None => format!("http://{host}"),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn endpoint_base_urls() {
        for (endpoint, want) in [
            ("http://127.0.0.1:9000", "http://127.0.0.1:9000"),
            ("http://127.0.0.1:9000/", "http://127.0.0.1:9000"),
            (
                "https://storage.example/storage/v1",
                "https://storage.example",
            ),
            (
                "https://storage.example/storage/v1/",
                "https://storage.example",
            ),
        ] {
            assert_eq!(endpoint_base_url(endpoint).unwrap(), want, "{endpoint}");
        }
        for endpoint in [
            "http://h/other",
            "http://h/storage/v1/b",
            "http://h?x=1",
            "ftp://h",
            "h:9000",
            "not a url",
        ] {
            let err = endpoint_base_url(endpoint).unwrap_err();
            assert!(
                matches!(err, ArtifactError::InvalidRequest(_)),
                "{endpoint}"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "invalid GCS endpoint {}: want http(s)://host[:port], optionally ending in \
                     /storage/v1",
                    go_quote(endpoint)
                )
            );
        }
    }

    #[test]
    fn emulator_base_urls() {
        assert_eq!(emulator_base_url("localhost:9023"), "http://localhost:9023");
        assert_eq!(
            emulator_base_url("https://emu.example:443/ignored/path"),
            "https://emu.example:443"
        );
    }
}
