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

//! Rust-only tests of the MCP HTTP client: an HTTP 404 reaches the refresher with
//! `reinit_on_expired_session(false)`, a 401 with and without `WWW-Authenticate` refreshes once
//! and replays, a second 401 is returned, and credential errors carry the `auth: …` text
//! (Task 7).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_auth::{
    Bearer, ConsentRequiredError, Credential, CredentialFuture, Error as AuthError, ProviderFn,
    RefreshingProvider,
};
use http::{Method, StatusCode};
use pretty_assertions::assert_eq;
use rmcp::ServiceError;

use super::*;
use crate::config::{Config, McpTransport, default_client_info};
use crate::testutil::{FrontHook, HttpTestServer, RecordedRequest, weather_server};
use crate::transport::{HttpTarget, build_transport, http_factory_for};

fn bearer(token: &str) -> Arc<dyn Credential> {
    Arc::new(Bearer {
        token: token.into(),
    })
}

/// Yields `current`; `refresh` counts itself, records the rejected token, and answers `refresh_to`,
/// which becomes `current` when it is a credential.
struct Rotating {
    current: Mutex<Arc<dyn Credential>>,
    refresh_to: Result<Arc<dyn Credential>, AuthError>,
    refreshes: AtomicUsize,
    rejected: Mutex<Vec<String>>,
}

