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

use gadk_genai::{Content, ROLE_USER};

use crate::context::builder::test_support::{named_agent, session};
use crate::context::{Context, ContextDelta};

// parity: skipped TestCallbackContextWrapper_LogsForToolContextMethods: Go's callback-context wrapper logs on tool-only methods; Rust has one Context type whose accessors are all supported (Appendix C3)

// parity: adapted: no log capture; asserts the callback context (a Context with actions) reports the invocation's values
#[test]
fn test_callback_context_wrapper_no_log_for_supported_methods() {
    let cc = Context::builder()
        .agent(named_agent("test-agent"))
        .session(session("app", "user", "sess-1"))
        .invocation_id("inv-1")
        .branch("branch-1")
        .user_content(Content::from_text("hi", ROLE_USER))
        .build()
        .with_delta(ContextDelta {
            actions: Some(Default::default()),
            ..Default::default()
        });
    assert_eq!(cc.agent_name(), "test-agent");
    assert_eq!(cc.app_name(), "app");
    assert_eq!(cc.branch(), "branch-1");
    assert_eq!(cc.invocation_id(), "inv-1");
    assert_eq!(cc.session_id(), "sess-1");
    assert_eq!(cc.user_id(), "user");
    assert!(cc.user_content().is_some());
    let _ = cc.readonly_state();
    let _ = cc.state();
}
