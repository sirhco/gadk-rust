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

//! The record and replay plugins under a runner with a scripted mock model and tools: the
//! written recordings file, replayed responses, parallel-agent synchronization and Go's error
//! texts.
//!
//! The goldens under `tests/fixtures/integration/record-tool-call/` were written by ADK-Go
//! itself (`docs/parity/ADK_GO_COMMIT`): a `go test -overlay` probe in package
//! `internal/configurable/conformance` (`scripts/gen-go-configurable/overlay/zz_t12probe_test.go.txt`,
//! whose header has the regeneration command) ran `llmagent` `root_agent` (instruction
//! `Use the tools.`, the conformance `validate_email` tool, `testutil.MockModel` answering a
//! `validate_email` call with id `call-1` and then `It is valid.`) under the public runner with
//! `recordplugin.MustNew(base)`, the session state holding `_adk_recordings_config` =
//! `{dir, user_message_index: 0, streaming_mode}`, once with `streaming_mode` `none` and once
//! with `sse` (and `agent.StreamingModeSSE`). The tests below run the same scenario through
//! gadk and compare the written bytes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt as _;
use futures::StreamExt as _;
use gadk_agent::{Agent, AgentConfig, AgentError, Context, RunConfig, StreamingMode, ToolError};
use gadk_configurable::conformance::{record, register_functions, replay};
use gadk_configurable::{LoadCtx, Registry};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::runner::PluginConfig;
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_engine::{RunOptions, Runner, RunnerConfig, llm_agent};
use gadk_genai::{Content, FunctionCall, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::Llm;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

const GOLDENS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/integration/record-tool-call"
);
const RECORDINGS: &str = "generated-recordings.yaml";
const RECORDINGS_SSE: &str = "generated-recordings-sse.yaml";
const USER_MESSAGE: &str = "Is a@b.co valid?";

/// The conformance `validate_email` tool, resolved through the registry as adkcli's configs do.
async fn validate_email() -> Arc<dyn gadk_agent::Tool> {
    let registry = Registry::new();
    register_functions(&registry).unwrap();
    let (tool, _) = registry
        .resolve_tool_reference(
            &LoadCtx::default(),
            "tools_agent_002.tools.validate_email",
            None,
        )
        .await
        .unwrap();
    tool.unwrap()
}

/// The model's tool call: `name(args)` with id `call-1`.
fn call(name: &str, args: Value) -> Content {
    Content::from_function_call(
        FunctionCall {
            id: "call-1".to_owned(),
            name: name.to_owned(),
            args: args.as_object().unwrap().clone(),
            ..Default::default()
        },
        ROLE_MODEL,
    )
}

/// Go's probe scenario: the model calls `validate_email` and then answers.
fn scripted_model() -> Arc<MockModel> {
    MockModel::from_contents(vec![
        call("validate_email", json!({"email": "a@b.co"})),
        Content::from_text("It is valid.", ROLE_MODEL),
    ])
}

fn llm(name: &str, model: Arc<MockModel>, tools: Vec<Arc<dyn gadk_agent::Tool>>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.to_owned(),
        instruction: "Use the tools.".to_owned(),
        model: Some(model as Arc<dyn Llm>),
        tools,
        ..Default::default()
    })
    .unwrap()
}

/// A runner over `agent` with adkcli's plugins (replay, then record; allowed base dir `base`)
/// and one session `s1` whose state is `state`.
async fn runner(agent: Arc<dyn Agent>, base: &Path, state: Value) -> Runner {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_owned(),
            user_id: "user".to_owned(),
            session_id: Some("s1".to_owned()),
            state: Some(state.as_object().unwrap().clone()),
        })
        .await
        .unwrap();
    Runner::new(RunnerConfig {
        app_name: "app".to_owned(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![replay::new(base), record::new(base)],
            ..Default::default()
        },
        auto_create_session: false,
        platform: Default::default(),
        compaction: None,
    })
    .unwrap()
}

