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

//! [`ReplayServer`]: serves one of Go's recorded Agent Engine conversations
//! (`testdata/replays/{stem}.json`) strictly, answering each REST request with the first unused
//! recorded call of the same gRPC method and an equal request.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_model::rest::test_util::{FakeServer, RecordedRequest};

/// A strict replay fake on 127.0.0.1.
pub struct ReplayServer {
    server: FakeServer,
}

impl ReplayServer {
    /// Serves `testdata/replays/{stem}.json`; panics if it is missing.
    pub fn start(stem: &str) -> Self {
        todo!()
    }

    /// The server's base URL.
    pub fn url(&self) -> String {
        todo!()
    }

    /// Each unmatched request rendered as
    /// `"{method} {request JSON}\n  closest unused: {recorded JSON or <none>}"`.
    pub fn unmatched(&self) -> Vec<String> {
        todo!()
    }

    /// Every request the client sent, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        todo!()
    }
}
