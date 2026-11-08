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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of ADK-Go `model/apigee/apigee_test.go`.
//!
//! Go's `t.Setenv` / `os.Unsetenv` become a per-test [`MapEnv`] (an unset variable is simply
//! absent), and Go's `roundTripFunc` `http.Client` becomes the scripted [`HttpClient`] below.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use gadk_gemini::apigee::{ApigeeConfig, ApigeeModel, ModelInfo, parse_model_name};
use gadk_gemini::{GeminiError, MapEnv};
use gadk_genai::{Content, GenerateContentConfig};
use gadk_model::{BodyStream, HttpClient, Llm, LlmRequest, ModelError};
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use rstest::rstest;

const PROXY_URL: &str = "https://test.apigee.net";

/// Go `roundTripFunc`: answers every request with one fixed response when the request URL is
/// one of `allowed`, otherwise fails the request (and the test) as Go's closure does. Records
/// the request URLs.
struct RoundTripFunc {
    allowed: Vec<&'static str>,
    content_type: &'static str,
    body: &'static str,
    urls: Mutex<Vec<String>>,
}

impl RoundTripFunc {
    fn new(allowed: Vec<&'static str>, content_type: &'static str, body: &'static str) -> Self {
        Self {
            allowed,
            content_type,
            body,
            urls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl HttpClient for RoundTripFunc {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let url = req.uri().to_string();
        self.urls.lock().push(url.clone());
        if !self.allowed.contains(&url.as_str()) {
            return Err(ModelError::Transport(
                format!("unexpected URL: {url}").into(),
            ));
        }
        let body: BodyStream = Box::pin(futures::stream::iter(vec![Ok(Bytes::from_static(
            self.body.as_bytes(),
        ))]));
        Ok(http::Response::builder()
            .status(200)
            .header("content-type", self.content_type)
            .body(body)
            .unwrap())
    }
}

/// The test environment (Go `t.Setenv` calls); variables not listed are unset.
fn env(vars: &[(&str, &str)]) -> Arc<MapEnv> {
    Arc::new(MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<HashMap<_, _>>(),
    ))
}

/// Go `WithProxyURL(proxyURL)` over `env`.
fn config_with_proxy(env: Arc<MapEnv>) -> ApigeeConfig {
    ApigeeConfig {
        proxy_url: Some(PROXY_URL.to_string()),
        env,
        ..ApigeeConfig::default()
    }
}

#[rstest]
#[case::apigee_gemini_1_5_flash("apigee/gemini-1.5-flash")]
#[case::apigee_v1_gemini_1_5_flash("apigee/v1/gemini-1.5-flash")]
#[case::apigee_vertex_ai_gemini_1_5_flash("apigee/vertex_ai/gemini-1.5-flash")]
#[case::apigee_gemini_v1_gemini_1_5_flash("apigee/gemini/v1/gemini-1.5-flash")]
#[case::apigee_vertex_ai_v1beta_gemini_1_5_flash("apigee/vertex_ai/v1beta/gemini-1.5-flash")]
#[tokio::test]
async fn test_new_model_with_valid_model_strings(#[case] model_name: &str) {
    let env = if model_name.contains("vertex_ai") {
        env(&[
            ("GOOGLE_API_KEY", "test-key"),
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
            ("GOOGLE_CLOUD_LOCATION", "test-location"),
        ])
    } else {
        env(&[("GOOGLE_API_KEY", "test-key")])
    };
    let client = Arc::new(RoundTripFunc::new(
        vec!["https://www.google.com"],
        "text/html",
        "<html><body>Mock Google</body></html>",
    ));
    let cfg = ApigeeConfig {
        http_client: Some(client.clone()),
        ..config_with_proxy(env)
    };

    let result = ApigeeModel::new(model_name, cfg).await;

    assert!(
        result.is_ok(),
        "NewModel({model_name:?}) returned an unexpected error: {:?}",
        result.err()
    );
    // NewModel sends no request.
    assert_eq!(*client.urls.lock(), Vec::<String>::new());
}

#[rstest]
#[case::apigee_openai_v1_gpt("apigee/openai/v1/gpt")]
#[case::apigee_("apigee/")]
#[case::apigee("apigee")]
#[case::gemini_pro("gemini-pro")]
#[case::apigee_vertex_ai_v1_model_extra("apigee/vertex_ai/v1/model/extra")]
#[case::apigee_unknown_model("apigee/unknown/model")]
#[tokio::test]
async fn test_new_model_with_invalid_model_strings(#[case] model_name: &str) {
    let cfg = config_with_proxy(env(&[("GOOGLE_API_KEY", "test-key")]));

    let err = ApigeeModel::new(model_name, cfg).await.unwrap_err();

    assert_eq!(
        err.to_string(),
        format!("invalid model string: {model_name}")
    );
    assert!(matches!(err, GeminiError::Config(_)), "{err:?}");
}

fn info(model_id: &str, api_version: &str, is_vertex_ai: bool) -> Option<ModelInfo> {
    Some(ModelInfo {
        model_id: model_id.to_string(),
        api_version: api_version.to_string(),
        is_vertex_ai,
    })
}

#[rstest]
#[case::simple("apigee/gemini-1.5-flash", "", info("gemini-1.5-flash", "", false))]
#[case::simple_vertex_env("apigee/gemini-1.5-flash", "true", info("gemini-1.5-flash", "", true))]
#[case::v1(
    "apigee/v1/gemini-1.5-flash",
    "",
    info("gemini-1.5-flash", "v1", false)
)]
#[case::vertex(
    "apigee/vertex_ai/gemini-1.5-flash",
    "",
    info("gemini-1.5-flash", "", true)
)]
#[case::gemini_v1(
    "apigee/gemini/v1/gemini-1.5-flash",
    "",
    info("gemini-1.5-flash", "v1", false)
)]
#[case::vertex_v1beta(
    "apigee/vertex_ai/v1beta/gemini-1.5-flash",
    "",
    info("gemini-1.5-flash", "v1beta", true)
)]
#[case::invalid_openai("apigee/openai/v1/gpt", "", None)]
#[case::invalid_trailing_slash("apigee/", "", None)]
#[case::invalid_extra_parts("apigee/vertex_ai/v1/model/extra", "", None)]
#[case::invalid_unknown("apigee/unknown/model", "", None)]
fn test_parse_model_name(
    #[case] model_name: &str,
    #[case] vertex_env: &str,
    #[case] want: Option<ModelInfo>,
) {
    let env = if vertex_env.is_empty() {
        env(&[])
    } else {
        env(&[("GOOGLE_GENAI_USE_VERTEXAI", vertex_env)])
    };

    let got = parse_model_name(model_name, env.as_ref());

    match want {
        Some(want) => assert_eq!(got.unwrap(), want, "parseModelName({model_name:?})"),
        None => assert_eq!(
            got.unwrap_err().to_string(),
            format!("invalid model string: {model_name}")
        ),
    }
}

