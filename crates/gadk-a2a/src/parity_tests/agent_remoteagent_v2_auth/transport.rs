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

//! Ports of ADK-Go `agent/remoteagent/v2/auth_test.go`: the transport-level tests (mint
//! groups, HTTP clients, redirects, redaction, credential application, scope escaping; plan 5b
//! Task 4), plus Rust-only tests of the same units (after the ports).
//!
//! Go cancels a waiter through its `context.Context`; here a waiter is cancelled by dropping its
//! future (plan D3), which these ports do with `tokio::time::timeout`. Go's `mintTimeout`
//! variable is the group's `timeout` field (`MintGroup::new(what, timeout)`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use a2a_client::middleware::CallInterceptor as _;
use futures::future::BoxFuture;
use gadk_auth::{
    AuthContext, Bearer, Credential, CredentialFuture, CredentialProvider, OAuth2, ProviderFn,
    StaticTokenSource, Token, TokenSource,
};
use http::HeaderMap;
use http::header::{HeaderName, HeaderValue};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::watch;
use url::Url;

use crate::Error;
use crate::remoteagent::auth::{
    AUTH_SCOPE, AuthInterceptor, AuthSteps, CallIdentity, CredentialCell, CredentialScope,
    MintGroup, RetrieveError, apply_credential, auth_http_client, card_fetch_http_client,
    card_sends_in_clear, check_redirect, credential_scope, current_credential_scope,
    redact_token_error, same_credential_target, steps_for, with_auth_scope,
};

// ---------------------------------------------------------------- helpers

/// Go `errResolve`: the provider/source failure asserted on through the source chain.
#[derive(Debug)]
struct ResolveFailed;

impl fmt::Display for ResolveFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("resolve failed")
    }
}

impl StdError for ResolveFailed {}

/// Go `errors.As`: the first `E` in `err`'s source chain.
fn find_in_chain<'a, E: StdError + 'static>(err: &'a (dyn StdError + 'static)) -> Option<&'a E> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(found) = e.downcast_ref::<E>() {
            return Some(found);
        }
        cur = e.source();
    }
    None
}

/// Go `errors.Is(err, errResolve)`.
fn wraps_resolve_failed(err: &Error) -> bool {
    find_in_chain::<ResolveFailed>(err).is_some()
}

/// Go `errors.Is(err, context.DeadlineExceeded)`.
fn wraps_deadline_exceeded(err: &Error) -> bool {
    find_in_chain::<gadk_agent::AgentError>(err)
        .is_some_and(|e| matches!(e, gadk_agent::AgentError::DeadlineExceeded))
}

/// A one-shot latch (Go's `close(ch)` / `<-ch`).
#[derive(Clone)]
struct Gate(Arc<watch::Sender<bool>>);

impl Gate {
    fn new() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }

    fn open(&self) {
        self.0.send_replace(true);
    }

    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|open| *open).await;
    }
}

type TokenFn = dyn Fn() -> BoxFuture<'static, Result<Token, gadk_auth::Error>> + Send + Sync;

/// Go `tokenSourceFunc`.
struct FnTokenSource(Box<TokenFn>);

#[async_trait::async_trait]
impl TokenSource for FnTokenSource {
    async fn token(&self) -> Result<Token, gadk_auth::Error> {
        (self.0)().await
    }
}

fn token_source<F>(f: F) -> Arc<dyn TokenSource>
where
    F: Fn() -> BoxFuture<'static, Result<Token, gadk_auth::Error>> + Send + Sync + 'static,
{
    Arc::new(FnTokenSource(Box::new(f)))
}

fn bearer(access_token: &str) -> Token {
    Token {
        access_token: access_token.into(),
        token_type: String::new(),
    }
}

/// A source that returns `tok` at once.
fn static_source(tok: &str) -> Arc<dyn TokenSource> {
    Arc::new(StaticTokenSource(bearer(tok)))
}

/// A source that signals `entered` on every call, then blocks until `release` and returns
/// `result`.
fn held_source(
    entered: Gate,
    release: Gate,
    calls: Arc<AtomicUsize>,
    result: Result<Token, ()>,
) -> Arc<dyn TokenSource> {
    token_source(move || {
        let (entered, release, calls, result) = (
            entered.clone(),
            release.clone(),
            calls.clone(),
            result.clone(),
        );
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            entered.open();
            release.wait().await;
            result.map_err(|()| gadk_auth::Error::other(ResolveFailed))
        })
    })
}

fn oauth2(ts: Arc<dyn TokenSource>) -> Arc<dyn Credential> {
    Arc::new(OAuth2 { token_source: ts })
}

type ApplyStep = dyn Fn() -> BoxFuture<'static, Result<Vec<(&'static str, String)>, gadk_auth::Error>>
    + Send
    + Sync;

/// Go `applyFunc`: a credential whose `apply` runs a closure and writes the headers it returns.
struct ApplyFn(Box<ApplyStep>);

impl fmt::Debug for ApplyFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApplyFn")
    }
}

#[async_trait::async_trait]
impl Credential for ApplyFn {
    async fn apply(&self, headers: &mut HeaderMap) -> Result<(), gadk_auth::Error> {
        for (name, value) in (self.0)().await? {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(&value).unwrap(),
            );
        }
        Ok(())
    }
}

fn apply_fn<F>(f: F) -> Arc<dyn Credential>
where
    F: Fn() -> BoxFuture<'static, Result<Vec<(&'static str, String)>, gadk_auth::Error>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(ApplyFn(Box::new(f)))
}

fn bearer_credential(token: &str) -> Arc<dyn Credential> {
    Arc::new(Bearer {
        token: token.into(),
    })
}

fn provider<F>(f: F) -> Arc<dyn CredentialProvider>
where
    F: Fn(&AuthContext) -> CredentialFuture + Send + Sync + 'static,
{
    ProviderFn::new(f).into_provider()
}

fn call_identity(scope: CredentialScope, auth: Arc<dyn CredentialProvider>) -> CallIdentity {
    CallIdentity {
        scope,
        identity: None,
        auth,
        card_fetch_client: None,
        cell: Arc::new(CredentialCell::new()),
    }
}

