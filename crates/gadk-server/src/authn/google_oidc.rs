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

//! Port of Go `server/authn/googleoidc.go`: Google-signed OIDC bearer tokens, as a Pub/Sub push
//! subscription or an Eventarc trigger attaches them when configured with a service account.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt as _;
use futures::future::BoxFuture;
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use super::jwks::{self, Payload};
use super::{Authenticator, AuthnError, Caller};

/// Port of Go `tokenValidator` (the signature of `idtoken.Validate`): verifies `(id_token,
/// audience)`. `Ok(None)` is Go's `(nil, nil)`.
pub(crate) type TokenValidator = Arc<
    dyn Fn(String, String) -> BoxFuture<'static, Result<Option<Payload>, String>> + Send + Sync,
>;

/// Go `googleIssuers`: the two spellings Google uses in `iss` (idtoken leaves the check to the
/// caller).
const GOOGLE_ISSUERS: [&str; 2] = ["accounts.google.com", "https://accounts.google.com"];

/// Port of Go `authn.GoogleOIDCConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoogleOidcConfig {
    /// The OIDC audience a token must carry to be accepted. Required.
    ///
    /// It does not identify the caller: whoever mints the token chooses it.
    pub audience: String,
    /// The service account emails permitted to call. Required and non-empty: since the audience
    /// does not identify the caller, each entry is matched against a token's verified `email`
    /// claim.
    pub allowed_service_accounts: Vec<String>,
}

/// Where a Google OIDC authenticator writes Go's `log.Printf` records (`adk: authn: rejected …`,
/// `adk: authn: refused …`): the message without the log prefix. See [`google_oidc_with_log`].
pub type AuthnLog = Arc<dyn Fn(&str) + Send + Sync>;

/// Port of Go `googleOIDC`.
pub(crate) struct GoogleOidc {
    pub(crate) audience: String,
    pub(crate) allowed_service_accounts: Vec<String>,
    /// Go `validate`: [`jwks::validate`] in production; tests install fakes. `None` is Go's nil
    /// func, which [`GoogleOidc::validate_token`] turns into a rejection.
    pub(crate) validate: Option<TokenValidator>,
    /// Go's `log` for the rejection records; `None` writes them as `tracing` warnings.
    pub(crate) log: Option<AuthnLog>,
}

/// Port of Go `authn.NewGoogleOIDC`: an [`Authenticator`] requiring a Google-signed OIDC bearer
/// token issued for `cfg.audience` whose verified `email` is one of
/// `cfg.allowed_service_accounts`.
///
/// Both fields are required: an empty audience would make the verifier skip the audience check,
/// and an empty allow-list (or a blank or space-padded entry) is refused with Go's text as
/// [`AuthnError::Other`]. A missing, malformed or unverifiable token — including one whose
/// signing certificates cannot be fetched — is [`AuthnError::Unauthenticated`] (401); a verified
/// token naming an unlisted or unverified principal is [`AuthnError::Forbidden`] (403).
///
/// Authenticating must run inside a Tokio runtime: a certificate-cache miss spawns its fetch
/// (`tokio::spawn`) so it completes and is cached even if the request goes away.
pub fn google_oidc(cfg: GoogleOidcConfig) -> Result<Arc<dyn Authenticator>, AuthnError> {
    Ok(Arc::new(new_google_oidc(cfg)?))
}

/// [`google_oidc`] writing its rejection records (Go's `log.Printf` lines `adk: authn: rejected an
/// OIDC-authenticated request: …` and `adk: authn: refused an OIDC-authenticated principal: …`)
/// to `log` instead of `tracing`: the launcher passes its Go log, so they land where Go's do.
pub fn google_oidc_with_log(
    cfg: GoogleOidcConfig,
    log: AuthnLog,
) -> Result<Arc<dyn Authenticator>, AuthnError> {
    let mut g = new_google_oidc(cfg)?;
    g.log = Some(log);
    Ok(Arc::new(g))
}

/// [`google_oidc`] before type erasure (Go tests type-assert the result to `*googleOIDC`).
pub(crate) fn new_google_oidc(cfg: GoogleOidcConfig) -> Result<GoogleOidc, AuthnError> {
    if cfg.audience.is_empty() {
        return Err(AuthnError::Other(
            "authn: NewGoogleOIDC requires an audience".into(),
        ));
    }
    if cfg.allowed_service_accounts.is_empty() {
        return Err(AuthnError::Other(
            "authn: NewGoogleOIDC requires at least one allowed service account; the audience alone does not identify the caller"
                .into(),
        ));
    }
    for account in &cfg.allowed_service_accounts {
        if account.is_empty() || account != account.trim() {
            return Err(AuthnError::Other(format!(
                "authn: NewGoogleOIDC allowed service account {} is empty or has surrounding whitespace",
                go_quote(account)
            )));
        }
    }
    let validate: TokenValidator = Arc::new(|token: String, audience: String| {
        Box::pin(async move { jwks::validate(&token, &audience).await.map(Some) })
    });
    Ok(GoogleOidc {
        audience: cfg.audience,
        allowed_service_accounts: cfg.allowed_service_accounts,
        validate: Some(validate),
        log: None,
    })
}

