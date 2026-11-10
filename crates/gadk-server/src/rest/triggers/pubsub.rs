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

//! Port of Go `controllers/triggers/pubsub.go` (plan 5a Task 10).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use super::{ControllerConfig, RetriableRunner};
use crate::rest::ServerError;
use crate::rest::routing::{Request, Response};

/// Port of Go `triggers.PubSubController`.
pub struct PubSubController {
    runner: RetriableRunner,
}

impl PubSubController {
    /// Go `NewPubSubControllerWithConfig`: fails when the compaction config cannot serve the loader's apps.
    pub fn new(cfg: ControllerConfig) -> Result<Self, ServerError> {
        todo!("plan 5a Task 10")
    }

    /// Go `PubSubTriggerHandler`.
    pub async fn pub_sub_trigger_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 10")
    }

    /// The trigger endpoint (POST) as an axum `MethodRouter` to mount at a path of the caller's
    /// choosing.
    pub fn handler(self: Arc<Self>) -> axum::routing::MethodRouter {
        todo!("plan 5a Task 10")
    }
}
