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

//! Ports of ADK-Go `server/adka2a/v2/cancel_auth_test.go`: the credential scope of the
//! executor's child-task cancel (`Executor::cancel_child_input_required_tasks`). Go's
//! `scopedTransport` / caller interceptor become a2a-rs call interceptors and credential providers
//! that read [`current_credential_scope`]; the remote is an a2a-rs JSON-RPC server answering only
//! `CancelTask`, behind a recorder of each request's JSON-RPC method and `Authorization` header.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, AgentState, AgentType, new_with_state};
use gadk_auth::{AuthContext, Credential, CredentialProvider};
use gadk_genai::{Content, FunctionCall, Part, ROLE_MODEL};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use parking_lot::Mutex;

use super::agent_remoteagent_v2_a2a_agent::harness::{
    MockExecutor, TestA2AServer, jsonrpc_card, serve_router,
};
use crate::Error;
use crate::convert::{to_a2a_parts, to_custom_metadata};
use crate::remoteagent::auth::{
    AUTH_SCOPE, CallIdentity, CredentialCell, CredentialScope, credential_scope,
    current_credential_scope,
};
use crate::remoteagent::client::auth_client_factory;
use crate::remoteagent::{
    A2AClient, A2AClientProvider, A2AServerConfig, ClientContext, RemoteAgentState,
    new_a2a_client_provider,
};
use crate::server::executor::find_remote_subagents;
use crate::server::{Executor, ExecutorConfig, RunnerConfig};

const APP_NAME: &str = "app";
const AGENT_NAME: &str = "remote";
const CONTEXT_ID: &str = "ctx-1";

/// The JSON-RPC method and `Authorization` header of every request the remote received.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// Go `cancelOnlyExecutor` behind a2a-rs's JSON-RPC router, wrapped by Go's request recorder
/// (`peekJSONRPCMethod`).
async fn cancel_only_server() -> (TestA2AServer, Seen) {
    let seen: Seen = Arc::default();
    let handler = Arc::new(a2a_server::DefaultRequestHandler::new(
        MockExecutor {
            execute: Some(Arc::new(|_ctx| Box::pin(futures::stream::empty()))),
            cancel: None,
        },
        a2a_server::InMemoryTaskStore::new(),
    ));
    let router = a2a_server::jsonrpc::jsonrpc_router(handler).layer(axum::middleware::from_fn({
        let seen = Arc::clone(&seen);
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let seen = Arc::clone(&seen);
            async move {
                let (parts, body) = req.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let method = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                let auth = parts
                    .headers
                    .get(http::header::AUTHORIZATION)
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                seen.lock().push((method, auth));
                let req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
                next.run(req).await
            }
        }
    }));
    (serve_router(router).await, seen)
}

/// The remote agent's card (Go: a bearer security scheme; a2a-rs clients ignore it, as a2a-go's
/// do, so only the interface matters here).
fn card(url: &str) -> a2a::AgentCard {
    let mut card = jsonrpc_card(url);
    card.name = AGENT_NAME.into();
    card
}

/// Go `newRemoteStateAgent`: an agent carrying the remote-agent state `find_remote_subagents`
/// looks for.
fn new_remote_state_agent(name: &str, remote_cfg: A2AServerConfig) -> Arc<dyn Agent> {
    new_with_state(
        AgentConfig {
            name: name.into(),
            run: Some(Arc::new(|_ic| Box::pin(futures::stream::empty()))),
            ..Default::default()
        },
        AgentState {
            agent_type: AgentType::RemoteAgent,
            config: Some(Arc::new(RemoteAgentState {
                a2a: Arc::new(remote_cfg),
            })),
            on_before_agent_callback_event: None,
            live: None,
        },
    )
    .unwrap()
}

