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

//! The remote A2A agent (port of ADK-Go `agent/remoteagent/v2/a2a_agent.go`: `A2AConfig`,
//! `NewA2A`, the run loop, `newMessage`, `toErrorEvent`, `cleanupRemoteTask`).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_agent::{
    AfterAgentCallback, Agent, AgentConfig, AgentError, AgentRunFn, AgentState, AgentType,
    BeforeAgentCallback, Context, EventStream, InvocationContext, StreamingMode,
};
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use super::auth::{
    CallIdentity, CredentialCell, card_fetch_http_client, card_sends_in_clear, credential_scope,
    with_auth_scope,
};
use super::card::AgentCardProvider;
use super::client::{
    A2AClient, A2AClientProvider, ClientContext, auth_client_factory, new_a2a_client_provider,
};
use super::config::{A2AServerConfig, RemoteAgentState, resolve_agent_card};
use super::history::{
    convert_parts, get_user_function_call_at, has_isolation_scope_history,
    to_missing_remote_session_parts,
};
use super::run_processor::{RunProcessor, converted_view};
use crate::Error;
use crate::convert::{
    A2APartConverter, GenAIPartConverter, new_remote_agent_event, stream_meta, stream_task_info,
    to_adk_meta_key, transfer_to_agent_from_meta,
};

/// Go `BeforeA2ARequestCallback`: an event short-circuits the request.
pub type BeforeA2ARequestCallback = Arc<
    dyn Fn(&Context, &a2a::SendMessageRequest) -> BoxFuture<'static, Result<Option<Event>, Error>>
        + Send
        + Sync,
>;

/// Go `A2AEventConverter`: converts a remote response (or transport error) to a session event.
pub type A2AEventConverter = Arc<
    dyn Fn(
            &InvocationContext,
            &a2a::SendMessageRequest,
            Result<&a2a::StreamResponse, &a2a::A2AError>,
        ) -> BoxFuture<'static, Result<Option<Event>, Error>>
        + Send
        + Sync,
>;

/// Go `AfterA2ARequestCallback`: an event replaces the converted one.
pub type AfterA2ARequestCallback = Arc<
    dyn Fn(
            &Context,
            &a2a::SendMessageRequest,
            Option<&Event>,
            Option<&a2a::A2AError>,
        ) -> BoxFuture<'static, Result<Option<Event>, Error>>
        + Send
        + Sync,
>;

/// Go `A2ARemoteTaskCleanupCallback`: runs after an abandoned or failed remote task was
/// cancelled; `task_info` is `(task_id, context_id)`.
pub type A2ARemoteTaskCleanupCallback = Arc<
    dyn Fn(
            &a2a::AgentCard,
            Arc<dyn A2AClient>,
            (String, String),
            Option<&Error>,
        ) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// Go `A2AConfig`.
#[derive(Clone, Default)]
pub struct A2AConfig {
    /// The agent name.
    pub name: String,
    /// The agent description.
    pub description: String,
    /// A fixed card (takes precedence over `agent_card_provider`).
    pub agent_card: Option<a2a::AgentCard>,
    /// Resolves the card on every invocation.
    pub agent_card_provider: Option<AgentCardProvider>,
    /// Go `BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<BeforeAgentCallback>,
    /// Go `BeforeRequestCallbacks`.
    pub before_request_callbacks: Vec<BeforeA2ARequestCallback>,
    /// Go `Converter`; `None` → `to_session_event_with_parts`.
    pub converter: Option<A2AEventConverter>,
    /// Go `AfterRequestCallbacks`.
    pub after_request_callbacks: Vec<AfterA2ARequestCallback>,
    /// Go `AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<AfterAgentCallback>,
    /// Go `AllowTransferToAgent`.
    pub allow_transfer_to_agent: bool,
    /// Go `A2APartConverter`.
    pub a2a_part_converter: Option<A2APartConverter>,
    /// Go `GenAIPartConverter`.
    pub genai_part_converter: Option<GenAIPartConverter>,
    /// Go `ClientProvider`; `None` → `new_a2a_client_provider(None)`.
    pub client_provider: Option<Arc<dyn A2AClientProvider>>,
    /// Go `Auth`: the credential every A2A call and the card fetch carry.
    pub auth: Option<Arc<dyn gadk_auth::CredentialProvider>>,
    /// Go `MessageSendConfig`.
    pub message_send_config: Option<a2a::SendMessageConfiguration>,
    /// Go `RemoteTaskCleanupCallback`.
    pub remote_task_cleanup_callback: Option<A2ARemoteTaskCleanupCallback>,
}

