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

//! The Live WebSocket URL and setup model name: port of genai v1.75.0 `live.go`
//! `Live.Connect` (lines 64–135) and `transformer.go` `tModel` / `tModelFullName`.

use gadk_model::Backend;
use gadk_platform::go_path_join;
use url::Url;

use crate::config::{Auth, ResolvedConfig};
use crate::error::GeminiError;

/// Port of the URL half of genai `Live.Connect`.
///
/// The scheme is the base URL's when it is `ws`/`wss`, else `wss`. Vertex AI uses
/// `ws/google.cloud.aiplatform.{v}.LlmBidiService/BidiGenerateContent` below the base URL's
/// path, or the base URL's path itself when there is no project/location (a proxy). The
/// Gemini API uses `ws/google.ai.generativelanguage.{v}.GenerativeService.BidiGenerateContent`
/// below the base URL's path. Ephemeral `auth_tokens/` keys are rejected (gadk deviation).
pub(crate) fn live_url(resolved: &ResolvedConfig) -> Result<Url, GeminiError> {
    let api_version = &resolved.api_version;
    if api_version.is_empty() {
        return Err(GeminiError::Config(
            "live module requires APIVersion to be set. You can set APIVersion to v1beta1 for BackendVertexAI or v1apha for BackendGeminiAPI".into(),
        ));
    }
    let base = Url::parse(&resolved.base_url)
        .map_err(|e| GeminiError::Config(format!("failed to parse base URL: {e}")))?;
    let scheme = match base.scheme() {
        s @ ("ws" | "wss") => s,
        _ => "wss",
    };

    let path = match resolved.backend {
        Backend::VertexAi => {
            let has_standard_auth = resolved.project.as_deref().is_some_and(|p| !p.is_empty())
                && resolved.location.as_deref().is_some_and(|l| !l.is_empty());
            // genai: `baseURL.String() != ""` always holds once the client is resolved, so
            // without standard auth the proxy's own path is used.
            if has_standard_auth {
                go_path_join(&[
                    base.path(),
                    format!(
                        "ws/google.cloud.aiplatform.{api_version}.LlmBidiService/BidiGenerateContent"
                    )
                    .as_str(),
                ])
            } else {
                base.path().to_string()
            }
        }
        Backend::GeminiApi => {
            match &resolved.auth {
                Some(Auth::ApiKey(key)) if key.starts_with("auth_tokens/") => {
                    return Err(GeminiError::Config(
                        "ephemeral auth_tokens keys are not supported".into(),
                    ));
                }
                Some(Auth::ApiKey(key)) if !key.is_empty() => {}
                // genai leaves the URL empty without an API key, and the dial fails.
                _ => {
                    return Err(GeminiError::Config(
                        "the Gemini API Live connection requires an API key".into(),
                    ));
                }
            }
            go_path_join(&[
                base.path(),
                format!(
                    "ws/google.ai.generativelanguage.{api_version}.GenerativeService.BidiGenerateContent"
                )
                .as_str(),
            ])
        }
    };

    // genai builds `url.URL{Scheme, Host, Path}`: user info, query and fragment are dropped.
    let host = base
        .host_str()
        .ok_or_else(|| GeminiError::Config(format!("base URL has no host: {base}")))?;
    let authority = match base.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    Url::parse(&format!("{scheme}://{authority}{path}"))
        .map_err(|e| GeminiError::Config(format!("invalid Live URL: {e}")))
}

