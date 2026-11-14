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

//! [`Error`]: the crate's error type, carrying Go's error texts and Go's `%w` / `errors.Join`
//! chains, plus the Go error classifiers the package uses: `hasErrorCode`, `isNotFoundError`,
//! `isConflictError` and `isPreconditionFailedError` (from `plugin/agentanalytics/schema_upgrade.go`),
//! `status.Code` (grpc-go), and the texts of `bigquery.Error` and grpc-go's status errors.
//! [`ApiError`] / [`ErrorItem`] (Go `*googleapi.Error`) are `gadk_model::rest::googleapi`'s,
//! re-exported here.

use std::error::Error as StdError;
use std::fmt;

use gadk_model::rest::RestError;
pub use gadk_model::rest::googleapi::{ApiError, ErrorItem};
use gadk_platform::go_quote;

use crate::context::ContextError;
use crate::schema::SchemaMismatchError;

/// A boxed error cause (a [`ContentFormatter`](crate::config::ContentFormatter)'s error, a caller's
/// offloader error, …).
pub type BoxError = Box<dyn StdError + Send + Sync>;

/// A gRPC status code (grpc-go `codes.Code`). `Display` is Go's `Code.String()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    /// `codes.OK` (0).
    Ok,
    /// `codes.Canceled` (1).
    Canceled,
    /// `codes.Unknown` (2).
    Unknown,
    /// `codes.InvalidArgument` (3).
    InvalidArgument,
    /// `codes.DeadlineExceeded` (4).
    DeadlineExceeded,
    /// `codes.NotFound` (5).
    NotFound,
    /// `codes.AlreadyExists` (6).
    AlreadyExists,
    /// `codes.PermissionDenied` (7).
    PermissionDenied,
    /// `codes.ResourceExhausted` (8).
    ResourceExhausted,
    /// `codes.FailedPrecondition` (9).
    FailedPrecondition,
    /// `codes.Aborted` (10).
    Aborted,
    /// `codes.OutOfRange` (11).
    OutOfRange,
    /// `codes.Unimplemented` (12).
    Unimplemented,
    /// `codes.Internal` (13).
    Internal,
    /// `codes.Unavailable` (14).
    Unavailable,
    /// `codes.DataLoss` (15).
    DataLoss,
    /// `codes.Unauthenticated` (16).
    Unauthenticated,
}

impl Code {
    /// The numeric code (grpc-go `uint32(code)`).
    pub fn as_u32(self) -> u32 {
        match self {
            Code::Ok => 0,
            Code::Canceled => 1,
            Code::Unknown => 2,
            Code::InvalidArgument => 3,
            Code::DeadlineExceeded => 4,
            Code::NotFound => 5,
            Code::AlreadyExists => 6,
            Code::PermissionDenied => 7,
            Code::ResourceExhausted => 8,
            Code::FailedPrecondition => 9,
            Code::Aborted => 10,
            Code::OutOfRange => 11,
            Code::Unimplemented => 12,
            Code::Internal => 13,
            Code::Unavailable => 14,
            Code::DataLoss => 15,
            Code::Unauthenticated => 16,
        }
    }

    /// The code for a number; unknown numbers are `None` (grpc-go prints them as `Code(n)`).
    pub fn from_u32(n: u32) -> Option<Code> {
        Some(match n {
            0 => Code::Ok,
            1 => Code::Canceled,
            2 => Code::Unknown,
            3 => Code::InvalidArgument,
            4 => Code::DeadlineExceeded,
            5 => Code::NotFound,
            6 => Code::AlreadyExists,
            7 => Code::PermissionDenied,
            8 => Code::ResourceExhausted,
            9 => Code::FailedPrecondition,
            10 => Code::Aborted,
            11 => Code::OutOfRange,
            12 => Code::Unimplemented,
            13 => Code::Internal,
            14 => Code::Unavailable,
            15 => Code::DataLoss,
            16 => Code::Unauthenticated,
            _ => return None,
        })
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Code::Ok => "OK",
            Code::Canceled => "Canceled",
            Code::Unknown => "Unknown",
            Code::InvalidArgument => "InvalidArgument",
            Code::DeadlineExceeded => "DeadlineExceeded",
            Code::NotFound => "NotFound",
            Code::AlreadyExists => "AlreadyExists",
            Code::PermissionDenied => "PermissionDenied",
            Code::ResourceExhausted => "ResourceExhausted",
            Code::FailedPrecondition => "FailedPrecondition",
            Code::Aborted => "Aborted",
            Code::OutOfRange => "OutOfRange",
            Code::Unimplemented => "Unimplemented",
            Code::Internal => "Internal",
            Code::Unavailable => "Unavailable",
            Code::DataLoss => "DataLoss",
            Code::Unauthenticated => "Unauthenticated",
        })
    }
}