/// Go `NewA2A`'s error when `Auth` and a custom `ClientProvider` are both set (verbatim).
const AUTH_WITH_CLIENT_PROVIDER: &str = "A2AConfig.Auth cannot be combined with a custom ClientProvider; wire the credential into your ClientProvider instead. When this agent runs, the context the provider receives is the ADK invocation context: compute remoteagent.CredentialScope(ctx.Session(), name) there and have the client it returns attach it with a2aclient.AttachSessionID on every call, the cleanup CancelTask included. An adka2a server cancelling an abandoned child task calls the provider with its own request context instead, which is not an agent.InvocationContext, so check the assertion. See A2AConfig.ClientProvider";

/// Go `context.Canceled`'s text: the response error a cancelled invocation ends with.
const CONTEXT_CANCELED: &str = "context canceled";

/// Go `cleanupTimeout`: bounds the cleanup `CancelTask`, credential resolution included.
pub(crate) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Go `NewA2A`: an agent of type `RemoteAgent` backed by the remote A2A agent.
///
/// Without `client_provider`, the client comes from [`new_a2a_client_provider`]: a2a-go's default
/// factory, or, with `auth` set, one over the auth-aware HTTP client with the credential
/// interceptor (Go passes `authHTTPClient` to the factory).
pub fn new_a2a(cfg: A2AConfig) -> Result<Arc<dyn Agent>, Error> {
    if cfg.agent_card.is_none() && cfg.agent_card_provider.is_none() {
        return Err(Error::msg(
            "either AgentCard or AgentCardProvider must be provided",
        ));
    }
    if cfg.auth.is_some() && cfg.client_provider.is_some() {
        return Err(Error::msg(AUTH_WITH_CLIENT_PROVIDER));
    }
    let mut card_client = None;
    let client_provider = match &cfg.client_provider {
        Some(provider) => Arc::clone(provider),
        None if cfg.auth.is_some() => {
            // Go `cardFetchHTTPClient(authHTTPClient(cfg.Auth))`: the card fetch shares the RPC
            // client's redirect policy, with its own timeout.
            card_client = Some(card_fetch_http_client());
            new_a2a_client_provider(Some(Arc::new(auth_client_factory())))
        }
        None => new_a2a_client_provider(None),
    };

    let server_config = Arc::new(A2AServerConfig {
        agent_card: cfg.agent_card.clone(),
        agent_card_provider: cfg.agent_card_provider.clone(),
        client_provider,
        owns_auth_scope: cfg.auth.is_some(),
        card_fetch_client: card_client.clone(),
        auth: cfg.auth.clone(),
    });
    let agent_config = AgentConfig {
        name: cfg.name.clone(),
        description: cfg.description.clone(),
        sub_agents: Vec::new(),
        before_agent_callbacks: cfg.before_agent_callbacks.clone(),
        after_agent_callbacks: cfg.after_agent_callbacks.clone(),
        run: None,
    };
    let remote = Arc::new(RemoteAgent {
        cfg: Arc::new(cfg),
        server_config: Arc::clone(&server_config),
        card_client,
        warned_cleartext: Mutex::new(HashSet::new()),
    });
    let run: AgentRunFn = Arc::new(move |ctx| Arc::clone(&remote).run(ctx));
    let agent = gadk_agent::new_with_state(
        AgentConfig {
            run: Some(run),
            ..agent_config
        },
        AgentState {
            agent_type: AgentType::RemoteAgent,
            config: Some(Arc::new(RemoteAgentState { a2a: server_config })),
            on_before_agent_callback_event: None,
            live: None,
        },
    )?;
    Ok(agent)
}

