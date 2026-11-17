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

//! Running one case against one agent: a fresh session, the turns in order, the transcript.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gadk_agent::{Agent, RunConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};

use crate::result::{RunTranscript, ToolCall, TurnRecord};
use crate::spec::EvalCase;

/// The app name of every eval session.
pub(crate) const APP_NAME: &str = "gadk-eval";
/// The user id of every eval session.
pub(crate) const USER_ID: &str = "eval";

/// Runs `case` against `agent` in a fresh in-memory session, bounded by `timeout`.
///
/// The transcript keeps the turns completed (or started) before an error or the timeout, and
/// `error` says why the run stopped early.
pub(crate) async fn run_case(
    agent: Arc<dyn Agent>,
    case: &EvalCase,
    timeout: Duration,
) -> RunTranscript {
    let mut t = RunTranscript::default();
    match tokio::time::timeout(timeout, drive(agent, case, &mut t)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => t.error = Some(e),
        Err(_) => t.error = Some(format!("run timed out after {timeout:?}")),
    }
    t
}

async fn drive(
    agent: Arc<dyn Agent>,
    case: &EvalCase,
    t: &mut RunTranscript,
) -> Result<(), String> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent,
        session_service: sessions.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .map_err(|e| format!("creating the runner: {e}"))?;
    let session = sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: USER_ID.into(),
            session_id: None,
            state: Some(case.state.clone()),
        })
        .await
        .map_err(|e| format!("creating the session: {e}"))?;
    let session_id = session.id().to_owned();
    for (i, user) in case.turns.iter().enumerate() {
        let n = i + 1;
        t.turns.push(TurnRecord {
            user: user.clone(),
            reply: String::new(),
            calls: Vec::new(),
        });
        let mut events = runner.run(
            USER_ID,
            &session_id,
            Some(Content::from_text(user.clone(), ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(ev) = events.next().await {
            let ev = ev.map_err(|e| format!("turn {n}: {e}"))?;
            let resp = &ev.response;
            if !resp.error_code.is_empty() || !resp.error_message.is_empty() {
                return Err(format!(
                    "turn {n}: model error {}: {}",
                    resp.error_code, resp.error_message
                ));
            }
            if resp.partial {
                continue;
            }
            let Some(content) = &resp.content else {
                continue;
            };
            let final_response = ev.is_final_response();
            let Some(rec) = t.turns.last_mut() else {
                continue;
            };
            for p in &content.parts {
                if let Some(fc) = &p.function_call {
                    rec.calls.push(ToolCall {
                        name: fc.name.clone(),
                        args: fc.args.clone(),
                    });
                }
                if final_response && !p.thought {
                    rec.reply.push_str(&p.text);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    use futures::stream::BoxStream;
    use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
    use gadk_engine::testutil::MockModel;
    use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
    use gadk_genai::{FunctionCall, ROLE_MODEL};
    use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
    use serde_json::{Map, Value, json};

    fn case(turns: &[&str], state: Value) -> EvalCase {
        EvalCase {
            id: "c".into(),
            state: state.as_object().cloned().unwrap_or_default(),
            turns: turns.iter().map(|s| (*s).to_owned()).collect(),
            expect: vec![],
        }
    }

    fn agent(
        model: Arc<dyn Llm>,
        instruction: &str,
        tools: Vec<Arc<dyn gadk_agent::Tool>>,
    ) -> Arc<dyn Agent> {
        llm_agent::new(LlmAgentConfig {
            name: "a".into(),
            model: Some(model),
            instruction: instruction.into(),
            tools,
            ..Default::default()
        })
        .unwrap()
    }

    fn system_instruction(req: &LlmRequest) -> String {
        req.config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref())
            .map(|si| si.parts.iter().map(|p| p.text.as_str()).collect())
            .unwrap_or_default()
    }

    const T: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn turns_share_one_session_seeded_with_state() {
        let model = MockModel::from_contents(vec![
            Content::from_text("Which order?", ROLE_MODEL),
            Content::from_text("Refunds close after 30 days.", ROLE_MODEL),
        ]);
        let t = run_case(
            agent(model.clone(), "Customer tier: {tier}.", vec![]),
            &case(&["I want a refund", "Order 123"], json!({"tier": "gold"})),
            T,
        )
        .await;
        assert_eq!(t.error, None);
        let replies: Vec<_> = t.turns.iter().map(|r| r.reply.as_str()).collect();
        assert_eq!(replies, ["Which order?", "Refunds close after 30 days."]);
        assert_eq!(t.turns[1].user, "Order 123");
        let reqs = model.requests();
        assert!(system_instruction(&reqs[0]).contains("Customer tier: gold."));
        // The second request carries the first turn's history: same session.
        let texts: Vec<String> = reqs[1]
            .contents
            .iter()
            .flat_map(|c| c.parts.iter().map(|p| p.text.clone()))
            .collect();
        assert!(texts.contains(&"I want a refund".to_owned()), "{texts:?}");
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct LookupArgs {
        order: String,
    }

    #[tokio::test]
    async fn function_calls_are_recorded_per_turn() {
        let lookup = FunctionTool::new(
            FunctionToolConfig::new("lookup_order", "Looks up an order."),
            |_ctx, args: LookupArgs| async move { Ok(json!({ "order": args.order, "days": 40 })) },
        )
        .unwrap();
        let mut args = Map::new();
        args.insert("order".into(), json!("123"));
        let model = MockModel::from_contents(vec![
            Content::from_text("Which order?", ROLE_MODEL),
            Content::from_function_call(
                FunctionCall {
                    name: "lookup_order".into(),
                    args: args.clone(),
                    ..Default::default()
                },
                ROLE_MODEL,
            ),
            Content::from_text("That order is 40 days old.", ROLE_MODEL),
        ]);
        let t = run_case(
            agent(model, "", vec![lookup]),
            &case(&["refund", "order 123"], json!({})),
            T,
        )
        .await;
        assert_eq!(t.error, None);
        assert!(t.turns[0].calls.is_empty());
        assert_eq!(
            t.turns[1].calls,
            [ToolCall {
                name: "lookup_order".into(),
                args
            }]
        );
        assert_eq!(t.turns[1].reply, "That order is 40 days old.");
    }

    #[tokio::test]
    async fn an_error_event_fails_the_run() {
        let model = MockModel::new(vec![Ok(LlmResponse {
            error_code: "SAFETY".into(),
            error_message: "blocked".into(),
            ..Default::default()
        })]);
        let t = run_case(agent(model, "", vec![]), &case(&["hi"], json!({})), T).await;
        assert_eq!(
            t.error.as_deref(),
            Some("turn 1: model error SAFETY: blocked")
        );
        assert_eq!(t.turns.len(), 1);
    }

    #[tokio::test]
    async fn a_model_failure_fails_the_run_and_keeps_earlier_turns() {
        let model = MockModel::from_contents(vec![Content::from_text("first", ROLE_MODEL)]);
        let t = run_case(
            agent(model, "", vec![]),
            &case(&["one", "two"], json!({})),
            T,
        )
        .await;
        assert_eq!(t.turns[0].reply, "first");
        let e = t.error.unwrap();
        assert!(e.starts_with("turn 2: "), "{e}");
    }

    struct Hang;

    impl Llm for Hang {
        fn name(&self) -> &str {
            "hang"
        }
        fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
            futures::stream::pending().boxed()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_agent_times_out() {
        let t = run_case(
            agent(Arc::new(Hang), "", vec![]),
            &case(&["hi"], json!({})),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(t.error.as_deref(), Some("run timed out after 50ms"));
    }
}
