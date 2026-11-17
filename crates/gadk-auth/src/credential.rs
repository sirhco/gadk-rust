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

//! Resolved credentials that write themselves onto an outbound request, and the [`TokenSource`]
//! they mint tokens from.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};

use crate::error::{Error, SharedError};

/// A resolved credential that applies itself to request headers. Shared as
/// `Arc<dyn Credential>`; identity is `Arc::ptr_eq`. `Any` lets the gcp provider read a token out
/// of a credential.
#[async_trait::async_trait]
pub trait Credential: Any + Send + Sync + fmt::Debug {
    /// Writes the credential's auth headers onto `headers`.
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error>;

    /// The credential a [`with_headers`] wrapper wraps; `None` for every other credential.
    fn inner(&self) -> Option<&dyn Credential> {
        None
    }
}

impl dyn Credential {
    /// This credential as a `T`, when it is one.
    pub fn downcast_ref<T: Credential>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }
}

/// `"<redacted>"` for a non-empty secret, the (empty) value otherwise.
fn redacted(secret: &str) -> &str {
    if secret.is_empty() { "" } else { "<redacted>" }
}

/// `headers.insert(name, value)` (replacing any existing value), refusing what `http` refuses.
fn set_header(
    headers: &mut HeaderMap,
    name: &str,
    value: &str,
    sensitive: bool,
) -> Result<(), Error> {
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|e| Error::InvalidHeader(SharedError::new(e)))?;
    let mut value =
        HeaderValue::from_str(value).map_err(|e| Error::InvalidHeader(SharedError::new(e)))?;
    value.set_sensitive(sensitive);
    headers.insert(name, value);
    Ok(())
}

/// An API key credential: sets header `name` to `value`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ApiKey {
    /// The header name, e.g. `X-Api-Key`.
    pub name: String,
    /// The key.
    pub value: String,
}

/// A bearer credential: `Authorization: Bearer <token>`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Bearer {
    /// The bearer token.
    pub token: String,
}

/// A basic-auth credential: `Authorization: Basic base64(user:pass)`
/// (`base64::engine::general_purpose::STANDARD`). Fails only when both are empty.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Basic {
    /// The user name (may be empty when the password is not).
    pub username: String,
    /// The password (may be empty when the user name is not).
    pub password: String,
}

/// An OAuth2 credential: `Authorization: <token.token_type()> <access_token>`, minting a fresh
/// token from `token_source` on each `apply`.
#[derive(Clone)]
pub struct OAuth2 {
    /// Where each `apply` gets its token.
    pub token_source: Arc<dyn TokenSource>,
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKey")
            .field("name", &self.name)
            .field("value", &redacted(&self.value))
            .finish()
    }
}

impl fmt::Debug for Bearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bearer")
            .field("token", &redacted(&self.token))
            .finish()
    }
}

impl fmt::Debug for Basic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Basic")
            .field("username", &self.username)
            .field("password", &redacted(&self.password))
            .finish()
    }
}

impl fmt::Debug for OAuth2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuth2")
            .field("token_source", &"<redacted>")
            .finish()
    }
}

#[async_trait::async_trait]
impl Credential for ApiKey {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error> {
        if self.name.is_empty() {
            return Err(Error::ApiKeyMissingName);
        }
        set_header(headers, &self.name, &self.value, true)
    }
}

#[async_trait::async_trait]
impl Credential for Bearer {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error> {
        if self.token.is_empty() {
            return Err(Error::BearerMissingToken);
        }
        set_header(
            headers,
            AUTHORIZATION.as_str(),
            &format!("Bearer {}", self.token),
            true,
        )
    }
}

#[async_trait::async_trait]
impl Credential for Basic {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error> {
        // An empty username or password alone is allowed; reject only when both are empty.
        if self.username.is_empty() && self.password.is_empty() {
            return Err(Error::BasicMissingCredentials);
        }
        let raw = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.username, self.password));
        set_header(
            headers,
            AUTHORIZATION.as_str(),
            &format!("Basic {raw}"),
            true,
        )
    }
}

#[async_trait::async_trait]
impl Credential for OAuth2 {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error> {
        let tok = self
            .token_source
            .token()
            .await
            .map_err(|e| Error::MintToken(Box::new(e)))?;
        set_header(
            headers,
            AUTHORIZATION.as_str(),
            &format!("{} {}", tok.token_type(), tok.access_token),
            true,
        )
    }
}

/// The credential [`with_headers`] returns.
struct WithHeaders {
    inner: Arc<dyn Credential>,
    headers: BTreeMap<String, String>,
}

impl fmt::Debug for WithHeaders {
    // Header values are not printed: a caller may put a secret in one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WithHeaders")
            .field("inner", &self.inner)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[async_trait::async_trait]
impl Credential for WithHeaders {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error> {
        self.inner.apply(headers).await?;
        for (k, v) in &self.headers {
            // Every header a credential adds is credential material (gcp mirrors the token).
            set_header(headers, k, v, true)?;
        }
        Ok(())
    }

    fn inner(&self) -> Option<&dyn Credential> {
        Some(&*self.inner)
    }
}

/// Wraps `inner` in a credential that applies `inner`, then sets each header, overriding inner on
/// conflict. The result's `inner()` is `Some(&*inner)` (pointer-equal to `inner`).
pub fn with_headers(
    inner: Arc<dyn Credential>,
    headers: BTreeMap<String, String>,
) -> Arc<dyn Credential> {
    Arc::new(WithHeaders { inner, headers })
}

/// A source of OAuth2 tokens.
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    /// A valid token; implementations cache and refresh as they see fit.
    async fn token(&self) -> Result<Token, Error>;
}

