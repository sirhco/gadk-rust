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

//! Ports of ADK-Go `server/authn/googleoidc_test.go`. The Go tests build `googleOIDC` literals
//! with a fake `validate`; these build [`GoogleOidc`] with a fake [`TokenValidator`]. Go's
//! `quietLogs` has no counterpart: the rejection logs are `tracing` events, silent without a
//! subscriber. Each Go subtest is a named case; a failure names it.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use http::StatusCode;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use tower::{Layer as _, ServiceExt as _};

use crate::authn::google_oidc::{
    GoogleOidc, GoogleOidcConfig, TokenValidator, bearer_token, new_google_oidc,
};
use crate::authn::jwks::Payload;
use crate::authn::{Authenticator, AuthnError, Caller, middleware};

const TEST_AUDIENCE: &str = "https://example-agent.example.com";
const TEST_SERVICE_ACCOUNT: &str = "pubsub-push@example-project.iam.gserviceaccount.com";
const TEST_SUBJECT: &str = "108100000000000000000";

/// Go `googlePayload`: what a Google-minted push-subscription token carries.
fn google_payload(aud: &str) -> Payload {
    Payload {
        audience: aud.to_owned(),
        issuer: "https://accounts.google.com".into(),
        subject: TEST_SUBJECT.into(),
        claims: claims(json!({"email": TEST_SERVICE_ACCOUNT, "email_verified": true})),
        ..Default::default()
    }
}

