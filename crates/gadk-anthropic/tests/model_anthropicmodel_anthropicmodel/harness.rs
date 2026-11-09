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

//! Shared helpers for the anthropicmodel parity tests (Task 6): tracing capture, Anthropic
//! JSON builders, sample PNG, model builders; the fake HTTP server, request recorder and SSE
//! builders are `gadk_model::rest::test_util`'s, re-exported here.
//!
//! Go equivalents:
//! - `httptest.NewServer(handler)` → [`FakeServer::start`] / [`FakeServer::start_async`]
//!   (a real HTTP/1 server on `127.0.0.1`, so the model's own `reqwest` client is exercised);
//! - `roundTripFunc` behind `http.Client` → [`ScriptedClient`] (an in-process
//!   `HttpClient`, set as `ClientConfig::http_client`);
//! - `slogCaptureHandler` → [`LogCapture`] (a `tracing` layer; events render as
//!   `msg key=value …`, plan D10);
//! - `staticTokenSource` → [`static_token_credentials`];
//! - `t.TempDir()` + `adc.json` + `t.Setenv("GOOGLE_APPLICATION_CREDENTIALS", …)` →
//!   [`TempFile`], [`adc_authorized_user_file`], [`adc_env`] (ruling R-b: never the real ADC);
//! - `samplePNG1x1` → [`SAMPLE_PNG_1X1`];
//! - `scrubAnthropicRequest` / `scrubAnthropicResponse` / `openHTTPRR` →
//!   [`scrub_anthropic_request`] / [`scrub_anthropic_response`] / [`open_httprr`];
//! - `for resp, err := range m.GenerateContent(...)` → [`generate`], [`generate_last`],
//!   [`generate_err`].
//!
//! Every server and capture is per test: nothing global is installed. The fake server needs a
//! running tokio runtime (`#[tokio::test]`) and real time (do not combine it with
//! `start_paused`).

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_anthropic::{AnthropicError, AnthropicModel, Backend, ClientConfig, MapEnv};
use gadk_httprr::{HttprrError, RecordReplay};
use gadk_model::{HttpClient, Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient};
use google_cloud_auth::credentials::{
    CacheableResource, Credentials, CredentialsProvider, EntityTag,
};
use google_cloud_auth::errors::CredentialsError;
use tracing::field::{Field, Visit};
use tracing::{Dispatch, Event, Level, Subscriber};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

pub use gadk_model::rest::test_util::{
    FakeResponse, FakeServer, ScriptedClient, sse_event, sse_lines,
};

/// The model name every Go test uses.
pub const MODEL: &str = "claude-sonnet-4-5-20250929";

// --------------------------------------------------------------------------------- log capture

/// One captured `tracing` event (Go `capturedLogRecord`).
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedEvent {
    /// Event level (Go `slog.LevelWarn` → `Level::WARN`).
    pub level: Level,
    /// Event target (module path of the emitting code).
    pub target: String,
    /// The message (Go `Record.Message`).
    pub message: String,
    /// Fields in emission order (Go `Attrs`), values rendered like Go `Value.String()`:
    /// strings raw, numbers and bools in decimal/`true`, anything else via `Debug`.
    pub fields: Vec<(String, String)>,
}

impl CapturedEvent {
    /// Go `records[i].Attrs[key]`: the field's value, if present.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `msg key=value key=value …` (plan D10), fields in emission order.
    pub fn render(&self) -> String {
        let mut out = self.message.clone();
        for (k, v) in &self.fields {
            out.push_str(&format!(" {k}={v}"));
        }
        out
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldVisitor {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, value.to_string());
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    target_prefix: Option<String>,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let meta = event.metadata();
        if let Some(prefix) = &self.target_prefix
            && !meta.target().starts_with(prefix.as_str())
        {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.events.lock().unwrap().push(CapturedEvent {
            level: *meta.level(),
            target: meta.target().to_string(),
            message: visitor.message,
            fields: visitor.fields,
        });
    }
}

/// Captures `tracing` events (Go `slogCaptureHandler` passed as `ClientConfig.Logger`).
/// Nothing global is installed: activate it per test with [`set_default`](Self::set_default)
/// (current thread; `#[tokio::test]`'s default runtime polls the model there) or per future
/// with [`capture`](Self::capture).
#[derive(Clone)]
pub struct LogCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    dispatch: Dispatch,
}

