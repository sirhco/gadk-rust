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

//! Port of the genai `NewInternalAPIClient` config resolution ladder
//! (`google.golang.org/genai@v1.75.0` `client.go`). Every warn and error branch has a
//! `config_ladder_*` case.

use gadk_gemini::*;
use gadk_model::Backend;
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

fn cfg(env: &[(&str, &str)]) -> ClientConfig {
    ClientConfig {
        env: Arc::new(MapEnv(
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        )),
        ..Default::default()
    }
}

fn anon() -> google_cloud_auth::credentials::Credentials {
    google_cloud_auth::credentials::anonymous::Builder::new().build()
}

// ---- warning capture -------------------------------------------------------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `resolve(c)` with a thread-local subscriber and returns the result and the log text.
fn resolve_logged(c: &ClientConfig) -> (Result<ResolvedConfig, GeminiError>, String) {
    let buf = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let r = tracing::subscriber::with_default(subscriber, || resolve(c));
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (r, out)
}

// ---- brief minimum cases ---------------------------------------------------

#[test]
fn config_ladder_default_backend_gemini_api_requires_key() {
    let err = resolve(&cfg(&[])).err().unwrap();
    assert!(
        err.to_string()
            .contains("api key is required for Google AI backend"),
        "{err}"
    );
    assert!(matches!(err, GeminiError::Config(_)));
}

#[test]
fn config_ladder_google_api_key_wins_over_gemini_api_key() {
    let (r, log) = resolve_logged(&cfg(&[("GOOGLE_API_KEY", "g"), ("GEMINI_API_KEY", "m")]));
    let r = r.unwrap();
    assert!(matches!(r.auth, Some(Auth::ApiKey(k)) if k == "g"));
    assert_eq!(
        (r.backend, r.base_url.as_str(), r.api_version.as_str()),
        (
            Backend::GeminiApi,
            "https://generativelanguage.googleapis.com/",
            "v1beta"
        )
    );
    assert!(
        log.contains("Both GOOGLE_API_KEY and GEMINI_API_KEY are set. Using GOOGLE_API_KEY."),
        "{log}"
    );
    assert!(!r.needs_adc);
}

#[test]
fn config_ladder_gemini_api_key_alone() {
    let (r, log) = resolve_logged(&cfg(&[("GEMINI_API_KEY", "m")]));
    let r = r.unwrap();
    assert!(matches!(r.auth, Some(Auth::ApiKey(k)) if k == "m"));
    assert!(!log.contains("Both GOOGLE_API_KEY"), "{log}");
}

#[test]
fn config_ladder_enterprise_env_wins_over_vertex_env() {
    let (r, log) = resolve_logged(&cfg(&[
        ("GOOGLE_GENAI_USE_ENTERPRISE", "false"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_API_KEY", "k"),
    ]));
    assert_eq!(r.unwrap().backend, Backend::GeminiApi);
    assert!(
        log.contains(
            "Both GOOGLE_GENAI_USE_ENTERPRISE and GOOGLE_GENAI_USE_VERTEXAI are set with conflicting values. The value of GOOGLE_GENAI_USE_ENTERPRISE will be used."
        ),
        "{log}"
    );
}

#[test]
fn config_ladder_enterprise_env_true_selects_vertex_without_conflict_warning() {
    let (r, log) = resolve_logged(&cfg(&[
        ("GOOGLE_GENAI_USE_ENTERPRISE", "TRUE"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "1"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
    ]));
    assert_eq!(r.unwrap().backend, Backend::VertexAi);
    assert!(!log.contains("conflicting values"), "{log}");
}

#[test]
fn config_ladder_vertex_env_false_selects_gemini_api() {
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "0"),
        ("GOOGLE_API_KEY", "k"),
    ]))
    .unwrap();
    assert_eq!(r.backend, Backend::GeminiApi);
}

#[test]
fn config_ladder_explicit_backend_ignores_env() {
    let mut c = cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_API_KEY", "k"),
    ]);
    c.backend = Some(Backend::GeminiApi);
    assert_eq!(resolve(&c).unwrap().backend, Backend::GeminiApi);
}

