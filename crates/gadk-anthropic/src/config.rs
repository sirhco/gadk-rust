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

//! Client configuration: [`ClientConfig`], its enums, model-name parsing and the
//! construction-time validators.
//!
//! The string enums are Rust enums whose [`FromStr`] accepts the documented values and fails
//! with an invalid-value text; negative `max_tokens` / `max_retries` are unrepresentable.

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};

use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;
use regex::Regex;
use serde::{Serialize, Serializer};

use crate::error::{AnthropicError, ErrorKind, go_quote};

/// The `max_tokens` sent when neither `GenerateContentConfig.max_output_tokens` nor
/// [`ClientConfig::max_tokens`] is set.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// The serving backend for Claude models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Backend {
    /// The first-party Anthropic Messages API (`"anthropic"`).
    #[default]
    Anthropic,
    /// Claude on Google Cloud Vertex AI (`"vertex"`).
    Vertex,
}

impl Backend {
    /// The string value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Vertex => "vertex",
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Accepts the string values; `""` is [`Backend::Anthropic`].
impl FromStr for Backend {
    type Err = AnthropicError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "anthropic" => Ok(Self::Anthropic),
            "vertex" => Ok(Self::Vertex),
            other => Err(AnthropicError::new(
                ErrorKind::InvalidConfig,
                format!(
                    "unsupported Backend {}; valid values are \"anthropic\", \"vertex\"",
                    go_quote(other)
                ),
            )),
        }
    }
}

/// How much reasoning effort Claude spends (sent as `output_config.effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effort {
    /// `"low"`.
    Low,
    /// `"medium"`.
    Medium,
    /// `"high"`.
    High,
    /// `"xhigh"`.
    XHigh,
    /// `"max"`.
    Max,
}

impl Effort {
    /// The string value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Effort {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Accepts the string values and fails with an invalid-effort text. An unset effort
/// is `None` in [`ClientConfig::effort`], so `""` is rejected here.
impl FromStr for Effort {
    type Err = AnthropicError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(AnthropicError::new(
                ErrorKind::InvalidConfig,
                format!(
                    "unsupported Effort {}; valid values are low, medium, high, xhigh, max",
                    go_quote(other)
                ),
            )),
        }
    }
}

/// The lifetime of an ephemeral prompt-cache breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PromptCacheTtl {
    /// Five minutes (`"5m"`), the default.
    #[default]
    FiveMinutes,
    /// One hour (`"1h"`).
    OneHour,
}

impl PromptCacheTtl {
    /// The string value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
        }
    }
}

impl fmt::Display for PromptCacheTtl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for PromptCacheTtl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Accepts the string values and fails with an invalid-TTL text; `""` is
/// [`PromptCacheTtl::FiveMinutes`].
impl FromStr for PromptCacheTtl {
    type Err = AnthropicError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "5m" => Ok(Self::FiveMinutes),
            "1h" => Ok(Self::OneHour),
            other => Err(AnthropicError::new(
                ErrorKind::InvalidConfig,
                format!(
                    "unsupported PromptCache.TTL {}; valid values are 5m, 1h",
                    go_quote(other)
                ),
            )),
        }
    }
}

/// Ephemeral prompt caching. When set on
/// [`ClientConfig::prompt_cache`], cache breakpoints go on the last tool declaration, the
/// system block and the last non-thinking content block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PromptCacheConfig {
    /// The cache lifetime.
    pub ttl: PromptCacheTtl,
}