fn header(h: &HeaderMap, name: &str) -> String {
    h.get(name)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

fn retrieve_error(status: &str, body: &str, code: &str, description: &str) -> RetrieveError {
    RetrieveError {
        status: Some(status.into()),
        body: body.as_bytes().to_vec(),
        error_code: code.into(),
        error_description: description.into(),
        error_uri: String::new(),
    }
}

/// A raw HTTP/1.1 test server: every connection gets one response from `respond(path)`
/// and is closed. Returns the base URL and the request heads it received.
async fn serve<F>(respond: F) -> (String, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str, &str) -> String + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let heads = Arc::new(Mutex::new(Vec::new()));
    let (respond, seen, me) = (Arc::new(respond), Arc::clone(&heads), base.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (respond, seen, me) = (Arc::clone(&respond), Arc::clone(&seen), me.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                seen.lock().push(head);
                let _ = sock.write_all(respond(&me, &path).as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (base, heads)
}

fn redirect_to(location: &str) -> String {
    format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

fn ok_response() -> String {
    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned()
}

/// The full source chain of `err`, joined with " / ".
fn chain_text(err: &(dyn StdError + 'static)) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(err);
    while let Some(e) = cur {
        parts.push(e.to_string());
        cur = e.source();
    }
    parts.join(" / ")
}

// ---------------------------------------------------------------- ported Go tests

/// Go `TestCredentialScope`.
#[test]
fn test_credential_scope() {
    let cases = [
        ("plain", ["app", "alice", "s1", "crm"], "app/alice/s1/crm"),
        (
            "separator in a part is escaped",
            ["a/b", "c", "d", "e"],
            "a%2Fb/c/d/e",
        ),
        (
            "the ambiguous twin does not collide",
            ["a", "b/c", "d", "e"],
            "a/b%2Fc/d/e",
        ),
        (
            "percent is escaped so escaping cannot be forged",
            ["a%2Fb", "c", "d", "e"],
            "a%252Fb/c/d/e",
        ),
        ("empty parts keep their positions", ["", "", "", ""], "///"),
    ];
    for (name, [app, user, session, agent], want) in cases {
        assert_eq!(
            credential_scope(app, user, session, agent),
            CredentialScope(want.into()),
            "{name}"
        );
    }
}

/// Go `TestMintAccessTokenHonorsContext`.
// parity: adapted: Go's expired caller context is the caller dropping its future; the drop must release the waiter while the source is still blocked
#[tokio::test]
async fn test_mint_access_token_honors_context() {
    let release = Gate::new();
    let blocked = held_source(
        Gate::new(),
        release.clone(),
        Arc::default(),
        Ok(bearer("late")),
    );
    let g = MintGroup::mints();
    let scope = CredentialScope("sid".into());
    let waited = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::time::timeout(Duration::from_millis(50), g.token(&scope, blocked)),
    )
    .await;
    match waited {
        Ok(Err(_elapsed)) => {}
        Ok(Ok(r)) => panic!("token() = {r:?}, want the caller's bound to release it"),
        Err(_) => panic!("token() did not return once the caller's bound expired"),
    }
    release.open();
}

/// Go `TestSameCredentialTarget`.
#[test]
fn test_same_credential_target() {
    let cases = [
        ("identical", "https://h/rpc", "https://h/other", true),
        (
            "default port made explicit",
            "https://h/rpc",
            "https://h:443/rpc",
            true,
        ),
        (
            "default http port made explicit",
            "http://h/rpc",
            "http://h:80/rpc",
            true,
        ),
        (
            "host case differs",
            "https://Host/rpc",
            "https://host/rpc",
            true,
        ),
        (
            "scheme upgraded to https, same explicit port",
            "http://h:8080/rpc",
            "https://h:8080/rpc",
            true,
        ),
        (
            "scheme upgraded to https, both ports implicit",
            "http://h/rpc",
            "https://h/rpc",
            true,
        ),
        (
            "scheme upgraded to https, explicit default ports",
            "http://h:80/rpc",
            "https://h:443/rpc",
            true,
        ),
        (
            "scheme upgraded to https onto a different port",
            "http://h/rpc",
            "https://h:9443/rpc",
            false,
        ),
        (
            "scheme downgraded with implicit ports",
            "https://h/rpc",
            "http://h/rpc",
            false,
        ),
        ("unrelated scheme", "https://h/rpc", "ftp://h/rpc", false),
        (
            "http to an unrelated scheme",
            "http://h/rpc",
            "ftp://h/rpc",
            false,
        ),
        (
            "an unrelated scheme to https",
            "ftp://h/rpc",
            "https://h/rpc",
            false,
        ),
        (
            "scheme upgraded to https, explicit http default to implicit",
            "http://h:80/rpc",
            "https://h/rpc",
            true,
        ),
        (
            "scheme downgraded to http",
            "https://h/rpc",
            "http://h/rpc",
            false,
        ),
        (
            "different host",
            "https://h/rpc",
            "https://other/rpc",
            false,
        ),
        (
            "different port",
            "https://h:8443/rpc",
            "https://h:9443/rpc",
            false,
        ),
    ];
    for (name, from, to, want) in cases {
        let (from_u, to_u) = (Url::parse(from).unwrap(), Url::parse(to).unwrap());
        assert_eq!(
            same_credential_target(&from_u, &to_u),
            want,
            "{name}: same_credential_target({from:?}, {to:?})"
        );
    }
}

/// Go `TestAuthHTTPClientRedirectCap`.
#[tokio::test]
async fn test_auth_http_client_redirect_cap() {
    let (base, heads) = serve(|_, path| redirect_to(path)).await;
    let err = auth_http_client()
        .get(format!("{base}/loop"))
        .send()
        .await
        .expect_err("want the redirect cap to stop the loop");
    let text = chain_text(&err);
    assert!(
        text.contains("remoteagent: stopped after 10 redirects"),
        "error = {text}"
    );
    // The literal, not the constant: 10 is net/http's own default.
    assert_eq!(heads.lock().len(), 10);
}

/// Go `TestAuthHTTPClientTimeout`.
// parity: adapted: reqwest::Client exposes no timeout getter; with tokio's paused clock a request to a listener that never answers fails after exactly 3 minutes
#[tokio::test(start_paused = true)]
async fn test_auth_http_client_timeout() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", silent.local_addr().unwrap());
    let start = tokio::time::Instant::now();
    let err = auth_http_client().get(url).send().await.unwrap_err();
    assert!(err.is_timeout(), "error = {err:?}");
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(180) && waited < Duration::from_secs(181),
        "the request timed out after {waited:?}, want 3m0s"
    );
}

/// Go `TestCheckRedirect`.
#[test]
fn test_check_redirect() {
    let cases: [(&str, &[&str], &str); 6] = [
        ("same origin", &["https://h/a", "https://h/b"], ""),
        ("upgrade to https", &["http://h/a", "https://h/a"], ""),
        (
            "two same-origin hops",
            &["https://h/a", "https://h/b", "https://h/c"],
            "",
        ),
        (
            "cross host",
            &["https://h/a", "https://other/a"],
            "refusing redirect",
        ),
        (
            "downgrade",
            &["https://h/a", "http://h/a"],
            "refusing redirect",
        ),
        (
            // Each hop is same-origin against the original http request, so only the
            // previous-hop check rejects the last one.
            "upgrade then downgrade",
            &["http://h/a", "https://h/b", "http://h/c"],
            "refusing redirect",
        ),
    ];
    for (name, urls, want_err) in cases {
        let urls: Vec<Url> = urls.iter().map(|u| Url::parse(u).unwrap()).collect();
        let (next, via) = urls.split_last().unwrap();
        let got = check_redirect(next, via);
        if want_err.is_empty() {
            assert!(got.is_ok(), "{name}: error = {got:?}, want none");
        } else {
            let err = got.expect_err(name).to_string();
            assert!(err.contains(want_err), "{name}: error = {err}");
        }
    }
    // The refusal names both origins, in Go's words.
    let err = check_redirect(
        &Url::parse("https://other:8443/a").unwrap(),
        &[Url::parse("https://h/a").unwrap()],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "remoteagent: refusing redirect from https://h to https://other:8443: A2AConfig.Auth is set and the credential would follow"
    );
}

/// Go `TestRedactTokenError`.
#[tokio::test]
async fn test_redact_token_error() {
    const BODY: &str = "sensitive-echo-of-the-request";
    struct Case {
        name: &'static str,
        error_code: &'static str,
        description: &'static str,
        uri: &'static str,
        want_keep: &'static [&'static str],
        want_gone: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "malformed error response carries the body",
            error_code: "",
            description: "",
            uri: "",
            want_keep: &["400 Bad Request"],
            want_gone: &[],
        },
        Case {
            name: "well-formed error response names a code",
            error_code: "invalid_grant",
            description: "assertion=eyJhbGciOi-SECRET",
            uri: "https://idp.invalid/errors/1",
            want_keep: &["invalid_grant"],
            want_gone: &[
                "assertion=eyJhbGciOi-SECRET",
                "https://idp.invalid/errors/1",
            ],
        },
    ];
    for tc in cases {
        let re = RetrieveError {
            error_uri: tc.uri.into(),
            ..retrieve_error("400 Bad Request", BODY, tc.error_code, tc.description)
        };
        let src = token_source(move || {
            let re = re.clone();
            Box::pin(async move { Err(gadk_auth::Error::other(re)) })
        });
        let err = MintGroup::mints()
            .token(&CredentialScope("sid".into()), src)
            .await
            .expect_err(tc.name);
        let text = err.to_string();
        assert!(!text.contains(BODY), "{}: error = {text}", tc.name);
        for want in tc.want_keep {
            assert!(
                text.contains(want),
                "{}: error = {text}, want {want:?}",
                tc.name
            );
        }
        for gone in tc.want_gone {
            assert!(
                !text.contains(gone),
                "{}: error = {text}, want {gone:?} dropped",
                tc.name
            );
        }
        assert!(
            find_in_chain::<RetrieveError>(&err).is_some(),
            "{}: the chain lost the RetrieveError",
            tc.name
        );
    }
}