/// Go `a2aAgent`.
struct RemoteAgent {
    cfg: Arc<A2AConfig>,
    server_config: Arc<A2AServerConfig>,
    /// Applies `A2AConfig.auth` to a card fetch; `None` when `auth` is unset.
    card_client: Option<reqwest::Client>,
    /// The `http://` interfaces already reported, so each is reported once per agent rather than
    /// once per invocation, and a card that later names a different one is reported again.
    warned_cleartext: Mutex<HashSet<String>>,
}

impl RemoteAgent {
    /// Go `authSendContext`: with `auth` set, the scope every outgoing call of one invocation
    /// runs in (the card fetch, the send, the cleanup cancel), with a fresh credential cell so
    /// the credential is resolved once per invocation. `None` without `auth`: nothing is
    /// attached, and any scope the caller is in stays in effect.
    fn call_identity(&self, ctx: &InvocationContext) -> Option<CallIdentity> {
        let auth = self.cfg.auth.clone()?;
        let identity = ctx.identity();
        Some(CallIdentity {
            scope: credential_scope(
                &identity.app_name,
                &identity.user_id,
                &identity.session_id,
                &self.cfg.name,
            ),
            identity: Some(identity),
            auth,
            card_fetch_client: self.card_client.clone(),
            cell: Arc::new(CredentialCell::new()),
        })
    }

