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

//! Parity port of Go internal/llminternal/instruction_processor_test.go.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_agent::{AgentError, Artifacts, InvocationContext};
use gadk_artifact::{ArtifactError, ArtifactService, InMemoryArtifactService};
use gadk_genai::Part;
use gadk_session::{
    CreateRequest, Event, InMemorySessionService, Session, SessionError, SessionService, State,
};
use serde_json::{Value, json};

use crate::{
    artifacts::ScopedArtifacts,
    llm::processors::instructions::{inject_session_state, replace_match},
};

/// Go errArtifactService: every Load fails with `err`; other methods are unsupported.
struct ErrArtifactService(fn() -> ArtifactError);

fn unsupported() -> ArtifactError {
    ArtifactError::InvalidRequest("unsupported".into())
}

#[async_trait]
impl ArtifactService for ErrArtifactService {
    async fn save(
        &self,
        _: gadk_artifact::SaveRequest,
    ) -> Result<gadk_artifact::SaveResponse, ArtifactError> {
        Err(unsupported())
    }
    async fn load(
        &self,
        _: gadk_artifact::LoadRequest,
    ) -> Result<gadk_artifact::LoadResponse, ArtifactError> {
        Err((self.0)())
    }
    async fn delete(&self, _: gadk_artifact::DeleteRequest) -> Result<(), ArtifactError> {
        Err(unsupported())
    }
    async fn list(
        &self,
        _: gadk_artifact::ListRequest,
    ) -> Result<gadk_artifact::ListResponse, ArtifactError> {
        Err(unsupported())
    }
    async fn versions(
        &self,
        _: gadk_artifact::VersionsRequest,
    ) -> Result<gadk_artifact::VersionsResponse, ArtifactError> {
        Err(unsupported())
    }
    async fn get_artifact_version(
        &self,
        _: gadk_artifact::GetArtifactVersionRequest,
    ) -> Result<gadk_artifact::GetArtifactVersionResponse, ArtifactError> {
        Err(unsupported())
    }
}

enum Svc {
    Default,
    Custom(fn() -> ArtifactError),
    Nil,
}

struct Case {
    name: &'static str,
    template: &'static str,
    state: Value,
    artifacts: Vec<(&'static str, &'static str)>,
    svc: Svc,
    want: &'static str,
    want_err: Option<&'static str>,
}

const MY_FILE: (&str, &str) = ("my_file", "This is my artifact content.");