/// Go `TestMintGroupRejectsANilToken`.
// parity: adapted: a Rust token source cannot return a nil token; its nearest shape, success with an empty token, must be rejected rather than reported as a panic
#[tokio::test]
async fn test_mint_group_rejects_a_nil_token() {
    let src = token_source(|| Box::pin(async { Ok(Token::default()) }));
    let err = MintGroup::mints()
        .token(&CredentialScope("sid".into()), src)
        .await
        .expect_err("want an error for an empty token");
    assert_eq!(
        err.to_string(),
        "remoteagent: oauth2 token source returned an empty access token"
    );
    assert!(!err.to_string().contains("panicked"));
}

/// Go `TestRedactTokenErrorLeavesOtherErrorsAlone`.
#[test]
fn test_redact_token_error_leaves_other_errors_alone() {
    let cases = [
        (
            "not a retrieve error",
            gadk_auth::Error::other(ResolveFailed),
        ),
        (
            "no response",
            gadk_auth::Error::other(RetrieveError {
                body: b"body".to_vec(),
                ..RetrieveError::default()
            }),
        ),
        (
            "no body",
            gadk_auth::Error::other(RetrieveError {
                status: Some("400 Bad Request".into()),
                ..RetrieveError::default()
            }),
        ),
    ];
    for (name, err) in cases {
        let before = err.to_string();
        let got = redact_token_error(err);
        assert_eq!(got.to_string(), before, "{name}: the error changed");
        assert!(
            find_in_chain::<ResolveFailed>(&got).is_some()
                || find_in_chain::<RetrieveError>(&got).is_some(),
            "{name}: the original error is gone"
        );
    }
}

/// Go `wrappedRetrieveError`: a wrapper that prints the body itself.
#[derive(Debug)]
struct WrappedRetrieveError(RetrieveError);

impl fmt::Display for WrappedRetrieveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "auth: cannot fetch token: 400\nResponse: {}",
            String::from_utf8_lossy(&self.0.body)
        )
    }
}

impl StdError for WrappedRetrieveError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.0)
    }
}

/// Go `quotingRetrieveError`: a wrapper that re-encodes the body.
#[derive(Debug)]
struct QuotingRetrieveError(RetrieveError);

impl fmt::Display for QuotingRetrieveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "auth: cannot fetch token: {}",
            gadk_platform::go_quote(&String::from_utf8_lossy(&self.0.body))
        )
    }
}

impl StdError for QuotingRetrieveError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.0)
    }
}

/// Go `TestRedactTokenErrorSeesThroughAReencodingWrapper`.
#[test]
fn test_redact_token_error_sees_through_a_reencoding_wrapper() {
    let body = "line-one\nassertion=eyJhbGciOi-SECRET";
    let err = gadk_auth::Error::other(QuotingRetrieveError(retrieve_error(
        "400 Bad Request",
        body,
        "",
        "",
    )));
    let got = redact_token_error(err).to_string();
    assert!(!got.contains("eyJhbGciOi-SECRET"), "redacted = {got:?}");
}