impl Default for LogCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl LogCapture {
    /// Captures events whose target starts with `gadk_anthropic` (the model's own warnings,
    /// as Go's per-model logger sees only the model's records).
    pub fn new() -> Self {
        Self::with_target_prefix(Some("gadk_anthropic"))
    }

    /// Captures events whose target starts with `prefix`, or every event for `None`.
    pub fn with_target_prefix(prefix: Option<&str>) -> Self {
        let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::default();
        let layer = CaptureLayer {
            events: events.clone(),
            target_prefix: prefix.map(str::to_string),
        };
        let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));
        Self { events, dispatch }
    }

    /// The subscriber, for `tracing::dispatcher::with_default` or
    /// `tracing::instrument::WithSubscriber::with_subscriber`.
    pub fn dispatch(&self) -> Dispatch {
        self.dispatch.clone()
    }

    /// Makes this capture the current thread's subscriber until the guard drops.
    pub fn set_default(&self) -> tracing::dispatcher::DefaultGuard {
        tracing::dispatcher::set_default(&self.dispatch)
    }

    /// Runs `fut` with this capture as its subscriber (wherever it is polled).
    pub async fn capture<F: Future>(&self, fut: F) -> F::Output {
        use tracing::instrument::WithSubscriber;
        fut.with_subscriber(self.dispatch.clone()).await
    }

    /// Go `Snapshot()`: every event captured so far.
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Only the `WARN` events.
    pub fn warnings(&self) -> Vec<CapturedEvent> {
        self.events()
            .into_iter()
            .filter(|e| e.level == Level::WARN)
            .collect()
    }

    /// Every event rendered as `msg key=value …`.
    pub fn rendered(&self) -> Vec<String> {
        self.events().iter().map(CapturedEvent::render).collect()
    }

    /// Forgets the events captured so far.
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

// ------------------------------------------------------------------------------------- JSON

/// Go's non-streaming text reply literal:
/// `{"id":<id>,"type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929",
/// "stop_reason":"end_turn","content":[{"type":"text","text":<text>}],
/// "usage":{"input_tokens":<in>,"output_tokens":<out>}}` (one line).
pub fn text_message_json(id: &str, text: &str, input_tokens: u64, output_tokens: u64) -> String {
    let id = serde_json::to_string(id).unwrap();
    let text = serde_json::to_string(text).unwrap();
    format!(
        r#"{{"id":{id},"type":"message","role":"assistant","model":"{MODEL}","stop_reason":"end_turn","content":[{{"type":"text","text":{text}}}],"usage":{{"input_tokens":{input_tokens},"output_tokens":{output_tokens}}}}}"#
    )
}

/// Go `json.Compact`: drops insignificant whitespace outside strings; `None` when `data` is
/// not valid JSON (Go returns an error and the caller keeps the original bytes).
pub fn json_compact(data: &[u8]) -> Option<Vec<u8>> {
    serde_json::from_slice::<serde_json::Value>(data).ok()?;
    let mut out = Vec::with_capacity(data.len());
    let (mut in_string, mut escaped) = (false, false);
    for &b in data {
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {}
            b'"' => {
                in_string = true;
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    Some(out)
}

// ------------------------------------------------------------------------------------ fixtures

/// Go `samplePNG1x1`: a valid 1x1 blue PNG (73 bytes).
pub const SAMPLE_PNG_1X1: [u8; 73] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, //
    0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, //
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, //
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, //
    0xde, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, //
    0x54, 0x78, 0x9c, 0x62, 0x62, 0x60, 0xf8, 0x0f, //
    0x08, 0x00, 0x00, 0xff, 0xff, 0x01, 0x0b, 0x01, //
    0x02, 0xd6, 0x95, 0x43, 0xa8, 0x00, 0x00, 0x00, //
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, //
    0x82,
];

/// [`SAMPLE_PNG_1X1`] as `Bytes` (for `Part::from_bytes`).
pub fn sample_png() -> Bytes {
    Bytes::from_static(&SAMPLE_PNG_1X1)
}

/// Go's ADC file contents in `TestNewModel_VertexAndModelNames` (authorized user, no
/// `token_uri`).
pub const ADC_AUTHORIZED_USER_JSON: &str = r#"{"type":"authorized_user","client_id":"test-client","client_secret":"test-secret","refresh_token":"test-refresh"}"#;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_path(suffix: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "gadk-anthropic-test-{}-{nanos}-{n}-{suffix}",
        std::process::id()
    ))
}

