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

//! Part of the port of ADK-Go `plugin/agentanalytics/late_callbacks_test.go`: the
//! 1 Go tests from `TestRunner_AfterRunEventKeepsInvocationSpan` to
//! `TestRunner_AfterRunEventKeepsInvocationSpan` in the Go file's order that run a gadk `Runner`
//! (the exact list is in the 7b-3a plan's contracts, Appendix A).
//!
//! Go `requireInvocationCompletedOK` and `textContent` are the events port's helpers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::{FutureExt, StreamExt};
use gadk_agent::{AgentConfig, AgentRunFn, InvocationContext, Plugin, PluginConfig, RunConfig};
use gadk_session::Event;

use crate::event_types::{EVENT_AGENT_RESPONSE, EVENT_INVOCATION_COMPLETED};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    json_object_column, new_test_runner, only_row_of_type, place_of,
    require_invocation_completed_ok, rows_of_type, run_turn, setup_test_plugin, text_content,
    until_invocation_end,
};

#[tokio::test]
async fn test_runner_after_run_event_keeps_invocation_span() {
    let tp = setup_test_plugin().await;
    let emitted = Arc::new(AtomicBool::new(false));
    let on_event = tp.plugin.on_event_callback().expect("on-event callback");
    let observer = Plugin::new(PluginConfig {
        name: "derived_context".into(),
        after_run_callback: Some({
            let emitted = Arc::clone(&emitted);
            Arc::new(move |ctx: InvocationContext| {
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.author = "derived".into();
                ev.response.content = Some(text_content("model", "derived context"));
                ev.response.turn_complete = true;
                let on_event = Arc::clone(&on_event);
                let emitted = Arc::clone(&emitted);
                async move {
                    if let Err(err) = on_event(ctx, ev).await {
                        panic!("{err}");
                    }
                    emitted.store(true, Ordering::SeqCst);
                }
                .boxed()
            })
        }),
        ..PluginConfig::default()
    });
    let run: AgentRunFn = Arc::new(|ctx: InvocationContext| {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.author = "plain".into();
        ev.response.content = Some(text_content("model", "answer"));
        ev.response.turn_complete = true;
        futures::stream::iter([Ok(ev)]).boxed()
    });
    let root = gadk_agent::new_agent(AgentConfig {
        name: "plain".into(),
        run: Some(run),
        ..AgentConfig::default()
    })
    .expect("agent");
    let r = new_test_runner(root, vec![tp.plugin.clone(), observer]);
    let (_, errs) = run_turn(&r, "u1", "s1", "question", RunConfig::default()).await;
    assert!(errs.is_empty(), "run errors: {errs:?}");
    assert!(
        emitted.load(Ordering::SeqCst),
        "derived event callback did not run"
    );
    let rows = tp
        .collect_rows(|rows| {
            until_invocation_end(rows) && rows_of_type(rows, EVENT_AGENT_RESPONSE).len() == 2
        })
        .await;
    require_invocation_completed_ok(&rows);
    let want = place_of(&only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED));
    let mut derived_rows = 0;
    for row in rows_of_type(&rows, EVENT_AGENT_RESPONSE) {
        if json_object_column(&row, "attributes")["event_author"] == "derived" {
            derived_rows += 1;
            assert_eq!(
                place_of(&row),
                want,
                "derived event does not retain the completed invocation's span identity"
            );
        }
    }
    assert_eq!(derived_rows, 1, "got {derived_rows} derived rows, want 1");
}
