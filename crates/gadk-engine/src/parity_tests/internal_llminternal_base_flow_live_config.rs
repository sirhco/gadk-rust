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

//! Parity port of Go internal/llminternal/base_flow_live_config_test.go.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use gadk_agent::{
    AgentConfig, AgentError, InvocationContext, LiveRunConfig, RunConfig, StreamingMode, agent,
};
use gadk_genai::{
    AutomaticActivityDetection, EndSensitivity, ProactivityConfig, RealtimeInputConfig,
};
use gadk_model::LlmRequest;
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::llm::flow::{Flow, RequestProcessor};
use crate::llm::live::flow::run_live;
use crate::llm::live::test_server::{
    FakeConn, FakeLiveServer, assert_no_run_live_leak, live_config_processor, probe_of, serve,
};

/// A request processor that rewrites the request's model (Go's closure writing
/// `req.Model = requestModel`); the model it writes is fixed per test row below.
fn request_model_processor(model: &'static str) -> RequestProcessor {
    match model {
        "gemini-3.1-flash-live-preview" => rewrite_to_3x,
        _ => rewrite_to_25,
    }
}

fn rewrite_to_3x<'a>(
    _ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    req.model = "gemini-3.1-flash-live-preview".into();
    Box::pin(async { Ok(Vec::new()) })
}

fn rewrite_to_25<'a>(
    _ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    req.model = "gemini-live-2.5-flash-preview".into();
    Box::pin(async { Ok(Vec::new()) })
}

