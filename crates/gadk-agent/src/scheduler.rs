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

//! Dynamic sub-scheduling (port of Go `agent/dynamic_scheduler.go`).

use std::any::Any;
use std::sync::Arc;

use serde_json::Value;

use crate::error::AgentError;

/// Go `agent.DynamicSubScheduler`: schedules and runs child nodes dynamically, resolving their
/// outputs for ancestor agents.
#[async_trait::async_trait]
pub trait DynamicSubScheduler: Send + Sync {
    /// Go `RunNode`: runs `node` with `input` and the engine-defined `opts`.
    async fn run_node(
        &self,
        node: Arc<dyn Any + Send + Sync>,
        input: Value,
        opts: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Result<Value, AgentError>;
    /// Go `ParentPath`.
    fn parent_path(&self) -> String;
    /// Go `OutputForAncestors`.
    fn output_for_ancestors(&self) -> Vec<String>;
    /// Go `DelegatedOutput`: `None` for Go's `(nil, false)`.
    fn delegated_output(&self) -> Option<Value>;
    /// Go `ResolveByRunID`.
    fn resolve_by_run_id(&self, child_name: &str, custom: &str) -> Result<String, AgentError>;
}
