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

//! The [`Launcher`] and [`SubLauncher`] traits.

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;

/// Parses command-line arguments and runs the matching logic.
#[async_trait::async_trait]
pub trait Launcher: Send {
    /// Parses `args` and runs; cancelling `cancel` stops the run.
    async fn execute(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
    ) -> Result<(), Error>;
    /// The command-line flags and arguments.
    fn command_line_syntax(&self) -> String;
}

/// One mode of operation (`console`, `web`) inside a parent launcher
/// such as the universal one.
#[async_trait::async_trait]
pub trait SubLauncher: Send + Sync {
    /// The command-line keyword that selects this sub-launcher.
    fn keyword(&self) -> String;
    /// Parses this sub-launcher's arguments and returns the unparsed rest.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error>;
    /// This sub-launcher's flags and arguments.
    fn command_line_syntax(&self) -> String;
    /// A one-line description.
    fn simple_description(&self) -> String;
    /// Runs until done or until `cancel` is cancelled.
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error>;
}