/// Configuration for [`AnthropicModel::new`](crate::AnthropicModel::new).
///
/// There is no token-source, logger or request-options field: use a custom
/// `google_cloud_auth` credentials provider in [`credentials`](Self::credentials), warnings go
/// through `tracing`, and use [`base_url`](Self::base_url), [`headers`](Self::headers) and
/// [`http_client`](Self::http_client) for request options.
#[derive(Clone)]
pub struct ClientConfig {
    /// The backend (default [`Backend::Anthropic`]).
    pub backend: Backend,
    /// Anthropic API key, sent as `x-api-key`. When unset and no custom base URL is set,
    /// `ANTHROPIC_API_KEY` is read.
    pub api_key: Option<String>,
    /// Anthropic bearer token, sent as `Authorization: Bearer`. When neither it nor
    /// `api_key` is set and no custom base URL is set, `ANTHROPIC_AUTH_TOKEN` is read.
    pub auth_token: Option<String>,
    /// Overrides the API endpoint. When unset on the Anthropic backend, `ANTHROPIC_BASE_URL`
    /// is read. When set, an explicit credential is required (`api_key`/`auth_token` on
    /// Anthropic, `credentials` on Vertex): ambient credentials are never sent to it.
    pub base_url: Option<String>,
    /// Google Cloud project (Vertex). When unset: the resource name's project,
    /// `GOOGLE_CLOUD_PROJECT`, then `CLOUD_ML_PROJECT_ID`.
    pub project: Option<String>,
    /// Google Cloud region such as `us-east5` or `global` (Vertex). When unset: the resource
    /// name's location, `GOOGLE_CLOUD_LOCATION`, then `CLOUD_ML_REGION`.
    pub location: Option<String>,
    /// Explicit Google Cloud credentials (Vertex). Without them, Application Default
    /// Credentials are used.
    pub credentials: Option<Credentials>,
    /// Custom HTTP client. On Vertex, requests still carry the credentials' headers.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Headers added to every request, after the ones gadk sets.
    pub headers: http::HeaderMap,
    /// Default `max_tokens` when `GenerateContentConfig.max_output_tokens` is 0; `None` or
    /// `Some(0)` means [`DEFAULT_MAX_TOKENS`].
    pub max_tokens: Option<u32>,
    /// Retry count (default 2).
    pub max_retries: Option<u32>,
    /// `output_config.effort`.
    pub effort: Option<Effort>,
    /// Enables ephemeral prompt caching.
    pub prompt_cache: Option<PromptCacheConfig>,
    /// Environment variable source (`ANTHROPIC_*`, `GOOGLE_*`, and
    /// `GOOGLE_APPLICATION_CREDENTIALS` for ADC). Without `GOOGLE_APPLICATION_CREDENTIALS` in it,
    /// ADC uses google-cloud-auth's default builder, which reads the process environment, the
    /// gcloud configuration files and the metadata server regardless of this source.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            backend: Backend::Anthropic,
            api_key: None,
            auth_token: None,
            base_url: None,
            project: None,
            location: None,
            credentials: None,
            http_client: None,
            headers: http::HeaderMap::new(),
            max_tokens: None,
            max_retries: None,
            effort: None,
            prompt_cache: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

/// Secrets (`api_key`, `auth_token`, header values, credentials in `base_url`) are redacted.
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("backend", &self.backend)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "base_url",
                &self.base_url.as_deref().map(gadk_model::redact_url),
            )
            .field("project", &self.project)
            .field("location", &self.location)
            .field("credentials", &self.credentials.is_some())
            .field("http_client", &self.http_client.is_some())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("max_tokens", &self.max_tokens)
            .field("max_retries", &self.max_retries)
            .field("effort", &self.effort)
            .field("prompt_cache", &self.prompt_cache)
            .finish_non_exhaustive()
    }
}

#[allow(clippy::expect_used)] // constant pattern, cannot fail
static VERTEX_RESOURCE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^projects/([^/]+)/locations/([^/]+)/publishers/anthropic/models/([^/:]+)$")
        .expect("valid resource-name regex")
});
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static VALID_VERTEX_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z]+(-[a-z0-9]+)*$").expect("valid location regex"));
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static VALID_VERTEX_PROJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._:-]*$").expect("valid project regex"));

/// Validates a Vertex project ID.
pub(crate) fn validate_vertex_project(project: &str) -> Result<(), AnthropicError> {
    if VALID_VERTEX_PROJECT.is_match(project) {
        return Ok(());
    }
    Err(AnthropicError::new(
        ErrorKind::InvalidConfig,
        format!("invalid Vertex project {}", go_quote(project)),
    ))
}

