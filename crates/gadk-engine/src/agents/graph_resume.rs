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

//! Which sub-agents of a sequential, parallel or loop agent hold a caller of the user's function
//! responses, so that a resume of a nested graph workflow skips the siblings that never saw the
//! calls.

use std::collections::BTreeSet;
use std::sync::Arc;

use gadk_agent::{Agent, AgentType, InvocationContext};
use gadk_session::Event;

use crate::tool::utils::{function_calls, function_responses};

/// The latest event holding the function call `call_id`.
pub(crate) fn find_call_event<'a>(events: &'a [Event], call_id: &str) -> Option<&'a Event> {
    events.iter().rev().find(|e| {
        function_calls(e.response.content.as_ref())
            .iter()
            .any(|fc| fc.id == call_id)
    })
}

/// In ascending order, the indexes among the sub-agents of `ctx.agent()` of the ones holding a
/// caller of the function responses in `ctx.user_content()`.
/// `None` (all sub-agents run) when none of the callers is in a graph workflow under
/// `ctx.agent()`, or when `ctx` is marked by
/// [`InvocationContext::with_graph_resume_done`].
///
/// A parallel agent runs the selected sub-agents; a sequential or loop agent starts from the
/// lowest index. A node agent is found through `sub_agents`, so a workflow only matches if its
/// node agents are listed in its sub-agents.
pub(crate) fn graph_resume_indexes(ctx: &InvocationContext) -> Option<Vec<usize>> {
    if ctx.graph_resume_done() {
        return None;
    }
    let responses = function_responses(ctx.user_content());
    // The session is read only when there is a function response to resolve.
    if responses.is_empty() {
        return None;
    }
    let events = ctx.session().events();
    let agent = ctx.agent();
    let mut selected = BTreeSet::new();
    let mut in_graph = false;
    for response in responses {
        let Some(ev) = find_call_event(&events, &response.id) else {
            continue;
        };
        for (i, sub) in agent.sub_agents().iter().enumerate() {
            if sub.find_agent(&ev.author).is_none() {
                continue;
            }
            selected.insert(i);
            in_graph = in_graph || called_from_graph(sub, &ev.author, false);
        }
    }
    if !in_graph {
        return None;
    }
    Some(selected.into_iter().collect())
}

/// Whether the agent named `author` is in the tree of `a`, at or below a graph workflow agent.
fn called_from_graph(a: &Arc<dyn Agent>, author: &str, in_graph: bool) -> bool {
    let in_graph = in_graph || a.agent_state().agent_type == AgentType::WorkflowAgent;
    if a.name() == author {
        return in_graph;
    }
    a.sub_agents()
        .iter()
        .any(|sub| called_from_graph(sub, author, in_graph))
}