impl Rotating {
    fn new(stale: &str, refresh_to: Result<Arc<dyn Credential>, AuthError>) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(bearer(stale)),
            refresh_to,
            refreshes: AtomicUsize::new(0),
            rejected: Mutex::default(),
        })
    }

    fn refreshes(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl CredentialProvider for Rotating {
    async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, AuthError> {
        Ok(self.current.lock().unwrap().clone())
    }

    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RefreshingProvider for Rotating {
    async fn refresh(
        &self,
        _cx: &AuthContext,
        rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, AuthError> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        let token = rejected.downcast_ref::<Bearer>().unwrap().token.clone();
        self.rejected.lock().unwrap().push(token);
        if let Ok(fresh) = &self.refresh_to {
            *self.current.lock().unwrap() = fresh.clone();
        }
        self.refresh_to.clone()
    }
}

/// A provider that never refreshes and always fails with `err`.
fn failing(err: AuthError) -> Arc<dyn CredentialProvider> {
    ProviderFn::new(move |_cx: &AuthContext| -> CredentialFuture {
        let err = err.clone();
        Box::pin(async move { Err(err) })
    })
    .into_provider()
}

/// Answers every request that does not carry `Bearer <good>` with `status`, go-sdk's
/// `invalid token\n` body and, when given, a `WWW-Authenticate` challenge; requests that do go on
/// to the MCP service.
fn reject_unless(
    good: &'static str,
    status: StatusCode,
    challenge: Option<&'static str>,
) -> FrontHook {
    Arc::new(move |parts, _| {
        let authorization = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        if authorization == Some(format!("Bearer {good}").as_str()) {
            return None;
        }
        let mut resp = http::Response::builder()
            .status(status)
            .header("content-type", "text/plain; charset=utf-8");
        if let Some(challenge) = challenge {
            resp = resp.header("www-authenticate", challenge);
        }
        Some(resp.body("invalid token\n".to_owned()).unwrap())
    })
}

fn authorizations(requests: &[RecordedRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|r| {
            r.headers
                .get("authorization")
                .map_or_else(String::new, |v| v.to_str().unwrap().to_owned())
        })
        .collect()
}

/// The client of a pooled session for `provider`, over a default `reqwest::Client`.
fn auth_client(provider: Arc<dyn CredentialProvider>) -> AdkHttpClient {
    AdkHttpClient {
        inner: reqwest::Client::new(),
        headers: http::HeaderMap::new(),
        auth: Some((provider, AuthContext::anonymous())),
    }
}

/// The `StreamableHttpError` behind a request error of an rmcp session.
fn http_error(err: &ServiceError) -> &HttpError {
    match err {
        ServiceError::TransportSend(e) => e.error.downcast_ref::<HttpError>().unwrap(),
        other => panic!("want a transport send error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_http_404_on_a_session_reaches_the_caller_with_reinit_off() {
    let server = HttpTestServer::start(weather_server).await;
    let factory = build_transport(&Config::new(McpTransport::Endpoint(server.url()))).unwrap();
    let session = factory.connect(default_client_info()).await.unwrap();
    session.peer().list_tools(None).await.unwrap();

    // The server forgets the session: every POST on it answers 404.
    server.set_front(Some(Arc::new(|parts, _| {
        (parts.method == Method::POST && parts.headers.contains_key("mcp-session-id")).then(|| {
            http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(String::new())
                .unwrap()
        })
    })));
    let err = session.peer().list_tools(None).await.unwrap_err();
    assert!(
        matches!(http_error(&err), StreamableHttpError::SessionExpired),
        "{err:?}"
    );
    // rmcp did not re-initialize on its own: one initialize POST (the only POST without a
    // session id) in all.
    let initializes = server
        .requests()
        .iter()
        .filter(|r| r.method == Method::POST && !r.headers.contains_key("mcp-session-id"))
        .count();
    assert_eq!(initializes, 1);
    session.cancel().await.unwrap();
}

#[tokio::test]
async fn an_auth_rejection_refreshes_once_and_replays() {
    let cases = [
        (StatusCode::UNAUTHORIZED, None),
        (
            StatusCode::UNAUTHORIZED,
            Some(r#"Bearer error="invalid_token""#),
        ),
        (StatusCode::FORBIDDEN, None),
        (
            StatusCode::FORBIDDEN,
            Some(r#"Bearer error="insufficient_scope", scope="mcp""#),
        ),
    ];
    for (status, challenge) in cases {
        let server = HttpTestServer::start(weather_server).await;
        server.set_front(Some(reject_unless("fresh", status, challenge)));
        let target = HttpTarget::from_transport(&McpTransport::Endpoint(server.url())).unwrap();
        let provider = Rotating::new("stale", Ok(bearer("fresh")));
        let factory = http_factory_for(&target, provider.clone(), AuthContext::anonymous());

        let session = factory.connect(default_client_info()).await.unwrap();
        let tools = session.peer().list_tools(None).await.unwrap().tools;
        assert_eq!(tools[0].name, "get_weather");
        session.cancel().await.unwrap();

        let case = format!("{status} with challenge {challenge:?}");
        assert_eq!(provider.refreshes(), 1, "{case}");
        assert_eq!(*provider.rejected.lock().unwrap(), ["stale"], "{case}");
        let got = authorizations(&server.requests());
        assert_eq!(got[..2], ["Bearer stale", "Bearer fresh"], "{case}");
        assert!(
            got[2..].iter().all(|a| a == "Bearer fresh"),
            "{case}: {got:?}"
        );
    }
}

#[tokio::test]
async fn a_second_401_is_returned() {
    let cases = [
        (
            None,
            "unexpected server response: HTTP 401 Unauthorized: invalid token\n",
        ),
        (Some(r#"Bearer error="invalid_token""#), "Auth required"),
    ];
    for (challenge, want) in cases {
        let server = HttpTestServer::start(weather_server).await;
        server.set_front(Some(reject_unless(
            "fresh",
            StatusCode::UNAUTHORIZED,
            challenge,
        )));
        let target = HttpTarget::from_transport(&McpTransport::Endpoint(server.url())).unwrap();
        let provider = Rotating::new("stale", Ok(bearer("also-stale")));
        let factory = http_factory_for(&target, provider.clone(), AuthContext::anonymous());

        let err = factory.connect(default_client_info()).await.err().unwrap();
        assert!(err.to_string().contains(want), "{challenge:?}: {err}");
        assert_eq!(provider.refreshes(), 1, "{challenge:?}");
        assert_eq!(
            authorizations(&server.requests()),
            ["Bearer stale", "Bearer also-stale"],
            "{challenge:?}"
        );
    }
}

#[tokio::test]
async fn delete_and_get_stream_refresh_on_a_rejection_too() {
    let server = HttpTestServer::start(weather_server).await;
    // `delete_session` and `get_stream` see a 401 without a challenge as `Client(e)` from
    // `error_for_status`. The fresh credential's DELETE is answered 200 and its GET 405.
    server.set_front(Some(Arc::new(|parts, _| {
        let fresh = parts
            .headers
            .get("authorization")
            .is_some_and(|v| v == "Bearer fresh");
        let status = match (fresh, &parts.method) {
            (false, _) => StatusCode::UNAUTHORIZED,
            (true, &Method::GET) => StatusCode::METHOD_NOT_ALLOWED,
            (true, _) => StatusCode::OK,
        };
        Some(
            http::Response::builder()
                .status(status)
                .body(String::new())
                .unwrap(),
        )
    })));
    let uri: Arc<str> = server.url().into();

    let provider = Rotating::new("stale", Ok(bearer("fresh")));
    let client = auth_client(provider.clone());
    client
        .delete_session(uri.clone(), "s-1".into(), None, HashMap::new())
        .await
        .unwrap();
    assert_eq!(provider.refreshes(), 1);

    let provider = Rotating::new("stale", Ok(bearer("fresh")));
    let client = auth_client(provider.clone());
    let err = client
        .get_stream(uri.clone(), Some("s-1".into()), None, None, HashMap::new())
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, StreamableHttpError::ServerDoesNotSupportSse),
        "{err:?}"
    );
    assert_eq!(provider.refreshes(), 1);

    let got = server.requests();
    let methods: Vec<_> = got.iter().map(|r| r.method.clone()).collect();
    assert_eq!(
        methods,
        [Method::DELETE, Method::DELETE, Method::GET, Method::GET]
    );
    assert_eq!(
        authorizations(&got),
        [
            "Bearer stale",
            "Bearer fresh",
            "Bearer stale",
            "Bearer fresh"
        ]
    );
}

#[tokio::test]
async fn a_failed_refresh_returns_the_rejection_unless_it_needs_consent() {
    let server = HttpTestServer::start(weather_server).await;
    server.set_front(Some(reject_unless("fresh", StatusCode::UNAUTHORIZED, None)));
    let uri: Arc<str> = server.url().into();

    // A refresh error degrades to the rejection the server sent.
    let provider = Rotating::new("stale", Err(AuthError::msg("refresh failed")));
    let err = auth_client(provider.clone())
        .delete_session(uri.clone(), "s-1".into(), None, HashMap::new())
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, StreamableHttpError::Client(e) if e.status() == Some(StatusCode::UNAUTHORIZED)),
        "{err:?}"
    );
    assert_eq!(provider.refreshes(), 1);

    // Interactive consent is returned, findable along the source chain.
    let consent = ConsentRequiredError {
        auth_uri: "https://consent.example/authorize".into(),
        ..Default::default()
    };
    let provider = Rotating::new("stale", Err(AuthError::from(consent.clone())));
    let err = auth_client(provider)
        .delete_session(uri.clone(), "s-1".into(), None, HashMap::new())
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "Io error: auth: interactive consent required"
    );
    assert_eq!(find_consent(&err), Some(&consent));

    // The fresh credential's apply error is returned and nothing more is sent.
    let provider = Rotating::new("stale", Ok(bearer("")));
    let err = auth_client(provider)
        .delete_session(uri.clone(), "s-1".into(), None, HashMap::new())
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "Io error: auth: apply credential: auth: bearer credential missing token"
    );
    // One rejected request per case, never replayed.
    assert_eq!(
        authorizations(&server.requests()),
        ["Bearer stale", "Bearer stale", "Bearer stale"]
    );
}

#[tokio::test]
async fn credential_errors_carry_the_auth_text() {
    let server = HttpTestServer::start(weather_server).await;
    let uri: Arc<str> = server.url().into();
    let cases = [
        (
            failing(AuthError::msg("no token")),
            "auth: resolve credential: no token",
        ),
        (
            Rotating::new("", Ok(bearer("fresh"))) as Arc<dyn CredentialProvider>,
            "auth: apply credential: auth: bearer credential missing token",
        ),
    ];
    for (provider, want) in cases {
        let err = auth_client(provider.clone())
            .delete_session(uri.clone(), "s-1".into(), None, HashMap::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), format!("Io error: {want}"));

        let target = HttpTarget::from_transport(&McpTransport::Endpoint(server.url())).unwrap();
        let factory = http_factory_for(&target, provider, AuthContext::anonymous());
        let err = factory.connect(default_client_info()).await.err().unwrap();
        assert!(err.to_string().contains(want), "{err}");
    }
    // A request whose credential fails is never sent.
    assert!(server.requests().is_empty(), "{:?}", server.requests());
}

#[test]
fn rejections_are_401_and_403_in_every_shape() {
    use std::borrow::Cow;

    use rmcp::transport::streamable_http_client::{AuthRequiredError, InsufficientScopeError};

    let yes: [HttpError; 6] = [
        StreamableHttpError::AuthRequired(AuthRequiredError::new("Bearer".into())),
        StreamableHttpError::InsufficientScope(InsufficientScopeError::new("Bearer".into(), None)),
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("HTTP 401 Unauthorized: x")),
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("HTTP 403 Forbidden: x")),
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("HTTP 401")),
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("HTTP 403")),
    ];
    for err in &yes {
        assert!(is_rejection(err), "{err:?}");
    }
    let no: [HttpError; 4] = [
        StreamableHttpError::SessionExpired,
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("HTTP 404 Not Found: x")),
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("bad HTTP 401")),
        StreamableHttpError::Io(std::io::Error::other("auth: resolve credential: x")),
    ];
    for err in &no {
        assert!(!is_rejection(err), "{err:?}");
    }
}

