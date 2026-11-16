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

//! Errors (port of ADK-Go `model/openaimodel/internal/shared/errors.go` and `errors.go`, plus
//! openai-go v3.91.1 `internal/apierror`'s `Error()` text).
//!
//! Go's 26 sentinels become [`ErrorKind`]; an [`OpenAiError`] carries the kinds it wraps, in
//! Go's `%w` order, and renders Go's `Error()` text as its `Display`. [`OpenAiError::is`]
//! replaces `errors.Is`, [`OpenAiError::status`] replaces `errors.As(err, &*openai.Error)`.

use std::fmt;

use bytes::Bytes;
use gadk_model::rest::RestError;
use gadk_model::{ModelError, error_chain, transport_error_detail};

/// One Go sentinel error of `openaimodel` (`internal/shared/errors.go`, re-exported by
/// `errors.go`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Go `ErrModelNameRequired`: a model name is not provided.
    ModelNameRequired,
    /// Go `ErrUnsupportedAPI`: `ClientConfig.API` names an API this package does not implement.
    UnsupportedApi,
    /// Go `ErrNoChoices`: a Chat Completions response carries no choices.
    NoChoices,
    /// Go `ErrRequestNil`: the provided request is nil.
    RequestNil,
    /// Go `ErrNoContents`: the LLM request has no contents.
    NoContents,
    /// Go `ErrFunctionCallMissingName`: a function call is missing a name.
    FunctionCallMissingName,
    /// Go `ErrTopKNotSupported`: TopK is used.
    TopKNotSupported,
    /// Go `ErrStopSequencesNotSupported`: stop sequences are used with the Responses API.
    StopSequencesNotSupported,
    /// Go `ErrMultipleCandidatesNotSupported`: multiple candidates are requested.
    MultipleCandidatesNotSupported,
    /// Go `ErrPenaltiesNotSupported`: frequency/presence penalties are used with the Responses
    /// API.
    PenaltiesNotSupported,
    /// Go `ErrLabelsNotSupported`: request labels are used.
    LabelsNotSupported,
    /// Go `ErrSafetySettingsNotSupported`: Gemini safety settings are used.
    SafetySettingsNotSupported,
    /// Go `ErrUnsupportedMIMEType`: an unsupported MIME type is used.
    UnsupportedMimeType,
    /// Go `ErrMediaMIMETypeRequired`: a part carries media without a MIME type.
    MediaMimeTypeRequired,
    /// Go `ErrMediaDataRequired`: a part carries inline media with no bytes.
    MediaDataRequired,
    /// Go `ErrMediaURIRequired`: a part carries file data without a URI.
    MediaUriRequired,
    /// Go `ErrMediaOnAssistantTurn`: a replayed assistant turn carries media.
    MediaOnAssistantTurn,
    /// Go `ErrUnsupportedConfigField`: a generation config field has no equivalent on the
    /// selected API; the message names it.
    UnsupportedConfigField,
    /// Go `ErrEmptyJSONSchema`: an empty JSON schema is provided.
    EmptyJsonSchema,
    /// Go `ErrEmptyResponse`: the OpenAI API returns an empty response.
    EmptyResponse,
    /// Go `ErrNoOutputItems`: the response contains no output items.
    NoOutputItems,
    /// Go `ErrResponseFailed`: the server reports the response itself as failed.
    ResponseFailed,
    /// Go `ErrUnsupportedMessageContentType`: an unsupported message content type is used.
    UnsupportedMessageContentType,
    /// Go `ErrUnsupportedOutputItemType`: an unsupported output item type is used.
    UnsupportedOutputItemType,
    /// Go `ErrFunctionCallArgs`: a function call's arguments are not a decodable JSON object.
    FunctionCallArgs,
    /// Go `ErrNoTextOrToolContent`: the response output does not contain text or tool content.
    NoTextOrToolContent,
}

