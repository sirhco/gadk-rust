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

//! Context deltas: how a [`Context`](super::Context) or invocation context is derived from
//! another with some fields changed, including the tool-context variant.
//!
//! Two behaviours worth knowing, neither observable by engine callers:
//! - A callback context derived through a delta inherits `path`, `run_id` and `sub_scheduler`
//!   from its parent.
//! - There is a single cancellation token per context, not one per layer.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gadk_genai::Content;
use gadk_session::{EventActions, Session, ToolConfirmation};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::context::activation::ActivationId;
use crate::context::cancel::CancelScope;
use crate::context::readonly::ContextInner;
use crate::mode::BoundModes;
use crate::scheduler::DynamicSubScheduler;

/// The fields to change when deriving a context. `None` = unchanged; `Some(x)` = set to `x`.
#[derive(Clone, Default)]
pub struct ContextDelta {
    /// The current agent.
    pub agent: Option<Arc<dyn Agent>>,
    /// The branch.
    pub branch: Option<String>,
    /// The isolation scope.
    pub isolation_scope: Option<String>,
    /// Makes the context a tool context: the call id (a new UUID when empty),
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
    /// The cancellation token.
    pub cancellation_token: Option<CancellationToken>,
    /// The actions callbacks and tools record into; prepared (both delta maps allocated).
    pub actions: Option<Arc<parking_lot::Mutex<EventActions>>>,
    /// The user content.
    pub user_content: Option<Option<Content>>,
    /// Resume inputs, keyed by interrupt id.
    pub resumed_inputs: Option<Arc<Map<String, Value>>>,
    /// The output for ancestors.
    pub output_for_ancestors: Option<Vec<String>>,
    /// Swaps the session (a fresh invocation context around a wrapped session).
    pub session: Option<Arc<dyn Session>>,
    /// A new end flag initialised from the parent's `ended()`;
    /// it also clears the function call id, as a fresh invocation has none.
    pub fresh_end_flag: bool,
    /// A fresh invocation context: its end flag starts NOT ended and its invocation-scoped fields
    /// take their zero values (empty isolation scope, no function call id, tool confirmation or
    /// actions, empty path/run id, no sub-scheduler, output-for-ancestors or resumed inputs). The
    /// reset applies first, so the delta's own fields still override it. Cancellation, bound
    /// modes, platform, plugin manager and the shared invocation values carry over.
    pub fresh_invocation: bool,
}

/// Allocates missing delta maps.
pub(crate) fn prepare_event_actions(a: &parking_lot::Mutex<EventActions>) {
    let mut g = a.lock();
    g.state_delta.get_or_insert_with(Map::new);
    g.artifact_delta.get_or_insert_with(BTreeMap::new);
}

/// Fresh prepared actions.
pub(crate) fn new_actions() -> Arc<parking_lot::Mutex<EventActions>> {
    let a = Arc::new(parking_lot::Mutex::new(EventActions::default()));
    prepare_event_actions(&a);
    a
}

