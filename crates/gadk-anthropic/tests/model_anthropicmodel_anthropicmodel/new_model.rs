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

//! Ports of TestNewModel_AnthropicCredentialsAndConfig, TestNewModel_VertexAndModelNames.
//!
//! Each Go `t.Run` subtest is a private `async fn` named after it, called in Go's order from
//! the test function; each table row or loop item is a named case inside it. Go's
//! `t.Setenv(...)` becomes the `ClientConfig::env` of each call ([`env`], [`empty_env`],
//! [`adc_env`]), so nothing touches the process environment or the real ADC (ruling R-b).
//!
//! Go's `RequestOptions`, `TokenSource` and `*google.Credentials` are not ported (spec
//! Divergences): `option.WithBaseURL`/`WithConfig` map to `ClientConfig::base_url`,
//! `option.WithHeader` to `ClientConfig::headers`, `option.WithHTTPClient` to
//! `ClientConfig::http_client`, and `TokenSource` to `ClientConfig::credentials`
//! ([`static_token_credentials`]). Rows with no gadk counterpart are listed, with the reason,
//! where Go has them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use gadk_anthropic::{
    AnthropicError, AnthropicModel, Backend, ClientConfig, DEFAULT_MAX_TOKENS, Effort, ErrorKind,
    PromptCacheTtl,
};
use gadk_genai::GenerateContentConfig;
use gadk_model::LlmRequest;
use http::{HeaderMap, HeaderValue};
use pretty_assertions::assert_eq;

use crate::harness::{
    FakeResponse, FakeServer, MODEL, ScriptedClient, adc_authorized_user_file,
    adc_authorized_user_file_with_token_uri, adc_env, anthropic_error, empty_env, env, generate,
    generate_err, generate_ok, new_model, static_token_credentials, text_message_json,
    text_request,
};

/// Go's `anthropicmodel.ErrInvalidConfig` prefix.
const INVALID: &str = "anthropicmodel: invalid configuration: ";

/// `AnthropicModel::new(name, cfg)`'s error; panics when it succeeds.
async fn new_err(name: &str, cfg: ClientConfig) -> AnthropicError {
    match AnthropicModel::new(name, cfg).await {
        Ok(_) => panic!("NewModel({name:?}) succeeded, want an error"),
        Err(e) => e,
    }
}

/// Go `errors.Is(err, ErrInvalidConfig)` plus the whole Go `Error()` text
/// (`anthropicmodel: invalid configuration: <detail>`).
#[track_caller]
fn assert_invalid_config(case: &str, err: &AnthropicError, detail: &str) {
    assert!(
        err.is(ErrorKind::InvalidConfig),
        "{case}: error = {err}, want ErrInvalidConfig"
    );
    assert_eq!(err.to_string(), format!("{INVALID}{detail}"), "{case}");
}

/// The Go handlers' non-streaming reply (`msg_1`, `ok`, 5/1 tokens).
fn ok_reply() -> String {
    text_message_json("msg_1", "ok", 5, 1)
}

/// The host of a request URL (Go `r.URL.Host` without the port).
fn host(url: &str) -> String {
    url.parse::<http::Uri>()
        .unwrap()
        .host()
        .unwrap_or_default()
        .to_string()
}

// ------------------------------------------------------------ TestNewModel_AnthropicCredentialsAndConfig

// parity: adapted: Go's cancelled ctx, RequestOptions, negative ints and transport reflection have no gadk input; those rows use drop, base_url/headers/http_client and an observed 10-minute timeout (see each subtest)
#[tokio::test]
async fn test_new_model_anthropic_credentials_and_config() {
    missing_credentials_fails().await;
    canceled_context_fails().await;
    env_api_key_and_precedence().await;
    env_credentials_without_base_url_and_anthropic_base_url_env().await;
    request_options_body_and_environment_mutations_work_with_env_credentials().await;
    explicit_auth_token().await;
    invalid_client_configs().await;
}

