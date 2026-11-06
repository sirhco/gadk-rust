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

//! Context deltas (port of Go `CommonContextDelta`/`InvocationContextDelta` in
//! `agent/common_context_delta.go`, `invocationContext.WithICDelta` and `NewToolContext`).
//!
//! Two deviations from Go, neither observable by engine callers:
//! - Go's `NewCallbackContext` builds a *fresh* `commonContext`, so a callback context loses `path`,
//!   `run_id` and `sub_scheduler`. Rust deltas inherit them.
//! - Go keeps a separate embedded `context.Context` per layer. Rust has one cancellation token.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gadk_genai::Content;
use gadk_session::{EventActions, Session, ToolConfirmation};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::context::readonly::ContextInner;
use crate::mode::BoundModes;
use crate::scheduler::DynamicSubScheduler;

/// Go `CommonContextDelta` + `InvocationContextDelta`. `None` = unchanged; `Some(x)` = set to `x`.
#[derive(Clone, Default)]
pub struct ContextDelta {
    /// The current agent.
    pub agent: Option<Arc<dyn Agent>>,
    /// The branch.
    pub branch: Option<String>,
    /// The isolation scope.
    pub isolation_scope: Option<String>,
    /// Makes the context a tool context (Go `NewToolContext`): the call id (a new UUID when empty),
    /// the delta's `actions` or fresh ones, and the delta's `tool_confirmation` or none.
    pub function_call_id: Option<String>,
    /// The tool confirmation.
    pub tool_confirmation: Option<Option<ToolConfirmation>>,
    /// The workflow node path.
    pub path: Option<String>,
    /// The workflow node run id.
    pub run_id: Option<String>,
    /// The dynamic sub-scheduler.
    pub sub_scheduler: Option<Option<Arc<dyn DynamicSubScheduler>>>,
    /// The bound agent modes.
    pub bound_modes: Option<BoundModes>,
    /// The cancellation token (Go's `Context`).
    pub cancellation_token: Option<CancellationToken>,
    /// The actions callbacks and tools record into; prepared (both delta maps allocated).
    pub actions: Option<Arc<parking_lot::Mutex<EventActions>>>,
    /// The user content.
    pub user_content: Option<Option<Content>>,
    /// Resume inputs, keyed by interrupt id.
    pub resumed_inputs: Option<Arc<Map<String, Value>>>,
    /// Go `CommonContextDelta.OutputForAncestors`.
    pub output_for_ancestors: Option<Vec<String>>,
    /// Swaps the session (Go: a fresh invocation context around a wrapped session).
    pub session: Option<Arc<dyn Session>>,
    /// A new end flag initialised from the parent's `ended()` (Go copies `Ended()` into a fresh
    /// invocation context); it also clears the function call id, as a fresh invocation has none.
    pub fresh_end_flag: bool,
}

/// Go `prepareEventActions`: allocate missing delta maps.
pub(crate) fn prepare_event_actions(a: &parking_lot::Mutex<EventActions>) {
    let mut g = a.lock();
    g.state_delta.get_or_insert_with(Map::new);
    g.artifact_delta.get_or_insert_with(BTreeMap::new);
}

/// Fresh prepared actions (Go `prepareEventActions(nil)`).
pub(crate) fn new_actions() -> Arc<parking_lot::Mutex<EventActions>> {
    let a = Arc::new(parking_lot::Mutex::new(EventActions::default()));
    prepare_event_actions(&a);
    a
}

impl ContextInner {
    /// Port of Go `commonContext.WithDelta` + `invocationContext.WithICDelta` + `NewToolContext`.
    pub(crate) fn apply(&self, d: ContextDelta) -> ContextInner {
        let forks_invocation = d.fresh_end_flag
            || d.session.is_some()
            || d.agent.is_some()
            || d.branch.is_some()
            || d.isolation_scope.is_some()
            || d.user_content.is_some()
            || d.cancellation_token.is_some()
            || d.bound_modes.is_some();
        let mut n = self.clone();
        if forks_invocation {
            // Go WithICDelta copies the invocationContext struct, endInvocation included.
            n.ended = Arc::new(AtomicBool::new(self.ended.load(Ordering::SeqCst)));
        }
        if let Some(v) = d.session {
            n.session = v;
        }
        if let Some(v) = d.agent {
            n.agent = v;
        }
        if let Some(v) = d.branch {
            n.branch = v;
        }
        if let Some(v) = d.isolation_scope {
            n.isolation_scope = v;
        }
        if let Some(v) = d.user_content {
            n.user_content = v.map(Arc::new);
        }
        if let Some(v) = d.cancellation_token {
            n.cancellation_token = v;
        }
        if let Some(v) = d.bound_modes {
            n.bound_modes = v;
        }
        if let Some(v) = d.resumed_inputs {
            n.resumed_inputs = Some(v);
        }
        if let Some(v) = d.path {
            n.path = v;
        }
        if let Some(v) = d.run_id {
            n.run_id = v;
        }
        if let Some(v) = d.sub_scheduler {
            n.sub_scheduler = v;
        }
        if let Some(v) = d.output_for_ancestors {
            n.output_for_ancestors = Some(v);
        }
        if let Some(a) = &d.actions {
            prepare_event_actions(a);
        }
        // A fresh invocation context (Go icontext.NewInvocationContext) has no function call id.
        if d.fresh_end_flag {
            n.function_call_id = String::new();
        }
        match d.function_call_id {
            Some(id) => {
                // Go NewToolContext: own call id (UUID when empty), own actions, own confirmation.
                n.function_call_id = if id.is_empty() {
                    self.shared.platform.new_uuid()
                } else {
                    id
                };
                n.actions = Some(d.actions.unwrap_or_else(new_actions));
                n.tool_confirmation = d.tool_confirmation.unwrap_or(None);
            }
            None => {
                if let Some(a) = d.actions {
                    n.actions = Some(a);
                }
                if let Some(tc) = d.tool_confirmation {
                    n.tool_confirmation = tc;
                }
            }
        }
        n
    }
}
