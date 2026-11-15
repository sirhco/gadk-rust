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

//! Port of ADK-Go `internal/llminternal/parallel_function_call_hitl_test.go`.

mod llminternal_support;

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use gadk_agent::{
    Agent, Context, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, StreamingMode, ToolError,
    original_call_from,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, ToolConfirmation};
use llminternal_support::*;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SecureActionArgs {
    #[allow(dead_code)]
    action_name: String,
}
#[derive(serde::Serialize, schemars::JsonSchema)]
struct SecureActionResult {
    executed: bool,
}

const PLACEHOLDER: &str =
    "error tool \"secure_action\" requires confirmation, please approve or reject";
const SUMMARY: &str = "Successfully processed your request after confirmation.";

/// Port of Go `hitlMockModel`.
#[derive(Default)]
struct HitlModel {
    calls: AtomicUsize,
}
impl Llm for HitlModel {
    fn name(&self) -> &str {
        "hitl-mock-model"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let parts = if call > 1 {
            vec![Part::from_text(SUMMARY)]
        } else {
            [
                ("secure_call_1", "deploy_prod"),
                ("secure_call_2", "delete_db"),
            ]
            .into_iter()
            .map(|(id, action)| Part {
                function_call: Some(FunctionCall {
                    id: id.into(),
                    name: "secure_action".into(),
                    args: json!({"action_name": action}).as_object().cloned().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect()
        };
        stream::iter(vec![Ok(LlmResponse {
            content: Some(Content::from_parts(parts, ROLE_MODEL)),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn hitl_agent() -> Arc<dyn Agent> {
    let secure_tool = FunctionTool::new(
        FunctionToolConfig {
            require_confirmation: true,
            ..FunctionToolConfig::new("secure_action", "performs a sensitive/secure action")
        },
        |_ctx: Context, _input: SecureActionArgs| async move {
            Ok::<_, ToolError>(SecureActionResult { executed: true })
        },
    )
    .unwrap();
    gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "hitl_tester".into(),
        description: "HITL tester agent".into(),
        instruction: "You are a secure helper.".into(),
        model: Some(Arc::new(HitlModel::default())),
        tools: vec![secure_tool],
        ..Default::default()
    })
    .unwrap()
}

/// Confirmation-call IDs for (secure_call_1, secure_call_2) found in the turn-1 confirmation event.
fn confirmation_ids(confirm_event: &Event) -> (String, String) {
    let (mut id1, mut id2) = (String::new(), String::new());
    for p in &confirm_event.response.content.as_ref().unwrap().parts {
        let fc = p
            .function_call
            .as_ref()
            .expect("confirmation part must be a function call");
        assert_eq!(
            fc.name, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
            "Expected function call name {REQUEST_CONFIRMATION_FUNCTION_CALL_NAME}"
        );
        let orig = original_call_from(fc).unwrap_or_else(|e| panic!("OriginalCallFrom: {e}"));
        match orig.id.as_str() {
            "secure_call_1" => id1 = fc.id.clone(),
            "secure_call_2" => id2 = fc.id.clone(),
            _ => {}
        }
    }
    assert!(
        !id1.is_empty() && !id2.is_empty(),
        "Failed to retrieve both confirmation function call IDs from turn 1 events"
    );
    (id1, id2)
}

fn approval(id: &str) -> Part {
    let json = serde_json::to_string(&ToolConfirmation {
        confirmed: true,
        ..Default::default()
    })
    .unwrap();
    Part {
        function_response: Some(FunctionResponse {
            name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.into(),
            id: id.into(),
            response: json!({"response": json}).as_object().cloned().unwrap(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn fr_part(id: &str, response: Value) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            name: "secure_action".into(),
            id: id.into(),
            response: response.as_object().cloned().unwrap(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go cmpopts.IgnoreFields(genai.FunctionResponse{}, "ID").
fn without_ids(parts: &[Part]) -> Vec<Part> {
    parts
        .iter()
        .cloned()
        .map(|mut p| {
            if let Some(fr) = p.function_response.as_mut() {
                fr.id.clear();
            }
            p
        })
        .collect()
}

#[tokio::test]
async fn test_parallel_function_calls_with_hitl() {
    let r = TestRunner::new(hitl_agent(), "testApp", "testUser", "testSession").await;

    // --- TURN 1 ---
    let turn1 = r
        .run(
            Content::from_parts(
                vec![Part::from_text("Please run prod deploy and db delete")],
                ROLE_USER,
            ),
            StreamingMode::Sse,
        )
        .await
        .unwrap();
    assert_eq!(turn1.len(), 3, "Expected 3 events in Turn 1");
    assert_eq!(turn1[0].response.content.as_ref().unwrap().parts.len(), 2);
    let placeholders = &turn1[1].response.content.as_ref().unwrap().parts;
    assert_eq!(
        placeholders,
        &vec![
            fr_part("secure_call_1", json!({"error": PLACEHOLDER})),
            fr_part("secure_call_2", json!({"error": PLACEHOLDER})),
        ]
    );
    assert_eq!(turn1[2].response.content.as_ref().unwrap().parts.len(), 2);
    let (id1, id2) = confirmation_ids(&turn1[2]);

    // --- TURN 2: user confirms both ---
    let turn2 = r
        .run(
            Content::from_parts(vec![approval(&id1), approval(&id2)], ROLE_USER),
            StreamingMode::Sse,
        )
        .await
        .unwrap();
    assert_eq!(turn2.len(), 2, "Expected 2 events in Turn 2");
    assert_eq!(
        without_ids(&turn2[0].response.content.as_ref().unwrap().parts),
        vec![
            fr_part("", json!({"executed": true})),
            fr_part("", json!({"executed": true}))
        ]
    );
    assert_eq!(
        turn2[1].response.content.as_ref().unwrap().parts[0].text,
        SUMMARY
    );
}

#[tokio::test]
async fn test_parallel_function_calls_with_partial_hitl() {
    let r = TestRunner::new(hitl_agent(), "testApp", "testUser", "testSession").await;
    let turn1 = r
        .run(
            Content::from_parts(
                vec![Part::from_text("Please run prod deploy and db delete")],
                ROLE_USER,
            ),
            StreamingMode::Sse,
        )
        .await
        .unwrap();
    assert_eq!(turn1.len(), 3, "Expected 3 events in Turn 1");
    let (_id1, id2) = confirmation_ids(&turn1[2]);

    // --- TURN 2: only secure_call_2 confirmed ---
    let turn2 = r
        .run(
            Content::from_parts(vec![approval(&id2)], ROLE_USER),
            StreamingMode::Sse,
        )
        .await
        .unwrap();
    assert_eq!(turn2.len(), 2, "Expected 2 events in Turn 2");
    assert_eq!(
        without_ids(&turn2[0].response.content.as_ref().unwrap().parts),
        vec![fr_part("", json!({"executed": true}))]
    );
    assert_eq!(
        turn2[1].response.content.as_ref().unwrap().parts[0].text,
        SUMMARY
    );
}

/// A confirmed tool that sets SkipSummarization ends the turn on resume, as it does when the
/// tool needs no confirmation. RequestConfirmationRequestProcessor runs the confirmed tool
/// before the model call, so its final response event comes out of preprocessing; the model
/// must not be called again to summarize it. adk-python stops on the same condition
/// (base_llm_flow.py, _run_one_step_async, preprocess_yielded_final_response).
#[tokio::test]
async fn test_confirmed_tool_with_skip_summarization_ends_the_turn() {
    let secure_tool = FunctionTool::new(
        FunctionToolConfig {
            require_confirmation: true,
            ..FunctionToolConfig::new("secure_action", "performs a sensitive/secure action")
        },
        |ctx: Context, _input: SecureActionArgs| async move {
            ctx.update_actions(|a| a.skip_summarization = true);
            Ok::<_, ToolError>(SecureActionResult { executed: true })
        },
    )
    .unwrap();
    let model = Arc::new(HitlModel::default());
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "hitl_tester".into(),
        model: Some(model.clone()),
        tools: vec![secure_tool],
        ..Default::default()
    })
    .unwrap();
    let r = TestRunner::new(agent, "testApp", "testUser", "testSession").await;

    // Turn 1: both calls ask for confirmation.
    let turn1 = r
        .run(
            Content::from_parts(
                vec![Part::from_text("Please run prod deploy and db delete")],
                ROLE_USER,
            ),
            StreamingMode::None,
        )
        .await
        .unwrap();
    let confirm_call_ids: Vec<String> = turn1
        .iter()
        .flat_map(|ev| ev.response.content.iter().flat_map(|c| c.parts.iter()))
        .filter_map(|p| p.function_call.as_ref())
        .filter(|fc| fc.name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME)
        .map(|fc| fc.id.clone())
        .collect();
    assert_eq!(
        confirm_call_ids.len(),
        2,
        "turn 1 asked for {} confirmations, want 2",
        confirm_call_ids.len()
    );

    // Turn 2: the user approves both.
    let reply = Content::from_parts(
        confirm_call_ids.iter().map(|id| approval(id)).collect(),
        ROLE_USER,
    );
    let turn2 = r.run(reply, StreamingMode::None).await.unwrap();

    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        1,
        "model calls: the confirmed tool's SkipSummarization response must end turn 2 without another model call"
    );
    assert_eq!(
        turn2.len(),
        1,
        "turn 2 yielded {} events, want 1 (the confirmed tool responses)",
        turn2.len()
    );
    assert!(
        turn2[0].is_final_response(),
        "turn 2 event IsFinalResponse() = false, want true"
    );
}
