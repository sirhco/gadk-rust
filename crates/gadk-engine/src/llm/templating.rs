// Copyright 2026 Chris Olson
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

//! Session-state templating for instruction providers.

use gadk_agent::{AgentError, InvocationContext, ReadonlyContext};

/// Renders `template` against the session behind `ctx` with the same templating plain
/// `instruction` strings get: `{var}` (required), `{var?}` (optional), `app:` / `user:` /
/// `temp:` state prefixes and `{artifact.name}`. Use it in an
/// [`InstructionProvider`](crate::llm::InstructionProvider), whose output is otherwise not
/// templated.
///
/// # Errors
///
/// A required state variable or artifact that does not exist, or a failing artifact service.
pub async fn inject_session_state(
    ctx: &ReadonlyContext,
    template: &str,
) -> Result<String, AgentError> {
    let ictx = InvocationContext::from_readonly(ctx);
    crate::llm::processors::instructions::inject_session_state(&ictx, template).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use gadk_agent::{Artifacts, InvocationContext};
    use gadk_artifact::InMemoryArtifactService;
    use gadk_genai::Part;
    use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
    use serde_json::json;

    use super::inject_session_state;
    use crate::artifacts::ScopedArtifacts;

    async fn ctx() -> InvocationContext {
        let svc = InMemorySessionService::new();
        let sess = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: Some("s".into()),
                state: json!({"a": 1}).as_object().cloned(),
            })
            .await
            .unwrap();
        let artifacts: Arc<dyn Artifacts> = Arc::new(ScopedArtifacts::new(
            Arc::new(InMemoryArtifactService::new()),
            "app",
            "u",
            "s",
        ));
        artifacts
            .save("f", Part::from_text("file body"))
            .await
            .unwrap();
        InvocationContext::builder()
            .session(sess)
            .artifacts(artifacts)
            .build_invocation()
    }

    #[tokio::test]
    async fn inject_session_state_public_renders_state_and_optional() {
        let ctx = ctx().await;
        let got = inject_session_state(ctx.as_readonly(), "x {a} {b?}")
            .await
            .unwrap();
        assert_eq!(got, "x 1 ");
    }

    #[tokio::test]
    async fn inject_session_state_public_missing_is_error() {
        let ctx = ctx().await;
        let err = inject_session_state(ctx.as_readonly(), "{missing}")
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                gadk_agent::AgentError::Session(gadk_session::SessionError::StateKeyNotExist)
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn inject_session_state_public_renders_artifact() {
        let ctx = ctx().await;
        let got = inject_session_state(ctx.as_readonly(), "<{artifact.f}>")
            .await
            .unwrap();
        assert_eq!(got, "<file body>");
    }
}