/// Runs one user message; the events until the first error, and that error.
async fn run(r: &Runner, text: &str, mode: StreamingMode) -> (Vec<Event>, Option<AgentError>) {
    let mut s = r.run(
        "user",
        "s1",
        Some(Content::from_text(text, ROLE_USER)),
        RunConfig {
            streaming_mode: mode,
            ..Default::default()
        },
        RunOptions::default(),
    );
    let mut events = Vec::new();
    while let Some(item) = s.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(e) => return (events, Some(e)),
        }
    }
    (events, None)
}

/// Each non-partial event's parts as `author: <part>` lines (text, call or response).
fn transcript(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter(|e| !e.response.partial)
        .flat_map(|e| {
            let parts = e
                .response
                .content
                .as_ref()
                .map(|c| c.parts.clone())
                .unwrap_or_default();
            parts
                .into_iter()
                .map(move |p| format!("{}: {}", e.author, part_line(&p)))
        })
        .collect()
}

fn part_line(p: &Part) -> String {
    if let Some(fc) = &p.function_call {
        return format!("call {}({})", fc.name, Value::Object(fc.args.clone()));
    }
    if let Some(fr) = &p.function_response {
        return format!(
            "response {} {}",
            fr.name,
            Value::Object(fr.response.clone())
        );
    }
    format!("text {}", p.text)
}

fn case_dir(base: &Path) -> PathBuf {
    base.join("case")
}

/// Recording the scenario writes Go's bytes: `none` → `generated-recordings.yaml`, `sse` →
/// `generated-recordings-sse.yaml` (partial chunks and `turncomplete` included), each equal to
/// the file ADK-Go's record plugin wrote for the same run.
#[tokio::test]
async fn record_writes_the_file_go_writes_for_the_same_run() {
    for (mode, mode_name, file) in [
        (StreamingMode::None, "none", RECORDINGS),
        (StreamingMode::Sse, "sse", RECORDINGS_SSE),
    ] {
        let base = tempfile::tempdir().unwrap();
        let dir = case_dir(base.path());
        std::fs::create_dir_all(&dir).unwrap();
        let model = scripted_model();
        let agent = llm("root_agent", model.clone(), vec![validate_email().await]);
        let r = runner(
            agent,
            base.path(),
            json!({"_adk_recordings_config": {
                "dir": dir, "user_message_index": 0, "streaming_mode": mode_name,
            }}),
        )
        .await;
        let (events, err) = run(&r, USER_MESSAGE, mode).await;
        assert!(err.is_none(), "{mode_name}: {err:?}");
        assert_eq!(
            transcript(&events),
            [
                r#"root_agent: call validate_email({"email":"a@b.co"})"#,
                r#"root_agent: response validate_email {"result":true}"#,
                "root_agent: text It is valid.",
            ],
            "{mode_name}"
        );
        assert_eq!(model.requests().len(), 2, "{mode_name}");

        // Only the mode's file is written.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [file], "{mode_name}");
        let got = std::fs::read_to_string(dir.join(file)).unwrap();
        let want = std::fs::read_to_string(Path::new(GOLDENS).join(file)).unwrap();
        assert_eq!(got, want, "{mode_name}");
    }
}

