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

use crate::credential::{Bearer, Credential};
use crate::gcp::client::{
    Client, Config, DEFAULT_AGENT_IDENTITY_URL, DEFAULT_CONNECTOR_URL, DEFAULT_INITIAL_BACKOFF,
    DEFAULT_POLL_TIMEOUT, map_credential,
};
use crate::gcp::redact::{
    MAX_ERROR_BODY, MAX_SCRUBBABLE_SECRET, MAX_STRADDLE_WINDOW, REDACTED_MARKER, WITHHELD_TEXT,
    redact, redacted_for_error, truncate_for_error,
};
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
