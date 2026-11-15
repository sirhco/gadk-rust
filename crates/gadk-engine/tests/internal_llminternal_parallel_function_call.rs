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

//! Port of ADK-Go `internal/llminternal/parallel_function_call_test.go` (httprr replay).

mod llminternal_support;

use gadk_agent::{Context, StreamingMode, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::testutil::gemini_test_model;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{
    Content, FinishReason, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER,
};
use gadk_model::LlmResponse;
use gadk_session::Event;
use llminternal_support::*;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Map, Value, json};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SumArgs {
    a: i64,
    b: i64,
}
#[derive(serde::Serialize, schemars::JsonSchema)]
struct SumResult {
    sum: i64,
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn calls(pairs: [(i64, i64); 3], model_version: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_parts(
            pairs
                .iter()
                .map(|(a, b)| Part::from_function_call("sum", obj(json!({"a": a, "b": b}))))
                .collect(),
            ROLE_MODEL,
        )),
        finish_reason: Some(FinishReason::Stop),
        model_version: model_version.into(),
        ..Default::default()
    }
}
fn responses(sums: [i64; 3]) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_parts(
            sums.iter()
                .map(|s| Part::from_function_response("sum", obj(json!({"sum": s}))))
                .collect(),
            ROLE_USER,
        )),
        ..Default::default()
    }
}
/// Final text; `pro` appends the empty (thought-signature-only) part gemini-3.1-pro returns.
fn answer(t: &str, pro: bool, model_version: &str) -> LlmResponse {
    let mut parts = vec![Part::from_text(t)];
    if pro {
        parts.push(Part::default());
    }
    LlmResponse {
        content: Some(Content::from_parts(parts, ROLE_MODEL)),
        finish_reason: Some(FinishReason::Stop),
        model_version: model_version.into(),
        ..Default::default()
    }
}

/// Port of expectedNonPartialLLMResponse25Flash / 3FlashPreview / 3ProPreview.
fn expected(model: &str) -> Vec<LlmResponse> {
    let (t1, t2, pro) = match model {
        "gemini-2.5-flash" => (
            "The sum of 2 and 3 is 5.\nThe sum of 4 and 5 is 9.\nThe sum of 6 and 7 is 13.",
            "The sum of 10 and 20 is 30.\nThe sum of 40 and 50 is 90.\nThe sum of 60 and 70 is 130.",
            false,
        ),
        "gemini-3-flash-preview" => (
            "The sum of 2 and 3 is 5, the sum of 4 and 5 is 9, and the sum of 6 and 7 is 13.",
            "The sum of 10 and 20 is 30, the sum of 40 and 50 is 90, and the sum of 60 and 70 is 130.",
            false,
        ),
        "gemini-3.1-pro-preview" => (
            "Here are the results of your additions:\n* 2 + 3 = 5\n* 4 + 5 = 9\n* 6 + 7 = 13",
            "Here are the results for those additions:\n* 10 + 20 = 30\n* 40 + 50 = 90\n* 60 + 70 = 130",
            true,
        ),
        other => panic!("unknown model {other}"),
    };
    vec![
        calls([(2, 3), (4, 5), (6, 7)], model),
        responses([5, 9, 13]),
        answer(t1, pro, model),
        calls([(10, 20), (40, 50), (60, 70)], model),
        responses([30, 90, 130]),
        answer(t2, pro, model),
    ]
}

/// Go ignoreFields: FunctionCall.ID, Part.ThoughtSignature, FunctionResponse.ID, LLMResponse.UsageMetadata.
fn normalize(mut r: LlmResponse) -> LlmResponse {
    r.usage_metadata = None;
    if let Some(c) = r.content.as_mut() {
        for p in &mut c.parts {
            p.thought_signature = Default::default();
            if let Some(fc) = p.function_call.as_mut() {
                fc.id.clear();
            }
            if let Some(fr) = p.function_response.as_mut() {
                fr.id.clear();
            }
        }
    }
    r
}