/// A file in the system temp directory, deleted on drop (Go `t.TempDir()` + `os.WriteFile`).
#[derive(Debug)]
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// Writes `contents` to a fresh file whose name ends in `suffix` (mode 0600 on Unix).
    pub fn with_contents(suffix: &str, contents: &str) -> Self {
        let path = unique_temp_path(suffix);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path).expect("create temp file");
        std::io::Write::write_all(&mut file, contents.as_bytes()).expect("write temp file");
        Self { path }
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's path as a `String` (for env values).
    pub fn path_string(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// An `adc.json` with [`ADC_AUTHORIZED_USER_JSON`].
pub fn adc_authorized_user_file() -> TempFile {
    TempFile::with_contents("adc.json", ADC_AUTHORIZED_USER_JSON)
}

/// Go's `fmt.Sprintf(`{…,"token_uri":%q}`, tokenSrv.URL)` ADC file (authorized user whose
/// token endpoint is a fake server).
pub fn adc_authorized_user_file_with_token_uri(token_uri: &str) -> TempFile {
    let uri = serde_json::to_string(token_uri).unwrap();
    TempFile::with_contents(
        "adc.json",
        &format!(
            r#"{{"type":"authorized_user","client_id":"test-client","client_secret":"test-secret","refresh_token":"test-refresh","token_uri":{uri}}}"#
        ),
    )
}

/// A path in the temp directory that does not exist (ruling R-b's "no ADC anywhere" form).
pub fn missing_adc_path() -> String {
    unique_temp_path("missing-adc.json")
        .to_string_lossy()
        .into_owned()
}

/// An empty environment: no `ANTHROPIC_*`, `GOOGLE_*` or `CLOUD_ML_*` variables (Go's
/// `t.Setenv(name, "")` resets).
pub fn empty_env() -> Arc<MapEnv> {
    Arc::new(MapEnv(Default::default()))
}

/// An environment with exactly these variables.
pub fn env(vars: &[(&str, &str)]) -> Arc<MapEnv> {
    Arc::new(MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    ))
}

/// An environment whose only variable is `GOOGLE_APPLICATION_CREDENTIALS=<path>`.
pub fn adc_env(path: &str) -> Arc<MapEnv> {
    env(&[("GOOGLE_APPLICATION_CREDENTIALS", path)])
}

// ------------------------------------------------------------------------------ model builders

/// Go `staticTokenSource{token}`: credentials whose headers are
/// `authorization: Bearer <token>`.
pub fn static_token_credentials(token: &str) -> Credentials {
    Credentials::from(StaticToken(format!("Bearer {token}")))
}

#[derive(Debug)]
struct StaticToken(String);

impl CredentialsProvider for StaticToken {
    async fn headers(
        &self,
        _extensions: http::Extensions,
    ) -> Result<CacheableResource<http::HeaderMap>, CredentialsError> {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&self.0).expect("ASCII token"),
        );
        Ok(CacheableResource::New {
            entity_tag: EntityTag::new(),
            data: headers,
        })
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

/// Go `ClientConfig{APIKey: "test-key", BaseURL: srv.URL}` with an empty environment.
pub fn anthropic_config(base_url: &str) -> ClientConfig {
    ClientConfig {
        backend: Backend::Anthropic,
        api_key: Some("test-key".to_string()),
        base_url: Some(base_url.to_string()),
        env: empty_env(),
        ..Default::default()
    }
}

/// Go `ClientConfig{Backend: BackendVertex, Project: "test-project", Location: "us-east5",
/// TokenSource: staticTokenSource{token: "test-token"}, BaseURL: srv.URL}` with an empty
/// environment.
pub fn vertex_config(base_url: &str) -> ClientConfig {
    ClientConfig {
        backend: Backend::Vertex,
        project: Some("test-project".to_string()),
        location: Some("us-east5".to_string()),
        credentials: Some(static_token_credentials("test-token")),
        base_url: Some(base_url.to_string()),
        env: empty_env(),
        ..Default::default()
    }
}

