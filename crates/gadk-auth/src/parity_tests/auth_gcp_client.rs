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

//! Port of ADK-Go `auth/gcp/client_test.go` (package `gcp`, an internal test: it reaches the
//! client's private fields, `map_credential` and the redaction layer).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::credential::{Bearer, Credential};
use crate::error::find_consent;
use crate::gcp::client::{
    Client, Config, DEFAULT_AGENT_IDENTITY_URL, DEFAULT_CONNECTOR_URL, DEFAULT_INITIAL_BACKOFF,
    DEFAULT_POLL_TIMEOUT, Request, Retrieval, map_credential,
};
use crate::gcp::redact::{
    MAX_ERROR_BODY, MAX_SCRUBBABLE_SECRET, MAX_STRADDLE_WINDOW, REDACTED_MARKER, WITHHELD_TEXT,
    decode_fully, go_to_lower, redact, redacted_for_error, truncate_for_error,
};
use crate::gcp::{ApiError, Error, ErrorKind, find_api_error, is_kind};
use crate::providers::AdcSource;
use crate::testserver::{Reply, TestServer};

/// Go `wantBearer`: fails unless `cred` is a `Bearer` carrying `token`.
fn want_bearer(cred: &Arc<dyn Credential>, token: &str) {
    let Some(b) = cred.downcast_ref::<Bearer>() else {
        panic!("credential = {cred:?}, want Bearer");
    };
    assert_eq!(b.token, token, "bearer token");
}

/// Go `wantAPIKey`: applying `cred` sets header `name` and the `X-Goog-Api-Key` mirror
/// (adk-python parity) to `value`.
async fn want_api_key(cred: &Arc<dyn Credential>, name: &str, value: &str) {
    let mut h = http::HeaderMap::new();
    if let Err(e) = cred.apply(&mut h).await {
        panic!("cred.apply() error = {e}");
    }
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    assert_eq!(get(name).as_deref(), Some(value), "header {name:?}");
    assert_eq!(
        get("X-Goog-Api-Key").as_deref(),
        Some(value),
        "X-Goog-Api-Key (adk-python parity)"
    );
}

