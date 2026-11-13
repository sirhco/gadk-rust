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

//! Port of ADK-Go `internal/configurable/conformance/recordplugin/record_plugin_test.go`.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_agent::{Context, ContextDelta, InvocationContext, Plugin, Tool};
use gadk_genai::{Content, GenerateContentConfig, Part};
use gadk_model::{HttpOptions, LlmRequest, LlmResponse};
use serde_json::{Map, Value, json};

use crate::GoValue;
use crate::conformance::gonode::{from_go_node, from_go_node_partial};
use crate::conformance::record;
use crate::conformance::recording::Recordings;
use crate::tests::temp_dir;

/// Go `MockTool`.
struct MockTool {
    name_val: &'static str,
}

impl Tool for MockTool {
    fn name(&self) -> &str {
        self.name_val
    }

    fn description(&self) -> &str {
        "mock tool"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `MockInvocationContext` on a `MockSession` with a `MockState`: the builder's in-memory
/// session (absent keys read as an error, which Go's `parseRecordConfig` treats as Go's nil).
fn invocation(id: &str) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id(id)
        .build_invocation()
}

/// The same session under another invocation id (Go builds each `MockInvocationContext` on
/// the one `mockSession`).
fn invocation_on(session_of: &InvocationContext, id: &str) -> InvocationContext {
    InvocationContext::builder()
        .session(session_of.session())
        .invocation_id(id)
        .build_invocation()
}

/// Go `MockCallbackContext` (agent name `test_agent`, the builder's default agent).
fn callback_ctx(inv: &InvocationContext) -> Context {
    inv.clone().into_context()
}

/// Go `MockToolContext`.
fn tool_ctx(inv: &InvocationContext, function_call_id: &str) -> Context {
    inv.clone().into_context().with_delta(ContextDelta {
        function_call_id: Some(function_call_id.to_owned()),
        ..Default::default()
    })
}

fn set_config(inv: &InvocationContext, config: Value) {
    inv.session()
        .state()
        .set("_adk_recordings_config", config)
        .unwrap();
}

async fn before_run(p: &Plugin, inv: &InvocationContext) -> Result<(), String> {
    (p.before_run_callback().unwrap())(inv.clone())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn after_run(p: &Plugin, inv: &InvocationContext) {
    (p.after_run_callback().unwrap())(inv.clone()).await;
}

async fn before_model(p: &Plugin, ctx: &Context, req: LlmRequest) -> Result<(), String> {
    let (_, r) = (p.before_model_callback().unwrap())(ctx.clone(), req).await;
    r.map(|_| ()).map_err(|e| e.to_string())
}

async fn after_model(p: &Plugin, ctx: &Context, resp: LlmResponse) -> Result<(), String> {
    let (_, r) = (p.after_model_callback().unwrap())(ctx.clone(), resp, None).await;
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn text_response(text: &str, partial: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            parts: vec![Part::from_text(text)],
            ..Default::default()
        }),
        partial,
        ..Default::default()
    }
}

/// Go `yaml.Unmarshal(data, &recs)` with the error ignored (`_ = yaml.Unmarshal`).
fn unmarshal_ignoring_errors(data: &[u8]) -> Recordings {
    let Ok(doc) = gadk_goyaml::unmarshal_node(data) else {
        return Recordings::default();
    };
    from_go_node_partial(&doc, doc.root())
        .map(|(recs, _)| recs)
        .unwrap_or_default()
}

fn response_text(r: &LlmResponse) -> &str {
    &r.content.as_ref().unwrap().parts[0].text
}

fn generated(dir: &Path) -> PathBuf {
    dir.join("generated-recordings.yaml")
}

