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

//! Port of ADK-Go `internal/configurable/conformance/replayplugin/replay_plugin_test.go`.
//!
//! Go's `MockState` / `MockSession` are ported as test types; Go's `MockInvocationContext`,
//! `MockCallbackContext` and `MockToolContext` are gadk's concrete contexts built with
//! `InvocationContext::builder()` / `Context::builder()` over the mock session (invocation id
//! `test-invocation`, agent `test_agent`). The plugin is driven through its public callbacks,
//! as Go's test does.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use gadk_agent::{AgentConfig, Context, InvocationContext, Plugin, Tool, new_agent};
use gadk_genai::{Content, Part};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::{Event, Session, SessionError, State};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::conformance::replay;

fn create_recordings_file(dir: &Path, content: &str) {
    let path = dir.join("generated-recordings.yaml");
    std::fs::write(path, content).unwrap();
}

/// Go `MockState`: `Get` of a missing key is `(nil, nil)`.
#[derive(Default)]
struct MockState {
    data: Mutex<Map<String, Value>>,
}

impl State for MockState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        Ok(self.data.lock().get(key).cloned().unwrap_or(Value::Null))
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.data.lock().insert(key.to_owned(), value);
        Ok(())
    }

    fn all(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
}

/// Go `MockSession`.
#[derive(Default)]
struct MockSession {
    state: MockState,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "mock-session-id"
    }

    fn app_name(&self) -> &str {
        "mock-app"
    }

    fn user_id(&self) -> &str {
        "mock-user"
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn events(&self) -> Vec<Event> {
        Vec::new()
    }

    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `MockTool` (no `Run` matching Go's `toolinternal.FunctionTool`: never run by replay).
struct MockTool {
    name_val: String,
}

impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name_val
    }

    fn description(&self) -> &str {
        "mock tool"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go's per-subtest `setup`: `MustNew("/")` and an empty mock session.
fn setup() -> (Plugin, Arc<MockSession>) {
    (replay::new("/"), Arc::new(MockSession::default()))
}

/// Go `MockInvocationContext{session, invocationID: "test-invocation"}`.
fn inv_context(session: &Arc<MockSession>) -> InvocationContext {
    InvocationContext::builder()
        .session(session.clone())
        .invocation_id("test-invocation")
        .build_invocation()
}

/// Go `MockCallbackContext` / `MockToolContext{state, invocationID: "test-invocation",
/// agentName: "test_agent"}`.
fn cb_context(session: &Arc<MockSession>) -> Context {
    Context::builder()
        .session(session.clone())
        .invocation_id("test-invocation")
        .agent(
            new_agent(AgentConfig {
                name: "test_agent".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .build()
}

fn set_replay_config(session: &MockSession, dir: &str) {
    session
        .state()
        .set(
            "_adk_replay_config",
            json!({"dir": dir, "user_message_index": 0}),
        )
        .unwrap();
}

fn hello_request() -> LlmRequest {
    LlmRequest {
        model: "gemini-2.0-flash".into(),
        contents: vec![Content {
            role: "user".into(),
            parts: vec![Part::from_text("Hello")],
        }],
        ..Default::default()
    }
}

async fn before_run(plugin: &Plugin, session: &Arc<MockSession>) {
    (plugin.before_run_callback().unwrap())(inv_context(session))
        .await
        .unwrap();
}

async fn before_model(
    plugin: &Plugin,
    session: &Arc<MockSession>,
    request: LlmRequest,
) -> Result<Option<LlmResponse>, gadk_agent::AgentError> {
    (plugin.before_model_callback().unwrap())(cb_context(session), request)
        .await
        .1
}

async fn before_tool(
    plugin: &Plugin,
    session: &Arc<MockSession>,
    name: &str,
    args: Value,
) -> Result<Option<Map<String, Value>>, gadk_agent::AgentError> {
    let tool: Arc<dyn Tool> = Arc::new(MockTool {
        name_val: name.into(),
    });
    let Value::Object(args) = args else {
        panic!("args must be an object")
    };
    (plugin.before_tool_callback().unwrap())(cb_context(session), tool, args)
        .await
        .1
}

async fn before_model_callback_with_matching_recording_returns_recorded_response() {
    let (plugin, mock_session) = setup();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. Create recording file
    let recordings_yaml = r#"
recordings:
  - user_message_index: 0
    agent_name: "test_agent"
    llm_recording:
      llm_request:
        model: "gemini-2.0-flash"
        contents:
          - role: "user"
            parts:
              - text: "Hello"
      llm_responses:
        - content:
            role: "model"
            parts:
              - text: "Recorded response"
"#;
    create_recordings_file(temp_dir.path(), recordings_yaml);

    // 2. Setup replay config
    set_replay_config(&mock_session, &temp_dir.path().to_string_lossy());

    // 3. Load recordings (BeforeRunCallback)
    before_run(&plugin, &mock_session).await;

    // 4. Call BeforeModelCallback with matching request
    let result = before_model(&plugin, &mock_session, hello_request()).await;

    // 5. Verify
    let result = result
        .expect("unexpected error")
        .expect("expected non-nil result");
    let content = result.content.expect("expected non-nil result.Content");
    assert_eq!(content.parts[0].text, "Recorded response");
}

async fn before_model_callback_with_single_llm_response_returns_recorded_response() {
    let (plugin, mock_session) = setup();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. Create recording file with singular llm_response instead of plural llm_responses
    let recordings_yaml = r#"
recordings:
  - user_message_index: 0
    agent_name: "test_agent"
    llm_recording:
      llm_request:
        model: "gemini-2.0-flash"
        contents:
          - role: "user"
            parts:
              - text: "Hello"
      llm_response:
        content:
          role: "model"
          parts:
            - text: "Recorded response"
"#;
    create_recordings_file(temp_dir.path(), recordings_yaml);

    // 2. Setup replay config
    set_replay_config(&mock_session, &temp_dir.path().to_string_lossy());

    // 3. Load recordings (BeforeRunCallback)
    before_run(&plugin, &mock_session).await;

    // 4. Call BeforeModelCallback with matching request
    let result = before_model(&plugin, &mock_session, hello_request()).await;

    // 5. Verify
    let result = result
        .expect("unexpected error")
        .expect("expected non-nil result");
    let content = result.content.expect("expected non-nil result.Content");
    assert_eq!(content.parts[0].text, "Recorded response");
}

async fn before_model_callback_request_mismatch_returns_empty() {
    let (plugin, mock_session) = setup();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. Create recording with DIFFERENT model
    let recordings_yaml = "
recordings:
  - user_message_index: 0
    agent_name: \"test_agent\"
    llm_recording:
      llm_request:
        model: \"gemini-1.5-pro\" 
        contents:
          - role: \"user\"
            parts:
              - text: \"Hello\"
";
    create_recordings_file(temp_dir.path(), recordings_yaml);

    // 2. Setup config
    set_replay_config(&mock_session, &temp_dir.path().to_string_lossy());

    // 3. Load recordings
    before_run(&plugin, &mock_session).await;

    // 4. Call with mismatched request (gemini-2.0-flash vs 1.5-pro)
    let result = before_model(&plugin, &mock_session, hello_request()).await;

    // 5. Verify result is nil (empty) and error is returned
    assert!(result.is_err(), "expected error due to mismatch");
}

async fn before_tool_callback_with_matching_recording_returns_recorded_response() {
    let (plugin, mock_session) = setup();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. Create recording with tool call
    let recordings_yaml = r#"
recordings:
  - user_message_index: 0
    agent_name: "test_agent"
    tool_recording:
      tool_call:
        name: "test_tool"
        args:
          param1: "value1"
          param2: 42.0
      tool_response:
        name: "test_tool"
        response:
          result: "success"
          data: "recorded data"
"#;
    create_recordings_file(temp_dir.path(), recordings_yaml);

    // 2. Setup config
    set_replay_config(&mock_session, &temp_dir.path().to_string_lossy());

    // 3. Load recordings
    before_run(&plugin, &mock_session).await;

    // 4. Call BeforeToolCallback
    let tool_args = json!({"param1": "value1", "param2": 42});
    let result = before_tool(&plugin, &mock_session, "test_tool", tool_args).await;

    // 5. Verify
    let result = result
        .expect("unexpected error")
        .expect("expected non-nil result");
    assert_eq!(result["result"], "success");
    assert_eq!(result["data"], "recorded data");
}

async fn before_tool_callback_tool_name_mismatch_returns_empty() {
    let (plugin, mock_session) = setup();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. Recording expects "expected_tool"
    let recordings_yaml = r#"
recordings:
  - user_message_index: 0
    agent_name: "test_agent"
    tool_recording:
      tool_call:
        name: "expected_tool"
        args:
          param: "value"
"#;
    create_recordings_file(temp_dir.path(), recordings_yaml);

    set_replay_config(&mock_session, &temp_dir.path().to_string_lossy());
    before_run(&plugin, &mock_session).await;

    // 2. Call with "actual_tool"
    let result = before_tool(
        &plugin,
        &mock_session,
        "actual_tool", // Wrong name
        json!({"param": "value"}),
    )
    .await;

    // 3. Verify nil result and error
    assert!(result.is_err(), "expected error due to tool name mismatch");
}

async fn before_tool_callback_tool_args_mismatch_returns_empty() {
    let (plugin, mock_session) = setup();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. Recording expects "expected_value"
    let recordings_yaml = r#"
recordings:
  - user_message_index: 0
    agent_name: "test_agent"
    tool_recording:
      tool_call:
        name: "test_tool"
        args:
          param: "expected_value"
"#;
    create_recordings_file(temp_dir.path(), recordings_yaml);

    set_replay_config(&mock_session, &temp_dir.path().to_string_lossy());
    before_run(&plugin, &mock_session).await;

    // 2. Call with "actual_value"
    let result = before_tool(
        &plugin,
        &mock_session,
        "test_tool",
        json!({"param": "actual_value"}),
    )
    .await;

    // 3. Verify nil result and error
    assert!(result.is_err(), "expected error due to tool args mismatch");
}

// parity: adapted: Go's mock contexts → gadk builder contexts over a ported MockSession; the tool-args subtest records `param2: 42.0` (Go passes a Go `int` 42 live, gadk's live args are JSON numbers, Go's float64)
#[tokio::test]
async fn test_replay_plugin() {
    before_model_callback_with_matching_recording_returns_recorded_response().await;
    before_model_callback_with_single_llm_response_returns_recorded_response().await;
    before_model_callback_request_mismatch_returns_empty().await;
    before_tool_callback_with_matching_recording_returns_recorded_response().await;
    before_tool_callback_tool_name_mismatch_returns_empty().await;
    before_tool_callback_tool_args_mismatch_returns_empty().await;
}

// parity: adapted: Go's mock contexts → gadk builder contexts over a ported MockSession
#[tokio::test]
async fn test_replay_plugin_path_validation() {
    // Create a temporary directory structure for testing path validation
    let temp = tempfile::tempdir().unwrap();
    let temp_dir = temp.path().to_string_lossy().into_owned();
    let safe_dir = format!("{temp_dir}/safe");
    std::fs::create_dir(&safe_dir).expect("failed to create safe dir");

    // Create a safe recordings file
    create_recordings_file(Path::new(&safe_dir), "recordings: []");

    // Initialize plugin with restricted base directory
    let plugin = replay::new(&safe_dir);
    let mock_session = Arc::new(MockSession::default());

    let tests: [(&str, String, bool); 5] = [
        ("ValidPath_InsideBaseDir", safe_dir.clone(), false),
        (
            "InvalidPath_ParentTraversal",
            format!("{safe_dir}/.."),
            true,
        ),
        // tempDir is parent of safeDir, so it's outside
        ("InvalidPath_OutsideBaseDir", temp_dir.clone(), true),
        ("InvalidPath_AbsoluteOutside", "/etc".into(), true), // outside
        ("InvalidPath_RelativeTraversal", "../".into(), true), // Relative path traversing up
    ];

    for (name, dir, expect_error) in tests {
        // Set the config
        set_replay_config(&mock_session, &dir);

        // Run BeforeRunCallback
        let result = (plugin.before_run_callback().unwrap())(inv_context(&mock_session)).await;

        if expect_error {
            assert!(
                result.is_err(),
                "{name}: expected error for dir {dir:?}, got nil"
            );
        } else if let Err(err) = result {
            panic!("{name}: unexpected error for dir {dir:?}: {err}");
        }
    }
}
