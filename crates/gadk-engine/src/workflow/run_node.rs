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

//! Port of Go workflow/run_node.go.

use std::sync::Arc;

use gadk_agent::{AgentError, Context};
use serde_json::Value;

use crate::error::WorkflowErrorKind;
use crate::workflow::Node;
use crate::workflow::scheduler::wf_error;

/// Port of Go runNodeOptions and its RunNodeOption functional options.
#[derive(Clone, Default)]
pub struct RunNodeOptions {
    /// Go WithRunID: a stable run id instead of the per-child auto-counter; a repeated id
    /// replays the cached output, and concurrent callers share one run.
    pub run_id: Option<String>,
    /// Go WithUseSubBranch: runs the child on `<branch>.<name>@<run_id>`.
    pub use_sub_branch: bool,
    /// Go WithUseAsOutput: the child's output becomes the parent's output.
    pub use_as_output: bool,
    /// Go WithOverrideBranch: replaces the base branch (empty = no override).
    pub override_branch: Option<String>,
    /// Go WithIsolationScope: the child's isolation scope (empty = none).
    pub isolation_scope: Option<String>,
    /// Go WithIsolationScopeFromNodePath: the child's isolation scope is its node path.
    pub isolation_scope_from_node_path: bool,
    /// Go WithRaiseOnWait: an unresolved long-running call pauses the parent.
    pub raise_on_wait: bool,
}

/// Port of Go workflow.RunNode (with `OUT = any`): schedules `child` under the dynamic node
/// whose context `ctx` is, and returns its output (`Value::Null` for none). Callers convert the
/// value to their type (Go's `OUT` assertion).
pub async fn run_node(
    ctx: &Context,
    child: Arc<dyn Node>,
    input: Value,
    opts: RunNodeOptions,
) -> Result<Value, AgentError> {
    let Some(ss) = ctx.sub_scheduler() else {
        return Err(wf_error(WorkflowErrorKind::InvalidRunNodeContext, ""));
    };
    ss.run_node(Arc::new(child), input, Some(Arc::new(opts)))
        .await
}