/// `AnthropicModel::new(name, cfg)`, panicking like Go's `t.Fatalf("NewModel() error = %v")`.
pub async fn new_model(name: &str, cfg: ClientConfig) -> Arc<dyn Llm> {
    match AnthropicModel::new(name, cfg).await {
        Ok(m) => m,
        Err(e) => panic!("NewModel() error = {e}"),
    }
}

/// Go `&model.LLMRequest{Contents: genai.Text(s)}`.
pub fn text_request(s: &str) -> LlmRequest {
    LlmRequest {
        contents: gadk_genai::text(s),
        ..Default::default()
    }
}

// --------------------------------------------------------------------------- stream collectors

/// Drains a `generate_content` stream.
pub async fn collect(
    stream: BoxStream<'static, Result<LlmResponse, ModelError>>,
) -> Vec<Result<LlmResponse, ModelError>> {
    stream.collect().await
}

/// Go's `for resp, err := range m.GenerateContent(ctx, req, stream)`: every item, in order.
pub async fn generate(
    m: &dyn Llm,
    req: LlmRequest,
    stream: bool,
) -> Vec<Result<LlmResponse, ModelError>> {
    collect(m.generate_content(req, stream)).await
}

/// `"<case>: "`, or `""` for an unlabelled call: the prefix of a helper's panic message.
fn case_prefix(case: &str) -> String {
    if case.is_empty() {
        String::new()
    } else {
        format!("{case}: ")
    }
}

/// Every response; panics on the first error (Go `t.Fatalf("GenerateContent() error = %v")`).
pub async fn generate_ok(m: &dyn Llm, req: LlmRequest, stream: bool) -> Vec<LlmResponse> {
    generate_ok_in("", m, req, stream).await
}

/// [`generate_ok`] whose panic message starts with `case`.
pub async fn generate_ok_in(
    case: &str,
    m: &dyn Llm,
    req: LlmRequest,
    stream: bool,
) -> Vec<LlmResponse> {
    let prefix = case_prefix(case);
    generate(m, req, stream)
        .await
        .into_iter()
        .map(|r| r.unwrap_or_else(|e| panic!("{prefix}GenerateContent() error = {e}")))
        .collect()
}

/// The last response (Go `got = resp`); panics on any error or an empty stream.
pub async fn generate_last(m: &dyn Llm, req: LlmRequest, stream: bool) -> LlmResponse {
    generate_last_in("", m, req, stream).await
}

/// [`generate_last`] whose panic message starts with `case`.
pub async fn generate_last_in(
    case: &str,
    m: &dyn Llm,
    req: LlmRequest,
    stream: bool,
) -> LlmResponse {
    generate_ok_in(case, m, req, stream)
        .await
        .pop()
        .unwrap_or_else(|| panic!("{}GenerateContent() yielded no response", case_prefix(case)))
}

/// The last error (Go `gotErr = err`); panics when the stream yielded none.
pub async fn generate_err(m: &dyn Llm, req: LlmRequest, stream: bool) -> ModelError {
    generate_err_in("", m, req, stream).await
}

/// [`generate_err`] whose panic message starts with `case`.
pub async fn generate_err_in(case: &str, m: &dyn Llm, req: LlmRequest, stream: bool) -> ModelError {
    generate(m, req, stream)
        .await
        .into_iter()
        .filter_map(Result::err)
        .next_back()
        .unwrap_or_else(|| panic!("{}GenerateContent() yielded no error", case_prefix(case)))
}

/// The [`AnthropicError`] inside a `ModelError::Other` (how the model reports every error),
/// or `None`.
pub fn as_anthropic(err: &ModelError) -> Option<&AnthropicError> {
    match err {
        ModelError::Other(inner) => inner.downcast_ref::<AnthropicError>(),
        _ => None,
    }
}

/// Like [`as_anthropic`], panicking when `err` is not an `AnthropicError`.
pub fn anthropic_error(err: &ModelError) -> &AnthropicError {
    as_anthropic(err).unwrap_or_else(|| panic!("error {err:?} is not an AnthropicError"))
}