impl ErrorKind {
    /// Every kind, in Go's declaration order (`internal/shared/errors.go`).
    pub const ALL: [ErrorKind; 26] = [
        Self::ModelNameRequired,
        Self::UnsupportedApi,
        Self::NoChoices,
        Self::RequestNil,
        Self::NoContents,
        Self::FunctionCallMissingName,
        Self::TopKNotSupported,
        Self::StopSequencesNotSupported,
        Self::MultipleCandidatesNotSupported,
        Self::PenaltiesNotSupported,
        Self::LabelsNotSupported,
        Self::SafetySettingsNotSupported,
        Self::UnsupportedMimeType,
        Self::MediaMimeTypeRequired,
        Self::MediaDataRequired,
        Self::MediaUriRequired,
        Self::MediaOnAssistantTurn,
        Self::UnsupportedConfigField,
        Self::EmptyJsonSchema,
        Self::EmptyResponse,
        Self::NoOutputItems,
        Self::ResponseFailed,
        Self::UnsupportedMessageContentType,
        Self::UnsupportedOutputItemType,
        Self::FunctionCallArgs,
        Self::NoTextOrToolContent,
    ];

    /// The sentinel's Go `Error()` text.
    pub fn sentinel_text(self) -> &'static str {
        match self {
            Self::ModelNameRequired => "openai: model name is required",
            Self::UnsupportedApi => "openai: unsupported API",
            Self::NoChoices => "openai: response included no choices",
            Self::RequestNil => "openai: request is nil",
            Self::NoContents => "openai: LLM request has no contents to convert",
            Self::FunctionCallMissingName => "openai: function call missing name",
            Self::TopKNotSupported => "openai: topK is not supported",
            Self::StopSequencesNotSupported => "openai: stop sequences are not supported",
            Self::MultipleCandidatesNotSupported => {
                "openai: multiple candidates per request are not supported"
            }
            Self::PenaltiesNotSupported => "openai: frequency/presence penalties are not supported",
            Self::LabelsNotSupported => "openai: request labels are not supported",
            Self::SafetySettingsNotSupported => "openai: gemini safety settings are not supported",
            Self::UnsupportedMimeType => "openai: unsupported mime type",
            Self::MediaMimeTypeRequired => "openai: media part is missing a mime type",
            Self::MediaDataRequired => "openai: inline media part carries no data",
            Self::MediaUriRequired => "openai: file data part is missing a uri",
            Self::MediaOnAssistantTurn => {
                "openai: media on a replayed assistant turn is not supported"
            }
            Self::UnsupportedConfigField => "openai: unsupported generation config field",
            Self::EmptyJsonSchema => "openai: empty json schema",
            Self::EmptyResponse => "openai: empty response",
            Self::NoOutputItems => "openai: response included no output items",
            Self::ResponseFailed => "openai: response failed",
            Self::UnsupportedMessageContentType => "openai: unsupported message content type",
            Self::UnsupportedOutputItemType => "openai: unsupported output item type",
            Self::FunctionCallArgs => "openai: parse function call args",
            Self::NoTextOrToolContent => {
                "openai: response output did not contain text or tool content"
            }
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentinel_text())
    }
}

/// An HTTP error response from the API (openai-go `*openai.Error`): the final response of a
/// call whose status is 400 or above, after retries.
///
/// `Display` is openai-go's `Error()`: `OpenAI API error: <status> <Go http.StatusText>`, or
/// `OpenAI API error: <status>` when Go has no text for the status. The body is never part of
/// the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// The HTTP status code.
    pub status: u16,
    /// The response body as received.
    pub body: Bytes,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match status_text(self.status) {
            Some(text) => write!(f, "OpenAI API error: {} {text}", self.status),
            None => write!(f, "OpenAI API error: {}", self.status),
        }
    }
}

impl std::error::Error for ApiError {}

/// Go `net/http.StatusText`; `None` where Go returns `""`.
fn status_text(code: u16) -> Option<&'static str> {
    Some(match code {
        100 => "Continue",
        101 => "Switching Protocols",
        102 => "Processing",
        103 => "Early Hints",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Request Entity Too Large",
        414 => "Request URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Requested Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => return None,
    })
}