#[tokio::test]
async fn test_inject_session_state() {
    let cases = vec![
        Case {
            name: "successful state injection",
            template: "Hello {user_name}, you are in {app_state} state.",
            state: json!({"user_name": "Foo", "app_state": "active"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Hello Foo, you are in active state.",
            want_err: None,
        },
        Case {
            name: "successful artifact injection",
            template: "The artifact content is: {artifact.my_file}",
            state: json!({}),
            artifacts: vec![MY_FILE],
            svc: Svc::Default,
            want: "The artifact content is: This is my artifact content.",
            want_err: None,
        },
        Case {
            name: "optional missing state variable",
            template: "Optional value: {optional_value?}",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Optional value: ",
            want_err: None,
        },
        Case {
            name: "missing required state variable",
            template: "Hello {missing_key}!",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "",
            want_err: Some("state key does not exist"),
        },
        Case {
            name: "dollar-brace pattern is not replaced",
            template: "The formatString supports interpolation via ${expression} syntax.",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "The formatString supports interpolation via ${expression} syntax.",
            want_err: None,
        },
        Case {
            name: "dollar-brace pattern next to state variable",
            template: "Hello {user_name}! Interpolation via ${expression} syntax.",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Hello Foo! Interpolation via ${expression} syntax.",
            want_err: None,
        },
        Case {
            name: "escaped braces are not replaced",
            template: r"Literal \{expression\} syntax.",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Default,
            want: r"Literal \{expression\} syntax.",
            want_err: None,
        },
        Case {
            name: "escaped opening brace is not replaced",
            template: r"Literal \{expression} syntax.",
            state: json!({"expression": "foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: r"Literal \{expression} syntax.",
            want_err: None,
        },
        Case {
            name: "dollar-double-brace pattern is not replaced",
            template: "Workflow syntax: ${{expression}} and {user_name}.",
            state: json!({"expression": "foo", "user_name": "bar"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Workflow syntax: ${{expression}} and bar.",
            want_err: None,
        },
        Case {
            name: "placeholder at start of template",
            template: "{user_name}, welcome",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Foo, welcome",
            want_err: None,
        },
        Case {
            name: "dollar-brace pattern at start of template",
            template: "${user_name}, welcome",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "${user_name}, welcome",
            want_err: None,
        },
        Case {
            name: "double-brace placeholder is replaced",
            template: "Say {{user_name}}",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Say Foo",
            want_err: None,
        },
        Case {
            name: "missing required artifact",
            template: "The artifact content is: {artifact.missing_file}",
            state: json!({}),
            artifacts: vec![MY_FILE],
            svc: Svc::Default,
            want: "",
            want_err: Some(
                "failed to load artifact missing_file: artifact not found: file does not exist",
            ),
        },
        Case {
            name: "invalid state name is not replaced",
            template: "Hello {invalid-key}!",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Hello {invalid-key}!",
            want_err: None,
        },
        Case {
            name: "invalid prefix state name is not replaced",
            template: "Hello {invalid:key}!",
            state: json!({"user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Hello {invalid:key}!",
            want_err: None,
        },
        Case {
            name: "valid prefixed state variable",
            template: "Hello {app:user_name}!",
            state: json!({"app:user_name": "Foo"}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Hello Foo!",
            want_err: None,
        },
        Case {
            name: "state value is nil",
            template: "Value: {test_key}",
            state: json!({"test_key": null}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "Value: ",
            want_err: None,
        },
        Case {
            name: "optional missing artifact",
            template: "Optional artifact: {artifact.missing_file?}",
            state: json!({}),
            artifacts: vec![MY_FILE],
            svc: Svc::Default,
            want: "Optional artifact: ",
            want_err: None,
        },
        Case {
            name: "optional artifact with systemic backend error propagates",
            template: "Content: {artifact.my_file?}",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Custom(|| ArtifactError::Backend("backend down".into())),
            want: "",
            want_err: Some("backend down"),
        },
        Case {
            name: "optional artifact with fs.ErrNotExist renders empty",
            template: "Content: {artifact.my_file?}",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Custom(|| ArtifactError::NotFound),
            want: "Content: ",
            want_err: None,
        },
        Case {
            name: "artifact service not initialized",
            template: "The artifact content is: {artifact.my_file}",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Nil,
            want: "",
            want_err: Some("artifact service is not initialized"),
        },
        Case {
            name: "empty artifact name",
            template: "The artifact content is: {artifact.}",
            state: json!({}),
            artifacts: vec![],
            svc: Svc::Default,
            want: "",
            want_err: Some(
                "failed to load artifact : request validation failed: invalid load request: missing required fields: FileName",
            ),
        },
        Case {
            name: "complex template with mixed variables and artifacts",
            template: "
Hello {user_name},
You are {user_age} years old.
Your favorite color is {favorite_color}.
The artifact says: {artifact.my_file}
And another optional artifact: {artifact.other_file?}
",
            state: json!({"user_name": "Foo", "user_age": 30, "favorite_color": "blue"}),
            artifacts: vec![MY_FILE],
            svc: Svc::Default,
            want: "
Hello Foo,
You are 30 years old.
Your favorite color is blue.
The artifact says: This is my artifact content.
And another optional artifact: 
",
            want_err: None,
        },
    ];
    for tc in cases {
        let svc = InMemorySessionService::new();
        let sess = svc
            .create(CreateRequest {
                app_name: "testApp".into(),
                user_id: "testUser".into(),
                session_id: Some("testSession".into()),
                state: tc.state.as_object().cloned(),
            })
            .await
            .unwrap();
        let artifacts: Option<Arc<dyn Artifacts>> = match tc.svc {
            Svc::Nil => None,
            Svc::Default => Some(Arc::new(ScopedArtifacts::new(
                Arc::new(InMemoryArtifactService::new()),
                "testApp",
                "testUser",
                "testSession",
            ))),
            Svc::Custom(f) => Some(Arc::new(ScopedArtifacts::new(
                Arc::new(ErrArtifactService(f)),
                "testApp",
                "testUser",
                "testSession",
            ))),
        };
        if let Some(a) = &artifacts {
            for (n, t) in &tc.artifacts {
                a.save(
                    n,
                    Part {
                        text: (*t).into(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            }
        }
        let mut b = InvocationContext::builder().session(sess);
        if let Some(a) = artifacts {
            b = b.artifacts(a);
        }
        let ctx = b.build_invocation();
        let got = inject_session_state(&ctx, tc.template).await;
        match tc.want_err {
            Some(w) => {
                let e = got.expect_err(tc.name).to_string();
                assert!(e.contains(w), "{}: error {e:?} lacks {w:?}", tc.name);
            }
            None => assert_eq!(got.unwrap(), tc.want, "{}", tc.name),
        }
    }
}

/// Go errorMockState: "systemic_error" fails with a backend error, every other key is missing.
struct ErrorMockState;

impl State for ErrorMockState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        if key == "systemic_error" {
            return Err(SessionError::Backend("database connection refused".into()));
        }
        Err(SessionError::StateKeyNotExist)
    }
    fn set(&self, _: &str, _: Value) -> Result<(), SessionError> {
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
}

/// Go errorMockSession.
struct ErrorMockSession;

impl Session for ErrorMockSession {
    fn id(&self) -> &str {
        "testSession"
    }
    fn app_name(&self) -> &str {
        "testApp"
    }
    fn user_id(&self) -> &str {
        "testUser"
    }
    fn state(&self) -> &dyn State {
        &ErrorMockState
    }
    fn events(&self) -> Vec<Event> {
        Vec::new()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn test_process_state_variable_systemic_error_propagates() {
    let ctx = InvocationContext::builder()
        .session(Arc::new(ErrorMockSession))
        .build_invocation();

    // missing optional key
    assert_eq!(replace_match(&ctx, "{missing_key?}").await.unwrap(), "");

    // missing required key
    let err = replace_match(&ctx, "{missing_key}").await.unwrap_err();
    assert!(
        matches!(err, AgentError::Session(SessionError::StateKeyNotExist)),
        "expected StateKeyNotExist, got: {err:?}"
    );

    // systemic error on optional key
    let err = replace_match(&ctx, "{systemic_error?}").await.unwrap_err();
    assert!(
        err.to_string().contains("database connection refused"),
        "expected systemic error to propagate even for optional key, got: {err}"
    );
}