    /// Go `a2aAgent.run`.
    fn run(self: Arc<Self>, ctx: InvocationContext) -> EventStream {
        Box::pin(async_stream::stream! {
            let cfg = Arc::clone(&self.cfg);
            let id = self.call_identity(&ctx);
            let card = match with_auth_scope(id.clone(), resolve_agent_card(&self.server_config)).await {
                Ok(card) => card,
                Err(err) => {
                    yield Ok(to_error_event(&ctx, &format!("agent card resolution failed: {err}")));
                    return;
                }
            };
            if cfg.auth.is_some() {
                // Only a fetched card is checked at resolution time, so a static, file-sourced or
                // caller-provided one reaches here unvalidated. Once per interface.
                for iface in card_sends_in_clear(&card) {
                    if self.warned_cleartext.lock().insert(iface.clone()) {
                        tracing::warn!(
                            agent = %cfg.name,
                            interface = %iface,
                            "a2a auth: the agent card names a non-loopback http interface, so the credential will be sent in cleartext"
                        );
                    }
                }
            }

            let client = match with_auth_scope(
                id.clone(),
                self.server_config
                    .client_provider
                    .create_client(ClientContext::Invocation(&ctx), &card),
            )
            .await
            {
                Ok(client) => client,
                Err(err) => {
                    yield Ok(to_error_event(&ctx, &format!("sender creation failed: {err}")));
                    return;
                }
            };
            // Go `defer destroy(ctx, sender)`: the guard destroys the client when the run ends,
            // and, once armed, cleans the remote task up first; a dropped stream does both on a
            // spawned task.
            let mut guard = RunGuard::new(Arc::clone(&client), ctx.clone());

            let msg = match new_message(&ctx, &cfg).await {
                Ok(msg) => msg,
                Err(err) => {
                    yield Ok(to_error_event(&ctx, &format!("message creation failed: {err}")));
                    guard.finish().await;
                    return;
                }
            };

            let req = a2a::SendMessageRequest {
                message: msg,
                configuration: cfg.message_send_config.clone(),
                metadata: None,
                tenant: None,
            };
            let mut processor = RunProcessor::new(Arc::clone(&cfg), req.clone());

            match processor.run_before_request_callbacks(&ctx).await {
                Ok(None) => {}
                before => {
                    let (bcb_resp, bcb_err) = split(before);
                    let bcb_a2a_err = bcb_err.as_ref().map(to_a2a_error);
                    match processor
                        .run_after_request_callbacks(&ctx, bcb_resp.as_ref(), bcb_a2a_err.as_ref())
                        .await
                    {
                        Ok(Some(event)) => yield Ok(event),
                        Err(err) => yield Err(agent_error(err)),
                        Ok(None) => match (bcb_resp, bcb_err) {
                            (_, Some(err)) => yield Err(agent_error(err)),
                            (Some(event), None) => yield Ok(event),
                            (None, None) => {}
                        },
                    }
                    guard.finish().await;
                    return;
                }
            }

            if req.message.parts.is_empty() {
                let resp = new_remote_agent_event(&ctx);
                match processor.run_after_request_callbacks(&ctx, Some(&resp), None).await {
                    Ok(Some(event)) => yield Ok(event),
                    Err(err) => yield Err(agent_error(err)),
                    Ok(None) => yield Ok(resp),
                }
                guard.finish().await;
                return;
            }

            guard.arm(Cleanup {
                cfg: Arc::clone(&cfg),
                card: card.clone(),
                id: id.clone(),
                last_event: None,
                last_err: None,
            });

            // Go's client calls run under the invocation context: its cancellation ends the
            // response stream with the context's error (one more response to process).
            let send_client = Arc::clone(&client);
            let send_req = req.clone();
            let send_id = id.clone();
            let mut responses: BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>> =
                if ctx.run_config().streaming_mode == StreamingMode::None {
                    Box::pin(futures::stream::once(async move {
                        with_auth_scope(send_id, send_client.send_message(&send_req))
                            .await
                            .map(send_response_event)
                    }))
                } else {
                    Box::pin(
                        futures::stream::once(async move {
                            with_auth_scope(send_id, send_client.send_streaming_message(&send_req))
                                .await
                        })
                        .flat_map(|result| match result {
                            Ok(stream) => stream,
                            Err(err) => {
                                Box::pin(futures::stream::once(std::future::ready(Err(err))))
                            }
                        }),
                    )
                };

            let mut cancelled = false;
            loop {
                if cancelled {
                    break;
                }
                let item = tokio::select! {
                    biased;
                    () = ctx.cancellation_token().cancelled() => {
                        cancelled = true;
                        Err(a2a::A2AError::internal(CONTEXT_CANCELED))
                    }
                    item = with_auth_scope(id.clone(), responses.next()) => match item {
                        Some(item) => item,
                        None => break,
                    },
                };
                let a2a_event = item.as_ref().ok();
                let a2a_err = item.as_ref().err();
                if let Some(a2a_event) = a2a_event {
                    guard.set_last_event(a2a_event.clone());
                }

                let (mut event, mut err) = match &cfg.converter {
                    Some(converter) => split(converter(&ctx, &req, item.as_ref()).await),
                    None => split(processor.convert_to_session_event(&ctx, a2a_event, a2a_err).await),
                };
                // Go's default conversion merges an artifact update's artifact metadata into the
                // update's own metadata in place; later reads see the merged view.
                let view = match (&cfg.converter, &event, a2a_event) {
                    (None, Some(_), Some(a2a_event)) => converted_view(a2a_event),
                    _ => None,
                };
                let meta_event = view.as_ref().or(a2a_event);

                // TransferToAgent is restored here, after conversion, only when the caller has
                // opted in via allow_transfer_to_agent.
                if err.is_none() && cfg.allow_transfer_to_agent
                    && let (Some(event), Some(a2a_event)) = (event.as_mut(), meta_event)
                    && let Some(transfer) = transfer_to_agent_from_meta(stream_meta(a2a_event))
                {
                    event.actions.transfer_to_agent = transfer;
                }

                let cb_err = err.as_ref().map(to_a2a_error);
                match processor
                    .run_after_request_callbacks(&ctx, event.as_ref(), cb_err.as_ref())
                    .await
                {
                    Ok(Some(cb_event)) => {
                        event = Some(cb_event);
                        err = None;
                    }
                    Err(cb_err) => {
                        guard.set_last_err(&cb_err);
                        yield Err(agent_error(cb_err));
                        continue;
                    }
                    Ok(None) => {}
                }

                if let Some(err) = err {
                    guard.set_last_err(&err);
                    yield Err(agent_error(err));
                    continue;
                }

                // an event might be skipped
                let Some(event) = event else { continue };
                for to_emit in processor.aggregate_partial(&ctx, meta_event, event) {
                    let mut out = to_emit.event;
                    // aggregate_partial may synthesize a brand-new non-partial event from
                    // buffered partial chunks (the reassembled artifact). That event has not
                    // passed through the after-callbacks, so run them here; the pass-through
                    // event already ran them.
                    if !to_emit.passthrough {
                        match processor.run_after_request_callbacks(&ctx, Some(&out), None).await {
                            Ok(Some(cb_event)) => out = cb_event,
                            Err(cb_err) => {
                                guard.set_last_err(&cb_err);
                                yield Err(agent_error(cb_err));
                                break;
                            }
                            Ok(None) => {}
                        }
                    }
                    yield Ok(out);
                }
            }
            guard.finish().await;
        })
    }
}