/// Where an [`OpenAiError`] comes from; decides the accessors that are true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A sentinel, bare or wrapped (`fmt.Errorf("%w…", sentinel…)`).
    Sentinel,
    /// An error text Go builds without a sentinel (`errors.New`, `fmt.Errorf` without `%w`).
    Plain,
    /// An [`ApiError`].
    Api,
    /// No response was received (Go's `*url.Error` from `http.Client.Do`).
    Transport,
    /// No response headers arrived within the default client's response-header timeout
    /// (Go `net/http: timeout awaiting response headers`).
    Timeout,
    /// Go `context.DeadlineExceeded`: `http_options.timeout` expired.
    DeadlineExceeded,
    /// Go `context.Canceled`. gadk cancels by dropping the stream (D9), so no path builds it
    /// today; kept so the accessor exists for callers matching Go's `errors.Is`.
    Canceled,
}

/// The error type of `gadk-openai`: Go's sentinel-wrapping errors, openai-go's API errors,
/// and the transport, timeout and deadline failures of a call.
///
/// `Display` is Go's `Error()` text. [`is`](Self::is) replaces Go's `errors.Is` for the
/// sentinels; [`status`](Self::status) and [`api_error`](Self::api_error) replace
/// `errors.As(err, &*openai.Error)`. Wrapping (`openai: call failed: %w` and the like) keeps
/// the kinds, the API error and the origin.
#[derive(Debug, Clone)]
pub struct OpenAiError {
    kinds: Vec<ErrorKind>,
    message: String,
    api: Option<Box<ApiError>>,
    origin: Origin,
}

