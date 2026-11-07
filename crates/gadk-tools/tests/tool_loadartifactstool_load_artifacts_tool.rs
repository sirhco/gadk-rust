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

//! Port of Go tool/loadartifactstool/load_artifacts_tool_test.go.

use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{AgentError, Artifacts, Context, ToolError};
use gadk_artifact::{
    ArtifactError, ArtifactService, InMemoryArtifactService, ListRequest, ListResponse,
    LoadRequest, LoadResponse, SaveRequest, SaveResponse,
};
use gadk_genai::{Content, Part, ROLE_USER};
use gadk_model::LlmRequest;
use gadk_tools::load_artifacts;
use serde_json::{Map, Value, json};

/// Port of Go `internal/artifact.Artifacts{Service: artifact.InMemoryService(), AppName: "app",
/// UserID: "user", SessionID: "session"}`.
struct TestArtifacts {
    service: InMemoryArtifactService,
}

#[async_trait]
impl Artifacts for TestArtifacts {
    async fn save(&self, name: &str, data: Part) -> Result<SaveResponse, AgentError> {
        Ok(self
            .service
            .save(SaveRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "session".into(),
                file_name: name.into(),
                part: Some(data),
                ..Default::default()
            })
            .await?)
    }

    async fn list(&self) -> Result<ListResponse, AgentError> {
        Ok(self
            .service
            .list(ListRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "session".into(),
            })
            .await?)
    }

    async fn load(&self, name: &str) -> Result<LoadResponse, AgentError> {
        self.load_version(name, 0).await
    }

    async fn load_version(&self, name: &str, version: i64) -> Result<LoadResponse, AgentError> {
        Ok(self
            .service
            .load(LoadRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "session".into(),
                file_name: name.into(),
                version,
            })
            .await?)
    }
}

/// Port of Go `createToolContext`.
fn create_tool_context() -> Context {
    Context::builder()
        .artifacts(Arc::new(TestArtifacts {
            service: InMemoryArtifactService::new(),
        }))
        .build()
}

async fn save_artifact(ctx: &Context, name: &str, text: &str) {
    ctx.artifacts()
        .expect("artifact service")
        .save(name, Part::from_text(text))
        .await
        .unwrap_or_else(|e| panic!("Failed to save artifact {name}: {e}"));
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not a JSON object: {other}"),
    }
}

fn fr_part(name: &str, response: Value) -> Part {
    Part::from_function_response(name, obj(response))
}

async fn process(ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
    let tool = load_artifacts::new();
    tool.process_request(ctx, req).await
}

fn system_text(req: &LlmRequest) -> String {
    req.config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
        .map(|si| si.parts[0].text.clone())
        .expect("system instruction")
}

fn assert_doc1_appended(req: &LlmRequest, case: &str) {
    assert_eq!(
        req.contents.len(),
        2,
        "{case}: Expected 2 contents, but got: {:?}",
        req.contents
    );
    let appended = &req.contents[1];
    assert_eq!(appended.role, ROLE_USER, "{case}: Appended Content Role");
    assert_eq!(
        appended.parts.len(),
        2,
        "{case}: Expected 2 parts in appended content, but got: {:?}",
        appended.parts
    );
    assert_eq!(
        appended.parts[0].text, "Artifact doc1.txt is:",
        "{case}: First part of appended content"
    );
    assert_eq!(
        appended.parts[1].text, "This is the content of doc1.txt",
        "{case}: Second part of appended content"
    );
}

/// Rust form of Go `errors.Is(err, fs.ErrNotExist)`: some error in the source chain is the
/// artifact service's not-found error.
fn is_not_found(err: &ToolError) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if matches!(
            e.downcast_ref::<ArtifactError>(),
            Some(ArtifactError::NotFound)
        ) || matches!(
            e.downcast_ref::<AgentError>(),
            Some(AgentError::Artifact(ArtifactError::NotFound))
        ) {
            return true;
        }
        cur = e.source();
    }
    false
}

