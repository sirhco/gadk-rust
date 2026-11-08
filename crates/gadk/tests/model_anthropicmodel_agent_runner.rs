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

#![cfg(feature = "anthropic")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of ADK-Go `TestAgentRunner_MultiTurnImageAndTool`
//! (`model/anthropicmodel/anthropicmodel_test.go`): an LLM agent on Claude runs a multi-turn
//! conversation with an image and a function tool through the runner, replaying
//! `testdata/model/anthropicmodel/testdata/TestAgentRunner_MultiTurnImageAndTool.httprr` with
//! Go's `scrubAnthropicRequest`/`scrubAnthropicResponse`. Besides Go's assertions, the test
//! checks that the model sent exactly the recorded requests, byte for byte (the httprr lookup
//! alone compares canonicalized bodies).
//!
//! The ledger lists this Go test as skipped in the gadk-anthropic directory target (D5): it
//! needs the runner, which gadk-anthropic cannot depend on.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk::agent::{Context, ToolError};
use gadk::anthropic::{AnthropicModel, Backend, ClientConfig, MapEnv, ProcessEnv};
use gadk::engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk::engine::testutil::{TestAgentRunner, collect_events, collect_text_parts};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::genai::{Content, Part, ROLE_USER};
use gadk::model::ReqwestClient;
use gadk::session::GetRequest;
use gadk_httprr::{HttprrError, RecordReplay};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Go `samplePNG1x1`.
const SAMPLE_PNG_1X1: [u8; 73] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, //
    0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, //
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, //
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, //
    0xde, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, //
    0x54, 0x78, 0x9c, 0x62, 0x62, 0x60, 0xf8, 0x0f, //
    0x08, 0x00, 0x00, 0xff, 0xff, 0x01, 0x0b, 0x01, //
    0x02, 0xd6, 0x95, 0x43, 0xa8, 0x00, 0x00, 0x00, //
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, //
    0x82,
];

/// The URL and raw body of every request the model sent, in order.
type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

fn testdata_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/model/anthropicmodel/testdata")
        .join(file)
}

/// Go `openHTTPRR(t, rrfile)` with Go's scrubbers. A first request scrubber also keeps each
/// request's URL and body as the model sent them (httprr hands scrubbers a copy of the request
/// whose body is the model's bytes; only the default scrub of credentials headers precedes it).
fn open_httprr(file: &str, seen: Seen) -> RecordReplay {
    let path = testdata_path(file);
    let mut rr = RecordReplay::open(&path, Arc::new(ReqwestClient::default()))
        .unwrap_or_else(|e| panic!("httprr.Open({:?}) failed: {e}", path.display()));
    rr.scrub_req(Box::new(move |req| {
        seen.lock()
            .unwrap()
            .push((req.uri().to_string(), req.body().to_vec()));
        Ok(())
    }));
    rr.scrub_req(Box::new(|req| {
        // Go `scrubAnthropicRequest`: drop the credential and client-identification headers…
        let drop: Vec<_> = req
            .headers()
            .keys()
            .filter(|k| {
                let lower = k.as_str().to_ascii_lowercase();
                matches!(
                    lower.as_str(),
                    "authorization" | "x-api-key" | "user-agent" | "x-goog-api-client"
                ) || lower.starts_with("x-stainless-")
            })
            .cloned()
            .collect();
        for k in drop {
            req.headers_mut().remove(k);
        }
        // …canonicalize a Vertex `/projects/<p>/` segment…
        let path = req.uri().path().to_string();
        if let Some(idx) = path.find("/projects/") {
            let rest = &path[idx + "/projects/".len()..];
            if let Some(slash) = rest.find('/').filter(|&s| s > 0) {
                let mut uri = format!("{}/projects/test-project{}", &path[..idx], &rest[slash..]);
                if let Some(q) = req.uri().query() {
                    uri = format!("{uri}?{q}");
                }
                if let (Some(scheme), Some(authority)) = (req.uri().scheme(), req.uri().authority())
                {
                    uri = format!("{scheme}://{authority}{uri}");
                }
                *req.uri_mut() = uri.parse().map_err(|e| HttprrError::Wire(format!("{e}")))?;
            }
        }
        // …and compact a JSON body.
        if !req.body().is_empty()
            && let Some(compact) = json_compact(req.body())
        {
            *req.body_mut() = compact.into();
        }
        Ok(())
    }));
    rr.scrub_resp(Box::new(scrub_anthropic_response));
    rr
}

