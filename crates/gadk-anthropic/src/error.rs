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

//! Errors (port of ADK-Go `model/anthropicmodel/errors.go`, plus the parts of anthropic-sdk-go
//! v1.81.0 `internal/apierror` that decide an error response's type and request ID).
//!
//! Go's sentinels become [`ErrorKind`]; an [`AnthropicError`] carries the kinds it wraps, in
//! Go's `%w` order, and renders Go's `Error()` text as its `Display`.

use std::fmt;

use gadk_model::ModelError;
use serde_json::{Map, Value};

/// The HTTP status code Anthropic uses when it is overloaded (Go `StatusOverloaded`).
pub const STATUS_OVERLOADED: u16 = 529;

/// One Go sentinel error of `anthropicmodel` (`errors.go`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Go `ErrNoContent`: a request has no sendable content blocks.
    NoContent,
    /// Go `ErrInvalidConfig`: `ClientConfig` or `GenerateContentConfig` is invalid.
    InvalidConfig,
    /// Go `ErrUnsupportedToolType`: a `Tool` contains a non-function tool.
    UnsupportedToolType,
    /// Go `ErrUnsupportedContent`: unsupported content caused a message to fail.
    UnsupportedContent,
    /// Go `ErrAuthentication`: authentication or permission failure (HTTP 401/403).
    Authentication,
    /// Go `ErrBadRequest`: invalid request error from the API (HTTP 400).
    BadRequest,
    /// Go `ErrRateLimited`: the API reports a rate limit (HTTP 429 or `rate_limit_error`).
    RateLimited,
    /// Go `ErrOverloaded`: the API reports overload (HTTP 529 or `overloaded_error`).
    Overloaded,
    /// Go `ErrAPIStatus`: any other non-2xx API response.
    ApiStatus,
}

impl ErrorKind {
    /// The sentinel's Go `Error()` text.
    pub fn sentinel_text(self) -> &'static str {
        match self {
            Self::NoContent => "anthropicmodel: request contains no sendable content",
            Self::InvalidConfig => "anthropicmodel: invalid configuration",
            Self::UnsupportedToolType => "anthropicmodel: unsupported tool type",
            Self::UnsupportedContent => "anthropicmodel: unsupported content",
            Self::Authentication => "anthropicmodel: authentication failed",
            Self::BadRequest => "anthropicmodel: bad request",
            Self::RateLimited => "anthropicmodel: rate limited",
            Self::Overloaded => "anthropicmodel: service overloaded",
            Self::ApiStatus => "anthropicmodel: unexpected API status",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentinel_text())
    }
}

/// A structured error returned by the Anthropic API or the Vertex AI Claude endpoint, as an
/// HTTP status response or as a mid-stream SSE `error` event (Go `APIError`).
///
/// Its `Display` holds only the status, error type, request ID and a sanitized message, never
/// prompts, model output, URLs or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status code (for example 400, 401, 429, 500, 529).
    pub status_code: u16,
    /// Anthropic error type (for example `rate_limit_error`); may be empty.
    pub error_type: String,
    /// The server's request ID; may be empty.
    pub request_id: String,
    /// A short, sanitized description; may be empty.
    pub message: String,
    /// The sentinel the error classifies as (Go `APIError.Err`).
    pub kind: ErrorKind,
}

/// Go `(*APIError).Error`.
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("anthropicmodel: api error")?;
        if self.status_code > 0 {
            write!(f, " (status {})", self.status_code)?;
        }
        if !self.error_type.is_empty() {
            write!(f, " [type={}]", self.error_type)?;
        }
        if !self.request_id.is_empty() {
            write!(f, " [request_id={}]", self.request_id)?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// Where an [`AnthropicError`] comes from; decides the accessors that are true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// `fmt.Errorf("%w: …", sentinel…)`.
    Sentinel,
    /// An [`ApiError`].
    Api,
    /// Go `context.DeadlineExceeded` (a per-attempt timeout on the last attempt).
    DeadlineExceeded,
    /// `anthropicmodel: transport error: …`.
    Transport,
    /// `anthropicmodel: request failed: …`.
    Request,
    /// An error text Go builds without a sentinel.
    Plain,
}