#[tokio::test]
async fn test_new_model_with_custom_headers() {
    let mut headers = http::HeaderMap::new();
    headers.insert("x-custom-header", "custom-value".parse().unwrap());
    let cfg = ApigeeConfig {
        custom_headers: headers,
        ..config_with_proxy(env(&[("GOOGLE_API_KEY", "test-key")]))
    };

    let model = ApigeeModel::new("apigee/gemini-1.5-flash", cfg).await;

    assert!(
        model.is_ok(),
        "NewModel() returned an unexpected error: {:?}",
        model.err()
    );
}

#[tokio::test]
async fn test_new_model_without_proxy_url() {
    let cfg = ApigeeConfig {
        env: env(&[("GOOGLE_API_KEY", "test-key")]),
        ..ApigeeConfig::default()
    };
    let err = ApigeeModel::new("apigee/gemini-1.5-flash", cfg)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "APIGEE_PROXY_URL environment variable not set"
    );

    let cfg = ApigeeConfig {
        env: env(&[
            ("GOOGLE_API_KEY", "test-key"),
            ("APIGEE_PROXY_URL", "https://env.proxy.url"),
        ]),
        ..ApigeeConfig::default()
    };
    let model = ApigeeModel::new("apigee/gemini-1.5-flash", cfg).await;
    assert!(
        model.is_ok(),
        "NewModel() returned an unexpected error: {:?}",
        model.err()
    );
}

#[tokio::test]
async fn test_new_model_vertex_missing_project_or_location() {
    let cfg = config_with_proxy(env(&[
        ("GOOGLE_API_KEY", "test-key"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
    ]));
    let err = ApigeeModel::new("apigee/gemini-1.5-flash", cfg)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("GOOGLE_CLOUD_PROJECT"),
        "NewModel() with vertex enabled but no project env var should fail: {err}"
    );
    assert_eq!(
        err.to_string(),
        "GOOGLE_CLOUD_PROJECT environment variable must be set"
    );

    let cfg = config_with_proxy(env(&[
        ("GOOGLE_API_KEY", "test-key"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "test-project"),
    ]));
    let err = ApigeeModel::new("apigee/gemini-1.5-flash", cfg)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("GOOGLE_CLOUD_LOCATION"),
        "NewModel() with vertex enabled but no location env var should fail: {err}"
    );
    assert_eq!(
        err.to_string(),
        "GOOGLE_CLOUD_LOCATION environment variable must be set"
    );
}

#[tokio::test]
async fn test_generate_content() {
    let env = env(&[
        ("GOOGLE_API_KEY", "test-key"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "test-project"),
        ("GOOGLE_CLOUD_LOCATION", "test-location"),
    ]);
    let client = Arc::new(RoundTripFunc::new(
        vec![
            "https://test.apigee.net/v1/models/gemini-1.5-flash:generateContent",
            "https://test.apigee.net/v1/models/gemini-1.5-flash:streamGenerateContent?alt=sse",
        ],
        "application/json",
        "{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Paris\"}]}}]}",
    ));
    let cfg = ApigeeConfig {
        http_client: Some(client.clone()),
        ..config_with_proxy(env)
    };
    let model = ApigeeModel::new("apigee/gemini/v1/gemini-1.5-flash", cfg)
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![Content::from_text(
            "What is the capital of France? One word.",
            "user",
        )],
        config: Some(GenerateContentConfig {
            temperature: Some(0.0),
            ..Default::default()
        }),
        ..Default::default()
    };

    let responses: Vec<_> = model.generate_content(req, false).collect().await;

    assert_eq!(responses.len(), 1);
    for resp in responses {
        let resp = resp.unwrap();
        let content = resp
            .content
            .expect("GenerateContent() returned an empty response");
        assert!(
            !content.parts.is_empty(),
            "GenerateContent() returned an empty response"
        );
    }
    assert_eq!(
        *client.urls.lock(),
        vec!["https://test.apigee.net/v1/models/gemini-1.5-flash:generateContent".to_string()]
    );
    assert_eq!(model.name(), "apigee/gemini/v1/gemini-1.5-flash");
}