impl ContextInner {
    /// Derives a context: applies the delta's fields, including the tool-context variant.
    pub(crate) fn apply(&self, d: ContextDelta) -> ContextInner {
        let forks_invocation = d.fresh_end_flag
            || d.fresh_invocation
            || d.session.is_some()
            || d.agent.is_some()
            || d.branch.is_some()
            || d.isolation_scope.is_some()
            || d.user_content.is_some()
            || d.cancellation_token.is_some()
            || d.bound_modes.is_some();
        let mut n = self.clone();
        if d.fresh_invocation || d.fresh_end_flag {
            // The path of the enclosing context is recorded before any reset.
            n.enclosing_path = if self.path.is_empty() {
                self.enclosing_path.clone()
            } else {
                self.path.clone()
            };
        }
        if forks_invocation {
            // The new context copies the invocation struct, end flag included; a fresh
            // invocation starts not ended. Each new flag is a new activation.
            let ended = !d.fresh_invocation && self.ended.load(Ordering::SeqCst);
            n.ended = Arc::new(AtomicBool::new(ended));
            n.activation = ActivationId::next();
        }
        if d.fresh_invocation {
            n.isolation_scope = String::new();
            n.function_call_id = String::new();
            n.tool_confirmation = None;
            n.actions = None;
            n.path = String::new();
            n.run_id = String::new();
            n.sub_scheduler = None;
            n.output_for_ancestors = None;
            n.resumed_inputs = None;
            // A fresh invocation has no resumption handle.
            n.live_handle = Arc::new(parking_lot::Mutex::new(String::new()));
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
            // A raw token starts a parentless cancel scope.
            n.cancel_scope = CancelScope::root(v);
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
        // A fresh invocation context has no function call id.
        if d.fresh_end_flag {
            n.function_call_id = String::new();
        }
        match d.function_call_id {
            Some(id) => {
                // Tool context: own call id (UUID when empty), own actions, own confirmation.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Map, Value, json};

    use crate::context::builder::test_support::named_agent;
    use crate::context::{ContextDelta, InvocationContext};

    /// A scoped, ended parent with every invocation-scoped field set.
    fn busy_parent() -> crate::context::Context {
        let mut resumed = Map::new();
        resumed.insert("ask".into(), json!("yes"));
        let parent = InvocationContext::builder()
            .agent(named_agent("parent"))
            .isolation_scope("scope-1")
            .branch("root")
            .build()
            .with_delta(ContextDelta {
                function_call_id: Some("fc-1".into()),
                path: Some("wf@1/node@1".into()),
                run_id: Some("1".into()),
                output_for_ancestors: Some(vec!["wf".into()]),
                resumed_inputs: Some(Arc::new(resumed)),
                ..Default::default()
            });
        parent.end_invocation();
        parent
    }

    #[test]
    fn fresh_invocation_resets_invocation_scoped_fields_and_starts_not_ended() {
        let parent = busy_parent();
        let token = parent.cancellation_token().clone();
        let child = parent.with_delta(ContextDelta {
            fresh_invocation: true,
            agent: Some(named_agent("child")),
            ..Default::default()
        });
        assert!(!child.ended(), "Go NewInvocationContext starts not ended");
        assert!(parent.ended(), "the parent's flag is untouched");
        assert_eq!(child.isolation_scope(), "");
        assert_eq!(child.function_call_id(), "");
        assert_eq!(child.path(), "");
        assert_eq!(child.run_id(), "");
        assert!(child.output_for_ancestors().is_empty());
        assert_eq!(child.resumed_input("ask"), None::<Value>);
        assert!(child.actions().state_delta.is_none_or(|d| d.is_empty()));
        // Inherited through the parent context.Context / shared invocation values.
        assert_eq!(child.branch(), "root");
        assert_eq!(child.invocation_id(), parent.invocation_id());
        token.cancel();
        assert!(child.cancellation_token().is_cancelled());
        // Ending the child does not end a sibling.
        child.end_invocation();
        let sibling = parent.with_delta(ContextDelta {
            fresh_invocation: true,
            ..Default::default()
        });
        assert!(!sibling.ended());
    }

    #[test]
    fn fresh_invocation_lets_the_delta_override_the_reset() {
        let child = busy_parent().with_delta(ContextDelta {
            fresh_invocation: true,
            isolation_scope: Some("scope-2".into()),
            path: Some("p".into()),
            ..Default::default()
        });
        assert_eq!(child.isolation_scope(), "scope-2");
        assert_eq!(child.path(), "p");
        assert!(!child.ended());
    }

    #[test]
    fn live_resumption_handle_is_shared_by_every_derived_context() {
        let inv = InvocationContext::builder().build_invocation();
        let child = inv.with_delta(ContextDelta {
            agent: Some(named_agent("child")),
            branch: Some("b".into()),
            ..Default::default()
        });
        let tool = child.clone().into_context().with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            ..Default::default()
        });
        let (cancellable, _guard) = tool.with_agent_cancel();
        cancellable
            .as_invocation()
            .set_live_session_resumption_handle("h1");
        assert_eq!(inv.live_session_resumption_handle(), "h1");
        assert_eq!(child.live_session_resumption_handle(), "h1");
        assert_eq!(tool.as_invocation().live_session_resumption_handle(), "h1");
    }

    #[test]
    fn fresh_invocation_installs_an_empty_resumption_handle_slot() {
        let parent = InvocationContext::builder().build_invocation();
        parent.set_live_session_resumption_handle("old");
        let fresh = parent.with_delta(ContextDelta {
            fresh_invocation: true,
            ..Default::default()
        });
        assert_eq!(
            fresh.live_session_resumption_handle(),
            "",
            "Go NewInvocationContext starts with no handle"
        );
        fresh.set_live_session_resumption_handle("new");
        assert_eq!(
            parent.live_session_resumption_handle(),
            "old",
            "the parent's slot is untouched"
        );
        // A derivation of the fresh context shares the fresh slot.
        let derived = fresh.with_delta(ContextDelta {
            branch: Some("x".into()),
            ..Default::default()
        });
        assert_eq!(derived.live_session_resumption_handle(), "new");
    }

    #[test]
    fn context_path_is_the_own_path_else_the_path_a_fresh_invocation_forked_from() {
        let root = InvocationContext::builder()
            .agent(named_agent("root"))
            .build();
        assert_eq!(root.context_path(), "");
        // Go PromoteWithDelta before RunLLMAgentAsNode: the promoted path reaches the forked task
        // agent's contexts through NewInvocationContext(parentCtx, …).
        let promoted = root.with_delta(ContextDelta {
            path: Some("wf@1/task@1".into()),
            ..Default::default()
        });
        let task = promoted.with_delta(ContextDelta {
            fresh_invocation: true,
            agent: Some(named_agent("task")),
            ..Default::default()
        });
        assert_eq!(task.path(), "", "path() is unchanged");
        assert_eq!(task.context_path(), "wf@1/task@1");
        // Go WithDelta copies (callback / tool contexts, agent swaps, cancel scopes) keep it.
        let tool = task.with_delta(ContextDelta {
            function_call_id: Some("fc".into()),
            branch: Some("b".into()),
            ..Default::default()
        });
        let (cancellable, _g) = tool.with_agent_cancel();
        assert_eq!(cancellable.context_path(), "wf@1/task@1");
        // A nested fresh fork from a pathless context keeps the enclosing path.
        let nested = cancellable.with_delta(ContextDelta {
            fresh_invocation: true,
            ..Default::default()
        });
        assert_eq!(nested.context_path(), "wf@1/task@1");
        // An own path wins; a fork from it records it.
        let own = nested.with_delta(ContextDelta {
            path: Some("inner@1".into()),
            ..Default::default()
        });
        assert_eq!(own.context_path(), "inner@1");
        let forked = own.with_delta(ContextDelta {
            fresh_invocation: true,
            ..Default::default()
        });
        assert_eq!(forked.context_path(), "inner@1");
    }

    #[test]
    fn fresh_end_flag_records_the_enclosing_path_too() {
        // Go agent_node.go:146: AgentNode::run forks with NewInvocationContext(ctx, …).
        let node = InvocationContext::builder()
            .build()
            .with_delta(ContextDelta {
                path: Some("wf@1/agent@1".into()),
                ..Default::default()
            });
        let run = node.with_delta(ContextDelta {
            fresh_end_flag: true,
            ..Default::default()
        });
        assert_eq!(run.context_path(), "wf@1/agent@1");
        // A later reset (fresh invocation) still reaches it.
        let inner = run.with_delta(ContextDelta {
            fresh_invocation: true,
            ..Default::default()
        });
        assert_eq!(inner.context_path(), "wf@1/agent@1");
        // A plain delta that clears the path does not record anything.
        let cleared = InvocationContext::builder()
            .build()
            .with_delta(ContextDelta {
                path: Some("x".into()),
                ..Default::default()
            });
        let cleared = cleared.with_delta(ContextDelta {
            path: Some(String::new()),
            ..Default::default()
        });
        assert_eq!(cleared.context_path(), "");
    }
}
