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

//! Per-user Google Cloud credentials from the Agent Identity and IAM Connector credential services.
//!
//! Given a resource name, a [`Client`] retrieves an end-user credential and maps it to a
//! [`crate::Credential`], polling while the service reports a non-interactive "pending" state and
//! surfacing interactive consent as a [`crate::ConsentRequiredError`].
//!
//! [`new_provider`] is the entry point for an agent: it returns a [`crate::CredentialProvider`]
//! that takes the acting user from the [`crate::AuthContext`], so a tool's outbound requests are
//! authenticated as the end user. [`Client`] is the transport underneath, usable on its own where
//! the caller already knows the user.
//!
//! The services have no generated client library and their surface is a single RPC, so the
//! client is hand-rolled over reqwest. Calls to the credential services are authenticated with
//! Application Default Credentials (cloud-platform scope) unless [`Config::http_client`] is set.

pub(crate) mod agent_identity;
pub(crate) mod client;
pub(crate) mod connector;
pub(crate) mod provider;
pub(crate) mod redact;

pub use client::{Client, Config, Request, Retrieval};
pub use provider::{ProviderConfig, ProviderScheme, new_provider};

use std::fmt;
use std::sync::Arc;

/// The five failures an [`Error`] can match directly; `Display`/`as_str` is the exact text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The user rejected consent: "gcp: user consent rejected"
    ConsentRejected,
    /// The service's response could not be decoded: "gcp: credentials service returned an
    /// undecodable response"
    MalformedResponse,
    /// Polling a pending retrieval timed out: "gcp: timed out waiting for credentials"
    PollTimeout,
    /// The default client could not be built: "gcp: default credentials client unavailable"
    ClientUnavailable,
    /// The call carries no acting user: "gcp: no acting user"
    NoActingUser,
}

impl ErrorKind {
    /// The sentinel's text.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::ConsentRejected => "gcp: user consent rejected",
            ErrorKind::MalformedResponse => {
                "gcp: credentials service returned an undecodable response"
            }
            ErrorKind::PollTimeout => "gcp: timed out waiting for credentials",
            ErrorKind::ClientUnavailable => "gcp: default credentials client unavailable",
            ErrorKind::NoActingUser => "gcp: no acting user",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A credential service answered with a non-2xx status. Branch on `status_code`; `body` is already
/// redacted and capped (see `redact.rs`) and is diagnostic text for a human, not something to match
/// on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// The HTTP status code of the response.
    pub status_code: u16,
    /// The response body, prepared for an error rather than verbatim.
    pub body: String,
}

impl fmt::Display for ApiError {
    // The body is quoted, not printed raw: it is service-controlled and can carry control bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gcp: credentials service returned status {}: {}",
            self.status_code,
            gadk_agent::go_quote(&self.body)
        )
    }
}

impl std::error::Error for ApiError {}

/// The gcp error: a chain of wrapped errors flattened to one message, the [`ErrorKind`] it matches
/// (if any) and the error it wraps (if any). `Display` is the whole message.
#[derive(Clone)]
pub struct Error {
    kind: Option<ErrorKind>,
    message: String,
    source: Option<Arc<dyn std::error::Error + Send + Sync + 'static>>,
}

impl Error {
    /// The [`ErrorKind`] this error matches directly.
    pub fn kind(&self) -> Option<ErrorKind> {
        self.kind
    }

    /// An error with only a message.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            kind: None,
            message: message.into(),
            source: None,
        }
    }

    /// `return ErrX`: message = `kind.as_str()`.
    pub(crate) fn from_kind(kind: ErrorKind) -> Self {
        Self {
            kind: Some(kind),
            message: kind.as_str().to_string(),
            source: None,
        }
    }

    /// An error of `kind` with a full message the caller formats.
    pub(crate) fn with_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind: Some(kind),
            message: message.into(),
            source: None,
        }
    }

    /// Attach the wrapped error (an [`ApiError`], a `ConsentRequiredError`, an inner
    /// `gcp::Error`, a `reqwest::Error` …). Message and kind are unchanged.
    pub(crate) fn caused_by<E: std::error::Error + Send + Sync + 'static>(
        mut self,
        source: E,
    ) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    /// Prefixes the message: "{prefix}: {message}"; kind and source are kept.
    pub(crate) fn prefixed(mut self, prefix: &str) -> Self {
        self.message = format!("{prefix}: {}", self.message);
        self
    }

    /// Appends the resource to the message: `<message> (resource "<resource>")`.
    pub(crate) fn with_resource(mut self, resource: &str) -> Self {
        self.message = format!(
            "{} (resource {})",
            self.message,
            gadk_agent::go_quote(resource)
        );
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            Some(e) => Some(&**e),
            None => None,
        }
    }
}

