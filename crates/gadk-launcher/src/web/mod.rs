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

//! Go `cmd/launcher/web`: the web server and its sub-launchers (Task 7).
// Contract stub (plan D1): Task 7 writes the bodies; the module declarations are final.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

#[cfg(feature = "a2a")]
pub mod a2a;
pub mod api;
mod router;
mod serve;
#[cfg(feature = "triggers")]
pub mod triggers;
#[cfg(feature = "webui")]
pub mod webui;

pub use self::router::{Middleware, WebRouter, axum_handler};

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::golog::Logger;
use crate::launcher::{Launcher, SubLauncher};

/// Go `web.Sublauncher`.
pub trait WebSublauncher: Send + Sync {
    /// Go `Keyword()`.
    fn keyword(&self) -> String;
    /// Go `Parse(args)`: parses this sub-launcher's flags and returns the unparsed rest.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error>;
    /// Go `CommandLineSyntax()`.
    fn command_line_syntax(&self) -> String;
    /// Go `SimpleDescription()`.
    fn simple_description(&self) -> String;
    /// Go `SetupSubrouters(router, config)`: mounts this sub-launcher's routes.
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error>;
    /// Go `UserMessage(webURL, printer)`: the startup lines this sub-launcher prints.
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str));
}

/// Go `web.webLauncher`.
pub struct WebLauncher {
    sublaunchers: Vec<Box<dyn WebSublauncher>>,
}

/// Go `web.NewLauncher(sublaunchers...)`.
pub fn new(sublaunchers: Vec<Box<dyn WebSublauncher>>) -> WebLauncher {
    WebLauncher { sublaunchers }
}

#[async_trait::async_trait]
impl SubLauncher for WebLauncher {
    fn keyword(&self) -> String {
        todo!("Task 7")
    }
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        todo!("Task 7")
    }
    fn command_line_syntax(&self) -> String {
        todo!("Task 7")
    }
    fn simple_description(&self) -> String {
        todo!("Task 7")
    }
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error> {
        todo!("Task 7")
    }
}

#[async_trait::async_trait]
impl Launcher for WebLauncher {
    async fn execute(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
    ) -> Result<(), Error> {
        todo!("Task 7")
    }
    fn command_line_syntax(&self) -> String {
        todo!("Task 7")
    }
}

/// Go `BuildBaseRouter()`: StrictSlash(true) + the request logger writing to `log`.
pub fn build_base_router(log: &Logger) -> WebRouter {
    todo!("Task 7")
}
