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

//! [`FakeAgentEngine`]: a stateful Agent Engine fake (sessions, events, operations) with Go
//! `fakeSessions`'s seeded behaviour.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_model::rest::test_util::{FakeServer, RecordedRequest};
use serde_json::Value;

/// A stateful Agent Engine fake on 127.0.0.1.
pub struct FakeAgentEngine {
    server: FakeServer,
}

impl FakeAgentEngine {
    /// Starts the fake.
    pub fn start() -> Self {
        todo!()
    }

    /// The server's base URL.
    pub fn url(&self) -> String {
        todo!()
    }

    /// Requests per gRPC method short name (`"CreateSession"`, `"GetSession"`,
    /// `"ListSessions"`, `"DeleteSession"`, `"AppendEvent"`, `"ListEvents"`, `"GetOperation"`).
    pub fn hits(&self, method: &str) -> usize {
        todo!()
    }

    /// Every request the client sent, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        todo!()
    }

    /// When true, create and delete return `done: false` and the first `/ui/` poll returns done.
    pub fn set_operations_pending(&self, pending: bool) {
        todo!()
    }

    /// The stored session (`userId`, `sessionState`) of `name`, if any.
    pub fn session(&self, name: &str) -> Option<Value> {
        todo!()
    }

    /// The stored events of session `name` (as appended, after name assignment).
    pub fn events(&self, name: &str) -> Vec<Value> {
        todo!()
    }
}
