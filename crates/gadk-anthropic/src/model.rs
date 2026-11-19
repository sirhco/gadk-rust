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

//! The Claude model: construction, request building (`build_message_params`) and the
//! [`Llm`] implementation.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use gadk_model::rest::{DEFAULT_MAX_RETRIES, RestTransport, RetryPolicy, header_str};
use gadk_model::{
    HttpClient, Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient, collect_body,
};
use gadk_platform::env::EnvProvider;
use google_cloud_auth::credentials::Credentials;
use http::header::CONTENT_TYPE;

use crate::cache::apply_cache_breakpoints;
use crate::client::{
    ANTHROPIC_DEFAULT_BASE_URL, Auth, DEFAULT_RESPONSE_HEADER_TIMEOUT, Endpoint, Transport,
    vertex_base_url,
};
use crate::config::{
    Backend, ClientConfig, DEFAULT_MAX_TOKENS, Effort, PromptCacheConfig, parse_model_name,
    validate_base_url, validate_vertex_location, validate_vertex_project,
};
use crate::error::{AnthropicError, ErrorKind, go_quote};
use crate::request::{ConvertCtx, build_messages, build_system_blocks};
use crate::response::{convert_message_to_llm_response, decode_message};
use crate::stream::stream_responses;
use crate::thinking::{build_output_config_param, build_thinking_param, is_thinking_enabled};
use crate::tools::build_tools_and_choice;
use crate::validate::{invalid_config, validate_generate_content_config};
use crate::wire::MessageNewParams;

/// OAuth scope requested for Application Default Credentials.
pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The error detail for an Application Default Credentials failure.
const ADC_FAILURE: &str = "failed to find Google Application Default Credentials for Vertex AI";

/// A Claude model on the Anthropic Messages API or Vertex AI.
///
/// Build it with [`AnthropicModel::new`]; it implements [`Llm`]. Requests never mutate the
/// caller's [`LlmRequest`], and one model serves concurrent requests.
pub struct AnthropicModel {
    name: String,
    model_id: String,
    backend: Backend,
    project: String,
    location: String,
    max_tokens: u32,
    effort: Option<Effort>,
    prompt_cache: Option<PromptCacheConfig>,
    transport: Arc<Transport>,
}

/// Credentials (API key, auth token, Vertex credentials, header values) are left out, and
/// user info in the base URL is redacted.
impl std::fmt::Debug for AnthropicModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicModel")
            .field("name", &self.name)
            .field("model_id", &self.model_id)
            .field("backend", &self.backend)
            .field("project", &self.project)
            .field("location", &self.location)
            .field(
                "base_url",
                &gadk_model::redact_url(&self.transport.base_url),
            )
            .field("max_tokens", &self.max_tokens)
            .field("effort", &self.effort)
            .field("prompt_cache", &self.prompt_cache)
            .finish_non_exhaustive()
    }
}

/// What [`AnthropicModel::build_message_params`] returns: the params, the request options
/// and the unwrap-output flag.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltParams {
    /// The Messages params.
    pub(crate) params: MessageNewParams,
    /// The resolved model ID (the Vertex URL's model segment).
    pub(crate) model_id: String,
    /// The per-attempt timeout (`LlmRequest.http_options.timeout` when > 0).
    pub(crate) timeout: Option<Duration>,
    /// Whether text responses must be unwrapped from `{"result": …}`.
    pub(crate) unwrap_output: bool,
}

/// `value` trimmed, `""` for `None`.
fn trimmed(value: Option<&str>) -> &str {
    value.map(str::trim).unwrap_or_default()
}

