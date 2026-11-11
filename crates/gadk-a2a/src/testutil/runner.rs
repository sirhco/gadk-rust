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

//! A runner that yields a scripted event sequence. Bodies: plan 5b Task 3.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_agent::{AgentError, EventStream, RunConfig};
use gadk_genai::Content;
use gadk_session::Event;

/// Yields its scripted events on every run and records each call.
pub struct ScriptedRunner {
    events: parking_lot::Mutex<Vec<Result<Event, AgentError>>>,
    calls: parking_lot::Mutex<Vec<(String, String, Content)>>,
}

impl ScriptedRunner {
    /// A runner yielding `events`.
    pub fn new(events: Vec<Result<Event, AgentError>>) -> Self {
        todo!("plan 5b Task 3")
    }

    /// The `(user_id, session_id, msg)` of every run so far.
    pub fn calls(&self) -> Vec<(String, String, Content)> {
        todo!("plan 5b Task 3")
    }
}

#[cfg(feature = "server")]
impl crate::server::Runner for ScriptedRunner {
    fn run(&self, user_id: &str, session_id: &str, msg: Content, cfg: RunConfig) -> EventStream {
        todo!("plan 5b Task 3")
    }
}
