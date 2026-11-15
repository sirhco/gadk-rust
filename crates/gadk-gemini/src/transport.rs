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

//! REST transport for `generateContent` / `streamGenerateContent` (port of the parts of
//! genai `api_client.go` and `models.go` that the Gemini model uses).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use gadk_genai::GenerateContentResponse;
use gadk_model::sse::sse_events;
use gadk_model::{
    Backend, BodyStream, HttpClient, HttpOptions, ModelError, api_error_from_body, body_prefix,
    collect_body,
};
use http::{HeaderMap, HeaderValue};
use serde_json::{Map, Value};

use crate::auth::auth_headers;
use crate::config::ResolvedConfig;

/// Port of genai `tModel` plus the Vertex AI `projects/{p}/locations/{l}/` prefix that
/// genai `createAPIURL` adds: the request path below the API version.
pub(crate) fn model_path(resolved: &ResolvedConfig, model: &str) -> Result<String, ModelError> {
    if model.is_empty() {
        return Err(ModelError::Config("tModel: model is empty".into()));
    }
    if model.contains('?') || model.contains('&') || model.contains("..") {
        return Err(ModelError::Config("tModel: invalid model parameter".into()));
    }
    match resolved.backend {
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
            match (resolved.project.as_deref(), resolved.location.as_deref()) {
                (Some(p), Some(l))
                    if !p.is_empty() && !l.is_empty() && !name.starts_with("projects/") =>
                {
                    Ok(format!("projects/{p}/locations/{l}/{name}"))
                }
                _ => Ok(name),
            }
        }
        Backend::GeminiApi => {
            if model.starts_with("models/") || model.starts_with("tunedModels/") {
                Ok(model.to_string())
            } else {
                Ok(format!("models/{model}"))
            }
        }
    }
}

fn timed_out() -> ModelError {
    ModelError::Transport(Box::new(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "request timed out",
    )))
}

async fn with_timeout<T>(
    timeout: Option<Duration>,
    fut: impl Future<Output = Result<T, ModelError>>,
) -> Result<T, ModelError> {
    match timeout {
        Some(d) if !d.is_zero() => tokio::time::timeout(d, fut)
            .await
            .unwrap_or_else(|_| Err(timed_out())),
        _ => fut.await,
    }
}

/// Wraps `body` so that each chunk must arrive within `timeout`; a timeout ends the stream.
fn chunk_timeout(body: BodyStream, timeout: Option<Duration>) -> BodyStream {
    let Some(d) = timeout.filter(|d| !d.is_zero()) else {
        return body;
    };
    futures::stream::unfold(Some(body), move |state| async move {
        let mut body = state?;
        match tokio::time::timeout(d, body.next()).await {
            Ok(Some(item)) => Some((item, Some(body))),
            Ok(None) => None,
            Err(_) => Some((Err(timed_out()), None)),
        }
    })
    .boxed()
}

/// Port of genai `generateContentResponseFrom{Mldev,Vertex}`: keeps the fields each
/// converter copies (Gemini API: `citationMetadata.citationSources` becomes `citations`).
fn normalize_response(backend: Backend, value: Value) -> Value {
    let Value::Object(from) = value else {
        return value;
    };
    let mut to = Map::new();
    let top: &[&str] = match backend {
        Backend::GeminiApi => &[
            "modelVersion",
            "promptFeedback",
            "responseId",
            "usageMetadata",
            "modelStatus",
        ],
        Backend::VertexAi => &[
            "createTime",
            "modelVersion",
            "promptFeedback",
            "responseId",
            "usageMetadata",
        ],
    };
    if let Some(candidates) = from.get("candidates").filter(|v| !v.is_null()) {
        let candidates = match (backend, candidates) {
            (Backend::GeminiApi, Value::Array(items)) => {
                Value::Array(items.iter().map(candidate_from_mldev).collect())
            }
            (_, other) => other.clone(),
        };
        to.insert("candidates".into(), candidates);
    }
    for key in top {
        if let Some(v) = from.get(*key).filter(|v| !v.is_null()) {
            to.insert((*key).to_string(), v.clone());
        }
    }
    Value::Object(to)
}

