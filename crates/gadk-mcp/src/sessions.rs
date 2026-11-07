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

//! The per-credential session pool (port of ADK-Go `tool/mcptoolset/client.go`
//! `credentialSessions`, `pooledSession`, `sessionKey`).

use std::sync::Arc;
use std::time::Duration;

use gadk_auth::{AuthContext, CredentialProvider};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig, Tool};

use crate::client::{CallContext, McpClient, SessionClient};
use crate::config::TransportFactory;
use crate::error::Error;
use crate::transport::HttpTarget;

/// client.go `sessionIdleTimeout`: a session unused this long is closed on the next acquire.
#[allow(dead_code)]
pub(crate) const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// client.go `maxSessions`: the most sessions kept open while none are in use.
#[allow(dead_code)]
pub(crate) const MAX_SESSIONS: usize = 64;

/// Go `credentialSessions`: one [`ConnectionRefresher`](crate::client::ConnectionRefresher) per
/// distinct credential, keyed by [`session_key`](CredentialSessions::session_key).
#[allow(dead_code)]
pub(crate) struct CredentialSessions {}

#[allow(unused_variables, dead_code, clippy::todo)]
impl CredentialSessions {
    /// A pool whose sessions speak streamable HTTP to `target`, each bound to the identity of
    /// the caller that created it.
    pub(crate) fn new(
        client_info: ClientConfig,
        target: HttpTarget,
        provider: Arc<dyn CredentialProvider>,
    ) -> Self {
        todo!("Task 8")
    }

    /// A pool whose sessions connect through `make(identity)` (tests whose sessions do not speak
    /// HTTP, as `client_test.go`'s do not).
    pub(crate) fn with_factory(
        client_info: ClientConfig,
        provider: Arc<dyn CredentialProvider>,
        make: Arc<dyn Fn(AuthContext) -> Arc<dyn TransportFactory> + Send + Sync>,
    ) -> Self {
        todo!("Task 8")
    }

    /// Go `sessionKey`: SHA-256 over the length-prefixed, sorted headers the credential resolved
    /// for `cx` applies.
    pub(crate) async fn session_key(&self, cx: &CallContext) -> Result<[u8; 32], Error> {
        todo!("Task 8")
    }

    /// Replaces the idle timeout and the session cap (Go `SetSessionBoundsForTest`).
    #[cfg(test)]
    pub(crate) fn set_bounds(&self, idle_timeout: Duration, max_sessions: usize) {
        todo!("Task 8")
    }

    /// How many sessions the pool keeps (Go `SessionCountForTest`).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        todo!("Task 8")
    }
}

#[async_trait::async_trait]
#[allow(unused_variables, clippy::todo)]
impl McpClient for CredentialSessions {
    async fn call_tool(
        &self,
        cx: &CallContext,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, Error> {
        todo!("Task 8")
    }

    async fn list_tools(&self, cx: &CallContext) -> Result<Vec<Tool>, Error> {
        todo!("Task 8")
    }
}

#[async_trait::async_trait]
#[allow(clippy::todo)]
impl SessionClient for CredentialSessions {
    async fn close(&self) -> Result<(), Arc<Error>> {
        todo!("Task 8")
    }

    fn as_credential_sessions(&self) -> Option<&CredentialSessions> {
        Some(self)
    }
}