/// Go `fakeADC`: Application Default Credentials pointed at a local token server, so
/// NewClient's ADC branch runs offline. The key arrives through `AdcSource::Json`, because
/// setting GOOGLE_APPLICATION_CREDENTIALS needs `unsafe` `env::set_var`. The token server must
/// outlive the client, so it is returned alongside the source.
async fn fake_adc() -> (TestServer, AdcSource) {
    let token_srv = TestServer::start(|_req| async {
        Reply::json(r#"{"access_token":"ADC-TOKEN","token_type":"Bearer","expires_in":1}"#)
    })
    .await;
    let adc = AdcSource::Json(serde_json::json!({
        "type": "authorized_user",
        "client_id": "c",
        "client_secret": "s",
        "refresh_token": "r",
        "token_uri": token_srv.url(),
    }));
    (token_srv, adc)
}

// parity: adapted: the "nil config uses defaults" case takes the ADC branch through `Client::with_adc(Config::default(), &AdcSource::Json(fake))` instead of GOOGLE_APPLICATION_CREDENTIALS, and Go's "httpClient = nil" check has no counterpart because `Client::http` is not optional
#[tokio::test]
async fn test_new_client() {
    // defaults: supply http_client so the constructor skips the ADC lookup (offline test).
    let c = Client::new(Config {
        http_client: Some(reqwest::Client::new().into()),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("defaults: NewClient() error = {e}"));
    assert_eq!(
        c.agent_identity_url, DEFAULT_AGENT_IDENTITY_URL,
        "defaults: agentIdentityURL"
    );
    assert_eq!(
        c.connector_url, DEFAULT_CONNECTOR_URL,
        "defaults: connectorURL"
    );
    assert_eq!(
        c.poll_timeout, DEFAULT_POLL_TIMEOUT,
        "defaults: pollTimeout"
    );
    assert_eq!(
        c.initial_backoff, DEFAULT_INITIAL_BACKOFF,
        "defaults: initialBackoff"
    );

    // nil config uses defaults: the documented zero-Config path, which takes the ADC branch.
    let (_token_srv, adc) = fake_adc().await;
    let c = Client::with_adc(Config::default(), &adc)
        .unwrap_or_else(|e| panic!("nil config: NewClient() error = {e}"));
    assert_eq!(
        (c.agent_identity_url.as_str(), c.connector_url.as_str()),
        (DEFAULT_AGENT_IDENTITY_URL, DEFAULT_CONNECTOR_URL),
        "nil config: endpoints, want the defaults"
    );
    assert_eq!(
        c.poll_timeout, DEFAULT_POLL_TIMEOUT,
        "nil config: pollTimeout"
    );

    // trims endpoint trailing slash
    let c = Client::new(Config {
        http_client: Some(reqwest::Client::new().into()),
        agent_identity_endpoint: "https://ai.example.com/".into(),
        connector_endpoint: "https://conn.example.com/".into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("trims: NewClient() error = {e}"));
    assert_eq!(
        c.agent_identity_url, "https://ai.example.com",
        "agentIdentityURL, want trailing slash trimmed"
    );
    assert_eq!(
        c.connector_url, "https://conn.example.com",
        "connectorURL, want trailing slash trimmed"
    );
}

#[tokio::test]
async fn test_map_credential() {
    #[derive(Default)]
    struct Case {
        name: &'static str,
        header: &'static str,
        token: &'static str,
        want_bearer: &'static str, // non-empty => expect bearer token
        want_api_key: (&'static str, &'static str),
        want_err: bool,
    }
    let cases = [
        Case {
            name: "authorization bearer",
            header: "Authorization: Bearer",
            token: "t",
            want_bearer: "t",
            ..Default::default()
        },
        Case {
            name: "authorization bearer lowercase",
            header: "authorization: bearer",
            token: "t",
            want_bearer: "t",
            ..Default::default()
        },
        Case {
            name: "custom header",
            header: "X-Goog-Api-Key",
            token: "k",
            want_api_key: ("X-Goog-Api-Key", "k"),
            ..Default::default()
        },
        // A name that is NOT X-Goog-Api-Key: with the mirror deleted, the two assertions in
        // want_api_key would otherwise read the same header and pass.
        Case {
            name: "third-party header is mirrored",
            header: "X-Acme-Token",
            token: "k",
            want_api_key: ("X-Acme-Token", "k"),
            ..Default::default()
        },
        Case {
            name: "empty header",
            header: "",
            token: "t",
            want_err: true,
            ..Default::default()
        },
        Case {
            name: "empty token",
            header: "Authorization: Bearer",
            token: "",
            want_err: true,
            ..Default::default()
        },
        Case {
            name: "header carrying a scheme is not a usable field name",
            header: "X-Api-Key: Token",
            token: "k",
            want_err: true,
            ..Default::default()
        },
        Case {
            name: "bare authorization maps to an api key",
            header: "Authorization",
            token: "k",
            want_api_key: ("Authorization", "k"),
            ..Default::default()
        },
    ];
    for tc in cases {
        let res = map_credential(tc.header, tc.token, &[]);
        if tc.want_err {
            assert!(
                res.is_err(),
                "{}: mapCredential() = nil error, want error",
                tc.name
            );
            continue;
        }
        let cred = res.unwrap_or_else(|e| panic!("{}: mapCredential() error = {e}", tc.name));
        if !tc.want_bearer.is_empty() {
            want_bearer(&cred, tc.want_bearer);
        } else {
            want_api_key(&cred, tc.want_api_key.0, tc.want_api_key.1).await;
        }
    }
}

/// The rejected header name is service-controlled and reaches the error by a third path,
/// separate from a response body and an operation message. Sized inside the examinable length
/// on purpose: past it the name is withheld before the cap is consulted.
#[test]
fn test_map_credential_caps_header_name_in_error() {
    let header = "x".repeat(MAX_ERROR_BODY * 4) + ": Token";
    let Err(err) = map_credential(&header, "SECRET-TOKEN", &[]) else {
        panic!("mapCredential() = nil error, want error");
    };
    let msg = err.to_string();
    assert!(
        msg.len() <= 2 * MAX_ERROR_BODY,
        "error is {} bytes, want the header name capped to {MAX_ERROR_BODY}",
        msg.len()
    );
    // A forward guard: the token must never reach an error however the message is reworded.
    assert!(!msg.contains("SECRET-TOKEN"), "error carries the token");
}

// parity: adapted: the "non utf8 keeps context" case ("\x80" x 2000) cannot be a &str (service bodies are decoded with from_utf8_lossy first, contract note 6); the other two cases are ported
#[test]
fn test_truncate_for_error() {
    let cases = [
        ("short is unchanged", "nope".to_string(), "nope".to_string()),
        ("long is cut", "a".repeat(2000), "a".repeat(1024) + "..."),
    ];
    let tail = |s: &str| s[s.len().saturating_sub(8)..].to_string();
    for (name, input, want) in cases {
        // Report the tail as well as the length: a body cut at the wrong place can still come
        // out the right size.
        let (got, cut) = truncate_for_error(&input);
        assert!(
            got == want && cut == (input != want),
            "{name}: truncateForError() = {} bytes ending {:?}, want {} bytes ending {:?}",
            got.len(),
            tail(&got),
            want.len(),
            tail(&want)
        );
    }
}

// parity: adapted: std::time::Duration cannot be negative, so `poll_timeout: Duration::ZERO` (the other non-positive value) is checked to take the default instead
#[tokio::test]
async fn test_new_client_rejects_negative_poll_timeout() {
    let srv = TestServer::start(|_req| async { Reply::ok("") }).await;
    let c = Client::new(Config {
        http_client: Some(srv.client().into()),
        poll_timeout: Duration::ZERO,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("NewClient() error = {e}"));
    assert_eq!(
        c.poll_timeout, DEFAULT_POLL_TIMEOUT,
        "pollTimeout = {:?}, want the default {:?} (a non-positive value must not mean 'never retry')",
        c.poll_timeout, DEFAULT_POLL_TIMEOUT
    );
}

/// The two ways one pass per value went wrong; both were reachable in production, where every
/// call site passes UserID and ContinueURI together.
#[test]
fn test_redact_across_several_secrets() {
    let cases: [(&str, &str, &[&str], &str); 10] = [
        // Redacting the URI first inserted "[redacted]", and the pass for "e" then matched the
        // e's inside that marker.
        (
            "a secret that occurs inside the marker",
            "error https://app.example.test/cb user e",
            &["https://app.example.test/cb", "e"],
            "[redacted]rror [redacted] us[redacted]r [redacted]",
        ),
        // Redacting the user first broke the URI that contains it.
        (
            "a secret contained in a longer secret",
            "bad continueUri: https://example.test/cb?user=alice",
            &["alice", "https://example.test/cb?user=alice"],
            "bad continueuri: [redacted]",
        ),
        // A straddle: the URI's occurrence starts before the user's and ends inside it.
        (
            "a secret straddling the end of another",
            "invalid: https://app.example.test/cb?login=alice@example.test",
            &["alice@example.test", "https://app.example.test/cb?login=al"],
            "invalid: [redacted]",
        ),
        (
            "the shortest straddle",
            "xalice",
            &["xa", "alice"],
            "[redacted]",
        ),
        // A SECOND occurrence of the same secret starting inside the range the first choice
        // covered.
        (
            "a second occurrence inside the chosen range",
            "https://cb.test/u/alice@example.test/alice@example.test",
            &[
                "alice@example.test",
                "https://cb.test/u/alice@example.test/a",
            ],
            "[redacted]",
        ),
        // Any value whose prefix equals its suffix overlaps itself.
        (
            "a secret that overlaps itself",
            "aaa",
            &["aa"],
            "[redacted]",
        ),
        // Longer than the exhaustive test's four-byte bodies, with a secret that tiles them.
        (
            "a repeated secret longer than the exhaustive bodies",
            "aaaaaa",
            &["aa"],
            "[redacted]",
        ),
        (
            "the same with the secret embedded in text",
            "before ababababab after",
            &["ab"],
            "before [redacted] after",
        ),
        // The no-match branch: the text comes back alone, case and all.
        (
            "no match leaves the text alone, case and all",
            "Bad Request: NOPE",
            &["alice", "https://example.test/cb"],
            "Bad Request: NOPE",
        ),
        (
            "order does not matter",
            "bad continueUri: https://example.test/cb?user=alice",
            &["https://example.test/cb?user=alice", "alice"],
            "bad continueuri: [redacted]",
        ),
    ];
    for (name, input, secrets, want) in cases {
        let got = redact(input, secrets);
        assert_eq!(got, want, "{name}: redact({input:?}, {secrets:?})");
    }
}

/// Guards the scan against becoming quadratic and the extension walk against being bounded
/// away. Measures cost and nothing else (a CPU budget on the real clock, Go's 10 s ceiling).
#[test]
fn test_redact_stays_linear() {
    // doPost's own read cap, declared local to it in Go, so it is spelled out here.
    const MAX_BODY: usize = 1 << 20;
    let long_secret = "ab".repeat(16);
    let second_factor = "ab".repeat(MAX_SCRUBBABLE_SECRET / 2);
    let cases: [(&str, &str); 4] = [
        ("a one-character secret: the walk never runs", "e"),
        (
            "a repeated multi-byte secret: the walk runs for the whole body",
            "ab",
        ),
        (
            "a long repeated secret: the walk runs and each step compares more",
            &long_secret,
        ),
        (
            "a secret long enough to show the second factor",
            &second_factor,
        ),
    ];
    for (name, secret) in cases {
        let body = secret.repeat(MAX_BODY / secret.len());
        let start = std::time::Instant::now();
        let got = redact(&body, &[secret, "https://app.example.test/cb"]);
        let elapsed = start.elapsed();
        // One marker for the whole run, not one per match.
        assert_eq!(
            got.len(),
            "[redacted]".len(),
            "{name}: redact() produced {} bytes, want {} — every byte matches, so the whole body is one redacted run",
            got.len(),
            "[redacted]".len()
        );
        assert!(
            elapsed <= Duration::from_secs(10),
            "{name}: redact() over {MAX_BODY} bytes took {elapsed:?}; the scan is meant to be linear in the body"
        );
    }
}

/// The ceiling on what one failed call can spend (Go's 5 s budget, real clock).
#[test]
fn test_service_text_cost_is_bounded() {
    let self_regen = |n: usize| r"\u005c".to_string() + &"u005c".repeat(n);
    let cases: [(&str, String, String, bool); 4] = [
        // The shape that measured 3m33.6s. The value is past the bound, so it is not matched
        // at all and nothing is shown.
        (
            "a value past the bound is not matched and nothing is shown",
            self_regen(12799),
            REDACTED_MARKER.repeat(102),
            true,
        ),
        // The service's own markers no longer multiply anything.
        (
            "a body of service-written markers does not multiply the value cost",
            "ab".repeat(2048),
            REDACTED_MARKER.repeat(102),
            false,
        ),
        // The worst case still reachable on the path that SHOWS something.
        (
            "a value at the bound against a body that tiles the window",
            "ab".repeat(MAX_SCRUBBABLE_SECRET / 2),
            "ab".repeat(MAX_STRADDLE_WINDOW / 2),
            false,
        ),
        // The same shape at doPost's read cap: withheld rather than examined in part.
        (
            "a value at the bound against a megabyte that tiles it",
            "ab".repeat(MAX_SCRUBBABLE_SECRET / 2),
            "ab".repeat((1 << 20) / 2),
            true,
        ),
    ];
    for (name, user, body, want_withheld) in cases {
        let start = std::time::Instant::now();
        let got = redacted_for_error(&body, &[user.as_str(), "https://app.test/cb"]);
        let elapsed = start.elapsed();
        assert_eq!(
            got == WITHHELD_TEXT,
            want_withheld,
            "{name}: redactedForError() withheld = {}, want {want_withheld}",
            got == WITHHELD_TEXT
        );
        assert!(
            elapsed <= Duration::from_secs(5),
            "{name}: redactedForError() over a {}-byte body with a {}-byte value took {elapsed:?}",
            body.len(),
            user.len()
        );
    }
}

/// Which side of the bound gets scrubbed and which gets nothing: a value one byte longer.
#[test]
fn test_scrubbable_bound_fails_closed() {
    let at_bound = "a".repeat(MAX_SCRUBBABLE_SECRET);
    // The body runs past the error-body cap either way, so the scrubbed answer carries the
    // ellipsis that says the rest was dropped.
    assert_eq!(
        redacted_for_error(&format!("denied for {at_bound}"), &[at_bound.as_str()]),
        format!("denied for {REDACTED_MARKER}..."),
        "at the bound"
    );
    let over_bound = "a".repeat(MAX_SCRUBBABLE_SECRET + 1);
    assert_eq!(
        redacted_for_error(&format!("denied for {over_bound}"), &[over_bound.as_str()]),
        WITHHELD_TEXT,
        "past the bound, want it withheld"
    );
}

const AUTH_PROVIDER_RESOURCE: &str = "projects/p/locations/l/authProviders/ap";
const CONNECTOR_RESOURCE: &str = "projects/p/locations/l/connectors/co";

/// Go `newTestClient`: both service endpoints at `srv`, and a tiny backoff so polling tests are
/// fast.
fn new_test_client(srv: &TestServer) -> Client {
    let mut c = Client::new(Config {
        http_client: Some(srv.client().into()),
        agent_identity_endpoint: srv.url().to_string(),
        connector_endpoint: srv.url().to_string(),
        poll_timeout: Duration::from_secs(2),
    })
    .unwrap_or_else(|e| panic!("NewClient() error = {e}"));
    c.initial_backoff = Duration::from_millis(1);
    c
}

fn request(resource: &str, user_id: &str) -> Request {
    Request {
        resource: resource.into(),
        user_id: user_id.into(),
        ..Default::default()
    }
}

/// A server answering every request with `status` and `body` (Go's
/// `w.WriteHeader(status); io.WriteString(w, body)` handlers).
async fn status_server(status: u16, body: impl Into<Vec<u8>>) -> TestServer {
    let body: Vec<u8> = body.into();
    TestServer::start(move |_req| {
        let body = body.clone();
        async move { Reply::status(status, body) }
    })
    .await
}

/// The error of a call that must fail.
fn want_err(got: Result<Retrieval, Error>, what: &str) -> Error {
    match got {
        Ok(r) => panic!("{what}: RetrieveCredential() = {r:?}, nil error; want an error"),
        Err(e) => e,
    }
}

/// Go `errors.As(err, &apiErr)`.
fn api_error(err: &Error) -> &ApiError {
    find_api_error(err).unwrap_or_else(|| panic!("error = {err}, want *APIError"))
}

/// Drives RetrieveCredential end to end for both services via a server that replays response
/// bodies in order; each case asserts one expected outcome. Real clock: the backoff is 1 ms and
/// the one polling-to-timeout row waits 30 ms (design note 6).
#[tokio::test]
async fn test_retrieve_credential() {
    #[derive(Default)]
    struct Case {
        name: &'static str,
        resource: &'static str,
        bodies: &'static [&'static str],
        want_calls: usize,         // >0 => assert the number of service calls
        want_bearer: &'static str, // expect a Bearer carrying this token
        want_expiry: &'static str, // non-empty => expect this as expires_at
        want_api_key: (&'static str, &'static str), // expect an API-key credential {name, value}
        want_consent: (&'static str, &'static str), // expect ConsentRequiredError {auth_uri, nonce}
        want_err_is: Option<ErrorKind>, // expect is_kind(err, kind)
        want_err_text: &'static str, // expect err to contain this substring
        poll_timeout: Duration,
    }
    let cases = [
        // Agent Identity: synchronous "result" oneof.
        Case {
            name: "agent identity bearer",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[r#"{"success":{"token":"tok","header":"Authorization: Bearer"}}"#],
            want_bearer: "tok",
            ..Default::default()
        },
        Case {
            name: "agent identity bearer with expiry",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[
                r#"{"success":{"token":"tok","header":"Authorization: Bearer","expireTime":"2999-01-01T00:00:00Z"}}"#,
            ],
            want_bearer: "tok",
            want_expiry: "2999-01-01T00:00:00Z",
            ..Default::default()
        },
        // An unparseable expiry leaves expires_at None, which the provider reads as "don't
        // cache"; the credential itself is still usable.
        Case {
            name: "agent identity bearer with unparseable expiry",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[
                r#"{"success":{"token":"tok","header":"Authorization: Bearer","expireTime":"not-a-time"}}"#,
            ],
            want_bearer: "tok",
            ..Default::default()
        },
        Case {
            name: "agent identity custom header",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[r#"{"success":{"token":"KEY","header":"X-Goog-Api-Key"}}"#],
            want_api_key: ("X-Goog-Api-Key", "KEY"),
            ..Default::default()
        },
        Case {
            name: "agent identity consent required",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[
                r#"{"uriConsentRequired":{"authorizationUri":"https://consent","consentNonce":"n"}}"#,
            ],
            want_consent: ("https://consent", "n"),
            ..Default::default()
        },
        Case {
            name: "agent identity consent rejected",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[r#"{"consentRejected":{}}"#],
            want_err_is: Some(ErrorKind::ConsentRejected),
            ..Default::default()
        },
        Case {
            name: "agent identity polls pending then succeeds",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &[
                r#"{"pending":{}}"#,
                r#"{"success":{"token":"tok","header":"Authorization: Bearer"}}"#,
            ],
            want_bearer: "tok",
            want_calls: 2,
            ..Default::default()
        },
        // IAM Connector: google.longrunning.Operation wrapper.
        Case {
            name: "connector bearer",
            resource: CONNECTOR_RESOURCE,
            bodies: &[
                r#"{"done":true,"response":{"@type":"x","token":"tok","header":"Authorization: Bearer"}}"#,
            ],
            want_bearer: "tok",
            ..Default::default()
        },
        // The connector's expireTime shape is an assumption, so a value of another JSON type
        // must cost the cache entry, not the credential.
        Case {
            name: "connector expireTime of an unexpected JSON type",
            resource: CONNECTOR_RESOURCE,
            bodies: &[
                r#"{"done":true,"response":{"token":"tok","header":"Authorization: Bearer","expireTime":{"seconds":1798761600}}}"#,
            ],
            want_bearer: "tok",
            ..Default::default()
        },
        // The connector reads its expiry from the operation's response, a different path from
        // Agent Identity's.
        Case {
            name: "connector bearer with expiry",
            resource: CONNECTOR_RESOURCE,
            bodies: &[
                r#"{"done":true,"response":{"token":"tok","header":"Authorization: Bearer","expireTime":"2999-01-01T00:00:00Z"}}"#,
            ],
            want_bearer: "tok",
            want_expiry: "2999-01-01T00:00:00Z",
            ..Default::default()
        },
        Case {
            name: "connector polls consent pending then succeeds",
            resource: CONNECTOR_RESOURCE,
            bodies: &[
                r#"{"metadata":{"@type":"x","consentPending":{}}}"#,
                r#"{"done":true,"response":{"token":"tok","header":"Authorization: Bearer"}}"#,
            ],
            want_bearer: "tok",
            want_calls: 2,
            ..Default::default()
        },
        Case {
            name: "connector consent required",
            resource: CONNECTOR_RESOURCE,
            bodies: &[
                r#"{"metadata":{"uriConsentRequired":{"authorizationUri":"https://c","consentNonce":"n"}}}"#,
            ],
            want_consent: ("https://c", "n"),
            ..Default::default()
        },
        Case {
            name: "connector consent rejected",
            resource: CONNECTOR_RESOURCE,
            bodies: &[r#"{"metadata":{"consentRejected":{}}}"#],
            want_err_is: Some(ErrorKind::ConsentRejected),
            ..Default::default()
        },
        Case {
            name: "connector operation error",
            resource: CONNECTOR_RESOURCE,
            bodies: &[r#"{"error":{"message":"boom"}}"#],
            want_err_text: "boom",
            ..Default::default()
        },
        // A terminal (done) operation carrying no credential must fail fast, not be polled to
        // the timeout.
        Case {
            name: "connector done without credential",
            resource: CONNECTOR_RESOURCE,
            bodies: &[r#"{"done":true}"#],
            want_err_text: "no credential",
            ..Default::default()
        },
        // The two services deliberately disagree on an unrecognized 200: Agent Identity's
        // result is a closed oneof, so an unknown arm can only be a mismatch worth failing on...
        Case {
            name: "agent identity unrecognized result fails fast",
            resource: AUTH_PROVIDER_RESOURCE,
            bodies: &["{}"],
            want_err_text: "empty result",
            want_calls: 1,
            ..Default::default()
        },
        // ...whereas a connector operation that is merely not done yet is normal, so an
        // unrecognized one keeps being polled until the timeout.
        Case {
            name: "connector unrecognized operation polls to timeout",
            resource: CONNECTOR_RESOURCE,
            bodies: &["{}"],
            want_err_is: Some(ErrorKind::PollTimeout),
            poll_timeout: Duration::from_millis(30),
            ..Default::default()
        },
    ];
    for tc in cases {
        let name = tc.name;
        let srv = TestServer::sequence(tc.bodies).await;
        let mut c = new_test_client(&srv);
        if !tc.poll_timeout.is_zero() {
            c.poll_timeout = tc.poll_timeout;
        }
        let got = c.retrieve_credential(request(tc.resource, "u")).await;

        if !tc.want_bearer.is_empty() {
            let got = got.unwrap_or_else(|e| panic!("{name}: RetrieveCredential() error = {e}"));
            want_bearer(&got.credential, tc.want_bearer);
            if tc.want_expiry.is_empty() {
                assert_eq!(got.expires_at, None, "{name}: ExpiresAt, want zero");
            } else {
                // Parsed independently of parse_expire_time: a mistyped want must not parse to
                // the "no expiry" a dropped expiry produces.
                let want = DateTime::parse_from_rfc3339(tc.want_expiry)
                    .unwrap_or_else(|e| {
                        panic!("{name}: parse wantExpiry {:?}: {e}", tc.want_expiry)
                    })
                    .with_timezone(&Utc);
                assert_eq!(got.expires_at, Some(want), "{name}: ExpiresAt");
            }
        } else if !tc.want_api_key.0.is_empty() {
            let got = got.unwrap_or_else(|e| panic!("{name}: RetrieveCredential() error = {e}"));
            want_api_key(&got.credential, tc.want_api_key.0, tc.want_api_key.1).await;
        } else if !tc.want_consent.0.is_empty() {
            let err = want_err(got, name);
            let Some(consent) = find_consent(&err) else {
                panic!("{name}: error = {err}, want ConsentRequiredError");
            };
            // Compare the fields: the consent error's Display reports neither of them.
            assert_eq!(
                (consent.auth_uri.as_str(), consent.nonce.as_str()),
                tc.want_consent,
                "{name}: consent {{authURI, nonce}}"
            );
        } else if let Some(kind) = tc.want_err_is {
            let err = want_err(got, name);
            assert!(
                is_kind(&err, kind),
                "{name}: error = {err}, want errors.Is {kind}"
            );
        } else if !tc.want_err_text.is_empty() {
            let err = want_err(got, name);
            assert!(
                err.to_string().contains(tc.want_err_text),
                "{name}: error = {err}, want it to contain {:?}",
                tc.want_err_text
            );
        } else {
            panic!("test case {name:?} sets no expectation");
        }

        if tc.want_calls != 0 {
            assert_eq!(srv.calls(), tc.want_calls, "{name}: service calls");
        }
    }
}

#[tokio::test]
async fn test_retrieve_routes_by_resource() {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default, rename = "userId")]
        user_id: String,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default, rename = "continueUri")]
        continue_uri: String,
    }
    for (name, resource, want_prefix) in [
        ("connector", CONNECTOR_RESOURCE, "/v1alpha/"),
        ("auth provider", AUTH_PROVIDER_RESOURCE, "/v1/"),
    ] {
        let srv = TestServer::start(|_req| async {
            Reply::ok(
                r#"{"done":true,"response":{"token":"t","header":"Authorization: Bearer"},"success":{"token":"t","header":"Authorization: Bearer"}}"#,
            )
        })
        .await;
        let req = Request {
            resource: resource.into(),
            user_id: "user-1".into(),
            scopes: vec!["scope-a".into(), "scope-b".into()],
            continue_uri: "https://example.test/continue".into(),
            ..Default::default()
        };
        if let Err(e) = new_test_client(&srv).retrieve_credential(req).await {
            panic!("{name}: RetrieveCredential() error = {e}");
        }
        let reqs = srv.requests();
        let got = &reqs[0];
        assert_eq!(got.method, "POST", "{name}: method, want POST");
        assert!(
            got.path.starts_with(want_prefix)
                && got.path.contains(resource)
                && got.path.ends_with("/credentials:retrieve"),
            "{name}: path = {:?}, want prefix {want_prefix:?} containing {resource:?} and suffix :retrieve",
            got.path
        );
        let body: Body = got.json();
        assert_eq!(body.user_id, "user-1", "{name}: body userId");
        assert_eq!(body.scopes, ["scope-a", "scope-b"], "{name}: body scopes");
        // ContinueURI is what makes the 3-legged flow work, so a wrong tag here would be
        // silent and expensive.
        assert_eq!(
            body.continue_uri, "https://example.test/continue",
            "{name}: body continueUri"
        );
    }
}

#[tokio::test]
async fn test_retrieve_http_error() {
    let srv = TestServer::start(|_req| async {
        // Go http.Error(w, "nope", 500): text/plain, nosniff and a trailing newline.
        Reply::status(500, "nope\n")
            .with_header("Content-Type", "text/plain; charset=utf-8")
            .with_header("X-Content-Type-Options", "nosniff")
    })
    .await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
            .await,
        "500",
    );
    let api = api_error(&err);
    assert_eq!(api.status_code, 500, "StatusCode");
    assert!(
        api.body.contains("nope"),
        "Body = {:?}, want it to carry the response body",
        api.body
    );
}

/// Each row names the message it expects, not a substring true of every error this method
/// returns: with validate_resource deleted, these requests reach the resource decoration and an
/// assertion on "resource " alone would stay green.
#[tokio::test]
async fn test_retrieve_validates_request() {
    const CHARSET: &str = "has invalid characters";
    const SEGMENT: &str = "has an empty or relative path segment";
    const MISSING: &str = "requires a";
    let cases: [(&str, Request, &str); 11] = [
        (
            "missing resource",
            Request {
                user_id: "u".into(),
                ..Default::default()
            },
            MISSING,
        ),
        (
            "missing user id",
            Request {
                resource: AUTH_PROVIDER_RESOURCE.into(),
                ..Default::default()
            },
            MISSING,
        ),
        (
            "resource path traversal",
            request("projects/p/../q/authProviders/a", "u"),
            SEGMENT,
        ),
        (
            "resource query injection",
            request("projects/p/authProviders/a?x=1", "u"),
            CHARSET,
        ),
        (
            "resource with space",
            request("projects/p/authProviders/a b", "u"),
            CHARSET,
        ),
        // A name that normalizes to a different one routes to a different service than the
        // one validate_resource inspected.
        (
            "resource empty segment",
            request("projects/p//authProviders/a", "u"),
            SEGMENT,
        ),
        (
            "resource trailing slash",
            request("projects/p/locations/l/connectors/c/", "u"),
            SEGMENT,
        ),
        (
            "resource dot segment",
            request("projects/p/locations/l/connectors/c/.", "u"),
            SEGMENT,
        ),
        // Percent-escapes are rejected by the charset, not decoded.
        (
            "resource percent-escaped dot",
            request("projects/p/authProviders/a%2e%2e", "u"),
            CHARSET,
        ),
        (
            "resource percent-escaped slash",
            request("projects/p%2flocations/authProviders/a", "u"),
            CHARSET,
        ),
        (
            "resource bare percent",
            request("projects/p/authProviders/a%", "u"),
            CHARSET,
        ),
    ];
    // A live server: a client with no endpoint fails at transport for every input, which
    // cannot tell a rejected request from an unreachable one.
    let srv = TestServer::start(|_req| async { Reply::ok("") }).await;
    let c = new_test_client(&srv);
    for (name, req, want_msg) in cases {
        let before = srv.calls();
        let err = want_err(c.retrieve_credential(req.clone()).await, name);
        assert!(
            err.to_string().contains(want_msg),
            "{name}: RetrieveCredential({req:?}) error = {err}, want it to say {want_msg:?}"
        );
        assert_eq!(
            srv.calls() - before,
            0,
            "{name}: credentials service called; a rejected request must not reach the wire"
        );
    }
}

/// The other side of the boundary test_retrieve_validates_request guards.
#[tokio::test]
async fn test_retrieve_accepts_resource_names() {
    for (name, resource) in [
        // A domain-scoped project id: the colon is safe because the name always follows a
        // scheme, a host and a version segment.
        (
            "domain-scoped project id",
            "projects/example.com:my-project/locations/l/authProviders/a",
        ),
        // Dots inside a segment, as opposed to a "." or ".." segment of their own.
        (
            "dots inside a segment",
            "projects/p/locations/l/authProviders/a..b",
        ),
        (
            "leading dot in a segment",
            "projects/p/locations/l/authProviders/.hidden",
        ),
    ] {
        let srv = TestServer::start(|_req| async {
            Reply::json(r#"{"success":{"header":"Authorization: Bearer","token":"tok"}}"#)
        })
        .await;
        if let Err(e) = new_test_client(&srv)
            .retrieve_credential(request(resource, "u"))
            .await
        {
            panic!("{name}: RetrieveCredential({resource:?}) error = {e}, want it accepted");
        }
        // The name must reach the wire unchanged: validation and routing both ran on the
        // string the server is about to resolve.
        let want = format!("/v1/{resource}/credentials:retrieve");
        assert_eq!(srv.requests()[0].path, want, "{name}: request path");
    }
}

// parity: adapted: a Rust caller gives up by dropping the retrieve_credential future (tokio::time::timeout under paused time), so Go's context.Canceled error has no counterpart; the promptness check stays
#[tokio::test(start_paused = true)]
async fn test_retrieve_context_canceled_while_pending() {
    let srv = TestServer::sequence(&[r#"{"pending":{}}"#]).await; // never resolves
    let mut c = new_test_client(&srv);
    // A backoff far longer than the window asserted below: without the give-up arm of the
    // poll wait, only the promptness (the point here) is lost.
    c.poll_timeout = Duration::from_secs(60);
    c.initial_backoff = Duration::from_secs(30);

    let start = tokio::time::Instant::now();
    let res = tokio::time::timeout(
        Duration::from_millis(20),
        c.retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u")),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(
        res.is_err(),
        "RetrieveCredential() = {res:?}, want it abandoned at the 20ms give-up"
    );
    assert!(
        elapsed <= Duration::from_secs(5),
        "returned after {elapsed:?}, want promptly after the give-up (backoff was {:?})",
        c.initial_backoff
    );
}

/// A service stuck in the non-interactive pending state past the poll timeout surfaces
/// PollTimeout (no hang). Paused time: the outcome holds however far the clock jumps.
#[tokio::test(start_paused = true)]
async fn test_retrieve_poll_timeout() {
    let srv = TestServer::sequence(&[r#"{"pending":{}}"#]).await; // never resolves
    let mut c = new_test_client(&srv);
    c.poll_timeout = Duration::from_millis(30);
    let err = want_err(
        c.retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
            .await,
        "poll timeout",
    );
    assert!(
        is_kind(&err, ErrorKind::PollTimeout),
        "RetrieveCredential() error = {err}, want PollTimeout"
    );
}

// parity: adapted: the ADC branch runs through `Client::with_adc` with a fake authorized_user key whose token_uri is a TestServer, instead of GOOGLE_APPLICATION_CREDENTIALS
#[tokio::test]
async fn test_new_client_refuses_redirects() {
    let (_token_srv, adc) = fake_adc().await;
    let target = TestServer::start(|_req| async {
        Reply::ok(r#"{"success":{"token":"attacker","header":"Authorization: Bearer"}}"#)
    })
    .await;
    let target_url = target.url().to_string();
    let redirector = TestServer::start(move |req| {
        let to = format!("{target_url}{}", req.path);
        async move { Reply::redirect(to, 307) }
    })
    .await;

    let c = Client::with_adc(
        Config {
            agent_identity_endpoint: redirector.url().to_string(),
            connector_endpoint: redirector.url().to_string(),
            ..Default::default()
        },
        &adc,
    )
    .unwrap_or_else(|e| panic!("NewClient() error = {e}"));
    let err = want_err(
        c.retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
            .await,
        "want the 3xx surfaced as an error",
    );
    assert_eq!(api_error(&err).status_code, 307, "error = {err}");
    // Go reads the target's Authorization header, which stays "" when it was never called.
    let target_saw_auth = target
        .requests()
        .iter()
        .find_map(|r| r.header("authorization").map(str::to_string));
    assert_eq!(
        target_saw_auth, None,
        "redirect target received Authorization; the token must not leave the configured host"
    );
}

// parity: adapted: `Client::new` takes no context, so there is no construction context to cancel; the test asserts a token minted after construction (fake ADC through `with_adc`) still authenticates the call
#[tokio::test]
async fn test_new_client_outlives_construction_ctx() {
    let (_token_srv, adc) = fake_adc().await;
    let srv =
        TestServer::sequence(&[r#"{"success":{"token":"tok","header":"Authorization: Bearer"}}"#])
            .await;
    let c = Client::with_adc(
        Config {
            agent_identity_endpoint: srv.url().to_string(),
            connector_endpoint: srv.url().to_string(),
            ..Default::default()
        },
        &adc,
    )
    .unwrap_or_else(|e| panic!("NewClient() error = {e}"));

    let got = c
        .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
        .await
        .unwrap_or_else(|e| panic!("RetrieveCredential() error = {e}"));
    want_bearer(&got.credential, "tok");
    assert_eq!(
        srv.requests()[0].header("authorization"),
        Some("Bearer ADC-TOKEN"),
        "the ADC token minted after construction authenticates the call"
    );
}

/// An error page big enough to trip the body cap must still report its status. Two sizes,
/// because the two paths bound the body by different means.
#[tokio::test]
async fn test_do_post_oversize_keeps_status() {
    for (name, size, want_shown) in [
        ("a page the check can examine", MAX_ERROR_BODY * 4, true),
        ("a page past the examinable length", (1 << 20) + 10, false),
    ] {
        let srv = status_server(502, "x".repeat(size)).await;
        let err = want_err(
            new_test_client(&srv)
                .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
                .await,
            name,
        );
        let api = api_error(&err);
        assert_eq!(api.status_code, 502, "{name}: StatusCode");
        assert_eq!(
            api.body != WITHHELD_TEXT,
            want_shown,
            "{name}: Body shown, want {want_shown} (Body is {} bytes)",
            api.body.len()
        );
        assert!(
            api.body.len() <= MAX_ERROR_BODY + "...".len(),
            "{name}: Body = {} bytes, want it capped to {MAX_ERROR_BODY}",
            api.body.len()
        );
    }
}

/// A service-controlled operation message must be capped like any response body; it reaches
/// the error by a different path than do_post's body.
#[tokio::test]
async fn test_retrieve_connector_error_message_is_capped() {
    let body = r#"{"error":{"code":7,"message":""#.to_string()
        + &"x".repeat(MAX_ERROR_BODY * 4)
        + r#""}}"#;
    let srv = TestServer::sequence(&[body.as_str()]).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request(CONNECTOR_RESOURCE, "u"))
            .await,
        "connector error",
    );
    let msg = err.to_string();
    assert!(
        msg.len() <= 2 * MAX_ERROR_BODY,
        "error is {} bytes, want the message capped to {MAX_ERROR_BODY}",
        msg.len()
    );
}

/// A 2xx body over the cap must be rejected, not decoded truncated (and thus garbled).
#[tokio::test]
async fn test_do_post_rejects_oversize_success() {
    let body = r#"{"success":{"token":"t","header":"Authorization: Bearer"}}"#.to_string()
        + &" ".repeat(1 << 20);
    let srv = status_server(200, body).await;
    let got = new_test_client(&srv)
        .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
        .await;
    match got {
        Err(e) if e.to_string().contains("exceeded") => {}
        other => panic!("error = {other:?}, want the oversize response rejected"),
    }
}

/// A service-controlled body must not be able to forge log lines through the returned error.
#[tokio::test]
async fn test_do_post_escapes_error_body() {
    let srv = status_server(
        503,
        "unavailable\r\nINFO auth: credential granted user=victim",
    )
    .await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
            .await,
        "503",
    );
    let msg = err.to_string();
    assert!(
        !msg.contains("\r\n"),
        "error carries raw control bytes: {msg:?}"
    );
    assert!(
        msg.contains(r"\r\n"),
        "error = {msg:?}, want the body escaped"
    );
}

/// The third service-text path, pinned against the acting user and not only against length.
/// The user is echoed INSIDE a larger name, so the surrounding text has to survive for the
/// assertion to be about redaction; the last row is the negative control.
#[tokio::test]
async fn test_map_credential_redacts_the_acting_user_in_error() {
    const USER: &str = "alice@example.test";
    // "@" and " " are not RFC 9110 token characters, so all three names are rejected.
    let cases: [(&str, String, &str, &[&str]); 3] = [
        (
            "the user echoed inside a larger name",
            format!("X-User-{USER}-Token"),
            USER,
            &["X-User-", "-Token", "not a usable HTTP header name"],
        ),
        (
            "the name is exactly the user",
            USER.to_string(),
            USER,
            &["not a usable HTTP header name"],
        ),
        (
            "a rejected name with no secret in it survives",
            "not a header".to_string(),
            "",
            &["not a header", "not a usable HTTP header name"],
        ),
    ];
    for (name, header, want_absent, want_present) in cases {
        let body = r#"{"success":{"header":""#.to_string() + &header + r#"","token":"t"}}"#;
        let srv = TestServer::sequence(&[body.as_str()]).await;
        let err = want_err(
            new_test_client(&srv)
                .retrieve_credential(request("projects/p/locations/l/authProviders/a", USER))
                .await,
            name,
        );
        let msg = err.to_string();
        if !want_absent.is_empty() {
            assert!(
                !msg.contains(want_absent),
                "{name}: error carries the acting user {want_absent:?}: {msg}"
            );
        }
        for want in want_present {
            // Case-insensitively: redact lowercases the text it scrubs.
            assert!(
                go_to_lower(&msg).contains(&go_to_lower(want)),
                "{name}: error lost {want:?}, so it is no longer diagnostic: {msg}"
            );
        }
    }
}

/// Pins the WIRING: each row fails on one deleted scrub call or one dropped secret and on
/// nothing else, and the surviving wording is asserted so a blanket drop fails too.
#[tokio::test]
async fn test_service_errors_redact_the_acting_user() {
    const USER: &str = "alice@example.test";
    // Deliberately free of the user id, so the two secrets can be asserted apart.
    const URI: &str = "https://app.test/cb?state=opaque";
    // Bearer material the Agent Identity route sends as forceRefreshToken. The "/" makes it an
    // unusable header name, so map_credential's arm is reachable.
    const PRIOR: &str = "prior-token/not-a-real-one";
    struct Case {
        name: &'static str,
        resource: &'static str,
        status: u16,
        body: String,
        want_absent: &'static [&'static str],
        want_present: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "an error body echoing the acting user",
            resource: AUTH_PROVIDER_RESOURCE,
            status: 403,
            body: format!("permission denied for {USER}"),
            want_absent: &[USER],
            want_present: &["permission denied for"],
        },
        Case {
            name: "an error body echoing the continue uri",
            resource: AUTH_PROVIDER_RESOURCE,
            status: 400,
            body: format!("continueUri not registered: {URI}"),
            want_absent: &[URI],
            want_present: &["continueuri not registered"],
        },
        // The connector operation's error arm.
        Case {
            name: "a connector operation message echoing the acting user",
            resource: CONNECTOR_RESOURCE,
            status: 200,
            body: format!(
                r#"{{"done":true,"error":{{"code":7,"message":"user {USER} is not permitted"}}}}"#
            ),
            want_absent: &[USER],
            want_present: &["is not permitted"],
        },
        Case {
            name: "a connector operation message echoing the continue uri",
            resource: CONNECTOR_RESOURCE,
            status: 200,
            body: format!(
                r#"{{"done":true,"error":{{"code":3,"message":"bad continueUri {URI}"}}}}"#
            ),
            want_absent: &[URI],
            want_present: &["bad continueuri"],
        },
        // The connector's own do_post secret list: only a non-2xx on a connector resource gets
        // here.
        Case {
            name: "a connector error body echoing the continue uri",
            resource: CONNECTOR_RESOURCE,
            status: 403,
            body: format!("continueUri not registered: {URI}"),
            want_absent: &[URI],
            want_present: &["continueuri not registered"],
        },
        // map_credential's secret list, reached only by a 2xx whose header name is unusable.
        Case {
            name: "an unusable header name echoing the continue uri",
            resource: AUTH_PROVIDER_RESOURCE,
            status: 200,
            body: format!(r#"{{"success":{{"header":"X-{URI}","token":"t"}}}}"#),
            want_absent: &[URI],
            want_present: &["not a usable HTTP header name"],
        },
        Case {
            name: "an error body echoing the prior token",
            resource: AUTH_PROVIDER_RESOURCE,
            status: 400,
            body: format!("invalid forceRefreshToken: {PRIOR}"),
            want_absent: &[PRIOR],
            want_present: &["invalid forcerefreshtoken"],
        },
        Case {
            name: "an unusable header name echoing the prior token",
            resource: AUTH_PROVIDER_RESOURCE,
            status: 200,
            body: format!(r#"{{"success":{{"header":"X-{PRIOR}","token":"t"}}}}"#),
            want_absent: &[PRIOR],
            want_present: &["not a usable HTTP header name"],
        },
        // The negative control: an error carrying neither identifier comes back whole.
        Case {
            name: "an error carrying neither identifier survives intact",
            resource: AUTH_PROVIDER_RESOURCE,
            status: 503,
            body: "backend overloaded, retry later".to_string(),
            want_absent: &[],
            want_present: &["backend overloaded, retry later"],
        },
    ];
    for tc in cases {
        let name = tc.name;
        let srv = status_server(tc.status, tc.body).await;
        let err = want_err(
            new_test_client(&srv)
                .retrieve_credential(Request {
                    resource: tc.resource.into(),
                    user_id: USER.into(),
                    continue_uri: URI.into(),
                    prior_token: PRIOR.into(),
                    ..Default::default()
                })
                .await,
            name,
        );
        let msg = err.to_string();
        for absent in tc.want_absent {
            assert!(
                !msg.contains(absent),
                "{name}: error carries {absent:?}: {msg}"
            );
        }
        for want in tc.want_present {
            // Case-insensitively: redact lowercases the text it scrubbed.
            assert!(
                go_to_lower(&msg).contains(&go_to_lower(want)),
                "{name}: error lost {want:?}, so it is no longer diagnostic: {msg}"
            );
        }
    }
}

/// The order the cap and the scrub run in: both simpler orders leak.
#[tokio::test]
async fn test_error_body_is_cut_before_the_scrub_not_after() {
    const USER: &str = "alice@example.test";
    const URI: &str = "https://app.example.test/callback";

    // redaction does not promote what the cut hid: percent-encoding is outside what the scrub
    // decodes, so if this tail reached the window it would be returned as it stands.
    let body = format!("{URI} ").repeat(34) + "alice%40example.test";
    let srv = status_server(403, body).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(Request {
                resource: AUTH_PROVIDER_RESOURCE.into(),
                user_id: USER.into(),
                continue_uri: URI.into(),
                ..Default::default()
            })
            .await,
        "promote",
    );
    let api = api_error(&err);
    assert!(
        !api.body.contains("alice%40example.test"),
        "Body reaches past the cut: {:?}",
        api.body
    );
    assert!(
        api.body.contains(REDACTED_MARKER),
        "Body = {:?}, want the echoed uri redacted rather than the text dropped",
        api.body
    );

    // an escaped occurrence straddling the cut is not sliced in half: each pad puts a
    // different byte of the escape on the boundary.
    for pad in [1006, 1010, 1014, 1018, 1020, 1022, 1023] {
        let got = redacted_for_error(&("x".repeat(pad) + r"alice\u0040example.test"), &[USER]);
        let lowered = go_to_lower(&got);
        for n in (3..=USER.len()).rev() {
            assert!(
                !lowered.contains(&USER[..n]),
                "pad {pad}: {:?} of the acting user survives: {got:?}",
                &USER[..n]
            );
        }
    }
}

/// A self-regenerating body of growing size, end to end (Go's 2 s CPU budget, real clock).
#[tokio::test]
async fn test_response_body_is_bounded_too() {
    for n in [4000, 40000, 200000] {
        let body = r"\".to_string() + &"u005c".repeat(n);
        let len = body.len();
        let srv = status_server(403, body).await;
        let c = new_test_client(&srv);
        let start = std::time::Instant::now();
        let got = c
            .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "alice@example.test"))
            .await;
        let elapsed = start.elapsed();
        drop(srv);
        assert!(
            got.is_err(),
            "RetrieveCredential() = nil error, want the 403"
        );
        assert!(
            elapsed <= Duration::from_secs(2),
            "a {len}-byte self-regenerating body took {elapsed:?}"
        );
    }
}

// parity: adapted: Go measures bytes allocated per call with testing.Benchmark, and counting allocations needs a #[global_allocator] with an `unsafe impl GlobalAlloc` (unsafe_code is forbidden); the test asserts what the window decides instead: a 1 MiB body past it is withheld unexamined, which fails both when the window is dropped and when it grows past the body
#[tokio::test]
async fn test_whole_text_check_reads_a_bounded_window() {
    let body = "ab".repeat(1 << 19); // 1 MiB, every byte a match for "a"
    let len = body.len();
    let srv = status_server(403, body).await;
    let c = new_test_client(&srv);
    let err = want_err(
        c.retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "a"))
            .await,
        "window",
    );
    let api = api_error(&err);
    assert_eq!(api.status_code, 403, "StatusCode");
    assert_eq!(
        api.body, WITHHELD_TEXT,
        "a {len}-byte body past the {MAX_STRADDLE_WINDOW}-byte window was examined rather than withheld"
    );
}

/// The truncation flag and the text come from one string: Go folds U+0130 to a one-byte "i",
/// so a flag taken from the lowered copy disagreed with the text. The ASCII row is the
/// control; each row asserts presence as well as absence.
#[tokio::test]
async fn test_truncation_flag_describes_the_text_returned() {
    const VICTIM: &str = "alice@example.test";
    for (name, padding, want_visible) in [
        (
            "a fold that shortens the lowered copy past the cap",
            "\u{130}".repeat(505),
            "i".repeat(100),
        ),
        (
            "the same byte length in ASCII",
            "x".repeat(1010),
            "x".repeat(100),
        ),
    ] {
        // The address one byte short, with its last byte escaped, so redact cannot match it
        // and only the decode can.
        let body = padding + &VICTIM[..VICTIM.len() - 1] + r"\u0074";
        let srv = status_server(403, body).await;
        let err = want_err(
            new_test_client(&srv)
                .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, VICTIM))
                .await,
            name,
        );
        let msg = err.to_string();
        for n in (5..=VICTIM.len()).rev() {
            assert!(
                !msg.contains(&VICTIM[..n]),
                "{name}: {n} of the address's {} bytes survive: {:?}",
                VICTIM.len(),
                &VICTIM[..n]
            );
        }
        assert!(
            msg.contains(&want_visible),
            "{name}: the service's own text did not survive the scrub: {msg}"
        );
    }
}

// parity: adapted: the decoder's wording is serde_json's ("invalid value: integer `…`, expected i64"), not encoding/json's "cannot unmarshal number" (contract note 7); the acting user is still absent and MalformedResponse still matches
#[tokio::test]
async fn test_decode_error_scrubs_the_acting_user() {
    // The connector's error code is an i64, so a numeric user id echoed there overflows it and
    // the literal lands in the decoder's message.
    const USER: &str = "10355512349999999999";
    let body = r#"{"done":true,"error":{"code":"#.to_string() + USER + r#","message":"x"}}"#;
    let srv = status_server(200, body).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request("projects/p/locations/l/connectors/c", USER))
            .await,
        "decode",
    );
    let msg = err.to_string();
    assert!(!msg.contains(USER), "error carries the acting user: {msg}");
    // Keyed on the secret, not a blanket drop.
    assert!(
        msg.contains("invalid value: integer"),
        "error lost the decoder's own wording: {msg}"
    );
    assert!(is_kind(&err, ErrorKind::MalformedResponse), "error = {msg}");
}

