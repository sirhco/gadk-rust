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

//! Ports of ADK-Go `agent/remoteagent/v2/auth_test.go`: the tests that run an agent through
//! `new_a2a` (plan 5b Task 8).
//!
//! Go hands the credential provider a `context.Context` that is the `agent.InvocationContext`
//! and carries the scope as an `a2aclient.SessionID` value. Here the provider receives an
//! `AuthContext { identity }` and reads the scope with `current_credential_scope()` (the
//! `AUTH_SCOPE` task-local, plan D3). Go's httptest servers wrapping `a2asrv` are the harness's
//! a2a-rs JSON-RPC router on an axum listener; a recording middleware stands in for Go's
//! wrapping `http.HandlerFunc`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::type_complexity
)]

use std::sync::Arc;

use gadk_agent::{InvocationContext, RunConfig, StreamingMode};
#[cfg(feature = "server")]
use gadk_auth::{AuthContext, CredentialFuture, ProviderFn};
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};

use crate::parity_tests::agent_remoteagent_v2_a2a_agent::harness::user_hello;
use crate::remoteagent::config::find_remote_agent_state;
use crate::remoteagent::{
    A2AClientProvider, A2AConfig, ClientContext, credential_scope, new_a2a, new_a2a_client_provider,
};

// ---------------------------------------------------------------- helpers