// ------------------------------------------------------------------------------------- httprr

/// `crates/gadk-anthropic/tests/testdata/model/anthropicmodel/testdata/<file>`.
pub fn testdata_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/model/anthropicmodel/testdata")
        .join(file)
}

/// Go `openHTTPRR(t, rrfile)`: opens `testdata_path(file)` over a real `reqwest` client
/// (used only when `GADK_HTTPRR_RECORD` selects the file), with Go's scrubbers. Use
/// `is_recording()` for Go's `rr.Recording()`.
pub fn open_httprr(file: &str) -> Arc<RecordReplay> {
    let path = testdata_path(file);
    let mut rr = RecordReplay::open(&path, Arc::new(ReqwestClient::default()))
        .unwrap_or_else(|e| panic!("httprr.Open({:?}) failed: {e}", path.display()));
    rr.scrub_req(Box::new(scrub_anthropic_request));
    rr.scrub_resp(Box::new(scrub_anthropic_response));
    Arc::new(rr)
}

/// Go `scrubAnthropicRequest`: drops `authorization`, `x-api-key`, `user-agent`,
/// `x-goog-api-client` and `x-stainless-*`; rewrites `/projects/<p>/` to
/// `/projects/test-project/`; compacts a JSON body.
pub fn scrub_anthropic_request(req: &mut http::Request<Bytes>) -> Result<(), HttprrError> {
    let drop: Vec<http::HeaderName> = req
        .headers()
        .keys()
        .filter(|k| {
            let lower = k.as_str().to_ascii_lowercase();
            matches!(
                lower.as_str(),
                "authorization" | "x-api-key" | "user-agent" | "x-goog-api-client"
            ) || lower.starts_with("x-stainless-")
        })
        .cloned()
        .collect();
    for k in drop {
        req.headers_mut().remove(k);
    }

    let path = req.uri().path().to_string();
    if let Some(idx) = path.find("/projects/") {
        let rest = &path[idx + "/projects/".len()..];
        if let Some(slash) = rest.find('/').filter(|&s| s > 0) {
            let new_path = format!("{}/projects/test-project{}", &path[..idx], &rest[slash..]);
            let path_and_query = match req.uri().query() {
                Some(q) => format!("{new_path}?{q}"),
                None => new_path,
            };
            let mut parts = req.uri().clone().into_parts();
            parts.path_and_query = Some(
                path_and_query
                    .parse()
                    .map_err(|e: http::uri::InvalidUri| HttprrError::Wire(e.to_string()))?,
            );
            *req.uri_mut() =
                http::Uri::from_parts(parts).map_err(|e| HttprrError::Wire(e.to_string()))?;
        }
    }

    if !req.body().is_empty()
        && let Some(compact) = json_compact(req.body())
    {
        *req.body_mut() = Bytes::from(compact);
    }
    Ok(())
}