fn claims(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// A [`TokenValidator`] answering with `f(token, audience)`.
fn validator<F>(f: F) -> TokenValidator
where
    F: Fn(&str, &str) -> Result<Option<Payload>, String> + Send + Sync + 'static,
{
    Arc::new(move |token: String, aud: String| {
        let r = f(&token, &aud);
        Box::pin(async move { r })
    })
}

/// A validator that answers with `p(aud)` whatever the token.
fn payload_validator(p: fn(&str) -> Payload) -> TokenValidator {
    validator(move |_, aud| Ok(Some(p(aud))))
}

fn oidc(allowed: &[&str], validate: Option<TokenValidator>) -> GoogleOidc {
    GoogleOidc {
        audience: TEST_AUDIENCE.into(),
        allowed_service_accounts: allowed.iter().map(|s| (*s).to_owned()).collect(),
        validate,
        log: None,
    }
}

/// `httptest.NewRequest(POST, path)` with the given `Authorization` header lines.
fn request(path: &str, auth: &[&str]) -> http::Request<Body> {
    let mut b = http::Request::post(path);
    for a in auth {
        b = b.header(http::header::AUTHORIZATION, *a);
    }
    b.body(Body::empty()).unwrap()
}

async fn authenticate(
    a: &GoogleOidc,
    req: http::Request<Body>,
) -> Result<Option<Caller>, AuthnError> {
    let (parts, _) = req.into_parts();
    a.authenticate(&parts).await
}

/// The outcome of a request through [`middleware`].
struct Served {
    status: StatusCode,
    body: String,
    /// The caller the next handler saw; `None` when it did not run.
    next_saw: Option<Option<Caller>>,
}

/// Go `Middleware(g)(next).ServeHTTP(rec, req)`, `next` recording the request's caller.
async fn serve(a: Arc<dyn Authenticator>, req: http::Request<Body>) -> Served {
    let seen: Arc<Mutex<Option<Option<Caller>>>> = Arc::new(Mutex::new(None));
    let next = {
        let seen = seen.clone();
        tower::service_fn(move |req: http::Request<Body>| {
            let seen = seen.clone();
            async move {
                *seen.lock() = Some(req.extensions().get::<Caller>().cloned());
                Ok::<_, Infallible>(axum::response::Response::new(Body::empty()))
            }
        })
    };
    let resp = middleware(a).layer(next).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let next_saw = seen.lock().take();
    Served {
        status,
        body: String::from_utf8(body.to_vec()).unwrap(),
        next_saw,
    }
}

fn is_unauthenticated(r: &Result<Option<Caller>, AuthnError>) -> bool {
    matches!(r, Err(AuthnError::Unauthenticated(_)))
}

fn is_forbidden(r: &Result<Option<Caller>, AuthnError>) -> bool {
    matches!(r, Err(AuthnError::Forbidden(_)))
}

#[tokio::test]
async fn test_google_oidc_authenticate() {
    struct Case {
        name: &'static str,
        allowed_accounts: &'static [&'static str],
        auth_header: &'static str,
        extra_auth_header: &'static str,
        /// `None`: the provider must not reach the validator.
        validate: Option<TokenValidator>,
        want_err: bool,
        want_forbidden: bool,
        want_user_id: &'static str,
    }
    let ok = || Case {
        name: "",
        allowed_accounts: &[TEST_SERVICE_ACCOUNT],
        auth_header: "",
        extra_auth_header: "",
        validate: None,
        want_err: false,
        want_forbidden: false,
        want_user_id: "",
    };
    let cases = vec![
        Case {
            name: "no Authorization header",
            allowed_accounts: &[],
            want_err: true,
            ..ok()
        },
        Case {
            name: "non-Bearer scheme",
            allowed_accounts: &[],
            auth_header: "Basic dXNlcjpwYXNz",
            want_err: true,
            ..ok()
        },
        Case {
            name: "Bearer with empty credential",
            allowed_accounts: &[],
            auth_header: "Bearer   ",
            want_err: true,
            ..ok()
        },
        Case {
            name: "lowercase bearer scheme is accepted",
            auth_header: "bearer valid-token",
            validate: Some(validator(|token, aud| {
                assert_eq!(token, "valid-token", "validate called with the wrong token");
                Ok(Some(google_payload(aud)))
            })),
            want_user_id: TEST_SUBJECT,
            ..ok()
        },
        Case {
            name: "valid bearer token",
            auth_header: "Bearer valid-token",
            validate: Some(validator(|token, aud| {
                assert_eq!((token, aud), ("valid-token", TEST_AUDIENCE));
                Ok(Some(google_payload(aud)))
            })),
            want_user_id: TEST_SUBJECT,
            ..ok()
        },
        Case {
            name: "token fails verification",
            allowed_accounts: &[],
            auth_header: "Bearer forged-token",
            validate: Some(validator(|_, _| Err("idtoken: invalid token".into()))),
            want_err: true,
            ..ok()
        },
        Case {
            name: "verification returns no payload",
            allowed_accounts: &[],
            auth_header: "Bearer empty-payload",
            validate: Some(validator(|_, _| Ok(None))),
            want_err: true,
            ..ok()
        },
        Case {
            name: "token from a non-Google issuer is rejected",
            allowed_accounts: &[],
            auth_header: "Bearer other-issuer",
            validate: Some(payload_validator(|aud| Payload {
                issuer: "https://accounts.example.com".into(),
                ..google_payload(aud)
            })),
            want_err: true,
            ..ok()
        },
        Case {
            name: "bare issuer spelling is accepted",
            auth_header: "Bearer bare-issuer",
            validate: Some(payload_validator(|aud| Payload {
                issuer: "accounts.google.com".into(),
                ..google_payload(aud)
            })),
            want_user_id: TEST_SUBJECT,
            ..ok()
        },
        Case {
            name: "allow-list configured, matching principal",
            allowed_accounts: &[
                "other@example.iam.gserviceaccount.com",
                TEST_SERVICE_ACCOUNT,
            ],
            auth_header: "Bearer valid-token",
            validate: Some(payload_validator(google_payload)),
            want_user_id: TEST_SUBJECT,
            ..ok()
        },
        Case {
            name: "allow-list configured, unlisted principal",
            auth_header: "Bearer valid-token",
            validate: Some(payload_validator(|aud| {
                let mut p = google_payload(aud);
                p.claims
                    .insert("email".into(), json!("someone-else@gmail.com"));
                p
            })),
            want_err: true,
            want_forbidden: true,
            ..ok()
        },
        Case {
            name: "allow-list configured, token has no email claim",
            auth_header: "Bearer no-email",
            validate: Some(payload_validator(|aud| Payload {
                audience: aud.to_owned(),
                issuer: "https://accounts.google.com".into(),
                subject: TEST_SUBJECT.into(),
                ..Default::default()
            })),
            want_err: true,
            want_forbidden: true,
            ..ok()
        },
        Case {
            name: "allow-list configured, verified but empty email",
            auth_header: "Bearer empty-email",
            validate: Some(payload_validator(|aud| Payload {
                audience: aud.to_owned(),
                issuer: "https://accounts.google.com".into(),
                subject: TEST_SUBJECT.into(),
                claims: claims(json!({"email_verified": true})),
                ..Default::default()
            })),
            want_err: true,
            want_forbidden: true,
            ..ok()
        },
        Case {
            name: "allow-list configured, email not verified",
            auth_header: "Bearer unverified-email",
            validate: Some(payload_validator(|aud| {
                let mut p = google_payload(aud);
                p.claims.insert("email_verified".into(), json!(false));
                p
            })),
            want_err: true,
            want_forbidden: true,
            ..ok()
        },
        Case {
            name: "allow-list configured, email_verified is a non-boolean",
            auth_header: "Bearer stringy-verified",
            validate: Some(payload_validator(|aud| {
                let mut p = google_payload(aud);
                p.claims.insert("email_verified".into(), json!("true"));
                p
            })),
            want_err: true,
            want_forbidden: true,
            ..ok()
        },
        Case {
            name: "duplicate Authorization headers are rejected",
            allowed_accounts: &[],
            auth_header: "Bearer valid-token",
            extra_auth_header: "Bearer second-token",
            want_err: true,
            ..ok()
        },
        Case {
            name: "no subject falls back to the email",
            auth_header: "Bearer no-subject",
            validate: Some(payload_validator(|aud| Payload {
                subject: String::new(),
                ..google_payload(aud)
            })),
            want_user_id: TEST_SERVICE_ACCOUNT,
            ..ok()
        },
    ];

    for tt in cases {
        let called = Arc::new(AtomicBool::new(false));
        let validate = tt.validate.unwrap_or_else(|| {
            let called = called.clone();
            validator(move |_, _| {
                called.store(true, Ordering::SeqCst);
                Err("unexpected call".into())
            })
        });
        let g = oidc(tt.allowed_accounts, Some(validate));
        let auth: Vec<&str> = [tt.auth_header, tt.extra_auth_header]
            .into_iter()
            .filter(|h| !h.is_empty())
            .collect();

        let got = authenticate(&g, request("/apps/test-agent/trigger/pubsub", &auth)).await;
        assert!(
            !called.load(Ordering::SeqCst),
            "{}: validate should not have been called",
            tt.name
        );
        if tt.want_err {
            if tt.want_forbidden {
                assert!(
                    is_forbidden(&got),
                    "{}: Authenticate() = {got:?}, want Forbidden",
                    tt.name
                );
            } else {
                assert!(
                    is_unauthenticated(&got),
                    "{}: Authenticate() = {got:?}, want Unauthenticated",
                    tt.name
                );
            }
            continue;
        }
        let caller = got.unwrap_or_else(|e| panic!("{}: Authenticate() error = {e}", tt.name));
        assert_eq!(
            caller.map(|c| c.user_id).as_deref(),
            Some(tt.want_user_id),
            "{}: Caller.UserID",
            tt.name
        );
    }
}

