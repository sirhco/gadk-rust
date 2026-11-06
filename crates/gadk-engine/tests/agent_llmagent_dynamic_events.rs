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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Port of Go agent/llmagent/dynamic_events_test.go.

use std::sync::Arc;

use gadk_agent::{AgentConfig, AgentError, InvocationContext};
use gadk_engine::testutil::{TestAgentRunner, collect_events};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_session::Event;

#[tokio::test]
async fn test_session_events_yielded_presence() {
    // Create a custom agent that yields an event and then checks the session events.
    let custom = gadk_agent::new_agent(AgentConfig {
        name: "test_agent".into(),
        run: Some(Arc::new(|ctx: InvocationContext| {
            Box::pin(async_stream::stream! {
                // 1. Yield a test event
                let mut first = Event::new(ctx.platform(), ctx.invocation_id());
                first.response.content = Some(Content::from_text("Initial test event", ROLE_MODEL));
                yield Ok::<Event, AgentError>(first);

                // 2. Check if the event is in the session (`yield` resumes only after the
                // runner has appended the event, Go's iterator semantics).
                let found = ctx.session().events().iter().any(|e| {
                    e.response
                        .content
                        .as_ref()
                        .and_then(|c| c.parts.first())
                        .is_some_and(|p| p.text == "Initial test event")
                });

                // 3. Yield the result of the check
                let mut result = Event::new(ctx.platform(), ctx.invocation_id());
                let text = if found {
                    "Found initial event in session"
                } else {
                    "Did NOT find initial event in session"
                };
                result.response.content = Some(Content::from_text(text, ROLE_MODEL));
                yield Ok::<Event, AgentError>(result);
            })
        })),
        ..Default::default()
    })
    .unwrap();

    let runner = TestAgentRunner::new(custom).await;
    let events = collect_events(runner.run("Hello"))
        .await
        .expect("run failed");
    let results: Vec<String> = events
        .iter()
        .filter_map(|e| {
            e.response
                .content
                .as_ref()
                .and_then(|c| c.parts.first())
                .map(|p| p.text.clone())
        })
        .collect();
    assert!(
        results
            .iter()
            .any(|t| t == "Found initial event in session"),
        "Expected to find initial event in session, but results were: {results:?}"
    );
}