/// The Go golden's structure, independently of the byte comparison: LLM call, tool call, LLM
/// call of `root_agent` for message 0, the second request carrying the call and its response.
#[test]
fn go_golden_structure() {
    let doc: gadk_configurable::GoValue = gadk_goyaml::unmarshal(
        &std::fs::read(Path::new(GOLDENS).join(RECORDINGS)).unwrap(),
        &gadk_goyaml::Options::default(),
    )
    .unwrap();
    let json = gadk_configurable::govalue::go_value_to_json(&doc).unwrap();
    let recs = json["recordings"].as_array().unwrap();
    let kinds: Vec<(&str, i64, &str)> = recs
        .iter()
        .map(|r| {
            let kind = if r.get("llm_recording").is_some() {
                "llm"
            } else {
                "tool"
            };
            (
                r["agent_name"].as_str().unwrap(),
                r["user_message_index"].as_i64().unwrap(),
                kind,
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("root_agent", 0, "llm"),
            ("root_agent", 0, "tool"),
            ("root_agent", 0, "llm")
        ]
    );
    assert_eq!(
        recs[1]["tool_recording"]["tool_response"]["response"],
        json!({"result": true})
    );
    let contents = recs[2]["llm_recording"]["llm_request"]["contents"]
        .as_array()
        .unwrap();
    assert_eq!(contents.len(), 3);
    assert_eq!(
        contents[1]["parts"][0]["function_call"]["name"],
        "validate_email"
    );
    assert_eq!(
        contents[2]["parts"][0]["function_response"]["response"],
        json!({"result": true})
    );
}

/// A replay case directory under `base` holding `yaml` as `generated-recordings.yaml`.
fn replay_case(base: &Path, yaml: &str) -> Value {
    let dir = case_dir(base);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(RECORDINGS), yaml).unwrap();
    json!({"_adk_replay_config": {"dir": dir, "user_message_index": 0}})
}

fn go_golden() -> String {
    std::fs::read_to_string(Path::new(GOLDENS).join(RECORDINGS)).unwrap()
}

/// Replaying Go's file: the recorded model responses come back in order, the tool response is
/// the recorded one, and the model is never called.
#[tokio::test]
async fn replay_of_the_go_file_returns_recorded_responses_without_the_model() {
    let base = tempfile::tempdir().unwrap();
    let state = replay_case(base.path(), &go_golden());
    let model = MockModel::from_contents(Vec::new());
    let agent = llm("root_agent", model.clone(), vec![validate_email().await]);
    let r = runner(agent, base.path(), state).await;
    let (events, err) = run(&r, USER_MESSAGE, StreamingMode::None).await;
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        transcript(&events),
        [
            r#"root_agent: call validate_email({"email":"a@b.co"})"#,
            r#"root_agent: response validate_email {"result":true}"#,
            "root_agent: text It is valid.",
        ]
    );
    assert!(model.requests().is_empty());
}

/// A tool whose live result is `{"value": <live>}` (switchable) and which counts its runs.
fn lookup_tool(
    live: Arc<parking_lot::Mutex<&'static str>>,
    runs: Arc<AtomicUsize>,
) -> Arc<dyn gadk_agent::Tool> {
    FunctionTool::new(
        FunctionToolConfig::new("lookup", "Looks a value up."),
        move |_ctx: Context, _args: Map<String, Value>| {
            let live = live.clone();
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let value = *live.lock();
                Ok::<Value, ToolError>(json!({"value": value}))
            }
        },
    )
    .unwrap()
}

