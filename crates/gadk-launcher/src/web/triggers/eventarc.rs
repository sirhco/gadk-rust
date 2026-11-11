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

//! Go `cmd/launcher/web/triggers/eventarc`: the Eventarc trigger endpoint (Task 11).
// Contract stub (plan D1): Task 11 writes the bodies.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use crate::config::Config;
use crate::error::Error;
use crate::web::{WebRouter, WebSublauncher};

/// Go `eventarc.eventarcLauncher`.
pub struct EventarcLauncher {}

/// Go `eventarc.NewLauncher()`.
pub fn new() -> EventarcLauncher {
    EventarcLauncher {}
}

impl WebSublauncher for EventarcLauncher {
    fn keyword(&self) -> String {
        todo!("Task 11")
    }
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        todo!("Task 11")
    }
    fn command_line_syntax(&self) -> String {
        todo!("Task 11")
    }
    fn simple_description(&self) -> String {
        todo!("Task 11")
    }
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        todo!("Task 11")
    }
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        todo!("Task 11")
    }
}