/// The sentinel that replaced the decoder error a caller could once match on.
#[tokio::test]
async fn test_malformed_response_is_matchable() {
    let body = r#"{"success":"#.to_string() + &"9".repeat(40) + "}";
    let srv = TestServer::sequence(&[body.as_str()]).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request("projects/p/locations/l/authProviders/a", "u"))
            .await,
        "malformed",
    );
    assert!(
        is_kind(&err, ErrorKind::MalformedResponse),
        "RetrieveCredential() error = {err}, want it to match MalformedResponse"
    );
    // The resource decoration must not break the match.
    assert!(
        err.to_string().contains("resource"),
        "error = {err}, want the resource named"
    );
}

/// A value spelled so its occurrence runs past the straddle window cannot be examined, so it
/// is withheld rather than shown in part. One character at nesting level k costs 2^(k-1)+5
/// bytes and takes k decode passes.
#[tokio::test]
async fn test_straddling_occurrence_is_withheld_not_shown_in_part() {
    let uri = "https://app.example.test/oauth/callback?state=".to_string() + &"k".repeat(154);
    let mut body = String::new();
    for (i, r) in uri.char_indices() {
        let depth = if i >= 170 { 12 } else { 1 };
        body.push_str(&"\\".repeat(1 << (depth - 1)));
        body.push_str(&format!("u{:04x}", u32::from(r)));
    }
    let srv = status_server(403, body).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(Request {
                resource: AUTH_PROVIDER_RESOURCE.into(),
                user_id: "alice@example.test".into(),
                continue_uri: uri.clone(),
                ..Default::default()
            })
            .await,
        "straddle",
    );
    let (got, _) = decode_fully(&err.to_string());
    for n in (5..=uri.len()).rev() {
        assert!(
            !got.contains(&uri[..n]),
            "{n} of the ContinueURI's {} characters are recoverable: {:?}",
            uri.len(),
            &uri[..n]
        );
    }
}

