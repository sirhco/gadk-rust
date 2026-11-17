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

//! Choosing the agent that handles the next request: from the session history and any function
//! response, the active task isolation scope, and whether an agent can transfer to its parent.

use std::collections::HashSet;
use std::sync::Arc;

use gadk_agent::{Agent, AgentError, ParentMap};
use gadk_genai::Content;
use gadk_session::{Event, Session};
use serde_json::Value;

use super::RunnerInner;
use crate::agents::graph_resume::find_call_event;
use crate::llm::state::reveal;
use crate::tool::finish_task::{FINISH_TASK_SUCCESS_RESULT, FINISH_TASK_TOOL_NAME};
use crate::tool::utils::function_responses;

/// The most recent isolation scope not yet closed by a successful `finish_task` function response;
/// `""` when there is none.
pub(crate) fn find_active_task_isolation_scope(sess: Option<&dyn Session>) -> String {
    let Some(sess) = sess else {
        return String::new();
    };
    let events = sess.events();
    let mut finished: HashSet<String> = HashSet::new();
    for ev in events.iter().rev() {
        if ev.isolation_scope.is_empty() {
            continue;
        }
        let scope = &ev.isolation_scope;
        // The first finish_task response in the event decides.
        if let Some(fr) = function_responses(ev.response.content.as_ref())
            .into_iter()
            .find(|fr| fr.name == FINISH_TASK_TOOL_NAME)
            && fr.response.get("result").and_then(Value::as_str) == Some(FINISH_TASK_SUCCESS_RESULT)
        {
            finished.insert(scope.clone());
        }
        if finished.contains(scope) {
            continue;
        }
        return scope.clone();
    }
    String::new()
}

/// The event holding the function call that the first function response in `msg` answers.
pub(crate) fn handle_user_function_call_response<'a>(
    events: &'a [Event],
    msg: Option<&Content>,
) -> Option<&'a Event> {
    if events.is_empty() {
        return None;
    }
    // Every response in the message is assumed to answer calls made by the same agent.
    let responses = function_responses(msg);
    find_call_event(events, &responses.first()?.id)
}

/// Whether `agent` and its parent chain are LlmAgents that all allow transfer to their parent.
pub(crate) fn is_transferable(parents: &ParentMap, agent: &Arc<dyn Agent>) -> bool {
    let mut cur = Some(agent.clone());
    while let Some(a) = cur {
        match reveal(&*a) {
            None => return false,
            Some(s) if s.cfg.disallow_transfer_to_parent => return false,
            Some(_) => {}
        }
        cur = parents.parent(a.name());
    }
    true
}

/// Test access to these crate-private methods.
#[cfg(test)]
impl super::Runner {
    /// See `RunnerInner::find_agent_to_run`.
    pub(crate) fn find_agent_to_run(
        &self,
        session: &dyn Session,
        msg: Option<&Content>,
    ) -> Result<Arc<dyn Agent>, AgentError> {
        self.inner.find_agent_to_run(session, msg)
    }

    /// See [`is_transferable`].
    pub(crate) fn is_transferable_across_agent_tree(&self, agent: &Arc<dyn Agent>) -> bool {
        is_transferable(&self.inner.parents, agent)
    }
}

impl RunnerInner {
    /// The agent that should handle the next request, from session history; the root when none
    /// fits.
    pub(super) fn find_agent_to_run(
        &self,
        session: &dyn Session,
        msg: Option<&Content>,
    ) -> Result<Arc<dyn Agent>, AgentError> {
        let events = session.events();
        if let Some(event) = handle_user_function_call_response(&events, msg) {
            if let Some(sub) = self.root_agent.find_agent(&event.author) {
                return Ok(sub);
            }
            tracing::warn!(
                "Function call from an unknown agent: {}, event id: {}",
                event.author,
                event.id
            );
        }
        for event in events.iter().rev() {
            if event.author == "user" {
                continue;
            }
            let Some(sub) = self.root_agent.find_agent(&event.author) else {
                // Agent not found: keep looking.
                tracing::warn!(
                    "Event from an unknown agent: {}, event id: {}",
                    event.author,
                    event.id
                );
                continue;
            };
            if is_transferable(&self.parents, &sub) {
                return Ok(sub);
            }
        }
        // Falls back to the root agent when no suitable agent is found in the session.
        Ok(self.root_agent.clone())
    }
}
