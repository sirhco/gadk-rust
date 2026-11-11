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

//! Go `cmd/launcher/console`: runs an agent interactively on stdin/stdout (Task 6).
// Contract stub (plan D1): Task 6 writes the bodies.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

mod hitl;

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::launcher::SubLauncher;

/// Line source for the console (default: stdin read on a blocking task).
pub type InputLines = Box<dyn Iterator<Item = std::io::Result<String>> + Send>;
/// Output sink for the console's `fmt.Print*` text (default: stdout).
pub type Output = std::sync::Arc<std::sync::Mutex<dyn std::io::Write + Send>>;

/// Go `console.consoleLauncher`.
pub struct ConsoleLauncher {}

/// Go `console.NewLauncher()`.
pub fn new() -> ConsoleLauncher {
    ConsoleLauncher {}
}

impl ConsoleLauncher {
    /// Replaces stdin/stdout and the char-device check (tests, replay).
    pub fn with_io(
        self,
        input: InputLines,
        output: Output,
        stdout_is_char_device: bool,
    ) -> ConsoleLauncher {
        todo!("Task 6")
    }
}

#[async_trait::async_trait]
impl SubLauncher for ConsoleLauncher {
    fn keyword(&self) -> String {
        todo!("Task 6")
    }
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        todo!("Task 6")
    }
    fn command_line_syntax(&self) -> String {
        todo!("Task 6")
    }
    fn simple_description(&self) -> String {
        todo!("Task 6")
    }
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error> {
        todo!("Task 6")
    }
}