/// The environment variable `key`, trimmed.
fn env_trimmed(env: &dyn EnvProvider, key: &str) -> String {
    env.var(key)
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

/// The first non-empty of the values.
fn first_non_empty(values: [&str; 4]) -> String {
    values
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The resolved Anthropic or Vertex half of `new`.
struct Resolved {
    base_url: String,
    endpoint: Endpoint,
    auth: Auth,
    project: String,
    location: String,
    /// The default HTTP client (10-minute response-header timeout) applies.
    default_client_timeout: bool,
}

impl AnthropicModel {
    /// Validates `name` and `cfg` in the order given on [`new_concrete`](Self::new_concrete),
    /// resolves credentials (env, ADC) and the endpoint, and builds the model.
    #[allow(clippy::new_ret_no_self)] // returns the model as `Arc<dyn Llm>`
    pub async fn new(name: &str, cfg: ClientConfig) -> Result<Arc<dyn Llm>, AnthropicError> {
        Ok(Arc::new(Self::new_concrete(name, cfg).await?))
    }

    /// [`new`](Self::new), returning the concrete type.
    ///
    /// Order: model name, max tokens (`None`/`Some(0)` → [`DEFAULT_MAX_TOKENS`]),
    /// `base_url` validation, then per backend:
    /// - Anthropic: a resource name is rejected; credentials are `api_key`/`auth_token`
    ///   (trimmed), else — only without a custom `base_url` — `ANTHROPIC_BASE_URL` (validated),
    ///   `ANTHROPIC_API_KEY`, then `ANTHROPIC_AUTH_TOKEN` read through `cfg.env`;
    /// - Vertex: project and location from `cfg`, the resource name, then
    ///   `GOOGLE_CLOUD_PROJECT`/`CLOUD_ML_PROJECT_ID` and
    ///   `GOOGLE_CLOUD_LOCATION`/`CLOUD_ML_REGION`; a custom `base_url` needs explicit
    ///   `credentials`; credentials (or ADC) are built now.
    ///
    /// The backend, negative-value, effort and TTL checks are enforced by the types.
    pub async fn new_concrete(name: &str, cfg: ClientConfig) -> Result<Self, AnthropicError> {
        let parsed = parse_model_name(name)?;
        let backend = cfg.backend;
        let max_tokens = match cfg.max_tokens {
            None | Some(0) => DEFAULT_MAX_TOKENS,
            Some(n) => n,
        };
        let base_url = cfg.base_url.as_deref().filter(|u| !u.is_empty());
        if let Some(url) = base_url {
            validate_base_url("base_url", url)?;
        }
        let has_custom_base_url = base_url.is_some();

        let resolved = match backend {
            Backend::Anthropic => resolve_anthropic(&cfg, parsed.is_resource, has_custom_base_url)?,
            Backend::Vertex => resolve_vertex(&cfg, &parsed, has_custom_base_url)?,
        };

        let (http, response_header_timeout): (Arc<dyn HttpClient>, Option<Duration>) =
            match cfg.http_client {
                Some(h) => (h, None),
                None => (
                    Arc::new(ReqwestClient::default()),
                    resolved
                        .default_client_timeout
                        .then_some(DEFAULT_RESPONSE_HEADER_TIMEOUT),
                ),
            };

        Ok(Self {
            name: name.trim().to_string(),
            model_id: parsed.model_id,
            backend,
            project: resolved.project,
            location: resolved.location,
            max_tokens,
            effort: cfg.effort,
            prompt_cache: cfg.prompt_cache,
            transport: Arc::new(Transport {
                rest: RestTransport {
                    http,
                    policy: RetryPolicy {
                        max_retries: cfg.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
                        ..RetryPolicy::ANTHROPIC
                    },
                    response_header_timeout,
                },
                base_url: resolved.base_url,
                endpoint: resolved.endpoint,
                auth: resolved.auth,
                headers: cfg.headers,
            }),
        })
    }

    /// The configured model ID for an empty `req_model`, else
    /// `req_model` parsed, with errors for a backend, project or location mismatch.
    pub(crate) fn resolve_model_id(&self, req_model: &str) -> Result<String, AnthropicError> {
        let trimmed = req_model.trim();
        if trimmed.is_empty() {
            return Ok(self.model_id.clone());
        }
        let parsed = parse_model_name(trimmed)?;
        if parsed.is_resource {
            if self.backend != Backend::Vertex {
                return Err(invalid_config(
                    "Vertex resource model name requires Backend::Vertex",
                ));
            }
            if parsed.project != self.project {
                return Err(invalid_config(format!(
                    "request model project {} does not match configured project {}",
                    go_quote(&parsed.project),
                    go_quote(&self.project)
                )));
            }
            if parsed.location != self.location {
                return Err(invalid_config(format!(
                    "request model location {} does not match configured location {}",
                    go_quote(&parsed.location),
                    go_quote(&self.location)
                )));
            }
        }
        Ok(parsed.model_id)
    }

    /// Builds the request params: model ID, config validation, max tokens, system blocks, tools
    /// and choice, messages, cache breakpoints, output config, thinking, sampling parameters
    /// (omitted with the warning `omitting sampling parameters when thinking or effort is
    /// active`, fields `backend` and `model`, when thinking or effort is active), stop
    /// sequences, and the per-attempt timeout.
    ///
    /// `params.model` is always set; [`Transport::encode_body`] leaves it out of a Vertex body.
    pub(crate) fn build_message_params(
        &self,
        req: &LlmRequest,
    ) -> Result<BuiltParams, AnthropicError> {
        let model_id = self.resolve_model_id(&req.model)?;
        let cfg = req.config.as_ref();

        validate_generate_content_config(cfg, &req.tools, self.effort, req.http_options.as_ref())?;

        let max_tokens = match cfg {
            Some(c) if c.max_output_tokens > 0 => u64::from(c.max_output_tokens.unsigned_abs()),
            _ => u64::from(self.max_tokens),
        };

        let mut system = build_system_blocks(cfg);
        let (mut tools, tool_choice) = build_tools_and_choice(cfg, &req.tools)?;
        let cx = ConvertCtx {
            backend: self.backend,
            model_id: &model_id,
        };
        let mut messages = build_messages(cx, &req.contents)?;
        apply_cache_breakpoints(
            self.prompt_cache.as_ref(),
            &mut system,
            &mut messages,
            &mut tools,
        );
        let (output_config, unwrap_output) = build_output_config_param(self.effort, cfg)?;

        let mut params = MessageNewParams {
            max_tokens,
            messages,
            model: model_id.clone(),
            output_config,
            system,
            thinking: build_thinking_param(cfg),
            tool_choice,
            tools,
            ..MessageNewParams::default()
        };

        if let Some(c) = cfg {
            if is_thinking_enabled(c.thinking_config.as_ref()) || self.effort.is_some() {
                if c.temperature.is_some() || c.top_p.is_some() || c.top_k.is_some() {
                    tracing::warn!(
                        backend = self.backend.as_str(),
                        model = model_id.as_str(),
                        "omitting sampling parameters when thinking or effort is active"
                    );
                }
            } else {
                params.temperature = c.temperature.map(f64::from);
                params.top_p = c.top_p.map(f64::from);
                // Truncation toward zero.
                params.top_k = c.top_k.map(|k| k as i64);
            }
            if !c.stop_sequences.is_empty() {
                params.stop_sequences = c.stop_sequences.clone();
            }
        }

        let timeout = req
            .http_options
            .as_ref()
            .and_then(|o| o.timeout)
            .filter(|t| !t.is_zero());

        Ok(BuiltParams {
            params,
            model_id,
            timeout,
            unwrap_output,
        })
    }
}

/// Resolves the Anthropic backend.
fn resolve_anthropic(
    cfg: &ClientConfig,
    is_resource: bool,
    has_custom_base_url: bool,
) -> Result<Resolved, AnthropicError> {
    if is_resource {
        return Err(invalid_config(
            "Vertex resource model name requires Backend::Vertex",
        ));
    }
    let env = cfg.env.as_ref();
    let mut api_key = trimmed(cfg.api_key.as_deref()).to_string();
    let mut auth_token = trimmed(cfg.auth_token.as_deref()).to_string();
    let mut base_url = trimmed(cfg.base_url.as_deref()).to_string();

    if has_custom_base_url {
        if api_key.is_empty() && auth_token.is_empty() {
            return Err(invalid_config(
                "custom base_url on Backend::Anthropic requires an explicit api_key or auth_token in ClientConfig; ambient environment credentials are not sent to custom endpoints",
            ));
        }
    } else {
        let env_base_url = env_trimmed(env, "ANTHROPIC_BASE_URL");
        if !env_base_url.is_empty() {
            validate_base_url("ANTHROPIC_BASE_URL", &env_base_url)?;
            base_url = env_base_url;
        }
        if api_key.is_empty() && auth_token.is_empty() {
            let env_key = env_trimmed(env, "ANTHROPIC_API_KEY");
            if !env_key.is_empty() {
                api_key = env_key;
            } else {
                auth_token = env_trimmed(env, "ANTHROPIC_AUTH_TOKEN");
            }
        }
    }

    if api_key.is_empty() && auth_token.is_empty() {
        return Err(AnthropicError::wrap2(
            ErrorKind::InvalidConfig,
            ErrorKind::Authentication,
            "no Anthropic credential found; set ClientConfig.api_key, ClientConfig.auth_token, ANTHROPIC_API_KEY, or ANTHROPIC_AUTH_TOKEN",
        ));
    }

    Ok(Resolved {
        base_url: if base_url.is_empty() {
            ANTHROPIC_DEFAULT_BASE_URL.to_string()
        } else {
            base_url
        },
        endpoint: Endpoint::Anthropic,
        auth: Auth::Anthropic {
            api_key: Some(api_key).filter(|k| !k.is_empty()),
            auth_token: Some(auth_token).filter(|t| !t.is_empty()),
        },
        project: String::new(),
        location: String::new(),
        default_client_timeout: true,
    })
}

/// Resolves the Vertex backend.
fn resolve_vertex(
    cfg: &ClientConfig,
    parsed: &crate::config::ParsedModelName,
    has_custom_base_url: bool,
) -> Result<Resolved, AnthropicError> {
    let env = cfg.env.as_ref();
    let cfg_project = trimmed(cfg.project.as_deref());
    let cfg_location = trimmed(cfg.location.as_deref());

    if parsed.is_resource {
        if !cfg_project.is_empty() && cfg_project != parsed.project {
            return Err(invalid_config(format!(
                "ClientConfig.project {} does not match resource project {}",
                go_quote(cfg_project),
                go_quote(&parsed.project)
            )));
        }
        if !cfg_location.is_empty() && cfg_location != parsed.location {
            return Err(invalid_config(format!(
                "ClientConfig.location {} does not match resource location {}",
                go_quote(cfg_location),
                go_quote(&parsed.location)
            )));
        }
    }

    let project = first_non_empty([
        cfg_project,
        &parsed.project,
        &env_trimmed(env, "GOOGLE_CLOUD_PROJECT"),
        &env_trimmed(env, "CLOUD_ML_PROJECT_ID"),
    ]);
    let location = first_non_empty([
        cfg_location,
        &parsed.location,
        &env_trimmed(env, "GOOGLE_CLOUD_LOCATION"),
        &env_trimmed(env, "CLOUD_ML_REGION"),
    ]);

    if project.is_empty() {
        return Err(invalid_config(
            "Backend::Vertex requires a project via ClientConfig.project, a full resource model name, GOOGLE_CLOUD_PROJECT, or CLOUD_ML_PROJECT_ID",
        ));
    }
    if location.is_empty() {
        return Err(invalid_config(
            "Backend::Vertex requires a location via ClientConfig.location, a full resource model name, GOOGLE_CLOUD_LOCATION, or CLOUD_ML_REGION",
        ));
    }
    validate_vertex_project(&project)?;
    validate_vertex_location(&location)?;

    if has_custom_base_url && cfg.credentials.is_none() {
        return Err(invalid_config(
            "custom base_url on Backend::Vertex requires explicit ClientConfig.credentials; Application Default Credentials are not sent to custom endpoints",
        ));
    }

    let credentials = resolve_vertex_credentials(cfg)?;

    let base_url = if has_custom_base_url {
        trimmed(cfg.base_url.as_deref()).to_string()
    } else {
        vertex_base_url(&location)
    };

    Ok(Resolved {
        base_url,
        endpoint: Endpoint::Vertex {
            project: project.clone(),
            location: location.clone(),
        },
        auth: Auth::Vertex(credentials),
        project,
        location,
        default_client_timeout: false,
    })
}

/// The credentials: `cfg.credentials`, else Application Default Credentials with
/// [`CLOUD_PLATFORM_SCOPE`] (see [`adc_credentials`]).
pub(crate) fn resolve_vertex_credentials(
    cfg: &ClientConfig,
) -> Result<Credentials, AnthropicError> {
    match &cfg.credentials {
        Some(credentials) => Ok(credentials.clone()),
        None => adc_credentials(cfg.env.as_ref()),
    }
}

/// Application Default Credentials read through `env` ([`gadk_model::gcp_auth::adc_credentials`]
/// with `env`'s `GOOGLE_APPLICATION_CREDENTIALS` and [`CLOUD_PLATFORM_SCOPE`]). Any failure is
/// the `ErrorKind::InvalidConfig` + `ErrorKind::Authentication` text `failed to find Google
/// Application Default Credentials for Vertex AI`; the underlying detail is logged at `debug`
/// only (the error carries none).
pub(crate) fn adc_credentials(env: &dyn EnvProvider) -> Result<Credentials, AnthropicError> {
    let file = env.var("GOOGLE_APPLICATION_CREDENTIALS");
    gadk_model::gcp_auth::adc_credentials(file.as_deref(), &[CLOUD_PLATFORM_SCOPE]).map_err(|e| {
        tracing::debug!(detail = %e, "anthropicmodel: Application Default Credentials lookup failed");
        AnthropicError::wrap2(
            ErrorKind::InvalidConfig,
            ErrorKind::Authentication,
            ADC_FAILURE,
        )
    })
}

/// One non-streaming call, converted to an [`LlmResponse`].
async fn generate_once(
    transport: Arc<Transport>,
    built: BuiltParams,
) -> Result<LlmResponse, AnthropicError> {
    let body = transport.encode_body(built.params, false)?;
    let resp = transport
        .send(&built.model_id, body, false, built.timeout)
        .await?;
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = collect_body(resp.into_body())
        .await
        .map_err(AnthropicError::from_model_error)?;
    let msg = decode_message(&content_type, &bytes)?;
    convert_message_to_llm_response(&msg, built.unwrap_output)
}

/// Opens one streaming call: its responses come from
/// [`stream_responses`].
async fn open_stream(
    transport: Arc<Transport>,
    built: BuiltParams,
) -> Result<BoxStream<'static, Result<LlmResponse, AnthropicError>>, AnthropicError> {
    let body = transport.encode_body(built.params, true)?;
    let resp = transport
        .send(&built.model_id, body, true, built.timeout)
        .await?;
    let status = resp.status().as_u16();
    let request_id = header_str(resp.headers(), "request-id").to_string();
    Ok(stream_responses(
        resp.into_body(),
        built.unwrap_output,
        status,
        request_id,
    ))
}

impl Llm for AnthropicModel {
    /// The name given to [`AnthropicModel::new`], trimmed.
    fn name(&self) -> &str {
        &self.name
    }

    /// Builds params (`build_message_params`), then returns one final response
    /// (non-streaming) or partials and a final response (streaming), every error as
    /// `ModelError::Other(AnthropicError)`. The request is sent on the stream's first poll;
    /// dropping the stream cancels it.
    ///
    /// The params are built (and any sampling warning logged) when this is called; the
    /// caller's request is only read.
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let built = self.build_message_params(&req);
        let transport = self.transport.clone();
        if stream {
            futures::stream::once(async move { open_stream(transport, built?).await })
                .try_flatten()
                .map_err(ModelError::from)
                .boxed()
        } else {
            futures::stream::once(async move { generate_once(transport, built?).await })
                .map_err(ModelError::from)
                .boxed()
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bytes::Bytes;

    use gadk_genai::{Content, GenerateContentConfig, ThinkingConfig};
    use gadk_model::rest::test_util::bearer_credentials;
    use gadk_model::{BodyStream, HttpOptions};
    use gadk_platform::env::MapEnv;
    use http::{HeaderMap, HeaderValue};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::config::PromptCacheTtl;

    const MODEL: &str = "claude-sonnet-4-5-20250929";
    const RESOURCE: &str =
        "projects/res-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5";

    fn env(vars: &[(&str, &str)]) -> Arc<MapEnv> {
        Arc::new(MapEnv(
            vars.iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        ))
    }

    fn anthropic(api_key: Option<&str>, vars: &[(&str, &str)]) -> ClientConfig {
        ClientConfig {
            api_key: api_key.map(str::to_string),
            env: env(vars),
            ..Default::default()
        }
    }

    fn vertex(vars: &[(&str, &str)]) -> ClientConfig {
        ClientConfig {
            backend: Backend::Vertex,
            credentials: Some(bearer_credentials("vertex-token")),
            env: env(vars),
            ..Default::default()
        }
    }

    async fn new_err(name: &str, cfg: ClientConfig) -> AnthropicError {
        match AnthropicModel::new_concrete(name, cfg).await {
            Ok(_) => panic!("new_concrete({name:?}) succeeded"),
            Err(e) => e,
        }
    }

    #[tokio::test]
    async fn debug_redacts_secrets() {
        let cfg = ClientConfig {
            auth_token: Some("tok-secret".into()),
            base_url: Some("https://user:url-secret@proxy.example/v1".into()),
            ..anthropic(Some("sk-ant-secret"), &[])
        };
        let m = AnthropicModel::new_concrete("claude-x", cfg).await.unwrap();
        let dbg = format!("{m:?}");
        for secret in ["sk-ant-secret", "tok-secret", "url-secret"] {
            assert!(!dbg.contains(secret), "{dbg}");
        }
        assert!(dbg.contains("AnthropicModel"), "{dbg}");
        assert!(dbg.contains("claude-x"), "{dbg}");
        assert!(dbg.contains("proxy.example/v1"), "{dbg}");
    }

    fn anthropic_auth(m: &AnthropicModel) -> (Option<String>, Option<String>) {
        match &m.transport.auth {
            Auth::Anthropic {
                api_key,
                auth_token,
            } => (api_key.clone(), auth_token.clone()),
            Auth::Vertex(_) => panic!("Vertex auth"),
        }
    }

    /// A file in the temp directory, deleted on drop.
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "gadk-anthropic-model-{}-{}-{name}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }

        fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    // ------------------------------------------------------------------------ construction

    #[tokio::test]
    async fn new_anthropic_with_explicit_key_uses_defaults() {
        let m = AnthropicModel::new_concrete(
            "  anthropic/claude-sonnet-4-5  ",
            anthropic(
                Some(" key "),
                &[("ANTHROPIC_BASE_URL", "https://ignored.example")],
            ),
        )
        .await
        .unwrap();
        assert_eq!(m.name(), "anthropic/claude-sonnet-4-5");
        assert_eq!(m.model_id, "claude-sonnet-4-5");
        assert_eq!(m.backend, Backend::Anthropic);
        assert_eq!(m.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(m.effort, None);
        assert_eq!(m.prompt_cache, None);
        assert_eq!(anthropic_auth(&m), (Some("key".to_string()), None));
        // ANTHROPIC_BASE_URL applies without a custom base URL.
        assert_eq!(m.transport.base_url, "https://ignored.example");
        assert_eq!(m.transport.endpoint, Endpoint::Anthropic);
        assert_eq!(m.transport.rest.policy.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(
            m.transport.rest.response_header_timeout,
            Some(DEFAULT_RESPONSE_HEADER_TIMEOUT)
        );

        let m = AnthropicModel::new_concrete(MODEL, anthropic(Some("key"), &[]))
            .await
            .unwrap();
        assert_eq!(m.transport.base_url, ANTHROPIC_DEFAULT_BASE_URL);
    }

    #[tokio::test]
    async fn new_anthropic_env_credential_precedence() {
        // ANTHROPIC_API_KEY before ANTHROPIC_AUTH_TOKEN, trimmed.
        let m = AnthropicModel::new_concrete(
            MODEL,
            anthropic(
                None,
                &[
                    ("ANTHROPIC_API_KEY", " env-key "),
                    ("ANTHROPIC_AUTH_TOKEN", "env-token"),
                ],
            ),
        )
        .await
        .unwrap();
        assert_eq!(anthropic_auth(&m), (Some("env-key".to_string()), None));

        let m = AnthropicModel::new_concrete(
            MODEL,
            anthropic(Some("   "), &[("ANTHROPIC_AUTH_TOKEN", "env-token")]),
        )
        .await
        .unwrap();
        assert_eq!(anthropic_auth(&m), (None, Some("env-token".to_string())));

        // An explicit credential wins; env credentials are not read.
        let cfg = ClientConfig {
            auth_token: Some("cfg-token".to_string()),
            env: env(&[("ANTHROPIC_API_KEY", "env-key")]),
            ..Default::default()
        };
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(anthropic_auth(&m), (None, Some("cfg-token".to_string())));

        // Both explicit: both are sent.
        let cfg = ClientConfig {
            api_key: Some("k".to_string()),
            auth_token: Some("t".to_string()),
            env: env(&[]),
            ..Default::default()
        };
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(
            anthropic_auth(&m),
            (Some("k".to_string()), Some("t".to_string()))
        );
    }

    #[tokio::test]
    async fn new_anthropic_custom_base_url_needs_explicit_credential() {
        let mut cfg = anthropic(None, &[("ANTHROPIC_API_KEY", "env-key")]);
        cfg.base_url = Some("https://proxy.example/v1".to_string());
        let err = new_err(MODEL, cfg).await;
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: custom base_url on Backend::Anthropic requires an explicit api_key or auth_token in ClientConfig; ambient environment credentials are not sent to custom endpoints"
        );

        let mut cfg = anthropic(
            Some("key"),
            &[("ANTHROPIC_BASE_URL", "https://env.example")],
        );
        cfg.base_url = Some(" https://proxy.example/v1 ".to_string());
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(m.transport.base_url, "https://proxy.example/v1");
    }

    #[tokio::test]
    async fn new_anthropic_without_credentials_is_config_and_auth_error() {
        let err = new_err(MODEL, anthropic(Some(""), &[])).await;
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: anthropicmodel: authentication failed: no Anthropic credential found; set ClientConfig.api_key, ClientConfig.auth_token, ANTHROPIC_API_KEY, or ANTHROPIC_AUTH_TOKEN"
        );
        assert!(err.is(ErrorKind::InvalidConfig));
        assert!(err.is(ErrorKind::Authentication));
    }

    #[tokio::test]
    async fn new_validation_errors_in_go_order() {
        // The model name comes first.
        let mut cfg = anthropic(None, &[]);
        cfg.base_url = Some("   ".to_string());
        assert_eq!(
            new_err("", cfg).await.to_string(),
            "anthropicmodel: invalid configuration: model name must not be empty"
        );
        // Then the base URL, before credentials.
        let mut cfg = anthropic(None, &[]);
        cfg.base_url = Some("   ".to_string());
        assert_eq!(
            new_err(MODEL, cfg).await.to_string(),
            "anthropicmodel: invalid configuration: base_url must not be whitespace-only"
        );
        // `Some("")` is unset (Go `cfg.BaseURL != ""`).
        let mut cfg = anthropic(Some("key"), &[]);
        cfg.base_url = Some(String::new());
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(m.transport.base_url, ANTHROPIC_DEFAULT_BASE_URL);
        // A resource name needs Vertex.
        assert_eq!(
            new_err(RESOURCE, anthropic(Some("key"), &[]))
                .await
                .to_string(),
            "anthropicmodel: invalid configuration: Vertex resource model name requires Backend::Vertex"
        );
        // ANTHROPIC_BASE_URL is validated.
        assert_eq!(
            new_err(
                MODEL,
                anthropic(Some("key"), &[("ANTHROPIC_BASE_URL", "ftp://x")])
            )
            .await
            .to_string(),
            "anthropicmodel: invalid configuration: ANTHROPIC_BASE_URL must be a valid http or https URL with a host"
        );
    }

    #[tokio::test]
    async fn new_carries_limits_effort_cache_headers_and_client() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom", HeaderValue::from_static("1"));
        let client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::default());
        let cfg = ClientConfig {
            api_key: Some("key".to_string()),
            max_tokens: Some(0),
            max_retries: Some(0),
            effort: Some(Effort::High),
            prompt_cache: Some(PromptCacheConfig {
                ttl: PromptCacheTtl::OneHour,
            }),
            headers: headers.clone(),
            http_client: Some(client),
            env: env(&[]),
            ..Default::default()
        };
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(m.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(m.transport.rest.policy.max_retries, 0);
        assert_eq!(m.effort, Some(Effort::High));
        assert_eq!(
            m.prompt_cache,
            Some(PromptCacheConfig {
                ttl: PromptCacheTtl::OneHour
            })
        );
        assert_eq!(m.transport.headers, headers);
        // A custom client gets no response-header timeout (Go sets it on its default only).
        assert_eq!(m.transport.rest.response_header_timeout, None);

        let mut cfg = anthropic(Some("key"), &[]);
        cfg.max_tokens = Some(100);
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(m.max_tokens, 100);
    }

    #[tokio::test]
    async fn new_vertex_resolves_project_and_location() {
        // From the resource name.
        let m = AnthropicModel::new_concrete(RESOURCE, vertex(&[]))
            .await
            .unwrap();
        assert_eq!(m.model_id, "claude-sonnet-4-5");
        assert_eq!(
            (m.project.as_str(), m.location.as_str()),
            ("res-proj", "us-east5")
        );
        assert_eq!(
            m.transport.endpoint,
            Endpoint::Vertex {
                project: "res-proj".to_string(),
                location: "us-east5".to_string()
            }
        );
        assert_eq!(
            m.transport.base_url,
            "https://us-east5-aiplatform.googleapis.com/"
        );
        assert_eq!(m.transport.rest.response_header_timeout, None);
        assert!(matches!(m.transport.auth, Auth::Vertex(_)));

        // Config, then env in Go's order.
        let m = AnthropicModel::new_concrete(
            MODEL,
            vertex(&[
                ("GOOGLE_CLOUD_PROJECT", " env-proj "),
                ("CLOUD_ML_PROJECT_ID", "ml-proj"),
                ("CLOUD_ML_REGION", "global"),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(
            (m.project.as_str(), m.location.as_str()),
            ("env-proj", "global")
        );
        assert_eq!(m.transport.base_url, "https://aiplatform.googleapis.com/");

        let mut cfg = vertex(&[
            ("CLOUD_ML_PROJECT_ID", "ml-proj"),
            ("GOOGLE_CLOUD_LOCATION", "eu"),
            ("CLOUD_ML_REGION", "us"),
        ]);
        cfg.project = Some(" cfg-proj ".to_string());
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        assert_eq!(
            (m.project.as_str(), m.location.as_str()),
            ("cfg-proj", "eu")
        );

        let m = AnthropicModel::new_concrete(
            MODEL,
            vertex(&[
                ("CLOUD_ML_PROJECT_ID", "ml-proj"),
                ("CLOUD_ML_REGION", "us"),
            ]),
        )
        .await
        .unwrap();
        assert_eq!((m.project.as_str(), m.location.as_str()), ("ml-proj", "us"));
    }

    #[tokio::test]
    async fn new_vertex_errors() {
        let mut cfg = vertex(&[]);
        cfg.project = Some("other".to_string());
        assert_eq!(
            new_err(RESOURCE, cfg).await.to_string(),
            "anthropicmodel: invalid configuration: ClientConfig.project \"other\" does not match resource project \"res-proj\""
        );
        let mut cfg = vertex(&[]);
        cfg.location = Some("global".to_string());
        assert_eq!(
            new_err(RESOURCE, cfg).await.to_string(),
            "anthropicmodel: invalid configuration: ClientConfig.location \"global\" does not match resource location \"us-east5\""
        );
        assert_eq!(
            new_err(MODEL, vertex(&[("GOOGLE_CLOUD_LOCATION", "us-east5")]))
                .await
                .to_string(),
            "anthropicmodel: invalid configuration: Backend::Vertex requires a project via ClientConfig.project, a full resource model name, GOOGLE_CLOUD_PROJECT, or CLOUD_ML_PROJECT_ID"
        );
        assert_eq!(
            new_err(MODEL, vertex(&[("GOOGLE_CLOUD_PROJECT", "p")]))
                .await
                .to_string(),
            "anthropicmodel: invalid configuration: Backend::Vertex requires a location via ClientConfig.location, a full resource model name, GOOGLE_CLOUD_LOCATION, or CLOUD_ML_REGION"
        );
        assert_eq!(
            new_err(
                MODEL,
                vertex(&[
                    ("GOOGLE_CLOUD_PROJECT", "-bad"),
                    ("GOOGLE_CLOUD_LOCATION", "us-east5")
                ])
            )
            .await
            .to_string(),
            "anthropicmodel: invalid configuration: invalid Vertex project \"-bad\""
        );
        assert_eq!(
            new_err(
                MODEL,
                vertex(&[
                    ("GOOGLE_CLOUD_PROJECT", "p"),
                    ("GOOGLE_CLOUD_LOCATION", "US")
                ])
            )
            .await
            .to_string(),
            "anthropicmodel: invalid configuration: invalid Vertex location \"US\"; expected a lowercase region such as \"us-east5\" or \"global\""
        );

        // A custom base URL needs explicit credentials (checked before ADC).
        let mut cfg = vertex(&[]);
        cfg.credentials = None;
        cfg.base_url = Some("https://proxy.example".to_string());
        assert_eq!(
            new_err(RESOURCE, cfg).await.to_string(),
            "anthropicmodel: invalid configuration: custom base_url on Backend::Vertex requires explicit ClientConfig.credentials; Application Default Credentials are not sent to custom endpoints"
        );
        let mut cfg = vertex(&[]);
        cfg.base_url = Some(" https://proxy.example/prefix ".to_string());
        let m = AnthropicModel::new_concrete(RESOURCE, cfg).await.unwrap();
        assert_eq!(m.transport.base_url, "https://proxy.example/prefix");
    }

    #[tokio::test]
    async fn new_vertex_builds_adc_from_env_file() {
        let file = TempFile::new(
            "adc.json",
            r#"{"type":"authorized_user","client_id":"c","client_secret":"s","refresh_token":"r"}"#,
        );
        let mut cfg = vertex(&[("GOOGLE_APPLICATION_CREDENTIALS", &file.path())]);
        cfg.credentials = None;
        let m = AnthropicModel::new_concrete(RESOURCE, cfg).await.unwrap();
        assert!(matches!(m.transport.auth, Auth::Vertex(_)));
    }

    #[tokio::test]
    async fn adc_failures_are_gos_text() {
        let want = "anthropicmodel: invalid configuration: anthropicmodel: authentication failed: failed to find Google Application Default Credentials for Vertex AI";
        let missing = std::env::temp_dir()
            .join(format!(
                "gadk-anthropic-missing-{}.json",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let not_json = TempFile::new("bad.json", "not json");
        let unknown = TempFile::new("unknown.json", r#"{"type":"mystery"}"#);
        for path in [missing, not_json.path(), unknown.path()] {
            let err =
                match adc_credentials(env(&[("GOOGLE_APPLICATION_CREDENTIALS", &path)]).as_ref()) {
                    Ok(_) => panic!("adc_credentials({path}) succeeded"),
                    Err(e) => e,
                };
            assert_eq!(err.to_string(), want, "{path}");
            assert!(err.is(ErrorKind::InvalidConfig) && err.is(ErrorKind::Authentication));

            let mut cfg = vertex(&[("GOOGLE_APPLICATION_CREDENTIALS", &path)]);
            cfg.credentials = None;
            assert_eq!(new_err(RESOURCE, cfg).await.to_string(), want);
        }
    }

    #[tokio::test]
    async fn resolve_vertex_credentials_prefers_explicit() {
        let mut cfg = vertex(&[("GOOGLE_APPLICATION_CREDENTIALS", "/nonexistent/adc.json")]);
        assert!(resolve_vertex_credentials(&cfg).is_ok());
        cfg.credentials = None;
        assert!(resolve_vertex_credentials(&cfg).is_err());
    }

    // -------------------------------------------------------------------- resolve_model_id

    #[tokio::test]
    async fn resolve_model_id_cases() {
        let m = AnthropicModel::new_concrete(RESOURCE, vertex(&[]))
            .await
            .unwrap();
        assert_eq!(m.resolve_model_id("  ").unwrap(), "claude-sonnet-4-5");
        assert_eq!(
            m.resolve_model_id(" anthropic/claude-x ").unwrap(),
            "claude-x"
        );
        assert_eq!(
            m.resolve_model_id(
                "projects/res-proj/locations/us-east5/publishers/anthropic/models/claude-y"
            )
            .unwrap(),
            "claude-y"
        );
        assert_eq!(
            m.resolve_model_id(
                "projects/other/locations/us-east5/publishers/anthropic/models/claude-y"
            )
            .unwrap_err()
            .to_string(),
            "anthropicmodel: invalid configuration: request model project \"other\" does not match configured project \"res-proj\""
        );
        assert_eq!(
            m.resolve_model_id(
                "projects/res-proj/locations/global/publishers/anthropic/models/claude-y"
            )
            .unwrap_err()
            .to_string(),
            "anthropicmodel: invalid configuration: request model location \"global\" does not match configured location \"us-east5\""
        );
        assert_eq!(
            m.resolve_model_id("a/b/c").unwrap_err().to_string(),
            "anthropicmodel: invalid configuration: invalid model name"
        );

        let m = AnthropicModel::new_concrete(MODEL, anthropic(Some("k"), &[]))
            .await
            .unwrap();
        assert_eq!(
            m.resolve_model_id(RESOURCE).unwrap_err().to_string(),
            "anthropicmodel: invalid configuration: Vertex resource model name requires Backend::Vertex"
        );
    }

    // ---------------------------------------------------------------- build_message_params

    async fn anthropic_model(effort: Option<Effort>) -> AnthropicModel {
        let mut cfg = anthropic(Some("k"), &[]);
        cfg.effort = effort;
        AnthropicModel::new_concrete(MODEL, cfg).await.unwrap()
    }

    fn request(config: Option<GenerateContentConfig>) -> LlmRequest {
        LlmRequest {
            contents: gadk_genai::text("hi"),
            config,
            ..Default::default()
        }
    }

    /// Temperature and top-k (Go rejects temperature with top-p on Claude).
    fn sampling() -> GenerateContentConfig {
        GenerateContentConfig {
            temperature: Some(0.5),
            top_k: Some(2.9),
            stop_sequences: vec!["STOP".to_string()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn build_message_params_defaults_and_sampling() {
        let m = anthropic_model(None).await;
        let built = m.build_message_params(&request(None)).unwrap();
        assert_eq!(built.model_id, MODEL);
        assert_eq!(built.params.model, MODEL);
        assert_eq!(built.params.max_tokens, u64::from(DEFAULT_MAX_TOKENS));
        assert_eq!(built.params.messages.len(), 1);
        assert_eq!(built.timeout, None);
        assert!(!built.unwrap_output);
        assert_eq!(built.params.temperature, None);

        let mut cfg = sampling();
        cfg.max_output_tokens = 77;
        cfg.system_instruction = Some(Content::from_text("sys", "user"));
        let built = m.build_message_params(&request(Some(cfg))).unwrap();
        assert_eq!(built.params.max_tokens, 77);
        assert_eq!(built.params.temperature, Some(0.5));
        assert_eq!(built.params.top_p, None);
        assert_eq!(built.params.top_k, Some(2));
        assert_eq!(built.params.stop_sequences, vec!["STOP".to_string()]);
        assert_eq!(built.params.system.len(), 1);
        assert_eq!(
            String::from_utf8(
                m.transport
                    .encode_body(built.params.clone(), false)
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            r#"{"max_tokens":77,"messages":[{"content":[{"text":"hi","type":"text"}],"role":"user"}],"model":"claude-sonnet-4-5-20250929","temperature":0.5,"top_k":2,"stop_sequences":["STOP"],"system":[{"text":"sys","type":"text"}]}"#
        );

        let built = m
            .build_message_params(&request(Some(GenerateContentConfig {
                top_p: Some(0.25),
                ..Default::default()
            })))
            .unwrap();
        assert_eq!(
            (built.params.temperature, built.params.top_p),
            (None, Some(0.25))
        );
    }

    #[tokio::test]
    async fn build_message_params_omits_sampling_with_effort_or_thinking() {
        let m = anthropic_model(Some(Effort::Low)).await;
        let mut cfg = sampling();
        cfg.temperature = None;
        cfg.top_p = Some(0.25);
        let built = m.build_message_params(&request(Some(cfg))).unwrap();
        assert_eq!(
            (
                built.params.temperature,
                built.params.top_p,
                built.params.top_k
            ),
            (None, None, None)
        );
        // Stop sequences are kept.
        assert_eq!(built.params.stop_sequences, vec!["STOP".to_string()]);
        assert_eq!(
            built.params.output_config.as_ref().and_then(|o| o.effort),
            Some(Effort::Low)
        );

        let m = anthropic_model(None).await;
        let mut cfg = sampling();
        cfg.thinking_config = Some(ThinkingConfig {
            thinking_budget: Some(2048),
            ..Default::default()
        });
        let built = m.build_message_params(&request(Some(cfg))).unwrap();
        assert_eq!(built.params.temperature, None);
        assert_eq!(
            built.params.thinking,
            Some(crate::wire::ThinkingConfigParam::Enabled {
                budget_tokens: 2048
            })
        );

        // A zero budget is "thinking disabled": sampling is sent.
        let mut cfg = sampling();
        cfg.thinking_config = Some(ThinkingConfig {
            thinking_budget: Some(0),
            ..Default::default()
        });
        let built = m.build_message_params(&request(Some(cfg))).unwrap();
        assert_eq!(built.params.temperature, Some(0.5));
        assert_eq!(
            built.params.thinking,
            Some(crate::wire::ThinkingConfigParam::Disabled)
        );
    }

    #[tokio::test]
    async fn build_message_params_timeout_validation_and_errors() {
        let m = anthropic_model(None).await;
        let mut req = request(None);
        req.http_options = Some(HttpOptions {
            timeout: Some(Duration::from_secs(3)),
            ..Default::default()
        });
        assert_eq!(
            m.build_message_params(&req).unwrap().timeout,
            Some(Duration::from_secs(3))
        );
        req.http_options = Some(HttpOptions {
            timeout: Some(Duration::ZERO),
            ..Default::default()
        });
        assert_eq!(m.build_message_params(&req).unwrap().timeout, None);

        req.http_options = Some(HttpOptions {
            base_url: Some("https://x".to_string()),
            ..Default::default()
        });
        assert_eq!(
            m.build_message_params(&req).unwrap_err().to_string(),
            "anthropicmodel: invalid configuration: http_options.base_url is not supported; set ClientConfig.base_url instead"
        );

        let empty = LlmRequest::default();
        let err = m.build_message_params(&empty).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request contains no sendable content: contents must not be empty"
        );

        // The request model is resolved first.
        let mut req = request(None);
        req.model = RESOURCE.to_string();
        assert_eq!(
            m.build_message_params(&req).unwrap_err().to_string(),
            "anthropicmodel: invalid configuration: Vertex resource model name requires Backend::Vertex"
        );
    }

    #[tokio::test]
    async fn build_message_params_applies_prompt_cache() {
        let mut cfg = anthropic(Some("k"), &[]);
        cfg.prompt_cache = Some(PromptCacheConfig::default());
        let m = AnthropicModel::new_concrete(MODEL, cfg).await.unwrap();
        let req = request(Some(GenerateContentConfig {
            system_instruction: Some(Content::from_text("sys", "user")),
            ..Default::default()
        }));
        let built = m.build_message_params(&req).unwrap();
        let body = String::from_utf8(
            m.transport
                .encode_body(built.params.clone(), false)
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(
            body,
            r#"{"max_tokens":8192,"messages":[{"content":[{"text":"hi","cache_control":{"ttl":"5m","type":"ephemeral"},"type":"text"}],"role":"user"}],"model":"claude-sonnet-4-5-20250929","system":[{"text":"sys","cache_control":{"ttl":"5m","type":"ephemeral"},"type":"text"}]}"#
        );
    }

    // ------------------------------------------------------------------- generate_content

    /// An `HttpClient` answering every request with one canned response; records requests.
    struct Canned {
        status: u16,
        content_type: &'static str,
        body: &'static str,
        seen: Mutex<Vec<(String, HeaderMap, Bytes)>>,
    }

    impl Canned {
        fn new(status: u16, content_type: &'static str, body: &'static str) -> Arc<Self> {
            Arc::new(Self {
                status,
                content_type,
                body,
                seen: Mutex::new(Vec::new()),
            })
        }

        fn seen(&self) -> Vec<(String, HeaderMap, Bytes)> {
            self.seen.lock().unwrap().clone()
        }
    }

    // `HttpClient` is an `async_trait`; gadk-anthropic does not depend on `async-trait`, so the
    // fake spells out the method the macro generates.
    impl HttpClient for Canned {
        fn send<'life0, 'async_trait>(
            &'life0 self,
            req: http::Request<Bytes>,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<http::Response<BodyStream>, ModelError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let (parts, body) = req.into_parts();
                self.seen
                    .lock()
                    .unwrap()
                    .push((parts.uri.to_string(), parts.headers, body));
                let body: BodyStream =
                    futures::stream::iter([Ok(Bytes::from_static(self.body.as_bytes()))]).boxed();
                let mut res = http::Response::new(body);
                *res.status_mut() = http::StatusCode::from_u16(self.status).unwrap();
                res.headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static(self.content_type));
                res.headers_mut()
                    .insert("request-id", HeaderValue::from_static("req_1"));
                Ok(res)
            })
        }
    }

    const MESSAGE_JSON: &str = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"Paris"}],"usage":{"input_tokens":3,"output_tokens":1}}"#;

    const SSE_BODY: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5-20250929\",\"content\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Par\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"is\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    async fn model_with(client: Arc<Canned>, mut cfg: ClientConfig) -> AnthropicModel {
        cfg.http_client = Some(client);
        cfg.max_retries = Some(0);
        AnthropicModel::new_concrete(MODEL, cfg).await.unwrap()
    }

    #[tokio::test]
    async fn generate_content_non_streaming_anthropic() {
        let client = Canned::new(200, "application/json", MESSAGE_JSON);
        let m = model_with(client.clone(), anthropic(Some("key"), &[])).await;
        let req = request(Some(GenerateContentConfig {
            temperature: Some(0.0),
            ..Default::default()
        }));
        let before = format!("{req:?}");
        let out: Vec<_> = m.generate_content(req.clone(), false).collect().await;
        assert_eq!(format!("{req:?}"), before);
        assert_eq!(out.len(), 1);
        let resp = out.into_iter().next().unwrap().unwrap();
        assert!(resp.turn_complete && !resp.partial);
        assert_eq!(resp.content.unwrap().parts[0].text, "Paris");

        let seen = client.seen();
        assert_eq!(seen.len(), 1);
        let (url, headers, body) = &seen[0];
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert_eq!(headers.get("x-api-key").unwrap(), "key");
        assert_eq!(
            std::str::from_utf8(body).unwrap(),
            r#"{"max_tokens":8192,"messages":[{"content":[{"text":"hi","type":"text"}],"role":"user"}],"model":"claude-sonnet-4-5-20250929","temperature":0}"#
        );
    }

    #[tokio::test]
    async fn generate_content_non_streaming_vertex() {
        let client = Canned::new(200, "application/json", MESSAGE_JSON);
        let m = model_with(
            client.clone(),
            vertex(&[
                ("GOOGLE_CLOUD_PROJECT", "p1"),
                ("GOOGLE_CLOUD_LOCATION", "us-east5"),
            ]),
        )
        .await;
        let out: Vec<_> = m.generate_content(request(None), false).collect().await;
        assert!(out[0].is_ok(), "{:?}", out[0]);
        let (url, headers, body) = &client.seen()[0];
        assert_eq!(
            url,
            "https://us-east5-aiplatform.googleapis.com/v1/projects/p1/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929:rawPredict"
        );
        assert_eq!(headers.get("authorization").unwrap(), "Bearer vertex-token");
        assert_eq!(
            std::str::from_utf8(body).unwrap(),
            r#"{"max_tokens":8192,"messages":[{"content":[{"text":"hi","type":"text"}],"role":"user"}],"anthropic_version":"vertex-2023-10-16"}"#
        );
    }

    #[tokio::test]
    async fn generate_content_streaming_yields_partials_then_final() {
        let client = Canned::new(200, "text/event-stream", SSE_BODY);
        let m = model_with(client.clone(), anthropic(Some("key"), &[])).await;
        let out: Vec<LlmResponse> = m
            .generate_content(request(None), true)
            .map(Result::unwrap)
            .collect()
            .await;
        let partials: Vec<&str> = out
            .iter()
            .filter(|r| r.partial)
            .map(|r| r.content.as_ref().unwrap().parts[0].text.as_str())
            .collect();
        assert_eq!(partials, vec!["Par", "is"]);
        let last = out.last().unwrap();
        assert!(!last.partial && last.turn_complete);
        assert_eq!(last.content.as_ref().unwrap().parts[0].text, "Paris");
        let (_, _, body) = &client.seen()[0];
        assert!(
            std::str::from_utf8(body)
                .unwrap()
                .ends_with(r#","stream":true}"#)
        );
    }

    #[tokio::test]
    async fn generate_content_errors_are_anthropic_errors() {
        let client = Canned::new(
            400,
            "application/json",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#,
        );
        let m = model_with(client.clone(), anthropic(Some("key"), &[])).await;
        for stream in [false, true] {
            let out: Vec<_> = m.generate_content(request(None), stream).collect().await;
            assert_eq!(out.len(), 1);
            let Err(ModelError::Other(err)) = &out[0] else {
                panic!("want ModelError::Other, got {:?}", out[0]);
            };
            let err = err.downcast_ref::<AnthropicError>().unwrap();
            assert!(err.is(ErrorKind::BadRequest), "{err}");
        }

        // A params error is yielded without sending.
        let out: Vec<_> = m
            .generate_content(LlmRequest::default(), false)
            .collect()
            .await;
        assert_eq!(
            out[0].as_ref().unwrap_err().to_string(),
            "anthropicmodel: request contains no sendable content: contents must not be empty"
        );
        assert_eq!(client.seen().len(), 2);
    }

    #[tokio::test]
    async fn generate_content_sends_nothing_before_first_poll() {
        let client = Canned::new(200, "application/json", MESSAGE_JSON);
        let m = model_with(client.clone(), anthropic(Some("key"), &[])).await;
        drop(m.generate_content(request(None), false));
        drop(m.generate_content(request(None), true));
        assert!(client.seen().is_empty());
        assert_eq!(m.google_variant(), None);
    }
}
