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

//! Port of Go agent/workflowagent/reentry_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use gadk_agent::{Agent, Context};
use gadk_session::{Event, RequestInput};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::agent_workflowagent_hitl::{
    FakeSession, HitlNode, chain, drain_agent, find_request, make_agent, new_counting_handler_node,
    new_mock_ctx, new_string_handler_node, resume_message, run_fresh_turn, start,
};
use crate::workflow::{Node, NodeConfig, new_request_input_event};

/// Go activations: (input, resumed snapshot) per activation, in order.
type Activations = Arc<Mutex<Vec<(Value, Map<String, Value>)>>>;

/// Go askerForSequence: a re-entry (`rerun_on_resume`) node that pauses on the first unanswered
/// id, recording the input and resumed map of every activation, then emits `final_output`.
fn asker_for_sequence(
    name: &str,
    ids: &[&str],
    final_output: Value,
) -> (Arc<dyn Node>, Activations) {
    let acts: Activations = Arc::new(Mutex::new(Vec::new()));
    let ids: Vec<String> = ids.iter().map(|s| (*s).to_string()).collect();
    let acts2 = acts.clone();
    let node = HitlNode::new(
        name,
        NodeConfig {
            rerun_on_resume: Some(true),
            ..Default::default()
        },
        move |ctx: Context, input: Value| {
            // Go snapshotResumed.
            let mut resumed = Map::new();
            for id in &ids {
                if let Some(v) = ctx.resumed_input(id) {
                    resumed.insert(id.clone(), v);
                }
            }
            acts2.lock().push((input, resumed.clone()));
            // Pause on the first id without a response.
            if let Some(id) = ids.iter().find(|id| !resumed.contains_key(*id)) {
                return vec![Ok(new_request_input_event(
                    &ctx,
                    RequestInput {
                        interrupt_id: id.clone(),
                        message: id.clone(),
                        ..Default::default()
                    },
                ))];
            }
            // All answered: emit the terminal output.
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(final_output.clone());
            vec![Ok(ev)]
        },
    );
    (node, acts)
}

/// Go resumeAndExpect: one resume turn, then the next pause's interrupt id ("" for none).
async fn resume_and_expect(
    sess: &Arc<FakeSession>,
    a: &Arc<dyn Agent>,
    reply_id: &str,
    reply: Value,
    want_next: &str,
) -> Vec<Event> {
    let events = drain_agent(
        sess,
        a.run(new_mock_ctx(
            sess,
            a,
            Some(resume_message(reply_id, reply.clone())),
        )),
        None,
    )
    .await;
    assert_eq!(
        find_request(&events),
        want_next,
        "after resume {reply_id}={reply}"
    );
    events
}

#[tokio::test]
async fn test_workflow_agent_re_entry_resumes_at_same_node() {
    let (asker, acts) = asker_for_sequence("asker", &["decide"], json!("decision"));
    let handler_input = Arc::new(Mutex::new(None));
    let handler = new_string_handler_node("handler", handler_input.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    assert_eq!(
        find_request(&run_fresh_turn(&sess, &a, "draft").await),
        "decide"
    );
    resume_and_expect(&sess, &a, "decide", json!("approve"), "").await;

    let acts = acts.lock();
    assert_eq!(acts.len(), 2, "one initial + one re-entry");
    assert_eq!(acts[1].1.get("decide"), Some(&json!("approve")));
    assert_eq!(handler_input.lock().as_deref(), Some("decision"));
}

#[tokio::test]
async fn test_workflow_agent_re_entry_preserves_original_input() {
    let (asker, acts) = asker_for_sequence("asker", &["decide"], json!("ack"));
    let a = make_agent(chain(&[start(), asker]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "draft").await;
    // The re-entry must see "draft" again as input, not the response.
    resume_and_expect(&sess, &a, "decide", json!("approve"), "").await;

    let acts = acts.lock();
    assert_eq!(acts.len(), 2, "activations");
    assert_eq!(acts[0].0, json!("draft"), "initial activation input");
    assert_eq!(
        acts[1].0,
        json!("draft"),
        "re-entry activation input (must preserve original input, not the response)"
    );
}

#[tokio::test]
async fn test_workflow_agent_re_entry_no_successor_before_output() {
    let count = Arc::new(AtomicI32::new(0));
    let (asker, _acts) = asker_for_sequence("asker", &["decide"], json!("ack"));
    let handler = new_counting_handler_node("handler", count.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "handler ran during pause turn"
    );

    resume_and_expect(&sess, &a, "decide", json!("yes"), "").await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "handler runs after re-entry"
    );
}

#[tokio::test]
async fn test_workflow_agent_re_entry_default_mode_is_handoff() {
    let activations = Arc::new(AtomicI32::new(0));
    let acts = activations.clone();
    let asker = HitlNode::new("asker", NodeConfig::default(), move |ctx, _input| {
        acts.fetch_add(1, Ordering::SeqCst);
        vec![Ok(new_request_input_event(
            &ctx,
            RequestInput {
                interrupt_id: "decide".into(),
                message: "?".into(),
                ..Default::default()
            },
        ))]
    });
    let handler_input = Arc::new(Mutex::new(None));
    let handler = new_string_handler_node("handler", handler_input.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("decide", json!("approve"))),
        )),
        None,
    )
    .await;

    assert_eq!(
        activations.load(Ordering::SeqCst),
        1,
        "handoff mode must NOT re-activate the asker"
    );
    assert_eq!(
        handler_input.lock().as_deref(),
        Some("approve"),
        "handoff mode delivers the response as the next node's input"
    );
}

#[tokio::test]
async fn test_workflow_agent_re_entry_accumulates_resume_inputs() {
    let (asker, acts) =
        asker_for_sequence("asker", &["ask_1", "ask_2", "ask_3"], json!("all answered"));
    let handler_input = Arc::new(Mutex::new(None));
    let handler = new_string_handler_node("handler", handler_input.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    assert_eq!(
        find_request(&run_fresh_turn(&sess, &a, "draft").await),
        "ask_1"
    );
    resume_and_expect(&sess, &a, "ask_1", json!("yes"), "ask_2").await;
    resume_and_expect(&sess, &a, "ask_2", json!("ok"), "ask_3").await;
    resume_and_expect(&sess, &a, "ask_3", json!("approve"), "").await;

    assert_eq!(handler_input.lock().as_deref(), Some("all answered"));

    // What the asker must have observed on each activation.
    let want: Vec<Value> = vec![
        json!({}),
        json!({"ask_1": "yes"}),
        json!({"ask_1": "yes", "ask_2": "ok"}),
        json!({"ask_1": "yes", "ask_2": "ok", "ask_3": "approve"}),
    ];
    let acts = acts.lock();
    assert_eq!(acts.len(), want.len(), "activations");
    for (i, w) in want.iter().enumerate() {
        assert_eq!(
            &Value::Object(acts[i].1.clone()),
            w,
            "activation {i} resumed"
        );
    }
}