/// The A2A event a `send_message` result is (Go's `SendMessageResult` is an `a2a.Event`).
fn send_response_event(resp: a2a::SendMessageResponse) -> a2a::StreamResponse {
    match resp {
        a2a::SendMessageResponse::Task(task) => a2a::StreamResponse::Task(task),
        a2a::SendMessageResponse::Message(msg) => a2a::StreamResponse::Message(msg),
    }
}

/// A Go `(*session.Event, error)` pair from a callback or converter result.
fn split(result: Result<Option<Event>, Error>) -> (Option<Event>, Option<Error>) {
    match result {
        Ok(event) => (event, None),
        Err(err) => (None, Some(err)),
    }
}

/// The error an after-request callback sees (its `err` parameter is an `A2AError`): an A2A
/// error as is, any other as an internal error with the same text.
fn to_a2a_error(err: &Error) -> a2a::A2AError {
    match err {
        Error::A2A(err) => err.clone(),
        other => a2a::A2AError::internal(other.to_string()),
    }
}

/// The run's error item.
fn agent_error(err: Error) -> AgentError {
    AgentError::Other(Box::new(err))
}

/// What the cleanup of an abandoned remote task needs (Go's deferred `cleanupRemoteTask` call).
struct Cleanup {
    cfg: Arc<A2AConfig>,
    card: a2a::AgentCard,
    id: Option<CallIdentity>,
    last_event: Option<a2a::StreamResponse>,
    /// Go `lastErr` (its text: the yielded error itself goes to the caller).
    last_err: Option<String>,
}

/// Go's deferred `cleanupRemoteTask` and `destroy`: run inline by [`RunGuard::finish`] when the
/// run ends, or on a spawned task when the event stream is dropped first (Go: the caller stops
/// the iteration).
struct RunGuard {
    state: Option<GuardState>,
}

struct GuardState {
    client: Arc<dyn A2AClient>,
    ctx: InvocationContext,
    cleanup: Option<Cleanup>,
}

impl RunGuard {
    fn new(client: Arc<dyn A2AClient>, ctx: InvocationContext) -> Self {
        Self {
            state: Some(GuardState {
                client,
                ctx,
                cleanup: None,
            }),
        }
    }

    /// From here on the remote task is cleaned up when the run ends.
    fn arm(&mut self, cleanup: Cleanup) {
        if let Some(state) = &mut self.state {
            state.cleanup = Some(cleanup);
        }
    }

    fn set_last_event(&mut self, event: a2a::StreamResponse) {
        if let Some(cleanup) = self.state.as_mut().and_then(|s| s.cleanup.as_mut()) {
            cleanup.last_event = Some(event);
        }
    }

    fn set_last_err(&mut self, err: &Error) {
        if let Some(cleanup) = self.state.as_mut().and_then(|s| s.cleanup.as_mut()) {
            cleanup.last_err = Some(err.to_string());
        }
    }

    /// Runs the cleanup and destroys the client now.
    async fn finish(&mut self) {
        if let Some(state) = self.state.take() {
            state.run().await;
        }
    }
}