/// Port of genai `candidateFromMldev` / `citationMetadataFromMldev`.
fn candidate_from_mldev(value: &Value) -> Value {
    let Some(from) = value.as_object() else {
        return value.clone();
    };
    let mut to = Map::new();
    for key in [
        "content",
        "citationMetadata",
        "tokenCount",
        "finishReason",
        "groundingMetadata",
        "avgLogprobs",
        "index",
        "logprobsResult",
        "safetyRatings",
        "urlContextMetadata",
        "continuationToken",
    ] {
        let Some(v) = from.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let v = if key == "citationMetadata" {
            let mut citation = Map::new();
            if let Some(c) = v.get("citationSources").filter(|c| !c.is_null()) {
                citation.insert("citations".into(), c.clone());
            }
            Value::Object(citation)
        } else {
            v.clone()
        };
        to.insert(key.to_string(), v);
    }
    Value::Object(to)
}

fn decode_error(message: String, body: &[u8]) -> ModelError {
    ModelError::Decode {
        message,
        body_prefix: body_prefix(body),
    }
}

/// Decodes one response object (a whole body, or one SSE event's data).
fn decode_response(backend: Backend, data: &[u8]) -> Result<GenerateContentResponse, ModelError> {
    let value: Value = serde_json::from_slice(data)
        .map_err(|e| decode_error(format!("invalid response JSON: {e}"), data))?;
    if value.get("error").is_some_and(Value::is_object) {
        // A chunk carrying `"error"` yields that `APIError` (genai v1.77.0
        // `iterateResponseStream`): code, status, message and details come from the body
        // only, so a missing code is 0 as in Go.
        return Err(api_error_from_body(0, data));
    }
    serde_json::from_value(normalize_response(backend, value))
        .map_err(|e| decode_error(format!("invalid GenerateContentResponse: {e}"), data))
}

/// Sends requests for one model client.
#[derive(Clone)]
pub(crate) struct Transport {
    http: Arc<dyn HttpClient>,
    resolved: Arc<ResolvedConfig>,
    base_url: String,
    api_version: String,
    timeout: Option<Duration>,
}

impl Transport {
    pub(crate) fn new(http: Arc<dyn HttpClient>, resolved: Arc<ResolvedConfig>) -> Self {
        Self {
            http,
            base_url: resolved.base_url.clone(),
            api_version: resolved.api_version.clone(),
            timeout: resolved.http_options.timeout,
            resolved,
        }
    }

    /// Applies a request's base URL, API version and timeout overrides (genai
    /// `patchHTTPOptions`: empty strings do not override).
    pub(crate) fn with_request_options(&self, opts: Option<&HttpOptions>) -> Self {
        let mut t = self.clone();
        if let Some(o) = opts {
            if let Some(u) = o.base_url.as_ref().filter(|u| !u.is_empty()) {
                t.base_url = u.clone();
            }
            if let Some(v) = o.api_version.as_ref().filter(|v| !v.is_empty()) {
                t.api_version = v.clone();
            }
            if o.timeout.is_some() {
                t.timeout = o.timeout;
            }
        }
        t
    }

    /// Port of genai `createAPIURL` for `{model}:{method}`.
    fn url(&self, model: &str, method: &str) -> Result<String, ModelError> {
        let path = model_path(&self.resolved, model)?;
        let version = &self.api_version;
        let path = match self.resolved.backend {
            Backend::VertexAi => format!("{version}/{path}"),
            Backend::GeminiApi if path.contains(&format!("/{version}/")) => path,
            Backend::GeminiApi => format!("{version}/{path}"),
        };
        let base = self.base_url.trim_end_matches('/');
        Ok(format!("{base}/{path}:{method}"))
    }