/// The error type of `gadk-anthropic` (Go's sentinel-wrapping errors, [`ApiError`],
/// `context.DeadlineExceeded`, and the transport and request failures of Go `classifyError`).
///
/// `Display` is Go's `Error()` text. [`is`](Self::is) replaces Go's `errors.Is` for the
/// sentinels: Go's double wraps (`%w: %w`) report both kinds.
#[derive(Debug, Clone)]
pub struct AnthropicError {
    kinds: Vec<ErrorKind>,
    message: String,
    api: Option<Box<ApiError>>,
    origin: Origin,
}

impl fmt::Display for AnthropicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AnthropicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.api
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl AnthropicError {
    /// The sentinels this error wraps, outermost first (Go `%w` order).
    pub fn kinds(&self) -> &[ErrorKind] {
        &self.kinds
    }

    /// Go `errors.Is(err, <sentinel>)`.
    pub fn is(&self, kind: ErrorKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Go `errors.As(err, &*APIError)`.
    pub fn api_error(&self) -> Option<&ApiError> {
        self.api.as_deref()
    }

    /// Go `errors.Is(err, context.DeadlineExceeded)`.
    pub fn is_deadline_exceeded(&self) -> bool {
        self.origin == Origin::DeadlineExceeded
    }

    /// True for `anthropicmodel: transport error: …` (no response was received).
    pub fn is_transport(&self) -> bool {
        self.origin == Origin::Transport
    }

    /// Go `fmt.Errorf("%w: <detail>", kind)`.
    pub(crate) fn new(kind: ErrorKind, detail: impl fmt::Display) -> Self {
        Self {
            kinds: vec![kind],
            message: format!("{kind}: {detail}"),
            api: None,
            origin: Origin::Sentinel,
        }
    }

    /// Go `fmt.Errorf("%w: %w: <detail>", outer, inner)`.
    pub(crate) fn wrap2(outer: ErrorKind, inner: ErrorKind, detail: impl fmt::Display) -> Self {
        Self {
            kinds: vec![outer, inner],
            message: format!("{outer}: {inner}: {detail}"),
            api: None,
            origin: Origin::Sentinel,
        }
    }

    /// An [`ApiError`] returned as it is (Go returns the `*APIError`).
    pub(crate) fn api(err: ApiError) -> Self {
        Self {
            kinds: vec![err.kind],
            message: err.to_string(),
            api: Some(Box::new(err)),
            origin: Origin::Api,
        }
    }

    /// Go `context.DeadlineExceeded`.
    pub(crate) fn deadline_exceeded() -> Self {
        Self {
            kinds: Vec::new(),
            message: "context deadline exceeded".to_string(),
            api: None,
            origin: Origin::DeadlineExceeded,
        }
    }

    /// Go `fmt.Errorf("anthropicmodel: transport error: %w", urlErr.Err)`. `detail` must not
    /// contain the request URL.
    pub(crate) fn transport(detail: impl fmt::Display) -> Self {
        Self {
            kinds: Vec::new(),
            message: format!("anthropicmodel: transport error: {detail}"),
            api: None,
            origin: Origin::Transport,
        }
    }

    /// Go `fmt.Errorf("anthropicmodel: request failed: %w", err)`.
    pub(crate) fn request_failed(detail: impl fmt::Display) -> Self {
        Self {
            kinds: Vec::new(),
            message: format!("anthropicmodel: request failed: {detail}"),
            api: None,
            origin: Origin::Request,
        }
    }

    /// An error Go builds with `fmt.Errorf` and no `%w` (for example
    /// `anthropicmodel: failed to decode tool_use input for "x"`).
    pub(crate) fn plain(message: impl Into<String>) -> Self {
        Self {
            kinds: Vec::new(),
            message: message.into(),
            api: None,
            origin: Origin::Plain,
        }
    }

    /// Port of Go `classifyError` for the errors an [`HttpClient`](gadk_model::HttpClient) or a
    /// body stream returns:
    /// - an `AnthropicError` boxed in [`ModelError::Other`] (how gadk-anthropic's own body
    ///   streams report a deadline) is returned as it is;
    /// - [`ModelError::Transport`] (no response) becomes a transport error, with the URL removed
    ///   from a `reqwest::Error` and the source chain joined with `": "`;
    /// - anything else becomes `anthropicmodel: request failed: <err>`.
    pub(crate) fn from_model_error(err: ModelError) -> Self {
        match err {
            ModelError::Other(boxed) => match boxed.downcast::<AnthropicError>() {
                Ok(e) => *e,
                Err(other) => Self::request_failed(ModelError::Other(other)),
            },
            ModelError::Transport(boxed) => match boxed.downcast::<reqwest::Error>() {
                Ok(e) => Self::transport(error_chain(&e.without_url())),
                Err(other) => Self::transport(error_chain(other.as_ref())),
            },
            other => Self::request_failed(other),
        }
    }
}

/// `err` and its sources, joined with `": "`.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(e) = cur {
        out.push_str(": ");
        out.push_str(&e.to_string());
        cur = e.source();
    }
    out
}