impl GuardState {
    async fn run(self) {
        if let Some(cleanup) = self.cleanup {
            // Go: the cause is the last yielded error, else the context's cancellation cause.
            let cause = match cleanup.last_err {
                Some(text) => Some(Error::msg(text)),
                None if self.ctx.cancellation_token().is_cancelled() => {
                    Some(Error::Agent(AgentError::Cancelled))
                }
                None => None,
            };
            cleanup_remote_task(
                cleanup.id,
                &cleanup.cfg,
                &cleanup.card,
                Arc::clone(&self.client),
                cleanup.last_event.as_ref(),
                cause.as_ref(),
            )
            .await;
        }
        destroy(self.client.as_ref()).await;
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(state.run());
            }
            Err(_) => {
                tracing::warn!(
                    "remote task cleanup skipped: the event stream was dropped outside a tokio runtime"
                );
            }
        }
    }
}

/// Go `cleanupRemoteTask`: when the last event left the remote task running, hands it to
/// `remote_task_cleanup_callback`, or else cancels it (bounded by [`CLEANUP_TIMEOUT`]) unless it
/// waits for input and the run did not fail. Runs inside the invocation's auth scope.
pub(crate) async fn cleanup_remote_task(
    id: Option<CallIdentity>,
    cfg: &A2AConfig,
    card: &a2a::AgentCard,
    client: Arc<dyn A2AClient>,
    last_event: Option<&a2a::StreamResponse>,
    cause: Option<&Error>,
) {
    let Some(last_event) = last_event else {
        return;
    };
    let (task_id, context_id) = stream_task_info(last_event);
    if task_id.is_empty() {
        return;
    }
    let state = match last_event {
        a2a::StreamResponse::Message(_) => return,
        a2a::StreamResponse::StatusUpdate(update) => update.status.state.clone(),
        a2a::StreamResponse::Task(task) => task.status.state.clone(),
        a2a::StreamResponse::ArtifactUpdate(_) => a2a::TaskState::Unspecified,
    };
    if state.is_terminal() {
        return;
    }

    if let Some(callback) = &cfg.remote_task_cleanup_callback {
        let task_info = (task_id.to_owned(), context_id.to_owned());
        // The callback itself is called inside the scope, not only its future: a callback may
        // read `current_credential_scope()` before it returns the future.
        with_auth_scope(id, async move {
            callback(card, client, task_info, cause).await;
        })
        .await;
        return;
    }

    if state == a2a::TaskState::InputRequired && cause.is_none() {
        return;
    }
    let req = a2a::CancelTaskRequest {
        id: task_id.to_owned(),
        metadata: None,
        tenant: None,
    };
    let cancel = with_auth_scope(id, client.cancel_task(&req));
    match tokio::time::timeout(CLEANUP_TIMEOUT, cancel).await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!(task_id = %task_id, error = %err, "failed to cancel task"),
        Err(_) => tracing::warn!(
            task_id = %task_id,
            error = "context deadline exceeded",
            "failed to cancel task"
        ),
    }
}

/// Go `newMessage`: the message for this invocation. On resume (the last event answers a
/// function call this peer made) it carries the function response, unflattened, with the
/// call's task and context IDs; otherwise the parts the remote session has not seen (a scoped
/// dispatch with no history of its own seeds from the user content) and the last context ID.
pub(crate) async fn new_message(
    ctx: &InvocationContext,
    cfg: &A2AConfig,
) -> Result<a2a::Message, Error> {
    let events = ctx.session().events();
    let agent = ctx.agent();
    let scope = ctx.isolation_scope();
    let last = isize::try_from(events.len()).map_or(-1, |n| n - 1);
    // Resume path: do not rewrite function responses to text.
    if let Some(call) = get_user_function_call_at(&events, last, agent.name(), &scope) {
        let parts = convert_parts(cfg, &call.response, None)
            .await
            .map_err(|e| Error::wrap("event part conversion failed", e))?;
        let mut msg = a2a::Message::new(a2a::Role::User, parts);
        msg.task_id = non_empty(call.task_id);
        msg.context_id = non_empty(call.context_id);
        return Ok(msg);
    }

    let (mut parts, context_id) = to_missing_remote_session_parts(ctx, &events, cfg).await;
    // A scoped node dispatch receives its input through UserContent, which is not an event in
    // the shared session. Seed from it when the scope has no history of its own yet.
    if !scope.is_empty()
        && !has_isolation_scope_history(&events, &scope)
        && let Some(uc) = ctx.user_content()
        && !uc.parts.is_empty()
    {
        let mut event = Event::new(ctx.platform(), ctx.invocation_id());
        event.author = "user".to_owned();
        event.response.content = Some(uc.clone());
        parts = convert_parts(cfg, &event, None)
            .await
            .map_err(|e| Error::wrap("event part conversion failed", e))?;
    }
    let mut msg = a2a::Message::new(a2a::Role::User, parts);
    msg.context_id = non_empty(context_id);
    Ok(msg)
}

