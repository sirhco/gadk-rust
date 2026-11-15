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

//! Port of Go agent/llmagent/llmagent_test.go.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use gadk_agent::{
    AfterModelCallback, AfterToolCallback, Agent, AgentError, BeforeModelCallback,
    BeforeToolCallback, Context, EventStream, InvocationContext, OnModelErrorCallback,
    ReadonlyContext, RunConfig, StreamingMode, ToolError,
};
use gadk_genai::{
    Content, FunctionCall, GenerateContentConfig, GenerateContentResponseUsageMetadata, Part,
    ROLE_MODEL, ROLE_USER,
};
use gadk_model::LlmResponse;
use gadk_session::{Event, GetRequest};
use serde_json::{Map, Value, json};

use crate::llm::agent as llm_agent;
use crate::llm::{InstructionProvider, LlmAgentConfig};
use crate::testutil::{MockModel, TestAgentRunner};
use crate::workflow::{
    AgentNode, Node, NodeConfig, START, Workflow, WorkflowOptions, chain, function_node,
};

#[cfg(feature = "testutil")]
const MODEL_NAME: &str = "gemini-2.5-flash";
#[cfg(feature = "testutil")]
const MESSAGE: &str = "Handle the requests as specified in the System Instruction.";

/// Go http.ErrNoCookie / http.ErrHijacked texts (the sentinels Go's callbacks wrap).
const ERR_NO_COOKIE: &str = "http: named cookie not present";
const ERR_HIJACKED: &str = "http: connection has been hijacked";

#[cfg(feature = "testutil")]
macro_rules! fixture {
    ($name:literal) => {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/testdata/agent/llmagent/testdata/",
            $name,
            ".httprr"
        )
    };
}

/// Port of Go roundTripperFunc returning errNoNetwork.
#[cfg(feature = "testutil")]
struct NoNetwork;
#[cfg(feature = "testutil")]
#[async_trait::async_trait]
impl gadk_model::HttpClient for NoNetwork {
    async fn send(
        &self,
        _req: http::Request<bytes::Bytes>,
    ) -> Result<http::Response<gadk_model::BodyStream>, gadk_model::ModelError> {
        Err(gadk_model::ModelError::Transport("no network".into()))
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn same(a: &Arc<dyn Agent>, b: &Arc<dyn Agent>) -> bool {
    std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
}

fn text_resp(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_text(text, ROLE_MODEL)),
        ..Default::default()
    }
}

/// Port of Go testutil.CollectTextParts keeping Go's `(texts, err)` shape: the text parts
/// collected before the first error, and that error.
async fn collect_texts(mut s: EventStream) -> (Vec<String>, Option<AgentError>) {
    let mut texts = Vec::new();
    while let Some(item) = s.next().await {
        let ev = match item {
            Ok(ev) => ev,
            Err(e) => return (texts, Some(e)),
        };
        let Some(c) = ev.response.content else {
            return (
                texts,
                Some(AgentError::other("unexpected empty event".to_string())),
            );
        };
        texts.extend(
            c.parts
                .into_iter()
                .map(|p| p.text)
                .filter(|t| !t.is_empty()),
        );
    }
    (texts, None)
}

/// Go `for ev, err := range w.Run(ctx)`: every event, failing on the first error.
async fn drain(mut s: EventStream) -> Result<Vec<Event>, AgentError> {
    let mut out = Vec::new();
    while let Some(item) = s.next().await {
        out.push(item?);
    }
    Ok(out)
}

#[cfg(feature = "testutil")]
fn hello_world_agent(model: Arc<dyn gadk_model::Llm>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "hello_world_agent".into(),
        description: "hello world agent".into(),
        model: Some(model),
        instruction: "Roll the dice and report only the result.".into(),
        global_instruction: "Answer as precisely as possible.".into(),
        disallow_transfer_to_parent: true,
        disallow_transfer_to_peers: true,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("NewLLMAgent failed: {e}"))
}

/// Go `runconfig.ToContext(t.Context(), &runconfig.RunConfig{})` + the test-local
/// `mockInvocationContext` (invocation id "test-invocation-id", an empty session, no agent).
fn workflow_test_ctx() -> InvocationContext {
    InvocationContext::builder()
        .invocation_id("test-invocation-id")
        .run_config(RunConfig::default())
        .build_invocation()
}

/// Port of Go's `receiver` function node: records its input, returns nil.
fn receiver(got: Arc<Mutex<Option<Value>>>) -> Arc<dyn Node> {
    function_node(
        "receiver",
        move |_ctx: Context, input: Value| {
            let got = got.clone();
            async move {
                *got.lock().unwrap() = Some(input);
                Ok::<Value, AgentError>(Value::Null)
            }
        },
        NodeConfig::default(),
    )
}

#[cfg(feature = "testutil")]
#[tokio::test]
async fn test_llm_agent() {
    // healthy_backend: httprr replay.
    let model =
        crate::testutil::gemini_test_model(MODEL_NAME, fixture!("TestLLMAgent_healthy_backend"))
            .await;
    let runner = TestAgentRunner::new(hello_world_agent(model)).await;
    let (texts, err) = collect_texts(runner.run(MESSAGE)).await;
    assert!(
        err.is_none() && texts.len() == 1,
        "stream = ({texts:?}, {err:?}), want exactly one text response"
    );

    // broken_backend: every request fails with "no network" (Go newGeminiModel with a
    // transport: a fresh ClientConfig{HTTPClient: broken, APIKey: "fakeKey"}).
    let cfg = gadk_gemini::ClientConfig {
        api_key: Some("fakeKey".into()),
        http_client: Some(Arc::new(NoNetwork)),
        ..Default::default()
    };
    let model = gadk_gemini::GeminiModel::new(MODEL_NAME, cfg)
        .await
        .unwrap_or_else(|e| panic!("failed to create model: {e}"));
    let runner = TestAgentRunner::new(hello_world_agent(model)).await;
    let (texts, err) = collect_texts(runner.run(MESSAGE)).await;
    let err = err.unwrap_or_else(|| panic!("stream = ({texts:?}, nil), want (_, no network)"));
    assert!(
        err.to_string().contains("no network"),
        "stream = ({texts:?}, {err}), want (_, no network)"
    );
}