/// Port of genai `tModel` then `tModelFullName`: the `model` of the setup message
/// (`liveConnectParametersTo*` applies `tModel` again, which leaves the full name as is).
pub(crate) fn model_full_name(
    resolved: &ResolvedConfig,
    model: &str,
) -> Result<String, GeminiError> {
    if model.is_empty() {
        return Err(GeminiError::Config(
            "tModelFullName: tModel: model is empty".into(),
        ));
    }
    if model.contains('?') || model.contains('&') || model.contains("..") {
        return Err(GeminiError::Config(
            "tModelFullName: tModel: invalid model parameter".into(),
        ));
    }
    match resolved.backend {
        Backend::GeminiApi => {
            if model.starts_with("models/") || model.starts_with("tunedModels/") {
                Ok(model.to_string())
            } else {
                Ok(format!("models/{model}"))
            }
        }
        Backend::VertexAi => {
            let name = if model.starts_with("projects/")
                || model.starts_with("models/")
                || model.starts_with("publishers/")
            {
                model.to_string()
            } else if let Some((publisher, name)) = model.split_once('/') {
                format!("publishers/{publisher}/models/{name}")
            } else {
                format!("publishers/google/models/{model}")
            };
            let project = resolved.project.as_deref().unwrap_or_default();
            let location = resolved.location.as_deref().unwrap_or_default();
            if project.is_empty() || location.is_empty() {
                return Ok(name);
            }
            if name.starts_with("publishers/") {
                Ok(format!("projects/{project}/locations/{location}/{name}"))
            } else if name.starts_with("models/") {
                Ok(format!(
                    "projects/{project}/locations/{location}/publishers/google/{name}"
                ))
            } else {
                Ok(name)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gadk_model::HttpOptions;

    fn resolved(
        backend: Backend,
        base_url: &str,
        project: Option<&str>,
        auth: Option<Auth>,
    ) -> ResolvedConfig {
        ResolvedConfig {
            backend,
            project: project.map(str::to_string),
            location: project.map(|_| "l".to_string()),
            base_url: base_url.into(),
            api_version: match backend {
                Backend::GeminiApi => "v1beta".into(),
                Backend::VertexAi => "v1beta1".into(),
            },
            auth,
            needs_adc: false,
            http_options: HttpOptions::default(),
        }
    }

    fn key(k: &str) -> Option<Auth> {
        Some(Auth::ApiKey(k.into()))
    }

    #[test]
    fn scheme_defaults_to_wss_and_keeps_ws() {
        let r = resolved(
            Backend::GeminiApi,
            "https://generativelanguage.googleapis.com/",
            None,
            key("k"),
        );
        assert_eq!(
            live_url(&r).unwrap().as_str(),
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
        );
        let r = resolved(Backend::GeminiApi, "ws://h:1/a/?q=1#f", None, key("k"));
        assert_eq!(
            live_url(&r).unwrap().as_str(),
            "ws://h:1/a/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
        );
        let r = resolved(
            Backend::VertexAi,
            "wss://l-aiplatform.googleapis.com/",
            Some("p"),
            None,
        );
        assert_eq!(
            live_url(&r).unwrap().as_str(),
            "wss://l-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );
    }

    #[test]
    fn vertex_without_project_uses_base_path() {
        let r = resolved(Backend::VertexAi, "https://proxy/x/y/", None, None);
        assert_eq!(live_url(&r).unwrap().as_str(), "wss://proxy/x/y/");
    }

    #[test]
    fn gemini_api_rejects_auth_tokens_and_missing_key() {
        let r = resolved(Backend::GeminiApi, "https://h/", None, key("auth_tokens/x"));
        assert!(matches!(live_url(&r), Err(GeminiError::Config(_))));
        let r = resolved(Backend::GeminiApi, "https://h/", None, None);
        assert!(matches!(live_url(&r), Err(GeminiError::Config(_))));
    }

    #[test]
    fn full_model_names() {
        let g = resolved(Backend::GeminiApi, "https://h/", None, key("k"));
        assert_eq!(model_full_name(&g, "m").unwrap(), "models/m");
        assert_eq!(
            model_full_name(&g, "tunedModels/t").unwrap(),
            "tunedModels/t"
        );
        let v = resolved(Backend::VertexAi, "https://h/", Some("p"), None);
        assert_eq!(
            model_full_name(&v, "m").unwrap(),
            "projects/p/locations/l/publishers/google/models/m"
        );
        assert_eq!(
            model_full_name(&v, "models/m").unwrap(),
            "projects/p/locations/l/publishers/google/models/m"
        );
        assert_eq!(
            model_full_name(&v, "acme/m").unwrap(),
            "projects/p/locations/l/publishers/acme/models/m"
        );
        assert_eq!(model_full_name(&v, "projects/q/x").unwrap(), "projects/q/x");
        let proxy = resolved(Backend::VertexAi, "https://h/", None, None);
        assert_eq!(
            model_full_name(&proxy, "m").unwrap(),
            "publishers/google/models/m"
        );
        assert!(model_full_name(&g, "").is_err());
        assert!(model_full_name(&g, "a?b").is_err());
    }
}