/// Go `t.Run("missing_credentials_fails")`.
async fn missing_credentials_fails() {
    let err = new_err(
        MODEL,
        ClientConfig {
            env: empty_env(),
            ..Default::default()
        },
    )
    .await;
    assert!(
        err.is(ErrorKind::InvalidConfig) && err.is(ErrorKind::Authentication),
        "missing_credentials_fails: NewModel() error = {err}, want wrapping ErrInvalidConfig and ErrAuthentication"
    );
    assert_eq!(
        err.to_string(),
        "anthropicmodel: invalid configuration: anthropicmodel: authentication failed: no Anthropic credential found; set ClientConfig.api_key, ClientConfig.auth_token, ANTHROPIC_API_KEY, or ANTHROPIC_AUTH_TOKEN"
    );
}

/// Go `t.Run("canceled_context_fails")`.
///
/// parity: adapted: `AnthropicModel::new` takes no context; cancellation is by drop (plan
/// D11). A `new` future and a `generate_content` stream, each dropped before its first poll,
/// send no request even after spawned work has had a chance to run (`settle`), so an eager
/// `tokio::spawn` of the send would be caught.
async fn canceled_context_fails() {
    let client = ScriptedClient::json(ok_reply());
    let cfg = ClientConfig {
        api_key: Some("k".to_string()),
        http_client: Some(client.clone()),
        env: empty_env(),
        ..Default::default()
    };
    drop(AnthropicModel::new(MODEL, cfg.clone()));
    settle().await;
    assert_eq!(client.hits(), 0, "canceled_context_fails: dropped NewModel");

    let m = new_model(MODEL, cfg).await;
    drop(m.generate_content(text_request("hi"), false));
    settle().await;
    assert_eq!(client.hits(), 0, "canceled_context_fails: dropped stream");
}