/// Go `TestRedactTokenErrorSeesThroughAWrapper`.
#[test]
fn test_redact_token_error_sees_through_a_wrapper() {
    let body = r#"{"error":"invalid_grant","error_description":"assertion=eyJhbGciOi-SECRET"}"#;
    let make = || {
        gadk_auth::Error::other(WrappedRetrieveError(retrieve_error(
            "400 Bad Request",
            body,
            "invalid_grant",
            "assertion=eyJhbGciOi-SECRET",
        )))
    };
    let got = redact_token_error(make()).to_string();
    assert!(!got.contains(body), "redacted = {got:?}");
    assert!(!got.contains("eyJhbGciOi-SECRET"), "redacted = {got:?}");
    assert!(
        got.contains("400 Bad Request") && got.contains("invalid_grant"),
        "redacted = {got:?}"
    );
    assert_eq!(
        got,
        r#"oauth2: cannot fetch token: 400 Bad Request (response body redacted): "invalid_grant""#
    );
    assert!(find_in_chain::<RetrieveError>(&redact_token_error(make())).is_some());
}

/// Go `TestMintAccessTokenHasItsOwnBudget`.
#[tokio::test]
async fn test_mint_access_token_has_its_own_budget() {
    let release = Gate::new();
    let blocked = held_source(
        Gate::new(),
        release.clone(),
        Arc::default(),
        Ok(bearer("late")),
    );
    let g = MintGroup::new("mint oauth2 token", Duration::from_millis(50));
    let waited = tokio::time::timeout(
        Duration::from_secs(10),
        g.token(&CredentialScope("sid".into()), blocked),
    )
    .await
    .expect("token() did not return; a caller without a bound left the mint unbounded");
    let err = waited.unwrap_err();
    assert!(wraps_deadline_exceeded(&err), "error = {err}");
    assert_eq!(
        err.to_string(),
        "remoteagent: mint oauth2 token: context deadline exceeded"
    );
    release.open();
}

/// Go `TestMintGroupSingleFlight`.
// parity: adapted: the three cancelled joiners are futures dropped at once (a zero timeout polls them once, after they joined)
#[tokio::test]
async fn test_mint_group_single_flight() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered, release) = (Gate::new(), Gate::new());
    let src = held_source(
        entered.clone(),
        release.clone(),
        Arc::clone(&calls),
        Ok(bearer("tok")),
    );
    let g = Arc::new(MintGroup::mints());
    let scope = CredentialScope("scope-a".into());

    let first = tokio::spawn({
        let (g, scope, src) = (Arc::clone(&g), scope.clone(), Arc::clone(&src));
        async move { g.token(&scope, src).await }
    });
    entered.wait().await;

    let in_flight = g
        .in_flight(&scope)
        .expect("no mint recorded as in flight while the token source is blocked");

    for _ in 0..3 {
        let joined = tokio::time::timeout(Duration::ZERO, g.token(&scope, Arc::clone(&src))).await;
        assert!(
            joined.is_err(),
            "a joined call returned {joined:?} instead of waiting"
        );
    }

    let still = g.in_flight(&scope).unwrap();
    assert!(
        Arc::ptr_eq(&still, &in_flight),
        "a second request for the same scope replaced the in-flight mint instead of joining it"
    );
    assert_eq!(g.in_flight_len(), 1);

    release.open();
    assert_eq!(first.await.unwrap().unwrap(), "tok");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "token source called more than once for 4 requests on one scope"
    );
}

/// Go `TestMintGroupSeparatesScopes`.
#[tokio::test]
async fn test_mint_group_separates_scopes() {
    let scopes = [
        credential_scope("shop", "alice", "s1", "crm"),
        credential_scope("shop", "bob", "s1", "crm"),
        credential_scope("shop", "alice", "s2", "crm"),
        credential_scope("shop", "alice", "s1", "billing"),
    ];
    let g = Arc::new(MintGroup::mints());
    let (entered, release) = (Gate::new(), Gate::new());
    let first = held_source(
        entered.clone(),
        release.clone(),
        Arc::default(),
        Ok(bearer("tok-0")),
    );
    let first_done = tokio::spawn({
        let (g, scope) = (Arc::clone(&g), scopes[0].clone());
        async move { g.token(&scope, first).await }
    });
    entered.wait().await;

    // While the first mint is in flight, every other identity must get its own token. One
    // that joined the first mint would block on release, so a bounded wait tells them apart.
    for (i, scope) in scopes[1..].iter().enumerate() {
        let want = format!("tok-{}", i + 1);
        let got =
            tokio::time::timeout(Duration::from_secs(5), g.token(scope, static_source(&want)))
                .await
                .unwrap_or_else(|_| {
                    panic!("{scope:?} joined the in-flight mint for {:?}", scopes[0])
                });
        assert_eq!(got.unwrap(), want, "{scope:?}");
    }
    release.open();
    assert_eq!(first_done.await.unwrap().unwrap(), "tok-0");
}

/// Go `TestMintGroupRetriesAfterFailure`.
#[tokio::test]
async fn test_mint_group_retries_after_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let src = token_source(move || {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if n == 1 {
                Err(gadk_auth::Error::other(ResolveFailed))
            } else {
                Ok(bearer("tok"))
            }
        })
    });
    let g = MintGroup::mints();
    let scope = CredentialScope("scope-a".into());
    let err = g.token(&scope, Arc::clone(&src)).await.unwrap_err();
    assert!(wraps_resolve_failed(&err), "error = {err}");
    assert_eq!(
        err.to_string(),
        "remoteagent: mint oauth2 token: resolve failed"
    );
    assert_eq!(g.token(&scope, src).await.unwrap(), "tok");
}

/// Go `TestMintGroupRecoversPanic`.
#[tokio::test]
async fn test_mint_group_recovers_panic() {
    let src = token_source(|| Box::pin(async { panic!("token source exploded") }));
    let g = MintGroup::mints();
    let scope = CredentialScope("scope-a".into());
    let err = g.token(&scope, src).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "remoteagent: mint oauth2 token: credential panicked: token source exploded"
    );
    // Same group and same scope: a fresh one could not observe a wedge.
    assert_eq!(g.token(&scope, static_source("tok")).await.unwrap(), "tok");
}

