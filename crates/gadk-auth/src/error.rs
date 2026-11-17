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

//! The core error type [`Error`] and the consent error [`ConsentRequiredError`]. The `Display`
//! texts are the ones ADK auth implementations report for the same failures.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// An error from outside this crate, shared so `Error` stays `Clone` (a lazy init hands one
/// result to every waiter). `source()` returns the wrapped error itself, so a chain walk sees its
/// concrete type (unlike `Arc<E>`'s own `Error` impl, which skips a level).
#[derive(Clone)]
pub struct SharedError(Arc<dyn std::error::Error + Send + Sync + 'static>);

impl SharedError {
    /// Wraps `err`.
    pub fn new<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        Self(Arc::new(err))
    }

    /// The wrapped error.
    pub fn get_ref(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        &*self.0
    }
}

/// A message-only error, the payload of `SharedError::from(String)`.
struct Message(String);

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl std::error::Error for Message {}

impl From<String> for SharedError {
    fn from(msg: String) -> Self {
        Self::new(Message(msg))
    }
}

impl From<&str> for SharedError {
    fn from(msg: &str) -> Self {
        Self::new(Message(msg.to_string()))
    }
}

impl fmt::Display for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

impl fmt::Debug for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

/// The core error type. Each variant's `Display` text matches ADK's text for that failure.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// [`ApiKey`](crate::ApiKey) applied with an empty header name.
    #[error("auth: api key credential missing header name")]
    ApiKeyMissingName,
    /// [`Bearer`](crate::Bearer) applied with an empty token.
    #[error("auth: bearer credential missing token")]
    BearerMissingToken,
    /// [`Basic`](crate::Basic) applied with an empty username and password.
    #[error("auth: basic credential missing username and password")]
    BasicMissingCredentials,
    /// [`OAuth2`](crate::OAuth2) applied, and its token source failed; the source is that error.
    #[error("auth: mint oauth2 token: {0}")]
    MintToken(#[source] Box<Error>),
    /// Application Default Credentials could not be found; the source is the builder's error.
    #[error("auth: find default credentials: {0}")]
    FindDefaultCredentials(#[source] SharedError),
    /// A service-account ID-token source could not be built (an audience was set).
    #[error("auth: id token source: {0}")]
    IdTokenSource(#[source] SharedError),
    /// A service-account key was given without scopes.
    #[error("auth: scopes are required for a service-account access token")]
    ServiceAccountScopesRequired,
    /// A service-account key could not be parsed or turned into credentials.
    #[error("auth: credentials from json: {0}")]
    CredentialsFromJson(#[source] SharedError),
    /// A lazily initialised token source did not initialise within its bound. `init_error` is the
    /// init's own text, appended to the message but not wrapped as a source: "…exceeded 30s:
    /// context deadline exceeded: <init_error>".
    #[error(
        "auth: credential initialization exceeded {}: context deadline exceeded{}",
        gadk_platform::go_format_duration(gadk_platform::GoDuration::from_std(*.bound)),
        .init_error.as_deref().map(|e| format!(": {e}")).unwrap_or_default()
    )]
    InitTimeout {
        /// The per-attempt bound that fired.
        bound: Duration,
        /// The init's own error text, when it failed past the bound.
        init_error: Option<String>,
    },
    /// The credential provider failed; the source is the provider's error.
    #[error("auth: resolve credential: {0}")]
    ResolveCredential(#[source] Box<Error>),
    /// The credential could not be applied to the request; the source is the apply error.
    #[error("auth: apply credential: {0}")]
    ApplyCredential(#[source] Box<Error>),
    /// A header name or value `http::HeaderMap` refuses. The source never carries the value.
    #[error("auth: invalid header: {0}")]
    InvalidHeader(#[source] SharedError),
    /// A provider returned a [`ConsentRequiredError`].
    #[error("{0}")]
    Consent(#[source] ConsentRequiredError),
    /// Any other error a provider, token source or store returns, including a `gcp::Error` (see
    /// `From<gcp::Error>`).
    #[error("{0}")]
    Other(#[source] SharedError),
}

impl Error {
    /// `Error::Other(SharedError::new(err))`.
    pub fn other<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        Error::Other(SharedError::new(err))
    }

    /// `Error::Other(SharedError::from(msg.into()))`.
    pub fn msg(msg: impl Into<String>) -> Self {
        Error::Other(SharedError::from(msg.into()))
    }
}

impl From<ConsentRequiredError> for Error {
    fn from(err: ConsentRequiredError) -> Self {
        Error::Consent(err)
    }
}

/// Interactive, 3-legged OAuth consent is needed before a credential can be issued. Find it with
/// [`find_consent`] (or `find_consent_in` for a transport error), since a provider may wrap it.
///
/// `Display` is a fixed text and never carries the URI: this error becomes a tool's error,
/// which is fed to the model and persisted in the session. Hand `auth_uri` to the end user and to
/// nobody else; do not log it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsentRequiredError {
    /// Where the end user grants consent.
    pub auth_uri: String,
    /// The consent nonce the service issued.
    pub nonce: String,
    /// The credential key the consent completes.
    pub key: String,
}

impl fmt::Display for ConsentRequiredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("auth: interactive consent required")
    }
}

impl std::error::Error for ConsentRequiredError {}

/// Finds a [`ConsentRequiredError`] by walking `err` and its `source()` chain.
pub fn find_consent<'a>(
    err: &'a (dyn std::error::Error + 'static),
) -> Option<&'a ConsentRequiredError> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(c) = e.downcast_ref::<ConsentRequiredError>() {
            return Some(c);
        }
        cur = e.source();
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn consent() -> ConsentRequiredError {
        ConsentRequiredError {
            auth_uri: "https://consent.example/authorize?login_hint=alice".into(),
            nonce: "n-1".into(),
            key: "k-1".into(),
        }
    }

    #[test]
    fn consent_error_text_omits_the_uri() {
        let c = consent();
        assert_eq!(c.to_string(), "auth: interactive consent required");
        assert_eq!(
            Error::from(c).to_string(),
            "auth: interactive consent required"
        );
    }

    #[test]
    fn find_consent_walks_two_wrapping_layers() {
        // Two crate-error layers, as a transport wraps a provider's MintToken failure.
        let nested = Error::ResolveCredential(Box::new(Error::MintToken(Box::new(
            Error::Consent(consent()),
        ))));
        assert_eq!(
            nested.to_string(),
            "auth: resolve credential: auth: mint oauth2 token: auth: interactive consent required"
        );
        assert_eq!(find_consent(&nested), Some(&consent()));

        // Through a SharedError (a foreign error type wrapping a crate error).
        let shared = Error::other(Error::ApplyCredential(Box::new(Error::from(consent()))));
        assert_eq!(find_consent(&shared), Some(&consent()));

        // The consent error itself, and a chain without one.
        assert_eq!(find_consent(&consent()), Some(&consent()));
        let none = Error::MintToken(Box::new(Error::msg("token source failure")));
        assert_eq!(find_consent(&none), None);
    }

    #[test]
    fn shared_error_source_is_the_wrapped_error() {
        let shared = SharedError::new(Error::BearerMissingToken);
        let src = std::error::Error::source(&shared).expect("source");
        assert!(matches!(
            src.downcast_ref::<Error>(),
            Some(Error::BearerMissingToken)
        ));
        assert!(matches!(
            shared.get_ref().downcast_ref::<Error>(),
            Some(Error::BearerMissingToken)
        ));
        assert_eq!(shared.to_string(), "auth: bearer credential missing token");
        assert_eq!(format!("{shared:?}"), "BearerMissingToken");
        let msg = SharedError::from("plain");
        assert_eq!(msg.to_string(), "plain");
        assert_eq!(format!("{msg:?}"), "\"plain\"");
    }

    #[test]
    fn init_timeout_text_matches_go() {
        let bare = Error::InitTimeout {
            bound: Duration::from_secs(30),
            init_error: None,
        };
        assert_eq!(
            bare.to_string(),
            "auth: credential initialization exceeded 30s: context deadline exceeded"
        );
        let with_init = Error::InitTimeout {
            bound: Duration::from_millis(50),
            init_error: Some("auth: find default credentials: no file".into()),
        };
        assert_eq!(
            with_init.to_string(),
            "auth: credential initialization exceeded 50ms: context deadline exceeded: auth: find default credentials: no file"
        );
    }

    #[test]
    fn fixed_texts_match_go() {
        assert_eq!(
            Error::ApiKeyMissingName.to_string(),
            "auth: api key credential missing header name"
        );
        assert_eq!(
            Error::BearerMissingToken.to_string(),
            "auth: bearer credential missing token"
        );
        assert_eq!(
            Error::BasicMissingCredentials.to_string(),
            "auth: basic credential missing username and password"
        );
        assert_eq!(
            Error::ServiceAccountScopesRequired.to_string(),
            "auth: scopes are required for a service-account access token"
        );
        assert_eq!(
            Error::FindDefaultCredentials(SharedError::from("boom")).to_string(),
            "auth: find default credentials: boom"
        );
        assert_eq!(
            Error::IdTokenSource(SharedError::from("boom")).to_string(),
            "auth: id token source: boom"
        );
        assert_eq!(
            Error::CredentialsFromJson(SharedError::from("boom")).to_string(),
            "auth: credentials from json: boom"
        );
        assert_eq!(
            Error::ApplyCredential(Box::new(Error::BearerMissingToken)).to_string(),
            "auth: apply credential: auth: bearer credential missing token"
        );
        assert_eq!(
            Error::InvalidHeader(SharedError::from("invalid HTTP header name")).to_string(),
            "auth: invalid header: invalid HTTP header name"
        );
        assert_eq!(
            Error::msg("token source failure").to_string(),
            "token source failure"
        );
    }
}