/// The session (`A2A_USER_<ctx>`/`<ctx>`) holding one remote-authored event per `(call, task)`
/// seed, and the input-required status whose message holds those calls.
async fn seeded_session(seeds: &[(&str, &str)]) -> (Arc<dyn SessionService>, a2a::TaskStatus) {
    let svc = Arc::new(InMemorySessionService::new());
    let session = svc
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: format!("A2A_USER_{CONTEXT_ID}"),
            session_id: Some(CONTEXT_ID.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut status_parts = Vec::new();
    for (call_id, task_id) in seeds {
        let mut event = Event::new(&Platform::default(), "invocation");
        event.author = AGENT_NAME.into();
        event.response.content = Some(Content::from_parts(
            vec![Part {
                function_call: Some(FunctionCall {
                    id: (*call_id).into(),
                    name: "ask".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ROLE_MODEL,
        ));
        event.response.custom_metadata = to_custom_metadata(task_id, CONTEXT_ID);
        svc.append_event(&*session, &mut event).await.unwrap();
        status_parts.extend(to_a2a_parts(&event.response.content.unwrap().parts, &[]).unwrap());
    }
    let status = a2a::TaskStatus {
        state: a2a::TaskState::InputRequired,
        message: Some(a2a::Message::new(a2a::Role::Agent, status_parts)),
        timestamp: None,
    };
    (svc, status)
}

/// Go `&a2asrv.ExecutorContext{ContextID: contextID}`.
fn request_context() -> a2a_server::ExecutorContext {
    a2a_server::ExecutorContext {
        message: None,
        task_id: String::new(),
        stored_task: None,
        context_id: CONTEXT_ID.into(),
        metadata: None,
        user: None,
        service_params: HashMap::new(),
        tenant: None,
    }
}

/// Runs the executor's child-task cancel over `agent` and `svc` (Go
/// `e.cancelChildInputRequiredTasks(ctx, reqCtx, status, cfg, findRemoteSubagents(cfg.Agent))`);
/// its result is ignored as in Go (the stub remote has no such task).
async fn cancel_children(
    agent: Arc<dyn Agent>,
    svc: Arc<dyn SessionService>,
    status: &a2a::TaskStatus,
) -> usize {
    let cfg = RunnerConfig {
        app_name: APP_NAME.into(),
        agent: Arc::clone(&agent),
        session_service: svc,
    };
    let subagents = find_remote_subagents(&agent);
    let _ = Executor::new(ExecutorConfig::default())
        .cancel_child_input_required_tasks(&request_context(), status, &cfg, &subagents)
        .await;
    subagents.len()
}

/// Stands in for Go's `scopedTransport`: the credential is `Bearer <token>` only when the call
/// runs in `scope` (the scope the executor attaches and the client carries down to the request),
/// so a header reaching the remote proves that propagation.
struct ScopedToken {
    scope: CredentialScope,
    token: &'static str,
}

#[async_trait::async_trait]
impl CredentialProvider for ScopedToken {
    async fn credential(&self, cx: &AuthContext) -> Result<Arc<dyn Credential>, gadk_auth::Error> {
        let token = if current_credential_scope().as_ref() == Some(&self.scope) {
            self.token
        } else {
            "wrong-scope"
        };
        gadk_auth::static_token(token).credential(cx).await
    }
}

/// A client provider that refuses to build a client outside a credential scope (Go: "the
/// provider is entitled to the same scope its client gets").
struct ScopeRequiringProvider(Arc<dyn A2AClientProvider>);

#[async_trait::async_trait]
impl A2AClientProvider for ScopeRequiringProvider {
    async fn create_client(
        &self,
        cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error> {
        if current_credential_scope().is_none() {
            return Err(Error::msg(
                "no credential scope on the context handed to ClientProvider",
            ));
        }
        self.0.create_client(cx, card).await
    }
}

// parity: adapted: Go's scopedTransport (keyed on the context's session ID) is a credential provider keyed on the current AUTH_SCOPE behind the remote agent's own auth client factory; A2AServerConfig.auth is set because gadk's scope means "apply this provider's credential"
#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_child_input_required_tasks_authenticates_cancel() {
    const TOKEN: &str = "scoped-token";
    let (srv, seen) = cancel_only_server().await;
    let user_id = format!("A2A_USER_{CONTEXT_ID}");
    let remote_cfg = A2AServerConfig {
        agent_card: Some(card(&srv.url)),
        agent_card_provider: None,
        // Stands in for the provider new_a2a builds for itself when auth is set, which is the
        // only way owns_auth_scope is ever true in production.
        client_provider: Arc::new(ScopeRequiringProvider(new_a2a_client_provider(Some(
            Arc::new(auth_client_factory()),
        )))),
        owns_auth_scope: true,
        card_fetch_client: None,
        auth: Some(Arc::new(ScopedToken {
            scope: credential_scope(APP_NAME, &user_id, CONTEXT_ID, AGENT_NAME),
            token: TOKEN,
        })),
    };
    // Two events the cancel scan matches on, each with its own pending call and remote task:
    // the second cancel goes through the cached client.
    let (svc, status) = seeded_session(&[("call-1", "task-1"), ("call-2", "task-2")]).await;
    let agent = new_remote_state_agent(AGENT_NAME, remote_cfg);
    assert_eq!(
        cancel_children(agent, svc, &status).await,
        1,
        "remote subagents"
    );

    let cancels: Vec<String> = seen
        .lock()
        .iter()
        .filter(|(m, _)| m == "CancelTask")
        .map(|(_, a)| a.clone())
        .collect();
    assert_eq!(cancels.len(), 2, "CancelTask requests (saw {cancels:?})");
    for (i, got) in cancels.iter().enumerate() {
        assert_eq!(
            got,
            &format!("Bearer {TOKEN}"),
            "CancelTask #{} Authorization",
            i + 1
        );
    }
}

/// The caller's own interceptor (Go `a2aclient.AuthInterceptor` over a credentials store keyed
/// on the caller's session ID): `Bearer <token>` only inside the caller's scope.
struct CallerInterceptor {
    scope: CredentialScope,
    token: &'static str,
}

#[async_trait::async_trait]
impl a2a_client::middleware::CallInterceptor for CallerInterceptor {
    async fn before(
        &self,
        _method: &str,
        params: &mut a2a_client::ServiceParams,
    ) -> Result<(), a2a::A2AError> {
        if current_credential_scope().as_ref() == Some(&self.scope) {
            params.insert(
                "authorization".into(),
                vec![format!("Bearer {}", self.token)],
            );
        }
        Ok(())
    }
}

// parity: adapted: Go's caller scope is a session ID attached to the context; here it is an enclosing AUTH_SCOPE the caller's own interceptor reads
#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_child_input_required_tasks_keeps_caller_scope() {
    const TOKEN: &str = "caller-token";
    let (srv, seen) = cancel_only_server().await;
    let caller_scope = CredentialScope("caller-tenant".into());
    let factory = a2a_client::A2AClientFactory::builder()
        .register(Arc::new(a2a_client::jsonrpc::JsonRpcTransportFactory::new(
            None,
        )))
        .with_interceptor(Arc::new(CallerInterceptor {
            scope: caller_scope.clone(),
            token: TOKEN,
        }))
        .build();
    // owns_auth_scope stays false: this is the caller's own interceptor.
    let remote_cfg = A2AServerConfig {
        agent_card: Some(card(&srv.url)),
        agent_card_provider: None,
        client_provider: new_a2a_client_provider(Some(Arc::new(factory))),
        owns_auth_scope: false,
        card_fetch_client: None,
        auth: None,
    };
    let (svc, status) = seeded_session(&[("call-1", "task-1")]).await;
    let agent = new_remote_state_agent(AGENT_NAME, remote_cfg);
    let caller = CallIdentity {
        scope: caller_scope,
        identity: None,
        auth: gadk_auth::static_token("unused"),
        card_fetch_client: None,
        cell: Arc::new(CredentialCell::new()),
    };
    AUTH_SCOPE
        .scope(caller, cancel_children(agent, svc, &status))
        .await;

    let seen = seen.lock().clone();
    let got = seen
        .iter()
        .find(|(m, _)| m == "CancelTask")
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| panic!("no CancelTask reached the remote subagent (saw {seen:?})"));
    assert_eq!(
        got,
        format!("Bearer {TOKEN}"),
        "CancelTask Authorization; the caller's own scope must survive"
    );
}

/// Records what a custom provider was handed.
struct RecordingProvider {
    inner: Arc<dyn A2AClientProvider>,
    saw_invocation: Arc<Mutex<bool>>,
    saw_scope: Arc<Mutex<bool>>,
}

#[async_trait::async_trait]
impl A2AClientProvider for RecordingProvider {
    async fn create_client(
        &self,
        cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error> {
        *self.saw_invocation.lock() = matches!(cx, ClientContext::Invocation(_));
        *self.saw_scope.lock() = current_credential_scope().is_some();
        self.inner.create_client(cx, card).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_child_input_required_tasks_custom_provider_context() {
    let (srv, seen) = cancel_only_server().await;
    let (saw_invocation, saw_scope) = (Arc::new(Mutex::new(false)), Arc::new(Mutex::new(false)));
    // A custom provider: owns_auth_scope stays false, as new_a2a leaves it.
    let remote_cfg = A2AServerConfig {
        agent_card: Some(card(&srv.url)),
        agent_card_provider: None,
        client_provider: Arc::new(RecordingProvider {
            inner: new_a2a_client_provider(None),
            saw_invocation: Arc::clone(&saw_invocation),
            saw_scope: Arc::clone(&saw_scope),
        }),
        owns_auth_scope: false,
        card_fetch_client: None,
        auth: None,
    };
    let (svc, status) = seeded_session(&[("call-1", "task-1")]).await;
    let agent = new_remote_state_agent(AGENT_NAME, remote_cfg);
    cancel_children(agent, svc, &status).await;

    assert!(
        !*saw_invocation.lock(),
        "the provider got an invocation context; the docs that tell callers to check it here are out of date"
    );
    assert!(
        !*saw_scope.lock(),
        "the provider got a credential scope without auth ownership; a caller's own interceptor could be fed a key it never chose"
    );
    let cancels = seen
        .lock()
        .iter()
        .filter(|(m, _)| m == "CancelTask")
        .count();
    assert_eq!(cancels, 1, "CancelTask reached the remote");
}