/// Go `TestMintGroupRetiresAnOverdueAttempt`.
#[tokio::test]
async fn test_mint_group_retires_an_overdue_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (first, release) = (Gate::new(), Gate::new());
    let hung = held_source(first.clone(), release.clone(), Arc::clone(&calls), Err(()));
    let g = Arc::new(MintGroup::new(
        "mint oauth2 token",
        Duration::from_millis(30),
    ));
    let scope = CredentialScope("scope-a".into());

    let done = tokio::spawn({
        let (g, scope) = (Arc::clone(&g), scope.clone());
        async move { g.token(&scope, hung).await }
    });
    first.wait().await;
    let err = done.await.unwrap().unwrap_err();
    assert!(wraps_deadline_exceeded(&err), "error = {err}");

    // The hung mint still holds its task, but its budget is spent, so the next caller must
    // mint afresh rather than wait on it again.
    let got = g.token(&scope, static_source("recovered")).await;
    assert_eq!(got.unwrap(), "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.open();
}

/// Go `TestMintGroupJoinerWaitsTheAttemptsRemainder`.
#[tokio::test]
async fn test_mint_group_joiner_waits_the_attempts_remainder() {
    let timeout = Duration::from_secs(2);
    let (first, release) = (Gate::new(), Gate::new());
    let hung = held_source(first.clone(), release.clone(), Arc::default(), Err(()));
    let g = Arc::new(MintGroup::new("mint oauth2 token", timeout));
    let scope = CredentialScope("scope-a".into());

    tokio::spawn({
        let (g, scope, hung) = (Arc::clone(&g), scope.clone(), Arc::clone(&hung));
        async move { g.token(&scope, hung).await }
    });
    first.wait().await;

    // Join halfway through the attempt.
    tokio::time::sleep(timeout / 2).await;
    let joinable = g
        .in_flight(&scope)
        .is_some_and(|call| tokio::time::Instant::now() < call.deadline());
    if !joinable {
        eprintln!(
            "the first attempt expired before this task could join it; the machine is too loaded to time this"
        );
        release.open();
        return;
    }

    let start = tokio::time::Instant::now();
    let err = g.token(&scope, hung).await.unwrap_err();
    assert!(wraps_deadline_exceeded(&err), "error = {err}");
    // The remainder is about timeout/2; a fresh bound would be the whole timeout.
    let waited = start.elapsed();
    assert!(
        waited <= timeout * 3 / 4,
        "a joiner waited {waited:?}, want at most the attempt's remainder rather than a fresh {timeout:?}"
    );
    release.open();
}

/// Go `TestMintGroupRetiredAttemptDoesNotEvictItsSuccessor`.
#[tokio::test]
async fn test_mint_group_retired_attempt_does_not_evict_its_successor() {
    let (entered_a, release_a) = (Gate::new(), Gate::new());
    let (entered_b, release_b) = (Gate::new(), Gate::new());
    let src_a = held_source(
        entered_a.clone(),
        release_a.clone(),
        Arc::default(),
        Ok(bearer("a")),
    );
    let src_b = held_source(
        entered_b.clone(),
        release_b.clone(),
        Arc::default(),
        Ok(bearer("b")),
    );
    let g = Arc::new(MintGroup::new(
        "mint oauth2 token",
        Duration::from_millis(40),
    ));
    let scope = CredentialScope("scope-a".into());

    let done = tokio::spawn({
        let (g, scope) = (Arc::clone(&g), scope.clone());
        async move { g.token(&scope, src_a).await }
    });
    entered_a.wait().await;
    let call_a = g.in_flight(&scope).unwrap();
    let _ = done.await; // the attempt's budget is now spent

    // A second caller retires the overdue attempt and starts its own.
    tokio::spawn({
        let (g, scope) = (Arc::clone(&g), scope.clone());
        async move { g.token(&scope, src_b).await }
    });
    entered_b.wait().await;
    let call_b = g
        .in_flight(&scope)
        .expect("the overdue attempt was not replaced");
    assert!(
        !Arc::ptr_eq(&call_b, &call_a),
        "the overdue attempt was not replaced"
    );

    // Now let the abandoned first mint finish. Its entry is gone, so it must leave the
    // successor's alone. The outcome is published inside the same locked section as the
    // removal, so waiting for it orders this check after whatever that task did to the map.
    release_a.open();
    call_a.done().await;

    let still = g.in_flight(&scope).unwrap();
    assert!(
        Arc::ptr_eq(&still, &call_b),
        "the retired attempt evicted its successor"
    );
    release_b.open();
}

/// Go `TestAuthTransportUnscopedMintsDoNotShare`.
#[tokio::test]
async fn test_auth_transport_unscoped_mints_do_not_share() {
    let (entered, release) = (Gate::new(), Gate::new());
    let first = held_source(
        entered.clone(),
        release.clone(),
        Arc::default(),
        Ok(bearer("first")),
    );
    let steps = Arc::new(AuthSteps::new());
    tokio::spawn({
        let steps = Arc::clone(&steps);
        async move {
            steps
                .apply(None, &oauth2(first), &mut HeaderMap::new())
                .await
        }
    });
    entered.wait().await;

    let mut h = HeaderMap::new();
    let applied = tokio::time::timeout(
        Duration::from_secs(5),
        steps.apply(None, &oauth2(static_source("second")), &mut h),
    )
    .await
    .expect("an unscoped request joined another unscoped request's mint");
    applied.unwrap();
    assert_eq!(header(&h, "authorization"), "Bearer second");
    release.open();
}

/// Go `TestAuthTransportUnscopedAppliesDoNotShare`.
#[tokio::test]
async fn test_auth_transport_unscoped_applies_do_not_share() {
    let (entered, release) = (Gate::new(), Gate::new());
    let slow = {
        let (entered, release) = (entered.clone(), release.clone());
        apply_fn(move || {
            let (entered, release) = (entered.clone(), release.clone());
            Box::pin(async move {
                entered.open();
                release.wait().await;
                Ok(vec![("authorization", "Bearer slow".to_owned())])
            })
        })
    };
    let steps = Arc::new(AuthSteps::new());
    tokio::spawn({
        let steps = Arc::clone(&steps);
        async move { steps.apply(None, &slow, &mut HeaderMap::new()).await }
    });
    entered.wait().await;

    let mut h = HeaderMap::new();
    let applied = tokio::time::timeout(
        Duration::from_secs(5),
        steps.apply(None, &bearer_credential("fast"), &mut h),
    )
    .await
    .expect("an unscoped request joined another unscoped request's apply");
    applied.unwrap();
    assert_eq!(header(&h, "authorization"), "Bearer fast");
    release.open();
}

