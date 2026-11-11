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
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

//! Go `cmd/launcher/web/agentengine`: the `agentengine` web sub-launcher, serving
//! [`gadk_server::agentengine`] under `-path_prefix` for Vertex AI Agent Engine.

use std::io::Write;
use std::sync::{Arc, Mutex};

use gadk_platform::GoDuration;
use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_server::agentengine::AgentEngineConfig;

use crate::config::Config;
use crate::error::Error;
use crate::web::{WebRouter, WebSublauncher};

/// Go `agentEngineConfig`'s flag-backed fields.
struct AgentEngineFlags {
    /// `-path_prefix` (default `/api`).
    path_prefix: Flag<String>,
    /// `-max_payload_size` (default 10 MiB).
    max_payload_size: Flag<i64>,
    /// `-sse-write-timeout` (default 120 s).
    sse_write_timeout: Flag<GoDuration>,
}

/// Go `agentEngineLauncher`.
pub struct AgentEngineLauncher {
    /// The `web` flag set (Go names it `"web"`), behind a `Mutex` as `ApiLauncher`'s is.
    flags: Mutex<FlagSet>,
    config: AgentEngineFlags,
    /// Go `config.pathPrefix` after `Parse` normalized it (the flag default until then).
    path_prefix: String,
    /// Go `config.agentEngineID`.
    agent_engine_id: String,
}

/// Go `NewLauncher(agentEngineId)`: the sub-launcher with Go's flags, defaults and usage text.
pub fn new(agent_engine_id: &str) -> AgentEngineLauncher {
    todo!("Task 8")
}

impl AgentEngineLauncher {
    /// Replaces where the flag set writes its errors and usage (Go `fs.SetOutput`; default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> AgentEngineLauncher {
        todo!("Task 8")
    }
}

/// The handler config from the launcher's `Config` (the web launcher has already defaulted
/// the services; a missing session service is `Error::Config`), its `log` the launcher's Go
/// logger.
pub(crate) fn engine_config(config: &Config) -> Result<AgentEngineConfig, Error> {
    todo!("Task 8")
}

impl WebSublauncher for AgentEngineLauncher {
    /// Go `Keyword`: `agentengine`.
    fn keyword(&self) -> String {
        todo!("Task 8")
    }

    /// Go `Parse`: the flags (`failed to parse agent engine flags: <err>`), then the prefix
    /// with a leading `/` added and one trailing `/` trimmed.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        todo!("Task 8")
    }

    /// Go `CommandLineSyntax`: `format_flag_usage` of the flag set.
    fn command_line_syntax(&self) -> String {
        todo!("Task 8")
    }

    /// Go `SimpleDescription`.
    fn simple_description(&self) -> String {
        todo!("Task 8")
    }

    /// Go `SetupSubrouters`: Go's `NewHandler` checks — `Config::validate`, then
    /// `gadk_server::agentengine::new_handler` — either error answered as
    /// `agentengine.NewHandler failed: <err>` (`Error::Config`, Go's `%v`), then the handler
    /// mounted with
    /// `router.handle_prefix(&[Method::POST], prefix, strip_prefix(prefix, handler))`.
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        todo!("Task 8")
    }

    /// Go `UserMessage`: Go's five lines verbatim (the `/api` in the first two is literal, not
    /// the prefix).
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        todo!("Task 8")
    }
}
