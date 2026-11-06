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

//! Port of Go workflow/synthesize_agent_output_test.go.

use gadk_genai::Content;
use gadk_session::Event;
use serde_json::json;

use crate::workflow::nodes::agent::synthesize_agent_output;

#[test]
fn test_synthesize_agent_output_skips_long_running_tool_event() {
    let mut ev = Event {
        long_running_tool_ids: vec!["fc-pending".into()],
        ..Event::default()
    };
    ev.response.content = Some(Content::from_text("Awaiting your approval…", "model"));
    synthesize_agent_output(&mut ev);
    assert_eq!(ev.output, None, "LRT-bearing events must not be promoted");
    assert!(
        !ev.node_info.as_ref().is_some_and(|i| i.message_as_output),
        "MessageAsOutput must stay unset for LRT pauses"
    );
}

#[test]
fn test_synthesize_agent_output_promotes_plain_model_text() {
    let mut ev = Event::default();
    ev.response.content = Some(Content::from_text("hello", "model"));
    synthesize_agent_output(&mut ev);
    assert_eq!(ev.output, Some(json!("hello")));
    assert!(
        ev.node_info.as_ref().is_some_and(|i| i.message_as_output),
        "MessageAsOutput = true"
    );
}