/// Go `TestAuthTransportBoundsAPointerOAuth2Mint`.
// parity: adapted: Rust has one OAuth2 credential form (no pointer twin); the bare OAuth2 credential is minted through the bounded group
#[tokio::test]
async fn test_auth_transport_bounds_a_pointer_o_auth2_mint() {
    let release = Gate::new();
    let hung = held_source(
        Gate::new(),
        release.clone(),
        Arc::default(),
        Ok(bearer("late")),
    );
    let steps = AuthSteps {
        mints: MintGroup::new("mint oauth2 token", Duration::from_millis(50)),
        applies: MintGroup::applies(),
    };
    let scope = CredentialScope("sid".into());
    let applied = tokio::time::timeout(
        Duration::from_secs(5),
        steps.apply(Some(&scope), &oauth2(hung), &mut HeaderMap::new()),
    )
    .await
    .expect("apply() did not return; an OAuth2 credential bypassed the mint bound");
    let err = applied.unwrap_err();
    assert!(wraps_deadline_exceeded(&err), "error = {err}");
    release.open();
}

/// Go `TestAuthTransportBoundsAWrappedOAuth2Credential`.
#[tokio::test]
async fn test_auth_transport_bounds_a_wrapped_o_auth2_credential() {
    let bounded = || AuthSteps {
        mints: MintGroup::new("mint oauth2 token", Duration::from_millis(50)),
        applies: MintGroup::new("apply auth credential", Duration::from_millis(50)),
    };
    let extra = || BTreeMap::from([("X-Goog-User-Project".to_owned(), "p".to_owned())]);
    let scope = CredentialScope("sid".into());

    // a hung token endpoint is bounded
    let release = Gate::new();
    let hung = held_source(
        Gate::new(),
        release.clone(),
        Arc::default(),
        Ok(bearer("late")),
    );
    let cred = gadk_auth::with_headers(oauth2(hung), extra());
    let applied = tokio::time::timeout(
        Duration::from_secs(5),
        bounded().apply(Some(&scope), &cred, &mut HeaderMap::new()),
    )
    .await
    .expect("apply() did not return; a wrapped OAuth2 credential bypassed the bound");
    let err = applied.unwrap_err();
    assert!(wraps_deadline_exceeded(&err), "error = {err}");
    assert_eq!(
        err.to_string(),
        "remoteagent: apply auth credential: context deadline exceeded"
    );
    release.open();

    // the response body is redacted
    let body = r#"{"error":"invalid_grant","error_description":"assertion=eyJhbGciOi-SECRET"}"#;
    let failing = token_source(move || {
        let re = retrieve_error(
            "400 Bad Request",
            body,
            "invalid_grant",
            "assertion=eyJhbGciOi-SECRET",
        );
        Box::pin(async move { Err(gadk_auth::Error::other(re)) })
    });
    let cred = gadk_auth::with_headers(oauth2(failing), extra());
    let err = bounded()
        .apply(Some(&scope), &cred, &mut HeaderMap::new())
        .await
        .expect_err("want the token endpoint's refusal");
    let text = err.to_string();
    assert!(!text.contains("eyJhbGciOi-SECRET"), "error = {text}");
    assert!(text.contains("invalid_grant"), "error = {text}");

    // the result is written only once it has landed
    let cred = gadk_auth::with_headers(bearer_credential("tok"), extra());
    let mut h = HeaderMap::new();
    bounded().apply(Some(&scope), &cred, &mut h).await.unwrap();
    assert_eq!(header(&h, "authorization"), "Bearer tok");
    assert_eq!(header(&h, "x-goog-user-project"), "p");
}

