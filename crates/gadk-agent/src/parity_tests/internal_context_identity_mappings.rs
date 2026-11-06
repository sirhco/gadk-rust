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

use crate::context::builder::test_support::session;
use crate::context::{ContextDelta, InvocationContext};

// parity: adapted: Go compares its two session→Identity literals; Rust has one read path, so compare a context built on the session with one that swapped to it from a different enclosing session (ContextDelta.session), and require every field non-empty
#[test]
fn test_identity_mappings_agree() {
    let s = session("mapping-app", "mapping-user", "mapping-sid");
    let from_builder = InvocationContext::builder()
        .session(s.clone())
        .build_invocation();
    let base = InvocationContext::builder()
        .session(session("enclosing-app", "enclosing-user", "enclosing-sid"))
        .build_invocation();
    let from_delta = base.into_context().with_delta(ContextDelta {
        session: Some(s),
        ..Default::default()
    });
    let a = (
        from_builder.user_id(),
        from_builder.app_name(),
        from_builder.session_id(),
    );
    let b = (
        from_delta.user_id(),
        from_delta.app_name(),
        from_delta.session_id(),
    );
    assert_eq!(a, b, "the two derivations disagree");
    for (name, v) in [("UserID", a.0), ("AppName", a.1), ("SessionID", a.2)] {
        assert!(
            !v.is_empty(),
            "Identity.{name} is empty for a session that sets every accessor"
        );
    }
}