#[test]
fn config_ladder_vertex_regional_endpoint() {
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "1"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
    ]))
    .unwrap();
    assert_eq!(
        (r.base_url.as_str(), r.api_version.as_str()),
        ("https://us-central1-aiplatform.googleapis.com/", "v1beta1")
    );
    assert!(r.auth.is_none() && r.needs_adc); // ADC built later
    assert_eq!(r.project.as_deref(), Some("p"));
}

#[test]
fn config_ladder_vertex_multi_region_rep_endpoint() {
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "eu"),
    ]))
    .unwrap();
    assert_eq!(r.base_url, "https://aiplatform.eu.rep.googleapis.com/");
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "us"),
    ]))
    .unwrap();
    assert_eq!(r.base_url, "https://aiplatform.us.rep.googleapis.com/");
}

#[test]
fn config_ladder_vertex_region_env_fallback_and_global_default() {
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_REGION", "asia-east1"),
    ]))
    .unwrap();
    assert_eq!(r.location.as_deref(), Some("asia-east1"));
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
    ]))
    .unwrap();
    assert_eq!(
        (r.location.as_deref(), r.base_url.as_str()),
        (Some("global"), "https://aiplatform.googleapis.com/")
    );
}

#[test]
fn config_ladder_vertex_location_env_beats_region_env() {
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "europe-west4"),
        ("GOOGLE_CLOUD_REGION", "asia-east1"),
    ]))
    .unwrap();
    assert_eq!(r.location.as_deref(), Some("europe-west4"));
}

#[test]
fn config_ladder_explicit_project_location_beat_env() {
    let mut c = cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "envp"),
        ("GOOGLE_CLOUD_LOCATION", "envl"),
    ]);
    c.project = Some("p".into());
    c.location = Some("us-east4".into());
    let r = resolve(&c).unwrap();
    assert_eq!(
        (
            r.project.as_deref(),
            r.location.as_deref(),
            r.base_url.as_str()
        ),
        (
            Some("p"),
            Some("us-east4"),
            "https://us-east4-aiplatform.googleapis.com/"
        )
    );
}

// Vertex warn case 4: implicit project/location beats implicit API key.
#[test]
fn config_ladder_env_project_beats_env_api_key_for_vertex() {
    let (r, log) = resolve_logged(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ("GOOGLE_API_KEY", "k"),
    ]));
    let r = r.unwrap();
    assert!(r.auth.is_none());
    assert!(r.needs_adc);
    assert!(
        log.contains("The project/location from the environment variables will take precedence over the API key from the environment variable."),
        "{log}"
    );
}

// Vertex warn case 1: user credentials beat the env API key.
#[test]
fn config_ladder_vertex_credentials_beat_env_api_key() {
    let mut c = cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ("GOOGLE_API_KEY", "k"),
    ]);
    c.credentials = Some(anon());
    let (r, log) = resolve_logged(&c);
    let r = r.unwrap();
    assert!(matches!(r.auth, Some(Auth::Credentials(_))));
    assert!(!r.needs_adc);
    assert!(
        log.contains("The user provided Google Cloud credentials will take precedence over the API key from the environment variable."),
        "{log}"
    );
}

// Vertex warn case 2: explicit API key beats implicit project/location. The key then hits the
// gadk deviation (no API keys on Vertex).
#[test]
fn config_ladder_vertex_explicit_api_key_beats_env_project_then_rejected() {
    let mut c = cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
    ]);
    c.api_key = Some("k".into());
    let (r, log) = resolve_logged(&c);
    let err = r.err().unwrap();
    assert!(
        log.contains("The user provided Vertex AI API key will take precedence over the project/location from the environment variables."),
        "{log}"
    );
    assert!(
        err.to_string()
            .contains("API keys are only supported for the Gemini API backend"),
        "{err}"
    );
}

