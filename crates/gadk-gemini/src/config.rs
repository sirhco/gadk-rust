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

//! Client configuration and its resolution: a port of genai's `NewInternalAPIClient`
//! (`google.golang.org/genai@v1.75.0` `client.go`) and `getBaseURL` (`base_url.go`).
//!
//! Deliberate gadk deviation: Vertex AI never takes an API key (genai's "express mode"),
//! and ephemeral `auth_tokens/` keys are rejected.

use std::fmt;
use std::sync::Arc;

use gadk_model::{Backend, HttpClient, HttpOptions};
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;

use crate::error::GeminiError;

/// OAuth scope requested for Application Default Credentials on Vertex AI.
pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/";
const VERTEX_GLOBAL_BASE_URL: &str = "https://aiplatform.googleapis.com/";

/// Port of genai's `multiRegionalLocations` (`client.go`).
const MULTI_REGIONAL_LOCATIONS: &[&str] = &["us", "eu"];

/// Client configuration (port of genai `ClientConfig`). Unset fields are resolved from the
/// environment by [`resolve`]. An empty string is treated as unset, as in genai.
#[derive(Clone)]
pub struct ClientConfig {
    /// Backend; `None` means resolve from `GOOGLE_GENAI_USE_ENTERPRISE` /
    /// `GOOGLE_GENAI_USE_VERTEXAI`, defaulting to the Gemini API.
    pub backend: Option<Backend>,
    /// API key (Gemini API only). Env: `GOOGLE_API_KEY`, then `GEMINI_API_KEY`.
    pub api_key: Option<String>,
    /// GCP project (Vertex AI). Env: `GOOGLE_CLOUD_PROJECT`.
    pub project: Option<String>,
    /// GCP location (Vertex AI). Env: `GOOGLE_CLOUD_LOCATION`, then `GOOGLE_CLOUD_REGION`.
    pub location: Option<String>,
    /// Explicit credentials (Vertex AI). Without them, ADC is used.
    pub credentials: Option<Credentials>,
    /// HTTP options (base URL, API version, headers, ...).
    pub http_options: HttpOptions,
    /// Custom HTTP client. When set, ADC is not looked up.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Environment variable source.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            backend: None,
            api_key: None,
            project: None,
            location: None,
            credentials: None,
            http_options: HttpOptions::default(),
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("backend", &self.backend)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("project", &self.project)
            .field("location", &self.location)
            .field("credentials", &self.credentials.is_some())
            .field("http_options", &self.http_options)
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// How requests authenticate.
#[derive(Clone)]
pub enum Auth {
    /// Gemini API key (`x-goog-api-key`).
    ApiKey(String),
    /// Google Cloud credentials (Vertex AI).
    Credentials(Credentials),
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey(<redacted>)"),
            Self::Credentials(c) => f.debug_tuple("Credentials").field(c).finish(),
        }
    }
}

/// The outcome of [`resolve`].
#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    /// The selected backend.
    pub backend: Backend,
    /// GCP project, if any.
    pub project: Option<String>,
    /// GCP location, if any.
    pub location: Option<String>,
    /// Base URL; always ends with `/`.
    pub base_url: String,
    /// API version (`v1beta` for the Gemini API, `v1beta1` for Vertex AI by default).
    pub api_version: String,
    /// Resolved authentication. `None` when ADC is still to be built (`needs_adc`) or when no
    /// auth applies (custom base URL without project, location or key; custom HTTP client).
    pub auth: Option<Auth>,
    /// Whether the caller must build Application Default Credentials with
    /// [`CLOUD_PLATFORM_SCOPE`] (genai `client.go` ADC condition).
    pub needs_adc: bool,
    /// The HTTP options with `base_url` and `api_version` filled in.
    pub http_options: HttpOptions,
}

