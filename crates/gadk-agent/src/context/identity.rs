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

//! The invocation identity value (port of Go `agent.Identity`, agent/common_context.go:58-90).

/// Go `agent.Identity`: who the invocation acts for. Read it with
/// [`ReadonlyContext::identity`](crate::ReadonlyContext::identity).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Identity {
    /// The acting end user, as the embedding server put it on the session (not authenticated).
    pub user_id: String,
    /// The app the invocation belongs to.
    pub app_name: String,
    /// The conversation the invocation belongs to.
    pub session_id: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::context::builder::test_support::session;
    use crate::context::{ContextDelta, InvocationContext};

    /// `identity()` agrees field for field with the three accessors, on every context shape.
    #[test]
    fn identity_agrees_with_the_accessors() {
        for (app, user, sid) in [("app", "user", "sid"), ("app", "", "sid"), ("", "", "")] {
            let ic = InvocationContext::builder()
                .session(session(app, user, sid))
                .build_invocation();
            let ctx = ic.clone().into_context();
            let tool_ctx = ctx.with_delta(ContextDelta {
                function_call_id: Some("fc".into()),
                ..Default::default()
            });
            let want = Identity {
                user_id: user.into(),
                app_name: app.into(),
                session_id: sid.into(),
            };
            for (name, c) in [
                ("invocation", &ic),
                ("context", ctx.as_invocation()),
                ("tool context", tool_ctx.as_invocation()),
            ] {
                assert_eq!(c.identity(), want, "{name}");
                assert_eq!(
                    c.identity(),
                    Identity {
                        user_id: c.user_id().into(),
                        app_name: c.app_name().into(),
                        session_id: c.session_id().into(),
                    },
                    "{name}"
                );
            }
            // Through `Deref` on `Context` itself.
            assert_eq!(ctx.identity(), want);
        }
    }
}