#[tokio::test]
async fn test_google_oidc_rejection_is_not_an_internal_failure() {
    let g = GoogleOidc {
        audience: TEST_AUDIENCE.into(),
        allowed_service_accounts: Vec::new(),
        validate: Some(validator(|_, _| Err("idtoken: invalid token".into()))),
        log: None,
    };
    let got = serve(Arc::new(g), request("/", &["Bearer forged-token"])).await;
    assert_eq!(got.status, StatusCode::UNAUTHORIZED);
    assert!(
        got.next_saw.is_none(),
        "the next handler ran on a rejected request"
    );
}

#[tokio::test]
async fn test_google_oidc_rejection_body_does_not_vary_with_reason() {
    let unauth_reasons: Vec<(&str, TokenValidator)> = vec![
        (
            "bad signature",
            validator(|_, _| Err("idtoken: invalid token".into())),
        ),
        (
            "untrusted issuer",
            payload_validator(|aud| Payload {
                issuer: "https://accounts.example.com".into(),
                ..google_payload(aud)
            }),
        ),
    ];

    let mut first: Option<String> = None;
    for (name, validate) in unauth_reasons {
        let g = oidc(&[TEST_SERVICE_ACCOUNT], Some(validate));
        let got = serve(Arc::new(g), request("/", &["Bearer some-token"])).await;
        assert_eq!(got.status, StatusCode::UNAUTHORIZED, "{name}: status");
        match &first {
            None => first = Some(got.body),
            Some(f) => assert_eq!(&got.body, f, "{name}: body differs from another 401"),
        }
    }
    let first = first.expect("no unauthenticated rejection was exercised");
    assert_eq!(first, "unauthorized\n");
    assert!(!first.to_lowercase().contains("service account"));

    let g = oidc(
        &[TEST_SERVICE_ACCOUNT],
        Some(payload_validator(|aud| {
            let mut p = google_payload(aud);
            p.claims
                .insert("email".into(), json!("someone-else@gmail.com"));
            p
        })),
    );
    let got = serve(Arc::new(g), request("/", &["Bearer some-token"])).await;
    assert_eq!(
        got.status,
        StatusCode::FORBIDDEN,
        "unlisted principal: status"
    );
    let forbidden_body = got.body.to_lowercase();
    assert_eq!(forbidden_body, "forbidden\n");
    assert!(!forbidden_body.contains("service account"));
    assert!(!forbidden_body.contains("someone-else@gmail.com"));
}