/// Validates a Vertex location.
pub(crate) fn validate_vertex_location(location: &str) -> Result<(), AnthropicError> {
    if VALID_VERTEX_LOCATION.is_match(location) {
        return Ok(());
    }
    Err(AnthropicError::new(
        ErrorKind::InvalidConfig,
        format!(
            "invalid Vertex location {}; expected a lowercase region such as \"us-east5\" or \"global\"",
            go_quote(location)
        ),
    ))
}

/// The result of [`parse_model_name`]: project, location, model ID and whether the name is a
/// resource name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ParsedModelName {
    /// The resource name's project; empty for a plain model ID.
    pub(crate) project: String,
    /// The resource name's location; empty for a plain model ID.
    pub(crate) location: String,
    /// The Claude model ID.
    pub(crate) model_id: String,
    /// Whether the name was a full Vertex resource name.
    pub(crate) is_resource: bool,
}

/// Parses a model name: `claude-*`, `anthropic/claude-*`, or
/// `projects/<project>/locations/<location>/publishers/anthropic/models/<model>`.
pub(crate) fn parse_model_name(raw: &str) -> Result<ParsedModelName, AnthropicError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(AnthropicError::new(
            ErrorKind::InvalidConfig,
            "model name must not be empty",
        ));
    }
    if name.starts_with("projects/") {
        let Some(caps) = VERTEX_RESOURCE_NAME.captures(name) else {
            return Err(AnthropicError::new(
                ErrorKind::InvalidConfig,
                "invalid Vertex model resource name format; expected projects/<project>/locations/<location>/publishers/anthropic/models/<model>",
            ));
        };
        let group = |i: usize| {
            caps.get(i)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        };
        let (project, location, model_id) = (group(1), group(2), group(3));
        validate_vertex_project(&project)?;
        validate_vertex_location(&location)?;
        return Ok(ParsedModelName {
            project,
            location,
            model_id,
            is_resource: true,
        });
    }
    let model_id = name.strip_prefix("anthropic/").unwrap_or(name);
    if model_id.is_empty() || model_id.contains('/') {
        return Err(AnthropicError::new(
            ErrorKind::InvalidConfig,
            "invalid model name",
        ));
    }
    Ok(ParsedModelName {
        model_id: model_id.to_string(),
        ..ParsedModelName::default()
    })
}

/// Validates a base URL: `source` names the setting in the error (`BaseURL`,
/// `ANTHROPIC_BASE_URL`). The URL must parse with an `http`/`https` scheme and a host; an ASCII
/// control character fails (`invalid control character in URL`).
pub(crate) fn validate_base_url(source: &str, raw: &str) -> Result<(), AnthropicError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AnthropicError::new(
            ErrorKind::InvalidConfig,
            format!("{source} must not be whitespace-only"),
        ));
    }
    let has_ctl = trimmed.bytes().any(|b| b < 0x20 || b == 0x7f);
    let ok = !has_ctl
        && url::Url::parse(trimmed).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
        });
    if ok && go_authority_has_host(trimmed) {
        return Ok(());
    }
    Err(AnthropicError::new(
        ErrorKind::InvalidConfig,
        format!("{source} must be a valid http or https URL with a host"),
    ))
}

