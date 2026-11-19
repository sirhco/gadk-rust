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

//! Port of Go `server/adkrest/compaction_integration_test.go`: context compaction configured
//! on the server reaches the runners it builds, and a config it cannot serve is refused at
//! construction.

use std::sync::Arc;

use gadk_agent::{AgentConfig, SingleLoader};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::testutil::MockModel;
use gadk_genai::Content;
use gadk_server::rest::testutil::{authenticate_as, post_json};
use gadk_server::rest::{Server, ServerConfig};
use gadk_session::compaction::{Config as CompactionConfig, SummarizeResult, Summarizer};
use gadk_session::{Event, GetRequest, InMemorySessionService, SessionService};
use serde_json::{Value, json};

const COMPACTION_APP: &str = "compaction_app";
const COMPACTION_USER: &str = "u";

/// Go `stubSummarizer`: a fixed summary, so the test does not depend on a model's wording.
struct StubSummarizer(&'static str);

#[async_trait::async_trait]
impl Summarizer for StubSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(SummarizeResult {
            content: Some(Content::from_text(self.0, "model")),
            usage: None,
        })
    }
}

/// Go `echoModel`: `answer <n>` for the n-th call; the prompts are its recorded requests.
fn echo_model() -> Arc<MockModel> {
    MockModel::from_contents(
        (1..=8)
            .map(|n| Content::from_text(format!("answer {n}"), "model"))
            .collect(),
    )
    .name_as("echo")
}

/// Go `newCompactionServer`: `compaction_app` on `model`, every caller authenticated as `u`.
fn new_compaction_server(
    model: Arc<MockModel>,
    sessions: Arc<dyn SessionService>,
    compaction: Option<CompactionConfig>,
) -> axum::Router {
    let root = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: COMPACTION_APP.into(),
        model: Some(model),
        instruction: "You are a helpful assistant.".into(),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(sessions);
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    cfg.compaction = compaction;
    cfg.authenticator = Some(authenticate_as(COMPACTION_USER));
    Server::new(cfg).unwrap().router()
}

/// Go `createCompactionSession`.
async fn create_compaction_session(router: &axum::Router) -> String {
    let resp: Value = post_json(
        router,
        &format!("/apps/{COMPACTION_APP}/users/{COMPACTION_USER}/sessions"),
        &json!({}),
    )
    .await;
    let id = resp["id"].as_str().unwrap_or_default().to_owned();
    assert!(!id.is_empty(), "create session returned an empty ID");
    id
}

/// Go `runCompactionTurn`.
async fn run_compaction_turn(router: &axum::Router, sid: &str, text: &str) {
    let _: Value = post_json(
        router,
        "/run",
        &json!({
            "appName": COMPACTION_APP,
            "userId": COMPACTION_USER,
            "sessionId": sid,
            "newMessage": {"role": "user", "parts": [{"text": text}]},
        }),
    )
    .await;
}

/// Go `sessionEvents`.
async fn session_events(svc: &Arc<dyn SessionService>, sid: &str) -> Vec<Event> {
    svc.get(GetRequest {
        app_name: COMPACTION_APP.into(),
        user_id: COMPACTION_USER.into(),
        session_id: sid.into(),
        ..Default::default()
    })
    .await
    .unwrap()
    .events()
}

/// Go `countCompactions` over `compactioninternal.HasUsableSummary` (a presence check of the
/// compacted content).
fn count_compactions(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|ev| {
            ev.actions
                .compaction
                .as_ref()
                .is_some_and(|c| c.compacted_content.is_some())
        })
        .count()
}

/// Go `compactionPromptText`: `[role] text` per text part.
fn compaction_prompt_text(contents: &[Content]) -> String {
    contents
        .iter()
        .flat_map(|c| {
            c.parts
                .iter()
                .filter(|p| !p.text.is_empty())
                .map(move |p| format!("[{}] {}\n", c.role, p.text))
        })
        .collect()
}