/// Go collectLiveSetupFields: every realtimeInputConfig, proactivity and enableAffectiveDialog
/// value in `v`, wherever the backend nests them.
fn collect_live_setup_fields(v: &Value, out: &mut Map<String, Value>) {
    match v {
        Value::Object(m) => {
            for (k, child) in m {
                if matches!(
                    k.as_str(),
                    "realtimeInputConfig" | "proactivity" | "enableAffectiveDialog"
                ) {
                    out.insert(k.clone(), child.clone());
                }
                collect_live_setup_fields(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_live_setup_fields(child, out);
            }
        }
        _ => {}
    }
}

/// Go liveSetupFields: runs RunLive for `model_name` with `live`, captures the setup frame the
/// client sends to a fake Live server, and returns its live setup fields. If `request_model`
/// is set, a request processor writes it to the request's model before the live config is
/// built.
async fn live_setup_fields(
    model_name: &str,
    request_model: Option<&'static str>,
    live: LiveRunConfig,
) -> Map<String, Value> {
    let (setup_tx, mut setup_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let server = FakeLiveServer::start_with_setup(
        Arc::new(move |_n, frame| {
            let _ = setup_tx.send(frame);
        }),
        serve(|_n: usize, mut conn: FakeConn| async move {
            conn.block_until_client_closes().await;
        }),
    )
    .await;
    let cancel = CancellationToken::new();
    let ctx = InvocationContext::builder()
        .agent(
            agent::new(AgentConfig {
                name: "test-live-agent".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .run_config(RunConfig {
            streaming_mode: StreamingMode::Bidi,
            live: Some(live),
            ..Default::default()
        })
        .cancellation_token(cancel.clone())
        .build_invocation();
    let mut processors: Vec<RequestProcessor> = vec![live_config_processor];
    if let Some(m) = request_model {
        processors.push(request_model_processor(m));
    }
    let flow = Arc::new(Flow {
        model: Some(server.model_named(model_name).await),
        request_processors: processors,
        ..Default::default()
    });
    let (sess, _stream) = run_live(flow, ctx).unwrap();
    let probe = probe_of(&sess);
    let frame = tokio::time::timeout(Duration::from_secs(10), setup_rx.recv())
        .await
        .expect("no setup frame within 10s")
        .expect("setup channel closed");
    let _ = sess.close().await;
    cancel.cancel();
    drop(_stream);
    assert_no_run_live_leak(&probe).await;
    let msg: Value = serde_json::from_str(&frame)
        .unwrap_or_else(|e| panic!("setup frame is not JSON: {e}\n{frame}"));
    let mut got = Map::new();
    collect_live_setup_fields(&msg, &mut got);
    got
}

/// RealtimeInputConfig, EnableAffectiveDialog and Proactivity from LiveRunConfig reach the
/// setup message, and the last two are left out for the models IsGemini3xLive matches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_sends_live_run_config_fields() {
    let live = || LiveRunConfig {
        realtime_input_config: Some(RealtimeInputConfig {
            automatic_activity_detection: Some(AutomaticActivityDetection {
                disabled: true,
                end_of_speech_sensitivity: Some(EndSensitivity::High),
                silence_duration_ms: Some(300),
                ..Default::default()
            }),
            ..Default::default()
        }),
        enable_affective_dialog: true,
        proactivity: Some(ProactivityConfig {
            proactive_audio: Some(true),
        }),
        ..Default::default()
    };
    let want_realtime = json!({
        "automaticActivityDetection": {
            "disabled": true,
            "endOfSpeechSensitivity": "END_SENSITIVITY_HIGH",
            "silenceDurationMs": 300,
        },
    });
    let all = json!({
        "realtimeInputConfig": want_realtime,
        "enableAffectiveDialog": true,
        "proactivity": {"proactiveAudio": true},
    });
    let realtime_only = json!({"realtimeInputConfig": want_realtime});
    // (name, model, request model, live, want)
    let cases: Vec<(&str, &str, Option<&'static str>, LiveRunConfig, Value)> = vec![
        (
            "gemini 2.5 live",
            "gemini-live-2.5-flash-preview",
            None,
            live(),
            all.clone(),
        ),
        (
            "gemini 3.x live",
            "gemini-3.1-flash-live-preview",
            None,
            live(),
            realtime_only.clone(),
        ),
        (
            "gemini 3.x live by resource name",
            "projects/p/locations/us-central1/publishers/google/models/gemini-3.1-flash-live-preview",
            None,
            live(),
            realtime_only.clone(),
        ),
        (
            // Live Translate is not treated as a 3.x Live model.
            "gemini 3.5 live translate",
            "gemini-3.5-live-translate-preview",
            None,
            live(),
            all.clone(),
        ),
        (
            // The connection is opened for the Flow's model, so the guard follows it even
            // when a request processor rewrites req.Model.
            "request model rewritten to gemini 3.x live",
            "gemini-live-2.5-flash-preview",
            Some("gemini-3.1-flash-live-preview"),
            live(),
            all.clone(),
        ),
        (
            "request model rewritten away from gemini 3.x live",
            "gemini-3.1-flash-live-preview",
            Some("gemini-live-2.5-flash-preview"),
            live(),
            realtime_only.clone(),
        ),
        // Each field is set on its own as well, so that dropping one of them cannot hide
        // behind another being set.
        (
            "realtime input config only",
            "gemini-live-2.5-flash-preview",
            None,
            LiveRunConfig {
                realtime_input_config: live().realtime_input_config,
                ..Default::default()
            },
            realtime_only.clone(),
        ),
        (
            "affective dialog only",
            "gemini-live-2.5-flash-preview",
            None,
            LiveRunConfig {
                enable_affective_dialog: true,
                ..Default::default()
            },
            json!({"enableAffectiveDialog": true}),
        ),
        (
            "proactivity only",
            "gemini-live-2.5-flash-preview",
            None,
            LiveRunConfig {
                proactivity: Some(ProactivityConfig {
                    proactive_audio: Some(true),
                }),
                ..Default::default()
            },
            json!({"proactivity": {"proactiveAudio": true}}),
        ),
        (
            "unset",
            "gemini-live-2.5-flash-preview",
            None,
            LiveRunConfig::default(),
            json!({}),
        ),
    ];
    for (name, model, request_model, live, want) in cases {
        let got = live_setup_fields(model, request_model, live).await;
        assert_eq!(want, Value::Object(got), "{name}: setup fields");
    }
}