/// A BigQuery job error (Go `*bigquery.Error`, from a job status's `errorResult` / `errors`).
/// `Display` is Go's `Error()`: `{Location: "…"; Message: "…"; Reason: "…"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BigQueryError {
    /// `location`.
    pub location: String,
    /// `message`.
    pub message: String,
    /// `reason`.
    pub reason: String,
}

impl fmt::Display for BigQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{Location: {}; Message: {}; Reason: {}}}",
            go_quote(&self.location),
            go_quote(&self.message),
            go_quote(&self.reason)
        )
    }
}

/// The crate's error. `Display` is the Go error's `Error()` text; [`Error::Wrapped`] and
/// [`Error::Joined`] keep Go's `%w` / `errors.Join` chains so [`Error::is_context`],
/// [`has_error_code`] and [`status_code`] see what Go's `errors.Is` / `errors.As` see.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Go `errors.New` / `fmt.Errorf` (without `%w`) text.
    #[error("{0}")]
    Message(String),
    /// A Go `fmt.Errorf` with one `%w`: `text` is the whole formatted message, `source` the
    /// wrapped error (Go `Unwrap`).
    #[error("{text}")]
    Wrapped {
        /// The full error text.
        text: String,
        /// The wrapped error.
        #[source]
        source: Box<Error>,
    },
    /// Go `errors.Join(errs...)` of at least one error: the texts joined by `\n`.
    #[error("{}", join_texts(.0))]
    Joined(Vec<Error>),
    /// A context error (`context.Canceled`, `context.DeadlineExceeded`).
    #[error("{0}")]
    Context(ContextError),
    /// A gRPC status error (grpc-go `status.Error(code, message)`): `rpc error: code = … desc = …`.
    /// 7b-2's tonic errors map into it.
    #[error("rpc error: code = {code} desc = {message}")]
    Status {
        /// The status code.
        code: Code,
        /// The status message.
        message: String,
    },
    /// A Google API HTTP error (Go `*googleapi.Error`).
    #[error("{0}")]
    Api(Box<ApiError>),
    /// A BigQuery job error (Go `*bigquery.Error`).
    #[error("{0}")]
    BigQuery(BigQueryError),
    /// An incompatible field between an existing and the desired table schema (Go
    /// `*schemaMismatchError`).
    #[error("{0}")]
    SchemaMismatch(SchemaMismatchError),
    /// The REST transport failed before a response ended the call.
    #[error("{0}")]
    Rest(RestError),
    /// An Arrow schema or IPC error.
    #[error("{0}")]
    Arrow(#[from] arrow_schema::ArrowError),
    /// Any other error (a caller's offloader or formatter error, a test fake's error).
    #[error("{0}")]
    Other(#[source] BoxError),
}

fn join_texts(errs: &[Error]) -> String {
    errs.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

impl Error {
    /// Go `errors.New(text)` / `fmt.Errorf` without `%w`.
    pub fn msg(text: impl Into<String>) -> Error {
        Error::Message(text.into())
    }

    /// Go `fmt.Errorf("{prefix}: %w", source)`.
    pub fn wrap(prefix: impl fmt::Display, source: Error) -> Error {
        Error::Wrapped {
            text: format!("{prefix}: {source}"),
            source: Box::new(source),
        }
    }

    /// Go `fmt.Errorf` with one `%w` placed anywhere: `text` is the complete message (the
    /// caller formats `source` into it), `source` what `Unwrap` returns.
    pub fn wrap_text(text: impl Into<String>, source: Error) -> Error {
        Error::Wrapped {
            text: text.into(),
            source: Box::new(source),
        }
    }

    /// Go `errors.Join(errs...)`: `None` (Go `nil`) when `errs` is empty.
    pub fn join(errs: Vec<Error>) -> Option<Error> {
        if errs.is_empty() {
            None
        } else {
            Some(Error::Joined(errs))
        }
    }

    /// Wraps any error as [`Error::Other`]; an [`Error`] is returned as is and a
    /// [`ContextError`] as [`Error::Context`], so the tree walks of [`Error::find`] see them.
    pub fn other(err: impl Into<BoxError>) -> Error {
        let boxed: BoxError = err.into();
        let boxed = match boxed.downcast::<Error>() {
            Ok(inner) => return *inner,
            Err(boxed) => boxed,
        };
        match boxed.downcast::<ContextError>() {
            Ok(ctx) => Error::Context(*ctx),
            Err(boxed) => Error::Other(boxed),
        }
    }

    /// Go `errors.Is(err, context.Canceled)` / `errors.Is(err, context.DeadlineExceeded)`:
    /// whether `target` is in this error's tree.
    pub fn is_context(&self, target: ContextError) -> bool {
        self.find(&mut |e| matches!(e, Error::Context(c) if *c == target))
            .is_some()
    }

    /// Go `errors.Is`/`errors.As` traversal: the first error of this error's tree (pre-order,
    /// depth first, as Go walks `Unwrap() error` and `Unwrap() []error`) for which `pred` holds.
    pub fn find<'a>(&'a self, pred: &mut dyn FnMut(&Error) -> bool) -> Option<&'a Error> {
        if pred(self) {
            return Some(self);
        }
        match self {
            Error::Wrapped { source, .. } => source.find(pred),
            Error::Joined(errs) => errs.iter().find_map(|e| e.find(pred)),
            // Go `errors.As` walks every `Unwrap`: follow the foreign `source()` chain to the
            // first `Error`, whose own tree the recursion walks. A bare `ContextError` link
            // (Go `fmt.Errorf("…: %w", ctx.Err())`) is tested as the matching `Error::Context`.
            Error::Other(boxed) => {
                static CANCELED: Error = Error::Context(ContextError::Canceled);
                static DEADLINE_EXCEEDED: Error = Error::Context(ContextError::DeadlineExceeded);
                let mut link: Option<&(dyn StdError + 'static)> = Some(boxed.as_ref());
                while let Some(e) = link {
                    if let Some(inner) = e.downcast_ref::<Error>() {
                        return inner.find(pred);
                    }
                    if let Some(ctx) = e.downcast_ref::<ContextError>() {
                        let as_error: &'static Error = match ctx {
                            ContextError::Canceled => &CANCELED,
                            ContextError::DeadlineExceeded => &DEADLINE_EXCEEDED,
                        };
                        return pred(as_error).then_some(as_error);
                    }
                    link = e.source();
                }
                None
            }
            _ => None,
        }
    }
}

