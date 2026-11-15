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

//! Gemini through an Apigee proxy (port of ADK-Go `model/apigee/apigee.go`).
//!
//! An Apigee model name is `apigee/[vertex_ai/|gemini/][vN/]<model>`. The model is served by a
//! [`GeminiModel`] whose base URL is the proxy URL; the name selects the backend (Vertex AI or
//! the Gemini API) and the API version.

use std::fmt;
use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_model::{Backend, HttpClient, HttpOptions, Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::go_to_lower;

use crate::config::ClientConfig;
use crate::error::GeminiError;
use crate::model::GeminiModel;

const APIGEE_PROXY_URL_ENV_VAR: &str = "APIGEE_PROXY_URL";
const GOOGLE_GENAI_USE_VERTEX_AI_ENV_VAR: &str = "GOOGLE_GENAI_USE_VERTEXAI";
const PROJECT_ENV_VAR: &str = "GOOGLE_CLOUD_PROJECT";
const LOCATION_ENV_VAR: &str = "GOOGLE_CLOUD_LOCATION";

/// Configuration for [`ApigeeModel::new`] (port of Go `apigee.Config` and its options).
#[derive(Clone)]
pub struct ApigeeConfig {
    /// Proxy URL (Go `WithProxyURL`). When `None` or empty, `APIGEE_PROXY_URL` is read from
    /// [`env`](Self::env).
    pub proxy_url: Option<String>,
    /// Headers sent with every request (Go `WithCustomHeaders`).
    pub custom_headers: http::HeaderMap,
    /// Custom HTTP client (Go `WithHTTPClient`).
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Environment variable source (`APIGEE_PROXY_URL`, `GOOGLE_GENAI_USE_VERTEXAI`,
    /// `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and the Gemini client's variables).
    pub env: Arc<dyn EnvProvider>,
}

impl Default for ApigeeConfig {
    fn default() -> Self {
        Self {
            proxy_url: None,
            custom_headers: http::HeaderMap::new(),
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

/// Header values and credentials in `proxy_url` are redacted.
impl fmt::Debug for ApigeeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApigeeConfig")
            .field(
                "proxy_url",
                &self.proxy_url.as_deref().map(gadk_model::redact_url),
            )
            .field(
                "custom_headers",
                &self.custom_headers.keys().collect::<Vec<_>>(),
            )
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// The parts of an Apigee model name (Go `modelInfo`).
#[doc(hidden)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelInfo {
    /// The Gemini model ID.
    pub model_id: String,
    /// The API version (`vN…`), or `""`.
    pub api_version: String,
    /// Whether the model is served by Vertex AI.
    pub is_vertex_ai: bool,
}

/// Go's `invalid model string: <name>`.
fn invalid_model_string(model_name: &str) -> GeminiError {
    GeminiError::Config(format!("invalid model string: {model_name}"))
}

/// Go `parseModelName`: `apigee/[vertex_ai/|gemini/][vN/]<model>`, with
/// `GOOGLE_GENAI_USE_VERTEXAI` read through `env`; Go's `invalid model string: <name>` as
/// [`GeminiError::Config`]. Hidden: public only for the parity test.
#[doc(hidden)]
pub fn parse_model_name(model_name: &str, env: &dyn EnvProvider) -> Result<ModelInfo, GeminiError> {
    let Some(model_part) = model_name.strip_prefix("apigee/") else {
        return Err(invalid_model_string(model_name));
    };
    if model_part.is_empty() {
        return Err(invalid_model_string(model_name));
    }
    let components: Vec<&str> = model_part.split('/').collect();

    let mut info = ModelInfo {
        is_vertex_ai: !model_name.starts_with("apigee/gemini/")
            && (model_name.starts_with("apigee/vertex_ai/")
                || is_enabled(env, GOOGLE_GENAI_USE_VERTEX_AI_ENV_VAR)),
        ..ModelInfo::default()
    };

    let validated = match components.as_slice() {
        [model_id] => {
            info.model_id = (*model_id).to_string();
            true
        }
        [first, model_id] if *first == "vertex_ai" || *first == "gemini" => {
            info.model_id = (*model_id).to_string();
            true
        }
        [version, model_id] if version.starts_with('v') => {
            info.api_version = (*version).to_string();
            info.model_id = (*model_id).to_string();
            true
        }
        [first, version, model_id]
            if (*first == "vertex_ai" || *first == "gemini") && version.starts_with('v') =>
        {
            info.api_version = (*version).to_string();
            info.model_id = (*model_id).to_string();
            true
        }
        _ => false,
    };

    if !validated {
        return Err(invalid_model_string(model_name));
    }
    Ok(info)
}

/// Go `resolveProxyURL`: the configured URL, else `APIGEE_PROXY_URL` (empty when unset).
fn resolve_proxy_url(proxy_url: Option<&str>, env: &dyn EnvProvider) -> String {
    match proxy_url {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => env.var(APIGEE_PROXY_URL_ENV_VAR).unwrap_or_default(),
    }
}

/// Go `generateHTTPOptions`: the proxy URL as base URL, the custom headers, and the API version
/// when the name carries one.
fn generate_http_options(
    proxy_url: String,
    api_version: &str,
    custom_headers: &http::HeaderMap,
) -> HttpOptions {
    HttpOptions {
        base_url: Some(proxy_url),
        api_version: (!api_version.is_empty()).then(|| api_version.to_string()),
        headers: custom_headers.clone(),
        ..HttpOptions::default()
    }
}

/// Go `backendType`.
fn backend_type(is_vertex_ai: bool) -> Backend {
    if is_vertex_ai {
        Backend::VertexAi
    } else {
        Backend::GeminiApi
    }
}

/// Go `isEnabled`: the variable is `true` (any case) or `1`.
fn is_enabled(env: &dyn EnvProvider, env_var_name: &str) -> bool {
    let val = env.var(env_var_name).unwrap_or_default();
    go_to_lower(&val) == "true" || val == "1"
}

/// Go `generateClientConfig`: Vertex AI requires `GOOGLE_CLOUD_PROJECT` and
/// `GOOGLE_CLOUD_LOCATION` (read from `env`, Go's error texts), which become the config's
/// project and location.
fn generate_client_config(
    is_vertex_ai: bool,
    backend: Backend,
    http_options: HttpOptions,
    http_client: Option<Arc<dyn HttpClient>>,
    env: Arc<dyn EnvProvider>,
) -> Result<ClientConfig, GeminiError> {
    let mut client_config = ClientConfig {
        backend: Some(backend),
        http_options,
        ..ClientConfig::default()
    };

    let project = env.var(PROJECT_ENV_VAR).unwrap_or_default();
    let location = env.var(LOCATION_ENV_VAR).unwrap_or_default();
    if is_vertex_ai {
        if project.is_empty() {
            return Err(GeminiError::Config(format!(
                "{PROJECT_ENV_VAR} environment variable must be set"
            )));
        }
        if location.is_empty() {
            return Err(GeminiError::Config(format!(
                "{LOCATION_ENV_VAR} environment variable must be set"
            )));
        }
        client_config.project = Some(project);
        client_config.location = Some(location);
    }

    client_config.http_client = http_client;
    client_config.env = env;
    Ok(client_config)
}

/// A Gemini model reached through an Apigee proxy (port of Go `apigeeModel`): it configures a
/// [`GeminiModel`] and delegates every request to it.
pub struct ApigeeModel {
    delegate: GeminiModel,
    name: String,
}

impl fmt::Debug for ApigeeModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApigeeModel")
            .field("name", &self.name)
            .field("delegate", &self.delegate)
            .finish()
    }
}

impl ApigeeModel {
    /// Port of Go `apigee.NewModel`: parses `apigee/[vertex_ai|gemini/][vN/]<model>`,
    /// resolves the proxy URL and the Vertex environment, and builds the delegate Gemini model.
    pub async fn new(name: &str, cfg: ApigeeConfig) -> Result<ApigeeModel, GeminiError> {
        let ApigeeConfig {
            proxy_url,
            custom_headers,
            http_client,
            env,
        } = cfg;

        if !name.starts_with("apigee/") {
            return Err(invalid_model_string(name));
        }
        let mi = parse_model_name(name, env.as_ref())?;

        let proxy_url = resolve_proxy_url(proxy_url.as_deref(), env.as_ref());
        if proxy_url.is_empty() {
            return Err(GeminiError::Config(format!(
                "{APIGEE_PROXY_URL_ENV_VAR} environment variable not set"
            )));
        }

        let http_options = generate_http_options(proxy_url, &mi.api_version, &custom_headers);
        let backend = backend_type(mi.is_vertex_ai);
        let client_config =
            generate_client_config(mi.is_vertex_ai, backend, http_options, http_client, env)?;

        let delegate = GeminiModel::new_concrete(&mi.model_id, client_config).await?;
        Ok(ApigeeModel {
            delegate,
            name: name.to_string(),
        })
    }
}

impl Llm for ApigeeModel {
    /// The full `apigee/…` name given to [`ApigeeModel::new`] (Go `Name`).
    fn name(&self) -> &str {
        &self.name
    }

    /// Delegates to the Gemini model (Go `GenerateContent`).
    fn generate_content(
        &self,
        mut req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        // The flow sets req.model to name(), the full "apigee/..." string, and the
        // delegate prefers req.model over the model ID it was built with. Only the
        // model ID, the last path component, names a model on the proxy (Go 71b0761f).
        if let Some(i) = req.model.rfind('/') {
            req.model = req.model[i + 1..].to_string();
        }
        self.delegate.generate_content(req, stream)
    }

    // `google_variant` keeps the trait default (`None`): Go's `apigeeModel` has no
    // `GetGoogleLLMVariant` method, so it is not a `model.GoogleLLM`.

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gadk_platform::env::MapEnv;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> Arc<dyn EnvProvider> {
        Arc::new(MapEnv(
            vars.iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<HashMap<_, _>>(),
        ))
    }

    #[test]
    fn apigee_config_debug_redacts_proxy_url_credentials() {
        let cfg = ApigeeConfig {
            proxy_url: Some("https://user:url-secret@proxy.example/apigee".into()),
            ..ApigeeConfig::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("url-secret"), "{dbg}");
        assert!(!dbg.contains("user"), "{dbg}");
        assert!(dbg.contains("proxy.example/apigee"), "{dbg}");
    }

    #[test]
    fn is_enabled_accepts_true_any_case_and_one() {
        for (val, want) in [
            ("true", true),
            ("TRUE", true),
            ("True", true),
            ("1", true),
            ("0", false),
            ("yes", false),
            ("", false),
        ] {
            let env = env(&[("V", val)]);
            assert_eq!(is_enabled(env.as_ref(), "V"), want, "{val:?}");
        }
        assert!(!is_enabled(env(&[]).as_ref(), "V"));
    }

    #[test]
    fn resolve_proxy_url_prefers_the_configured_url() {
        let env = env(&[("APIGEE_PROXY_URL", "https://env")]);
        assert_eq!(
            resolve_proxy_url(Some("https://cfg"), env.as_ref()),
            "https://cfg"
        );
        assert_eq!(resolve_proxy_url(Some(""), env.as_ref()), "https://env");
        assert_eq!(resolve_proxy_url(None, env.as_ref()), "https://env");
        assert_eq!(resolve_proxy_url(None, self::env(&[]).as_ref()), "");
    }

    #[test]
    fn generate_http_options_sets_base_url_version_and_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom-header", http::HeaderValue::from_static("v"));
        let o = generate_http_options("https://p".into(), "v1", &headers);
        assert_eq!(o.base_url.as_deref(), Some("https://p"));
        assert_eq!(o.api_version.as_deref(), Some("v1"));
        assert_eq!(o.headers, headers);

        let o = generate_http_options("https://p".into(), "", &http::HeaderMap::new());
        assert_eq!(o.api_version, None);
        assert!(o.headers.is_empty());
    }

    #[test]
    fn generate_client_config_sets_vertex_project_and_location() {
        let env = env(&[
            ("GOOGLE_CLOUD_PROJECT", "p"),
            ("GOOGLE_CLOUD_LOCATION", "l"),
        ]);
        let cfg = generate_client_config(
            true,
            Backend::VertexAi,
            HttpOptions::default(),
            None,
            env.clone(),
        )
        .unwrap();
        assert_eq!(cfg.backend, Some(Backend::VertexAi));
        assert_eq!(cfg.project.as_deref(), Some("p"));
        assert_eq!(cfg.location.as_deref(), Some("l"));

        // The Gemini API ignores the Vertex variables.
        let cfg =
            generate_client_config(false, Backend::GeminiApi, HttpOptions::default(), None, env)
                .unwrap();
        assert_eq!(cfg.backend, Some(Backend::GeminiApi));
        assert_eq!(cfg.project, None);
        assert_eq!(cfg.location, None);
    }

    /// An `HttpClient` that is never called (it lets a Vertex model skip ADC).
    struct NoHttp;

    #[async_trait::async_trait]
    impl HttpClient for NoHttp {
        async fn send(
            &self,
            _req: http::Request<bytes::Bytes>,
        ) -> Result<http::Response<gadk_model::BodyStream>, ModelError> {
            Err(ModelError::Transport("no requests expected".into()))
        }
    }

    #[tokio::test]
    async fn name_and_delegate_endpoint() {
        let cfg = ApigeeConfig {
            proxy_url: Some("https://test.apigee.net".into()),
            env: env(&[("GOOGLE_API_KEY", "k")]),
            ..ApigeeConfig::default()
        };
        let m = ApigeeModel::new("apigee/v1/gemini-1.5-flash", cfg)
            .await
            .unwrap();
        assert_eq!(m.name(), "apigee/v1/gemini-1.5-flash");
        let resolved = m.delegate.resolved();
        assert_eq!(resolved.backend, Backend::GeminiApi);
        assert_eq!(resolved.base_url, "https://test.apigee.net/");
        assert_eq!(resolved.api_version, "v1");
    }

    /// Go's `apigeeModel` does not implement `model.GoogleLLM` (it has no
    /// `GetGoogleLLMVariant` method, `model/apigee/apigee.go`), so `google_variant` is the
    /// trait default `None` for both backends, not the delegate's variant.
    #[tokio::test]
    async fn google_variant_is_none_like_go() {
        for name in [
            "apigee/gemini/v1/gemini-1.5-flash",
            "apigee/vertex_ai/v1beta/gemini-1.5-flash",
        ] {
            let cfg = ApigeeConfig {
                proxy_url: Some("https://test.apigee.net".into()),
                http_client: Some(Arc::new(NoHttp)),
                env: env(&[
                    ("GOOGLE_API_KEY", "k"),
                    ("GOOGLE_CLOUD_PROJECT", "p"),
                    ("GOOGLE_CLOUD_LOCATION", "l"),
                ]),
                ..ApigeeConfig::default()
            };
            let m = ApigeeModel::new(name, cfg).await.unwrap();
            assert_eq!(m.google_variant(), None, "{name}");
        }
    }
}
