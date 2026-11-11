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

//! The identity header bridge: the authenticated user reaches the executor through the
//! reserved `x-gadk-a2a-user` header (spec R5, R6).
//!
//! Go reads the user from the a2a call context; a2a-rs 0.5.3 does not propagate one into
//! `a2a_server::ExecutorContext::user`, so [`a2a_identity_layer`] (mounted inside the
//! authentication layer) writes the user into a reserved header that a2a-rs passes on as a
//! service param, and [`identity_header_resolver`] reads it back.

use std::sync::Arc;
use std::task::{Context, Poll};

use super::executor::UserResolver;

/// The reserved request header that carries the authenticated user to the executor.
pub const IDENTITY_HEADER: &str = "x-gadk-a2a-user";

/// The default user resolver: `ctx.user.name` when it is non-empty; it never reads headers, so a
/// deployment that does not mount [`a2a_identity_layer`] cannot be spoofed through
/// [`IDENTITY_HEADER`].
pub fn default_user_resolver() -> UserResolver {
    Arc::new(context_user)
}

/// Go's user lookup: the a2a call context's user name, when non-empty.
fn context_user(ctx: &a2a_server::ExecutorContext) -> Option<String> {
    ctx.user
        .as_ref()
        .map(|u| u.name.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// The first non-empty [`IDENTITY_HEADER`] service param, else [`default_user_resolver`].
/// Install it only together with [`a2a_identity_layer`] inside the authentication layer: the
/// layer is what removes a header the client sent.
pub fn identity_header_resolver() -> UserResolver {
    Arc::new(|ctx: &a2a_server::ExecutorContext| {
        ctx.service_params
            .get(IDENTITY_HEADER)
            .and_then(|values| values.iter().find(|v| !v.is_empty()))
            .cloned()
            .or_else(|| context_user(ctx))
    })
}

/// A tower layer that removes any inbound [`IDENTITY_HEADER`] and, when `extract` finds a user
/// in the request extensions, sets the header to it.
///
/// Mount it inside the authentication layer (so the extensions already hold the caller) and
/// around the A2A routers, and install [`identity_header_resolver`] as
/// `ExecutorConfig::user_resolver`. A user ID that is not a valid header value (control
/// characters, non-ASCII text) is dropped with a warning, and the executor falls back to its
/// anonymous `A2A_USER_<context_id>` user: a2a-rs passes only visible-ASCII header values on as
/// service params.
pub fn a2a_identity_layer<F>(extract: F) -> IdentityLayer<F>
where
    F: Fn(&http::Extensions) -> Option<String> + Clone + Send + Sync + 'static,
{
    IdentityLayer { extract }
}

/// The layer [`a2a_identity_layer`] returns.
#[derive(Clone)]
pub struct IdentityLayer<F> {
    extract: F,
}

impl<S, F: Clone> tower::Layer<S> for IdentityLayer<F> {
    type Service = IdentityService<S, F>;

    fn layer(&self, inner: S) -> Self::Service {
        IdentityService {
            inner,
            extract: self.extract.clone(),
        }
    }
}

/// The service [`IdentityLayer`] wraps around the A2A router.
#[derive(Clone)]
pub struct IdentityService<S, F> {
    inner: S,
    extract: F,
}

impl<S, F, B> tower::Service<http::Request<B>> for IdentityService<S, F>
where
    S: tower::Service<http::Request<B>>,
    F: Fn(&http::Extensions) -> Option<String> + Clone + Send + Sync + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        // Always strip what the client sent: only this layer may set the header.
        req.headers_mut().remove(IDENTITY_HEADER);
        if let Some(user) = (self.extract)(req.extensions()) {
            match http::HeaderValue::from_str(&user) {
                Ok(value) => {
                    req.headers_mut()
                        .insert(http::HeaderName::from_static(IDENTITY_HEADER), value);
                }
                Err(_) => tracing::warn!(
                    "a2a identity: the user ID is not a valid header value, so the request \
                     reaches the executor without one"
                ),
            }
        }
        self.inner.call(req)
    }
}

