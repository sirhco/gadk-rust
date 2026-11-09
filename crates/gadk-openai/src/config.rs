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

//! Client configuration (port of ADK-Go `model/openaimodel/openaimodel.go`'s `ClientConfig`
//! and `API`, plus openai-go v3.82.0's `DefaultClientOptions` environment defaults).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::go_quote;
use http::HeaderMap;

use crate::error::{ErrorKind, OpenAiError};

/// The base URL a client uses when neither the configuration nor `OPENAI_BASE_URL` names one
/// (openai-go `WithEnvironmentProduction`).
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/";

/// The OpenAI HTTP API a model talks to (Go `API`). The default is [`Api::Responses`], as Go's
/// zero value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Api {
    /// OpenAI's Responses API, `POST {base}responses` (Go `APIResponses`, `"responses"`).
    #[default]
    Responses,
    /// OpenAI's Chat Completions API, `POST {base}chat/completions` (Go `APIChatCompletions`,
    /// `"chat_completions"`), the surface most OpenAI-compatible providers implement.
    ChatCompletions,
}

impl Api {
    /// Go's string value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
        }
    }
}

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Accepts Go's values; `""` is [`Api::Responses`], as Go's zero `API` is. Anything else fails
/// as Go's `NewModel` does: `fmt.Errorf("%w: %q", ErrUnsupportedAPI, cfg.API)`.
impl FromStr for Api {
    type Err = OpenAiError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "responses" => Ok(Self::Responses),
            "chat_completions" => Ok(Self::ChatCompletions),
            other => Err(OpenAiError::new(ErrorKind::UnsupportedApi, go_quote(other))),
        }
    }
}

/// Configuration for [`new_model`](crate::new_model) (Go `ClientConfig`).
///
/// Resolution follows openai-go's `NewClient`: the environment applies first (`OPENAI_BASE_URL`,
/// `OPENAI_API_KEY`, `OPENAI_ORG_ID` → `OpenAI-Organization`, `OPENAI_PROJECT_ID` →
/// `OpenAI-Project`, `OPENAI_CUSTOM_HEADERS` lines `Name: value`), then the non-empty fields
/// here. A variable set to the empty string still applies (Go `os.LookupEnv`): an empty
/// `OPENAI_API_KEY` sends no `Authorization`, an empty `OPENAI_BASE_URL` makes every call fail.
///
/// Make sure `api_key` is non-empty when `base_url` points at another provider: otherwise
/// `OPENAI_API_KEY` is read and sent there, as in Go.
///
/// Not ported from Go: `Options []option.RequestOption` (use [`headers`](Self::headers) and
/// [`http_client`](Self::http_client)).
#[derive(Clone)]
pub struct ClientConfig {
    /// The API key, sent as `Authorization: Bearer <key>`. Empty: `OPENAI_API_KEY`.
    pub api_key: String,
    /// The base URL, for OpenAI-compatible endpoints. Empty: `OPENAI_BASE_URL`, else
    /// [`DEFAULT_BASE_URL`]. A base whose path does not end in `/` gets one (openai-go
    /// `WithBaseURL`), so `http://host/v1` sends to `http://host/v1/responses`.
    pub base_url: String,
    /// The API to talk to (default [`Api::Responses`]).
    pub api: Api,
    /// A custom HTTP client. Without one gadk builds a `reqwest` client with openai-go's
    /// 10-minute response-header timeout; a custom client gets none.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Headers sent with every request, after the environment's (Go `option.WithHeader` in
    /// `Options`). An `Authorization` here replaces the API key's.
    pub headers: HeaderMap,
    /// Environment variable source (`OPENAI_*`); tests inject a [`MapEnv`](gadk_platform::env::MapEnv).
    pub env: Arc<dyn EnvProvider>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: String::new(),
            api: Api::Responses,
            http_client: None,
            headers: HeaderMap::new(),
            env: Arc::new(ProcessEnv),
        }
    }
}

/// The API key and header values are never printed; a base URL loses embedded credentials.
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field(
                "api_key",
                &(!self.api_key.is_empty()).then_some("<redacted>"),
            )
            .field("base_url", &gadk_model::redact_url(&self.base_url))
            .field("api", &self.api)
            .field("http_client", &self.http_client.is_some())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// What a client sends, resolved once from a [`ClientConfig`] and its environment.