#[tokio::test]
async fn test_google_oidc_puts_caller_on_context() {
    let g = oidc(
        &[TEST_SERVICE_ACCOUNT],
        Some(payload_validator(google_payload)),
    );
    let got = serve(Arc::new(g), request("/", &["Bearer valid-token"])).await;
    assert_eq!(got.status, StatusCode::OK);
    let caller = got
        .next_saw
        .expect("no caller reached the next handler")
        .expect("the request carries no Caller");
    assert_eq!(caller.user_id, TEST_SUBJECT);
    let claims = caller.claims.unwrap_or_default();
    assert_eq!(claims.get("email"), Some(&json!(TEST_SERVICE_ACCOUNT)));
    assert_eq!(claims.get("email_verified"), Some(&json!(true)));
    assert_eq!(claims.get("aud"), Some(&json!(TEST_AUDIENCE)));
}

#[test]
fn test_new_google_oidc_requires_audience_and_allow_list() {
    let cfg = |audience: &str, accounts: &[&str]| GoogleOidcConfig {
        audience: audience.to_owned(),
        allowed_service_accounts: accounts.iter().map(|s| (*s).to_owned()).collect(),
    };
    let err = |c: GoogleOidcConfig| match new_google_oidc(c) {
        Err(AuthnError::Other(msg)) => msg,
        Err(e) => panic!("NewGoogleOIDC error = {e:?}, want Other"),
        Ok(_) => panic!("NewGoogleOIDC error = nil, want an error"),
    };
    assert_eq!(
        err(cfg("", &[TEST_SERVICE_ACCOUNT])),
        "authn: google_oidc requires an audience",
        "no audience"
    );
    // A missing and an empty allow-list are the same `Vec` in Rust.
    assert_eq!(
        err(cfg(TEST_AUDIENCE, &[])),
        "authn: google_oidc requires at least one allowed service account; the audience alone does not identify the caller",
        "empty allow-list"
    );
    assert_eq!(
        err(cfg(TEST_AUDIENCE, &[""])),
        r#"authn: google_oidc allowed service account "" is empty or has surrounding whitespace"#,
        "blank entry"
    );
    assert_eq!(
        err(cfg(TEST_AUDIENCE, &[TEST_SERVICE_ACCOUNT, ""])),
        r#"authn: google_oidc allowed service account "" is empty or has surrounding whitespace"#,
        "blank entry after a real one"
    );
    let padded = format!(" {TEST_SERVICE_ACCOUNT} ");
    assert_eq!(
        err(cfg(TEST_AUDIENCE, &[&padded])),
        format!(
            r#"authn: google_oidc allowed service account " {TEST_SERVICE_ACCOUNT} " is empty or has surrounding whitespace"#
        ),
        "padded entry"
    );
    assert!(new_google_oidc(cfg(TEST_AUDIENCE, &[TEST_SERVICE_ACCOUNT])).is_ok());
    // The public constructor reports the same errors.
    assert!(matches!(
        crate::authn::google_oidc(cfg("", &[TEST_SERVICE_ACCOUNT])),
        Err(AuthnError::Other(_))
    ));
}

#[test]
fn test_new_google_oidc_installs_validator() {
    let g = new_google_oidc(GoogleOidcConfig {
        audience: TEST_AUDIENCE.into(),
        allowed_service_accounts: vec![TEST_SERVICE_ACCOUNT.into()],
    })
    .unwrap();
    assert!(
        g.validate.is_some(),
        "NewGoogleOIDC() left validate unset; production would reject every request"
    );
}