/// Every [`AnthropicError`] becomes [`ModelError::Other`], so `Display` is unchanged and the
/// typed error is recovered by downcasting.
impl From<AnthropicError> for ModelError {
    fn from(err: AnthropicError) -> Self {
        ModelError::Other(Box::new(err))
    }
}

// ---------------------------------------------------------------- API error classification

/// Go `json.Unmarshal` into a `string` field: absent or `null` leave `""`; any other non-string
/// value is an error.
fn go_string(obj: &Map<String, Value>, key: &str) -> Result<String, ()> {
    match go_field(obj, key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(()),
    }
}

/// Go's JSON field matching: an exact key, else a case-insensitive one.
fn go_field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    obj.get(key).or_else(|| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    })
}

/// Go `json.Unmarshal` into a struct field: absent or `null` leave the zero value; a non-object
/// is an error.
fn go_object<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a Map<String, Value>>, ()> {
    match go_field(obj, key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(_) => Err(()),
    }
}

/// Go `errorEnvelope` (`errors.go`), decoded with `json.Unmarshal` semantics.
#[derive(Debug, Default, PartialEq, Eq)]
struct Envelope {
    envelope_type: String,
    request_id: String,
    error_type: String,
    error_message: String,
}

fn parse_envelope(v: &Value) -> Option<Envelope> {
    let obj = match v {
        Value::Object(m) => m,
        Value::Null => return Some(Envelope::default()),
        _ => return None,
    };
    let mut env = Envelope {
        envelope_type: go_string(obj, "type").ok()?,
        request_id: go_string(obj, "request_id").ok()?,
        ..Envelope::default()
    };
    if let Some(e) = go_object(obj, "error").ok()? {
        env.error_type = go_string(e, "type").ok()?;
        env.error_message = go_string(e, "message").ok()?;
    }
    Some(env)
}

/// anthropic-sdk-go `(*apierror.Error).UnmarshalJSON`'s error type: `error.type` of the
/// envelope, or `""` when the body is not JSON or does not decode.
fn sdk_error_type(v: &Value) -> String {
    let Value::Object(obj) = v else {
        return String::new();
    };
    match go_object(obj, "error") {
        Ok(Some(e)) => go_string(e, "type").unwrap_or_default(),
        _ => String::new(),
    }
}