// parity: adapted: Go's []string and []any argument values are both JSON arrays in Rust; the two error cases also pin Go's encoding/json error text
#[tokio::test]
async fn test_load_artifacts_tool_run() {
    let tool = load_artifacts::new();
    let tc = create_tool_context();
    let tool_impl = tool
        .as_runnable()
        .expect("loadArtifactsTool does not implement FunctionTool");
    let cases: Vec<(&str, Value, Result<Value, &str>)> = vec![
        (
            "basic string slice",
            json!({"artifact_names": ["file1", "file2"]}),
            Ok(json!({"artifact_names": ["file1", "file2"]})),
        ),
        ("empty args", json!({}), Ok(json!({"artifact_names": []}))),
        (
            "any slice with strings",
            json!({"artifact_names": ["fileA", "fileB"]}),
            Ok(json!({"artifact_names": ["fileA", "fileB"]})),
        ),
        (
            "empty string slice",
            json!({"artifact_names": []}),
            Ok(json!({"artifact_names": []})),
        ),
        (
            "empty any slice",
            json!({"artifact_names": []}),
            Ok(json!({"artifact_names": []})),
        ),
        (
            "nil value",
            json!({"artifact_names": null}),
            Ok(json!({"artifact_names": []})),
        ),
        (
            "incorrect type (not a slice)",
            json!({"artifact_names": "not a slice"}),
            Err(
                "failed to unmarshal artifact_names from JSON to []string: json: cannot unmarshal string into Go value of type []string",
            ),
        ),
        (
            "any slice with non-string",
            json!({"artifact_names": ["fileA", 123]}),
            Err(
                "failed to unmarshal artifact_names from JSON to []string: json: cannot unmarshal number into Go value of type string",
            ),
        ),
    ];
    for (name, args, want) in cases {
        let got = tool_impl.run(tc.clone(), args).await;
        match want {
            Ok(want) => {
                let result = got.unwrap_or_else(|e| panic!("{name}: Run() error = {e}"));
                assert_eq!(Value::Object(result), want, "{name}: Run() result diff");
            }
            Err(want_err) => {
                let err = got.expect_err(&format!("{name}: Run() error = nil, wantErr true"));
                assert_eq!(err.to_string(), want_err, "{name}");
            }
        }
    }
}

#[tokio::test]
async fn test_load_artifacts_tool_process_request() {
    let tc = create_tool_context();
    save_artifact(&tc, "file1.txt", "content1").await;
    save_artifact(&tc, "file2.pdf", "content2").await;
    let mut llm_request = LlmRequest::default();
    process(&tc, &mut llm_request)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest failed: {e}"));
    let instruction = system_text(&llm_request);
    assert!(
        instruction.contains("You have a list of artifacts"),
        "Instruction should contain 'You have a list of artifacts', but got: {instruction}"
    );
    assert!(
        instruction.contains("\"file1.txt\"") && instruction.contains("\"file2.pdf\""),
        "Instruction should contain artifact names, but got: {instruction}"
    );
    assert!(
        llm_request.contents.is_empty(),
        "Expected no contents, but got: {:?}",
        llm_request.contents
    );
}

#[tokio::test]
async fn test_load_artifacts_tool_process_request_artifacts_load_artifacts_function_call() {
    let tc = create_tool_context();
    save_artifact(&tc, "doc1.txt", "This is the content of doc1.txt").await;
    let mut llm_request = LlmRequest {
        contents: vec![Content::from_parts(
            vec![fr_part(
                "load_artifacts",
                json!({"artifact_names": ["doc1.txt"]}),
            )],
            "model",
        )],
        ..Default::default()
    };
    process(&tc, &mut llm_request)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest failed: {e}"));
    assert_doc1_appended(&llm_request, "LoadArtifactsFunctionCall");
}

// parity: adapted: Go's structpb-style []any response value is the same JSON array as []string in Rust, so this case exercises the same Value::Array path
#[tokio::test]
async fn test_load_artifacts_tool_process_request_artifacts_load_artifacts_function_call_any_slice()
{
    let tc = create_tool_context();
    save_artifact(&tc, "doc1.txt", "This is the content of doc1.txt").await;
    let mut llm_request = LlmRequest {
        contents: vec![Content::from_parts(
            vec![fr_part(
                "load_artifacts",
                json!({"artifact_names": ["doc1.txt"]}),
            )],
            "model",
        )],
        ..Default::default()
    };
    process(&tc, &mut llm_request)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest failed: {e}"));
    assert_doc1_appended(&llm_request, "AnySlice");
}