#[async_trait::async_trait]
impl Authenticator for GoogleOidc {
    async fn authenticate(&self, req: &http::request::Parts) -> Result<Option<Caller>, AuthnError> {
        let auth_headers: Vec<String> = req
            .headers
            .get_all(http::header::AUTHORIZATION)
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect();
        let token = bearer_token(&auth_headers).map_err(|e| self.deny(e))?;

        let payload = self
            .validate_token(&token)
            .await
            .map_err(|e| self.deny(format!("invalid identity token: {e}")))?;
        let Some(payload) = payload else {
            return Err(self.deny("identity token verified with no payload".into()));
        };
        if !GOOGLE_ISSUERS.contains(&payload.issuer.as_str()) {
            return Err(self.deny(format!("untrusted issuer {}", go_quote(&payload.issuer))));
        }

        let email = payload
            .claims
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // A non-boolean email_verified fails closed (Go's type assertion).
        let email_verified = payload
            .claims
            .get("email_verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !email_verified || !self.allowed_service_accounts.contains(&email) {
            return Err(self.forbid(format!(
                "principal {} (verified={email_verified}) is not an allowed service account",
                go_quote(&email)
            )));
        }

        // sub distinguishes a recreated service account the email cannot; the email is the
        // fallback only because nothing requires a sub.
        let user_id = if payload.subject.is_empty() {
            email.clone()
        } else {
            payload.subject.clone()
        };
        Ok(Some(Caller {
            user_id,
            claims: Some(Map::from_iter([
                ("aud".to_owned(), Value::String(payload.audience)),
                ("email".to_owned(), Value::String(email)),
                ("email_verified".to_owned(), Value::Bool(email_verified)),
            ])),
        }))
    }
}

impl GoogleOidc {
    /// Go `validateToken`: runs the validator and turns a panic (or a missing validator, Go's nil
    /// func call) into an error, so no request can crash the handler.
    async fn validate_token(&self, token: &str) -> Result<Option<Payload>, String> {
        let Some(validate) = &self.validate else {
            return Err(
                "identity token validation panicked: runtime error: invalid memory address or nil pointer dereference"
                    .into(),
            );
        };
        let fut = std::panic::catch_unwind(AssertUnwindSafe(|| {
            validate(token.to_owned(), self.audience.clone())
        }))
        .map_err(panicked)?;
        AssertUnwindSafe(fut)
            .catch_unwind()
            .await
            .unwrap_or_else(|p| Err(panicked(p)))
    }
}

/// Go `fmt.Errorf("identity token validation panicked: %v", r)`.
fn panicked(p: Box<dyn Any + Send>) -> String {
    let msg = p
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    format!("identity token validation panicked: {msg}")
}

impl GoogleOidc {
    /// Go `log.Printf(msg)` through [`GoogleOidc::log`], or a `tracing` warning without one.
    fn log_printf(&self, msg: &str) {
        match &self.log {
            Some(log) => log(msg),
            None => tracing::warn!("{msg}"),
        }
    }

    /// Go `deny`: logs the reason and returns it as a credential problem (401). The reason stays
    /// server-side.
    fn deny(&self, reason: String) -> AuthnError {
        self.log_printf(&format!(
            "adk: authn: rejected an OIDC-authenticated request: {reason}"
        ));
        AuthnError::Unauthenticated(Some(reason))
    }

    /// Go `forbid`: logs the reason and returns it as an authorization problem (403).
    fn forbid(&self, reason: String) -> AuthnError {
        self.log_printf(&format!(
            "adk: authn: refused an OIDC-authenticated principal: {reason}"
        ));
        AuthnError::Forbidden(Some(reason))
    }
}

/// Go `bearerToken`: the credential of the single `Authorization` header, whose scheme is
/// `Bearer` in any case (RFC 9110). More than one header is rejected rather than resolved.
pub(crate) fn bearer_token(auth_headers: &[String]) -> Result<String, String> {
    let [header] = auth_headers else {
        if auth_headers.is_empty() {
            return Err("no Authorization header".into());
        }
        return Err(format!(
            "request carries {} Authorization headers",
            auth_headers.len()
        ));
    };
    let Some((scheme, token)) = header.split_once(' ') else {
        return Err("authorization header is not a Bearer credential".into());
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err("authorization header is not a Bearer credential".into());
    }
    // Go `strings.TrimSpace`: `unicode.IsSpace` is Unicode `White_Space`, as `str::trim`.
    let token = token.trim();
    if token.is_empty() {
        return Err("bearer credential is empty".into());
    }
    Ok(token.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// [`google_oidc_with_log`] writes Go's `log.Printf` records to the given log, not `tracing`.
    #[tokio::test]
    async fn with_log_writes_the_rejection_records_to_the_log() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let log: AuthnLog = {
            let lines = Arc::clone(&lines);
            Arc::new(move |msg: &str| lines.lock().unwrap().push(msg.to_owned()))
        };
        let a = google_oidc_with_log(
            GoogleOidcConfig {
                audience: "https://example.com/push".into(),
                allowed_service_accounts: vec!["push@example.iam.gserviceaccount.com".into()],
            },
            log,
        )
        .unwrap();
        let (parts, ()) = http::Request::post("/").body(()).unwrap().into_parts();
        assert!(matches!(
            a.authenticate(&parts).await,
            Err(AuthnError::Unauthenticated(_))
        ));
        let (parts, ()) = http::Request::post("/")
            .header("Authorization", "Bearer not-a-jwt")
            .body(())
            .unwrap()
            .into_parts();
        assert!(a.authenticate(&parts).await.is_err());
        assert_eq!(
            *lines.lock().unwrap(),
            vec![
                "adk: authn: rejected an OIDC-authenticated request: no Authorization header"
                    .to_owned(),
                "adk: authn: rejected an OIDC-authenticated request: invalid identity token: idtoken: invalid token, token must have three segments; found 1"
                    .to_owned(),
            ]
        );
    }
}