/// Record with the tool answering `recorded`, then replay with it answering `live`: the
/// function tool still runs (Go runs non-agent function tools during replay), but the function
/// response is the recorded one, so the second model request matches its recording.
#[tokio::test]
async fn replay_returns_the_recorded_tool_response_and_still_runs_the_tool() {
    let base = tempfile::tempdir().unwrap();
    let dir = case_dir(base.path());
    std::fs::create_dir_all(&dir).unwrap();
    let live = Arc::new(parking_lot::Mutex::new("recorded"));
    let runs = Arc::new(AtomicUsize::new(0));
    let script = || {
        MockModel::from_contents(vec![
            call("lookup", json!({"key": "k"})),
            Content::from_text("Found it.", ROLE_MODEL),
        ])
    };

    let agent = llm(
        "root_agent",
        script(),
        vec![lookup_tool(live.clone(), runs.clone())],
    );
    let r = runner(
        agent,
        base.path(),
        json!({"_adk_recordings_config": {"dir": dir, "user_message_index": 0}}),
    )
    .await;
    let (_, err) = run(&r, "look k up", StreamingMode::None).await;
    assert!(err.is_none(), "{err:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let recorded = std::fs::read_to_string(dir.join(RECORDINGS)).unwrap();
    assert!(recorded.contains("value: recorded"), "{recorded}");

    *live.lock() = "live";
    let replay_base = tempfile::tempdir().unwrap();
    let state = replay_case(replay_base.path(), &recorded);
    let model = MockModel::from_contents(Vec::new());
    let agent = llm(
        "root_agent",
        model.clone(),
        vec![lookup_tool(live.clone(), runs.clone())],
    );
    let r = runner(agent, replay_base.path(), state).await;
    let (events, err) = run(&r, "look k up", StreamingMode::None).await;
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        transcript(&events),
        [
            r#"root_agent: call lookup({"key":"k"})"#,
            r#"root_agent: response lookup {"value":"recorded"}"#,
            "root_agent: text Found it.",
        ]
    );
    assert_eq!(runs.load(Ordering::SeqCst), 2, "the tool ran once more");
    assert!(model.requests().is_empty());
}

/// A user message the recording does not hold: Go's mismatch prefix (the diff body is gadk's
/// field-path diff, ledgered).
#[tokio::test]
async fn replay_request_mismatch_fails_with_go_prefix() {
    let base = tempfile::tempdir().unwrap();
    let state = replay_case(base.path(), &go_golden());
    let model = MockModel::from_contents(Vec::new());
    let agent = llm("root_agent", model.clone(), vec![validate_email().await]);
    let r = runner(agent, base.path(), state).await;
    let (events, err) = run(&r, "Is x@y.org valid?", StreamingMode::None).await;
    assert!(events.is_empty());
    let msg = err.unwrap().to_string();
    assert!(
        msg.starts_with("LLM request mismatch for agent 'root_agent' (index 0):\n"),
        "{msg}"
    );
    assert!(msg.contains("Is x@y.org valid?"), "{msg}");
    assert!(model.requests().is_empty());
}

/// The recording ends after the tool call: the runtime's second model request is one more than
/// recorded, Go's `runtime sent more requests than expected …` error.
#[tokio::test]
async fn replay_extra_request_fails_with_go_text() {
    let golden = go_golden();
    // Keep the first two recordings (LLM call, tool call); drop the final LLM call.
    let third = golden
        .match_indices("\n    - user_message_index:")
        .nth(2)
        .unwrap()
        .0;
    let base = tempfile::tempdir().unwrap();
    let state = replay_case(base.path(), &golden[..=third]);
    let model = MockModel::from_contents(Vec::new());
    let agent = llm("root_agent", model.clone(), vec![validate_email().await]);
    let r = runner(agent, base.path(), state).await;
    let (events, err) = run(&r, USER_MESSAGE, StreamingMode::None).await;
    assert_eq!(
        transcript(&events),
        [
            r#"root_agent: call validate_email({"email":"a@b.co"})"#,
            r#"root_agent: response validate_email {"result":true}"#,
        ]
    );
    assert_eq!(
        err.unwrap().to_string(),
        "runtime sent more requests than expected for agent 'root_agent' at user_message_index 0. Expected 2, but got request at index 2"
    );
    assert!(model.requests().is_empty());
}

/// Splits a recordings file into its header and one chunk per recording.
fn split_recordings(yaml: &str) -> (String, Vec<String>) {
    let marker = "    - user_message_index:";
    let mut chunks: Vec<String> = Vec::new();
    let mut header = String::new();
    for line in yaml.split_inclusive('\n') {
        if line.starts_with(marker) {
            chunks.push(String::new());
        }
        match chunks.last_mut() {
            Some(c) => c.push_str(line),
            None => header.push_str(line),
        }
    }
    (header, chunks)
}