/// Go `json.Compact`: `None` when `data` is not JSON (Go keeps the body unchanged).
fn json_compact(data: &[u8]) -> Option<Vec<u8>> {
    serde_json::from_slice::<serde_json::Value>(data).ok()?;
    let mut out = Vec::with_capacity(data.len());
    let (mut in_string, mut escaped) = (false, false);
    for &b in data {
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {}
            b'"' => {
                in_string = true;
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    Some(out)
}

/// Go `scrubAnthropicResponse`: removes the `anthropic-organization-id`,
/// `anthropic-workspace-id`, `set-cookie`, `request-id` and `x-request-id` header lines
/// (case-insensitive) from the logged response, up to the first empty line.
fn scrub_anthropic_response(buf: &mut Vec<u8>) -> Result<(), HttprrError> {
    const DROPPED: [&str; 5] = [
        "anthropic-organization-id:",
        "anthropic-workspace-id:",
        "set-cookie:",
        "request-id:",
        "x-request-id:",
    ];
    let mut out: Vec<&[u8]> = Vec::new();
    let mut in_headers = true;
    for line in buf.split(|&b| b == b'\n') {
        if in_headers {
            let trimmed = line.strip_suffix(b"\r").unwrap_or(line);
            if trimmed.is_empty() {
                in_headers = false;
                out.push(line);
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if DROPPED.iter().any(|p| lower.starts_with(p.as_bytes())) {
                continue;
            }
        }
        out.push(line);
    }
    *buf = out.join(&b'\n');
    Ok(())
}

/// The request URLs and bodies of an httprr trace, in order (`httprr trace v1`, then entries
/// `<req len> <resp len>\n<req><resp>`; a request's body follows its blank line).
fn recorded_requests(file: &str) -> Vec<(String, Vec<u8>)> {
    let data = std::fs::read(testdata_path(file)).unwrap();
    let mut rest = data
        .strip_prefix(b"httprr trace v1\n".as_slice())
        .expect("httprr header");
    let mut out = Vec::new();
    while !rest.is_empty() {
        let nl = rest.iter().position(|&b| b == b'\n').unwrap();
        let sizes = std::str::from_utf8(&rest[..nl]).unwrap();
        let (req_len, resp_len) = sizes.split_once(' ').unwrap();
        let (req_len, resp_len): (usize, usize) =
            (req_len.parse().unwrap(), resp_len.parse().unwrap());
        let req = &rest[nl + 1..nl + 1 + req_len];
        rest = &rest[nl + 1 + req_len + resp_len..];
        let request_line = req.split(|&b| b == b'\r').next().unwrap();
        let url = std::str::from_utf8(request_line)
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .to_string();
        let body_at = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        out.push((url, req[body_at..].to_vec()));
    }
    out
}

// Go `paletteArgs` (a plain comment: a doc comment would become the schema's description).
#[derive(Deserialize, JsonSchema)]
struct PaletteArgs {
    /// Dominant color name
    color: String,
}

// Go `paletteResult`.
#[derive(Serialize, JsonSchema)]
struct PaletteResult {
    color: String,
    hex_code: String,
}

// parity: adapted: the TestAgentRunner session is gadk's fixed "test_session" (Go names it
// "multi_turn_session"; the ID never reaches the model); the recorded request bodies are also
// compared byte for byte.
#[tokio::test]
async fn test_agent_runner_multi_turn_image_and_tool() {
    let file = "TestAgentRunner_MultiTurnImageAndTool.httprr";
    let seen: Seen = Arc::default();
    let rr = open_httprr(file, seen.clone());
    let recording = rr.is_recording();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| recording && !k.is_empty())
        .unwrap_or_else(|| "test-anthropic-key".to_string());
    // Go `t.Setenv("ANTHROPIC_BASE_URL", "")` when replaying: no ambient environment.
    let env: Arc<dyn gadk::anthropic::EnvProvider> = if recording {
        Arc::new(ProcessEnv)
    } else {
        Arc::new(MapEnv(HashMap::new()))
    };

    let m = AnthropicModel::new(
        "claude-sonnet-4-5-20250929",
        ClientConfig {
            backend: Backend::Anthropic,
            api_key: Some(api_key),
            http_client: Some(Arc::new(rr)),
            env,
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("NewModel() error = {e}"));

    let tool_calls = Arc::new(AtomicUsize::new(0));
    let calls = tool_calls.clone();
    let palette_tool = FunctionTool::new(
        FunctionToolConfig::new(
            "get_palette_info",
            "Returns the canonical hex code for a color name.",
        ),
        move |_ctx: Context, args: PaletteArgs| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok::<_, ToolError>(PaletteResult {
                    color: args.color,
                    hex_code: "#0055FF".into(),
                })
            }
        },
    )
    .unwrap_or_else(|e| panic!("functiontool.New() error = {e}"));

    let a = llm_agent::new(LlmAgentConfig {
        name: "vision_palette_agent".into(),
        description: "Inspects an image and looks up its palette info.".into(),
        model: Some(m),
        instruction:
            "Identify the color of the image and call get_palette_info to report its hex code."
                .into(),
        tools: vec![palette_tool],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New() error = {e}"));

    let tr = TestAgentRunner::new(a).await;

    // Turn 1: send an inline PNG image + text prompt; agent calls get_palette_info and answers.
    let turn1_content = Content {
        role: ROLE_USER.into(),
        parts: vec![
            Part::from_bytes(SAMPLE_PNG_1X1.to_vec().into(), "image/png"),
            Part::from_text(
                "What color is this image and what does get_palette_info return for it?",
            ),
        ],
    };
    let turn1_events = collect_events(tr.run_content(turn1_content))
        .await
        .unwrap_or_else(|e| panic!("Turn 1 RunContent() error = {e}"));
    let n = tool_calls.load(Ordering::SeqCst);
    assert_eq!(n, 1, "Turn 1 toolCalls = {n}, want 1");
    assert!(
        turn1_events.len() >= 3,
        "Turn 1 emitted {} events, want at least 3 (tool call, tool response, final answer)",
        turn1_events.len()
    );
    let last_turn1 = turn1_events.last().unwrap();
    let content = last_turn1.response.content.as_ref();
    assert!(
        content.is_some_and(|c| !c.parts.is_empty() && c.parts[0].text.contains("#0055FF")),
        "Turn 1 final event content = {content:?}, want text containing #0055FF"
    );

    // Verify stored session events after Turn 1.
    let sess = tr
        .session_service()
        .get(GetRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            session_id: tr.session_id().into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("SessionService.Get() error = {e}"));
    let stored = sess.events().len();
    assert!(
        stored >= 4,
        "stored session has {stored} events after Turn 1, want at least 4"
    );

    // Turn 2: run a follow-up turn in the same session built from the stored events of Turn 1.
    let turn2_texts = collect_text_parts(tr.run("Summarize what you found in one short sentence."))
        .await
        .unwrap_or_else(|e| panic!("Turn 2 Run() error = {e}"));
    let joined_turn2 = turn2_texts.concat();
    assert!(
        joined_turn2.contains("#0055FF"),
        "Turn 2 text = {joined_turn2:?}, want summary referencing #0055FF"
    );

    // The model sent exactly the recorded requests: same URLs, byte-identical bodies.
    if !recording {
        let want = recorded_requests(file);
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), want.len(), "request count");
        for ((got_url, got_body), (want_url, want_body)) in got.iter().zip(&want) {
            assert_eq!(got_url, want_url);
            pretty_assertions::assert_eq!(
                String::from_utf8_lossy(got_body),
                String::from_utf8_lossy(want_body)
            );
        }
    }
}
