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

//! The executor context handed to callbacks (port of ADK-Go
//! `server/adka2a/v2/executor_context.go`). The implementation: plan 5b Task 7.

use gadk_genai::Content;
use gadk_session::Event;
use serde_json::Value;

/// Go `ExecutorContext`: what the after-event and after-execute callbacks see of an execution.
pub trait ExecutorContext: Send + Sync {
    /// The session ID (the A2A context ID).
    fn session_id(&self) -> &str;
    /// The ADK user ID of the request.
    fn user_id(&self) -> &str;
    /// The root agent's name.
    fn agent_name(&self) -> &str;
    /// The session state; empty until the executor plugin captured the invocation session.
    fn readonly_state(&self) -> serde_json::Map<String, Value>;
    /// The session events; empty until the executor plugin captured the invocation session.
    fn events(&self) -> Vec<Event>;
    /// The converted A2A message passed to the runner.
    fn user_content(&self) -> &Content;
    /// The original A2A request context.
    fn request_context(&self) -> &a2a_server::ExecutorContext;
    /// What the before-execute callback stored (Go's returned `context.Context` values).
    fn extensions(&self) -> &http::Extensions;
}