/// The user ID gadk-server's authentication layer stored (`gadk_server::authn::Caller`), when it
/// is non-empty. Pass it to [`a2a_identity_layer`] when the A2A routers sit behind
/// `gadk_server::authn::middleware`.
#[cfg(feature = "gadk-server")]
pub fn caller_user(ext: &http::Extensions) -> Option<String> {
    ext.get::<gadk_server::authn::Caller>()
        .map(|caller| caller.user_id.clone())
        .filter(|user_id| !user_id.is_empty())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tower::{Layer, ServiceExt};

    use super::*;

    fn executor_context(
        user: Option<&str>,
        params: &[(&str, &[&str])],
    ) -> a2a_server::ExecutorContext {
        a2a_server::ExecutorContext {
            message: None,
            task_id: "task-1".into(),
            stored_task: None,
            context_id: "ctx-1".into(),
            metadata: None,
            user: user.map(a2a_server::User::authenticated),
            service_params: params
                .iter()
                .map(|(k, vs)| {
                    (
                        (*k).to_owned(),
                        vs.iter().map(|v| (*v).to_owned()).collect(),
                    )
                })
                .collect(),
            tenant: None,
        }
    }

    /// a2a-rs's `extract_service_params` (crate-private there): lowercase header names, values
    /// that are not visible ASCII skipped, multi-valued headers in order.
    #[cfg(feature = "gadk-server")]
    fn service_params(headers: &http::HeaderMap) -> a2a_server::ServiceParams {
        let mut params = a2a_server::ServiceParams::new();
        for (name, value) in headers {
            if let Ok(v) = value.to_str() {
                params
                    .entry(name.as_str().to_owned())
                    .or_default()
                    .push(v.to_owned());
            }
        }
        params
    }

    /// Runs one request through `a2a_identity_layer(extract)` and returns the identity header
    /// values the inner service saw.
    async fn seen_identity_headers<F>(extract: F, req: http::Request<()>) -> Vec<String>
    where
        F: Fn(&http::Extensions) -> Option<String> + Clone + Send + Sync + 'static,
    {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_in = seen.clone();
        let inner = tower::service_fn(move |req: http::Request<()>| {
            let seen = seen_in.clone();
            async move {
                *seen.lock() = req
                    .headers()
                    .get_all(IDENTITY_HEADER)
                    .iter()
                    .map(|v| v.to_str().unwrap().to_owned())
                    .collect();
                Ok::<_, Infallible>(())
            }
        });
        a2a_identity_layer(extract)
            .layer(inner)
            .oneshot(req)
            .await
            .unwrap();
        seen.lock().clone()
    }

    fn spoofed_request() -> http::Request<()> {
        http::Request::builder()
            .uri("/jsonrpc")
            .header(IDENTITY_HEADER, "mallory")
            .header(IDENTITY_HEADER, "eve")
            .header("x-other", "kept")
            .body(())
            .unwrap()
    }

    #[tokio::test]
    async fn inbound_identity_header_is_stripped_when_the_extractor_returns_none() {
        let seen = seen_identity_headers(|_: &http::Extensions| None, spoofed_request()).await;
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[tokio::test]
    async fn inbound_identity_header_is_replaced_when_the_extractor_returns_some() {
        #[derive(Clone)]
        struct Authenticated(String);
        let mut req = spoofed_request();
        req.extensions_mut().insert(Authenticated("alice".into()));
        let seen = seen_identity_headers(
            |ext: &http::Extensions| ext.get::<Authenticated>().map(|a| a.0.clone()),
            req,
        )
        .await;
        assert_eq!(seen, ["alice"]);
    }

    #[tokio::test]
    async fn a_user_id_that_is_not_a_header_value_is_dropped() {
        let seen = seen_identity_headers(
            |_: &http::Extensions| Some("bad\nuser".to_owned()),
            spoofed_request(),
        )
        .await;
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[tokio::test]
    async fn other_headers_pass_through_the_identity_layer() {
        let inner = tower::service_fn(|req: http::Request<()>| async move {
            Ok::<_, Infallible>(req.headers().get("x-other").cloned())
        });
        let other = a2a_identity_layer(|_: &http::Extensions| None)
            .layer(inner)
            .oneshot(spoofed_request())
            .await
            .unwrap();
        assert_eq!(other.unwrap(), "kept");
    }

    #[test]
    fn default_resolver_reads_the_context_user_and_ignores_the_header() {
        let resolve = default_user_resolver();
        assert_eq!(
            resolve(&executor_context(
                Some("alice"),
                &[(IDENTITY_HEADER, &["mallory"])]
            )),
            Some("alice".to_owned())
        );
        assert_eq!(
            resolve(&executor_context(None, &[(IDENTITY_HEADER, &["mallory"])])),
            None
        );
    }

    #[test]
    fn default_resolver_treats_an_empty_user_name_as_none() {
        let resolve = default_user_resolver();
        assert_eq!(resolve(&executor_context(Some(""), &[])), None);
    }

    #[test]
    fn identity_header_resolver_prefers_the_first_non_empty_header_value() {
        let resolve = identity_header_resolver();
        assert_eq!(
            resolve(&executor_context(
                Some("alice"),
                &[(IDENTITY_HEADER, &["", "bob", "carol"])]
            )),
            Some("bob".to_owned())
        );
    }

    #[test]
    fn identity_header_resolver_falls_back_to_the_default_resolver() {
        let resolve = identity_header_resolver();
        assert_eq!(
            resolve(&executor_context(Some("alice"), &[])),
            Some("alice".to_owned())
        );
        assert_eq!(
            resolve(&executor_context(
                Some("alice"),
                &[(IDENTITY_HEADER, &[""])]
            )),
            Some("alice".to_owned())
        );
        assert_eq!(resolve(&executor_context(None, &[])), None);
    }

    #[cfg(feature = "gadk-server")]
    #[test]
    fn caller_user_reads_a_non_empty_caller_user_id() {
        let mut ext = http::Extensions::new();
        assert_eq!(caller_user(&ext), None);
        ext.insert(gadk_server::authn::Caller::default());
        assert_eq!(caller_user(&ext), None);
        ext.insert(gadk_server::authn::Caller {
            user_id: "alice".into(),
            claims: None,
        });
        assert_eq!(caller_user(&ext), Some("alice".to_owned()));
    }

    /// gadk-server's authn middleware outside, the identity layer inside: the executor's resolver
    /// sees the authenticated caller, never the header the client sent.
    #[cfg(feature = "gadk-server")]
    #[tokio::test]
    async fn authn_middleware_and_identity_layer_carry_the_caller_to_the_resolver() {
        use axum::body::Body;
        use axum::response::{IntoResponse, Response};

        let resolved = Arc::new(Mutex::new(None));
        let resolved_in = resolved.clone();
        let inner = tower::service_fn(move |req: http::Request<Body>| {
            let resolved = resolved_in.clone();
            async move {
                // What a2a-rs hands the executor: the request headers as service params.
                let mut ctx = executor_context(None, &[]);
                ctx.service_params = service_params(req.headers());
                *resolved.lock() = identity_header_resolver()(&ctx);
                Ok::<Response, Infallible>(http::StatusCode::OK.into_response())
            }
        });
        let svc = gadk_server::authn::middleware(gadk_server::authn::header("x-user"))
            .layer(a2a_identity_layer(caller_user).layer(inner));

        let req = http::Request::builder()
            .uri("/jsonrpc")
            .header("x-user", "alice")
            .header(IDENTITY_HEADER, "mallory")
            .body(Body::empty())
            .unwrap();
        let resp = svc.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resolved.lock().clone(), Some("alice".to_owned()));

        // Without credentials the authn layer answers 401 and the executor is never reached.
        *resolved.lock() = None;
        let req = http::Request::builder()
            .uri("/jsonrpc")
            .header(IDENTITY_HEADER, "mallory")
            .body(Body::empty())
            .unwrap();
        let resp = svc.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
        assert_eq!(resolved.lock().clone(), None);
    }

    /// Spec R6 end to end: gadk-server's authn middleware outside, the identity layer inside, the
    /// a2a-rs JSON-RPC router and a real [`crate::server::Executor`] with
    /// [`identity_header_resolver`]: the agent runs as the authenticated caller, never as the
    /// user the client claims in the identity header. (The request goes out through reqwest,
    /// which the `client` feature brings.)
    #[cfg(all(feature = "gadk-server", feature = "client"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_executor_runs_as_the_authenticated_caller() {
        use futures::StreamExt as _;
        use gadk_agent::{AgentConfig, AgentError, new_agent};

        use crate::server::{Executor, ExecutorConfig};

        let users = Arc::new(Mutex::new(Vec::new()));
        let agent = {
            let users = Arc::clone(&users);
            new_agent(AgentConfig {
                name: "agent".into(),
                run: Some(Arc::new(move |ic: gadk_agent::InvocationContext| {
                    users.lock().push(ic.session().user_id().to_owned());
                    futures::stream::empty::<Result<gadk_session::Event, AgentError>>().boxed()
                })),
                ..Default::default()
            })
            .unwrap()
        };
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(gadk_engine::runner::RunnerConfig {
                app_name: "agent".into(),
                agent,
                session_service: Arc::new(gadk_session::InMemorySessionService::new()),
                artifact_service: None,
                memory_service: None,
                plugin_config: gadk_engine::runner::PluginConfig::default(),
                auto_create_session: false,
                platform: gadk_platform::Platform::default(),
                compaction: None,
            }),
            user_resolver: Some(identity_header_resolver()),
            ..Default::default()
        });
        let handler = Arc::new(a2a_server::DefaultRequestHandler::new(
            executor,
            a2a_server::InMemoryTaskStore::new(),
        ));
        // The mounting order of R15: authn outermost, the identity layer inside it.
        let app = a2a_server::jsonrpc::jsonrpc_router(handler)
            .layer(a2a_identity_layer(caller_user))
            .layer(gadk_server::authn::middleware(gadk_server::authn::header(
                "x-user",
            )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let send = |user: Option<&'static str>| {
            let mut req = reqwest::Client::new()
                .post(&url)
                .header("a2a-version", "1.0")
                .header(IDENTITY_HEADER, "mallory")
                .json(&serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "SendMessage",
                    "params": {"message": {"messageId": "m1", "role": "ROLE_USER", "parts": [{"text": "hi"}]}},
                }));
            if let Some(user) = user {
                req = req.header("x-user", user);
            }
            req.send()
        };
        let resp = send(Some("alice")).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.get("result").is_some(), "{body}");
        assert_eq!(users.lock().clone(), vec!["alice".to_owned()]);

        // Without credentials the authn layer answers 401 and no run starts.
        let resp = send(None).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
        assert_eq!(users.lock().len(), 1);
    }
}
