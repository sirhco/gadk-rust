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
//! Go `agentregistry/registry.go`: [`Config`], [`Client::new`] (ADC, endpoint and quota-project
//! resolution).

use std::path::PathBuf;

use gadk_auth::AuthContext;
use gadk_platform::env::{EnvProvider, ProcessEnv};

use crate::error::Error;
use crate::requester::RestRequester;

/// Go `cloudPlatformScope`: the OAuth scope used for Application Default Credentials.
pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// Go `baseURLProd`.
pub(crate) const BASE_URL_PROD: &str = "https://agentregistry.googleapis.com/v1";
/// Go `baseURLMTLS`.
pub(crate) const BASE_URL_MTLS: &str = "https://agentregistry.mtls.googleapis.com/v1";

/// Go `Config`: configures a [`Client`].
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Go `ProjectID`: the Google Cloud project ID. Required.
    pub project_id: String,
    /// Go `Location`: the Google Cloud location (region), e.g. `us-central1`. Required.
    pub location: String,
    /// Go `HTTPClient`: used for registry API calls, unauthenticated by this crate (a supplied
    /// client manages its own credentials). `None` authenticates with Application Default
    /// Credentials (cloud-platform scope) over a default client and resolves the endpoint from
    /// `GOOGLE_API_USE_MTLS_ENDPOINT`.
    pub http_client: Option<reqwest::Client>,
}

/// Go `Client`: a client for the Google Cloud Agent Registry.
#[derive(Clone)]
pub struct Client {
    /// Go `requester` + `httpClient`: the REST transport; the factory helpers reuse its HTTP
    /// client and credential provider for egress to `*.googleapis.com` endpoints.
    pub(crate) requester: RestRequester,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.requester.base_url)
            .field("base_path", &self.requester.base_path)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Go `New`: validates the config and wires the REST requester. Without
    /// [`Config::http_client`], ADC is resolved here (one `credential` call), so a missing ADC
    /// fails now with `agentregistry: loading Application Default Credentials: …`.
    pub async fn new(cfg: Config) -> Result<Client, Error> {
        Client::new_with_env(cfg, &ProcessEnv).await
    }

    /// [`Client::new`] reading the environment from `env` (tests).
    pub(crate) async fn new_with_env(cfg: Config, env: &dyn EnvProvider) -> Result<Client, Error> {
        if cfg.project_id.is_empty() || cfg.location.is_empty() {
            return Err(Error::Config);
        }

        let (http, auth, base_url, creds_json) = match cfg.http_client {
            Some(http) => (http, None, endpoint_for(env, true), None),
            None => {
                // Go `google.FindDefaultCredentials`: discovery only (no token is minted).
                let auth = gadk_auth::adc(&[CLOUD_PLATFORM_SCOPE]);
                auth.credential(&AuthContext::anonymous())
                    .await
                    .map_err(|e| Error::Credentials(e.to_string()))?;
                let http = reqwest::Client::builder()
                    .build()
                    .map_err(|e| Error::HttpClient(e.to_string()))?;
                (http, Some(auth), endpoint_for(env, false), adc_json(env))
            }
        };

        // x-goog-user-project: GOOGLE_CLOUD_QUOTA_PROJECT, then the credentials' quota project,
        // then the project ID.
        let quota = quota_project_id(env, creds_json.as_deref());
        let user_project = if quota.is_empty() {
            cfg.project_id.clone()
        } else {
            quota
        };

        Ok(Client {
            requester: RestRequester {
                http,
                auth,
                base_url: base_url.to_owned(),
                base_path: format!("projects/{}/locations/{}", cfg.project_id, cfg.location),
                user_project,
            },
        })
    }
}

/// Go's endpoint choice: the ADC path's `htransport.NewHTTPClient` (no client certificate:
/// `GOOGLE_API_USE_MTLS_ENDPOINT`, else the deprecated `GOOGLE_API_USE_MTLS`, lowercased; `always`
/// → mTLS) when `custom_client` is false, Go `customClientBaseURL` (only
/// `GOOGLE_API_USE_MTLS_ENDPOINT`, `EqualFold("always")` → mTLS) when true.
pub(crate) fn endpoint_for(env: &dyn EnvProvider, custom_client: bool) -> &'static str {
    let var = |key: &str| env.var(key).unwrap_or_default();
    let always = if custom_client {
        var("GOOGLE_API_USE_MTLS_ENDPOINT").eq_ignore_ascii_case("always")
    } else {
        // Go `getMTLSMode` (google-api internal/cba.go); with no client certificate only
        // `always` selects the mTLS endpoint.
        let mut mode = var("GOOGLE_API_USE_MTLS_ENDPOINT");
        if mode.is_empty() {
            mode = var("GOOGLE_API_USE_MTLS");
        }
        mode.to_lowercase() == "always"
    };
    if always { BASE_URL_MTLS } else { BASE_URL_PROD }
}

/// Go `quotaProjectID`: `GOOGLE_CLOUD_QUOTA_PROJECT` when non-empty, else the credentials JSON's
/// `quota_project_id` (`None` / unparsable JSON → empty), else `""`.
pub(crate) fn quota_project_id(env: &dyn EnvProvider, creds_json: Option<&[u8]>) -> String {
    if let Some(q) = env
        .var("GOOGLE_CLOUD_QUOTA_PROJECT")
        .filter(|q| !q.is_empty())
    {
        return q;
    }
    let Some(json) = creds_json.filter(|j| !j.is_empty()) else {
        return String::new();
    };
    // Divergence (ledger 5c): the key is matched case-sensitively (Go's `json.Unmarshal`
    // also accepts e.g. `Quota_Project_ID`).
    #[derive(serde::Deserialize)]
    struct Parsed {
        #[serde(default)]
        quota_project_id: Option<String>,
    }
    serde_json::from_slice::<Parsed>(json)
        .ok()
        .and_then(|p| p.quota_project_id)
        .unwrap_or_default()
}

/// The ADC credentials JSON as Go's `creds.JSON`: the file named by
/// `GOOGLE_APPLICATION_CREDENTIALS`, else the gcloud well-known file
/// (`$HOME/.config/gcloud/application_default_credentials.json`, or
/// `%APPDATA%\gcloud\application_default_credentials.json` on Windows); `None` when unreadable
/// (metadata-server credentials carry no JSON).
pub(crate) fn adc_json(env: &dyn EnvProvider) -> Option<Vec<u8>> {
    let var = |key: &str| env.var(key).filter(|v| !v.is_empty());
    // Go `FindDefaultCredentialsWithParams`: the named file, else the well-known file.
    let path = match var("GOOGLE_APPLICATION_CREDENTIALS") {
        Some(file) => PathBuf::from(file),
        None if cfg!(windows) => PathBuf::from(var("APPDATA")?)
            .join("gcloud")
            .join("application_default_credentials.json"),
        None => PathBuf::from(var("HOME")?)
            .join(".config")
            .join("gcloud")
            .join("application_default_credentials.json"),
    };
    std::fs::read(path).ok()
}