/// An OAuth2 token: the two fields a credential reads.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Token {
    /// The access token.
    pub access_token: String,
    /// The token type as the issuer wrote it ("Bearer", "bearer", "MAC", …).
    pub token_type: String,
}

impl Token {
    /// The type to send in `Authorization`: "bearer"/"mac"/"basic" (any case) →
    /// "Bearer"/"MAC"/"Basic"; any other non-empty value as is; empty → "Bearer".
    pub fn token_type(&self) -> &str {
        let t = self.token_type.as_str();
        if go_equal_fold_ascii(t, "bearer") {
            "Bearer"
        } else if go_equal_fold_ascii(t, "mac") {
            "MAC"
        } else if go_equal_fold_ascii(t, "basic") {
            "Basic"
        } else if !t.is_empty() {
            t
        } else {
            "Bearer"
        }
    }
}

/// Case-insensitive equality of `s` with an all-lowercase ASCII `target`: Unicode simple folding
/// adds exactly two non-ASCII matches for ASCII letters, U+212A KELVIN SIGN (k) and U+017F LATIN
/// SMALL LETTER LONG S (s).
fn go_equal_fold_ascii(s: &str, target: &str) -> bool {
    let mut it = s.chars();
    for want in target.chars() {
        let Some(c) = it.next() else { return false };
        let ok = c.to_ascii_lowercase() == want
            || (c == '\u{212A}' && want == 'k')
            || (c == '\u{017F}' && want == 's');
        if !ok {
            return false;
        }
    }
    it.next().is_none()
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("access_token", &redacted(&self.access_token))
            .field("token_type", &self.token_type)
            .finish()
    }
}

/// A token source that always returns the same token.
#[derive(Clone, Debug)]
pub struct StaticTokenSource(pub Token);

#[async_trait::async_trait]
impl TokenSource for StaticTokenSource {
    async fn token(&self) -> Result<Token, Error> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn token_type_matches_oauth2() {
        let ty = |t: &str| {
            Token {
                access_token: String::new(),
                token_type: t.into(),
            }
            .token_type()
            .to_string()
        };
        assert_eq!(ty(""), "Bearer");
        assert_eq!(ty("bearer"), "Bearer");
        assert_eq!(ty("BEARER"), "Bearer");
        assert_eq!(ty("mac"), "MAC");
        assert_eq!(ty("Basic"), "Basic");
        assert_eq!(ty("ba\u{017F}ic"), "Basic");
        assert_eq!(ty("DPoP"), "DPoP");
        assert_eq!(ty("bearers"), "bearers");
    }

    #[test]
    fn debug_withholds_secrets() {
        let k = ApiKey {
            name: "X-Api-Key".into(),
            value: "s3cr3t".into(),
        };
        assert_eq!(
            format!("{k:?}"),
            r#"ApiKey { name: "X-Api-Key", value: "<redacted>" }"#
        );
        assert_eq!(
            format!(
                "{:?}",
                Bearer {
                    token: "s3cr3t".into()
                }
            ),
            r#"Bearer { token: "<redacted>" }"#
        );
        assert_eq!(
            format!("{:?}", Bearer::default()),
            r#"Bearer { token: "" }"#
        );
        assert_eq!(
            format!(
                "{:?}",
                Basic {
                    username: "u".into(),
                    password: "s3cr3t".into()
                }
            ),
            r#"Basic { username: "u", password: "<redacted>" }"#
        );
        let tok = Token {
            access_token: "s3cr3t".into(),
            token_type: "Bearer".into(),
        };
        let o = OAuth2 {
            token_source: Arc::new(StaticTokenSource(tok.clone())),
        };
        assert_eq!(format!("{o:?}"), r#"OAuth2 { token_source: "<redacted>" }"#);
        assert!(!format!("{tok:?}").contains("s3cr3t"));
        let w = with_headers(
            Arc::new(k),
            BTreeMap::from([("X-S".into(), "s3cr3t".into())]),
        );
        assert!(!format!("{w:?}").contains("s3cr3t"));
    }

    #[tokio::test]
    async fn secret_header_values_are_sensitive() {
        let mut h = HeaderMap::new();
        Bearer { token: "t".into() }.apply(&mut h).await.unwrap();
        assert!(h[AUTHORIZATION].is_sensitive());
        let mut h = HeaderMap::new();
        with_headers(
            Arc::new(Bearer { token: "t".into() }),
            BTreeMap::from([("X-Goog-Api-Key".into(), "t".into())]),
        )
        .apply(&mut h)
        .await
        .unwrap();
        assert!(h[AUTHORIZATION].is_sensitive());
        assert!(h["x-goog-api-key"].is_sensitive());
    }

    #[tokio::test]
    async fn invalid_header_is_an_error_without_the_value() {
        let mut h = HeaderMap::new();
        let err = ApiKey {
            name: "bad name".into(),
            value: "s3cr3t".into(),
        }
        .apply(&mut h)
        .await
        .unwrap_err();
        assert!(matches!(err, Error::InvalidHeader(_)), "{err:?}");
        assert!(!err.to_string().contains("s3cr3t"));
        let err = ApiKey {
            name: "X-Api-Key".into(),
            value: "line\nbreak".into(),
        }
        .apply(&mut h)
        .await
        .unwrap_err();
        assert!(matches!(err, Error::InvalidHeader(_)), "{err:?}");
        assert!(!err.to_string().contains("break"));
    }

    #[test]
    fn downcast_ref_reads_the_concrete_credential() {
        let c: Arc<dyn Credential> = Arc::new(Bearer {
            token: "abc".into(),
        });
        assert_eq!(
            c.downcast_ref::<Bearer>(),
            Some(&Bearer {
                token: "abc".into()
            })
        );
        assert!(c.downcast_ref::<ApiKey>().is_none());
        assert!(c.inner().is_none());
    }
}