/// Empty string means unset, as in genai.
fn non_empty(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn is_true(v: &str) -> bool {
    let v = v.to_lowercase();
    v == "1" || v == "true"
}

/// Port of genai `getAPIKeyFromEnv`.
fn api_key_from_env(env: &dyn EnvProvider) -> String {
    let google = env.var("GOOGLE_API_KEY").unwrap_or_default();
    let gemini = env.var("GEMINI_API_KEY").unwrap_or_default();
    if !google.is_empty() && !gemini.is_empty() {
        tracing::warn!("Both GOOGLE_API_KEY and GEMINI_API_KEY are set. Using GOOGLE_API_KEY.");
    }
    if !google.is_empty() { google } else { gemini }
}

/// Port of genai `getBaseURL` (`base_url.go`). genai's process-global `SetDefaultBaseURLs`
/// overrides are not ported.
fn base_url_for(backend: Backend, http_options: &HttpOptions, env: &dyn EnvProvider) -> String {
    if let Some(u) = http_options.base_url.as_ref().filter(|u| !u.is_empty()) {
        return u.clone();
    }
    let key = match backend {
        Backend::VertexAi => "GOOGLE_VERTEX_BASE_URL",
        Backend::GeminiApi => "GOOGLE_GEMINI_BASE_URL",
    };
    env.var(key).unwrap_or_default()
}

/// Resolves `cfg` into a concrete backend, endpoint and auth, following genai's
/// `NewInternalAPIClient` step by step. Application Default Credentials are not built here:
/// when [`ResolvedConfig::needs_adc`] is set the caller builds them.
pub fn resolve(cfg: &ClientConfig) -> Result<ResolvedConfig, GeminiError> {
    let env = cfg.env.as_ref();
    let credentials = cfg.credentials.clone();
    let mut http_options = cfg.http_options.clone();

    let config_api_key = non_empty(&cfg.api_key);
    let config_project = non_empty(&cfg.project);
    let config_location = non_empty(&cfg.location);

    if credentials.is_some() && !config_api_key.is_empty() {
        return Err(GeminiError::Config(
            "credentials and API key are mutually exclusive in the client initializer".into(),
        ));
    }

    // genai maps BackendEnterprise to BackendVertexAI here; gadk has no Enterprise variant.
    let backend = match cfg.backend {
        Some(b) => b,
        None => {
            let v_enterprise = env.var("GOOGLE_GENAI_USE_ENTERPRISE");
            let v_vertex = env.var("GOOGLE_GENAI_USE_VERTEXAI");
            let is_enterprise = v_enterprise.as_deref().is_some_and(is_true);
            let is_vertex = v_vertex.as_deref().is_some_and(is_true);

            if v_enterprise.is_some() && v_vertex.is_some() && is_enterprise != is_vertex {
                tracing::warn!(
                    "Both GOOGLE_GENAI_USE_ENTERPRISE and GOOGLE_GENAI_USE_VERTEXAI are set with conflicting values. The value of GOOGLE_GENAI_USE_ENTERPRISE will be used."
                );
            }

            let use_vertex = if v_enterprise.is_some() {
                is_enterprise
            } else if v_vertex.is_some() {
                is_vertex
            } else {
                false
            };
            if use_vertex {
                Backend::VertexAi
            } else {
                Backend::GeminiApi
            }
        }
    };

    // Retrieve implicitly set values from the environment.
    let env_api_key = api_key_from_env(env);
    let env_project = env.var("GOOGLE_CLOUD_PROJECT").unwrap_or_default();
    let env_location = match env.var("GOOGLE_CLOUD_LOCATION") {
        Some(l) => l,
        None => env.var("GOOGLE_CLOUD_REGION").unwrap_or_default(),
    };

    let mut api_key = if config_api_key.is_empty() {
        env_api_key.clone()
    } else {
        config_api_key.clone()
    };
    let mut project = if config_project.is_empty() {
        env_project.clone()
    } else {
        config_project.clone()
    };
    let mut location = if config_location.is_empty() {
        env_location.clone()
    } else {
        config_location.clone()
    };

    let mut base_url = match backend {
        Backend::VertexAi => {
            // Handle when to use Vertex AI in express mode (api key).
            if credentials.is_some() && !env_api_key.is_empty() {
                tracing::warn!(
                    "The user provided Google Cloud credentials will take precedence over the API key from the environment variable."
                );
                api_key.clear();
            } else if !config_api_key.is_empty()
                && config_project.is_empty()
                && config_location.is_empty()
                && (!env_project.is_empty() || !env_location.is_empty())
            {
                // Explicit API key takes precedence over implicit project/location.
                tracing::warn!(
                    "The user provided Vertex AI API key will take precedence over the project/location from the environment variables."
                );
                project.clear();
                location.clear();
            } else if (!config_project.is_empty() || !config_location.is_empty())
                && config_api_key.is_empty()
                && !env_api_key.is_empty()
            {
                // Explicit project/location takes precedence over implicit API key.
                tracing::warn!(
                    "The user provided project/location will take precedence over the API key from the environment variable."
                );
                api_key.clear();
            } else if config_project.is_empty()
                && config_location.is_empty()
                && config_api_key.is_empty()
                && (!env_project.is_empty() || !env_location.is_empty())
                && !env_api_key.is_empty()
            {
                // Implicit project/location takes precedence over implicit API key.
                tracing::warn!(
                    "The project/location from the environment variables will take precedence over the API key from the environment variable."
                );
                api_key.clear();
            }

            // Resolve the base URL early to determine whether it is custom.
            let mut url = base_url_for(backend, &http_options, env);

            // If no location, no API key, and no custom base URL, default to global.
            if location.is_empty() && api_key.is_empty() && url.is_empty() {
                location = "global".into();
            }

            let config_has_sufficient_auth = (!config_project.is_empty()
                && !config_location.is_empty())
                || !config_api_key.is_empty();
            let has_sufficient_auth =
                (!project.is_empty() && !location.is_empty()) || !api_key.is_empty();
            let has_custom_base_url = !url.is_empty();

            if !has_sufficient_auth && !has_custom_base_url {
                return Err(GeminiError::Config(
                    "project/location or API key must be set when using Vertex AI backend".into(),
                ));
            }

            // With a custom base URL and no explicit auth, clear project/location to avoid
            // `projects//locations//` path construction (the base URL handles it, e.g. a proxy).
            if has_custom_base_url && !config_has_sufficient_auth {
                project.clear();
                location.clear();
            }

            // Set the default base URL if still empty.
            if url.is_empty() {
                url = if location == "global" || !api_key.is_empty() {
                    VERTEX_GLOBAL_BASE_URL.to_string()
                } else if MULTI_REGIONAL_LOCATIONS.contains(&location.as_str()) {
                    format!("https://aiplatform.{location}.rep.googleapis.com/")
                } else {
                    format!("https://{location}-aiplatform.googleapis.com/")
                };
            }
            url
        }
        Backend::GeminiApi => {
            let mut url = base_url_for(backend, &http_options, env);
            if url.is_empty() {
                url = GEMINI_DEFAULT_BASE_URL.to_string();
            }
            if api_key.is_empty() {
                return Err(GeminiError::Config(
                    "api key is required for Google AI backend.\nYou can get the API key from https://ai.google.dev/gemini-api/docs/api-key".into(),
                ));
            }
            if !config_project.is_empty() || !config_location.is_empty() {
                return Err(GeminiError::Config(
                    "project and location are not supported for Gemini API backend".into(),
                ));
            }
            url
        }
    };

    // gadk deviation: no Vertex AI express mode.
    if backend == Backend::VertexAi && !api_key.is_empty() {
        return Err(GeminiError::Config(
            "API keys are only supported for the Gemini API backend; use ADC or credentials for Vertex AI".into(),
        ));
    }
    if api_key.starts_with("auth_tokens/") {
        return Err(GeminiError::Config(
            "ephemeral auth_tokens keys are not supported".into(),
        ));
    }

    let skip_adc =
        !base_url.is_empty() && project.is_empty() && location.is_empty() && api_key.is_empty();
    let needs_adc = backend == Backend::VertexAi
        && credentials.is_none()
        && api_key.is_empty()
        && cfg.http_client.is_none()
        && !skip_adc;

    let api_version = match http_options.api_version.as_ref().filter(|v| !v.is_empty()) {
        Some(v) => v.clone(),
        None => match backend {
            Backend::VertexAi => "v1beta1".to_string(),
            Backend::GeminiApi => "v1beta".to_string(),
        },
    };

    if !base_url.ends_with('/') {
        base_url.push('/');
    }
    http_options.base_url = Some(base_url.clone());
    http_options.api_version = Some(api_version.clone());

    let auth = if !api_key.is_empty() {
        Some(Auth::ApiKey(api_key))
    } else {
        credentials.map(Auth::Credentials)
    };

    Ok(ResolvedConfig {
        backend,
        project: opt(project),
        location: opt(location),
        base_url,
        api_version,
        auth,
        needs_adc,
        http_options,
    })
}