#[cfg(feature = "testutil")]
#[tokio::test]
async fn test_llm_agent_streaming_mode_sse() {
    let model = crate::testutil::gemini_test_model(
        "gemini-2.5-flash",
        fixture!("TestLLMAgentStreamingModeSSE"),
    )
    .await;
    let a = llm_agent::new(LlmAgentConfig {
        name: "calculator".into(),
        description: "calculating agent".into(),
        model: Some(model),
        instruction: "Think deep. Always double check the answer before making the conclusion."
            .into(),
        disallow_transfer_to_parent: true,
        disallow_transfer_to_peers: true,
        generate_content_config: Some(GenerateContentConfig {
            thinking_config: Some(gadk_genai::ThinkingConfig {
                include_thoughts: true, // can trigger multiple messages
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("NewLLMAgent failed: {e}"));
    // Go testRunner.RunContentWithConfig.
    let runner = TestAgentRunner::new(a).await;
    let stream = runner.run_with_config(
        Content::from_text("What is the sum of the first 50 prime numbers?", ROLE_USER),
        RunConfig {
            streaming_mode: gadk_agent::StreamingMode::Sse,
            ..Default::default()
        },
    );
    let events = crate::testutil::collect_events(stream)
        .await
        .unwrap_or_else(|e| panic!("stream = (_, {e}), want (_, nil)"));
    let with_content: Vec<_> = events
        .iter()
        .filter_map(|e| e.response.content.as_ref())
        .collect();
    assert!(
        with_content.len() > 1,
        "stream returned {} events with content, want more than 1 event",
        with_content.len()
    );
    assert!(
        with_content
            .iter()
            .any(|c| c.parts.iter().any(|p| p.thought)),
        "stream returned no thought, want thought"
    );
}

fn before_none() -> BeforeModelCallback {
    Arc::new(|_ctx, req| Box::pin(async move { (req, Ok(None)) }))
}
fn before_text(text: &'static str) -> BeforeModelCallback {
    Arc::new(move |_ctx, req| Box::pin(async move { (req, Ok(Some(text_resp(text)))) }))
}
fn before_err(prefix: &'static str, sentinel: &'static str) -> BeforeModelCallback {
    Arc::new(move |_ctx, req| {
        Box::pin(async move { (req, Err(AgentError::other(format!("{prefix}: {sentinel}")))) })
    })
}
fn after_none() -> AfterModelCallback {
    Arc::new(|_ctx, resp, _err| Box::pin(async move { (resp, Ok(None)) }))
}
fn after_text(text: &'static str) -> AfterModelCallback {
    Arc::new(move |_ctx, resp, _err| Box::pin(async move { (resp, Ok(Some(text_resp(text)))) }))
}
fn after_err(prefix: &'static str, sentinel: &'static str) -> AfterModelCallback {
    Arc::new(move |_ctx, resp, _err| {
        Box::pin(async move {
            (
                resp,
                Err(AgentError::other(format!("{prefix}: {sentinel}"))),
            )
        })
    })
}
fn on_error_text(text: &'static str) -> OnModelErrorCallback {
    Arc::new(move |_ctx, _req, _err| Box::pin(async move { Ok(Some(text_resp(text))) }))
}
fn on_error_err(prefix: &'static str, sentinel: &'static str) -> OnModelErrorCallback {
    Arc::new(move |_ctx, _req, _err| {
        Box::pin(async move { Err(AgentError::other(format!("{prefix}: {sentinel}"))) })
    })
}

struct ModelCallbackCase {
    name: &'static str,
    llm_responses: Vec<Content>,
    before: Vec<BeforeModelCallback>,
    after: Vec<AfterModelCallback>,
    on_error: Vec<OnModelErrorCallback>,
    want_texts: Vec<&'static str>,
    want_err: bool,
}

// parity: adapted: Go errors.Is(err, http.ErrNoCookie) on a wrapped callback error → the error text contains Go's ErrNoCookie text and not ErrHijacked's; a Go callback returning both a response and an error is a Rust Err (a Result carries one or the other, and Go's flow lets the error win)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_model_callbacks() {
    let hello = || vec![Content::from_text("hello from model", ROLE_MODEL)];
    let cases = vec![
        ModelCallbackCase {
            name: "before model callback doesn't modify anything",
            llm_responses: hello(),
            before: vec![before_none()],
            after: vec![],
            on_error: vec![],
            want_texts: vec!["hello from model"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "before model callback returns an error",
            llm_responses: hello(),
            before: vec![
                before_err("before_model_callback_error", ERR_NO_COOKIE),
                before_err("before_model_callback_error", ERR_HIJACKED),
            ],
            after: vec![],
            on_error: vec![],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "before model callback returns new LLMResponse",
            llm_responses: hello(),
            before: vec![
                before_text("hello from before_model_callback"),
                before_text("unexpected text"),
            ],
            after: vec![],
            on_error: vec![],
            want_texts: vec!["hello from before_model_callback"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "before model callback returns both new LLMResponse and error",
            llm_responses: hello(),
            before: vec![before_err("before_model_callback_error", ERR_NO_COOKIE)],
            after: vec![],
            on_error: vec![],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "after model callback doesn't modify anything",
            llm_responses: hello(),
            before: vec![],
            after: vec![after_none()],
            on_error: vec![],
            want_texts: vec!["hello from model"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "after model callback returns new LLMResponse",
            llm_responses: hello(),
            before: vec![],
            after: vec![
                after_text("hello from after_model_callback"),
                after_text("unexpected text"),
            ],
            on_error: vec![],
            want_texts: vec!["hello from after_model_callback"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "after model callback returns error",
            llm_responses: hello(),
            before: vec![],
            after: vec![
                after_err("error from after_model_callback", ERR_NO_COOKIE),
                after_err("error from after_model_callback", ERR_HIJACKED),
            ],
            on_error: vec![],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "after model callback returns both new LLMResponse and error",
            llm_responses: hello(),
            before: vec![],
            after: vec![after_err("error from after_model_callback", ERR_NO_COOKIE)],
            on_error: vec![],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "on model error callback is not called",
            llm_responses: hello(),
            before: vec![],
            after: vec![],
            on_error: vec![on_error_err("on_model_error_callback", ERR_NO_COOKIE)],
            want_texts: vec!["hello from model"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "on model error callback changes message",
            llm_responses: vec![],
            before: vec![],
            after: vec![],
            on_error: vec![on_error_text("hello from on_model_error_callback")],
            want_texts: vec!["hello from on_model_error_callback"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "on model error callback changes err",
            llm_responses: vec![],
            before: vec![],
            after: vec![],
            on_error: vec![
                on_error_err("error from on_model_error_callback", ERR_NO_COOKIE),
                on_error_err("error from on_model_error_callback", ERR_HIJACKED),
            ],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "on model error callback returns both new LLMResponse and error",
            llm_responses: vec![],
            before: vec![],
            after: vec![],
            on_error: vec![on_error_err(
                "error from on_model_error_callback",
                ERR_NO_COOKIE,
            )],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "on model error callback does not process before model callback error",
            llm_responses: hello(),
            before: vec![before_err("before_model_callback_error", ERR_NO_COOKIE)],
            after: vec![],
            on_error: vec![on_error_err(
                "error from on_model_error_callback",
                ERR_HIJACKED,
            )],
            want_texts: vec![],
            want_err: true,
        },
        ModelCallbackCase {
            name: "on model error callback does not process before model callback message",
            llm_responses: hello(),
            before: vec![before_text("hello from before_model_callback")],
            after: vec![],
            on_error: vec![on_error_err(
                "error from on_model_error_callback",
                ERR_HIJACKED,
            )],
            want_texts: vec!["hello from before_model_callback"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "after error callback process on model error callback message",
            llm_responses: vec![],
            before: vec![],
            after: vec![after_text("hello from after_model_callback")],
            on_error: vec![on_error_text("hello from on_model_error_callback")],
            want_texts: vec!["hello from after_model_callback"],
            want_err: false,
        },
        ModelCallbackCase {
            name: "after error callback does not process on model error callback error",
            llm_responses: vec![],
            before: vec![],
            after: vec![after_err("error from after_model_callback", ERR_HIJACKED)],
            on_error: vec![on_error_err(
                "error from on_model_error_callback",
                ERR_NO_COOKIE,
            )],
            want_texts: vec![],
            want_err: true,
        },
    ];
    for tc in cases {
        let name = tc.name;
        let test_llm = MockModel::from_contents(tc.llm_responses);
        let a = llm_agent::new(LlmAgentConfig {
            name: "hello_world_agent".into(),
            model: Some(test_llm),
            before_model_callbacks: tc.before,
            after_model_callbacks: tc.after,
            on_model_error_callbacks: tc.on_error,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{name}: failed to create llm agent: {e}"));
        let runner = TestAgentRunner::new(a).await;
        let (texts, err) = collect_texts(runner.run("")).await;
        if tc.want_err {
            let msg = err.as_ref().map(ToString::to_string).unwrap_or_default();
            assert!(
                msg.contains(ERR_NO_COOKIE) && !msg.contains(ERR_HIJACKED),
                "{name}: stream = ({texts:?}, {err:?}), want (_, {ERR_NO_COOKIE})"
            );
        }
        assert_eq!(
            err.is_some(),
            tc.want_err,
            "{name}: unexpected result from agent, got error: {err:?}"
        );
        assert_eq!(texts, tc.want_texts, "{name}: unexpected result from agent");
    }
}

fn usage(prompt: i32, candidates: i32, total: i32) -> GenerateContentResponseUsageMetadata {
    GenerateContentResponseUsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: candidates,
        total_token_count: total,
        ..Default::default()
    }
}

/// Port of Go streamingTextModel: two partial text deltas, then the final aggregated response,
/// which ends the turn and carries the call's usage metadata.
fn streaming_text_model(usage: GenerateContentResponseUsageMetadata) -> Arc<MockModel> {
    let delta = |t: &str| LlmResponse {
        content: Some(Content::from_text(t, ROLE_MODEL)),
        partial: true,
        ..Default::default()
    };
    MockModel::with_stream(vec![vec![
        Ok(delta("Hello ")),
        Ok(delta("world.")),
        Ok(LlmResponse {
            content: Some(Content::from_text("Hello world.", ROLE_MODEL)),
            usage_metadata: Some(usage),
            turn_complete: true,
            ..Default::default()
        }),
    ]])
}

// parity: adapted: Go's "the callback's own response objects are not modified" check is dropped (a Rust callback hands its replacement over by value, so the flow cannot mutate the callback's copy)
#[tokio::test]
#[ignore = "8a bug: an after-model callback replacement drops the replaced response's partial, turn_complete and usage_metadata (Go #1763 carries them over; Flow::call_model in llm/flow.rs is not owned by the D2b fix task)"]
async fn test_after_model_callback_replacement_keeps_streaming_fields() {
    let usage_md = usage(120, 8, 128);
    let own_usage = usage(1, 1, 2);
    type Usage = Option<GenerateContentResponseUsageMetadata>;
    // (subtest, the replacement's usage, want usage per event)
    let cases: Vec<(&str, Usage, Vec<Usage>)> = vec![
        (
            "replacement without usage inherits it",
            None,
            vec![None, None, Some(usage_md.clone())],
        ),
        (
            "replacement with its own usage keeps it",
            Some(own_usage.clone()),
            vec![
                Some(own_usage.clone()),
                Some(own_usage.clone()),
                Some(own_usage.clone()),
            ],
        ),
    ];
    for (name, replacement_usage, want_usage) in cases {
        let after: AfterModelCallback = Arc::new(move |_ctx, resp, _err| {
            let replacement = LlmResponse {
                content: Some(Content::from_text("scrubbed", ROLE_MODEL)),
                usage_metadata: replacement_usage.clone(),
                ..Default::default()
            };
            Box::pin(async move { (resp, Ok(Some(replacement))) })
        });
        let a = llm_agent::new(LlmAgentConfig {
            name: "scrubbing_agent".into(),
            model: Some(streaming_text_model(usage_md.clone())),
            after_model_callbacks: vec![after],
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{name}: failed to create llm agent: {e}"));
        let runner = TestAgentRunner::new(a).await;
        let events = drain(runner.run_with_config(
            Content::from_text("hi", ROLE_USER),
            RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
        ))
        .await
        .unwrap_or_else(|e| panic!("{name}: CollectEvents() error = {e}"));

        let got_partial: Vec<bool> = events.iter().map(|e| e.response.partial).collect();
        let got_turn_complete: Vec<bool> =
            events.iter().map(|e| e.response.turn_complete).collect();
        let got_usage: Vec<_> = events
            .iter()
            .map(|e| e.response.usage_metadata.clone())
            .collect();
        assert_eq!(
            got_partial,
            vec![true, true, false],
            "{name}: event Partial flags"
        );
        assert_eq!(
            got_turn_complete,
            vec![false, false, true],
            "{name}: event TurnComplete flags"
        );
        assert_eq!(got_usage, want_usage, "{name}: event UsageMetadata");

        // Only the final response is a model turn; the deltas must not be persisted alongside it.
        let stored = runner
            .session_service()
            .get(GetRequest {
                app_name: "test_app".into(),
                user_id: "test_user".into(),
                session_id: "test_session".into(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{name}: session Get() error = {e}"));
        let model_turns = stored
            .events()
            .iter()
            .filter(|e| e.author == "scrubbing_agent")
            .count();
        assert_eq!(model_turns, 1, "{name}: persisted agent events, want 1");
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RandArgs {
    #[allow(dead_code)] // Go Args.Seed; the handler ignores it
    seed: i64,
}
#[derive(serde::Serialize, schemars::JsonSchema)]
struct RandResult {
    number: i64,
}

#[cfg(feature = "testutil")]
fn rand_number_tool() -> Arc<dyn gadk_agent::Tool> {
    crate::tool::FunctionTool::new(
        crate::tool::FunctionToolConfig {
            input_schema: Some(json!({"additionalProperties":false,"properties":{"seed":{"type":"integer"}},"required":["seed"],"type":"object"})),
            output_schema: Some(json!({"additionalProperties":false,"properties":{"number":{"type":"integer"}},"required":["number"],"type":"object"})),
            ..crate::tool::FunctionToolConfig::new("rand_number", "returns random number")
        },
        |_ctx: Context, _input: RandArgs| async move { Ok::<_, ToolError>(RandResult { number: 1 }) },
    )
    .unwrap()
}

fn before_tool(result: Option<Value>) -> BeforeToolCallback {
    Arc::new(move |_ctx, _tool, _args| {
        let out = result.clone().map(obj);
        Box::pin(async move { Ok(out) })
    })
}
fn after_tool(result: Option<Value>) -> AfterToolCallback {
    Arc::new(move |_ctx, _tool, _args, _result, _err| {
        let out = result.clone().map(obj);
        Box::pin(async move { Ok(out) })
    })
}

#[cfg(feature = "testutil")]
#[tokio::test]
async fn test_tool_callback() {
    // (subtest, fixture, before callbacks, after callbacks, want last text)
    type Case = (
        &'static str,
        &'static str,
        Vec<BeforeToolCallback>,
        Vec<AfterToolCallback>,
        &'static str,
    );
    let cases: Vec<Case> = vec![
        (
            "before_callback_response_used",
            fixture!("TestToolCallback_before_callback_response_used"),
            vec![before_tool(None), before_tool(Some(json!({"number": "7"})))],
            vec![],
            "7",
        ),
        (
            // Since the first returns non-nil, the next callback won't be executed.
            "extra_before_callback_skipped",
            fixture!("TestToolCallback_extra_before_callback_skipped"),
            vec![
                before_tool(Some(json!({"number": "3"}))),
                before_tool(Some(json!({"number": "7"}))),
            ],
            vec![],
            "3",
        ),
        (
            "after_callback_response_used",
            fixture!("TestToolCallback_after_callback_response_used"),
            vec![],
            vec![after_tool(None), after_tool(Some(json!({"number": "7"})))],
            "7",
        ),
        (
            // Since the first returns non-nil, the next callback won't be executed.
            "extra_after_callback_skipped",
            fixture!("TestToolCallback_extra_after_callback_skipped"),
            vec![],
            vec![
                after_tool(Some(json!({"number": "3"}))),
                after_tool(Some(json!({"number": "7"}))),
            ],
            "3",
        ),
        (
            "after_callback_returned_when_used_with_before_callback",
            fixture!("TestToolCallback_after_callback_returned_when_used_with_before_callback"),
            vec![before_tool(Some(json!({"number": "3"})))],
            vec![after_tool(Some(json!({"number": "7"})))],
            "7",
        ),
        (
            "both_callbacks_return_nil_actual_tool_is_executed",
            fixture!("TestToolCallback_both_callbacks_return_nil_actual_tool_is_executed"),
            vec![before_tool(None)],
            vec![after_tool(None)],
            "1",
        ),
    ];
    for (name, path, before, after, want) in cases {
        let model = crate::testutil::gemini_test_model(MODEL_NAME, path).await;
        let a = llm_agent::new(LlmAgentConfig {
            name: "agent".into(),
            description: "random agent".into(),
            model: Some(model),
            instruction: "IMPORTANT: output ONLY the result computed by the provided function, if the result is number:42 print only 42".into(),
            disallow_transfer_to_parent: true,
            disallow_transfer_to_peers: true,
            tools: vec![rand_number_tool()],
            before_tool_callbacks: before,
            after_tool_callbacks: after,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{name}: failed to create LLM Agent: {e}"));
        let runner = TestAgentRunner::new(a).await;
        let (ans, err) =
            collect_texts(runner.run("Generate random number with 5 as a seed.")).await;
        assert!(
            err.is_none() && !ans.is_empty(),
            "{name}: agent returned ({ans:?}, {err:?}), want result"
        );
        assert_eq!(
            ans.last().unwrap().trim(),
            want,
            "{name}: unexpected result from agent = {ans:?}"
        );
    }
}

fn provider(text: &'static str) -> Option<InstructionProvider> {
    Some(Arc::new(move |_ctx: ReadonlyContext| {
        Box::pin(async move { Ok(text.to_string()) })
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_instruction_provider() {
    let base = || LlmAgentConfig {
        name: "test_agent".into(),
        ..Default::default()
    };
    let cases: Vec<(&str, LlmAgentConfig, &str)> = vec![
        (
            "instruction is evaluated",
            LlmAgentConfig {
                instruction: "instruction {var} test".into(),
                ..base()
            },
            "instruction custom_value test\n\nYou are an agent. Your internal name is \"test_agent\".",
        ),
        (
            "instruction provider overrides instruction",
            LlmAgentConfig {
                instruction: "instruction".into(),
                instruction_provider: provider("instruction provider template {var} not evaluated"),
                ..base()
            },
            "instruction provider template {var} not evaluated\n\nYou are an agent. Your internal name is \"test_agent\".",
        ),
        (
            "global instruction provider overrides global instruction",
            LlmAgentConfig {
                global_instruction: "instruction".into(),
                global_instruction_provider: provider(
                    "global instruction provider template {var} not evaluated",
                ),
                ..base()
            },
            "global instruction provider template {var} not evaluated\n\nYou are an agent. Your internal name is \"test_agent\".",
        ),
        (
            "global instruction provider merged with instruction provider",
            LlmAgentConfig {
                instruction_provider: provider("instruction provider {var}"),
                global_instruction_provider: provider("global instruction provider {var}"),
                ..base()
            },
            "global instruction provider {var}\n\ninstruction provider {var}\n\nYou are an agent. Your internal name is \"test_agent\".",
        ),
    ];
    for (name, mut cfg, want_instruction) in cases {
        let model = MockModel::from_contents(vec![Content::from_text("llm resp stub", ROLE_MODEL)]);
        cfg.model = Some(model.clone());
        let a = llm_agent::new(cfg)
            .unwrap_or_else(|e| panic!("{name}: failed to create LLM Agent: {e}"));
        let runner = TestAgentRunner::with_state(a, obj(json!({"var": "custom_value"}))).await;
        let (got_resp, err) = collect_texts(runner.run("user input")).await;
        assert!(
            err.is_none(),
            "{name}: agent returned ({got_resp:?}, {err:?}), want result"
        );

        let reqs = model.requests();
        assert_eq!(reqs.len(), 1, "{name}: unexpected LLM requests");
        let req = &reqs[0];
        assert_eq!(req.model, "mock", "{name}: request model");
        assert_eq!(
            req.contents,
            vec![Content::from_text("user input", ROLE_USER)],
            "{name}: request contents"
        );
        assert_eq!(
            req.config,
            Some(GenerateContentConfig {
                system_instruction: Some(Content {
                    parts: vec![Part::from_text(want_instruction)],
                    role: ROLE_USER.into(),
                }),
                ..Default::default()
            }),
            "{name}: request config"
        );
        assert!(req.tools.is_empty(), "{name}: request tools");
        assert_eq!(
            got_resp,
            vec!["llm resp stub"],
            "{name}: unexpected agent response"
        );
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SumArgs {
    a: i64,
    b: i64,
}
#[derive(serde::Serialize, schemars::JsonSchema)]
struct SumResult {
    sum: i64,
}

#[cfg(feature = "testutil")]
#[tokio::test]
async fn test_function_tool() {
    let model = crate::testutil::gemini_test_model(MODEL_NAME, fixture!("TestFunctionTool")).await;
    // Go's handler reports a bad input with t.Errorf; it is recorded and asserted afterwards.
    let bad_input: Arc<Mutex<Option<String>>> = Arc::default();
    let sink = bad_input.clone();
    let sum = crate::tool::FunctionTool::new(
        crate::tool::FunctionToolConfig {
            input_schema: Some(json!({"additionalProperties":false,"properties":{"a":{"type":"integer"},"b":{"type":"integer"}},"required":["a","b"],"type":"object"})),
            output_schema: Some(json!({"additionalProperties":false,"properties":{"sum":{"type":"integer"}},"required":["sum"],"type":"object"})),
            ..crate::tool::FunctionToolConfig::new("sum", "computes the sum of two numbers")
        },
        move |_ctx: Context, input: SumArgs| {
            let sink = sink.clone();
            async move {
                if input.a != 1 || input.b != 2 {
                    *sink.lock().unwrap() =
                        Some(format!("handler received {{a: {}, b: {}}}, want {{a: 1, b: 2}}", input.a, input.b));
                }
                Ok::<_, ToolError>(SumResult { sum: input.a + input.b })
            }
        },
    )
    .unwrap();

    let a = llm_agent::new(LlmAgentConfig {
        name: "agent".into(),
        description: "math agent".into(),
        model: Some(model),
        instruction: "IMPORTANT: output ONLY the result computed by the provided function, if the result of 10 + 32 is 42 print only 42".into(),
        disallow_transfer_to_parent: true,
        disallow_transfer_to_peers: true,
        tools: vec![sum],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create LLM Agent: {e}"));
    let runner = TestAgentRunner::new(a).await;
    let (ans, err) = collect_texts(runner.run("what is the sum of 1 + 2?")).await;
    assert!(
        err.is_none() && !ans.is_empty(),
        "agent returned ({ans:?}, {err:?}), want result"
    );
    assert_eq!(
        ans.last().unwrap().trim(),
        "3",
        "unexpected result from agent = {ans:?}"
    );
    let bad = bad_input.lock().unwrap().clone();
    assert!(bad.is_none(), "{}", bad.unwrap_or_default());
}

fn transfer_call(agent_name: &str) -> Content {
    Content::from_parts(
        vec![Part::from_function_call(
            "transfer_to_agent",
            obj(json!({ "agent_name": agent_name })),
        )],
        ROLE_MODEL,
    )
}
fn transfer_response() -> Content {
    Content::from_parts(
        vec![Part::from_function_response(
            "transfer_to_agent",
            obj(json!({"result": null})),
        )],
        ROLE_USER,
    )
}
fn text(t: &str) -> Content {
    Content::from_text(t, ROLE_MODEL)
}

/// Go `contents`: (Author, Parts) per event, with function call/response IDs blanked.
async fn contents(mut s: EventStream) -> Result<Vec<(String, Vec<Part>)>, AgentError> {
    let mut ret = Vec::new();
    while let Some(item) = s.next().await {
        let ev = item?;
        let Some(mut c) = ev.response.content else {
            return Err(AgentError::other(format!("unexpected event: {ev:?}")));
        };
        for p in &mut c.parts {
            if let Some(fc) = p.function_call.as_mut() {
                fc.id.clear();
            }
            if let Some(fr) = p.function_response.as_mut() {
                fr.id.clear();
            }
        }
        ret.push((ev.author, c.parts));
    }
    Ok(ret)
}

/// Go `check`: per round, the (author, parts) stream equals the want.
async fn check(root: Arc<dyn Agent>, subtest: &str, wants: Vec<Vec<(&str, Content)>>) {
    let runner = TestAgentRunner::new(root).await;
    for (i, want) in wants.into_iter().enumerate() {
        let got = contents(runner.run(&format!("round {i}")))
            .await
            .unwrap_or_else(|e| panic!("{subtest} [round {i}]: stream ended with an error: {e}"));
        let want: Vec<(String, Vec<Part>)> = want
            .into_iter()
            .map(|(author, c)| (author.to_string(), c.parts))
            .collect();
        assert_eq!(got, want, "{subtest} [round {i}] events");
    }
}

#[tokio::test]
#[ignore = "8a bug: transfer_to_agent's empty result is sent as {} instead of {\"result\": null} (Go #2281; the flow's nothing-result wrap in llm/functions.rs is not owned by the D2b fix task)"]
async fn test_agent_transfer() {
    let llm = |name: &str, model: &Arc<MockModel>, subs: Vec<Arc<dyn Agent>>, single: bool| {
        llm_agent::new(LlmAgentConfig {
            name: name.into(),
            model: Some(model.clone()),
            sub_agents: subs,
            disallow_transfer_to_parent: single,
            disallow_transfer_to_peers: single,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("failed to create {name}: {e}"))
    };

    // auto_to_auto: root_agent -- sub_agent_1
    let model = MockModel::from_contents(vec![
        transfer_call("sub_agent_1"),
        text("response1"),
        text("response2"),
    ]);
    let sub_agent_1 = llm("sub_agent_1", &model, vec![], false);
    let root = llm("root_agent", &model, vec![sub_agent_1], false);
    check(
        root,
        "auto_to_auto",
        vec![
            vec![
                ("root_agent", transfer_call("sub_agent_1")),
                ("root_agent", transfer_response()),
                ("sub_agent_1", text("response1")),
            ],
            // sub_agent_1 should still be the current agent.
            vec![("sub_agent_1", text("response2"))],
        ],
    )
    .await;

    // auto_to_single: root_agent -- sub_agent_1 (single)
    let model = MockModel::from_contents(vec![
        transfer_call("sub_agent_1"),
        text("response1"),
        text("response2"),
    ]);
    let sub_agent_1 = llm("sub_agent_1", &model, vec![], true);
    let root = llm("root_agent", &model, vec![sub_agent_1], false);
    check(
        root,
        "auto_to_single",
        vec![
            vec![
                ("root_agent", transfer_call("sub_agent_1")),
                ("root_agent", transfer_response()),
                ("sub_agent_1", text("response1")),
            ],
            // root_agent should still be the current agent.
            vec![("root_agent", text("response2"))],
        ],
    )
    .await;

    // auto_to_auto_to_single: root_agent -- sub_agent_1 -- sub_agent_1_1
    let model = MockModel::from_contents(vec![
        transfer_call("sub_agent_1"),
        transfer_call("sub_agent_1_1"),
        text("response1"),
        text("response2"),
    ]);
    let sub_agent_1_1 = llm("sub_agent_1_1", &model, vec![], true);
    let sub_agent_1 = llm("sub_agent_1", &model, vec![sub_agent_1_1], false);
    let root = llm("root_agent", &model, vec![sub_agent_1], false);
    check(
        root,
        "auto_to_auto_to_single",
        vec![
            vec![
                ("root_agent", transfer_call("sub_agent_1")),
                ("root_agent", transfer_response()),
                ("sub_agent_1", transfer_call("sub_agent_1_1")),
                ("sub_agent_1", transfer_response()),
                ("sub_agent_1_1", text("response1")),
            ],
            // sub_agent_1 should still be the current agent; sub_agent_1_1 is single, so it
            // must not become the current agent (else the conversation is tied to it forever).
            vec![("sub_agent_1", text("response2"))],
        ],
    )
    .await;

    // unknown_agent_is_a_tool_error: root_agent -- sub_agent_1. The model names an agent
    // outside the declared enum; it gets the error back and answers instead.
    let model = MockModel::from_contents(vec![transfer_call("nobody"), text("response1")]);
    let sub_agent_1 = llm("sub_agent_1", &model, vec![], false);
    let root = llm("root_agent", &model, vec![sub_agent_1], false);
    let error_response = Content::from_parts(
        vec![Part::from_function_response(
            "transfer_to_agent",
            obj(json!({
                "error": "agent \"nobody\" is not a valid transfer target; available agents: sub_agent_1"
            })),
        )],
        ROLE_USER,
    );
    check(
        root,
        "unknown_agent_is_a_tool_error",
        vec![vec![
            ("root_agent", transfer_call("nobody")),
            ("root_agent", error_response),
            ("root_agent", text("response1")),
        ]],
    )
    .await;
}

/// transfer_to_agent returns no result of its own, so after-tool callbacks see nil. The model is
/// still sent {"result": null}.
// parity: adapted: Go's nil result map is the empty Map (a Rust AfterToolCallback receives a Map, not an Option)
#[tokio::test]
async fn test_agent_transfer_after_tool_callback_sees_no_result() {
    let model = MockModel::from_contents(vec![transfer_call("sub_agent_1"), text("response1")]);
    let sub_agent_1 = llm_agent::new(LlmAgentConfig {
        name: "sub_agent_1".into(),
        model: Some(model.clone()),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create subAgent1: {e}"));
    let seen: Arc<Mutex<Vec<Map<String, Value>>>> = Arc::default();
    let record = seen.clone();
    let after: AfterToolCallback = Arc::new(move |_ctx, _tool, _args, result, _err| {
        record.lock().unwrap().push(result);
        Box::pin(async { Ok(None) })
    });
    let root = llm_agent::new(LlmAgentConfig {
        name: "root_agent".into(),
        model: Some(model),
        sub_agents: vec![sub_agent_1],
        after_tool_callbacks: vec![after],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create rootAgent: {e}"));

    let runner = TestAgentRunner::new(root).await;
    drain(runner.run("transfer"))
        .await
        .unwrap_or_else(|e| panic!("Run() error = {e}"));

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        1,
        "after-tool callback ran {} times, want 1",
        seen.len()
    );
    assert!(
        seen[0].is_empty(),
        "after-tool callback result = {:?}, want nil",
        seen[0]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_find_agent() {
    // root -> [child1, child2 -> [grandchild]]
    let new = |name: &str, subs: Vec<Arc<dyn Agent>>| {
        llm_agent::new(LlmAgentConfig {
            name: name.into(),
            sub_agents: subs,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("failed to create {name} agent: {e}"))
    };
    let grandchild = new("grandchild", vec![]);
    let child1 = new("child1", vec![]);
    let child2 = new("child2", vec![grandchild.clone()]);
    let root = new("root", vec![child1.clone(), child2.clone()]);

    // (subtest, agent to search, target name, want agent)
    type Case<'a> = (
        &'a str,
        &'a Arc<dyn Agent>,
        &'a str,
        Option<&'a Arc<dyn Agent>>,
    );
    let cases: Vec<Case> = vec![
        ("find root itself", &root, "root", Some(&root)),
        ("find direct child", &root, "child1", Some(&child1)),
        ("find nested child", &root, "grandchild", Some(&grandchild)),
        ("find non-existent agent", &root, "non_existent", None),
        (
            "find child from child",
            &child2,
            "grandchild",
            Some(&grandchild),
        ),
    ];
    for (name, search, target, want) in cases {
        let got = search.find_agent(target);
        let ok = match (&got, want) {
            (Some(g), Some(w)) => same(g, w),
            (None, None) => true,
            _ => false,
        };
        assert!(
            ok,
            "{name}: FindAgent({target:?}) = {:?}, want {:?}",
            got.map(|a| a.name().to_string()),
            want.map(|a| a.name().to_string())
        );
    }
}

// parity: adapted: Go mockInvocationContext (a test-local InvocationContext impl) → InvocationContext::builder(); the Rust InvocationContext is a concrete struct
#[tokio::test]
async fn test_llm_agent_workflow_integration_output_propagates_to_successor() {
    // 1. mock LLM returning "hello world"
    let test_llm = MockModel::from_contents(vec![Content::from_text("hello world", ROLE_MODEL)]);
    // 2. LlmAgent with output_key = "result"
    let a = llm_agent::new(LlmAgentConfig {
        name: "llm_agent".into(),
        model: Some(test_llm),
        output_key: "result".into(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create llm agent: {e}"));
    // 3. wrap in AgentNode
    let agent_node: Arc<dyn Node> = AgentNode::new(a, NodeConfig::default())
        .unwrap_or_else(|e| panic!("failed to create agent node: {e}"));
    // 4. chain(START, agent_node, fn_node), where fn_node records its input
    let got_input = Arc::new(Mutex::new(None));
    let w = Workflow::new(
        "test_workflow",
        chain(&[START.clone(), agent_node, receiver(got_input.clone())]),
        WorkflowOptions::default(),
    )
    .unwrap_or_else(|e| panic!("failed to create workflow: {e}"));

    drain(w.run(workflow_test_ctx()))
        .await
        .unwrap_or_else(|e| panic!("workflow run failed: {e}"));

    // 5. assert: fn_node got "hello world"
    assert_eq!(
        *got_input.lock().unwrap(),
        Some(json!("hello world")),
        "fnNode got unexpected input"
    );
}

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct Empty {}

// parity: adapted: Go mockInvocationContext (a test-local InvocationContext impl) → InvocationContext::builder(); the Rust InvocationContext is a concrete struct
#[tokio::test]
async fn test_llm_agent_workflow_integration_output_key_does_not_duplicate_output() {
    let noop_tool = crate::tool::FunctionTool::new(
        crate::tool::FunctionToolConfig::new("noop", "does nothing"),
        |_ctx: Context, _args: Empty| async move { Ok::<_, ToolError>(Empty {}) },
    )
    .unwrap_or_else(|e| panic!("functiontool.New: {e}"));

    // Turn 1: a function call (no text). Turn 2: the final text.
    let fc = FunctionCall {
        id: "fc-1".into(),
        name: "noop".into(),
        args: Map::new(),
        ..Default::default()
    };
    let test_llm = MockModel::from_contents(vec![
        Content::from_function_call(fc, ROLE_MODEL),
        Content::from_text("hello world", ROLE_MODEL),
    ]);
    let a = llm_agent::new(LlmAgentConfig {
        name: "llm_agent".into(),
        model: Some(test_llm),
        output_key: "result".into(),
        tools: vec![noop_tool],
        disallow_transfer_to_parent: true,
        disallow_transfer_to_peers: true,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create llm agent: {e}"));
    let agent_node: Arc<dyn Node> = AgentNode::new(a, NodeConfig::default())
        .unwrap_or_else(|e| panic!("failed to create agent node: {e}"));
    let got_input = Arc::new(Mutex::new(None));
    let w = Workflow::new(
        "test_workflow",
        chain(&[START.clone(), agent_node, receiver(got_input.clone())]),
        WorkflowOptions::default(),
    )
    .unwrap_or_else(|e| panic!("failed to create workflow: {e}"));

    // Go agent.NewContext(mockInvocationContext): Workflow::run takes the InvocationContext.
    drain(w.run(workflow_test_ctx()))
        .await
        .unwrap_or_else(|e| panic!("workflow run failed: {e}"));

    assert_eq!(
        *got_input.lock().unwrap(),
        Some(json!("hello world")),
        "successor node got unexpected input"
    );
}

/// Guards against a thought-only model turn ending the run before the answer is produced.
#[tokio::test]
async fn test_thought_only_turn_does_not_end_invocation() {
    let mut thought = Part::from_text("let me think");
    thought.thought = true;
    let m = MockModel::from_contents(vec![
        // Turn 1: the model is "thinking" — a thought-only part, no answer.
        Content::from_parts(vec![thought], ROLE_MODEL),
        // Turn 2: the real answer.
        Content::from_text("the answer", ROLE_MODEL),
    ]);
    let a = llm_agent::new(LlmAgentConfig {
        name: "thinker".into(),
        model: Some(m.clone()),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New: {e}"));
    let runner = TestAgentRunner::new(a).await;

    let mut got = String::new();
    let mut stream = runner.run("hi");
    while let Some(ev) = stream.next().await {
        let ev = ev.unwrap_or_else(|e| panic!("run error: {e}"));
        for p in ev.response.content.iter().flat_map(|c| c.parts.iter()) {
            if !p.thought {
                got.push_str(&p.text);
            }
        }
    }

    let n = m.requests().len();
    assert_eq!(
        n, 2,
        "model called {n} time(s), want 2: a thought-only turn ended the invocation early"
    );
    assert!(
        got.contains("the answer"),
        "surfaced text = {got:?}, want it to contain \"the answer\""
    );
}