#[test]
fn configured_headers_are_sent_and_joined() {
    let mut headers = http::HeaderMap::new();
    headers.append("x-a", "1".parse().unwrap());
    headers.append("x-a", "2".parse().unwrap());
    headers.insert("x-b", "b".parse().unwrap());
    let client = AdkHttpClient {
        inner: reqwest::Client::new(),
        headers,
        auth: None,
    };
    let mut rmcp_own = HashMap::new();
    rmcp_own.insert(
        HeaderName::from_static("mcp-protocol-version"),
        HeaderValue::from_static("2025-06-18"),
    );
    let sent = client.tracked_headers(rmcp_own);
    let get = |name: &'static str| {
        sent.get(&HeaderName::from_static(name))
            .unwrap()
            .to_str()
            .unwrap()
    };
    assert_eq!(get("x-a"), "1, 2");
    assert_eq!(get("x-b"), "b");
    assert_eq!(get("mcp-protocol-version"), "2025-06-18");
    assert_eq!(get("user-agent"), gadk_platform::tracking_header());
    assert_eq!(sent.len(), 5);
}

/// Go `auth.Transport.RoundTrip` degrades every non-consent `Refresh` error to the rejection,
/// even one that is itself an `Error::ApplyCredential`: only `applyAndSend`'s apply error of the
/// fresh credential is returned.
#[tokio::test]
async fn a_refresh_error_that_is_an_apply_error_still_degrades() {
    let server = HttpTestServer::start(weather_server).await;
    server.set_front(Some(reject_unless("fresh", StatusCode::UNAUTHORIZED, None)));
    let uri: Arc<str> = server.url().into();

    let provider = Rotating::new(
        "stale",
        Err(AuthError::ApplyCredential(Box::new(AuthError::msg("boom")))),
    );
    let err = auth_client(provider.clone())
        .delete_session(uri, "s-1".into(), None, HashMap::new())
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, StreamableHttpError::Client(e) if e.status() == Some(StatusCode::UNAUTHORIZED)),
        "{err:?}"
    );
    assert_eq!(provider.refreshes(), 1);
    assert_eq!(authorizations(&server.requests()), ["Bearer stale"]);
}

