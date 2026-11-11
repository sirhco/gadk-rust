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

//! The remote agent's server config and state (port of ADK-Go `internal/agent/remoteagent`
//! `a2a_config.go`).

use std::sync::Arc;

use gadk_agent::Agent;

use super::card::AgentCardProvider;
use super::client::{A2AClient, A2AClientProvider, ClientContext};
use crate::Error;

/// Go `A2AServerConfig`: how to reach the remote agent.
#[derive(Clone)]
pub struct A2AServerConfig {
    /// A fixed card (takes precedence over `agent_card_provider`).
    pub agent_card: Option<a2a::AgentCard>,
    /// Resolves the card on every invocation.
    pub agent_card_provider: Option<AgentCardProvider>,
    /// Creates the client for a card.
    pub client_provider: Arc<dyn A2AClientProvider>,
    /// Go `OwnsAuthScope`: the remote agent enters the credential scope around its calls.
    pub owns_auth_scope: bool,
    /// Go `WithCardFetchClient`: overrides the card-fetch HTTP client.
    pub card_fetch_client: Option<reqwest::Client>,
    /// The credential provider (Go `A2AConfig.Auth`).
    pub auth: Option<Arc<dyn gadk_auth::CredentialProvider>>,
}

/// Go `RemoteAgentState`: stored as the remote agent's `AgentState.config`.
#[derive(Clone)]
pub struct RemoteAgentState {
    /// The server config.
    pub a2a: Arc<A2AServerConfig>,
}

/// Go `ResolveAgentCard`: the fixed card, else the provider's.
#[allow(dead_code)] // called by the remote agent (Task 8) and the executor cleanup (Task 7)
pub(crate) async fn resolve_agent_card(cfg: &A2AServerConfig) -> Result<a2a::AgentCard, Error> {
    if let Some(card) = &cfg.agent_card {
        return Ok(card.clone());
    }
    if let Some(provider) = &cfg.agent_card_provider {
        return provider().await;
    }
    Err(Error::msg(
        "either AgentCard or AgentCardProvider must be set",
    ))
}

/// Go `CreateA2AClient`: the resolved card and a client for it. The caller enters the auth
/// scope around it when `cfg.owns_auth_scope` (the card fetch reads it).
#[allow(dead_code)] // called by the remote agent (Task 8) and the executor cleanup (Task 7)
pub(crate) async fn create_a2a_client(
    cx: ClientContext<'_>,
    cfg: &A2AServerConfig,
) -> Result<(a2a::AgentCard, Arc<dyn A2AClient>), Error> {
    let card = resolve_agent_card(cfg)
        .await
        .map_err(|e| Error::wrap("agent card resolution failed", e))?;
    let client = cfg
        .client_provider
        .create_client(cx, &card)
        .await
        .map_err(|e| Error::wrap("client creation failed", e))?;
    Ok((card, client))
}

