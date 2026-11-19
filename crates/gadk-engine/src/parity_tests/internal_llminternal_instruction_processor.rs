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
    artifacts: Vec<(&'static str, Part)>,
    svc: Svc,
    want: &'static str,
    want_err: Option<&'static str>,
}

fn my_file() -> (&'static str, Part) {
    ("my_file", Part::from_text("This is my artifact content."))
}

/// Go `genai.NewPartFromBytes(data, mimeType)`.
fn bytes_part(data: &'static [u8], mime_type: &str) -> Part {
    Part::from_bytes(bytes::Bytes::from_static(data), mime_type)
}

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
            artifacts: vec![my_file()],
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
        // A text artifact can come back as inline data rather than Text: gcsartifact stores
        // a text Part as a text/plain object and loads it back as bytes. adk-python injects
        // the decoded text (_instructions_utils.py, _artifact_to_text).
        Case {
            name: "text artifact loaded as inline data",
            template: "The artifact content is: {artifact.notes.txt}",
            state: json!({}),
            artifacts: vec![("notes.txt", bytes_part(b"hello from GCS", "text/plain"))],
            svc: Svc::Default,
            want: "The artifact content is: hello from GCS",
            want_err: None,
        },
        Case {
            name: "text artifact with a charset parameter",
            template: "The artifact content is: {artifact.notes}",
            state: json!({}),
            artifacts: vec![(
                "notes",
                bytes_part("café".as_bytes(), "Text/Markdown; charset=utf-8"),
            )],
            svc: Svc::Default,
            want: "The artifact content is: café",
            want_err: None,
        },
        Case {
            name: "json artifact with a charset parameter",
            template: "Config: {artifact.config}",
            state: json!({}),
            artifacts: vec![(
                "config",
                bytes_part(br#"{"k":"v"}"#, "application/JSON; charset=utf-8"),
            )],
            svc: Svc::Default,
            want: r#"Config: {"k":"v"}"#,
            want_err: None,
        },
        Case {
            name: "text file uploaded as octet-stream",
            template: "Rows: {artifact.rows.csv}",
            state: json!({}),
            artifacts: vec![(
                "rows.csv",
                bytes_part(b"a,b\n1,2", "application/octet-stream"),
            )],
            svc: Svc::Default,
            want: "Rows: a,b\n1,2",
            want_err: None,
        },
        Case {
            name: "binary artifact still renders empty",
            template: "Image: {artifact.photo.png}",
            state: json!({}),
            artifacts: vec![("photo.png", bytes_part(b"\x89PNG", "image/png"))],
            svc: Svc::Default,
            want: "Image: ",
            want_err: None,
        },
        // A binary artifact with a text file name is not decoded: Gemini accepts its MIME
        // type inline. TestArtifactText covers each rule.
        Case {
            name: "pdf named like a text file is not decoded",
            template: "Doc: {artifact.notes.txt}",
            state: json!({}),
            artifacts: vec![(
                "notes.txt",
                bytes_part(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3", "application/pdf"),
            )],
            svc: Svc::Default,
            want: "Doc: ",
            want_err: None,
        },
        Case {
            name: "missing required artifact",
            template: "The artifact content is: {artifact.missing_file}",
            state: json!({}),
            artifacts: vec![my_file()],
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
            artifacts: vec![my_file()],
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
                "failed to load artifact : request validation failed: invalid load request: missing required fields: file_name",
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
            artifacts: vec![my_file()],
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
            for (n, part) in &tc.artifacts {
                a.save(n, part.clone()).await.unwrap();
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

/// Each rule Go's artifactText applies to inline data, in adk-python's order: MIME types
/// Gemini accepts inline are never decoded, then text MIME types and text file extensions are.
// parity: adapted: Go calls the unexported artifactText(fileName, part); gadk has no such
// helper, so each row renders `{artifact.<fileName>}` through inject_session_state.
#[tokio::test]
async fn test_artifact_text() {
    // (name, file name, MIME type, data, want)
    let cases: Vec<(&str, &str, &str, &'static [u8], &str)> = vec![
        // Gemini accepts these inline, so they are not decoded, whatever the name.
        (
            "pdf",
            "notes.txt",
            "application/pdf",
            b"%PDF-1.7\n%\xe2\xe3\xcf\xd3",
            "",
        ),
        ("image", "photo.xml", "image/png", b"\x89PNG", ""),
        (
            "audio",
            "memo.csv",
            "audio/wav",
            b"RIFF\x24\x00\x00\x00WAVE",
            "",
        ),
        (
            "video",
            "clip.json",
            "video/mp4",
            b"\x00\x00\x00\x18ftyp",
            "",
        ),
        // Gemini rejects SVG and XML images inline, so adk-python decodes them.
        ("svg image", "logo", "image/svg", b"<svg/>", "<svg/>"),
        (
            "svg+xml image",
            "logo",
            "image/svg+xml",
            b"<svg/>",
            "<svg/>",
        ),
        ("xml image", "logo", "image/xml", b"<svg/>", "<svg/>"),
        ("csv", "rows", "application/csv", b"a,b", "a,b"),
        (
            "svg+xml application",
            "logo",
            "application/svg+xml",
            b"<svg/>",
            "<svg/>",
        ),
        (
            "xml application",
            "feed",
            "application/xml",
            b"<feed/>",
            "<feed/>",
        ),
        (
            "space before the parameters",
            "config",
            "application/json ; charset=utf-8",
            br#"{"k":"v"}"#,
            r#"{"k":"v"}"#,
        ),
        (
            "txt extension",
            "readme.txt",
            "application/octet-stream",
            b"read me",
            "read me",
        ),
        (
            "upper-case json extension",
            "config.JSON",
            "application/octet-stream",
            br#"{"k":"v"}"#,
            r#"{"k":"v"}"#,
        ),
        (
            "xml extension",
            "layout.xml",
            "application/octet-stream",
            b"<a/>",
            "<a/>",
        ),
        (
            "other binary",
            "blob.bin",
            "application/octet-stream",
            b"\x00\x01",
            "",
        ),
        // One U+FFFD per run of invalid bytes. Python's errors='replace' gives
        // "caf\u{FFFD} \u{FFFD}\u{FFFD}" here.
        (
            "invalid UTF-8",
            "notes",
            "text/plain",
            b"caf\xe9 \xe2\xe3",
            "caf\u{FFFD} \u{FFFD}",
        ),
    ];
    for (name, file_name, mime_type, data, want) in cases {
        let svc = InMemorySessionService::new();
        let sess = svc
            .create(CreateRequest {
                app_name: "testApp".into(),
                user_id: "testUser".into(),
                session_id: Some("testSession".into()),
                state: None,
            })
            .await
            .unwrap();
        let artifacts: Arc<dyn Artifacts> = Arc::new(ScopedArtifacts::new(
            Arc::new(InMemoryArtifactService::new()),
            "testApp",
            "testUser",
            "testSession",
        ));
        artifacts
            .save(file_name, bytes_part(data, mime_type))
            .await
            .unwrap();
        let ctx = InvocationContext::builder()
            .session(sess)
            .artifacts(artifacts)
            .build_invocation();
        let got = inject_session_state(&ctx, &format!("{{artifact.{file_name}}}"))
            .await
            .unwrap_or_else(|e| panic!("{name}: inject_session_state error: {e}"));
        assert_eq!(
            got, want,
            "{name}: artifactText({file_name:?}, {mime_type:?})"
        );
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