// Vertex warn case 3: explicit project/location beats implicit API key.
#[test]
fn config_ladder_vertex_explicit_project_beats_env_api_key() {
    let mut c = cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_API_KEY", "k"),
    ]);
    c.project = Some("p".into());
    let (r, log) = resolve_logged(&c);
    let r = r.unwrap();
    assert!(r.auth.is_none() && r.needs_adc);
    assert_eq!(r.location.as_deref(), Some("global"));
    assert!(
        log.contains("The user provided project/location will take precedence over the API key from the environment variable."),
        "{log}"
    );
}

#[test]
fn config_vertex_api_key_rejected() {
    let mut c = cfg(&[("GOOGLE_GENAI_USE_VERTEXAI", "true")]);
    c.api_key = Some("k".into());
    let err = resolve(&c).err().unwrap();
    assert!(
        err.to_string()
            .contains("API keys are only supported for the Gemini API backend"),
        "{err}"
    );
}

#[test]
fn config_ladder_vertex_env_api_key_only_rejected() {
    let err = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_API_KEY", "k"),
    ]))
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "API keys are only supported for the Gemini API backend; use ADC or credentials for Vertex AI"
    );
}

#[test]
fn config_ladder_vertex_insufficient_auth() {
    let err = resolve(&cfg(&[("GOOGLE_GENAI_USE_VERTEXAI", "true")]))
        .err()
        .unwrap();
    assert!(
        err.to_string()
            .contains("project/location or API key must be set when using Vertex AI backend"),
        "{err}"
    );
}

#[test]
fn config_ladder_gemini_api_rejects_explicit_project() {
    let mut c = cfg(&[("GOOGLE_API_KEY", "k")]);
    c.project = Some("p".into());
    assert!(
        resolve(&c)
            .err()
            .unwrap()
            .to_string()
            .contains("project and location are not supported for Gemini API backend")
    );
}

#[test]
fn config_ladder_gemini_api_rejects_explicit_location() {
    let mut c = cfg(&[("GOOGLE_API_KEY", "k")]);
    c.location = Some("us-central1".into());
    assert!(
        resolve(&c)
            .err()
            .unwrap()
            .to_string()
            .contains("project and location are not supported for Gemini API backend")
    );
}

#[test]
fn config_ladder_gemini_api_ignores_env_project() {
    let r = resolve(&cfg(&[
        ("GOOGLE_API_KEY", "k"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "l"),
    ]))
    .unwrap();
    assert_eq!(r.backend, Backend::GeminiApi);
}

#[test]
fn config_ladder_credentials_and_api_key_exclusive() {
    let mut c = cfg(&[]);
    c.api_key = Some("k".into());
    c.credentials = Some(anon());
    assert!(
        resolve(&c)
            .err()
            .unwrap()
            .to_string()
            .contains("credentials and API key are mutually exclusive")
    );
}

#[test]
fn config_ladder_base_url_env_overrides() {
    let r = resolve(&cfg(&[
        ("GOOGLE_API_KEY", "k"),
        ("GOOGLE_GEMINI_BASE_URL", "http://localhost:9/"),
    ]))
    .unwrap();
    assert_eq!(r.base_url, "http://localhost:9/");
}

#[test]
fn config_ladder_base_url_gets_trailing_slash() {
    let r = resolve(&cfg(&[
        ("GOOGLE_API_KEY", "k"),
        ("GOOGLE_GEMINI_BASE_URL", "http://localhost:9"),
    ]))
    .unwrap();
    assert_eq!(r.base_url, "http://localhost:9/");
}

#[test]
fn config_ladder_explicit_base_url_beats_env() {
    let mut c = cfg(&[
        ("GOOGLE_API_KEY", "k"),
        ("GOOGLE_GEMINI_BASE_URL", "http://env/"),
    ]);
    c.http_options.base_url = Some("http://explicit/".into());
    assert_eq!(resolve(&c).unwrap().base_url, "http://explicit/");
}

#[test]
fn config_ladder_vertex_base_url_env() {
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ("GOOGLE_VERTEX_BASE_URL", "http://vertex.local/"),
        ("GOOGLE_GEMINI_BASE_URL", "http://gemini.local/"),
    ]))
    .unwrap();
    assert_eq!(r.base_url, "http://vertex.local/");
    // Explicit project/location from env are sufficient auth only when explicit; env values are
    // cleared under a custom base URL, which also skips ADC.
    assert!(r.project.is_none() && r.location.is_none() && !r.needs_adc);
}