/// Go `TestAuthTransportKeysEachStepOnTheRequestScope`.
#[tokio::test]
async fn test_auth_transport_keys_each_step_on_the_request_scope() {
    let alice = credential_scope("shop", "alice", "s1", "crm");
    let bob = credential_scope("shop", "bob", "s1", "crm");

    type Make = fn(Gate, Gate) -> Arc<dyn Credential>;
    let cases: [(&str, Make, Make); 2] = [
        (
            "oauth2 mint",
            |entered, release| {
                oauth2(held_source(
                    entered,
                    release,
                    Arc::default(),
                    Ok(bearer("alice")),
                ))
            },
            |_, _| oauth2(static_source("bob")),
        ),
        (
            "credential apply",
            |entered, release| {
                apply_fn(move || {
                    let (entered, release) = (entered.clone(), release.clone());
                    Box::pin(async move {
                        entered.open();
                        release.wait().await;
                        Ok(vec![("authorization", "Bearer alice".to_owned())])
                    })
                })
            },
            |_, _| bearer_credential("bob"),
        ),
    ];
    for (name, held, fast) in cases {
        let (entered, release) = (Gate::new(), Gate::new());
        let steps = Arc::new(AuthSteps::new());
        tokio::spawn({
            let (steps, alice, cred) = (
                Arc::clone(&steps),
                alice.clone(),
                held(entered.clone(), release.clone()),
            );
            async move {
                steps
                    .apply(Some(&alice), &cred, &mut HeaderMap::new())
                    .await
            }
        });
        entered.wait().await;

        let mut h = HeaderMap::new();
        let applied = tokio::time::timeout(
            Duration::from_secs(5),
            steps.apply(Some(&bob), &fast(Gate::new(), Gate::new()), &mut h),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: bob's request joined alice's in-flight step"));
        applied.unwrap();
        assert_eq!(header(&h, "authorization"), "Bearer bob", "{name}");
        release.open();
    }
}

/// Go `TestAuthTransportClosesTheBodyOnFailure`.
// parity: adapted: Rust owns the request body, so nothing can leak; the port pins the Go contract that remains: a credential that fails to resolve or apply fails the call before anything is sent
#[tokio::test]
async fn test_auth_transport_closes_the_body_on_failure() {
    let fail_resolve =
        provider(|_| Box::pin(async { Err(gadk_auth::Error::other(ResolveFailed)) }));
    let fail_apply = provider(|_| {
        Box::pin(async {
            Ok(apply_fn(|| {
                Box::pin(async { Err(gadk_auth::Error::other(ResolveFailed)) })
            }))
        })
    });
    for (name, auth, want) in [
        (
            "resolve fails",
            fail_resolve,
            "remoteagent: resolve auth credential: resolve failed",
        ),
        (
            "apply fails",
            fail_apply,
            "remoteagent: apply auth credential: resolve failed",
        ),
    ] {
        let id = call_identity(credential_scope("shop", "alice", "s1", "crm"), auth);
        let mut params = a2a_client::ServiceParams::new();
        let err = with_auth_scope(
            Some(id.clone()),
            AuthInterceptor.before("SendMessage", &mut params),
        )
        .await
        .expect_err(name);
        assert_eq!(err.message, want, "{name}");
        assert!(params.is_empty(), "{name}: params = {params:?}");

        let mut headers = HeaderMap::new();
        let err = with_auth_scope(Some(id), apply_credential(&mut headers))
            .await
            .expect_err(name);
        assert!(wraps_resolve_failed(&err), "{name}: error = {err}");
        assert!(headers.is_empty(), "{name}");
    }
}

/// Go `TestAuthTransportSharesAStepWithinAScope`.
// parity: adapted: the second, cancelled request is a future dropped at once (a zero timeout polls it once, after it joined)
#[tokio::test]
async fn test_auth_transport_shares_a_step_within_a_scope() {
    let scope = credential_scope("shop", "alice", "s1", "crm");
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered, release) = (Gate::new(), Gate::new());
    let cred = {
        let (calls, entered, release) = (Arc::clone(&calls), entered.clone(), release.clone());
        apply_fn(move || {
            let (calls, entered, release) = (Arc::clone(&calls), entered.clone(), release.clone());
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.open();
                release.wait().await;
                Ok(vec![("authorization", "Bearer alice".to_owned())])
            })
        })
    };
    let steps = Arc::new(AuthSteps::new());
    let first = tokio::spawn({
        let (steps, scope, cred) = (Arc::clone(&steps), scope.clone(), Arc::clone(&cred));
        async move {
            let mut h = HeaderMap::new();
            steps.apply(Some(&scope), &cred, &mut h).await.map(|()| h)
        }
    });
    entered.wait().await;
    let held = steps
        .applies
        .in_flight(&scope)
        .expect("no step recorded in flight for the scope while its apply is running");

    let mut h = HeaderMap::new();
    let joined =
        tokio::time::timeout(Duration::ZERO, steps.apply(Some(&scope), &cred, &mut h)).await;
    assert!(joined.is_err(), "the joined request returned {joined:?}");
    let still = steps.applies.in_flight(&scope).unwrap();
    assert!(Arc::ptr_eq(&still, &held) && steps.applies.in_flight_len() == 1);

    release.open();
    let h = first.await.unwrap().unwrap();
    assert_eq!(header(&h, "authorization"), "Bearer alice");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "apply ran more than once for two overlapping requests on one scope"
    );
}

/// Go `TestAuthTransportSharesAMintWithinAScope`.
// parity: adapted: the second, cancelled request is a future dropped at once (a zero timeout polls it once, after it joined)
#[tokio::test]
async fn test_auth_transport_shares_a_mint_within_a_scope() {
    let scope = credential_scope("shop", "alice", "s1", "crm");
    let (entered, release) = (Gate::new(), Gate::new());
    let cred = oauth2(held_source(
        entered.clone(),
        release.clone(),
        Arc::default(),
        Ok(bearer("alice")),
    ));
    let steps = Arc::new(AuthSteps::new());
    let first = tokio::spawn({
        let (steps, scope, cred) = (Arc::clone(&steps), scope.clone(), Arc::clone(&cred));
        async move {
            steps
                .apply(Some(&scope), &cred, &mut HeaderMap::new())
                .await
        }
    });
    entered.wait().await;
    let held = steps
        .mints
        .in_flight(&scope)
        .expect("no mint recorded in flight for the scope while it runs");

    let joined = tokio::time::timeout(
        Duration::ZERO,
        steps.apply(Some(&scope), &cred, &mut HeaderMap::new()),
    )
    .await;
    assert!(joined.is_err(), "the joined request returned {joined:?}");
    let still = steps.mints.in_flight(&scope).unwrap();
    assert!(Arc::ptr_eq(&still, &held) && steps.mints.in_flight_len() == 1);

    release.open();
    first.await.unwrap().unwrap();
}

// ---------------------------------------------------------------- Rust-only tests

/// The interceptor writes the scope's credential into the call's service params, replacing a
/// value under the same name in any letter case, and does nothing outside a scope.
#[tokio::test]
async fn interceptor_writes_the_credential_into_the_service_params() {
    let mut params = a2a_client::ServiceParams::new();
    AuthInterceptor
        .before("SendMessage", &mut params)
        .await
        .unwrap();
    assert!(params.is_empty(), "unscoped params = {params:?}");

    let id = call_identity(
        credential_scope("shop", "alice", "s1", "crm"),
        gadk_auth::static_token("secret"),
    );
    params.insert("Authorization".into(), vec!["Bearer stale".into()]);
    params.insert("A2A-Version".into(), vec!["1.0".into()]);
    with_auth_scope(Some(id), AuthInterceptor.before("SendMessage", &mut params))
        .await
        .unwrap();
    assert_eq!(
        params.get("authorization"),
        Some(&vec!["Bearer secret".to_owned()])
    );
    assert!(!params.contains_key("Authorization"));
    assert_eq!(params.get("A2A-Version"), Some(&vec!["1.0".to_owned()]));
}