impl From<ContextError> for Error {
    fn from(err: ContextError) -> Self {
        Error::Context(err)
    }
}

impl From<RestError> for Error {
    fn from(err: RestError) -> Self {
        Error::Rest(err)
    }
}

impl From<ApiError> for Error {
    fn from(err: ApiError) -> Self {
        Error::Api(Box::new(err))
    }
}

/// Go `hasErrorCode`: the HTTP status of the first `*googleapi.Error` in `err`'s tree equals
/// `http_code`; when there is none, the code of the first gRPC status in the tree equals
/// `grpc_code`.
pub(crate) fn has_error_code(err: &Error, http_code: u16, grpc_code: Code) -> bool {
    if let Some(Error::Api(api)) = err.find(&mut |e| matches!(e, Error::Api(_))) {
        return api.code == http_code;
    }
    if let Some(Error::Status { code, .. }) = err.find(&mut |e| matches!(e, Error::Status { .. })) {
        return *code == grpc_code;
    }
    false
}

/// Go `isNotFoundError`: HTTP 404 or gRPC `NotFound`.
pub(crate) fn is_not_found_error(err: &Error) -> bool {
    has_error_code(err, 404, Code::NotFound)
}

/// Go `isConflictError`: HTTP 409 or gRPC `AlreadyExists`.
pub(crate) fn is_conflict_error(err: &Error) -> bool {
    has_error_code(err, 409, Code::AlreadyExists)
}

/// Go `isPreconditionFailedError`: HTTP 412 or gRPC `FailedPrecondition`.
pub(crate) fn is_precondition_failed_error(err: &Error) -> bool {
    has_error_code(err, 412, Code::FailedPrecondition)
}