/// The server config of a remote agent (downcasts `AgentState.config` to
/// [`RemoteAgentState`]); `None` for any other agent.
#[allow(dead_code)] // called by the remote agent (Task 8) and the executor cleanup (Task 7)
pub(crate) fn find_remote_agent_state(agent: &dyn Agent) -> Option<Arc<A2AServerConfig>> {
    agent
        .agent_state()
        .config
        .as_ref()?
        .downcast_ref::<RemoteAgentState>()
        .map(|state| Arc::clone(&state.a2a))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gadk_agent::{AgentConfig, AgentState, AgentType};

    use super::*;
    use crate::testutil::{FakeA2AClient, FakeClientProvider};

    fn card(name: &str) -> a2a::AgentCard {
        serde_json::from_value(serde_json::json!({
            "name": name, "version": "1.0", "supportedInterfaces": [], "capabilities": {},
        }))
        .unwrap()
    }

    fn config(
        agent_card: Option<a2a::AgentCard>,
        agent_card_provider: Option<AgentCardProvider>,
        client_provider: Arc<dyn A2AClientProvider>,
    ) -> A2AServerConfig {
        A2AServerConfig {
            agent_card,
            agent_card_provider,
            client_provider,
            owns_auth_scope: false,
            card_fetch_client: None,
            auth: None,
        }
    }

    fn counting_provider(name: &'static str, calls: Arc<AtomicUsize>) -> AgentCardProvider {
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(card(name)) })
        })
    }

    fn fake_provider() -> Arc<dyn A2AClientProvider> {
        Arc::new(FakeClientProvider::new(Arc::new(FakeA2AClient::new())))
    }

    #[tokio::test]
    async fn resolve_prefers_the_fixed_card_then_the_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let both = config(
            Some(card("fixed")),
            Some(counting_provider("provided", Arc::clone(&calls))),
            fake_provider(),
        );
        assert_eq!(resolve_agent_card(&both).await.unwrap().name, "fixed");
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let provided = config(
            None,
            Some(counting_provider("provided", Arc::clone(&calls))),
            fake_provider(),
        );
        assert_eq!(
            resolve_agent_card(&provided).await.unwrap().name,
            "provided"
        );
        assert_eq!(
            resolve_agent_card(&provided).await.unwrap().name,
            "provided"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "resolved on every call");

        let neither = config(None, None, fake_provider());
        assert_eq!(
            resolve_agent_card(&neither).await.unwrap_err().to_string(),
            "either AgentCard or AgentCardProvider must be set"
        );
    }

    #[tokio::test]
    async fn create_returns_the_card_and_a_client() {
        let fake = Arc::new(FakeA2AClient::new());
        let provider = Arc::new(FakeClientProvider::new(Arc::clone(&fake)));
        let cfg = config(Some(card("fixed")), None, provider.clone());
        let (card, client) = create_a2a_client(ClientContext::Detached, &cfg)
            .await
            .unwrap();
        assert_eq!(card.name, "fixed");
        assert_eq!(provider.created(), 1);
        client.destroy().await.unwrap();
        assert_eq!(fake.calls().len(), 1);
    }

    #[tokio::test]
    async fn create_wraps_resolution_and_creation_errors_as_go_does() {
        let failing_card: AgentCardProvider =
            Arc::new(|| Box::pin(async { Err(Error::msg("card boom")) }));
        let cfg = config(None, Some(failing_card), fake_provider());
        let err = create_a2a_client(ClientContext::Detached, &cfg)
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "agent card resolution failed: card boom");

        struct Failing;
        #[async_trait::async_trait]
        impl A2AClientProvider for Failing {
            async fn create_client(
                &self,
                _cx: ClientContext<'_>,
                _card: &a2a::AgentCard,
            ) -> Result<Arc<dyn A2AClient>, Error> {
                Err(Error::msg("client boom"))
            }
        }
        let cfg = config(Some(card("fixed")), None, Arc::new(Failing));
        let err = create_a2a_client(ClientContext::Detached, &cfg)
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "client creation failed: client boom");
    }

    #[test]
    fn find_remote_agent_state_reads_the_remote_agent_config() {
        let cfg = Arc::new(config(Some(card("fixed")), None, fake_provider()));
        let remote = gadk_agent::new_with_state(
            AgentConfig {
                name: "remote".into(),
                ..Default::default()
            },
            AgentState {
                agent_type: AgentType::RemoteAgent,
                config: Some(Arc::new(RemoteAgentState {
                    a2a: Arc::clone(&cfg),
                })),
                on_before_agent_callback_event: None,
                live: None,
            },
        )
        .unwrap();
        let found = find_remote_agent_state(&*remote).unwrap();
        assert!(Arc::ptr_eq(&found, &cfg));

        let plain = gadk_agent::new_agent(AgentConfig {
            name: "plain".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(find_remote_agent_state(&*plain).is_none());

        let other_config = gadk_agent::new_with_state(
            AgentConfig {
                name: "other".into(),
                ..Default::default()
            },
            AgentState {
                agent_type: AgentType::CustomAgent,
                config: Some(Arc::new(42_u32)),
                on_before_agent_callback_event: None,
                live: None,
            },
        )
        .unwrap();
        assert!(find_remote_agent_state(&*other_config).is_none());
    }
}