impl From<Error> for crate::Error {
    fn from(e: Error) -> Self {
        crate::Error::Other(crate::SharedError::new(e))
    }
}

/// True when any `gcp::Error` in `err`'s `source()` chain (the start included) has `kind`. Works
/// through `crate::Error` wrapping.
pub fn is_kind(err: &(dyn std::error::Error + 'static), kind: ErrorKind) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if e.downcast_ref::<Error>().and_then(Error::kind) == Some(kind) {
            return true;
        }
        cur = e.source();
    }
    false
}

/// The first [`ApiError`] in `err`'s `source()` chain, if any.
pub fn find_api_error<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a ApiError> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(api) = e.downcast_ref::<ApiError>() {
            return Some(api);
        }
        cur = e.source();
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::error::{ConsentRequiredError, find_consent};

    #[test]
    fn sentinel_texts_match_go() {
        let cases = [
            (ErrorKind::ConsentRejected, "gcp: user consent rejected"),
            (
                ErrorKind::MalformedResponse,
                "gcp: credentials service returned an undecodable response",
            ),
            (
                ErrorKind::PollTimeout,
                "gcp: timed out waiting for credentials",
            ),
            (
                ErrorKind::ClientUnavailable,
                "gcp: default credentials client unavailable",
            ),
            (ErrorKind::NoActingUser, "gcp: no acting user"),
        ];
        for (kind, text) in cases {
            assert_eq!(kind.as_str(), text);
            assert_eq!(kind.to_string(), text);
            let e = Error::from_kind(kind);
            assert_eq!(e.to_string(), text);
            assert_eq!(e.kind(), Some(kind));
        }
    }

    #[test]
    fn api_error_quotes_the_body() {
        let e = ApiError {
            status_code: 403,
            body: "denied\nforged line".into(),
        };
        assert_eq!(
            e.to_string(),
            r#"gcp: credentials service returned status 403: "denied\nforged line""#
        );
    }

    #[test]
    fn message_builders_match_go_wrapping() {
        let e = Error::with_kind(
            ErrorKind::NoActingUser,
            "gcp: no acting user: no ADK invocation identity on the context",
        );
        assert_eq!(
            e.to_string(),
            "gcp: no acting user: no ADK invocation identity on the context"
        );
        let e = Error::new(r#"resource "x y" has invalid characters"#).prefixed("gcp: NewProvider");
        assert_eq!(
            e.to_string(),
            r#"gcp: NewProvider: resource "x y" has invalid characters"#
        );
        assert_eq!(e.kind(), None);
        let e = Error::from_kind(ErrorKind::PollTimeout).with_resource("projects/p/x");
        assert_eq!(
            e.to_string(),
            r#"gcp: timed out waiting for credentials (resource "projects/p/x")"#
        );
        assert_eq!(e.kind(), Some(ErrorKind::PollTimeout));
        assert_eq!(
            format!("{e:?}"),
            r#"Error { kind: Some(PollTimeout), message: "gcp: timed out waiting for credentials (resource \"projects/p/x\")" }"#
        );
    }

    #[test]
    fn is_kind_and_find_api_error_walk_the_chain() {
        let api = ApiError {
            status_code: 503,
            body: "busy".into(),
        };
        let inner = Error::new(api.to_string()).caused_by(api.clone());
        let outer = Error::with_kind(
            ErrorKind::ClientUnavailable,
            format!("{}: {inner}", ErrorKind::ClientUnavailable),
        )
        .caused_by(inner);
        let core: crate::Error = outer.clone().into();
        let wrapped = crate::Error::ResolveCredential(Box::new(core));
        for e in [&outer as &(dyn std::error::Error + 'static), &wrapped] {
            assert!(is_kind(e, ErrorKind::ClientUnavailable));
            assert!(!is_kind(e, ErrorKind::NoActingUser));
            assert_eq!(find_api_error(e), Some(&api));
        }
        assert_eq!(
            wrapped.to_string(),
            "auth: resolve credential: gcp: default credentials client unavailable: gcp: credentials service returned status 503: \"busy\""
        );
        assert!(find_api_error(&Error::new("plain")).is_none());
    }

    #[test]
    fn consent_is_findable_through_a_gcp_error() {
        let c = ConsentRequiredError {
            auth_uri: "https://consent.example".into(),
            nonce: "n".into(),
            key: String::new(),
        };
        let e: crate::Error = Error::new(c.to_string()).caused_by(c.clone()).into();
        assert_eq!(find_consent(&e), Some(&c));
    }
}