#[derive(Default)]
struct Seen {
    calls: Vec<FunctionCall>,
    responses: Vec<FunctionResponse>,
    partial_calls: Vec<FunctionCall>,
    partial_responses: Vec<FunctionResponse>,
    non_partial: Vec<LlmResponse>,
}
impl Seen {
    /// Port of Go `handleLoop`.
    fn handle(&mut self, ev: &Event) {
        let partial = ev.response.partial;
        if !partial {
            self.non_partial.push(ev.response.clone());
        }
        for part in ev.response.content.iter().flat_map(|c| c.parts.iter()) {
            if let Some(fc) = &part.function_call {
                if partial {
                    self.partial_calls.push(fc.clone())
                } else {
                    self.calls.push(fc.clone())
                }
            }
            if let Some(fr) = &part.function_response {
                if partial {
                    self.partial_responses.push(fr.clone())
                } else {
                    self.responses.push(fr.clone())
                }
            }
        }
    }
}

#[rstest]
#[case::replay_gemini_2_5_flash("gemini-2.5-flash", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/testdata/internal/llminternal/testdata/TestParallelFunctionCalls_test_parallel_function_calls_gemini-2.5-flash.httprr"))]
#[case::replay_gemini_3_flash_preview("gemini-3-flash-preview", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/testdata/internal/llminternal/testdata/TestParallelFunctionCalls_test_parallel_function_calls_gemini-3-flash-preview.httprr"))]
#[case::replay_gemini_3_1_pro_preview("gemini-3.1-pro-preview", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/testdata/internal/llminternal/testdata/TestParallelFunctionCalls_test_parallel_function_calls_gemini-3.1-pro-preview.httprr"))]
#[tokio::test]
async fn test_parallel_function_calls(#[case] model_name: &str, #[case] fixture: &str) {
    let gemini = gemini_test_model(model_name, fixture).await;
    let sum_tool = FunctionTool::new(
        FunctionToolConfig {
            // Copied verbatim from the fixture's functionDeclarations[0] (identical in all three fixtures).
            input_schema: Some(json!({"additionalProperties":false,"properties":{"a":{"type":"integer"},"b":{"type":"integer"}},"required":["a","b"],"type":"object"})),
            output_schema: Some(json!({"additionalProperties":false,"properties":{"sum":{"type":"integer"}},"required":["sum"],"type":"object"})),
            ..FunctionToolConfig::new("sum", "sums two integers")
        },
        |_ctx: Context, input: SumArgs| async move { Ok::<_, ToolError>(SumResult { sum: input.a + input.b }) },
    )
    .unwrap();
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "calculator".into(),
        description: "A calculator that can add two integers".into(),
        // Deliberate typo "recieve": it is in the recorded cassette prompt.
        instruction: "You are a calculator assistant. You will recieve requests to add two integers. Respond with the sum of the two integers and you must use the sum tool to calculate the sum.".into(),
        model: Some(gemini),
        tools: vec![sum_tool],
        ..Default::default()
    })
    .unwrap();
    let r = TestRunner::new(agent, "testApp", "testUser", "testSession").await;
    let mut seen = Seen::default();

    for ev in r
        .run(
            Content::from_parts(
                vec![Part::from_text(
                    "Can you add 2 and 3? Also 4 and 5? And 6 and 7?",
                )],
                ROLE_USER,
            ),
            StreamingMode::Sse,
        )
        .await
        .unwrap()
    {
        seen.handle(&ev);
    }
    assert_eq!(
        (seen.calls.len(), seen.responses.len()),
        (3, 3),
        "function calls / responses after turn 1"
    );
    assert_eq!(
        (seen.partial_calls.len(), seen.partial_responses.len()),
        (3, 0),
        "partial calls / responses after turn 1"
    );

    for ev in r
        .run(
            Content::from_parts(
                vec![Part::from_text(
                    "Great, now can you add 10 and 20? Also 40 and 50? And 60 and 70?",
                )],
                ROLE_USER,
            ),
            StreamingMode::Sse,
        )
        .await
        .unwrap()
    {
        seen.handle(&ev);
    }
    assert_eq!(
        (seen.calls.len(), seen.responses.len()),
        (6, 6),
        "function calls / responses after turn 2"
    );
    assert_eq!(
        (seen.partial_calls.len(), seen.partial_responses.len()),
        (6, 0),
        "partial calls / responses after turn 2"
    );

    let want = expected(model_name);
    for (i, ev) in seen.non_partial.iter().enumerate() {
        if i == 0 || i == 3 {
            assert!(
                !ev.content.as_ref().unwrap().parts[0]
                    .thought_signature
                    .is_empty(),
                "event[{i}]: expected non-empty thought signature"
            );
        }
        assert_eq!(normalize(ev.clone()), want[i], "diff in event[{i}]");
    }
}