/// Go `scrubAnthropicResponse`: removes the `anthropic-organization-id`,
/// `anthropic-workspace-id`, `set-cookie`, `request-id` and `x-request-id` header lines
/// (case-insensitive) from the logged response, up to the first empty line.
pub fn scrub_anthropic_response(buf: &mut Vec<u8>) -> Result<(), HttprrError> {
    const DROPPED: [&str; 5] = [
        "anthropic-organization-id:",
        "anthropic-workspace-id:",
        "set-cookie:",
        "request-id:",
        "x-request-id:",
    ];
    let mut out: Vec<&[u8]> = Vec::new();
    let mut in_headers = true;
    for line in buf.split(|&b| b == b'\n') {
        if in_headers {
            let mut trimmed = line;
            while let Some(rest) = trimmed.strip_suffix(b"\r") {
                trimmed = rest;
            }
            if trimmed.is_empty() {
                in_headers = false;
                out.push(line);
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if DROPPED.iter().any(|p| lower.starts_with(p.as_bytes())) {
                continue;
            }
        }
        out.push(line);
    }
    let joined = out.join(&b'\n');
    *buf = joined;
    Ok(())
}

// ---------------------------------------------------------------------------- harness self-test

#[tokio::test]
async fn harness_log_capture_renders_msg_key_value() {
    let logs = LogCapture::new();
    {
        let _guard = logs.set_default();
        tracing::warn!(
            target: "gadk_anthropic::request",
            backend = "vertex",
            model = MODEL,
            role = "user",
            part_kind = "file_data",
            index = 3_i64,
            "skipping unsupported content part for Claude request"
        );
        tracing::info!(target: "hyper::proto", "ignored: outside gadk_anthropic");
    }
    tracing::warn!(target: "gadk_anthropic::request", "after the guard: not captured");

    let events = logs.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].level, Level::WARN);
    assert_eq!(events[0].target, "gadk_anthropic::request");
    assert_eq!(
        events[0].message,
        "skipping unsupported content part for Claude request"
    );
    assert_eq!(events[0].field("backend"), Some("vertex"));
    assert_eq!(events[0].field("index"), Some("3"));
    assert_eq!(events[0].field("missing"), None);
    assert_eq!(
        logs.rendered(),
        vec![format!(
            "skipping unsupported content part for Claude request backend=vertex model={MODEL} role=user part_kind=file_data index=3"
        )]
    );
    assert_eq!(logs.warnings().len(), 1);

    let all = LogCapture::with_target_prefix(None);
    all.capture(async {
        tracing::info!(target: "elsewhere", answer = 42_u64, "inside capture");
    })
    .await;
    assert_eq!(all.rendered(), vec!["inside capture answer=42".to_string()]);
    assert!(all.warnings().is_empty());
    all.clear();
    assert!(all.events().is_empty());
}

#[test]
fn harness_json_builders_match_go_layout() {
    assert_eq!(
        text_message_json("msg_v", "say \"hi\"", 4, 1),
        r#"{"id":"msg_v","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"say \"hi\""}],"usage":{"input_tokens":4,"output_tokens":1}}"#
    );
    assert_eq!(
        json_compact(b"{ \"a\" : [1, 2],\n\t\"b\": \"x y\\\" z\" }").unwrap(),
        br#"{"a":[1,2],"b":"x y\" z"}"#.to_vec()
    );
    assert_eq!(json_compact(b"not json"), None);
}

#[test]
fn harness_sample_png_is_go_bytes() {
    assert_eq!(SAMPLE_PNG_1X1.len(), 73);
    assert_eq!(&SAMPLE_PNG_1X1[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&SAMPLE_PNG_1X1[12..16], b"IHDR");
    assert_eq!(&SAMPLE_PNG_1X1[65..73], b"IEND\xaeB`\x82");
    assert_eq!(sample_png().len(), 73);
}

#[test]
fn harness_adc_files_and_env() {
    let path;
    {
        let file = adc_authorized_user_file();
        path = file.path().to_path_buf();
        assert_eq!(
            std::fs::read_to_string(file.path()).unwrap(),
            ADC_AUTHORIZED_USER_JSON
        );
        let env = adc_env(&file.path_string());
        assert_eq!(
            gadk_anthropic::EnvProvider::var(&*env, "GOOGLE_APPLICATION_CREDENTIALS"),
            Some(file.path_string())
        );
        let with_uri = adc_authorized_user_file_with_token_uri("http://127.0.0.1:9/token");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(with_uri.path()).unwrap()).unwrap();
        assert_eq!(v["token_uri"], "http://127.0.0.1:9/token");
        assert_eq!(v["type"], "authorized_user");
    }
    assert!(!path.exists(), "TempFile must be removed on drop");
    assert!(!Path::new(&missing_adc_path()).exists());
    assert_eq!(
        gadk_anthropic::EnvProvider::var(&*empty_env(), "ANTHROPIC_API_KEY"),
        None
    );
    let e = env(&[("ANTHROPIC_BASE_URL", "")]);
    assert_eq!(
        gadk_anthropic::EnvProvider::var(&*e, "ANTHROPIC_BASE_URL"),
        Some(String::new())
    );
}