/// The host is the text between `scheme://` and the next `/`, `?` or
/// `#`, after any `userinfo@`; the WHATWG parser behind `url::Url` skips extra slashes
/// (`https:///x` has host `x` there, none here), so the host is also checked this way.
fn go_authority_has_host(s: &str) -> bool {
    let Some((_, rest)) = s.split_once(':') else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("//") else {
        return false;
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    !host.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn default_max_tokens_is_8192() {
        assert_eq!(DEFAULT_MAX_TOKENS, 8192);
    }

    #[test]
    fn backend_from_str_accepts_gos_values_and_rejects_others_with_gos_text() {
        assert_eq!(
            "anthropic".parse::<Backend>().ok(),
            Some(Backend::Anthropic)
        );
        assert_eq!("".parse::<Backend>().ok(), Some(Backend::Anthropic));
        assert_eq!("vertex".parse::<Backend>().ok(), Some(Backend::Vertex));
        let err = "bedrock"
            .parse::<Backend>()
            .err()
            .map(|e| (e.to_string(), e.is(ErrorKind::InvalidConfig)));
        assert_eq!(
            err,
            Some((
                "anthropicmodel: invalid configuration: unsupported Backend \"bedrock\"; valid values are \"anthropic\", \"vertex\"".to_string(),
                true
            ))
        );
        assert_eq!(Backend::default(), Backend::Anthropic);
        assert_eq!(Backend::Vertex.to_string(), "vertex");
    }

    #[test]
    fn effort_from_str_accepts_every_level_and_rejects_others_with_gos_text() {
        for (s, e) in [
            ("low", Effort::Low),
            ("medium", Effort::Medium),
            ("high", Effort::High),
            ("xhigh", Effort::XHigh),
            ("max", Effort::Max),
        ] {
            assert_eq!(s.parse::<Effort>().ok(), Some(e));
            assert_eq!(e.as_str(), s);
            assert_eq!(serde_json::to_string(&e).ok(), Some(format!("\"{s}\"")));
        }
        assert_eq!(
            "extreme".parse::<Effort>().err().map(|e| e.to_string()),
            Some("anthropicmodel: invalid configuration: unsupported Effort \"extreme\"; valid values are low, medium, high, xhigh, max".to_string())
        );
        assert!("".parse::<Effort>().is_err());
    }

    #[test]
    fn prompt_cache_ttl_from_str_like_go() {
        assert_eq!(
            "5m".parse::<PromptCacheTtl>().ok(),
            Some(PromptCacheTtl::FiveMinutes)
        );
        assert_eq!(
            "".parse::<PromptCacheTtl>().ok(),
            Some(PromptCacheTtl::FiveMinutes)
        );
        assert_eq!(
            "1h".parse::<PromptCacheTtl>().ok(),
            Some(PromptCacheTtl::OneHour)
        );
        assert_eq!(
            "10m".parse::<PromptCacheTtl>().err().map(|e| e.to_string()),
            Some("anthropicmodel: invalid configuration: unsupported PromptCache.TTL \"10m\"; valid values are 5m, 1h".to_string())
        );
        assert_eq!(
            PromptCacheConfig::default().ttl,
            PromptCacheTtl::FiveMinutes
        );
        assert_eq!(
            serde_json::to_string(&PromptCacheTtl::OneHour).ok(),
            Some("\"1h\"".to_string())
        );
    }

    #[test]
    fn client_config_debug_redacts_secrets() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", http::HeaderValue::from_static("hdr-secret"));
        let cfg = ClientConfig {
            api_key: Some("sk-ant-secret".into()),
            auth_token: Some("tok-secret".into()),
            base_url: Some("https://user:url-secret@proxy.example/v1".into()),
            headers,
            ..ClientConfig::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("url-secret"), "{dbg}");
        assert!(!dbg.contains("user"), "{dbg}");
        assert!(dbg.contains("proxy.example/v1"), "{dbg}");
        assert!(!dbg.contains("sk-ant-secret"), "{dbg}");
        assert!(!dbg.contains("tok-secret"), "{dbg}");
        assert!(!dbg.contains("hdr-secret"), "{dbg}");
        assert!(dbg.contains("x-custom"), "{dbg}");
    }

    #[test]
    fn parse_model_name_accepts_ids_prefixed_ids_and_resource_names() {
        let p = parse_model_name("  claude-sonnet-4-5-20250929 ").ok();
        assert_eq!(
            p,
            Some(ParsedModelName {
                model_id: "claude-sonnet-4-5-20250929".into(),
                ..ParsedModelName::default()
            })
        );
        let p = parse_model_name("anthropic/claude-opus-4").ok();
        assert_eq!(p.map(|p| p.model_id), Some("claude-opus-4".to_string()));
        let p = parse_model_name(
            "projects/my-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5@20250929",
        )
        .ok();
        assert_eq!(
            p,
            Some(ParsedModelName {
                project: "my-proj".into(),
                location: "us-east5".into(),
                model_id: "claude-sonnet-4-5@20250929".into(),
                is_resource: true,
            })
        );
    }

    #[test]
    fn parse_model_name_rejections_use_gos_text() {
        let cases = [
            (
                "   ",
                "anthropicmodel: invalid configuration: model name must not be empty",
            ),
            (
                "foo/bar/baz",
                "anthropicmodel: invalid configuration: invalid model name",
            ),
            (
                "anthropic/",
                "anthropicmodel: invalid configuration: invalid model name",
            ),
            (
                "projects/p/locations/l/models/m",
                "anthropicmodel: invalid configuration: invalid Vertex model resource name format; expected projects/<project>/locations/<location>/publishers/anthropic/models/<model>",
            ),
            (
                "projects/p/locations/l/publishers/anthropic/models/m:rawPredict",
                "anthropicmodel: invalid configuration: invalid Vertex model resource name format; expected projects/<project>/locations/<location>/publishers/anthropic/models/<model>",
            ),
            (
                "projects/-bad/locations/us-east5/publishers/anthropic/models/m",
                "anthropicmodel: invalid configuration: invalid Vertex project \"-bad\"",
            ),
            (
                "projects/p/locations/US-East5/publishers/anthropic/models/m",
                "anthropicmodel: invalid configuration: invalid Vertex location \"US-East5\"; expected a lowercase region such as \"us-east5\" or \"global\"",
            ),
        ];
        for (name, want) in cases {
            let got = parse_model_name(name).err();
            assert_eq!(
                got.as_ref().map(|e| e.to_string()),
                Some(want.to_string()),
                "{name:?}"
            );
            assert!(got.is_some_and(|e| e.is(ErrorKind::InvalidConfig)));
        }
    }

    #[test]
    fn vertex_project_and_location_validators() {
        for ok in ["p", "my-proj", "a.b:c_d-e", "0abc"] {
            assert!(validate_vertex_project(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-p", "p/q", "p q"] {
            assert!(validate_vertex_project(bad).is_err(), "{bad}");
        }
        for ok in ["global", "us", "eu", "us-east5", "europe-west1"] {
            assert!(validate_vertex_location(ok).is_ok(), "{ok}");
        }
        for bad in ["", "us_east5", "US", "us-", "-us", "us--east"] {
            assert!(validate_vertex_location(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validate_base_url_like_go() {
        assert!(validate_base_url("BaseURL", "https://example.com").is_ok());
        assert!(validate_base_url("BaseURL", " http://127.0.0.1:8080/prefix ").is_ok());
        assert_eq!(
            validate_base_url("BaseURL", "   ")
                .err()
                .map(|e| e.to_string()),
            Some(
                "anthropicmodel: invalid configuration: BaseURL must not be whitespace-only"
                    .to_string()
            )
        );
        for bad in [
            "ftp://example.com",
            "example.com",
            "localhost:8080",
            "http://",
            "https:///path",
            // Go url.Parse: `invalid control character in URL` (the WHATWG parser strips tabs
            // and newlines and would accept these).
            "https://exa\tmple.com",
            "https://example.com/a\nb",
            "https://example.com/\u{7f}",
            "https://example.com/\u{1}",
        ] {
            assert_eq!(
                validate_base_url("ANTHROPIC_BASE_URL", bad).err().map(|e| e.to_string()),
                Some("anthropicmodel: invalid configuration: ANTHROPIC_BASE_URL must be a valid http or https URL with a host".to_string()),
                "{bad}"
            );
        }
    }
}