/// The [`ApiError`] for an error response, or for the data of a mid-stream `error` event.
///
/// Port of anthropic-sdk-go's `apierror.Error` decoding (type from `{"error":{"type"}}`,
/// request ID from the `request-id` header) followed by Go `classifySDKError` and
/// `buildAPIError`: the header's request ID wins over the body's `request_id`; the message is
/// the envelope's `error.message`, sanitized. `status` is the HTTP status of the response (200
/// for a mid-stream event), `request_id_header` the `request-id` header (`""` when absent).
pub(crate) fn api_error_from_response(
    status: u16,
    request_id_header: &str,
    body: &[u8],
) -> ApiError {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let mut error_type = parsed.as_ref().map(sdk_error_type).unwrap_or_default();
    let mut request_id = request_id_header.to_string();
    let mut message = String::new();
    if let Some(env) = parsed.as_ref().and_then(parse_envelope) {
        if error_type.is_empty() && !env.error_type.is_empty() {
            error_type = env.error_type;
        }
        if request_id.is_empty() && !env.request_id.is_empty() {
            request_id = env.request_id;
        }
        message = env.error_message;
    }
    build_api_error(status, &error_type, &request_id, &message)
}

/// Go `classifyError`'s fallback for a mid-stream error that reached it as text: the JSON from
/// the first `{` on, when it decodes as an error envelope with an error type or with
/// `"type":"error"`, gives an [`ApiError`] with status 0 (mapped by [`build_api_error`]).
pub(crate) fn sniff_error_envelope(msg: &str) -> Option<ApiError> {
    let idx = msg.find('{')?;
    let raw = msg[idx..].trim();
    let v: Value = serde_json::from_str(raw).ok()?;
    let env = parse_envelope(&v)?;
    if env.error_type.is_empty() && env.envelope_type != "error" {
        return None;
    }
    Some(build_api_error(
        0,
        &env.error_type,
        &env.request_id,
        &env.error_message,
    ))
}

/// Go `buildAPIError`: the sentinel from the status or the error type, and a status filled in
/// when the response had none (0) or was 200 (a mid-stream event).
pub(crate) fn build_api_error(
    status: u16,
    error_type: &str,
    request_id: &str,
    raw_msg: &str,
) -> ApiError {
    let error_type = error_type.trim();
    let no_status = status == 0 || status == 200;
    let (kind, status) = if status == 429 || error_type == "rate_limit_error" {
        (ErrorKind::RateLimited, if no_status { 429 } else { status })
    } else if status == STATUS_OVERLOADED || error_type == "overloaded_error" {
        (
            ErrorKind::Overloaded,
            if no_status { STATUS_OVERLOADED } else { status },
        )
    } else if status == 401
        || status == 403
        || error_type == "authentication_error"
        || error_type == "permission_error"
    {
        (
            ErrorKind::Authentication,
            if no_status { 401 } else { status },
        )
    } else if status == 400 || error_type == "invalid_request_error" {
        (ErrorKind::BadRequest, if no_status { 400 } else { status })
    } else {
        let status = if no_status {
            match error_type {
                "not_found_error" => 404,
                "timeout_error" => 504,
                "billing_error" => 402,
                _ => 500,
            }
        } else {
            status
        };
        (ErrorKind::ApiStatus, status)
    };
    ApiError {
        status_code: status,
        error_type: error_type.to_string(),
        request_id: request_id.trim().to_string(),
        message: sanitize_error_message(raw_msg),
        kind,
    }
}

/// Go `sanitizeErrorMessage`: `""` for a blank message; `api request rejected` when the
/// message holds a URL, a `gs://` URI, an `sk-ant-` key or a bearer token; otherwise the
/// trimmed message cut to at most 256 bytes on a character boundary.
pub(crate) fn sanitize_error_message(msg: &str) -> String {
    let msg = msg.trim();
    if msg.is_empty() {
        return String::new();
    }
    let lower = msg.to_lowercase();
    if ["http://", "https://", "gs://", "sk-ant-", "bearer "]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return "api request rejected".to_string();
    }
    if msg.len() > 256 {
        let mut end = 256;
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        return msg[..end].to_string();
    }
    msg.to_string()
}

// ---------------------------------------------------------------- Go %q