#[tokio::test]
async fn test_load_artifacts_tool_process_request_artifacts_other_function_call() {
    let tc = create_tool_context();
    save_artifact(&tc, "doc1.txt", "content1").await;
    let mut llm_request = LlmRequest {
        contents: vec![Content::from_parts(
            vec![fr_part("other_function", json!({"some_key": "some_value"}))],
            "model",
        )],
        ..Default::default()
    };
    process(&tc, &mut llm_request)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest failed: {e}"));
    assert_eq!(
        llm_request.contents.len(),
        1,
        "Expected 1 content, but got: {:?}",
        llm_request.contents
    );
    assert_eq!(llm_request.contents[0].role, "model", "Content Role");
}

#[tokio::test]
async fn test_load_artifacts_tool_process_request_nil_artifacts() {
    // A context with no artifact service configured.
    let tc = Context::builder().build();
    let mut llm_request = LlmRequest::default();
    let err = process(&tc, &mut llm_request).await.expect_err(
        "ProcessRequest should return an error when no artifact service is configured, but got nil",
    );
    assert!(
        err.to_string().contains("artifact service"),
        "error should mention the missing artifact service, got: {err}"
    );
}

#[tokio::test]
async fn test_load_artifacts_tool_process_request_nil_artifacts_with_function_call() {
    // A context with no artifact service configured.
    let tc = Context::builder().build();
    let mut llm_request = LlmRequest {
        contents: vec![Content::from_parts(
            vec![fr_part(
                "load_artifacts",
                json!({"artifact_names": ["doc1.txt"]}),
            )],
            "model",
        )],
        ..Default::default()
    };
    let err = process(&tc, &mut llm_request).await.expect_err(
        "ProcessRequest should return an error when no artifact service is configured, but got nil",
    );
    assert!(
        err.to_string().contains("artifact service"),
        "error should mention the missing artifact service, got: {err}"
    );
}

// parity: adapted: Go's nil *genai.Part becomes an empty Part because a Rust part list cannot hold nil
#[tokio::test]
async fn test_load_artifacts_tool_process_request_artifacts_multi_part_function_response() {
    let other = || fr_part("other_function", json!({"status": "ok"}));
    let load = || fr_part("load_artifacts", json!({"artifact_names": ["doc1.txt"]}));
    let cases: Vec<(&str, Vec<Part>)> = vec![
        ("load_artifacts after other function", vec![other(), load()]),
        (
            "load_artifacts before other function",
            vec![load(), other()],
        ),
        (
            "nil part alongside load_artifacts",
            vec![Part::default(), load()],
        ),
    ];
    for (name, parts) in cases {
        let tc = create_tool_context();
        save_artifact(&tc, "doc1.txt", "This is the content of doc1.txt").await;
        let mut llm_request = LlmRequest {
            contents: vec![Content::from_parts(parts, ROLE_USER)],
            ..Default::default()
        };
        process(&tc, &mut llm_request)
            .await
            .unwrap_or_else(|e| panic!("{name}: ProcessRequest failed: {e}"));
        assert_doc1_appended(&llm_request, name);
    }
}

// parity: adapted: Go's errors.Is(err, fs.ErrNotExist) becomes a walk of the source chain for AgentError::Artifact(ArtifactError::NotFound)
#[tokio::test]
async fn test_load_artifacts_tool_process_request_load_error() {
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("missing artifact", vec!["missing.txt"]),
        (
            "missing artifact alongside a saved one",
            vec!["doc1.txt", "missing.txt"],
        ),
    ];
    for (name, artifact_names) in cases {
        let tc = create_tool_context();
        save_artifact(&tc, "doc1.txt", "This is the content of doc1.txt").await;
        let mut llm_request = LlmRequest {
            contents: vec![Content::from_parts(
                vec![fr_part(
                    "load_artifacts",
                    json!({"artifact_names": artifact_names}),
                )],
                ROLE_USER,
            )],
            ..Default::default()
        };
        let err = process(&tc, &mut llm_request).await.expect_err(&format!(
            "{name}: ProcessRequest should return an error when an artifact cannot be loaded, but got nil"
        ));
        let text = err.to_string();
        assert_eq!(
            text.matches("failed to load artifact").count(),
            1,
            "{name}: error names the failure the wrong number of times, want once: {text}"
        );
        assert!(
            is_not_found(&err),
            "{name}: errors.Is(err, fs.ErrNotExist) = false, want true, got: {text}"
        );
        assert_eq!(
            text, "failed to load artifact missing.txt: artifact not found: file does not exist",
            "{name}"
        );
    }
}