/// Go's empty-string-means-absent for message IDs.
fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Go `toErrorEvent`: a turn-complete event carrying `err` as its error message and under
/// `a2a:error`.
pub(crate) fn to_error_event(ctx: &InvocationContext, err: &str) -> Event {
    let mut event = new_remote_agent_event(ctx);
    event.response.error_message = err.to_owned();
    let mut meta = Map::new();
    meta.insert(to_adk_meta_key("error"), Value::String(err.to_owned()));
    event.response.custom_metadata = meta;
    event.response.turn_complete = true;
    event
}

/// Go `destroy`.
async fn destroy(client: &dyn A2AClient) {
    if let Err(err) = client.destroy().await {
        tracing::warn!(error = %err, "failed to destroy client");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gadk_agent::{RunConfig, StreamingMode};
    use gadk_genai::{Content, ROLE_USER};

    use super::*;
    use crate::testutil::{FakeA2AClient, FakeCall, FakeClientProvider};

    fn card() -> a2a::AgentCard {
        serde_json::from_value(serde_json::json!({
            "name": "remote", "version": "1.0", "capabilities": {},
            "supportedInterfaces": [
                {"url": "http://127.0.0.1:1", "protocolBinding": "JSONRPC", "protocolVersion": "1.0"}
            ],
        }))
        .unwrap()
    }

    fn task(state: a2a::TaskState) -> a2a::Task {
        a2a::Task {
            id: "task-1".into(),
            context_id: "ctx-1".into(),
            status: a2a::TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        }
    }

    fn working_update() -> a2a::StreamResponse {
        a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
            task_id: "task-1".into(),
            context_id: "ctx-1".into(),
            status: a2a::TaskStatus {
                state: a2a::TaskState::Working,
                message: Some(a2a::Message::new(
                    a2a::Role::Agent,
                    vec![a2a::Part::text("thinking")],
                )),
                timestamp: None,
            },
            metadata: None,
        })
    }

    fn context(agent: &Arc<dyn Agent>, mode: StreamingMode) -> InvocationContext {
        InvocationContext::builder()
            .agent(Arc::clone(agent))
            .user_content(Content::from_text("hello", ROLE_USER))
            .isolation_scope("scope")
            .run_config(RunConfig {
                streaming_mode: mode,
                ..Default::default()
            })
            .build_invocation()
    }

    fn remote(fake: &Arc<FakeA2AClient>, provider: Option<AgentCardProvider>) -> Arc<dyn Agent> {
        new_a2a(A2AConfig {
            name: "remote".into(),
            agent_card: provider.is_none().then(card),
            agent_card_provider: provider,
            client_provider: Some(Arc::new(FakeClientProvider::new(Arc::clone(fake)))),
            ..Default::default()
        })
        .unwrap()
    }

    async fn collect(agent: &Arc<dyn Agent>, mode: StreamingMode) -> Vec<Event> {
        agent
            .run(context(agent, mode))
            .map(|item| item.unwrap())
            .collect()
            .await
    }

    /// R8: the card is resolved on every invocation, once.
    #[tokio::test]
    async fn the_card_is_resolved_once_per_invocation() {
        let resolutions = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&resolutions);
        let provider: AgentCardProvider = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(card()) })
        });
        let fake = Arc::new(
            FakeA2AClient::new().with_stream(vec![Ok(a2a::StreamResponse::Task(task(
                a2a::TaskState::Completed,
            )))]),
        );
        let agent = remote(&fake, Some(provider));
        collect(&agent, StreamingMode::Sse).await;
        assert_eq!(resolutions.load(Ordering::SeqCst), 1);
        collect(&agent, StreamingMode::Sse).await;
        assert_eq!(resolutions.load(Ordering::SeqCst), 2);
    }

    /// R8: `StreamingMode::None` sends with `send_message`, every other mode streams.
    #[tokio::test]
    async fn streaming_mode_selects_send_or_stream() {
        for (mode, streams) in [
            (StreamingMode::None, false),
            (StreamingMode::Sse, true),
            (StreamingMode::Bidi, true),
        ] {
            let fake = Arc::new(
                FakeA2AClient::new()
                    .with_send(Ok(a2a::SendMessageResponse::Task(task(
                        a2a::TaskState::Completed,
                    ))))
                    .with_stream(vec![Ok(a2a::StreamResponse::Task(task(
                        a2a::TaskState::Completed,
                    )))]),
            );
            let agent = remote(&fake, None);
            let events = collect(&agent, mode).await;
            assert_eq!(events.len(), 1, "{mode:?}");
            let calls = fake.calls();
            assert_eq!(calls.len(), 2, "{mode:?}: {calls:?}");
            match (&calls[0], streams) {
                (FakeCall::Stream(req), true) | (FakeCall::Send(req), false) => {
                    assert_eq!(req.message.parts, vec![a2a::Part::text("hello")]);
                }
                (other, _) => panic!("{mode:?}: unexpected call {other:?}"),
            }
            assert!(matches!(calls[1], FakeCall::Destroy), "{mode:?}");
        }
    }

    /// Dropping the event stream before a terminal event still cancels the remote task and
    /// destroys the client (on a spawned task).
    #[tokio::test]
    async fn an_abandoned_run_cancels_the_remote_task() {
        let fake = Arc::new(
            FakeA2AClient::new()
                .with_stream(vec![Ok(working_update()), Ok(working_update())])
                .with_cancel(Ok(task(a2a::TaskState::Canceled))),
        );
        let agent = remote(&fake, None);
        let mut stream = agent.run(context(&agent, StreamingMode::Sse));
        let first = stream.next().await.unwrap().unwrap();
        assert!(first.response.partial);
        drop(stream);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while fake.calls().len() < 3 {
            assert!(tokio::time::Instant::now() < deadline, "{:?}", fake.calls());
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let calls = fake.calls();
        match &calls[1] {
            FakeCall::Cancel(req) => assert_eq!(req.id, "task-1"),
            other => panic!("expected the cancel, got {other:?}"),
        }
        assert!(matches!(calls[2], FakeCall::Destroy));
    }

    /// A run that ends on a non-terminal task cancels it inline, before the client is destroyed.
    #[tokio::test]
    async fn a_run_ending_on_a_working_task_cancels_it_before_destroy() {
        let fake = Arc::new(
            FakeA2AClient::new()
                .with_stream(vec![Ok(working_update())])
                .with_cancel(Ok(task(a2a::TaskState::Canceled))),
        );
        let agent = remote(&fake, None);
        collect(&agent, StreamingMode::Sse).await;
        let calls = fake.calls();
        assert!(matches!(calls[0], FakeCall::Stream(_)), "{calls:?}");
        assert!(matches!(calls[1], FakeCall::Cancel(_)), "{calls:?}");
        assert!(matches!(calls[2], FakeCall::Destroy), "{calls:?}");
    }

    /// Go `NewA2A`'s validation texts.
    #[test]
    fn new_a2a_validates_the_config() {
        let err = new_a2a(A2AConfig::default()).err().unwrap();
        assert_eq!(
            err.to_string(),
            "either AgentCard or AgentCardProvider must be provided"
        );
        let agent = new_a2a(A2AConfig {
            name: "remote".into(),
            agent_card: Some(card()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(agent.agent_state().agent_type, AgentType::RemoteAgent);
        assert!(crate::remoteagent::config::find_remote_agent_state(&*agent).is_some());
    }
}