impl fmt::Display for OpenAiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OpenAiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.api
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl OpenAiError {
    /// The sentinels this error wraps, outermost first (Go `%w` order).
    pub fn kinds(&self) -> &[ErrorKind] {
        &self.kinds
    }

    /// Go `errors.Is(err, <sentinel>)`.
    pub fn is(&self, kind: ErrorKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// The HTTP status of an API error (Go `errors.As(err, &apiErr)` then `apiErr.StatusCode`).
    pub fn status(&self) -> Option<u16> {
        self.api.as_ref().map(|e| e.status)
    }

    /// Go `errors.As(err, &*openai.Error)`.
    pub fn api_error(&self) -> Option<&ApiError> {
        self.api.as_deref()
    }

    /// Go `errors.Is(err, context.DeadlineExceeded)`.
    pub fn is_deadline_exceeded(&self) -> bool {
        self.origin == Origin::DeadlineExceeded
    }

    /// Go `errors.Is(err, context.Canceled)`.
    pub fn is_canceled(&self) -> bool {
        self.origin == Origin::Canceled
    }

    /// True when no response was received (a connection, DNS, TLS or request-building error).
    pub fn is_transport(&self) -> bool {
        matches!(self.origin, Origin::Transport | Origin::Timeout)
    }

    /// True for the default client's response-header timeout (Go's `*url.Error` whose
    /// `Timeout()` is true).
    pub fn is_timeout(&self) -> bool {
        self.origin == Origin::Timeout
    }

    /// The bare sentinel (Go returns `ErrX` itself).
    pub(crate) fn sentinel(kind: ErrorKind) -> Self {
        Self {
            kinds: vec![kind],
            message: kind.sentinel_text().to_string(),
            api: None,
            origin: Origin::Sentinel,
        }
    }

    /// Go `fmt.Errorf("%w: <detail>", kind)`.
    pub(crate) fn new(kind: ErrorKind, detail: impl fmt::Display) -> Self {
        Self::sentinel(kind).suffixed(format!(": {detail}"))
    }

    /// An error Go builds without a sentinel (`errors.New`, or `fmt.Errorf` with no `%w` of
    /// a sentinel).
    pub(crate) fn plain(message: impl Into<String>) -> Self {
        Self {
            kinds: Vec::new(),
            message: message.into(),
            api: None,
            origin: Origin::Plain,
        }
    }

    /// openai-go's `*openai.Error` for a final response with status ≥ 400.
    pub(crate) fn api(status: u16, body: Bytes) -> Self {
        let api = ApiError { status, body };
        Self {
            kinds: Vec::new(),
            message: api.to_string(),
            api: Some(Box::new(api)),
            origin: Origin::Api,
        }
    }

    /// A failure to get a response (Go's `*url.Error` cause; the `Post "<url>": ` prefix Go's
    /// text carries is not reproduced, so no URL reaches an error).
    pub(crate) fn transport(detail: impl Into<String>) -> Self {
        Self {
            kinds: Vec::new(),
            message: detail.into(),
            api: None,
            origin: Origin::Transport,
        }
    }

    /// Go `net/http: timeout awaiting response headers` (the default client's 10-minute
    /// response-header timeout).
    pub(crate) fn response_header_timeout() -> Self {
        Self {
            kinds: Vec::new(),
            message: RestError::ResponseHeaderTimeout.to_string(),
            api: None,
            origin: Origin::Timeout,
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

    /// Go `context.Canceled`.
    #[allow(dead_code)] // gadk cancels by drop (D9); kept for Go's `errors.Is(err, context.Canceled)`
    pub(crate) fn canceled() -> Self {
        Self {
            kinds: Vec::new(),
            message: "context canceled".to_string(),
            api: None,
            origin: Origin::Canceled,
        }
    }

    /// Go `fmt.Errorf("<prefix>%w", err)`: kinds, API error and origin are kept.
    pub(crate) fn prefixed(mut self, prefix: impl fmt::Display) -> Self {
        self.message = format!("{prefix}{}", self.message);
        self
    }

    /// Go `fmt.Errorf("%w<suffix>", err)`: kinds, API error and origin are kept.
    pub(crate) fn suffixed(mut self, suffix: impl fmt::Display) -> Self {
        self.message = format!("{}{suffix}", self.message);
        self
    }

    /// Go `fmt.Errorf("openai: call failed: %w", err)` (the blocking paths' wrap of a client
    /// error).
    pub(crate) fn call_failed(self) -> Self {
        self.prefixed("openai: call failed: ")
    }

    /// A response body's read error, as openai-go reports it (a streaming body's error, or a
    /// final ≥ 400 body that failed to read): the body stream's own text.
    /// - `ModelError::Other(RestError::DeadlineExceeded)` (a `CallTimeout::Call` deadline
    ///   passing while the body streams) → [`deadline_exceeded`](Self::deadline_exceeded);
    /// - `ModelError::Other(OpenAiError)` → that error;
    /// - `ModelError::Transport(cause)` → the cause's text ([`transport_error_detail`]), as
    ///   a transport error;
    /// - anything else → its `Display`, as a plain error.
    pub(crate) fn from_body_error(err: ModelError) -> Self {
        match err {
            ModelError::Other(boxed) => match boxed.downcast::<RestError>() {
                Ok(rest) => Self::from(*rest),
                Err(other) => match other.downcast::<OpenAiError>() {
                    Ok(e) => *e,
                    Err(other) => Self::plain(error_chain(other.as_ref())),
                },
            },
            ModelError::Transport(boxed) => Self::transport(transport_error_detail(boxed)),
            other => Self::plain(other.to_string()),
        }
    }
}

/// Every [`OpenAiError`] becomes [`ModelError::Other`], so `Display` is unchanged and the typed
/// error is recovered by downcasting (`boxed.downcast_ref::<OpenAiError>()`).
impl From<OpenAiError> for ModelError {
    fn from(err: OpenAiError) -> Self {
        ModelError::Other(Box::new(err))
    }
}

/// The shared transport's failures in openai-go's texts (`RequestConfig.Execute`):
/// - no response ([`RestError::Transport`]) → a transport error with the cause's text (a
///   `reqwest::Error` without its URL);
/// - a header failure ([`RestError::Headers`]) → the boxed [`OpenAiError`] as it is, else a
///   transport error;
/// - [`RestError::ResponseHeaderTimeout`] → `net/http: timeout awaiting response headers`;
/// - [`RestError::DeadlineExceeded`] → `context deadline exceeded`;
/// - [`RestError::DroppedBody`] → the bare read error for a final status ≥ 400, else
///   `error reading response body: <err>` (openai-go's two texts).
impl From<RestError> for OpenAiError {
    fn from(err: RestError) -> Self {
        match err {
            RestError::Transport(ModelError::Transport(boxed)) => {
                Self::transport(transport_error_detail(boxed))
            }
            RestError::Transport(other) => Self::transport(other.to_string()),
            RestError::Headers(boxed) => match boxed.downcast::<OpenAiError>() {
                Ok(e) => *e,
                Err(other) => Self::transport(error_chain(other.as_ref())),
            },
            RestError::ResponseHeaderTimeout => Self::response_header_timeout(),
            RestError::DeadlineExceeded => Self::deadline_exceeded(),
            RestError::DroppedBody { parts, source } => {
                let read = Self::from_body_error(source);
                if parts.status.as_u16() >= 400 {
                    read
                } else {
                    read.prefixed("error reading response body: ")
                }
            }
        }
    }
}

/// Go `encoding/json`'s text for a serde_json error, where a Go test asserts it (P3): a
/// truncated document is `unexpected end of JSON input`; every other error keeps serde_json's
/// wording (an accepted divergence, ledger 3b).
pub(crate) fn go_json_error(err: &serde_json::Error) -> String {
    if err.is_eof() {
        "unexpected end of JSON input".to_string()
    } else {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use pretty_assertions::assert_eq;

    /// Every sentinel's text, copied from Go `internal/shared/errors.go`.
    #[test]
    fn sentinel_texts_are_gos() {
        let want = [
            "openai: model name is required",
            "openai: unsupported API",
            "openai: response included no choices",
            "openai: request is nil",
            "openai: LLM request has no contents to convert",
            "openai: function call missing name",
            "openai: topK is not supported",
            "openai: stop sequences are not supported",
            "openai: multiple candidates per request are not supported",
            "openai: frequency/presence penalties are not supported",
            "openai: request labels are not supported",
            "openai: gemini safety settings are not supported",
            "openai: unsupported mime type",
            "openai: media part is missing a mime type",
            "openai: inline media part carries no data",
            "openai: file data part is missing a uri",
            "openai: media on a replayed assistant turn is not supported",
            "openai: unsupported generation config field",
            "openai: empty json schema",
            "openai: empty response",
            "openai: response included no output items",
            "openai: response failed",
            "openai: unsupported message content type",
            "openai: unsupported output item type",
            "openai: parse function call args",
            "openai: response output did not contain text or tool content",
        ];
        let got: Vec<String> = ErrorKind::ALL.iter().map(|k| k.to_string()).collect();
        assert_eq!(got, want);
        for kind in ErrorKind::ALL {
            let err = OpenAiError::sentinel(kind);
            assert_eq!(err.to_string(), kind.sentinel_text());
            assert!(err.is(kind));
            assert_eq!(err.kinds(), &[kind]);
        }
    }

    #[test]
    fn wrapping_keeps_kinds_and_renders_gos_text() {
        let err = OpenAiError::new(ErrorKind::UnsupportedConfigField, "ThinkingConfig.Foo");
        assert_eq!(
            err.to_string(),
            "openai: unsupported generation config field: ThinkingConfig.Foo"
        );
        let err = OpenAiError::sentinel(ErrorKind::FunctionCallMissingName)
            .suffixed(" (call_id \"c1\")")
            .prefixed("openai: system instruction: ");
        assert_eq!(
            err.to_string(),
            "openai: system instruction: openai: function call missing name (call_id \"c1\")"
        );
        assert!(err.is(ErrorKind::FunctionCallMissingName));
        assert!(!err.is(ErrorKind::NoContents));

        let plain = OpenAiError::plain("openai: tool 0 is nil");
        assert!(plain.kinds().is_empty());
        assert_eq!(plain.to_string(), "openai: tool 0 is nil");
    }

    /// openai-go `(*apierror.Error).Error()`: Go `http.StatusText`, or no text.
    #[test]
    fn api_error_texts_use_gos_status_text() {
        for (status, want) in [
            (400, "OpenAI API error: 400 Bad Request"),
            (413, "OpenAI API error: 413 Request Entity Too Large"),
            (418, "OpenAI API error: 418 I'm a teapot"),
            (429, "OpenAI API error: 429 Too Many Requests"),
            (500, "OpenAI API error: 500 Internal Server Error"),
            (529, "OpenAI API error: 529"),
            (599, "OpenAI API error: 599"),
        ] {
            let err = OpenAiError::api(status, Bytes::from_static(b"{\"error\":{}}"));
            assert_eq!(err.to_string(), want);
            assert_eq!(err.status(), Some(status));
            assert_eq!(
                err.api_error().unwrap().body,
                Bytes::from_static(b"{\"error\":{}}")
            );
            let wrapped = err.call_failed();
            assert_eq!(wrapped.to_string(), format!("openai: call failed: {want}"));
            assert_eq!(wrapped.status(), Some(status));
            assert!(std::error::Error::source(&wrapped).is_some());
        }
        assert_eq!(OpenAiError::plain("x").status(), None);
    }

    #[test]
    fn rest_errors_map_to_openai_go_texts() {
        let deadline = OpenAiError::from(RestError::DeadlineExceeded).call_failed();
        assert_eq!(
            deadline.to_string(),
            "openai: call failed: context deadline exceeded"
        );
        assert!(deadline.is_deadline_exceeded());
        assert!(!deadline.is_transport());

        let header = OpenAiError::from(RestError::ResponseHeaderTimeout);
        assert_eq!(
            header.to_string(),
            "net/http: timeout awaiting response headers"
        );
        assert!(header.is_timeout() && header.is_transport());

        let refused = OpenAiError::from(RestError::Transport(ModelError::Transport(
            "connection refused".into(),
        )));
        assert_eq!(refused.to_string(), "connection refused");
        assert!(refused.is_transport());

        let boxed = RestError::Headers(Box::new(OpenAiError::plain("bad header")));
        assert_eq!(OpenAiError::from(boxed).to_string(), "bad header");

        let parts = |status: u16| {
            let (parts, ()) = http::Response::builder()
                .status(status)
                .body(())
                .unwrap()
                .into_parts();
            Box::new(parts)
        };
        let dropped = |status| RestError::DroppedBody {
            parts: parts(status),
            source: ModelError::Transport("unexpected EOF".into()),
        };
        assert_eq!(
            OpenAiError::from(dropped(500)).to_string(),
            "unexpected EOF"
        );
        assert_eq!(
            OpenAiError::from(dropped(200)).to_string(),
            "error reading response body: unexpected EOF"
        );
    }

    #[test]
    fn body_errors_unwrap_deadlines_and_openai_errors() {
        let deadline = ModelError::Other(Box::new(RestError::DeadlineExceeded));
        assert!(OpenAiError::from_body_error(deadline).is_deadline_exceeded());
        let inner = ModelError::from(OpenAiError::sentinel(ErrorKind::NoChoices));
        assert!(OpenAiError::from_body_error(inner).is(ErrorKind::NoChoices));
        let eof = ModelError::Transport("unexpected EOF".into());
        let err = OpenAiError::from_body_error(eof);
        assert_eq!(err.to_string(), "unexpected EOF");
        assert!(err.is_transport());
    }

    #[test]
    fn model_error_round_trips_by_downcast() {
        let err: ModelError = OpenAiError::sentinel(ErrorKind::NoContents).into();
        assert_eq!(
            err.to_string(),
            "openai: LLM request has no contents to convert"
        );
        let ModelError::Other(boxed) = err else {
            panic!("want ModelError::Other");
        };
        assert!(
            boxed
                .downcast_ref::<OpenAiError>()
                .unwrap()
                .is(ErrorKind::NoContents)
        );
    }

    #[test]
    fn go_json_error_maps_eof_only() {
        let eof = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        assert_eq!(go_json_error(&eof), "unexpected end of JSON input");
        let empty = serde_json::from_str::<serde_json::Value>("").unwrap_err();
        assert_eq!(go_json_error(&empty), "unexpected end of JSON input");
        let syntax = serde_json::from_str::<serde_json::Value>("{x").unwrap_err();
        assert_eq!(go_json_error(&syntax), syntax.to_string());
    }

    #[test]
    fn canceled_is_reported() {
        let err = OpenAiError::canceled();
        assert!(err.is_canceled());
        assert_eq!(err.to_string(), "context canceled");
    }
}