/// A bearer token that also writes its own `user-agent`.
#[derive(Debug)]
struct AgentBearer(&'static str);

#[async_trait::async_trait]
impl Credential for AgentBearer {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), AuthError> {
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::try_from(format!("Bearer {}", self.0)).unwrap(),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("cred-agent/1"));
        Ok(())
    }
}

/// Yields `AgentBearer("stale")` and refreshes to `AgentBearer("fresh")`.
struct AgentProvider;

#[async_trait::async_trait]
impl CredentialProvider for AgentProvider {
    async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, AuthError> {
        Ok(Arc::new(AgentBearer("stale")))
    }

    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RefreshingProvider for AgentProvider {
    async fn refresh(
        &self,
        _cx: &AuthContext,
        _rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, AuthError> {
        Ok(Arc::new(AgentBearer("fresh")))
    }
}

/// Go wraps `auth.Transport` around `trackingTransport` (set.go `httpClient`), so the tracking
/// step merges a `user-agent` the credential wrote after the ADK tokens, on the first send and on
/// the replay.
#[tokio::test]
async fn a_credential_user_agent_is_merged_after_the_tracking_tokens() {
    let server = HttpTestServer::start(weather_server).await;
    server.set_front(Some(Arc::new(|parts, _| {
        let fresh = parts
            .headers
            .get("authorization")
            .is_some_and(|v| v == "Bearer fresh");
        let status = if fresh {
            StatusCode::OK
        } else {
            StatusCode::UNAUTHORIZED
        };
        Some(
            http::Response::builder()
                .status(status)
                .body(String::new())
                .unwrap(),
        )
    })));
    let mut headers = http::HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("caller-app/1.0"));
    let client = AdkHttpClient {
        inner: reqwest::Client::new(),
        headers,
        auth: Some((Arc::new(AgentProvider), AuthContext::anonymous())),
    };
    client
        .delete_session(server.url().into(), "s-1".into(), None, HashMap::new())
        .await
        .unwrap();

    let adk = gadk_platform::tracking_header();
    let got = server.requests();
    assert_eq!(authorizations(&got), ["Bearer stale", "Bearer fresh"]);
    for r in &got {
        // The credential's `insert` replaces the configured value, as Go's `Header.Set` does.
        let ua: Vec<_> = r.headers.get_all("user-agent").iter().collect();
        assert_eq!(ua, [format!("{adk} cred-agent/1").as_str()]);
        let xgac: Vec<_> = r.headers.get_all("x-goog-api-client").iter().collect();
        assert_eq!(xgac, [adk.as_str()]);
    }
}