/// grpc-go `status.Code(err)` for a non-nil error: the code of the first gRPC status in `err`'s
/// tree, else `Unknown`.
pub(crate) fn status_code(err: &Error) -> Code {
    match err.find(&mut |e| matches!(e, Error::Status { .. })) {
        Some(Error::Status { code, .. }) => *code,
        _ => Code::Unknown,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn api(code: u16, message: &str) -> Error {
        Error::from(ApiError {
            code,
            message: message.into(),
            ..ApiError::default()
        })
    }

    #[test]
    fn googleapi_error_texts() {
        let no_message = ApiError {
            code: 502,
            body: "bad gateway".into(),
            ..ApiError::default()
        };
        assert_eq!(
            no_message.to_string(),
            "googleapi: got HTTP response code 502 with body: bad gateway"
        );
        let plain = ApiError {
            code: 403,
            message: "forbidden".into(),
            ..ApiError::default()
        };
        assert_eq!(plain.to_string(), "googleapi: Error 403: forbidden");
        let one_item = ApiError {
            code: 404,
            message: "Not found: Table p:d.t".into(),
            errors: vec![ErrorItem {
                reason: "notFound".into(),
                message: "Not found: Table p:d.t".into(),
            }],
            ..ApiError::default()
        };
        assert_eq!(
            one_item.to_string(),
            "googleapi: Error 404: Not found: Table p:d.t, notFound"
        );
        let many = ApiError {
            code: 400,
            message: "bad".into(),
            errors: vec![
                ErrorItem {
                    reason: "invalid".into(),
                    message: "a".into(),
                },
                ErrorItem {
                    reason: "invalidQuery".into(),
                    message: "b".into(),
                },
            ],
            ..ApiError::default()
        };
        assert_eq!(
            many.to_string(),
            "googleapi: Error 400: bad\nMore details:\nReason: invalid, Message: a\nReason: \
             invalidQuery, Message: b\n"
        );
    }

    #[test]
    fn googleapi_error_details_are_go_indented_json() {
        let err = ApiError {
            code: 400,
            message: "bad".into(),
            details: vec![json!({"@type": "t<x>", "empty": {}, "list": [1, 2], "none": []})],
            ..ApiError::default()
        };
        assert_eq!(
            err.to_string(),
            "googleapi: Error 400: bad\nDetails:\n[\n  {\n    \"@type\": \"t\\u003cx\\u003e\",\n    \
             \"empty\": {},\n    \"list\": [\n      1,\n      2\n    ],\n    \"none\": []\n  }\n]"
        );
        let with_item = ApiError {
            errors: vec![ErrorItem {
                reason: "r".into(),
                message: "bad".into(),
            }],
            ..err
        };
        assert!(
            with_item.to_string().ends_with("  }\n]\n, r"),
            "{with_item}"
        );
    }

    #[test]
    fn status_bigquery_and_context_texts() {
        let st = Error::Status {
            code: Code::Canceled,
            message: "transport: stream canceled".into(),
        };
        assert_eq!(
            st.to_string(),
            "rpc error: code = Canceled desc = transport: stream canceled"
        );
        let bq = Error::BigQuery(BigQueryError {
            location: "query".into(),
            message: "Syntax error: \"x\"".into(),
            reason: "invalidQuery".into(),
        });
        assert_eq!(
            bq.to_string(),
            r#"{Location: "query"; Message: "Syntax error: \"x\""; Reason: "invalidQuery"}"#
        );
        assert_eq!(
            Error::from(ContextError::Canceled).to_string(),
            "context canceled"
        );
        assert_eq!(
            Error::from(ContextError::DeadlineExceeded).to_string(),
            "context deadline exceeded"
        );
    }

    #[test]
    fn wrap_and_join_keep_go_texts_and_chains() {
        let wrapped = Error::wrap(
            "failed to get BigQuery table t metadata",
            api(403, "forbidden"),
        );
        assert_eq!(
            wrapped.to_string(),
            "failed to get BigQuery table t metadata: googleapi: Error 403: forbidden"
        );
        assert!(wrapped.source().is_some());
        let text = Error::wrap_text(
            "view creation canceled during v: context canceled (boom)",
            Error::from(ContextError::Canceled),
        );
        assert!(text.is_context(ContextError::Canceled));
        assert!(!text.is_context(ContextError::DeadlineExceeded));
        let joined = Error::join(vec![Error::msg("a"), text]).unwrap();
        assert_eq!(
            joined.to_string(),
            "a\nview creation canceled during v: context canceled (boom)"
        );
        assert!(joined.is_context(ContextError::Canceled));
        assert!(Error::join(Vec::new()).is_none());
        let other = Error::other(Error::from(ContextError::DeadlineExceeded));
        assert!(other.is_context(ContextError::DeadlineExceeded));
    }

    #[test]
    fn has_error_code_matches_http_then_grpc() {
        assert!(is_not_found_error(&api(404, "nf")));
        assert!(is_conflict_error(&Error::wrap("x", api(409, "dup"))));
        assert!(is_precondition_failed_error(&api(412, "etag")));
        assert!(!is_not_found_error(&api(409, "dup")));
        let grpc = |code| Error::Status {
            code,
            message: String::new(),
        };
        assert!(is_not_found_error(&grpc(Code::NotFound)));
        assert!(is_conflict_error(&grpc(Code::AlreadyExists)));
        assert!(is_precondition_failed_error(&Error::wrap(
            "w",
            grpc(Code::FailedPrecondition)
        )));
        // A googleapi error in the tree decides, even when a status is there too.
        let both = Error::Joined(vec![api(500, "x"), grpc(Code::NotFound)]);
        assert!(!is_not_found_error(&both));
        assert!(!is_not_found_error(&Error::msg("Not found")));
    }

    /// A caller's error type whose `source()` is an [`Error`] (Go: a foreign error with an
    /// `Unwrap() error`).
    #[derive(Debug)]
    struct Foreign(Error);

    impl fmt::Display for Foreign {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "foreign: {}", self.0)
        }
    }

    impl StdError for Foreign {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn other_sees_boxed_context_and_foreign_source_chains() {
        let canceled = Error::other(ContextError::Canceled);
        assert!(canceled.is_context(ContextError::Canceled));
        assert!(!canceled.is_context(ContextError::DeadlineExceeded));
        assert_eq!(canceled.to_string(), "context canceled");

        let inner = Error::other(Error::msg("inner"));
        assert!(matches!(inner, Error::Message(_)), "{inner:?}");
        assert_eq!(inner.to_string(), "inner");

        let foreign = Error::other(Foreign(Error::wrap(
            "w",
            Error::Status {
                code: Code::NotFound,
                message: "gone".into(),
            },
        )));
        assert_eq!(
            foreign.to_string(),
            "foreign: w: rpc error: code = NotFound desc = gone"
        );
        assert_eq!(status_code(&foreign), Code::NotFound);
        assert!(has_error_code(&foreign, 404, Code::NotFound));
        assert!(is_not_found_error(&foreign));

        // Two foreign links deep, under a context error.
        let deep = Error::other(Foreign(Error::other(Foreign(Error::from(
            ContextError::DeadlineExceeded,
        )))));
        assert!(deep.is_context(ContextError::DeadlineExceeded));

        for target in [ContextError::Canceled, ContextError::DeadlineExceeded] {
            let other = match target {
                ContextError::Canceled => ContextError::DeadlineExceeded,
                ContextError::DeadlineExceeded => ContextError::Canceled,
            };
            // (i) A bare `ContextError` boxed straight into the variant.
            let direct = Error::Other(Box::new(target));
            assert!(direct.is_context(target), "{target:?}");
            assert!(!direct.is_context(other), "{target:?}");
            // (ii) A bare `ContextError` at the end of a foreign chain (Go
            // `fmt.Errorf("…: %w", ctx.Err())`).
            let chained = Error::other(RunnerErr(target));
            assert_eq!(chained.to_string(), format!("runner: {target}"));
            assert!(chained.is_context(target), "{target:?}");
            assert!(!chained.is_context(other), "{target:?}");
        }
    }

    /// A caller's error type whose `source()` is a bare [`ContextError`].
    #[derive(Debug)]
    struct RunnerErr(ContextError);

    impl fmt::Display for RunnerErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "runner: {}", self.0)
        }
    }

    impl StdError for RunnerErr {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn status_code_defaults_to_unknown() {
        assert_eq!(status_code(&Error::msg("x")), Code::Unknown);
        let wrapped = Error::wrap(
            "w",
            Error::Status {
                code: Code::Canceled,
                message: "c".into(),
            },
        );
        assert_eq!(status_code(&wrapped), Code::Canceled);
        for n in 0..=16 {
            assert_eq!(Code::from_u32(n).unwrap().as_u32(), n);
        }
        assert_eq!(Code::from_u32(17), None);
    }
}