/// Port of Go `strconv.Quote` (Go `%q` on a string), shared with the other gadk crates.
pub(crate) use gadk_platform::go_quote;

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn api(status: u16, ty: &str, rid: &str, msg: &str, kind: ErrorKind) -> ApiError {
        ApiError {
            status_code: status,
            error_type: ty.to_string(),
            request_id: rid.to_string(),
            message: msg.to_string(),
            kind,
        }
    }

    #[test]
    fn sentinel_texts_are_gos() {
        let cases = [
            (
                ErrorKind::NoContent,
                "anthropicmodel: request contains no sendable content",
            ),
            (
                ErrorKind::InvalidConfig,
                "anthropicmodel: invalid configuration",
            ),
            (
                ErrorKind::UnsupportedToolType,
                "anthropicmodel: unsupported tool type",
            ),
            (
                ErrorKind::UnsupportedContent,
                "anthropicmodel: unsupported content",
            ),
            (
                ErrorKind::Authentication,
                "anthropicmodel: authentication failed",
            ),
            (ErrorKind::BadRequest, "anthropicmodel: bad request"),
            (ErrorKind::RateLimited, "anthropicmodel: rate limited"),
            (ErrorKind::Overloaded, "anthropicmodel: service overloaded"),
            (
                ErrorKind::ApiStatus,
                "anthropicmodel: unexpected API status",
            ),
        ];
        for (kind, want) in cases {
            assert_eq!(kind.to_string(), want);
        }
    }

    /// Go `(*APIError).Error` with every optional part present, then each one absent.
    #[test]
    fn api_error_display_is_gos() {
        let full = api(
            429,
            "rate_limit_error",
            "req_1",
            "Rate limit exceeded",
            ErrorKind::RateLimited,
        );
        assert_eq!(
            full.to_string(),
            "anthropicmodel: api error (status 429) [type=rate_limit_error] [request_id=req_1]: Rate limit exceeded"
        );
        assert_eq!(
            api(0, "", "", "", ErrorKind::ApiStatus).to_string(),
            "anthropicmodel: api error"
        );
        assert_eq!(
            api(500, "", "", "boom", ErrorKind::ApiStatus).to_string(),
            "anthropicmodel: api error (status 500): boom"
        );
        assert_eq!(
            api(0, "x", "r", "", ErrorKind::ApiStatus).to_string(),
            "anthropicmodel: api error [type=x] [request_id=r]"
        );
    }

    #[test]
    fn wrapping_constructors_render_gos_text_and_report_every_kind() {
        let e = AnthropicError::new(ErrorKind::InvalidConfig, "model name must not be empty");
        assert_eq!(
            e.to_string(),
            "anthropicmodel: invalid configuration: model name must not be empty"
        );
        assert!(e.is(ErrorKind::InvalidConfig));
        assert!(!e.is(ErrorKind::Authentication));

        let e = AnthropicError::wrap2(
            ErrorKind::InvalidConfig,
            ErrorKind::Authentication,
            "failed to find Google Application Default Credentials for Vertex AI",
        );
        assert_eq!(
            e.to_string(),
            "anthropicmodel: invalid configuration: anthropicmodel: authentication failed: failed to find Google Application Default Credentials for Vertex AI"
        );
        assert_eq!(
            e.kinds(),
            &[ErrorKind::InvalidConfig, ErrorKind::Authentication]
        );
        assert!(e.is(ErrorKind::Authentication));

        let e = AnthropicError::api(api(529, "overloaded_error", "", "", ErrorKind::Overloaded));
        assert!(e.is(ErrorKind::Overloaded));
        assert_eq!(e.api_error().map(|a| a.status_code), Some(529));
        assert_eq!(
            e.to_string(),
            "anthropicmodel: api error (status 529) [type=overloaded_error]"
        );

        let e = AnthropicError::deadline_exceeded();
        assert_eq!(e.to_string(), "context deadline exceeded");
        assert!(e.is_deadline_exceeded());
        assert!(e.kinds().is_empty());

        assert_eq!(
            AnthropicError::transport("connection refused").to_string(),
            "anthropicmodel: transport error: connection refused"
        );
        assert_eq!(
            AnthropicError::request_failed("boom").to_string(),
            "anthropicmodel: request failed: boom"
        );
        assert_eq!(AnthropicError::plain("x").to_string(), "x");
    }

    /// The `errorCases` table of Go `TestFinishReasonAndErrorClassification`
    /// (`http_status_and_midstream_sse_errors`): status, type and sentinel per response.
    #[test]
    #[allow(clippy::type_complexity)]
    fn api_error_from_response_classifies_like_go() {
        let cases: [(&str, u16, &str, &str, ErrorKind, u16, &str, &str); 7] = [
            (
                "http_429_rate_limit",
                429,
                "req_hdr",
                r#"{"type":"error","request_id":"req_429","error":{"type":"rate_limit_error","message":"Rate limit exceeded"}}"#,
                ErrorKind::RateLimited,
                429,
                "rate_limit_error",
                "req_hdr",
            ),
            (
                "http_529_overloaded",
                529,
                "req_hdr",
                r#"{"type":"error","request_id":"req_529","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
                ErrorKind::Overloaded,
                529,
                "overloaded_error",
                "req_hdr",
            ),
            (
                "http_401_auth",
                401,
                "req_hdr",
                r#"{"type":"error","request_id":"req_401","error":{"type":"authentication_error","message":"Invalid key sk-ant-secret"}}"#,
                ErrorKind::Authentication,
                401,
                "authentication_error",
                "req_hdr",
            ),
            (
                "http_400_bad_request",
                400,
                "req_hdr",
                r#"{"type":"error","request_id":"req_400","error":{"type":"invalid_request_error","message":"thinking.budget_tokens: Input should be >= 1024"}}"#,
                ErrorKind::BadRequest,
                400,
                "invalid_request_error",
                "req_hdr",
            ),
            (
                "midstream_sse_overloaded",
                200,
                "",
                r#"{"type":"error","request_id":"req_sse_529","error":{"type":"overloaded_error","message":"Overloaded mid-stream"}}"#,
                ErrorKind::Overloaded,
                529,
                "overloaded_error",
                "req_sse_529",
            ),
            (
                "midstream_sse_rate_limit",
                200,
                "",
                r#"{"type":"error","request_id":"req_sse_429","error":{"type":"rate_limit_error","message":"Rate limited mid-stream"}}"#,
                ErrorKind::RateLimited,
                429,
                "rate_limit_error",
                "req_sse_429",
            ),
            (
                "midstream_sse_api_error_maps_to_500_not_200",
                200,
                "",
                r#"{"type":"error","request_id":"req_sse_500","error":{"type":"api_error","message":"Internal server error mid-stream"}}"#,
                ErrorKind::ApiStatus,
                500,
                "api_error",
                "req_sse_500",
            ),
        ];
        for (name, status, hdr, body, kind, want_status, want_type, want_rid) in cases {
            let e = api_error_from_response(status, hdr, body.as_bytes());
            assert_eq!(e.kind, kind, "{name}");
            assert_eq!(e.status_code, want_status, "{name}");
            assert_eq!(e.error_type, want_type, "{name}");
            assert_eq!(e.request_id, want_rid, "{name}");
            assert!(!e.to_string().contains("sk-ant-secret"), "{name}");
        }
        let e = api_error_from_response(
            401,
            "",
            br#"{"error":{"type":"authentication_error","message":"Invalid key sk-ant-secret"}}"#,
        );
        assert_eq!(e.message, "api request rejected");
    }

    #[test]
    fn api_error_from_unusual_bodies() {
        // Not JSON: no type, no message; the status decides.
        let e = api_error_from_response(503, "", b"<html>busy</html>");
        assert_eq!(e, api(503, "", "", "", ErrorKind::ApiStatus));
        // Empty body on a 200 (a mid-stream event without data) maps to 500.
        let e = api_error_from_response(200, "", b"");
        assert_eq!(e, api(500, "", "", "", ErrorKind::ApiStatus));
        // A wrongly typed field discards the envelope (Go json.Unmarshal fails), but the SDK's
        // own `error.type` lookup still succeeds.
        let e = api_error_from_response(
            200,
            "",
            br#"{"request_id":"r","error":{"type":"not_found_error","message":5}}"#,
        );
        assert_eq!(e, api(404, "not_found_error", "", "", ErrorKind::ApiStatus));
        // Go matches JSON keys case-insensitively.
        let e = api_error_from_response(
            200,
            "",
            br#"{"Request_ID":"r","Error":{"Type":"billing_error","Message":"pay"}}"#,
        );
        assert_eq!(
            e,
            api(402, "billing_error", "r", "pay", ErrorKind::ApiStatus)
        );
        let e = api_error_from_response(200, "", br#"{"error":{"type":"timeout_error"}}"#);
        assert_eq!(e.status_code, 504);
        let e =
            api_error_from_response(403, " rid ", br#"{"error":{"type":" permission_error "}}"#);
        assert_eq!(
            e,
            api(
                403,
                "permission_error",
                "rid",
                "",
                ErrorKind::Authentication
            )
        );
    }

    #[test]
    fn sniff_error_envelope_ports_classify_errors_fallback() {
        let e = sniff_error_envelope(
            r#"received error while streaming: {"type":"error","request_id":"r","error":{"type":"overloaded_error","message":"m"}}"#,
        );
        assert_eq!(
            e,
            Some(api(
                529,
                "overloaded_error",
                "r",
                "m",
                ErrorKind::Overloaded
            ))
        );
        let e = sniff_error_envelope(r#"x {"type":"error"}"#);
        assert_eq!(e, Some(api(500, "", "", "", ErrorKind::ApiStatus)));
        assert_eq!(sniff_error_envelope(r#"x {"type":"message"}"#), None);
        assert_eq!(sniff_error_envelope("no json here"), None);
        assert_eq!(sniff_error_envelope("bad {json"), None);
    }

    #[test]
    fn sanitize_error_message_like_go() {
        assert_eq!(sanitize_error_message("  "), "");
        assert_eq!(
            sanitize_error_message(" field x: too big "),
            "field x: too big"
        );
        for leaky in [
            "see https://example.com/x",
            "HTTP://a",
            "gs://bucket/o",
            "key sk-ant-abc",
            "Authorization: Bearer abc",
        ] {
            assert_eq!(
                sanitize_error_message(leaky),
                "api request rejected",
                "{leaky}"
            );
        }
        // 256-byte cut that lands inside a two-byte character drops the partial character
        // (Go strings.ToValidUTF8(msg[:256], "")).
        let msg = format!("a{}", "é".repeat(200));
        let got = sanitize_error_message(&msg);
        assert_eq!(got.len(), 255);
        assert_eq!(got, format!("a{}", "é".repeat(127)));
        let ascii = "x".repeat(300);
        assert_eq!(sanitize_error_message(&ascii).len(), 256);
    }

    #[test]
    fn model_error_conversion_round_trips_and_classifies() {
        let original = AnthropicError::deadline_exceeded();
        let me: ModelError = original.into();
        assert_eq!(me.to_string(), "context deadline exceeded");
        let back = AnthropicError::from_model_error(me);
        assert!(back.is_deadline_exceeded());

        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let e = AnthropicError::from_model_error(ModelError::Transport(Box::new(io)));
        assert!(e.is_transport());
        assert_eq!(
            e.to_string(),
            "anthropicmodel: transport error: connection refused"
        );

        let e = AnthropicError::from_model_error(ModelError::EmptyResponse);
        assert_eq!(
            e.to_string(),
            "anthropicmodel: request failed: empty response"
        );

        let e = AnthropicError::from_model_error(ModelError::Other("boom".into()));
        assert_eq!(e.to_string(), "anthropicmodel: request failed: boom");
    }
}
