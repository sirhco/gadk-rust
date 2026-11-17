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

//! gadk-a2a: the Agent2Agent (A2A) protocol for gadk agents, built on the official A2A Rust SDK
//! (`a2a-lf`, `a2a-server-lf`, `a2a-client-lf`). Wire-compatible with ADK A2A servers and clients:
//! the metadata keys, extension marker and error texts are the ones ADK uses.
//!
//! - [`convert`] (always built): ADK ↔ A2A part, event, metadata and extension conversions.
//! - `server` (feature `server`): an `a2a_server::AgentExecutor` that runs a gadk agent, the
//!   agent-card skill builder and the identity header bridge.
//! - `remoteagent` (feature `client`): a gadk agent backed by a remote A2A agent, with
//!   agent-card resolution, credential scoping and remote-task cleanup.
//!
//! The a2a-rs crates are re-exported unwrapped: [`a2a`], `a2a_client` (feature `client`) and
//! `a2a_server` (feature `server`).

pub use a2a;
#[cfg(feature = "client")]
pub use a2a_client;
#[cfg(feature = "server")]
pub use a2a_server;

pub mod convert;
#[cfg(feature = "client")]
pub mod remoteagent;
#[cfg(feature = "server")]
pub mod server;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

mod error;
#[cfg(test)]
mod parity_tests;

pub use error::Error;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod contract_tests {
    //! The Appendix C values Task 1 fixes, and the auto traits later tasks rely on.

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[test]
    fn error_is_send_sync_and_static() {
        fn assert_static<T: 'static>() {}
        assert_send_sync::<crate::Error>();
        assert_static::<crate::Error>();
    }

    #[test]
    fn adk_extension_uri_is_gos() {
        assert_eq!(
            crate::convert::ADK_EXTENSION_URI,
            "https://google.github.io/adk-docs/a2a/a2a-extension/"
        );
    }

    #[cfg(feature = "server")]
    #[test]
    fn server_defaults_and_constants() {
        use crate::server::{ExecutorConfig, IDENTITY_HEADER, OutputMode};

        assert_eq!(IDENTITY_HEADER, "x-gadk-a2a-user");
        assert_eq!(OutputMode::default(), OutputMode::ArtifactPerRun);
        let cfg = ExecutorConfig::default();
        assert!(cfg.runner_config.is_none() && cfg.runner_provider.is_none());
        assert!(cfg.user_resolver.is_none());
        assert_eq!(cfg.output_mode, OutputMode::ArtifactPerRun);

        fn assert_executor<E: a2a_server::AgentExecutor + Clone>() {}
        assert_executor::<crate::server::Executor>();
        assert_send_sync::<dyn crate::server::ExecutorContext>();
        assert_send_sync::<dyn crate::server::Runner>();
    }

    #[cfg(feature = "client")]
    #[test]
    fn client_defaults_and_constants() {
        use std::time::Duration;

        use crate::remoteagent::{A2AConfig, CardResolveOptions};

        let cfg = A2AConfig::default();
        assert!(cfg.name.is_empty() && cfg.agent_card.is_none() && cfg.auth.is_none());
        assert!(!cfg.allow_transfer_to_agent);
        let opts = CardResolveOptions::default();
        assert!(opts.path.is_none() && opts.request_headers.is_empty());
        assert_eq!(
            crate::remoteagent::auth::MINT_TIMEOUT,
            Duration::from_secs(30)
        );
        assert_send_sync::<dyn crate::remoteagent::A2AClient>();
        assert_send_sync::<dyn crate::remoteagent::A2AClientProvider>();
        assert_send_sync::<crate::remoteagent::A2AServerConfig>();
    }
}