#[tokio::test]
async fn harness_static_token_credentials_and_configs() {
    let creds = static_token_credentials("test-token");
    let CacheableResource::New { data, .. } = creds.headers(http::Extensions::new()).await.unwrap()
    else {
        panic!("want new headers");
    };
    assert_eq!(data["authorization"], "Bearer test-token");

    let a = anthropic_config("http://127.0.0.1:1");
    assert_eq!(a.backend, Backend::Anthropic);
    assert_eq!(a.api_key.as_deref(), Some("test-key"));
    assert_eq!(a.base_url.as_deref(), Some("http://127.0.0.1:1"));
    assert_eq!(a.env.var("ANTHROPIC_API_KEY"), None);

    let v = vertex_config("http://127.0.0.1:1");
    assert_eq!(v.backend, Backend::Vertex);
    assert_eq!(v.project.as_deref(), Some("test-project"));
    assert_eq!(v.location.as_deref(), Some("us-east5"));
    assert!(v.credentials.is_some());
    assert!(v.api_key.is_none());

    let req = text_request("hi");
    assert_eq!(req.contents, gadk_genai::text("hi"));
}

#[tokio::test]
async fn harness_httprr_replays_with_go_scrubbers() {
    let rr = open_httprr("TestGenerate_Vertex.httprr");
    assert!(!rr.is_recording());
    // A pretty-printed body, a real project ID and secret headers: the scrubbers make it
    // match the recorded request.
    let body = "{\n  \"max_tokens\": 8192,\n  \"messages\": [{\"content\": [{\"text\": \"What is 2 + 2? Reply with the digit only.\", \"type\": \"text\"}], \"role\": \"user\"}],\n  \"temperature\": 0,\n  \"anthropic_version\": \"vertex-2023-10-16\"\n}";
    let req = http::Request::builder()
        .method("POST")
        .uri("https://us-east5-aiplatform.googleapis.com/v1/projects/my-real-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929:rawPredict")
        .header("authorization", "Bearer secret")
        .header("x-goog-api-client", "gadk")
        .header("content-type", "application/json")
        .body(Bytes::from(body))
        .unwrap();
    let resp = rr.send(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = gadk_model::collect_body(resp.into_body()).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains(r#""text":"4""#));
}

#[test]
fn harness_scrub_request_matches_go() {
    let mut req = http::Request::builder()
        .uri("https://h/v1/projects/p-1/locations/l/x:rawPredict?alt=sse")
        .header("Authorization", "Bearer s")
        .header("X-Api-Key", "k")
        .header("User-Agent", "ua")
        .header("X-Goog-Api-Client", "c")
        .header("X-Stainless-Os", "linux")
        .header("Anthropic-Version", "2023-06-01")
        .body(Bytes::from_static(b"{ \"a\": 1 }"))
        .unwrap();
    scrub_anthropic_request(&mut req).unwrap();
    assert_eq!(
        req.uri().to_string(),
        "https://h/v1/projects/test-project/locations/l/x:rawPredict?alt=sse"
    );
    let names: Vec<&str> = req.headers().keys().map(|k| k.as_str()).collect();
    assert_eq!(names, vec!["anthropic-version"]);
    assert_eq!(&req.body()[..], br#"{"a":1}"#);

    // `/projects//` (empty project) and non-JSON bodies are left alone, as in Go.
    let mut req = http::Request::builder()
        .uri("https://h/v1/projects//x")
        .body(Bytes::from_static(b"not json "))
        .unwrap();
    scrub_anthropic_request(&mut req).unwrap();
    assert_eq!(req.uri().to_string(), "https://h/v1/projects//x");
    assert_eq!(&req.body()[..], b"not json ");
}

#[test]
fn harness_scrub_response_matches_go() {
    let mut buf = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nRequest-Id: req_1\r\nanthropic-organization-id: org\r\nSet-Cookie: a=b\r\nX-Request-Id: x\r\nAnthropic-Workspace-Id: w\r\n\r\nrequest-id: body line kept\n".to_vec();
    scrub_anthropic_response(&mut buf).unwrap();
    assert_eq!(
        String::from_utf8(buf).unwrap(),
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\nrequest-id: body line kept\n"
    );

    // Go `strings.TrimRight(line, "\r")` strips every trailing CR: `\r\r` still ends the headers.
    let mut buf = b"HTTP/1.1 200 OK\r\n\r\r\nrequest-id: body\n".to_vec();
    scrub_anthropic_response(&mut buf).unwrap();
    assert_eq!(
        String::from_utf8(buf).unwrap(),
        "HTTP/1.1 200 OK\r\n\r\r\nrequest-id: body\n"
    );
}
