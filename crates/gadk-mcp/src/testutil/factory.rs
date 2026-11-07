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

//! In-memory [`TransportFactory`]s: [`in_memory`] and the observable [`ScriptedFactory`] (Go
//! `mcp.NewInMemoryTransports`, and the `spyTransport`, `reconnectableTransport`,
//! `gatedSessionTransport` and `connectFunc` fakes of ADK-Go's tests).

use std::sync::Arc;

use rmcp::model::ClientConfig;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServerHandler};

use crate::config::TransportFactory;
use crate::error::BoxError;

/// Each connect spawns `make_server()` on one half of a `tokio::io::duplex` and serves the client
/// on the other (Go `mcp.NewInMemoryTransports` + `server.Connect`).
#[allow(unused_variables, clippy::todo)]
pub fn in_memory<S, F>(make_server: F) -> Arc<dyn TransportFactory>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: ServerHandler,
{
    todo!("Task 4")
}

/// A [`TransportFactory`] over in-memory servers that a test can observe and steer.
///
/// Every connect counts itself and announces itself to [`entered`](ScriptedFactory::entered);
/// while the gate is closed it then waits for [`open_gate`](ScriptedFactory::open_gate) (a
/// dropped connect future stops waiting: Go's `ctx.Done()` branch); then it fails if a failure
/// is set, else serves a fresh server over a duplex.
pub struct ScriptedFactory {}

#[allow(unused_variables, clippy::todo)]
impl ScriptedFactory {
    /// Serves a fresh `make_server()` per connect; the gate starts open.
    pub fn new<S, F>(make_server: F) -> Arc<Self>
    where
        F: Fn() -> S + Send + Sync + 'static,
        S: ServerHandler,
    {
        todo!("Task 4")
    }

    /// Every connect fails with a `BoxError` whose text is `msg` (Go's `connectFunc` returning
    /// `errors.New(msg)`).
    pub fn failing(msg: &str) -> Arc<Self> {
        todo!("Task 4")
    }

    /// How many times `connect` was called (Go `spyTransport.connectCount`,
    /// `gatedSessionTransport.calls`).
    pub fn connects(&self) -> usize {
        todo!("Task 4")
    }

    /// Makes later connects wait for [`open_gate`](ScriptedFactory::open_gate).
    pub fn close_gate(&self) {
        todo!("Task 4")
    }

    /// Releases every waiting connect and lets later ones through (Go `close(transport.gate)`).
    pub fn open_gate(&self) {
        todo!("Task 4")
    }

    /// Waits until a connect has entered; each entry satisfies one call (Go
    /// `<-transport.entered`).
    pub async fn entered(&self) {
        todo!("Task 4")
    }

    /// Makes later connects fail with `msg` (`None` serves again).
    pub fn fail_with(&self, msg: Option<&str>) {
        todo!("Task 4")
    }

    /// Go `spyTransport.lastConn.Close()`: ends the most recent connection's server side and waits
    /// for it, so the client's next request on it fails with a closed transport.
    pub async fn kill_last(&self) {
        todo!("Task 4")
    }

    /// How many server sessions are still running (Go `server.Sessions()`): a closed client
    /// session ends its server session.
    pub fn open_sessions(&self) -> usize {
        todo!("Task 4")
    }
}

#[async_trait::async_trait]
#[allow(unused_variables, clippy::todo)]
impl TransportFactory for ScriptedFactory {
    async fn connect(
        &self,
        client_info: ClientConfig,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        todo!("Task 4")
    }
}
