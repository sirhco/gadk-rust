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

//! Port of Go agent/llmagent/llmagent_compaction_test.go (package llmagent_test, mapped
//! external→internal because it reads compactioninternal).

#![cfg(feature = "testutil")]

use std::collections::HashSet;
use std::sync::Arc;

use gadk_agent::{BeforeModelCallback, Context, ToolError};
use gadk_genai::Content;
use gadk_session::compaction::{Config, LlmSummarizer, LlmSummarizerConfig, content};
use gadk_session::{Event, GetRequest};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::compaction::apply::has_usable_summary;
use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::testutil::{TestAgentRunner, collect_text_parts, gemini_test_model};
use crate::tool::{FunctionTool, FunctionToolConfig};

/// Go compactionModelName: the model this end-to-end test records against (the model name is
/// part of the request URL the cassette keys on).
pub(super) const COMPACTION_MODEL_NAME: &str = "gemini-3.5-flash";

/// The cassette a test replays.
pub(super) fn cassette(test: &str) -> String {
    format!(
        "{}/tests/testdata/agent/llmagent/testdata/{test}.httprr",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Port of Go compactionModel: one replaying model per test, shared by the agent and the
/// summarizer (Go memoises by t.Name()).
pub(super) async fn compaction_model(test: &str) -> Arc<dyn gadk_model::Llm> {
    gemini_test_model(COMPACTION_MODEL_NAME, cassette(test)).await
}

/// Port of Go promptHasFunctionTraffic.
pub(super) fn prompt_has_function_traffic(contents: &[Content]) -> bool {
    contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .any(|p| p.function_call.is_some() || p.function_response.is_some())
}

/// Port of Go callKey.
fn call_key(id: &str, name: &str) -> String {
    if !id.is_empty() {
        format!("id:{id}")
    } else {
        format!("name:{name}")
    }
}

/// Port of Go assertNoOrphanFunctionResponses (its informational t.Logf is not ported).
pub(super) fn assert_no_orphan_function_responses(prompt_idx: usize, contents: &[Content]) {
    let mut seen_calls = HashSet::new();
    for c in contents {
        for part in &c.parts {
            if let Some(fc) = &part.function_call {
                seen_calls.insert(call_key(&fc.id, &fc.name));
            }
            if let Some(fr) = &part.function_response {
                assert!(
                    seen_calls.contains(&call_key(&fr.id, &fr.name)),
                    "prompt {prompt_idx} carries a function response {:?} with no preceding call",
                    fr.name
                );
            }
        }
    }
}

/// Port of Go sessionEventsFor.
pub(super) async fn session_events_for(r: &TestAgentRunner) -> Vec<Event> {
    r.session_service()
        .get(GetRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            session_id: r.session_id().to_string(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("session Get() error = {e}"))
        .events()
}

/// Port of Go textOf.
pub(super) fn text_of(c: Option<&Content>) -> String {
    c.map(|c| c.parts.iter().map(|p| p.text.as_str()).collect::<String>())
        .unwrap_or_default()
}

/// Port of Go promptTextOf.
fn prompt_text_of(contents: &[Content]) -> String {
    let mut b = String::new();
    for c in contents {
        for p in &c.parts {
            if !p.text.is_empty() {
                b.push_str(&format!("[{}] {}\n", c.role, p.text));
            } else if let Some(fc) = &p.function_call {
                b.push_str(&format!("[{}] CALL {}\n", c.role, fc.name));
            } else if let Some(fr) = &p.function_response {
                b.push_str(&format!("[{}] RESPONSE {}\n", c.role, fr.name));
            }
        }
    }
    b
}

/// Port of Go sessionHoldsText.
fn session_holds_text(events: &[Event], want: &str) -> bool {
    events
        .iter()
        .filter_map(content)
        .flat_map(|c| c.parts.iter())
        .any(|p| p.text.contains(want))
}

/// Port of Go promptMentionsCallID.
fn prompt_mentions_call_id(contents: &[Content], id: &str) -> bool {
    contents.iter().flat_map(|c| c.parts.iter()).any(|p| {
        p.function_call.as_ref().is_some_and(|fc| fc.id == id)
            || p.function_response.as_ref().is_some_and(|fr| fr.id == id)
    })
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CityArgs {
    /// the city to look up
    city: String,
}

#[derive(Serialize, schemars::JsonSchema)]
struct WeatherResult {
    weather: String,
}

#[tokio::test]
async fn test_compaction_e2_e() {
    let model = compaction_model("TestCompactionE2E").await;

    // Captured before each model call: the exact history the agent sent.
    let prompts: Arc<Mutex<Vec<Vec<Content>>>> = Arc::default();
    let capture: BeforeModelCallback = {
        let prompts = prompts.clone();
        Arc::new(move |_ctx: Context, req: gadk_model::LlmRequest| {
            prompts.lock().push(req.contents.clone());
            Box::pin(async move { (req, Ok(None)) })
        })
    };

    // Schemas copied from the cassette's request so the declaration matches byte for byte.
    let weather = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(json!({
                "additionalProperties": false,
                "properties": {"city": {"description": "the city to look up", "type": "string"}},
                "required": ["city"],
                "type": "object"
            })),
            output_schema: Some(json!({
                "additionalProperties": false,
                "properties": {"weather": {"type": "string"}},
                "required": ["weather"],
                "type": "object"
            })),
            ..FunctionToolConfig::new("get_weather", "Returns the weather in a city.")
        },
        |_ctx: Context, args: CityArgs| async move {
            Ok::<_, ToolError>(WeatherResult {
                weather: format!("sunny in {}", args.city),
            })
        },
    )
    .unwrap();

    let a = llm_agent::new(LlmAgentConfig {
        name: "compaction_agent".into(),
        description: "agent used to exercise context compaction".into(),
        model: Some(model.clone()),
        instruction: "You are a concise assistant. Answer in one short sentence.".into(),
        tools: vec![weather],
        before_model_callbacks: vec![capture],
        disallow_transfer_to_parent: true,
        disallow_transfer_to_peers: true,
        ..Default::default()
    })
    .unwrap();

    // An explicit summarizer with no timeout (a deadline travels to the wire as a header).
    let summarizer = LlmSummarizer::new(LlmSummarizerConfig {
        model: Some(model.clone()),
        ..Default::default()
    })
    .unwrap();
    let r = TestAgentRunner::with_compaction(
        a,
        Config {
            compaction_interval: 2,
            summarizer: Some(Arc::new(summarizer)),
            ..Default::default()
        },
    )
    .await;

    // Go runs on session "compaction_session"; the Rust TestAgentRunner has the fixed session
    // "test_session". The session ID never reaches the model.
    let turns = [
        "What is the weather in Zurich?",
        "My favourite colour is teal, remember that.",
        "What was my favourite colour again?",
        "Now check the weather in Oslo.",
    ];
    let mut answers: Vec<Vec<String>> = vec![Vec::new(); turns.len()];
    let mut run_err = None;
    for (i, turn) in turns.iter().enumerate() {
        match collect_text_parts(r.run(turn)).await {
            Ok(a) => answers[i] = a,
            Err(e) => {
                run_err = Some((i, e));
                break;
            }
        }
    }

    // The offline checks run on whatever was captured, before any run failure is reported.
    let captured = prompts.lock().clone();
    for (i, p) in captured.iter().enumerate() {
        assert_no_orphan_function_responses(i, p);
    }
    if let Some((i, e)) = run_err {
        panic!("turn {} ({:?}) failed: {e}", i + 1, turns[i]);
    }

    let events = session_events_for(&r).await;
    let summaries: Vec<&Event> = events.iter().filter(|e| has_usable_summary(e)).collect();
    assert!(
        !summaries.is_empty(),
        "no compaction event after {} turns, so this test exercised nothing",
        turns.len()
    );
    // The first summary, not the last: the last is written after the final model call.
    let covered = summaries[0].actions.compaction.as_ref().unwrap();
    let summary_text = text_of(covered.compacted_content.as_ref());
    assert!(
        !summary_text.trim().is_empty(),
        "the stored summary is empty"
    );

    assert!(
        captured.len() >= turns.len(),
        "captured {} prompts, want at least {}",
        captured.len(),
        turns.len()
    );
    let last = captured.last().unwrap();
    let final_text = prompt_text_of(last);
    assert!(
        final_text.contains(summary_text.trim()),
        "final prompt does not contain the stored summary.\nsummary:\n{summary_text}\n\nprompt:\n{final_text}"
    );
    assert!(
        !final_text.contains(turns[0]),
        "final prompt still contains the compacted first turn {:?}:\n{final_text}",
        turns[0]
    );
    assert!(
        final_text.contains(turns[turns.len() - 1]),
        "final prompt is missing the current turn {:?}:\n{final_text}",
        turns[turns.len() - 1]
    );
    assert!(
        !final_text.contains(turns[1]),
        "final prompt still contains the compacted second turn {:?}:\n{final_text}",
        turns[1]
    );

    // Compacting rather than truncating, asserted against the store.
    assert!(
        events.len() >= turns.len(),
        "the session holds {} events for {} turns, so history was deleted rather than compacted",
        events.len(),
        turns.len()
    );
    for want in turns {
        assert!(
            session_holds_text(&events, want),
            "turn {want:?} is no longer in the session: compaction must leave history intact"
        );
    }
    let recall = answers[2].join(" ").to_lowercase();
    assert!(
        recall.contains("teal"),
        "the model could not recall the color from the summary alone; answer was {recall:?}"
    );

    // Every event the first summary covers is gone from the final prompt (searched with the
    // summary removed, since it paraphrases the turns it covers).
    let outside_summary = final_text.replace(summary_text.trim(), "");
    let mut checked = 0;
    for ev in &events {
        if ev.actions.compaction.is_some()
            || ev.timestamp < covered.start_timestamp
            || ev.timestamp > covered.end_timestamp
        {
            continue;
        }
        let Some(c) = content(ev) else { continue };
        for part in &c.parts {
            let text = part.text.trim();
            if !text.is_empty() {
                checked += 1;
                assert!(
                    !outside_summary.contains(text),
                    "event {:?} is covered by the summary but its text is still in the final prompt: {:?}",
                    ev.id,
                    part.text
                );
            }
            if let Some(fc) = part.function_call.as_ref().filter(|fc| !fc.id.is_empty()) {
                checked += 1;
                assert!(
                    !prompt_mentions_call_id(last, &fc.id),
                    "event {:?} is covered but its function call {:?} is still in the final prompt",
                    ev.id,
                    fc.id
                );
            }
            if let Some(fr) = part
                .function_response
                .as_ref()
                .filter(|fr| !fr.id.is_empty())
            {
                checked += 1;
                assert!(
                    !prompt_mentions_call_id(last, &fr.id),
                    "event {:?} is covered but its function response {:?} is still in the final prompt",
                    ev.id,
                    fr.id
                );
            }
        }
    }
    assert!(
        checked >= 4,
        "the covered-range sweep made {checked} comparisons, want at least 4: it is not examining what it claims to"
    );

    let with_summary_and_tools = captured
        .iter()
        .filter(|p| {
            prompt_has_function_traffic(p) && prompt_text_of(p).contains(summary_text.trim())
        })
        .count();
    assert!(
        with_summary_and_tools > 0,
        "no recorded prompt carries both a summary and function traffic, so pairing across a summary was never exercised"
    );
}