/// Go `newInvocationContextFor`: a session with an explicit identity holding one user `hello`,
/// in SSE mode.
async fn invocation_context_for(
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> InvocationContext {
    let service = InMemorySessionService::new();
    let session = service
        .create(CreateRequest {
            app_name: app_name.to_owned(),
            user_id: user_id.to_owned(),
            session_id: Some(session_id.to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut hello = user_hello();
    service.append_event(&*session, &mut hello).await.unwrap();
    let session = service
        .get(gadk_session::GetRequest {
            app_name: app_name.to_owned(),
            user_id: user_id.to_owned(),
            session_id: session_id.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    InvocationContext::builder()
        .session(session)
        .run_config(RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        })
        .build_invocation()
}

/// A card with one JSON-RPC interface at `url` and no security section.
fn plain_card(url: &str) -> a2a::AgentCard {
    serde_json::from_value(serde_json::json!({
        "name": "a2a",
        "version": "",
        "supportedInterfaces": [
            {"url": url, "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}
        ],
        "capabilities": {"streaming": true},
    }))
    .unwrap()
}

/// Go `newSecureCard`: `plain_card` declaring the given security schemes (the auth path never
/// reads them).
fn secure_card(url: &str, schemes: Vec<(&str, a2a::SecurityScheme)>) -> a2a::AgentCard {
    let mut card = plain_card(url);
    if !schemes.is_empty() {
        card.security_schemes = Some(
            schemes
                .into_iter()
                .map(|(name, scheme)| (name.to_owned(), scheme))
                .collect(),
        );
    }
    card
}

/// Go `bearerCard`.
fn bearer_card(url: &str) -> a2a::AgentCard {
    secure_card(
        url,
        vec![(
            "bearer",
            a2a::SecurityScheme::HttpAuth(a2a::HttpAuthSecurityScheme {
                scheme: "Bearer".into(),
                description: None,
                bearer_format: None,
            }),
        )],
    )
}

/// A card that asks for an API key in `X-Card-Key`.
#[cfg(feature = "server")]
fn api_key_card(url: &str) -> a2a::AgentCard {
    secure_card(
        url,
        vec![(
            "apikey",
            a2a::SecurityScheme::ApiKey(a2a::ApiKeySecurityScheme {
                location: "header".into(),
                name: "X-Card-Key".into(),
                description: None,
            }),
        )],
    )
}

/// A provider returning a clone of `cred` on every call.
#[cfg(feature = "server")]
fn provider_of(cred: Arc<dyn gadk_auth::Credential>) -> Arc<dyn gadk_auth::CredentialProvider> {
    ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
        let cred = Arc::clone(&cred);
        Box::pin(async move { Ok(cred) })
    })
    .into_provider()
}

// ---------------------------------------------------------------- ports without a server

#[tokio::test]
async fn test_credential_scope_exported() {
    let ic = invocation_context_for("shop", "alice", "s1").await;
    assert_eq!(
        credential_scope(ic.app_name(), ic.user_id(), ic.session_id(), "crm").0,
        "shop/alice/s1/crm"
    );
}

#[test]
fn test_new_a2_a_auth_with_client_provider_is_error() {
    let err = new_a2a(A2AConfig {
        name: "a2a".into(),
        agent_card: Some(plain_card("http://example.invalid")),
        auth: Some(gadk_auth::static_token("tok")),
        client_provider: Some(new_a2a_client_provider(None)),
        ..Default::default()
    })
    .err()
    .expect("Auth combined with a custom ClientProvider must be refused");
    assert!(
        err.to_string()
            .contains("cannot be combined with a custom ClientProvider"),
        "{err}"
    );
}

// parity: skipped TestNewA2AAuthTypedNilIsError: a Rust `Option<Arc<dyn CredentialProvider>>` cannot hold a nil provider inside a non-nil value; Go's typed-nil check has nothing to guard
// parity: skipped TestIsTypedNil: Go's isTypedNil (nil func/pointer inside an interface) has no Rust analog; `A2AConfig.auth` is `None` or a live provider
// parity: skipped TestAuthContextDerivation: Go's authContext wrapper (WithContext/WithICDelta carrying the scope, cell and card client onto derived contexts) does not exist; the scope is the AUTH_SCOPE task-local, visible to everything the run awaits
// parity: skipped TestAuthContextWithICDeltaWithoutContext: as TestAuthContextDerivation; there is no context value to lose across an invocation-context delta

// parity: adapted: reqwest::Client has no timeout getter, so the 30 s card-fetch bound is pinned by the transport port's paused-clock card-fetch test; here the derived config fields are checked
#[test]
fn test_new_a2_a_owns_auth_scope() {
    for (name, auth, want) in [
        ("auth set", Some(gadk_auth::static_token("tok")), true),
        ("auth unset", None, false),
    ] {
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card("http://example.invalid")),
            auth,
            ..Default::default()
        })
        .unwrap();
        let state = find_remote_agent_state(&*agent).expect("a remote agent's state");
        assert_eq!(state.owns_auth_scope, want, "{name}: owns_auth_scope");
        assert_eq!(
            state.card_fetch_client.is_some(),
            want,
            "{name}: card_fetch_client set"
        );
        assert_eq!(state.auth.is_some(), want, "{name}: auth");
    }
}

// parity: adapted: Go checks authSendContext returns the invocation context unchanged; Rust has no send context, so the port pins the observable rule: without Auth no credential scope is in effect while the client is built and called
#[tokio::test]
async fn test_remote_agent_auth_unset_leaves_the_context_alone() {
    use parking_lot::Mutex;

    struct Observing(Mutex<Vec<Option<String>>>);
    #[async_trait::async_trait]
    impl A2AClientProvider for Observing {
        async fn create_client(
            &self,
            _cx: ClientContext<'_>,
            _card: &a2a::AgentCard,
        ) -> Result<Arc<dyn crate::remoteagent::A2AClient>, crate::Error> {
            self.0
                .lock()
                .push(crate::remoteagent::current_credential_scope().map(|s| s.0));
            Err(crate::Error::msg("observed"))
        }
    }
    let observing = Arc::new(Observing(Mutex::new(Vec::new())));
    let agent = new_a2a(A2AConfig {
        name: "a2a".into(),
        agent_card: Some(plain_card("http://example.invalid")),
        client_provider: Some(observing.clone()),
        ..Default::default()
    })
    .unwrap();
    let ic = invocation_context_for("app", "erin", "default").await;
    let mut stream = agent.run(ic);
    while futures::StreamExt::next(&mut stream).await.is_some() {}
    assert_eq!(*observing.0.lock(), vec![None]);
}

// ---------------------------------------------------------------- ports against a server

#[cfg(feature = "server")]
mod served {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::Request;
    use axum::middleware::Next;
    use axum::response::{IntoResponse, Response};
    use futures::StreamExt as _;
    use gadk_agent::{Agent, AgentError, InvocationContext, StreamingMode};
    use gadk_auth::{
        ApiKey, AuthContext, Basic, Bearer, Credential, CredentialFuture, CredentialProvider,
        OAuth2, ProviderFn, StaticTokenSource, Token, TokenSource,
    };
    use gadk_session::Event;
    use http::{HeaderMap, StatusCode};
    use parking_lot::Mutex;
    use serde_json::Value;

    use super::{api_key_card, bearer_card, invocation_context_for, plain_card, provider_of};
    use crate::parity_tests::agent_remoteagent_v2_a2a_agent::harness::{
        A2AStream, MockExecutor, a2a_event_replay, invocation_context, run_and_collect,
        status_update, submitted_task, user_hello,
    };
    use crate::remoteagent::auth::{AUTH_SCOPE, CallIdentity, CredentialCell, CredentialScope};
    use crate::remoteagent::{
        A2AClient, A2AClientProvider, A2AConfig, CardResolveOptions, ClientContext,
        credential_scope, current_credential_scope, new_a2a, new_a2a_client_provider,
        new_agent_card_provider,
    };
    use crate::testutil::eventually;

    // ------------------------------------------------------------ helpers

    /// A test server; shut down when dropped.
    struct Server {
        url: String,
        _shutdown: tokio::sync::oneshot::Sender<()>,
    }

    /// Serves the router `build` makes from the server's own base URL.
    async fn serve(build: impl FnOnce(&str) -> axum::Router) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = build(&url);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Server { url, _shutdown: tx }
    }

    /// One request a recording server saw.
    #[derive(Debug, Clone)]
    struct Seen {
        /// The JSON-RPC method (Go `jsonRPCMethod`), `""` for other bodies.
        method: String,
        path: String,
        headers: HeaderMap,
    }

    type Log = Arc<Mutex<Vec<Seen>>>;

    impl Seen {
        fn header(&self, name: &str) -> String {
            self.headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        }
    }

    /// Records every request before `router` serves it (Go's wrapping `http.HandlerFunc` with
    /// `record`); `gate` may answer instead (Go's enforcing server).
    fn recording(
        router: axum::Router,
        log: Log,
        gate: Option<Arc<dyn Fn(&HeaderMap) -> Option<Response> + Send + Sync>>,
    ) -> axum::Router {
        router.layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let log = Arc::clone(&log);
                let gate = gate.clone();
                async move {
                    let (parts, body) = req.into_parts();
                    let bytes = axum::body::to_bytes(body, usize::MAX)
                        .await
                        .unwrap_or_default();
                    let method = serde_json::from_slice::<Value>(&bytes)
                        .ok()
                        .and_then(|v| v.get("method")?.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    log.lock().push(Seen {
                        method,
                        path: parts.uri.path().to_owned(),
                        headers: parts.headers.clone(),
                    });
                    if let Some(answer) = gate.as_ref().and_then(|g| g(&parts.headers)) {
                        return answer;
                    }
                    next.run(Request::from_parts(parts, Body::from(bytes)))
                        .await
                }
            },
        ))
    }

    /// The a2a-rs JSON-RPC router over `executor`.
    fn a2a_router(executor: MockExecutor) -> axum::Router {
        a2a_server::jsonrpc::jsonrpc_router(Arc::new(a2a_server::DefaultRequestHandler::new(
            executor,
            a2a_server::InMemoryTaskStore::new(),
        )))
    }

    /// Go `serveRecordingA2A` replaying one agent message `text`.
    async fn serve_recording_a2a(text: &str) -> (Server, Log) {
        let log: Log = Arc::default();
        let msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text(text)]);
        let router = recording(
            a2a_router(a2a_event_replay(vec![a2a::StreamResponse::Message(msg)])),
            Arc::clone(&log),
            None,
        );
        (serve(|_| router).await, log)
    }

    /// Go's open-task executor: a submitted task, one data artifact, then nothing until the
    /// request goes away; `cancel` answers with a canceled status.
    fn open_task_executor() -> MockExecutor {
        MockExecutor {
            execute: Some(Arc::new(|ctx: a2a_server::ExecutorContext| -> A2AStream {
                let task = submitted_task(&ctx);
                let artifact = a2a::TaskArtifactUpdateEvent {
                    task_id: ctx.task_id.clone(),
                    context_id: ctx.context_id.clone(),
                    artifact: a2a::Artifact {
                        artifact_id: "artifact-1".into(),
                        name: None,
                        description: None,
                        parts: vec![a2a::Part::data(serde_json::json!({"foo": "bar"}))],
                        metadata: None,
                        extensions: None,
                    },
                    append: None,
                    last_chunk: None,
                    metadata: None,
                };
                futures::stream::iter(vec![
                    Ok(a2a::StreamResponse::Task(task)),
                    Ok(a2a::StreamResponse::ArtifactUpdate(artifact)),
                ])
                .chain(futures::stream::pending())
                .boxed()
            })),
            cancel: Some(Arc::new(|ctx: a2a_server::ExecutorContext| -> A2AStream {
                let update = status_update(&ctx.task_id, &ctx.context_id, a2a::TaskState::Canceled);
                futures::stream::iter(vec![Ok(a2a::StreamResponse::StatusUpdate(update))]).boxed()
            })),
        }
    }

    /// Go's `for … range agent.Run(ic) { …; break }`: takes the first item and drops the run,
    /// which hands the remote-task cleanup to a spawned task.
    async fn run_first(agent: &Arc<dyn Agent>, ic: InvocationContext) {
        let mut stream = agent.run(ic);
        let first = stream.next().await.expect("one event");
        assert!(first.is_ok(), "agent.Run() error = {:?}", first.err());
    }

    /// Go `firstErrorEvent`.
    fn first_error_event(events: &[Event]) -> Option<&Event> {
        events.iter().find(|e| !e.response.error_message.is_empty())
    }

    /// Go `eventsContainText`.
    fn events_contain_text(events: &[Event], want: &str) -> bool {
        events.iter().any(|e| {
            e.response
                .content
                .as_ref()
                .is_some_and(|c| c.parts.iter().any(|p| p.text.contains(want)))
        })
    }

    /// Runs one invocation over a user `hello` in `mode` and returns its events (Go
    /// `runAndCollect` with `newInvocationContext`; a run error fails the test).
    async fn run_hello(agent: &Arc<dyn Agent>, mode: StreamingMode) -> Vec<Event> {
        let ic = invocation_context("auth", Arc::clone(agent), vec![user_hello()], mode).await;
        let (events, err) = run_and_collect(ic, agent).await;
        assert!(err.is_none(), "agent.Run() error = {err:?}");
        events
    }

    /// Go `runAndCollect(newInvocationContext(t, []*session.Event{newUserHello()}), agent)`:
    /// the events and the run's error.
    async fn run_hello_err(
        agent: &Arc<dyn Agent>,
        mode: StreamingMode,
    ) -> (Vec<Event>, Option<String>) {
        let ic = invocation_context("auth", Arc::clone(agent), vec![user_hello()], mode).await;
        let (events, err) = run_and_collect(ic, agent).await;
        (events, err.map(|e| e.to_string()))
    }

    /// A provider resolving `f(identity, scope)` as a bearer token.
    fn bearer_provider(
        f: impl Fn(Option<&gadk_agent::Identity>, Option<CredentialScope>) -> Result<String, String>
        + Send
        + Sync
        + 'static,
    ) -> Arc<dyn CredentialProvider> {
        ProviderFn::new(move |cx: &AuthContext| -> CredentialFuture {
            let result = f(cx.identity.as_ref(), current_credential_scope());
            Box::pin(async move {
                match result {
                    Ok(token) => Ok(Arc::new(Bearer { token }) as Arc<dyn Credential>),
                    Err(e) => Err(gadk_auth::Error::msg(e)),
                }
            })
        })
        .into_provider()
    }

    /// Go `errResolve`.
    #[derive(Debug)]
    struct ResolveFailed;

    impl std::fmt::Display for ResolveFailed {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("resolve failed")
        }
    }

    impl std::error::Error for ResolveFailed {}

    // ------------------------------------------------------------ ports

    #[tokio::test]
    async fn test_remote_agent_auth_attaches_bearer_header() {
        let (srv, log) = serve_recording_a2a("ok").await;
        let card = bearer_card(&srv.url);
        let provided = card.clone();
        let provider: crate::remoteagent::AgentCardProvider = Arc::new(move || {
            let card = provided.clone();
            Box::pin(async move { Ok(card) })
        });
        let cases = [
            (
                "static card, streaming",
                A2AConfig {
                    agent_card: Some(card.clone()),
                    ..Default::default()
                },
                StreamingMode::Sse,
            ),
            (
                "card provider, streaming",
                A2AConfig {
                    agent_card_provider: Some(provider),
                    ..Default::default()
                },
                StreamingMode::Sse,
            ),
            (
                "static card, non-streaming",
                A2AConfig {
                    agent_card: Some(card.clone()),
                    ..Default::default()
                },
                StreamingMode::None,
            ),
        ];
        for (name, cfg, mode) in cases {
            log.lock().clear();
            let agent = new_a2a(A2AConfig {
                name: "a2a".into(),
                auth: Some(gadk_auth::static_token("secret-token")),
                ..cfg
            })
            .unwrap();
            run_hello(&agent, mode).await;
            let seen = log.lock().clone();
            assert!(!seen.is_empty(), "{name}: no request reached the server");
            for s in &seen {
                assert_eq!(s.header("authorization"), "Bearer secret-token", "{name}");
            }
        }
    }

    // parity: adapted: a2a-rs's JSON-RPC transport ignores the HTTP status, so the rejection surfaces as its body-decoding error rather than an error naming 401
    #[tokio::test]
    async fn test_remote_agent_auth_accepted_by_enforcing_server() {
        const GOOD: &str = "good-token";
        let msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text("ok")]);
        let gate: Arc<dyn Fn(&HeaderMap) -> Option<Response> + Send + Sync> =
            Arc::new(|h: &HeaderMap| {
                let ok = h
                    .get("authorization")
                    .is_some_and(|v| v.as_bytes() == format!("Bearer {GOOD}").as_bytes());
                (!ok).then(|| (StatusCode::UNAUTHORIZED, "unauthorized\n").into_response())
            });
        let router = recording(
            a2a_router(a2a_event_replay(vec![a2a::StreamResponse::Message(msg)])),
            Arc::default(),
            Some(gate),
        );
        let srv = serve(|_| router).await;
        for (name, token, want_err) in [("accepted", GOOD, false), ("rejected", "bad-token", true)]
        {
            let agent = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(bearer_card(&srv.url)),
                auth: Some(gadk_auth::static_token(token)),
                ..Default::default()
            })
            .unwrap();
            let (events, err) = run_hello_err(&agent, StreamingMode::Sse).await;
            if want_err {
                let e = err.unwrap_or_else(|| {
                    panic!("{name}: want an error from the rejected request, got none")
                });
                // a2a-rs's JSON-RPC transport does not check the HTTP status; it fails decoding
                // the 401 body instead of naming the status as a2a-go does.
                assert!(
                    e.contains(
                        "failed to parse JSON-RPC response: expected value at line 1 column 1"
                    ),
                    "{name}: agent.Run() error = {e:?}"
                );
                assert!(
                    !events_contain_text(&events, "ok"),
                    "{name}: the rejected request answered"
                );
                continue;
            }
            assert!(err.is_none(), "{name}: agent.Run() error = {err:?}");
            assert!(events_contain_text(&events, "ok"), "{name}: reply missing");
        }
    }

    // parity: adapted: the provider reads the scope with current_credential_scope() (Go a2aclient.SessionIDFrom(ctx))
    #[tokio::test]
    async fn test_remote_agent_auth_scope_is_per_user() {
        let (srv, log) = serve_recording_a2a("ok").await;
        let cache: Arc<Mutex<HashMap<CredentialScope, String>>> = Arc::default();
        let per_scope = {
            let cache = Arc::clone(&cache);
            bearer_provider(move |_, scope| {
                let scope = scope.ok_or("no credential scope on the context")?;
                let mut cache = cache.lock();
                let n = cache.len();
                Ok(cache
                    .entry(scope)
                    .or_insert_with(|| format!("tok-{n}"))
                    .clone())
            })
        };
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(per_scope),
            ..Default::default()
        })
        .unwrap();
        let mut seen = BTreeMap::new();
        for user in ["alice", "bob"] {
            log.lock().clear();
            let ic =
                invocation_context_for("TestRemoteAgent_AuthScopeIsPerUser", user, "default").await;
            let (_, err) = run_and_collect(ic, &agent).await;
            assert!(err.is_none(), "{user}: {err:?}");
            let got = log
                .lock()
                .last()
                .map(|s| s.header("authorization"))
                .unwrap_or_default();
            seen.insert(user, got);
        }
        assert!(
            !seen["alice"].is_empty() && !seen["bob"].is_empty(),
            "{seen:?}"
        );
        assert_ne!(
            seen["alice"], seen["bob"],
            "the credential scope collides across users"
        );
    }

    // parity: adapted: a Rust provider receives AuthContext { identity } rather than the invocation context it would type-assert; the acting user comes from the identity
    #[tokio::test]
    async fn test_remote_agent_auth_provider_sees_invocation_context() {
        let (srv, log) = serve_recording_a2a("ok").await;
        let per_user = bearer_provider(|identity, _| {
            let identity = identity.ok_or("no invocation identity")?;
            Ok(format!("tok-{}", identity.user_id))
        });
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(per_user),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context_for("app", "carol", "default").await;
        let (_, err) = run_and_collect(ic, &agent).await;
        assert!(err.is_none(), "{err:?}");
        let seen = log.lock().clone();
        assert!(!seen.is_empty());
        for s in seen {
            assert_eq!(s.header("authorization"), "Bearer tok-carol");
        }
    }

    /// The caller's own interceptor (Go `a2aclient.AuthInterceptor` over a credentials store):
    /// writes the token stored for `scope`.
    struct OwnInterceptor {
        scope: Option<CredentialScope>,
        store: HashMap<String, String>,
    }

    #[async_trait::async_trait]
    impl a2a_client::middleware::CallInterceptor for OwnInterceptor {
        async fn before(
            &self,
            _method: &str,
            params: &mut a2a_client::ServiceParams,
        ) -> Result<(), a2a::A2AError> {
            // A scope captured when the client was built, else the one in effect for the call.
            let scope = self.scope.clone().or_else(current_credential_scope);
            if let Some(token) = scope.and_then(|s| self.store.get(&s.0)) {
                params.insert("authorization".into(), vec![format!("Bearer {token}")]);
            }
            Ok(())
        }
    }

    fn factory_with(interceptor: OwnInterceptor) -> Arc<a2a_client::A2AClientFactory> {
        Arc::new(
            a2a_client::A2AClientFactory::builder()
                .register(Arc::new(a2a_client::jsonrpc::JsonRpcTransportFactory::new(
                    None,
                )))
                .with_interceptor(Arc::new(interceptor))
                .build(),
        )
    }

    // parity: adapted: the remediation captures the scope in the caller's own interceptor when the provider builds the client (Go attaches it to each call's context with a2aclient.AttachSessionID); the provider computes it from ClientContext::Invocation
    #[tokio::test]
    async fn test_remote_agent_auth_client_provider_scope_remediation() {
        let log: Log = Arc::default();
        let router = recording(a2a_router(open_task_executor()), Arc::clone(&log), None);
        let srv = serve(|_| router).await;

        let ic = invocation_context_for("remediation", "dave", "default").await;
        let scope = credential_scope(ic.app_name(), ic.user_id(), ic.session_id(), "a2a");

        struct Remediating {
            store: HashMap<String, String>,
        }
        #[async_trait::async_trait]
        impl A2AClientProvider for Remediating {
            async fn create_client(
                &self,
                cx: ClientContext<'_>,
                card: &a2a::AgentCard,
            ) -> Result<Arc<dyn A2AClient>, crate::Error> {
                let ClientContext::Invocation(ic) = cx else {
                    return Err(crate::Error::msg(
                        "ClientProvider got a detached context, want the invocation the error message promises",
                    ));
                };
                let scope = credential_scope(ic.app_name(), ic.user_id(), ic.session_id(), "a2a");
                let factory = factory_with(OwnInterceptor {
                    scope: Some(scope),
                    store: self.store.clone(),
                });
                new_a2a_client_provider(Some(factory))
                    .create_client(cx, card)
                    .await
            }
        }
        let store = HashMap::from([(scope.0, "own-token".to_owned())]);
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            client_provider: Some(Arc::new(Remediating { store })),
            ..Default::default()
        })
        .unwrap();
        run_first(&agent, ic).await;

        let has_cancel = || log.lock().iter().any(|s| s.method == "CancelTask");
        assert!(
            eventually(has_cancel).await,
            "no CancelTask reached the server"
        );
        let seen = log.lock().clone();
        for method in ["SendStreamingMessage", "CancelTask"] {
            let s = seen
                .iter()
                .find(|s| s.method == method)
                .unwrap_or_else(|| panic!("no {method} reached the server"));
            assert_eq!(s.header("authorization"), "Bearer own-token", "{method}");
        }
    }

    // parity: adapted: the redirect refusal surfaces through a2a-rs's transport error ("HTTP request failed: error following redirect for url (…)"), which drops the policy's "refusing redirect" source text; the port checks the redirect error and that the target saw nothing
    #[tokio::test]
    async fn test_remote_agent_auth_refuses_cross_origin_redirect() {
        let hits: Log = Arc::default();
        let elsewhere = {
            let router = recording(
                axum::Router::new().fallback(|| async { StatusCode::OK }),
                Arc::clone(&hits),
                None,
            );
            serve(|_| router).await
        };
        let target = elsewhere.url.clone();
        let declared = serve(move |_| {
            axum::Router::new().fallback(move || {
                let target = target.clone();
                async move {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(http::header::LOCATION, target)],
                    )
                }
            })
        })
        .await;

        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(api_key_card(&declared.url)),
            auth: Some(gadk_auth::api_key("X-Card-Key", "secret")),
            ..Default::default()
        })
        .unwrap();
        let (_, err) = run_hello_err(&agent, StreamingMode::Sse).await;
        let seen = hits.lock().clone();
        assert!(
            seen.is_empty(),
            "the redirect target received {} requests; it saw X-Card-Key = {:?}",
            seen.len(),
            seen.first().map(|s| s.header("x-card-key"))
        );
        let e = err.expect("want an error from the refused redirect, got none");
        assert!(
            e.contains("error following redirect"),
            "agent.Run() error = {e:?}"
        );
    }

    #[tokio::test]
    async fn test_remote_agent_auth_allows_same_origin_redirect() {
        let log: Log = Arc::default();
        let msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text("ok")]);
        let rpc = recording(
            a2a_router(a2a_event_replay(vec![a2a::StreamResponse::Message(msg)])),
            Arc::clone(&log),
            None,
        );
        let srv = serve(move |_| {
            axum::Router::new().nest("/rpc", rpc).fallback(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(http::header::LOCATION, "/rpc")],
                )
            })
        })
        .await;
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&format!("{}/start", srv.url))),
            auth: Some(gadk_auth::static_token("secret-token")),
            ..Default::default()
        })
        .unwrap();
        let events = run_hello(&agent, StreamingMode::Sse).await;
        assert!(first_error_event(&events).is_none(), "{events:?}");
        let seen = log.lock().clone();
        assert!(
            !seen.is_empty(),
            "the redirect target never received the request"
        );
        assert_eq!(seen[0].header("authorization"), "Bearer secret-token");
    }

    // parity: adapted: a caller's scope is an AUTH_SCOPE the run is awaited inside (Go: a2aclient.AttachSessionID on the invocation context) and the caller's interceptor reads it with current_credential_scope()
    #[tokio::test]
    async fn test_remote_agent_auth_preserves_caller_scope() {
        let (srv, log) = serve_recording_a2a("ok").await;
        let factory = factory_with(OwnInterceptor {
            scope: None,
            store: HashMap::from([("my-tenant".to_owned(), "tenant-token".to_owned())]),
        });
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            client_provider: Some(new_a2a_client_provider(Some(factory))),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context_for("app", "frank", "default").await;
        let caller = CallIdentity {
            scope: CredentialScope("my-tenant".into()),
            identity: None,
            auth: gadk_auth::static_token("unused"),
            card_fetch_client: None,
            cell: Arc::new(CredentialCell::new()),
        };
        let (_, err) = AUTH_SCOPE.scope(caller, run_and_collect(ic, &agent)).await;
        assert!(err.is_none(), "{err:?}");
        let seen = log.lock().clone();
        assert!(!seen.is_empty());
        assert_eq!(
            seen[0].header("authorization"),
            "Bearer tenant-token",
            "the caller's own scope must survive"
        );
    }

    // parity: adapted: the provider reads the user from AuthContext.identity (Go type-asserts the invocation context); breaking out of Go's range is dropping the stream, whose cleanup then runs on a spawned task the test waits for
    #[tokio::test]
    async fn test_remote_agent_auth_cleanup_reuses_the_invocation_credential() {
        let log: Log = Arc::default();
        let router = recording(a2a_router(open_task_executor()), Arc::clone(&log), None);
        let srv = serve(|_| router).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let per_user = {
            let calls = Arc::clone(&calls);
            bearer_provider(move |identity, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                let identity = identity.ok_or("no invocation identity")?;
                Ok(format!("tok-{}", identity.user_id))
            })
        };
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(per_user),
            ..Default::default()
        })
        .unwrap();
        run_first(
            &agent,
            invocation_context_for("app", "frank", "default").await,
        )
        .await;
        let has_cancel = || log.lock().iter().any(|s| s.method == "CancelTask");
        assert!(
            eventually(has_cancel).await,
            "no CancelTask reached the server; cleanup did not run"
        );
        let seen = log.lock().clone();
        let cancel = seen.iter().find(|s| s.method == "CancelTask").unwrap();
        assert_eq!(cancel.header("authorization"), "Bearer tok-frank");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "provider called across the send and the cleanup"
        );
    }

    // parity: adapted: the provider reads the scope with current_credential_scope(); concurrent invocations are tokio tasks on a multi-thread runtime
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_remote_agent_auth_concurrent_invocations() {
        let log: Log = Arc::default();
        let executor = MockExecutor {
            execute: Some(Arc::new(|ctx: a2a_server::ExecutorContext| -> A2AStream {
                let mut msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text("ok")]);
                msg.task_id = Some(ctx.task_id.clone());
                msg.context_id = Some(ctx.context_id.clone());
                futures::stream::iter(vec![Ok(a2a::StreamResponse::Message(msg))]).boxed()
            })),
            cancel: None,
        };
        let router = recording(a2a_router(executor), Arc::clone(&log), None);
        let srv = serve(|_| router).await;
        let per_scope = bearer_provider(|_, scope| {
            let scope = scope.ok_or("no credential scope on the context")?;
            Ok(format!("tok-{}", scope.0))
        });
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(per_scope),
            ..Default::default()
        })
        .unwrap();

        let mut want = Vec::new();
        let mut tasks = Vec::new();
        for i in 0..8 {
            let user = format!("user-{i}");
            let ic = invocation_context_for("concurrent", &user, "default").await;
            want.push(format!(
                "Bearer tok-{}",
                credential_scope(ic.app_name(), ic.user_id(), ic.session_id(), "a2a").0
            ));
            let agent = Arc::clone(&agent);
            tasks.push(tokio::spawn(
                async move { run_and_collect(ic, &agent).await.1 },
            ));
        }
        for (i, task) in tasks.into_iter().enumerate() {
            let err: Option<AgentError> = task.await.unwrap();
            assert!(err.is_none(), "user {i}: {err:?}");
        }
        let mut seen: Vec<String> = log
            .lock()
            .iter()
            .map(|s| s.header("authorization"))
            .collect();
        seen.sort();
        want.sort();
        assert_eq!(
            seen, want,
            "a shared provider must not cross credentials between invocations"
        );
    }

    // parity: adapted: breaking out of Go's range is dropping the stream; its cleanup runs on a spawned task the test waits for
    #[tokio::test]
    async fn test_remote_agent_auth_attached_to_cleanup_cancel() {
        let log: Log = Arc::default();
        let router = recording(a2a_router(open_task_executor()), Arc::clone(&log), None);
        let srv = serve(|_| router).await;
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(gadk_auth::static_token("secret-token")),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context(
            "auth",
            Arc::clone(&agent),
            vec![user_hello()],
            StreamingMode::Sse,
        )
        .await;
        run_first(&agent, ic).await;
        let has_cancel = || log.lock().iter().any(|s| s.method == "CancelTask");
        assert!(
            eventually(has_cancel).await,
            "no CancelTask request reached the server"
        );
        let seen = log.lock().clone();
        let cancel = seen.iter().find(|s| s.method == "CancelTask").unwrap();
        assert_eq!(cancel.header("authorization"), "Bearer secret-token");
    }

    // parity: adapted: the cleanup callback cannot receive a context; the observable half of Go's check is whether the credential scope is in effect while it runs (with Auth, the invocation's; without, none). Its future is not tied to the invocation's cancellation either way
    #[tokio::test]
    async fn test_remote_agent_cleanup_context_type_tracks_auth() {
        for (name, auth, want) in [
            ("auth set", Some(gadk_auth::static_token("tok")), true),
            ("auth unset", None, false),
        ] {
            let srv = {
                let router = a2a_router(open_task_executor());
                serve(|_| router).await
            };
            let observed: Arc<Mutex<Option<Option<CredentialScope>>>> = Arc::default();
            let callback: crate::remoteagent::A2ARemoteTaskCleanupCallback = {
                let observed = Arc::clone(&observed);
                Arc::new(move |_card, _client, _task_info, _cause| {
                    *observed.lock() = Some(current_credential_scope());
                    Box::pin(async {})
                })
            };
            let agent = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(bearer_card(&srv.url)),
                auth,
                remote_task_cleanup_callback: Some(callback),
                ..Default::default()
            })
            .unwrap();
            let ic = invocation_context_for("cleanup", "hana", "default").await;
            run_first(&agent, ic).await;
            assert!(
                eventually(|| observed.lock().is_some()).await,
                "{name}: RemoteTaskCleanupCallback was not called"
            );
            let scope = observed.lock().clone().unwrap();
            assert_eq!(
                scope.is_some(),
                want,
                "{name}: cleanup carries a credential scope"
            );
            if let Some(scope) = scope {
                assert_eq!(scope.0, "cleanup/hana/default/a2a", "{name}");
            }
        }
    }

    // parity: adapted: Go checks authSendContext on a caller-scoped context; here the run is awaited inside a caller AUTH_SCOPE and the provider records the scope in effect when it resolves
    #[tokio::test]
    async fn test_remote_agent_auth_overwrites_a_caller_scope() {
        let (srv, _log) = serve_recording_a2a("ok").await;
        let seen: Arc<Mutex<Vec<Option<CredentialScope>>>> = Arc::default();
        let recorder = {
            let seen = Arc::clone(&seen);
            bearer_provider(move |_, scope| {
                seen.lock().push(scope);
                Ok("tok".into())
            })
        };
        let agent = new_a2a(A2AConfig {
            name: "crm".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(recorder),
            ..Default::default()
        })
        .unwrap();
        let ic = invocation_context_for("shop", "iris", "s7").await;
        let tenant = CallIdentity {
            scope: CredentialScope("one-tenant-for-everyone".into()),
            identity: None,
            auth: gadk_auth::static_token("unused"),
            card_fetch_client: None,
            cell: Arc::new(CredentialCell::new()),
        };
        let (_, err) = AUTH_SCOPE.scope(tenant, run_and_collect(ic, &agent)).await;
        assert!(err.is_none(), "{err:?}");
        let seen = seen.lock().clone();
        assert!(!seen.is_empty(), "no credential resolved");
        for scope in seen {
            assert_eq!(
                scope.map(|s| s.0).as_deref(),
                Some("shop/iris/s7/crm"),
                "this package owns the scope when Auth is set"
            );
        }
    }

    // parity: adapted: the redirect refusal surfaces through a2a-rs's transport error ("HTTP request failed: error following redirect for url (…)"), which drops the policy's "refusing redirect" source text
    #[tokio::test]
    async fn test_remote_agent_auth_refuses_a_different_hostname() {
        let hosts: Arc<Mutex<Vec<String>>> = Arc::default();
        let srv = {
            let hosts = Arc::clone(&hosts);
            serve(move |base| {
                let port = base.rsplit(':').next().unwrap().to_owned();
                axum::Router::new().fallback(move |req: Request| {
                    let hosts = Arc::clone(&hosts);
                    let port = port.clone();
                    async move {
                        let host = req
                            .headers()
                            .get(http::header::HOST)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        hosts.lock().push(host);
                        // Same scheme, same port, same listener — only the name differs.
                        let to = format!("http://localhost:{port}{}", req.uri().path());
                        (
                            StatusCode::TEMPORARY_REDIRECT,
                            [(http::header::LOCATION, to)],
                        )
                    }
                })
            })
            .await
        };
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(gadk_auth::static_token("secret-token")),
            ..Default::default()
        })
        .unwrap();
        let (_, err) = run_hello_err(&agent, StreamingMode::Sse).await;
        let hosts = hosts.lock().clone();
        for host in &hosts {
            assert!(
                !host.starts_with("localhost"),
                "the redirect was followed to {host:?} (saw {hosts:?})"
            );
        }
        let e = err.expect("want an error from the refused redirect, got none");
        assert!(
            e.contains("error following redirect"),
            "agent.Run() error = {e:?}"
        );
    }

    /// Counts WARN events whose message contains `needle` (Go `countingHandler`), on the thread
    /// it is the default subscriber of.
    struct WarnCounter {
        needle: &'static str,
        count: Arc<AtomicUsize>,
        next_id: AtomicUsize,
    }

    struct MessageVisitor(String);

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl tracing::Subscriber for WarnCounter {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(self.next_id.fetch_add(1, Ordering::SeqCst) as u64 + 1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }
            let mut v = MessageVisitor(String::new());
            event.record(&mut v);
            if v.0.contains(self.needle) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    // parity: adapted: Go attaches a counting slog handler to the invocation context; here a counting tracing subscriber is the default on the test's (current-thread) runtime thread
    #[tokio::test]
    async fn test_remote_agent_auth_warns_on_cleartext_interface() {
        let (srv, _log) = serve_recording_a2a("ok").await;
        let cases: [(&str, String, Option<&str>, bool, usize); 5] = [
            ("loopback is not reported", srv.url.clone(), None, false, 0),
            (
                "non-loopback http is reported once",
                "http://remote.invalid:8080".into(),
                None,
                false,
                1,
            ),
            (
                "each cleartext interface is reported",
                "http://remote.invalid:8080".into(),
                Some("http://other.invalid:8080"),
                false,
                2,
            ),
            (
                "https is not reported",
                "https://remote.invalid".into(),
                None,
                false,
                0,
            ),
            (
                "auth unset stays silent",
                "http://remote.invalid:8080".into(),
                None,
                true,
                0,
            ),
        ];
        for (name, url, second, no_auth, want) in cases {
            let mut card = bearer_card(&url);
            if let Some(second) = second {
                let mut iface = card.supported_interfaces[0].clone();
                iface.url = second.to_owned();
                card.supported_interfaces.push(iface);
            }
            let agent = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(card),
                auth: (!no_auth).then(|| gadk_auth::static_token("secret-token")),
                ..Default::default()
            })
            .unwrap();
            let count = Arc::new(AtomicUsize::new(0));
            let dispatch = tracing::Dispatch::new(WarnCounter {
                needle: "will be sent in cleartext",
                count: Arc::clone(&count),
                next_id: AtomicUsize::new(0),
            });
            let _guard = tracing::dispatcher::set_default(&dispatch);
            // Three times: "warn once" must not be "warn every time but the first". The
            // unreachable hosts make the run fail; the warning comes before the first request.
            for _ in 0..3 {
                let ic = invocation_context(
                    "auth",
                    Arc::clone(&agent),
                    vec![user_hello()],
                    StreamingMode::Sse,
                )
                .await;
                let _ = run_and_collect(ic, &agent).await;
            }
            assert_eq!(
                count.load(Ordering::SeqCst),
                want,
                "{name}: cleartext warnings over 3 invocations"
            );
        }
    }

    /// Go `applyFunc`: a credential whose `apply` is `f`.
    struct ApplyFn(Box<dyn Fn(&mut HeaderMap) -> Result<(), gadk_auth::Error> + Send + Sync>);

    impl std::fmt::Debug for ApplyFn {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ApplyFn")
        }
    }

    #[async_trait::async_trait]
    impl Credential for ApplyFn {
        async fn apply(&self, headers: &mut HeaderMap) -> Result<(), gadk_auth::Error> {
            (self.0)(headers)
        }
    }

    // parity: adapted: Go's pointer-credential case is the same bearer credential (Rust has no pointer twin of a credential type)
    #[tokio::test]
    async fn test_remote_agent_auth_credential_decides_placement() {
        let bearer = |t: &str| -> Arc<dyn Credential> { Arc::new(Bearer { token: t.into() }) };
        let oauth2: Arc<dyn Credential> = Arc::new(OAuth2 {
            token_source: Arc::new(StaticTokenSource(Token {
                access_token: "at".into(),
                token_type: String::new(),
            })),
        });
        type CardFn = fn(&str) -> a2a::AgentCard;
        let cases: Vec<(&str, CardFn, Arc<dyn Credential>, Vec<(&str, &str)>)> = vec![
            (
                "api key goes in the header the credential names",
                api_key_card,
                Arc::new(ApiKey {
                    name: "X-Caller-Key".into(),
                    value: "secret".into(),
                }),
                vec![
                    ("X-Caller-Key", "secret"),
                    ("X-Card-Key", ""),
                    ("Authorization", ""),
                ],
            ),
            (
                "bearer is sent although the card asks for an api key",
                api_key_card,
                bearer("tok"),
                vec![("Authorization", "Bearer tok"), ("X-Card-Key", "")],
            ),
            (
                "basic works",
                api_key_card,
                Arc::new(Basic {
                    username: "u".into(),
                    password: "p".into(),
                }),
                vec![("Authorization", "Basic dTpw")],
            ),
            (
                "extra headers ride along",
                api_key_card,
                gadk_auth::with_headers(
                    bearer("tok"),
                    BTreeMap::from([("X-Goog-User-Project".to_owned(), "proj".to_owned())]),
                ),
                vec![
                    ("Authorization", "Bearer tok"),
                    ("X-Goog-User-Project", "proj"),
                ],
            ),
            (
                "oauth2 is minted and sent as a bearer token",
                api_key_card,
                oauth2,
                vec![("Authorization", "Bearer at")],
            ),
            (
                "a card that declares no security still gets the credential",
                plain_card,
                bearer("tok"),
                vec![("Authorization", "Bearer tok")],
            ),
        ];
        for (name, card, cred, want) in cases {
            let (srv, log) = serve_recording_a2a("ok").await;
            let agent = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(card(&srv.url)),
                auth: Some(provider_of(cred)),
                ..Default::default()
            })
            .unwrap();
            run_hello(&agent, StreamingMode::Sse).await;
            let seen = log.lock().last().cloned().expect("a request");
            for (header, value) in want {
                assert_eq!(seen.header(header), value, "{name}: {header}");
            }
        }
    }

    // parity: adapted: a Rust provider cannot return a nil or typed-nil credential (it returns Arc<dyn Credential>), so Go's two "nil credential" rows have no counterpart
    #[tokio::test]
    async fn test_remote_agent_auth_fails_closed() {
        struct FailingSource;
        #[async_trait::async_trait]
        impl TokenSource for FailingSource {
            async fn token(&self) -> Result<Token, gadk_auth::Error> {
                Err(gadk_auth::Error::msg("token endpoint refused"))
            }
        }
        let failing = |err: fn() -> gadk_auth::Error| -> Arc<dyn CredentialProvider> {
            ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
                Box::pin(async move { Err(err()) })
            })
            .into_provider()
        };
        let cases: Vec<(&str, Arc<dyn CredentialProvider>, &str, Option<&str>)> = vec![
            (
                "provider error",
                failing(|| gadk_auth::Error::other(ResolveFailed)),
                "resolve auth credential",
                None,
            ),
            (
                "consent required",
                failing(|| gadk_auth::Error::Consent(gadk_auth::ConsentRequiredError::default())),
                "resolve auth credential",
                None,
            ),
            (
                "credential that cannot apply",
                gadk_auth::static_token(""),
                "apply auth credential",
                None,
            ),
            (
                "oauth2 mint failure",
                gadk_auth::token_source(Arc::new(FailingSource)),
                "mint oauth2 token",
                None,
            ),
            (
                "oauth2 non-bearer token",
                gadk_auth::token_source(Arc::new(StaticTokenSource(Token {
                    access_token: "super-secret".into(),
                    token_type: "mac".into(),
                }))),
                "cannot be sent as a bearer token",
                Some("super-secret"),
            ),
        ];
        for (name, provider, want_err, absent) in cases {
            let (srv, log) = serve_recording_a2a("ok").await;
            let agent = new_a2a(A2AConfig {
                name: "a2a".into(),
                agent_card: Some(bearer_card(&srv.url)),
                auth: Some(provider),
                ..Default::default()
            })
            .unwrap();
            let (_, err) = run_hello_err(&agent, StreamingMode::Sse).await;
            assert_eq!(
                log.lock().len(),
                0,
                "{name}: a credential failure must stop the request"
            );
            let e = err.unwrap_or_else(|| {
                panic!("{name}: want an error explaining the failure, got none")
            });
            assert!(e.contains(want_err), "{name}: agent.Run() error = {e:?}");
            if let Some(absent) = absent {
                assert!(!e.contains(absent), "{name}: leaked {absent}");
            }
        }
    }

    /// The card-and-invoke server of Go's card-fetch tests: the card at the well-known path names
    /// `<base>/invoke`, where a replay of one `ok` message is served.
    async fn serve_card_and_invoke(log: Log) -> Server {
        serve(move |base| {
            let card = serde_json::json!({
                "name": "",
                "version": "",
                "supportedInterfaces": [
                    {"url": format!("{base}/invoke"), "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}
                ],
                "capabilities": {"streaming": true},
            });
            let msg = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text("ok")]);
            let router = axum::Router::new()
                .nest(
                    "/invoke",
                    a2a_router(a2a_event_replay(vec![a2a::StreamResponse::Message(msg)])),
                )
                .route(
                    "/.well-known/agent-card.json",
                    axum::routing::get(move || {
                        let card = card.clone();
                        async move { axum::Json(card) }
                    }),
                );
            recording(router, log, None)
        })
        .await
    }

    #[tokio::test]
    async fn test_remote_agent_auth_covers_the_card_fetch_once() {
        let log: Log = Arc::default();
        let srv = serve_card_and_invoke(Arc::clone(&log)).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = {
            let calls = Arc::clone(&calls);
            bearer_provider(move |_, _| {
                Ok(format!("tok-{}", calls.fetch_add(1, Ordering::SeqCst) + 1))
            })
        };
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card_provider: Some(new_agent_card_provider(
                &srv.url,
                CardResolveOptions::default(),
            )),
            auth: Some(provider),
            ..Default::default()
        })
        .unwrap();
        let events = run_hello(&agent, StreamingMode::Sse).await;
        assert!(first_error_event(&events).is_none(), "{events:?}");
        let seen = log.lock().clone();
        for path in ["/.well-known/agent-card.json", "/invoke"] {
            let s = seen
                .iter()
                .find(|s| s.path == path)
                .unwrap_or_else(|| panic!("no request to {path} (saw {seen:?})"));
            assert_eq!(s.header("authorization"), "Bearer tok-1", "{path}");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "provider calls in one invocation"
        );
    }

    #[tokio::test]
    async fn test_remote_agent_auth_refuses_an_insecure_card_source() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = {
            let calls = Arc::clone(&calls);
            bearer_provider(move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok("tok".into())
            })
        };
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            // Nothing listens here; a fetch attempt would fail with a connection error rather
            // than the refusal under test.
            agent_card_provider: Some(new_agent_card_provider(
                "http://remote.invalid",
                CardResolveOptions::default(),
            )),
            auth: Some(provider),
            ..Default::default()
        })
        .unwrap();
        let (_, err) = run_hello_err(&agent, StreamingMode::Sse).await;
        assert!(
            err.as_ref().is_some_and(|e| e.contains("must use https")),
            "agent.Run() error = {err:?}, want the insecure card source refused"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "nothing resolved for a refused fetch"
        );
    }

    // parity: adapted: breaking out of Go's range is dropping the stream; the test waits for the spawned cleanup to have tried the credential, then for any CancelTask to land
    #[tokio::test]
    async fn test_remote_agent_auth_cleanup_failure_sends_no_cancel() {
        let log: Log = Arc::default();
        let router = recording(a2a_router(open_task_executor()), Arc::clone(&log), None);
        let srv = serve(|_| router).await;
        let applies = Arc::new(AtomicUsize::new(0));
        let cred: Arc<dyn Credential> = {
            let applies = Arc::clone(&applies);
            Arc::new(ApplyFn(Box::new(move |h: &mut HeaderMap| {
                if applies.fetch_add(1, Ordering::SeqCst) + 1 > 1 {
                    return Err(gadk_auth::Error::other(ResolveFailed));
                }
                h.insert(http::header::AUTHORIZATION, "Bearer tok".parse().unwrap());
                Ok(())
            })))
        };
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card: Some(bearer_card(&srv.url)),
            auth: Some(provider_of(cred)),
            ..Default::default()
        })
        .unwrap();
        run_first(
            &agent,
            invocation_context_for("app", "ivan", "default").await,
        )
        .await;
        assert!(
            eventually(|| applies.load(Ordering::SeqCst) >= 2).await,
            "the cleanup never tried the credential"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        let seen = log.lock().clone();
        let count = |m: &str| seen.iter().filter(|s| s.method == m).count();
        assert_eq!(count("SendStreamingMessage"), 1, "{seen:?}");
        assert_eq!(
            count("CancelTask"),
            0,
            "without a credential the cancel must not go out"
        );
    }

    // parity: adapted: the provider reads the identity from AuthContext.identity (Go agent.IdentityFromContext)
    #[tokio::test]
    async fn test_remote_agent_auth_provider_sees_the_identity() {
        let log: Log = Arc::default();
        let srv = serve_card_and_invoke(Arc::clone(&log)).await;
        let provider = bearer_provider(|identity, _| {
            let id = identity.ok_or("no identity")?;
            Ok(format!("{}:{}:{}", id.app_name, id.user_id, id.session_id))
        });
        let agent = new_a2a(A2AConfig {
            name: "a2a".into(),
            agent_card_provider: Some(new_agent_card_provider(
                &srv.url,
                CardResolveOptions::default(),
            )),
            auth: Some(provider),
            ..Default::default()
        })
        .unwrap();
        let (_, err) =
            run_and_collect(invocation_context_for("shop", "ivy", "s3").await, &agent).await;
        assert!(err.is_none(), "{err:?}");
        let seen = log.lock().clone();
        assert!(
            seen.len() >= 2,
            "want the card fetch and the send, saw {seen:?}"
        );
        for (i, s) in seen.iter().enumerate() {
            assert_eq!(
                s.header("authorization"),
                "Bearer shop:ivy:s3",
                "request {i}"
            );
        }
    }
}