#[derive(Clone)]
pub(crate) struct Resolved {
    /// The effective base URL, as configured (the trailing `/` rule applies when a request URL
    /// is built, [`request_url`]).
    pub(crate) base_url: String,
    /// The effective API key; `""` sends no `Authorization`.
    pub(crate) api_key: String,
    /// Whether an `Authorization` header was set after the last API-key assignment (openai-go
    /// `headerOverride`): the bearer token is then not applied.
    pub(crate) authorization_overridden: bool,
    /// The environment's headers, in openai-go's order (`OpenAI-Organization`,
    /// `OpenAI-Project`, then each `OPENAI_CUSTOM_HEADERS` line); each replaces an earlier
    /// value of the same name.
    pub(crate) env_headers: Vec<(String, String)>,
    /// [`ClientConfig::headers`].
    pub(crate) headers: HeaderMap,
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("base_url", &gadk_model::redact_url(&self.base_url))
            .field(
                "api_key",
                &(!self.api_key.is_empty()).then_some("<redacted>"),
            )
            .field("authorization_overridden", &self.authorization_overridden)
            .field(
                "env_headers",
                &self.env_headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// openai-go `DefaultClientOptions` + `NewClient(opts...)` for the options ADK-Go passes, in
/// openai-go's assignment order: env base URL, env key (`SetAPIKey` clears `headerOverride`),
/// env headers (`SetHeader("Authorization")` sets it), config key, config base URL, config
/// headers.
pub(crate) fn resolve(cfg: &ClientConfig) -> Resolved {
    let env = cfg.env.as_ref();
    let mut base_url = DEFAULT_BASE_URL.to_string();
    let mut api_key = String::new();
    let mut overridden = false;
    let mut env_headers = Vec::new();

    if let Some(v) = env.var("OPENAI_BASE_URL") {
        base_url = v;
    }
    if let Some(v) = env.var("OPENAI_API_KEY") {
        api_key = v;
        overridden = false;
    }
    if let Some(v) = env.var("OPENAI_ORG_ID") {
        env_headers.push(("OpenAI-Organization".to_string(), v));
    }
    if let Some(v) = env.var("OPENAI_PROJECT_ID") {
        env_headers.push(("OpenAI-Project".to_string(), v));
    }
    if let Some(v) = env.var("OPENAI_CUSTOM_HEADERS") {
        for line in v.split('\n') {
            if let Some((name, value)) = line.split_once(':') {
                let name = name.trim();
                if name.eq_ignore_ascii_case("authorization") {
                    overridden = true;
                }
                env_headers.push((name.to_string(), value.trim().to_string()));
            }
        }
    }
    if !cfg.api_key.is_empty() {
        api_key = cfg.api_key.clone();
        overridden = false;
    }
    if !cfg.base_url.is_empty() {
        base_url = cfg.base_url.clone();
    }
    if cfg.headers.contains_key(http::header::AUTHORIZATION) {
        overridden = true;
    }
    Resolved {
        base_url,
        api_key,
        authorization_overridden: overridden,
        env_headers,
        headers: cfg.headers.clone(),
    }
}

/// How a request URL could not be built from the base URL.
#[allow(dead_code)] // D1: reached through `OpenAiClient::post` once Tasks 8 and 9 call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlError {
    /// The base has no scheme (`""` or a relative reference): Go's `http.Client` fails the
    /// call with `unsupported protocol scheme ""`.
    NoScheme,
    /// The base does not parse (Go `requestoption: WithBaseURL failed to parse url …`).
    Parse(String),
}

/// `path` resolved against `base_url` as openai-go does: `WithBaseURL` gives a non-empty path
/// a trailing `/`, then `url.Parse` + `ResolveReference` keep the base's path as a prefix and
/// drop its query and fragment.
#[allow(dead_code)] // D1: reached through `OpenAiClient::post` once Tasks 8 and 9 call it.
pub(crate) fn request_url(base_url: &str, path: &str) -> Result<url::Url, UrlError> {
    let mut base = match url::Url::parse(base_url) {
        Ok(u) => u,
        Err(url::ParseError::RelativeUrlWithoutBase) => return Err(UrlError::NoScheme),
        Err(e) => return Err(UrlError::Parse(e.to_string())),
    };
    if !base.path().ends_with('/') {
        let with_slash = format!("{}/", base.path());
        base.set_path(&with_slash);
    }
    base.join(path).map_err(|e| UrlError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::HashMap;

    use super::*;
    use gadk_platform::env::MapEnv;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;

    fn env(vars: &[(&str, &str)]) -> Arc<dyn EnvProvider> {
        Arc::new(MapEnv(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        ))
    }

    fn cfg(vars: &[(&str, &str)]) -> ClientConfig {
        ClientConfig {
            env: env(vars),
            ..ClientConfig::default()
        }
    }

    #[test]
    fn api_parses_gos_values_and_rejects_others_with_gos_text() {
        assert_eq!("".parse::<Api>().unwrap(), Api::Responses);
        assert_eq!("responses".parse::<Api>().unwrap(), Api::Responses);
        assert_eq!(
            "chat_completions".parse::<Api>().unwrap(),
            Api::ChatCompletions
        );
        let err = "grpc".parse::<Api>().unwrap_err();
        assert!(err.is(ErrorKind::UnsupportedApi));
        assert_eq!(err.to_string(), "openai: unsupported API: \"grpc\"");
        let err = "a\"b\n".parse::<Api>().unwrap_err();
        assert_eq!(err.to_string(), "openai: unsupported API: \"a\\\"b\\n\"");
        assert_eq!(Api::default(), Api::Responses);
        assert_eq!(Api::ChatCompletions.to_string(), "chat_completions");
    }

    #[test]
    fn defaults_without_environment() {
        let r = resolve(&cfg(&[]));
        assert_eq!(r.base_url, DEFAULT_BASE_URL);
        assert_eq!(r.api_key, "");
        assert!(!r.authorization_overridden);
        assert!(r.env_headers.is_empty());
    }

    #[test]
    fn environment_applies_then_non_empty_fields_override() {
        let vars = [
            ("OPENAI_BASE_URL", "https://proxy.example/v1"),
            ("OPENAI_API_KEY", "env-key"),
            ("OPENAI_ORG_ID", "org-1"),
            ("OPENAI_PROJECT_ID", "proj-1"),
            (
                "OPENAI_CUSTOM_HEADERS",
                "X-A: 1\nmalformed\n X-B :  two words \nX-A: 3",
            ),
        ];
        let r = resolve(&cfg(&vars));
        assert_eq!(r.base_url, "https://proxy.example/v1");
        assert_eq!(r.api_key, "env-key");
        assert_eq!(
            r.env_headers,
            vec![
                ("OpenAI-Organization".to_string(), "org-1".to_string()),
                ("OpenAI-Project".to_string(), "proj-1".to_string()),
                ("X-A".to_string(), "1".to_string()),
                ("X-B".to_string(), "two words".to_string()),
                ("X-A".to_string(), "3".to_string()),
            ]
        );

        let mut c = cfg(&vars);
        c.api_key = "cfg-key".into();
        c.base_url = "http://127.0.0.1:9/v1".into();
        let r = resolve(&c);
        assert_eq!(
            (r.api_key.as_str(), r.base_url.as_str()),
            ("cfg-key", "http://127.0.0.1:9/v1")
        );
    }

    /// Go `os.LookupEnv`: a variable set to `""` applies; an empty config field does not.
    #[test]
    fn empty_environment_values_still_apply() {
        let r = resolve(&cfg(&[
            ("OPENAI_BASE_URL", ""),
            ("OPENAI_API_KEY", ""),
            ("OPENAI_ORG_ID", ""),
            ("OPENAI_PROJECT_ID", ""),
        ]));
        assert_eq!(r.base_url, "");
        assert_eq!(r.api_key, "");
        assert_eq!(
            r.env_headers,
            vec![
                ("OpenAI-Organization".to_string(), String::new()),
                ("OpenAI-Project".to_string(), String::new()),
            ]
        );
        let r = resolve(&cfg(&[("OPENAI_API_KEY", "env-key")]));
        assert_eq!(r.api_key, "env-key", "an empty api_key does not override");
    }

    /// openai-go's `headerOverride`: an `Authorization` header set after the last API-key
    /// assignment wins; a later key assignment clears it.
    #[test]
    fn authorization_override_follows_assignment_order() {
        let custom_auth = [
            ("OPENAI_API_KEY", "env-key"),
            ("OPENAI_CUSTOM_HEADERS", "authorization: X"),
        ];
        assert!(resolve(&cfg(&custom_auth)).authorization_overridden);

        let mut c = cfg(&custom_auth);
        c.api_key = "cfg-key".into();
        let r = resolve(&c);
        assert!(
            !r.authorization_overridden,
            "the config key comes after the env headers"
        );
        assert_eq!(r.api_key, "cfg-key");

        let mut c = cfg(&[("OPENAI_API_KEY", "env-key")]);
        c.api_key = "cfg-key".into();
        c.headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer other"),
        );
        assert!(resolve(&c).authorization_overridden);
    }

    #[test]
    fn request_url_follows_with_base_url() {
        for (base, want) in [
            (
                "https://api.openai.com/v1/",
                "https://api.openai.com/v1/responses",
            ),
            (
                "http://127.0.0.1:8080/v1",
                "http://127.0.0.1:8080/v1/responses",
            ),
            (
                "http://127.0.0.1:8080/v1/",
                "http://127.0.0.1:8080/v1/responses",
            ),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080/responses"),
            ("http://h/a/b?q=1#f", "http://h/a/b/responses"),
        ] {
            assert_eq!(
                request_url(base, "responses").unwrap().as_str(),
                want,
                "{base}"
            );
        }
        assert_eq!(
            request_url("http://h/v1", "chat/completions")
                .unwrap()
                .as_str(),
            "http://h/v1/chat/completions"
        );
        assert_eq!(request_url("", "responses"), Err(UrlError::NoScheme));
        assert_eq!(
            request_url("localhost/v1", "responses"),
            Err(UrlError::NoScheme)
        );
        assert!(matches!(
            request_url("http://[::1", "responses"),
            Err(UrlError::Parse(_))
        ));
    }

    #[test]
    fn debug_redacts_the_key_and_header_values() {
        let mut c = cfg(&[]);
        c.api_key = "sk-secret".into();
        c.base_url = "https://user:pw@proxy.example/v1".into();
        c.headers
            .insert("x-secret", HeaderValue::from_static("hidden-value"));
        let s = format!("{c:?}");
        assert!(
            !s.contains("sk-secret") && !s.contains("pw@") && !s.contains("hidden-value"),
            "{s}"
        );
        assert!(s.contains("<redacted>") && s.contains("x-secret"), "{s}");
        let r = format!("{:?}", resolve(&c));
        assert!(
            !r.contains("sk-secret") && !r.contains("hidden-value"),
            "{r}"
        );
    }
}