#[tokio::test]
async fn test_new_google_oidc_enforces_audience() {
    // A token minted for `minted_for` through an authenticator built by the constructor; the
    // stand-in reproduces idtoken's audience rule (an empty audience accepts anything).
    async fn authenticate_minted_for(
        minted_for: &'static str,
    ) -> Result<Option<Caller>, AuthnError> {
        let mut g = new_google_oidc(GoogleOidcConfig {
            audience: TEST_AUDIENCE.into(),
            allowed_service_accounts: vec![TEST_SERVICE_ACCOUNT.into()],
        })
        .unwrap();
        g.validate = Some(validator(move |_, audience| {
            assert_eq!(
                audience, TEST_AUDIENCE,
                "validate called with the wrong audience"
            );
            if !audience.is_empty() && audience != minted_for {
                return Err(
                    "idtoken: audience provided does not match aud claim in the JWT".into(),
                );
            }
            Ok(Some(google_payload(minted_for)))
        }));
        authenticate(&g, request("/", &["Bearer some-token"])).await
    }

    let accepted = authenticate_minted_for(TEST_AUDIENCE).await;
    assert!(
        accepted.is_ok(),
        "token for the configured audience: {accepted:?}"
    );
    let refused = authenticate_minted_for("https://another-agent.example.com").await;
    assert!(
        is_unauthenticated(&refused),
        "token for another audience: {refused:?}, want Unauthenticated"
    );
}

// parity: adapted: the config is moved into the constructor, so the caller mutates its own clone
#[tokio::test]
async fn test_new_google_oidc_copies_allow_list() {
    let mut cfg = GoogleOidcConfig {
        audience: TEST_AUDIENCE.into(),
        allowed_service_accounts: vec![TEST_SERVICE_ACCOUNT.into()],
    };
    let mut g = new_google_oidc(cfg.clone()).unwrap();
    cfg.allowed_service_accounts[0] = "attacker@evil.example.com".into();

    g.validate = Some(payload_validator(|aud| {
        let mut p = google_payload(aud);
        p.claims
            .insert("email".into(), json!("attacker@evil.example.com"));
        p
    }));
    let got = authenticate(&g, request("/", &["Bearer valid-token"])).await;
    assert!(
        is_forbidden(&got),
        "Authenticate() = {got:?}, want Forbidden"
    );
}

#[tokio::test]
async fn test_google_oidc_validate_panic_is_rejected() {
    let g = oidc(
        &[TEST_SERVICE_ACCOUNT],
        Some(validator(|_, _| {
            panic!("slice bounds out of range [:32] with capacity 0")
        })),
    );
    let got = authenticate(&g, request("/", &["Bearer malformed-token"])).await;
    match &got {
        Err(AuthnError::Unauthenticated(Some(cause))) => assert_eq!(
            cause,
            "invalid identity token: identity token validation panicked: slice bounds out of range [:32] with capacity 0"
        ),
        other => panic!("Authenticate() = {other:?}, want Unauthenticated"),
    }

    // A validator whose future panics is rejected the same way.
    let g = oidc(
        &[TEST_SERVICE_ACCOUNT],
        Some(Arc::new(|_: String, _: String| {
            Box::pin(async { panic!("panicked while verifying") })
        })),
    );
    let got = serve(Arc::new(g), request("/", &["Bearer malformed-token"])).await;
    assert_eq!(got.status, StatusCode::UNAUTHORIZED);
    assert!(
        got.next_saw.is_none(),
        "the next handler ran on a token that failed validation"
    );
}

#[tokio::test]
async fn test_google_oidc_nil_validator_is_rejected() {
    let g = oidc(&[TEST_SERVICE_ACCOUNT], None);
    let got = serve(Arc::new(g), request("/", &["Bearer some-token"])).await;
    assert_eq!(got.status, StatusCode::UNAUTHORIZED);
    assert!(
        got.next_saw.is_none(),
        "the next handler ran with no validator installed"
    );
}

#[test]
fn test_bearer_token() {
    let cases: &[(&str, &[&str], Option<&str>)] = &[
        ("no header", &[], None),
        ("empty header value", &[""], None),
        ("scheme only", &["Bearer"], None),
        ("empty credential", &["Bearer "], None),
        ("whitespace credential", &["Bearer    "], None),
        ("wrong scheme", &["Basic dXNlcjpwYXNz"], None),
        ("two headers", &["Bearer a", "Bearer b"], None),
        ("canonical", &["Bearer abc"], Some("abc")),
        ("lowercase scheme", &["bearer abc"], Some("abc")),
        ("mixed-case scheme", &["BeArEr abc"], Some("abc")),
        ("padded credential", &["Bearer   abc  "], Some("abc")),
    ];
    for (name, headers, want) in cases {
        let headers: Vec<String> = headers.iter().map(|h| (*h).to_owned()).collect();
        let got = bearer_token(&headers);
        match want {
            None => assert!(got.is_err(), "{name}: bearerToken = {got:?}, want an error"),
            Some(w) => assert_eq!(got.as_deref(), Ok(*w), "{name}"),
        }
    }
}