/// The guard that compaction is reachable from the REST server: `ServerConfig::compaction` →
/// runtime controller → runner config → compaction.
#[tokio::test]
async fn test_rest_compaction_enabled_via_server_config() {
    let model = echo_model();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let router = new_compaction_server(
        Arc::clone(&model),
        Arc::clone(&sessions),
        Some(CompactionConfig {
            compaction_interval: 2,
            summarizer: Some(Arc::new(StubSummarizer("SUMMARY-OF-EARLIER-TURNS"))),
            ..Default::default()
        }),
    );

    let sid = create_compaction_session(&router).await;
    run_compaction_turn(&router, &sid, "q1").await;
    run_compaction_turn(&router, &sid, "q2").await;

    let got = count_compactions(&session_events(&sessions, &sid).await);
    assert_eq!(
        got, 1,
        "session holds {got} compaction events after 2 turns, want 1; compaction is not reaching the REST runner"
    );

    // A third turn must be prompted with the summary rather than the raw turns.
    run_compaction_turn(&router, &sid, "q3").await;
    let prompt = compaction_prompt_text(&model.requests().last().unwrap().contents);
    assert!(
        prompt.contains("SUMMARY-OF-EARLIER-TURNS"),
        "prompt does not contain the summary:\n{prompt}"
    );
    for gone in ["q1", "q2"] {
        assert!(
            !prompt.contains(gone),
            "prompt still contains compacted turn {gone:?}:\n{prompt}"
        );
    }
}

/// Leaving the field unset keeps the previous behaviour exactly.
#[tokio::test]
async fn test_rest_compaction_disabled_by_default() {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let router = new_compaction_server(echo_model(), Arc::clone(&sessions), None);

    let sid = create_compaction_session(&router).await;
    for i in 0..4 {
        run_compaction_turn(&router, &sid, &format!("q{i}")).await;
    }

    let got = count_compactions(&session_events(&sessions, &sid).await);
    assert_eq!(
        got, 0,
        "session holds {got} compaction events with no config set, want 0"
    );
}

/// An unusable compaction config stops the server starting instead of failing every request.
#[test]
fn test_new_server_rejects_invalid_compaction_config() {
    let root = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "assistant".into(),
        model: Some(echo_model()),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    // Overlap without an interval: sliding-window compaction can never run.
    cfg.compaction = Some(CompactionConfig {
        overlap_size: 2,
        ..Default::default()
    });
    let Err(err) = Server::new(cfg) else {
        panic!("Server::new() accepted an invalid compaction config, want it refused at startup");
    };
    assert!(
        err.to_string().contains("compaction"),
        "error {err:?} does not name the offending field"
    );
}

/// A well-shaped config the server cannot run (no summarizer over a root agent with no model)
/// is refused at construction, naming the app.
#[test]
fn test_rest_compaction_startup_rejects_a_config_it_cannot_serve() {
    // A workflow agent has no model of its own.
    let root = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "wf_app".into(),
            ..Default::default()
        },
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    // Well-shaped, and unservable: no summarizer and no model to make one.
    cfg.compaction = Some(CompactionConfig {
        compaction_interval: 2,
        ..Default::default()
    });
    let Err(err) = Server::new(cfg) else {
        panic!("Server::new() accepted a compaction config that cannot serve its only app");
    };
    assert!(
        err.to_string().contains("wf_app"),
        "error {err:?} does not name the app the operator has to fix"
    );
}

/// A loader with no agents whose root panics, like gadk-configurable's conformance loader when
/// every `root_agent.yaml` fails to load (Go returns a nil root there).
struct NoRootLoader;

impl gadk_agent::Loader for NoRootLoader {
    fn list_agents(&self) -> Vec<String> {
        Vec::new()
    }

    fn load_agent(
        &self,
        name: &str,
    ) -> Result<Arc<dyn gadk_agent::Agent>, gadk_agent::LoaderError> {
        Err(gadk_agent::LoaderError(format!("agent {name} not found")))
    }

    fn root_agent(&self) -> Arc<dyn gadk_agent::Agent> {
        panic!("NoRootLoader has no root agent")
    }
}

/// The startup compaction check never asks the loader for its root agent (Go passes no agent):
/// a loader with no agents has nothing to check.
#[test]
fn test_compaction_startup_check_with_no_root_agent() {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(NoRootLoader));
    cfg.compaction = Some(CompactionConfig {
        compaction_interval: 2,
        ..Default::default()
    });
    if let Err(err) = Server::new(cfg) {
        panic!("Server::new() = {err:?}, want Ok: there is no app to check");
    }
}
