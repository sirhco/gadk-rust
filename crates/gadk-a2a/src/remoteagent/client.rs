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

//! The A2A client seam: the calls the remote agent makes and the provider that creates a client.

use std::sync::Arc;
use std::time::Duration;

use a2a_client::jsonrpc::JsonRpcTransportFactory;
use a2a_client::rest::RestTransportFactory;
use futures::stream::BoxStream;
use gadk_agent::InvocationContext;

use super::auth::{AuthInterceptor, auth_http_client};
use crate::Error;

/// The calls the remote agent makes.
#[async_trait::async_trait]
pub trait A2AClient: Send + Sync {
    /// Sends a message and returns the task or message the peer answers with.
    async fn send_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, a2a::A2AError>;
    /// Sends a message and streams the peer's responses.
    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError>;
    /// Cancels a task.
    async fn cancel_task(&self, req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError>;
    /// Releases the client's resources.
    async fn destroy(&self) -> Result<(), a2a::A2AError>;
}

/// What a client is created for: an agent invocation, or work detached from one (the server's
/// child-task cleanup).
#[derive(Clone, Copy)]
pub enum ClientContext<'a> {
    /// A remote-agent invocation.
    Invocation(&'a InvocationContext),
    /// No invocation (server-side cleanup).
    Detached,
}

/// Creates the [`A2AClient`] for an agent card.
#[async_trait::async_trait]
pub trait A2AClientProvider: Send + Sync {
    /// A client for the agent `card` describes.
    async fn create_client(
        &self,
        cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error>;
}

/// The timeout of the HTTP client the default JSON-RPC and REST transports use.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// The a2a-rs message for a card no registered transport can serve (a2a-client-lf 0.2.9
/// `create_from_card`); mapped to the ADK error text.
const A2A_RS_NO_COMPATIBLE_TRANSPORT: &str =
    "no compatible transport found for agent card interfaces";

/// A provider that creates clients from `factory`; `None` → a default `A2AClientFactory`.
///
/// The default factory has JSON-RPC and REST transports over an HTTP client with a 3 min timeout
/// and no credential. The remote agent passes `auth_client_factory` when `A2AConfig.auth` is
/// set, a factory over the auth-aware HTTP client.
pub fn new_a2a_client_provider(
    factory: Option<Arc<a2a_client::A2AClientFactory>>,
) -> Arc<dyn A2AClientProvider> {
    let factory = factory.unwrap_or_else(|| Arc::new(default_client_factory()));
    Arc::new(FactoryClientProvider { factory })
}

/// The default factory: JSON-RPC and REST over one HTTP client with
/// [`DEFAULT_REQUEST_TIMEOUT`].
fn default_client_factory() -> a2a_client::A2AClientFactory {
    // `build` fails only when reqwest cannot initialize its TLS backend. The fallback is
    // `reqwest::Client::new()`, which panics on that same failure: this panics exactly when
    // reqwest's own `Client::new` would. `new_a2a_client_provider`
    // does not return an error, so there is nothing to propagate it through.
    let http = reqwest::Client::builder()
        .timeout(DEFAULT_REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default();
    transport_factory(http).build()
}

/// The factory the remote agent builds when `A2AConfig.auth` is set: JSON-RPC and REST over
/// [`auth_http_client`], with [`AuthInterceptor`] applying the credential
/// of the current auth scope to every call. Fails as [`auth_http_client`] does.
pub(crate) fn auth_client_factory() -> Result<a2a_client::A2AClientFactory, Error> {
    Ok(transport_factory(auth_http_client()?)
        .with_interceptor(Arc::new(AuthInterceptor))
        .build())
}

/// A factory builder with the JSON-RPC and REST transports over `http`.
fn transport_factory(http: reqwest::Client) -> a2a_client::factory::A2AClientFactoryBuilder {
    a2a_client::A2AClientFactory::builder()
        .register(Arc::new(JsonRpcTransportFactory::new(Some(http.clone()))))
        .register(Arc::new(RestTransportFactory::new(Some(http))))
}

/// The provider [`new_a2a_client_provider`] returns.
struct FactoryClientProvider {
    factory: Arc<a2a_client::A2AClientFactory>,
}

#[async_trait::async_trait]
impl A2AClientProvider for FactoryClientProvider {
    async fn create_client(
        &self,
        _cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error> {
        // The checks and error texts of an ADK client factory.
        if card.supported_interfaces.is_empty() {
            return Err(Error::msg("agent card has no supported interfaces"));
        }
        match self.factory.create_from_card(card).await {
            Ok(client) => Ok(Arc::new(FactoryClient { client })),
            Err(e) if e.message == A2A_RS_NO_COMPATIBLE_TRANSPORT => {
                let protocols: Vec<String> = card
                    .supported_interfaces
                    .iter()
                    .map(|i| format!("{}_{}", i.protocol_binding, i.protocol_version))
                    .collect();
                Err(Error::msg(format!(
                    "no compatible transports found: available transports - [{}]",
                    protocols.join(",")
                )))
            }
            Err(e) => Err(Error::A2A(e)),
        }
    }
}

/// An a2a-rs client as an [`A2AClient`].
struct FactoryClient {
    client: a2a_client::A2AClient<Box<dyn a2a_client::Transport>>,
}

#[async_trait::async_trait]
impl A2AClient for FactoryClient {
    async fn send_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, a2a::A2AError> {
        self.client.send_message(req).await
    }

    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError> {
        self.client.send_streaming_message(req).await
    }

    async fn cancel_task(&self, req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError> {
        self.client.cancel_task(req).await
    }

    async fn destroy(&self) -> Result<(), a2a::A2AError> {
        self.client.destroy().await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::remoteagent::card::tests::serve;

    fn card(ifaces: &[(&str, &str)]) -> a2a::AgentCard {
        let ifaces: Vec<Value> = ifaces
            .iter()
            .map(|(url, binding)| json!({"url": url, "protocolBinding": binding, "protocolVersion": "1.0"}))
            .collect();
        serde_json::from_value(json!({
            "name": "remote",
            "version": "1.0",
            "supportedInterfaces": ifaces,
            "capabilities": {},
        }))
        .unwrap()
    }

    fn request(text: &str) -> a2a::SendMessageRequest {
        a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text(text)]),
            configuration: None,
            metadata: None,
            tenant: None,
        }
    }

    fn agent_message() -> Value {
        json!({"messageId": "m1", "role": "ROLE_AGENT", "parts": [{"text": "hello"}]})
    }

    fn reply_text(resp: a2a::SendMessageResponse) -> String {
        match resp {
            a2a::SendMessageResponse::Message(m) => m.text().unwrap_or_default().to_owned(),
            other => panic!("want a message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_provider_sends_over_json_rpc() {
        let (base, recorded) = serve(|_, req| {
            let rpc: Value = serde_json::from_slice(&req.body).unwrap();
            let resp =
                json!({"jsonrpc": "2.0", "id": rpc["id"], "result": {"message": agent_message()}});
            (200, resp.to_string())
        })
        .await;
        let provider = new_a2a_client_provider(None);
        let client = provider
            .create_client(
                ClientContext::Detached,
                &card(&[(&format!("{base}/jsonrpc"), "JSONRPC")]),
            )
            .await
            .unwrap();
        let resp = client.send_message(&request("hi")).await.unwrap();
        assert_eq!(reply_text(resp), "hello");
        client.destroy().await.unwrap();

        let req = recorded.lock()[0].clone();
        assert_eq!(
            (req.method.as_str(), req.target.as_str()),
            ("POST", "/jsonrpc")
        );
        let rpc: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(rpc["method"], "SendMessage");
        assert_eq!(rpc["params"]["message"]["parts"][0]["text"], "hi");
    }

    #[tokio::test]
    async fn default_provider_sends_over_rest() {
        let (base, recorded) =
            serve(|_, _| (200, json!({"message": agent_message()}).to_string())).await;
        let provider = new_a2a_client_provider(None);
        let client = provider
            .create_client(
                ClientContext::Detached,
                &card(&[(&format!("{base}/rest"), "HTTP+JSON")]),
            )
            .await
            .unwrap();
        let resp = client.send_message(&request("hi")).await.unwrap();
        assert_eq!(reply_text(resp), "hello");
        let req = recorded.lock()[0].clone();
        assert_eq!(
            (req.method.as_str(), req.target.as_str()),
            ("POST", "/rest/message:send")
        );
    }

    #[tokio::test]
    async fn default_provider_prefers_json_rpc_when_the_card_offers_both() {
        let (base, recorded) = serve(|_, req| {
            let rpc: Value = serde_json::from_slice(&req.body).unwrap();
            let resp =
                json!({"jsonrpc": "2.0", "id": rpc["id"], "result": {"message": agent_message()}});
            (200, resp.to_string())
        })
        .await;
        let provider = new_a2a_client_provider(None);
        let client = provider
            .create_client(
                ClientContext::Detached,
                &card(&[
                    (&format!("{base}/rest"), "HTTP+JSON"),
                    (&format!("{base}/jsonrpc"), "JSONRPC"),
                ]),
            )
            .await
            .unwrap();
        client.send_message(&request("hi")).await.unwrap();
        assert_eq!(recorded.lock()[0].target, "/jsonrpc");
    }

    #[tokio::test]
    async fn provider_reports_a_card_without_a_usable_interface_as_a2a_go_does() {
        let provider = new_a2a_client_provider(None);
        let err = provider
            .create_client(ClientContext::Detached, &card(&[]))
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "agent card has no supported interfaces");

        let empty = Arc::new(
            a2a_client::A2AClientFactory::builder()
                .no_defaults()
                .build(),
        );
        let err = new_a2a_client_provider(Some(empty))
            .create_client(
                ClientContext::Detached,
                &card(&[
                    ("http://127.0.0.1:1/x", "JSONRPC"),
                    ("http://127.0.0.1:1/y", "GRPC"),
                ]),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            "no compatible transports found: available transports - [JSONRPC_1.0,GRPC_1.0]"
        );
    }
}