// parity: adapted: Go's hand-written Mock{State,Session,InvocationContext,CallbackContext,
// ToolContext} are gadk-agent's builder contexts on one in-memory session; the plugin's
// callbacks are awaited (gadk callbacks return futures).
#[tokio::test]
async fn test_record_plugin() {
    // Go: record_plugin_test.go TestRecordPlugin
    let setup = |base_dir: &Path| (record::new(base_dir), invocation("setup"));

    // t.Run("RecordLlmAndToolCallsAndSaveYaml")
    {
        let (_guard, temp_dir) = temp_dir();
        let (p, session) = setup(&temp_dir);

        set_config(
            &session,
            json!({"dir": temp_dir.to_str().unwrap(), "user_message_index": 0}),
        );

        let inv_context = invocation_on(&session, "test-invocation");

        // 1. beforeRun
        before_run(&p, &inv_context)
            .await
            .unwrap_or_else(|e| panic!("beforeRun failed: {e}"));

        // 2. beforeModel (LLM request)
        let cb_context = callback_ctx(&inv_context);
        let mut tools: HashMap<String, Arc<dyn Any + Send + Sync>> = HashMap::new();
        tools.insert("test_tool".to_owned(), Arc::new("dummy_val"));
        let req = LlmRequest {
            model: "gemini-2.0-flash".to_owned(),
            contents: vec![Content {
                role: "user".to_owned(),
                parts: vec![Part::from_text("Hello agent")],
            }],
            config: Some(GenerateContentConfig::default()),
            // Go's Config.HTTPOptions (with an ExtrasRequestProvider func) is gadk's
            // LlmRequest.http_options.
            http_options: Some(HttpOptions {
                extras_request_provider: Some(Arc::new(|body| body)),
                ..Default::default()
            }),
            tools,
        };
        before_model(&p, &cb_context, req)
            .await
            .unwrap_or_else(|e| panic!("beforeModel failed: {e}"));

        // 3. afterModel (LLM response)
        let resp = LlmResponse {
            content: Some(Content {
                role: "model".to_owned(),
                parts: vec![Part::from_text("Hello user, calling tool now")],
            }),
            model_version: "gemini-2.0-flash".to_owned(),
            partial: false,
            ..Default::default()
        };
        after_model(&p, &cb_context, resp)
            .await
            .unwrap_or_else(|e| panic!("afterModel failed: {e}"));

        // 4. beforeTool
        let tool_context = tool_ctx(&inv_context, "call-123");
        let mock_tool: Arc<dyn Tool> = Arc::new(MockTool {
            name_val: "test_tool",
        });
        let mut tool_args = Map::new();
        tool_args.insert("param".to_owned(), json!("test"));
        (p.before_tool_callback().unwrap())(
            tool_context.clone(),
            mock_tool.clone(),
            tool_args.clone(),
        )
        .await
        .unwrap_or_else(|e| panic!("beforeTool failed: {e}"));

        // 5. afterTool
        let mut tool_result = Map::new();
        tool_result.insert("result".to_owned(), json!("test_success"));
        (p.after_tool_callback().unwrap())(
            tool_context.clone(),
            mock_tool.clone(),
            tool_args.clone(),
            tool_result,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("afterTool failed: {e}"));

        // 6. afterRun
        after_run(&p, &inv_context).await;

        // Verify created YAML content
        let data = std::fs::read(generated(&temp_dir))
            .unwrap_or_else(|e| panic!("failed to read recordings file: {e}"));

        let doc = gadk_goyaml::unmarshal_node(&data).unwrap();
        let recordings = from_go_node(&doc, doc.root())
            .unwrap_or_else(|e| panic!("failed to unmarshal yaml recordings: {e}"));

        // Assert that raw YAML string does not contain nulls, empty collections, or
        // default-zero/false properties
        let yaml_str = String::from_utf8(data).unwrap();
        for unexpected in [
            "null",
            "candidatecount",
            "thoughtsignature",
            "thought: false",
            "tools:",
        ] {
            assert!(
                !yaml_str.contains(unexpected),
                "YAML contains unexpected '{unexpected}' field:\n{yaml_str}"
            );
        }

        assert_eq!(
            recordings.recordings.len(),
            2,
            "expected exactly 2 recordings"
        );

        // First recording: LLM
        let r1 = &recordings.recordings[0];
        assert_eq!(r1.user_message_index, 0, "expected index 0");
        assert_eq!(r1.agent_name, "test_agent");
        let llm = r1
            .llm_recording
            .as_ref()
            .expect("expected LLM recording to be present");
        assert_eq!(llm.llm_request.as_ref().unwrap().model, "gemini-2.0-flash");

        // Second recording: Tool
        let r2 = &recordings.recordings[1];
        let tool = r2
            .tool_recording
            .as_ref()
            .expect("expected Tool recording to be present");
        assert_eq!(tool.tool_call.as_ref().unwrap().name, "test_tool");
        assert_eq!(
            tool.tool_response
                .as_ref()
                .unwrap()
                .response
                .as_ref()
                .unwrap()["result"],
            GoValue::String("test_success".to_owned())
        );
    }

    // t.Run("PathValidation")
    {
        let (_guard, temp_dir) = temp_dir();
        let safe_dir = temp_dir.join("safe");
        let _ = std::fs::create_dir(&safe_dir);

        let (p, session) = setup(&safe_dir);

        let tests = [
            ("ValidPath_InsideBaseDir", safe_dir.clone(), false),
            ("InvalidPath_ParentTraversal", safe_dir.join(".."), true),
            ("InvalidPath_AbsoluteOutside", "/etc".into(), true),
        ];

        for (name, dir, expect_error) in tests {
            set_config(
                &session,
                json!({"dir": dir.to_str().unwrap(), "user_message_index": 0}),
            );

            let inv_context = invocation_on(&session, &format!("test-invocation-{name}"));

            let err = before_run(&p, &inv_context).await;
            if expect_error {
                assert!(
                    err.is_err(),
                    "{name}: expected path error for {dir:?}, got nil"
                );
            } else {
                assert!(
                    err.is_ok(),
                    "{name}: unexpected path error for {dir:?}: {err:?}"
                );
            }
        }
    }

    // t.Run("MultiTurnAppendAndDeduplication")
    {
        let (_guard, temp_dir) = temp_dir();
        let (p, session) = setup(&temp_dir);
        let dir = temp_dir.to_str().unwrap();

        // One turn: beforeRun, beforeModel, afterModel, afterRun; errors ignored as in Go.
        let turn = |index: i64, inv_id: &'static str, model: &'static str, text: &'static str| {
            let p = p.clone();
            let session = session.clone();
            async move {
                set_config(&session, json!({"dir": dir, "user_message_index": index}));
                let inv = invocation_on(&session, inv_id);
                let _ = before_run(&p, &inv).await;
                let cb = callback_ctx(&inv);
                let _ = before_model(
                    &p,
                    &cb,
                    LlmRequest {
                        model: model.to_owned(),
                        ..Default::default()
                    },
                )
                .await;
                let _ = after_model(&p, &cb, text_response(text, false)).await;
                after_run(&p, &inv).await;
            }
        };

        // --- Turn 0 ---
        turn(0, "inv-0", "model-0", "Response 0").await;
        // --- Turn 1 Append ---
        turn(1, "inv-1", "model-1", "Response 1").await;

        // Read and verify both turns are recorded
        let data = std::fs::read(generated(&temp_dir))
            .unwrap_or_else(|e| panic!("failed to read recordings file: {e}"));
        let recs = unmarshal_ignoring_errors(&data);
        assert_eq!(
            recs.recordings.len(),
            2,
            "expected 2 recordings after multi-turn run"
        );
        assert_eq!(
            (
                recs.recordings[0].user_message_index,
                recs.recordings[1].user_message_index
            ),
            (0, 1),
            "unexpected sequence indexes"
        );

        // --- Same-Turn Deduplication/Overwrite ---
        turn(0, "inv-0-new", "model-0", "Response 0 Updated").await;

        let data = std::fs::read(generated(&temp_dir)).unwrap_or_default();
        let recs = unmarshal_ignoring_errors(&data);

        // Verify we still have exactly 2 recordings, but the turn 0 content is updated!
        assert_eq!(
            recs.recordings.len(),
            2,
            "expected exactly 2 recordings after deduplication"
        );
        let mut found_turn0 = false;
        for r in &recs.recordings {
            if r.user_message_index == 0 {
                found_turn0 = true;
                let got_text = response_text(&r.llm_recording.as_ref().unwrap().llm_responses[0]);
                assert_eq!(got_text, "Response 0 Updated", "expected overwritten text");
            }
        }
        assert!(
            found_turn0,
            "turn 0 recording was unexpectedly lost during deduplication"
        );
    }

    // t.Run("StreamingChunkAccumulation")
    {
        let (_guard, temp_dir) = temp_dir();
        let (p, session) = setup(&temp_dir);

        set_config(
            &session,
            json!({"dir": temp_dir.to_str().unwrap(), "user_message_index": 0}),
        );

        let inv_context = invocation_on(&session, "inv-stream");
        let _ = before_run(&p, &inv_context).await;

        let cb_context = callback_ctx(&inv_context);
        let _ = before_model(
            &p,
            &cb_context,
            LlmRequest {
                model: "model-stream".to_owned(),
                ..Default::default()
            },
        )
        .await;

        // 3 Partial responses
        for chunk in ["Chunk 1", "Chunk 2", "Chunk 3"] {
            let _ = after_model(&p, &cb_context, text_response(chunk, true)).await;
        }

        // 1 Final non-partial response
        let _ = after_model(&p, &cb_context, text_response("Final Chunk", false)).await;
        after_run(&p, &inv_context).await;

        let data = std::fs::read(generated(&temp_dir)).unwrap_or_default();
        let recs = unmarshal_ignoring_errors(&data);

        assert_eq!(recs.recordings.len(), 1, "expected 1 recording");
        let llm_rec = recs.recordings[0].llm_recording.as_ref().unwrap();
        assert_eq!(
            llm_rec.llm_responses.len(),
            4,
            "expected all 4 stream chunks to be accumulated"
        );
        assert_eq!(
            response_text(&llm_rec.llm_responses[3]),
            "Final Chunk",
            "unexpected final response chunk text"
        );
    }

    // t.Run("DisabledBypass")
    {
        let (_guard, temp_dir) = temp_dir();
        let (p, session) = setup(&temp_dir);

        // We don't set any recording configs in mockSession state

        let inv_context = invocation_on(&session, "inv-bypass");
        before_run(&p, &inv_context)
            .await
            .unwrap_or_else(|e| panic!("beforeRun should not error on empty config, got: {e}"));

        let cb_context = callback_ctx(&inv_context);
        before_model(
            &p,
            &cb_context,
            LlmRequest {
                model: "model-bypass".to_owned(),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("beforeModel should not error on empty config, got: {e}"));

        after_run(&p, &inv_context).await;

        assert!(
            !generated(&temp_dir).exists(),
            "recordings file was unexpectedly created even though the plugin was disabled"
        );
    }
}