/// Lets any work spawned by the code under test run before a "nothing was sent" check: on the
/// test's current-thread runtime, yielding hands the scheduler to every queued task, and the
/// short real-time sleep covers spawned work that itself awaits before sending.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// Go `t.Run("env_api_key_and_precedence")`.
async fn env_api_key_and_precedence() {
    let srv = FakeServer::json(ok_reply());
    let env = env(&[
        ("ANTHROPIC_API_KEY", "env-api-key"),
        ("ANTHROPIC_AUTH_TOKEN", "env-auth-token"),
    ]);
    let custom = "custom base_url on Backend::Anthropic requires an explicit api_key or auth_token in ClientConfig; ambient environment credentials are not sent to custom endpoints";

    // Custom BaseURL WITHOUT explicit credential must be rejected even when env vars are set.
    let err = new_err(
        MODEL,
        ClientConfig {
            base_url: Some(srv.url()),
            env: env.clone(),
            ..Default::default()
        },
    )
    .await;
    assert_invalid_config("base_url_without_explicit_credential", &err, custom);

    // Go's RequestOptions loop: `option.WithBaseURL(u)` and
    // `option.WithConfig(&config.Config{BaseURL: u})` both set the base URL, which gadk takes
    // only from `ClientConfig::base_url`; the five Go options cover these three URLs.
    // parity: adapted: request_options_base_url rows use ClientConfig::base_url (no RequestOptions)
    for (case, url) in [
        ("request_options_with_base_url_server", srv.url()),
        (
            "request_options_with_base_url_api_anthropic_com",
            "https://api.anthropic.com".to_string(),
        ),
        (
            "request_options_with_base_url_api_anthropic_com_custom_prefix",
            "https://api.anthropic.com/custom-prefix".to_string(),
        ),
    ] {
        let err = new_err(
            MODEL,
            ClientConfig {
                base_url: Some(url),
                env: env.clone(),
                ..Default::default()
            },
        )
        .await;
        assert_invalid_config(case, &err, custom);
    }

    // Explicit APIKey with custom BaseURL succeeds and uses explicit APIKey over env.
    let m = new_model(
        "anthropic/claude-sonnet-4-5-20250929",
        ClientConfig {
            api_key: Some("explicit-api-key".to_string()),
            base_url: Some(srv.url()),
            env: env.clone(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(m.name(), "anthropic/claude-sonnet-4-5-20250929");
    generate_ok(m.as_ref(), text_request("hi"), false).await;
    let req = srv.last_request();
    assert_eq!(req.header("X-Api-Key"), "explicit-api-key");
    assert_eq!(req.header("Authorization"), "");
    // Spec R3: an unset `max_tokens` sends DEFAULT_MAX_TOKENS (Go DefaultMaxTokens = 8192).
    assert_eq!(DEFAULT_MAX_TOKENS, 8192);
    assert_eq!(
        req.json()["max_tokens"],
        serde_json::json!(DEFAULT_MAX_TOKENS)
    );
}

/// Go `t.Run("env_credentials_without_base_url_and_anthropic_base_url_env")`.
async fn env_credentials_without_base_url_and_anthropic_base_url_env() {
    let client = ScriptedClient::json(ok_reply());

    // 1. Both ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN set -> ANTHROPIC_API_KEY wins.
    let m = new_model(
        MODEL,
        ClientConfig {
            http_client: Some(client.clone()),
            env: env(&[
                ("ANTHROPIC_API_KEY", "env-api-key"),
                ("ANTHROPIC_AUTH_TOKEN", "env-auth-token"),
                ("ANTHROPIC_BASE_URL", ""),
            ]),
            ..Default::default()
        },
    )
    .await;
    generate_ok(m.as_ref(), text_request("hi"), false).await;
    let req = client.last_request();
    assert_eq!(
        (req.header("X-Api-Key"), req.header("Authorization")),
        ("env-api-key".to_string(), String::new()),
        "api_key_env_wins"
    );

    // 2. Only ANTHROPIC_AUTH_TOKEN and ANTHROPIC_BASE_URL set -> Bearer header and env base URL used.
    let m2 = new_model(
        MODEL,
        ClientConfig {
            http_client: Some(client.clone()),
            env: env(&[
                ("ANTHROPIC_API_KEY", ""),
                ("ANTHROPIC_AUTH_TOKEN", "env-auth-token"),
                ("ANTHROPIC_BASE_URL", "https://gateway.internal.example.com"),
            ]),
            ..Default::default()
        },
    )
    .await;
    generate_ok(m2.as_ref(), text_request("hi"), false).await;
    let req = client.last_request();
    assert_eq!(
        (req.header("X-Api-Key"), req.header("Authorization")),
        (String::new(), "Bearer env-auth-token".to_string()),
        "auth_token_and_base_url_env"
    );
    assert_eq!(host(&req.url), "gateway.internal.example.com");

    // 3. Invalid ANTHROPIC_BASE_URL must fail with ErrInvalidConfig naming ANTHROPIC_BASE_URL.
    let err = new_err(
        MODEL,
        ClientConfig {
            http_client: Some(client.clone()),
            env: env(&[
                ("ANTHROPIC_API_KEY", "env-api-key"),
                ("ANTHROPIC_AUTH_TOKEN", "env-auth-token"),
                ("ANTHROPIC_BASE_URL", "localhost:8080"),
            ]),
            ..Default::default()
        },
    )
    .await;
    assert_invalid_config(
        "invalid_anthropic_base_url_env",
        &err,
        "ANTHROPIC_BASE_URL must be a valid http or https URL with a host",
    );
    assert_eq!(client.hits(), 2, "NewModel sends no request");
}

/// Go `t.Run("request_options_body_and_environment_mutations_work_with_env_credentials")`.
///
/// parity: adapted: of Go's four request options only `option.WithHeader` has a gadk
/// counterpart (`ClientConfig::headers`). `WithEnvironmentProduction` is the default base URL
/// (asserted through the request host); `WithJSONDel("top_k")` and
/// `WithJSONSet("custom_flag", true)` (raw body mutation) are not ported, so the body keeps
/// `top_k` and gains no `custom_flag`.
async fn request_options_body_and_environment_mutations_work_with_env_credentials() {
    let client = ScriptedClient::json(ok_reply());
    let mut headers = HeaderMap::new();
    headers.insert("X-Custom-Opt", HeaderValue::from_static("opt-val"));
    let m = new_model(
        MODEL,
        ClientConfig {
            http_client: Some(client.clone()),
            headers,
            env: env(&[
                ("ANTHROPIC_API_KEY", "env-api-key"),
                ("ANTHROPIC_BASE_URL", ""),
            ]),
            ..Default::default()
        },
    )
    .await;
    generate_ok(
        m.as_ref(),
        LlmRequest {
            config: Some(GenerateContentConfig {
                top_k: Some(10.0),
                ..Default::default()
            }),
            ..text_request("hi")
        },
        false,
    )
    .await;
    let req = client.last_request();
    assert_eq!(req.header("X-Custom-Opt"), "opt-val");
    assert_eq!(req.header("X-Api-Key"), "env-api-key");
    assert_eq!(host(&req.url), "api.anthropic.com");
    let body = req.json();
    assert_eq!(body["top_k"], serde_json::json!(10));
    assert_eq!(body.get("custom_flag"), None);
}

/// Go `t.Run("explicit_auth_token")`.
async fn explicit_auth_token() {
    let srv = FakeServer::json(ok_reply());
    let m = new_model(
        MODEL,
        ClientConfig {
            auth_token: Some("my-bearer-token".to_string()),
            base_url: Some(srv.url()),
            env: empty_env(),
            ..Default::default()
        },
    )
    .await;
    generate_ok(m.as_ref(), text_request("hi"), false).await;
    assert_eq!(
        srv.last_request().header("Authorization"),
        "Bearer my-bearer-token"
    );
}

/// Go `t.Run("invalid_client_configs")`: the table, then its two nested subtests.
async fn invalid_client_configs() {
    let key = || ClientConfig {
        api_key: Some("k".to_string()),
        env: empty_env(),
        ..Default::default()
    };
    let resource = "projects/p/locations/l/publishers/anthropic/models/claude-sonnet-4-5-20250929";

    let cases: Vec<(&str, &str, ClientConfig, &str)> = vec![
        (
            "empty_model_name",
            "   ",
            key(),
            "model name must not be empty",
        ),
        (
            "invalid_model_slash",
            "foo/bar/baz",
            key(),
            "invalid model name",
        ),
        (
            "vertex_resource_on_anthropic_backend",
            resource,
            ClientConfig {
                backend: Backend::Anthropic,
                ..key()
            },
            "Vertex resource model name requires Backend::Vertex",
        ),
        (
            "invalid_base_url_scheme",
            MODEL,
            ClientConfig {
                base_url: Some("ftp://example.com".to_string()),
                ..key()
            },
            "base_url must be a valid http or https URL with a host",
        ),
        (
            "whitespace_base_url",
            MODEL,
            ClientConfig {
                base_url: Some("   ".to_string()),
                ..key()
            },
            "base_url must not be whitespace-only",
        ),
        // parity: adapted: invalid_request_option_base_url: `option.WithBaseURL("ftp://…")`
        // is `ClientConfig::base_url` in gadk; Go's "BaseURL in RequestOptions" source name
        // becomes "BaseURL".
        (
            "invalid_request_option_base_url",
            MODEL,
            ClientConfig {
                base_url: Some("ftp://example.com".to_string()),
                ..key()
            },
            "base_url must be a valid http or https URL with a host",
        ),
    ];
    for (case, name, cfg, detail) in cases {
        let err = new_err(name, cfg).await;
        assert_invalid_config(case, &err, detail);
    }

    // parity: adapted: unknown_backend, invalid_effort, invalid_prompt_cache_ttl: `Backend`,
    // `Effort` and `PromptCacheTtl` are enums, so Go's invalid strings fail where gadk parses
    // them (`FromStr`, Go's text) instead of in `NewModel`.
    assert_invalid_config(
        "unknown_backend",
        &"bedrock".parse::<Backend>().unwrap_err(),
        r#"unsupported Backend "bedrock"; valid values are "anthropic", "vertex""#,
    );
    assert_invalid_config(
        "invalid_effort",
        &"extreme".parse::<Effort>().unwrap_err(),
        r#"unsupported effort "extreme"; valid values are low, medium, high, xhigh, max"#,
    );
    assert_invalid_config(
        "invalid_prompt_cache_ttl",
        &"10m".parse::<PromptCacheTtl>().unwrap_err(),
        r#"unsupported prompt_cache.ttl "10m"; valid values are 5m, 1h"#,
    );
    // parity: adapted: negative_max_tokens, negative_max_retries: `max_tokens`/`max_retries`
    // are `Option<u32>`, so a negative value does not compile; nothing is left to run.
    // parity: adapted: nil_request_option: there are no RequestOptions, so no nil option can
    // be passed; nothing is left to run.

    unparsable_request_option_base_url_does_not_retry_or_leak_userinfo().await;
    default_http_client_sets_response_header_timeout().await;
}

/// Go `t.Run("unparsable_request_option_base_url_does_not_retry_or_leak_userinfo")`.
///
/// parity: adapted: Go's options become `ClientConfig { base_url, max_retries: Some(3) }`;
/// `WithRequestTimeout` has no `ClientConfig` counterpart (the per-attempt timeout is
/// `LlmRequest::http_options`), and the error names `BaseURL` instead of Go's "BaseURL in
/// RequestOptions".
async fn unparsable_request_option_base_url_does_not_retry_or_leak_userinfo() {
    // 1. A valid base URL with MaxRetries(3) builds at once: NewModel sends no probe request.
    let probe = tokio::time::timeout(
        Duration::from_millis(200),
        AnthropicModel::new(
            MODEL,
            ClientConfig {
                api_key: Some("k".to_string()),
                base_url: Some("https://gateway.example.com".to_string()),
                max_retries: Some(3),
                env: empty_env(),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("NewModel(valid base_url + max_retries 3) took longer than 200ms");
    if let Err(e) = probe {
        panic!("NewModel(valid base_url + max_retries 3) error = {e}, want nil");
    }

    // 2. An unparsable base URL fails immediately with a fixed message without leaking userinfo.
    let start = std::time::Instant::now();
    let err = new_err(
        MODEL,
        ClientConfig {
            api_key: Some("k".to_string()),
            base_url: Some("https://user:s3cret@gateway.example:badport/".to_string()),
            max_retries: Some(3),
            env: empty_env(),
            ..Default::default()
        },
    )
    .await;
    let elapsed = start.elapsed();
    assert_invalid_config(
        "unparsable_base_url",
        &err,
        "base_url must be a valid http or https URL with a host",
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "NewModel() took {elapsed:?}, want < 500ms"
    );
    let msg = err.to_string();
    assert!(
        !msg.contains("s3cret") && !msg.contains("gateway.example"),
        "error = {msg:?} leaked raw URL or userinfo credentials"
    );
}

/// Go `t.Run("default_http_client_sets_response_header_timeout")`.
///
/// parity: adapted: Go reflects into the SDK client to read the default transport's
/// `ResponseHeaderTimeout`; gadk's transport is private, so the timeout is observed instead.
/// With tokio time paused, a request to a socket that never answers fails after exactly 10
/// virtual minutes with Go's `net/http: timeout awaiting response headers`; Go's "custom
/// transport is not mutated" check becomes "a caller-supplied `http_client` gets no such
/// timeout" (the same request still pending after an hour).
async fn default_http_client_sets_response_header_timeout() {
    // Accepts connections (kernel backlog) and never answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_url = format!("http://{}", silent.local_addr().unwrap());

    tokio::time::pause();

    let m = new_model(
        MODEL,
        ClientConfig {
            api_key: Some("test-key".to_string()),
            base_url: Some(silent_url),
            max_retries: Some(0),
            env: empty_env(),
            ..Default::default()
        },
    )
    .await;
    let start = tokio::time::Instant::now();
    let err = generate_err(m.as_ref(), text_request("hi"), false).await;
    let elapsed = start.elapsed();
    assert_eq!(
        anthropic_error(&err).to_string(),
        "anthropicmodel: transport error: net/http: timeout awaiting response headers"
    );
    assert!(
        elapsed >= Duration::from_secs(600) && elapsed < Duration::from_secs(601),
        "default client response-header timeout = {elapsed:?}, want 10m"
    );

    let client = ScriptedClient::always(FakeResponse::never());
    let m = new_model(
        MODEL,
        ClientConfig {
            api_key: Some("test-key".to_string()),
            http_client: Some(client.clone()),
            max_retries: Some(0),
            env: empty_env(),
            ..Default::default()
        },
    )
    .await;
    let pending = tokio::time::timeout(
        Duration::from_secs(3600),
        generate(m.as_ref(), text_request("hi"), false),
    )
    .await;
    assert!(
        pending.is_err(),
        "a caller-supplied http_client must get no response-header timeout"
    );
    assert_eq!(client.hits(), 1);

    tokio::time::resume();
    drop(silent);
}

// ---------------------------------------------------------------- TestNewModel_VertexAndModelNames

// parity: adapted: TokenSource is ClientConfig::credentials, RequestOptions rows use base_url/http_client, ADC uses a temp file via cfg.env (R-b); rows with no gadk input are noted in each subtest
#[tokio::test]
async fn test_new_model_vertex_and_model_names() {
    resource_name_resolution_and_headers().await;
    vertex_validation_and_base_url_guard().await;
    adc_credentials_detach_init_context().await;
    http_client_and_request_options_authentication().await;
}

/// Go's `TestNewModel_VertexAndModelNames` env: `GOOGLE_CLOUD_PROJECT`, `CLOUD_ML_PROJECT_ID`,
/// `GOOGLE_CLOUD_LOCATION`, `CLOUD_ML_REGION` all empty, and `ts` = `staticTokenSource
/// {token: "vertex-token"}`.
fn vertex_cfg() -> ClientConfig {
    ClientConfig {
        backend: Backend::Vertex,
        credentials: Some(static_token_credentials("vertex-token")),
        env: env(&[
            ("GOOGLE_CLOUD_PROJECT", ""),
            ("CLOUD_ML_PROJECT_ID", ""),
            ("GOOGLE_CLOUD_LOCATION", ""),
            ("CLOUD_ML_REGION", ""),
        ]),
        ..Default::default()
    }
}

/// Go `t.Run("resource_name_resolution_and_headers")`.
///
/// parity: adapted: `x-goog-api-client` and `user-agent` carry gadk's
/// `gadk_platform::tracking_header()` (`gadk-rust/<v> rust/<v>`) instead of Go's
/// `google-adk/<v> gl-go/<v>` (spec Divergences).
async fn resource_name_resolution_and_headers() {
    let srv = FakeServer::json(text_message_json("msg_v", "ok", 4, 1));
    let resource_name = "projects/res-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929";
    let m = new_model(
        resource_name,
        ClientConfig {
            base_url: Some(srv.url()),
            ..vertex_cfg()
        },
    )
    .await;
    generate_ok(m.as_ref(), text_request("ping"), false).await;

    let req = srv.last_request();
    assert_eq!(
        req.path,
        "/v1/projects/res-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929:rawPredict"
    );
    assert_eq!(
        req.json()["anthropic_version"],
        serde_json::json!("vertex-2023-10-16")
    );
    let tracking = gadk_platform::tracking_header();
    assert_eq!(req.header("X-Goog-Api-Client"), tracking);
    assert_eq!(req.header("User-Agent"), tracking);
    assert_eq!(req.header("Authorization"), "Bearer vertex-token");

    // Request-level resource name with mismatched project or location must fail before network call.
    for (case, req_model, detail) in [
        (
            "mismatched_request_project",
            "projects/other-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929",
            r#"request model project "other-proj" does not match configured project "res-proj""#,
        ),
        (
            "mismatched_request_location",
            "projects/res-proj/locations/europe-west1/publishers/anthropic/models/claude-sonnet-4-5-20250929",
            r#"request model location "europe-west1" does not match configured location "us-east5""#,
        ),
    ] {
        let results = generate(
            m.as_ref(),
            LlmRequest {
                model: req_model.to_string(),
                ..text_request("ping")
            },
            false,
        )
        .await;
        assert_eq!(results.len(), 1, "{case}: one error");
        let err = results.into_iter().next().unwrap().err().unwrap();
        assert_invalid_config(case, anthropic_error(&err), detail);
    }
    assert_eq!(srv.hits(), 1, "mismatched request models send nothing");
}

/// Go `t.Run("vertex_validation_and_base_url_guard")`.
async fn vertex_validation_and_base_url_guard() {
    let resource_a =
        "projects/proj-a/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929";
    let with = |project: Option<&str>, location: Option<&str>| ClientConfig {
        project: project.map(str::to_string),
        location: location.map(str::to_string),
        ..vertex_cfg()
    };

    let cases: Vec<(&str, &str, ClientConfig, &str)> = vec![
        // Mismatch between ClientConfig.Project and resource name in NewModel.
        (
            "mismatched_project",
            resource_a,
            with(Some("proj-b"), Some("us-east5")),
            r#"ClientConfig.project "proj-b" does not match resource project "proj-a""#,
        ),
        // Mismatch between ClientConfig.Location and resource name in NewModel.
        (
            "mismatched_location",
            resource_a,
            with(Some("proj-a"), Some("global")),
            r#"ClientConfig.location "global" does not match resource location "us-east5""#,
        ),
        // Missing project or location.
        (
            "missing_project",
            MODEL,
            with(None, Some("us-east5")),
            "Backend::Vertex requires a project via ClientConfig.project, a full resource model name, GOOGLE_CLOUD_PROJECT, or CLOUD_ML_PROJECT_ID",
        ),
        (
            "missing_location",
            MODEL,
            with(Some("proj-a"), None),
            "Backend::Vertex requires a location via ClientConfig.location, a full resource model name, GOOGLE_CLOUD_LOCATION, or CLOUD_ML_REGION",
        ),
    ];
    for (case, name, cfg, detail) in cases {
        let err = new_err(name, cfg).await;
        assert_invalid_config(case, &err, detail);
    }

    // Custom BaseURL on BackendVertex without explicit Credentials must fail even when ambient
    // Application Default Credentials are present (a temp ADC file via cfg.env, ruling R-b).
    let adc = adc_authorized_user_file();
    let custom = "custom base_url on Backend::Vertex requires explicit ClientConfig.credentials; Application Default Credentials are not sent to custom endpoints";
    // parity: adapted: Go's RequestOptions rows (`WithBaseURL` custom-proxy and
    // api.anthropic.com, `WithConfig` api.anthropic.com) use ClientConfig::base_url.
    for (case, url) in [
        (
            "base_url_without_explicit_credentials",
            "https://custom-proxy.example.com",
        ),
        (
            "request_options_with_base_url_custom_proxy",
            "https://custom-proxy.example.com",
        ),
        (
            "request_options_with_base_url_api_anthropic_com",
            "https://api.anthropic.com",
        ),
        (
            "request_options_with_config_api_anthropic_com",
            "https://api.anthropic.com",
        ),
    ] {
        let err = new_err(
            MODEL,
            ClientConfig {
                project: Some("proj-a".to_string()),
                location: Some("us-east5".to_string()),
                base_url: Some(url.to_string()),
                credentials: None,
                env: adc_env(&adc.path_string()),
                ..vertex_cfg()
            },
        )
        .await;
        assert_invalid_config(case, &err, custom);
    }

    // Invalid Vertex Location or Project (e.g. URL injection characters) must fail; then a
    // malformed resource name.
    let cases: Vec<(&str, &str, ClientConfig, &str)> = vec![
        (
            "malicious_location_in_resource_name",
            "projects/proj-a/locations/attacker.example#/publishers/anthropic/models/claude-sonnet-4-5-20250929",
            vertex_cfg(),
            r#"invalid Vertex location "attacker.example#"; expected a lowercase region such as "us-east5" or "global""#,
        ),
        (
            "malicious_client_config_location",
            MODEL,
            with(Some("proj-a"), Some("attacker.example#")),
            r#"invalid Vertex location "attacker.example#"; expected a lowercase region such as "us-east5" or "global""#,
        ),
        (
            "malicious_client_config_project",
            MODEL,
            with(Some("bad#project"), Some("us-east5")),
            r#"invalid Vertex project "bad#project""#,
        ),
        // parity: adapted: credentials_with_nil_token_source: a google_cloud_auth
        // `Credentials` always wraps a provider, so Go's `&google.Credentials{}` (nil
        // TokenSource) cannot be built; nothing is left to run.
        (
            "malformed_resource_name",
            "projects/proj-a/locations/us-east5/models/claude",
            vertex_cfg(),
            "invalid Vertex model resource name format; expected projects/<project>/locations/<location>/publishers/anthropic/models/<model>",
        ),
    ];
    for (case, name, cfg, detail) in cases {
        let err = new_err(name, cfg).await;
        assert_invalid_config(case, &err, detail);
    }
}

/// Go `t.Run("adc_credentials_detach_init_context")`.
///
/// parity: adapted: `new` takes no context to cancel; the detached-refresh property is
/// observed instead: building the model fetches no token, and the first request refreshes
/// it from the ADC file's `token_uri` (a temp file via `cfg.env`, ruling R-b).
async fn adc_credentials_detach_init_context() {
    let token_srv = FakeServer::json(
        r#"{"access_token":"adc-refreshed-token","token_type":"Bearer","expires_in":3600}"#,
    );
    let adc = adc_authorized_user_file_with_token_uri(&token_srv.url());
    let api = ScriptedClient::json(text_message_json("msg_adc", "ok", 4, 1));

    let m = new_model(
        MODEL,
        ClientConfig {
            backend: Backend::Vertex,
            project: Some("proj-a".to_string()),
            location: Some("us-east5".to_string()),
            http_client: Some(api.clone()),
            env: adc_env(&adc.path_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(token_srv.hits(), 0, "NewModel must not fetch a token");

    generate_ok(m.as_ref(), text_request("ping"), false).await;
    assert_eq!(
        api.last_request().header("Authorization"),
        "Bearer adc-refreshed-token"
    );
    assert_eq!(token_srv.hits(), 1);
}

/// Go `t.Run("http_client_and_request_options_authentication")`.
///
/// parity: adapted: Go's `option.WithHTTPClient` rows become `ClientConfig::http_client`.
/// Go's row 4 (Vertex `WithHTTPClient` replaces the OAuth-wrapped client, so no
/// `Authorization`) has no gadk input: gadk always adds the Vertex credential headers to the
/// request it hands any `http_client` (row 3); nothing is left to run for it.
async fn http_client_and_request_options_authentication() {
    let dummy = || text_message_json("msg_dummy", "ok", 1, 1);

    // 1. BackendAnthropic with ClientConfig.HTTPClient receives APIKey.
    // 2. BackendAnthropic with option.WithHTTPClient in RequestOptions receives APIKey.
    for case in [
        "anthropic_http_client",
        "anthropic_request_options_http_client",
    ] {
        let client = ScriptedClient::json(dummy());
        let m = new_model(
            MODEL,
            ClientConfig {
                backend: Backend::Anthropic,
                api_key: Some("test-anthropic-key".to_string()),
                http_client: Some(client.clone()),
                env: empty_env(),
                ..Default::default()
            },
        )
        .await;
        generate_ok(m.as_ref(), text_request("ping"), false).await;
        assert_eq!(
            client.last_request().header("X-Api-Key"),
            "test-anthropic-key",
            "{case}"
        );
    }

    // 3. BackendVertex with ClientConfig.HTTPClient receives the credentials' token.
    let client = ScriptedClient::json(dummy());
    let m = new_model(
        MODEL,
        ClientConfig {
            project: Some("proj-a".to_string()),
            location: Some("us-east5".to_string()),
            http_client: Some(client.clone()),
            ..vertex_cfg()
        },
    )
    .await;
    generate_ok(m.as_ref(), text_request("ping"), false).await;
    assert_eq!(
        client.last_request().header("Authorization"),
        "Bearer vertex-token",
        "vertex_http_client"
    );
}