/// The provider sees the scope's identity; a caching cell resolves once per invocation, an
/// uncached cell (the server's cleanup cancel) on every call.
#[tokio::test]
async fn the_credential_cell_resolves_once_per_invocation() {
    let resolved = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let auth = {
        let (resolved, seen) = (Arc::clone(&resolved), Arc::clone(&seen));
        provider(move |cx| {
            resolved.fetch_add(1, Ordering::SeqCst);
            seen.lock()
                .push(cx.identity.as_ref().map(|i| i.user_id.clone()));
            Box::pin(async { Ok(bearer_credential("tok")) })
        })
    };
    let identity = gadk_agent::Identity {
        user_id: "alice".into(),
        app_name: "shop".into(),
        session_id: "s1".into(),
    };
    let id = CallIdentity {
        identity: Some(identity),
        ..call_identity(
            credential_scope("shop", "alice", "s1", "crm"),
            Arc::clone(&auth),
        )
    };
    for _ in 0..2 {
        let mut h = HeaderMap::new();
        with_auth_scope(Some(id.clone()), apply_credential(&mut h))
            .await
            .unwrap();
        assert_eq!(header(&h, "authorization"), "Bearer tok");
    }
    assert_eq!(resolved.load(Ordering::SeqCst), 1);
    assert_eq!(*seen.lock(), vec![Some("alice".to_owned())]);

    let uncached = CallIdentity {
        cell: Arc::new(CredentialCell::uncached()),
        ..call_identity(credential_scope("shop", "alice", "s1", "crm"), auth)
    };
    for _ in 0..2 {
        with_auth_scope(
            Some(uncached.clone()),
            apply_credential(&mut HeaderMap::new()),
        )
        .await
        .unwrap();
    }
    assert_eq!(resolved.load(Ordering::SeqCst), 3);
    assert_eq!(seen.lock()[1..], [None, None]);
}

/// One provider value has one set of single-flight groups; another provider has its own.
#[test]
fn steps_are_per_provider_value() {
    let a = gadk_auth::static_token("a");
    let b = gadk_auth::static_token("a");
    assert!(Arc::ptr_eq(&steps_for(&a), &steps_for(&a)));
    assert!(!Arc::ptr_eq(&steps_for(&a), &steps_for(&b)));
}

/// `current_credential_scope` is the scope inside a scoped call and `None` outside one.
#[tokio::test]
async fn current_credential_scope_reads_the_task_local() {
    assert_eq!(current_credential_scope(), None);
    let scope = credential_scope("shop", "alice", "s1", "crm");
    let id = call_identity(scope.clone(), gadk_auth::static_token("tok"));
    let inside = with_auth_scope(Some(id), async {
        (
            current_credential_scope(),
            AUTH_SCOPE.with(|id| id.identity.clone()),
        )
    })
    .await;
    assert_eq!(inside, (Some(scope), None));
}

/// A same-origin redirect keeps every credential header (reqwest strips `Authorization` only
/// across scheme, host or port); a cross-origin one is refused before the target is reached.
#[tokio::test]
async fn redirects_follow_go_check_redirect_on_the_wire() {
    let (target, target_heads) = serve(|_, _| ok_response()).await;
    let (base, heads) = serve(move |me, path| match path {
        "/a" => redirect_to(&format!("{me}/b")),
        "/away" => redirect_to(&format!("{target}/stolen")),
        _ => ok_response(),
    })
    .await;
    let client = auth_http_client();

    let resp = client
        .get(format!("{base}/a"))
        .header("authorization", "Bearer secret")
        .header("x-card-key", "key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let second = heads.lock()[1].to_ascii_lowercase();
    assert!(second.starts_with("get /b "), "second request = {second}");
    assert!(second.contains("authorization: bearer secret"), "{second}");
    assert!(second.contains("x-card-key: key"), "{second}");

    let err = client
        .get(format!("{base}/away"))
        .header("x-card-key", "key")
        .send()
        .await
        .unwrap_err();
    let text = chain_text(&err);
    assert!(
        text.contains("remoteagent: refusing redirect from http://127.0.0.1:"),
        "error = {text}"
    );
    assert!(
        target_heads.lock().is_empty(),
        "the redirect target was reached"
    );
}

/// The card-fetch client times out after exactly 30 s.
#[tokio::test(start_paused = true)]
async fn card_fetch_http_client_times_out_after_30s() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", silent.local_addr().unwrap());
    let start = tokio::time::Instant::now();
    let err = card_fetch_http_client().get(url).send().await.unwrap_err();
    assert!(err.is_timeout(), "error = {err:?}");
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(30) && waited < Duration::from_secs(31),
        "the request timed out after {waited:?}, want 30s"
    );
}

/// Go `cardSendsInClear`: non-loopback http interfaces, in card order.
#[test]
fn card_sends_in_clear_reports_non_loopback_http() {
    let card = |urls: &[&str]| a2a::AgentCard {
        supported_interfaces: urls
            .iter()
            .map(|u| a2a::AgentInterface::new(*u, "JSONRPC"))
            .collect(),
        ..serde_json::from_value(serde_json::json!({
            "name": "a2a", "description": "", "version": "1",
            "capabilities": {}, "defaultInputModes": [], "defaultOutputModes": [],
            "skills": [], "supportedInterfaces": []
        }))
        .unwrap()
    };
    let cases: [(&[&str], &[&str]); 7] = [
        (&["http://127.0.0.1:8080"], &[]),
        (&["http://localhost:8080", "http://a.localhost"], &[]),
        (&["http://[::1]:8080", "http://[::ffff:127.0.0.1]/"], &[]),
        (&["https://remote.invalid"], &[]),
        (
            &["http://remote.invalid:8080"],
            &["http://remote.invalid:8080"],
        ),
        (
            &["http://remote.invalid:8080", "http://other.invalid:8080"],
            &["http://remote.invalid:8080", "http://other.invalid:8080"],
        ),
        (&["HTTPS://remote.invalid", "not a url"], &[]),
    ];
    for (urls, want) in cases {
        assert_eq!(card_sends_in_clear(&card(urls)), want, "{urls:?}");
    }
}

/// A token error whose chain holds a reqwest status error (the shape google-cloud-auth builds
/// around a response body it prints) is redacted to the status.
#[tokio::test]
async fn redact_token_error_redacts_a_reqwest_status_error() {
    let (base, _) = serve(|_, _| {
        "HTTP/1.1 400 Bad Request\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret"
            .to_owned()
    })
    .await;
    let status_err = reqwest::get(base)
        .await
        .unwrap()
        .error_for_status()
        .unwrap_err();
    #[derive(Debug)]
    struct PrintsBody(reqwest::Error);
    impl fmt::Display for PrintsBody {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fetch token, body=<secret>")
        }
    }
    impl StdError for PrintsBody {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }
    let got = redact_token_error(gadk_auth::Error::other(PrintsBody(status_err)));
    assert_eq!(
        got.to_string(),
        "oauth2: cannot fetch token: 400 Bad Request (response body redacted)"
    );
    assert!(find_in_chain::<reqwest::Error>(&got).is_some());
}