    /// Sends `body` to `url` with `headers` plus auth and JSON content type (genai
    /// `buildRequest`); a non-2xx status becomes the API error from the body.
    async fn send(
        &self,
        url: String,
        body: Map<String, Value>,
        mut headers: HeaderMap,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        if let Some(d) = self.timeout.filter(|d| !d.is_zero()) {
            let secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
            headers.insert("x-server-timeout", HeaderValue::from(secs));
        }
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        // `extend` replaces each auth header name once and keeps all of its values (a
        // `HeaderMap` yields `None` names for a header's second and later values).
        headers.extend(auth_headers(&self.resolved.auth).await?);
        let bytes = serde_json::to_vec(&body).map_err(|e| ModelError::Other(Box::new(e)))?;
        let mut req = http::Request::builder()
            .method(http::Method::POST)
            .uri(&url)
            .body(Bytes::from(bytes))
            .map_err(|e| ModelError::Config(format!("invalid request URL {url:?}: {e}")))?;
        *req.headers_mut() = headers;

        let resp = self.http.send(req).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = collect_body(resp.into_body()).await?;
            return Err(api_error_from_body(status.as_u16(), &body));
        }
        Ok(resp)
    }

    /// `POST {model}:generateContent`.
    pub(crate) async fn generate(
        &self,
        model: &str,
        body: Map<String, Value>,
        headers: HeaderMap,
    ) -> Result<GenerateContentResponse, ModelError> {
        let url = self.url(model, "generateContent")?;
        let backend = self.resolved.backend;
        with_timeout(self.timeout, async move {
            let resp = self.send(url, body, headers).await?;
            let bytes = collect_body(resp.into_body()).await?;
            decode_response(backend, &bytes)
        })
        .await
    }

    /// `POST {model}:streamGenerateContent?alt=sse`: one response per SSE event. The
    /// request is sent when the stream is first polled; an error ends the stream.
    pub(crate) fn stream(
        &self,
        model: &str,
        body: Map<String, Value>,
        headers: HeaderMap,
    ) -> BoxStream<'static, Result<GenerateContentResponse, ModelError>> {
        let this = self.clone();
        let model = model.to_string();
        let backend = self.resolved.backend;
        let events = futures::stream::once(async move {
            let url = this.url(&model, "streamGenerateContent?alt=sse")?;
            let resp = with_timeout(this.timeout, this.send(url, body, headers)).await?;
            Ok::<_, ModelError>(sse_events(chunk_timeout(resp.into_body(), this.timeout)))
        })
        .try_flatten();
        let mut failed = false;
        events
            .map(move |event| event.and_then(|ev| decode_response(backend, ev.data.as_bytes())))
            .take_while(move |item| {
                let keep = !failed;
                failed |= item.is_err();
                futures::future::ready(keep)
            })
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use gadk_model::HttpOptions;

    fn resolved(backend: Backend, project: Option<&str>, location: Option<&str>) -> ResolvedConfig {
        ResolvedConfig {
            backend,
            project: project.map(str::to_string),
            location: location.map(str::to_string),
            base_url: "https://example.test/".into(),
            api_version: "v1beta".into(),
            auth: None,
            needs_adc: false,
            http_options: HttpOptions::default(),
        }
    }

    #[test]
    fn model_path_gemini() {
        let r = resolved(Backend::GeminiApi, None, None);
        assert_eq!(
            model_path(&r, "gemini-2.5-flash").unwrap(),
            "models/gemini-2.5-flash"
        );
        assert_eq!(model_path(&r, "models/x").unwrap(), "models/x");
        assert_eq!(model_path(&r, "tunedModels/x").unwrap(), "tunedModels/x");
        assert!(matches!(model_path(&r, ""), Err(ModelError::Config(_))));
        assert!(matches!(model_path(&r, "a?b"), Err(ModelError::Config(_))));
        assert!(matches!(model_path(&r, "../x"), Err(ModelError::Config(_))));
    }

    #[test]
    fn model_path_vertex() {
        let r = resolved(Backend::VertexAi, Some("p"), Some("l"));
        assert_eq!(
            model_path(&r, "gemini-2.0-flash").unwrap(),
            "projects/p/locations/l/publishers/google/models/gemini-2.0-flash"
        );
        assert_eq!(
            model_path(&r, "anthropic/claude").unwrap(),
            "projects/p/locations/l/publishers/anthropic/models/claude"
        );
        assert_eq!(
            model_path(&r, "projects/q/locations/m/endpoints/e").unwrap(),
            "projects/q/locations/m/endpoints/e"
        );
        let no_project = resolved(Backend::VertexAi, None, None);
        assert_eq!(
            model_path(&no_project, "gemini-2.0-flash").unwrap(),
            "publishers/google/models/gemini-2.0-flash"
        );
    }

    #[test]
    fn normalize_mldev_citations() {
        let v = serde_json::json!({
            "candidates": [{"citationMetadata": {"citationSources": [{"uri": "u"}]}, "finishMessage": "dropped"}],
            "createTime": "2026-01-01T00:00:00Z"
        });
        assert_eq!(
            normalize_response(Backend::GeminiApi, v),
            serde_json::json!({"candidates": [{"citationMetadata": {"citations": [{"uri": "u"}]}}]})
        );
    }
}