/// `team`: a `ParallelAgent` over LLM agents `a` and `b`. With `gate`, `b` starts only after
/// `a` finished (before/after-agent callbacks), so recording is deterministic: Go's record
/// plugin keeps one pending LLM request per invocation, and concurrent LLM calls of parallel
/// agents overwrite each other's (Go and gadk alike).
fn team(a: Arc<MockModel>, b: Arc<MockModel>, gate: bool) -> Arc<dyn Agent> {
    let done = Arc::new(tokio::sync::Notify::new());
    let mut agent_a = LlmAgentConfig {
        name: "a".to_owned(),
        model: Some(a as Arc<dyn Llm>),
        ..Default::default()
    };
    let mut agent_b = LlmAgentConfig {
        name: "b".to_owned(),
        model: Some(b as Arc<dyn Llm>),
        ..Default::default()
    };
    if gate {
        let notify = done.clone();
        agent_a.after_agent_callbacks = vec![Arc::new(move |_ctx| {
            notify.notify_one();
            async { Ok(None) }.boxed()
        })];
        agent_b.before_agent_callbacks = vec![Arc::new(move |_ctx| {
            let done = done.clone();
            async move {
                done.notified().await;
                Ok(None)
            }
            .boxed()
        })];
    }
    parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "team".to_owned(),
            sub_agents: vec![
                llm_agent::new(agent_a).unwrap(),
                llm_agent::new(agent_b).unwrap(),
            ],
            ..Default::default()
        },
    })
    .unwrap()
}

/// Two LLM agents under a `ParallelAgent`, recorded (`a` then `b`) and replayed with the
/// recordings in that order and reversed: the replay plugin makes the agent whose recording
/// comes later wait for the earlier one (in either order, with both agents running
/// concurrently), hands each agent its own response, never calls a model, and does not
/// deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_agents_replay_in_either_recorded_order() {
    let base = tempfile::tempdir().unwrap();
    let dir = case_dir(base.path());
    std::fs::create_dir_all(&dir).unwrap();
    let r = runner(
        team(
            MockModel::from_contents(vec![Content::from_text("from a", ROLE_MODEL)]),
            MockModel::from_contents(vec![Content::from_text("from b", ROLE_MODEL)]),
            true,
        ),
        base.path(),
        json!({"_adk_recordings_config": {"dir": dir, "user_message_index": 0}}),
    )
    .await;
    let (_, err) = run(&r, "go", StreamingMode::None).await;
    assert!(err.is_none(), "{err:?}");
    let recorded = std::fs::read_to_string(dir.join(RECORDINGS)).unwrap();
    let (header, chunks) = split_recordings(&recorded);
    let agents: Vec<bool> = chunks
        .iter()
        .map(|c| c.contains("agent_name: a\n"))
        .collect();
    assert_eq!(agents, [true, false], "{recorded}");

    for reversed in [false, true] {
        let mut order = chunks.clone();
        if reversed {
            order.reverse();
        }
        let yaml = format!("{header}{}", order.concat());
        let replay_base = tempfile::tempdir().unwrap();
        let state = replay_case(replay_base.path(), &yaml);
        let (ma, mb) = (
            MockModel::from_contents(Vec::new()),
            MockModel::from_contents(Vec::new()),
        );
        let r = runner(
            team(ma.clone(), mb.clone(), false),
            replay_base.path(),
            state,
        )
        .await;
        let (events, err) =
            tokio::time::timeout(Duration::from_secs(10), run(&r, "go", StreamingMode::None))
                .await
                .expect("replay deadlocked");
        assert!(err.is_none(), "reversed={reversed}: {err:?}");
        let mut got = transcript(&events);
        got.sort();
        assert_eq!(
            got,
            ["a: text from a", "b: text from b"],
            "reversed={reversed}"
        );
        assert!(ma.requests().is_empty() && mb.requests().is_empty());
    }
}