#[test]
fn config_ladder_custom_base_url_skips_adc() {
    let mut c = cfg(&[("GOOGLE_GENAI_USE_VERTEXAI", "true")]);
    c.http_options.base_url = Some("http://localhost:9/".into());
    let r = resolve(&c).unwrap();
    assert!(!r.needs_adc && r.auth.is_none());
    assert!(r.location.is_none());
    let r = resolve(&cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
        ("GOOGLE_CLOUD_LOCATION", "l"),
    ]))
    .unwrap();
    assert!(r.needs_adc);
}

#[test]
fn config_ladder_custom_base_url_with_explicit_project_keeps_adc() {
    let mut c = cfg(&[("GOOGLE_GENAI_USE_VERTEXAI", "true")]);
    c.http_options.base_url = Some("http://localhost:9/".into());
    c.project = Some("p".into());
    c.location = Some("l".into());
    let r = resolve(&c).unwrap();
    assert_eq!(
        (r.project.as_deref(), r.location.as_deref()),
        (Some("p"), Some("l"))
    );
    assert!(r.needs_adc);
}

#[test]
fn config_ladder_http_client_skips_adc() {
    struct Nop;
    #[async_trait::async_trait]
    impl gadk_model::HttpClient for Nop {
        async fn send(
            &self,
            _req: http::Request<bytes::Bytes>,
        ) -> Result<http::Response<gadk_model::BodyStream>, gadk_model::ModelError> {
            unreachable!()
        }
    }
    let mut c = cfg(&[
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("GOOGLE_CLOUD_PROJECT", "p"),
    ]);
    c.http_client = Some(Arc::new(Nop));
    let r = resolve(&c).unwrap();
    assert!(!r.needs_adc && r.auth.is_none());
}

#[test]
fn config_ladder_api_version_defaults_and_override() {
    let mut c = cfg(&[("GOOGLE_API_KEY", "k")]);
    c.http_options.api_version = Some("v1".into());
    let r = resolve(&c).unwrap();
    assert_eq!(r.api_version, "v1");
    assert_eq!(r.http_options.api_version.as_deref(), Some("v1"));
    assert_eq!(
        r.http_options.base_url.as_deref(),
        Some("https://generativelanguage.googleapis.com/")
    );
}

#[test]
fn config_ladder_auth_tokens_rejected() {
    let err = resolve(&cfg(&[("GOOGLE_API_KEY", "auth_tokens/abc")]))
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "ephemeral auth_tokens keys are not supported"
    );
}

#[test]
fn config_ladder_empty_explicit_values_are_unset() {
    let mut c = cfg(&[("GOOGLE_API_KEY", "k")]);
    c.api_key = Some(String::new());
    c.project = Some(String::new());
    let r = resolve(&c).unwrap();
    assert!(matches!(r.auth, Some(Auth::ApiKey(k)) if k == "k"));
}

#[test]
fn config_scope_constant() {
    assert_eq!(
        CLOUD_PLATFORM_SCOPE,
        "https://www.googleapis.com/auth/cloud-platform"
    );
}

#[test]
fn config_map_env_lookup() {
    let env = MapEnv(HashMap::from([("A".to_string(), "1".to_string())]));
    assert_eq!(env.var("A").as_deref(), Some("1"));
    assert_eq!(env.var("B"), None);
}

#[test]
fn config_debug_redacts_header_values() {
    let mut c = cfg(&[("GOOGLE_API_KEY", "k")]);
    c.http_options.headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer secret-token"),
    );
    let dbg = format!("{c:?}");
    assert!(!dbg.contains("secret-token"), "{dbg}");
    assert!(dbg.contains("authorization"), "{dbg}");

    let r = resolve(&c).unwrap();
    let dbg = format!("{r:?}");
    assert!(!dbg.contains("secret-token"), "{dbg}");
    assert!(dbg.contains("authorization"), "{dbg}");
    assert!(dbg.contains("<redacted>"), "{dbg}");
}