/// What fails when the window is too SMALL. The size repeats the window's defining expression
/// rather than naming MAX_STRADDLE_WINDOW, so redefining that constant moves the threshold
/// while the fixture stays put.
#[tokio::test]
async fn test_body_inside_the_window_is_still_shown() {
    let body =
        "denied: ".to_string() + &"z".repeat(MAX_ERROR_BODY + 8 * MAX_SCRUBBABLE_SECRET - 100);
    let len = body.len();
    let srv = status_server(403, body).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "alice@example.test"))
            .await,
        "inside the window",
    );
    assert!(
        err.to_string().contains("denied"),
        "a body of {len} bytes carrying no identifier was withheld: {err}"
    );
}

/// The size of what is returned, not of what was read: one marker per matched run, and the
/// SERVICE picks the number of runs. The source is deliberately UNDER the cap; both halves are
/// asserted.
#[tokio::test]
async fn test_markers_are_budgeted_against_the_cap() {
    let body = "u ".repeat(500); // 1000 bytes in, 5500 bytes of markers out
    assert!(
        body.len() <= MAX_ERROR_BODY,
        "fixture is {} bytes, want it under the {MAX_ERROR_BODY}-byte input cap so the output cap is what fires",
        body.len()
    );
    let srv = status_server(403, body).await;
    let err = want_err(
        new_test_client(&srv)
            .retrieve_credential(request(AUTH_PROVIDER_RESOURCE, "u"))
            .await,
        "markers",
    );
    let api = api_error(&err);
    // Presence first, stated as content: suppressing or emptying the body also satisfies a
    // ceiling.
    assert!(
        api.body.contains(REDACTED_MARKER),
        "Body carries no redacted run, want the body shown and cut rather than suppressed: {:?}",
        api.body
    );
    assert!(
        api.body.len() >= MAX_ERROR_BODY / 2,
        "Body = {} bytes, want at least {} — the cap should cut this body, not gut it",
        api.body.len(),
        MAX_ERROR_BODY / 2
    );
    assert!(
        api.body.len() <= MAX_ERROR_BODY + "...".len(),
        "Body = {} bytes, want it capped to {}",
        api.body.len(),
        MAX_ERROR_BODY + "...".len()
    );
}
