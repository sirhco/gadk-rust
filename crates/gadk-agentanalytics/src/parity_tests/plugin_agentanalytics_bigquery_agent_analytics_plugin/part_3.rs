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

//! Part of the port of ADK-Go `plugin/agentanalytics/bigquery_agent_analytics_plugin_test.go`:
//! the 13 Go tests from `TestValueEndingInKeyName` to the end of the Go file
//! (`TestNewBigQueryAgentAnalyticsPluginWithConfig_BQClientLifecycle`), in the Go file's order.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gadk_agent::{ContextDelta, InvocationContext, Plugin, PluginConfig, ToolError};
use gadk_genai::{Content, FileData, GenerateContentResponseUsageMetadata, Part};
use gadk_model::LlmRequest;
use gadk_model::rest::test_util::{FakeResponse, RecordedRequest, ScriptedClient};
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use super::helpers::{CallbackErrors, fake_tool, parts_of, text_content};
use super::support::{
    MockCtx, event_types, json_object_column, lookup, new_test_plugin_with_config,
    only_row_of_type, queue_processor, rows_of_type, setup_test_plugin, until_event_type,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::BigQueryClient;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{
    ClientOptions, Config, ContentFormatter, DropStats, Logger, SCHEMA_VERSION, SCHEMA_VERSION_KEY,
};
use crate::context::Context;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_batch_processor::support::{
    CommittedState, committed_stream_server,
};
use crate::plugin::rows::{cache_type, identity_of};
use crate::plugin::version::{ADK_MODULE_PATH, adk_version};
use crate::plugin::{
    AnalyticsPlugin, EventData, Payload, PluginState, as_bigquery_agent_analytics_plugin,
    new_bigquery_agent_analytics_plugin_with_clients,
    new_bigquery_agent_analytics_plugin_with_config, new_plugin,
};
use crate::redaction::{
    COLUMN_CONTENT, COLUMN_CONTENT_PARTS, REDACTED_SENSITIVE_TEXT_PLACEHOLDER,
    UNPARSEABLE_JSON_PLACEHOLDER,
};
use crate::row::{QueuedRow, Row, RowValue};
use crate::schema::events_schema;
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::trace::{CallbackCtx, Identity, RowSpan};
use crate::validate::validate_config;
use crate::write::gapic::WriteClient;

// ---------------------------------------------------------------------------------------------
// File-local Go helpers (bigquery_agent_analytics_plugin_test.go, arrow_test_helper_test.go)
// ---------------------------------------------------------------------------------------------

/// Go `errors.New(text)` as a tool error.
fn tool_err(text: &str) -> Arc<ToolError> {
    Arc::new(ToolError::Execution(text.to_owned().into()))
}

/// Go `mockTransport.roundTrip`.
type RoundTrip = Box<dyn Fn(&RecordedRequest) -> FakeResponse + Send + Sync>;

/// Go `mockTransport{roundTrip}`: `round_trip`'s answer (a `200 {}` job answer becomes a done
/// job); without one, a done job for job requests and `200 {}` otherwise.
fn mock_transport(round_trip: Option<RoundTrip>) -> Arc<ScriptedClient> {
    const DONE_JOB: &str = r#"{"jobReference":{"projectId":"test-project","jobId":"test-job"},"status":{"state":"DONE"}}"#;
    ScriptedClient::new(move |r| {
        if let Some(rt) = &round_trip {
            let resp = rt(r);
            if r.path.contains("/jobs") && resp.status == 200 {
                let body = String::from_utf8_lossy(&resp.body_bytes())
                    .trim()
                    .to_owned();
                if body.is_empty() || body == "{}" {
                    return FakeResponse::json(DONE_JOB);
                }
            }
            return resp;
        }
        if r.path.contains("/jobs") {
            return FakeResponse::json(DONE_JOB);
        }
        FakeResponse::json("{}")
    })
}

/// Go `bq.NewClient(ctx, project, option.WithHTTPClient(&http.Client{Transport: transport}))`.
fn bq_client_with(project: &str, transport: Arc<ScriptedClient>) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        http_client: Some(transport),
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new(project, &options).expect("bq.NewClient"))
}

/// Whether `parts` is one part whose `key` is the string `want`.
fn one_part_with(parts: &[Row], key: &str, want: &str) -> bool {
    parts.len() == 1 && parts[0].get(key) == Some(&RowValue::Str(want.to_owned()))
}

/// Go's `onlyRowOfType(t, rows, eventType)` read of a JSON object column at `path`.
fn json_at(row: &Row, column: &str, path: &[&str]) -> Option<Value> {
    lookup(&json_object_column(row, column), path).cloned()
}

// ---------------------------------------------------------------------------------------------
// TestValueEndingInKeyName
// ---------------------------------------------------------------------------------------------

/// One case of `TestValueEndingInKeyName`.
struct KeyNameCase {
    name: &'static str,
    text: String,
    /// The error_message, and also the content, content_parts and attributes text unless
    /// `want_content` is set.
    want: &'static str,
    want_content: &'static str,
    /// The AGENT_RESPONSE content.response, if it is not "text: '" + the content + "'".
    want_response: &'static str,
}

fn kc(name: &'static str, text: impl Into<String>, want: &'static str) -> KeyNameCase {
    KeyNameCase {
        name,
        text: text.into(),
        want,
        want_content: "",
        want_response: "",
    }
}

fn kc_content(
    name: &'static str,
    text: impl Into<String>,
    want: &'static str,
    want_content: &'static str,
) -> KeyNameCase {
    KeyNameCase {
        want_content,
        ..kc(name, text, want)
    }
}

fn kc_response(
    name: &'static str,
    text: impl Into<String>,
    want: &'static str,
    want_response: &'static str,
) -> KeyNameCase {
    KeyNameCase {
        want_response,
        ..kc(name, text, want)
    }
}

/// Go's `marshalN(s, n)`: `s` JSON-marshaled `n` times.
fn marshal_n(s: &str, n: usize) -> String {
    let mut s = s.to_owned();
    for _ in 0..n {
        s = serde_json::to_string(&Value::String(s)).expect("json.Marshal");
    }
    s
}

#[allow(clippy::too_many_lines)]
fn key_name_cases() -> Vec<KeyNameCase> {
    const RSTP: &str = REDACTED_SENSITIVE_TEXT_PLACEHOLDER;
    const UJP: &str = UNPARSEABLE_JSON_PLACEHOLDER;
    vec![
        kc(
            "password before a wrapped error",
            "connect user=bob password=dev-SECRET123-password: dial tcp 10.0.0.1:5432: connection refused",
            "connect user=bob password=[REDACTED] tcp 10.0.0.1:5432: connection refused",
        ),
        kc(
            "quoted password before an error",
            r#"login failed: password="SECRET123-secret": invalid"#,
            r#"login failed: password="[REDACTED]""#,
        ),
        kc_response(
            "API key before a wrapped error",
            "api_key=prodSECRET123-api-key: permission denied",
            "api_key=[REDACTED]",
            "text: 'api_key=[REDACTED]",
        ),
        kc(
            "token in prose",
            "my login is token=abc-SECRET123-token: please check",
            "my login is token=[REDACTED] check",
        ),
        kc(
            "temp: value ending in a key name before a wrapped error",
            "password=temp:SECRET123-token: dial tcp",
            "password=[REDACTED] tcp",
        ),
        kc(
            "temp: value before a wrapped error",
            "password=temp:SECRET123: dial tcp",
            "password=[REDACTED] tcp",
        ),
        kc(
            "user:temp: value before a wrapped error",
            "password=user:temp:SECRET123: dial tcp",
            "password=[REDACTED] tcp",
        ),
        kc(
            "quoted temp: value before a wrapped error",
            r#"password="temp:SECRET123": dial tcp"#,
            r#"password="[REDACTED]" tcp"#,
        ),
        kc_response(
            "temp: key on the line of credentials",
            "credentials: temp:foo: SECRET123",
            "credentials:[REDACTED]",
            "text: 'credentials:[REDACTED]",
        ),
        kc_response(
            "temp: key on an indented line under credentials",
            "credentials:\n  temp:foo: SECRET123",
            "credentials:\n  temp:foo:[REDACTED]",
            "text: 'credentials:[REDACTED]",
        ),
        kc_content(
            "password after an escaped line break in a JSON message",
            r#"{"msg":"retry\npassword=hunter2 vault: secret: kv/data/db-password: \"s3cr3t pass\""}"#,
            r#"{"msg":"retry\npassword=[REDACTED] vault: secret:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "password after an escaped line break in a JSON error",
            r#"{"error":"connect failed\npassword=hunter2 credentials: user/password:\"bob pw\""}"#,
            r#"{"error":"connect failed\npassword=[REDACTED] credentials:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "password after an escaped line break in a JSON object whose last field is a bare pair",
            r#"{"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\"","code":"token=abc"}"#,
            r#"{"code":"token=[REDACTED]","msg":"retry\npassword=[REDACTED] vault: secret:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON error with a password after an escaped line break",
            r#"POST /v1/login: 400: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefix with bracket tag before JSON error with a password after an escaped line break",
            r#"POST /v1/login [400]: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "bracket-led log line before JSON error with only an escaped-newline password",
            r#"[ERROR] POST /v1/login: 400: {"msg":"retry\npassword=SECRET123"}"#,
            UJP,
            UJP,
        ),
        kc_content(
            "bracket-prefixed JSON error with a password after an escaped line break and a raw marker",
            r#"[ERROR] POST /v1/login: 400: {"msg":"retry\npassword=SECRET123 vault: secret: kv/data/db-password: \"OTHER pass\""}"#,
            UJP,
            UJP,
        ),
        kc_content(
            "JSON error followed by trailing status text with a password after an escaped line break",
            r#"POST /v1/login: {"msg":"retry\npassword=SECRET123"} (status 400)"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object whose string ends with a value ending in a key name before a word",
            r#"{"msg":"retry\npassword=dev-SECRET123-password: dial"}"#,
            r#"{"msg":"retry\npassword=[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "JSON object with a password after an escaped line break and a final value ending in a key name",
            r#"{"msg":"retry\npassword=hunter2 token=abc-SECRET123-token: dial"}"#,
            r#"{"msg":"retry\npassword=[REDACTED] token=[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "JSON array whose string ends with a value ending in a key name before a word",
            r#"["retry\npassword=dev-SECRET123-password: dial"]"#,
            r#"["retry\npassword=[REDACTED]"]"#,
            RSTP,
        ),
        kc_content(
            "JSON object whose string ends with a value ending in a key name",
            r#"{"msg":"retry\npassword=dev-SECRET123-password"}"#,
            r#"{"msg":"retry\npassword=[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "JSON array whose string ends with a value ending in a key name",
            r#"["retry\npassword=dev-SECRET123-password"]"#,
            r#"["retry\npassword=[REDACTED]"]"#,
            RSTP,
        ),
        kc_content(
            "stray quote before JSON error with an escaped-newline password",
            r#"bad token " in body: {"msg":"retry\npassword=SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "escaped quote before JSON error with an escaped-newline password",
            r#"flag \"foo: {"msg":"retry\npassword=SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "non-JSON backslash before escaped quote and escaped-newline password",
            r#"err: "dir C:\work\"retry\npassword=SECRET123""#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "5-deep marshaled JSON string with an escaped-newline password",
            format!(
                "upstream: {}",
                marshal_n(r#""retry\npassword=SECRET123""#, 5)
            ),
            RSTP,
            RSTP,
        ),
        kc_content(
            "9-deep marshaled JSON string with an escaped-newline password",
            format!(
                "upstream: {}",
                marshal_n(r#""retry\npassword=SECRET123""#, 9)
            ),
            RSTP,
            RSTP,
        ),
        kc_content(
            "truncated unclosed JSON error body with an escaped-newline password",
            r#"upstream error: {"error":"login failed\npassword=SECRET123"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "single-quoted Python dict with an escaped-newline password",
            r"{'error': 'retry\npassword=SECRET123'}",
            UJP,
            UJP,
        ),
        kc_content(
            "prefixed single-quoted Python dict with an escaped-newline password",
            r"bad config: {'error': 'retry\npassword=SECRET123'}",
            RSTP,
            RSTP,
        ),
        kc_content(
            "unquoted escaped log line with an escaped-newline password",
            r"err=retry\npassword=SECRET123",
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where earlier token swallows escaped newline before value ending in key name",
            r#"{"msg":"token=abc\npassword: dev-SECRET123-password: dial"}"#,
            r#"{"msg":"token=[REDACTED]\npassword:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows escaped newline before value ending in key name",
            r#"POST /v1/login: 400: {"msg":"token=abc\npassword: dev-SECRET123-password: dial"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where earlier compound key value swallows escaped newline before password",
            r#"{"msg":"secret: kv/data/db-password: other\npassword: SECRET123"}"#,
            r#"{"msg":"secret:[REDACTED]\npassword:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier compound key value swallows escaped newline before password",
            r#"POST /v1/login: 400: {"msg":"secret: kv/data/db-password: other\npassword: SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where password colon is followed by escaped newline and space",
            r#"{"msg":"password:\n SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where password colon is followed by escaped newline and space",
            r#"POST /v1/login: 400: {"msg":"password:\n SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefixed single-quoted Python dict where earlier token swallows escaped newline before password",
            r"bad config: {'error': 'token=abc\npassword: SECRET123'}",
            RSTP,
            RSTP,
        ),
        kc_content(
            "truncated JSON error body where earlier token swallows escaped newline before password",
            r#"upstream error: {"error":"token=abc\npassword: SECRET123"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "bare token value ending with escaped quote before password key",
            r#"token=abc\"password\": SECRET123"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefixed quoted string where earlier token swallows escaped newline before password",
            r#"upstream failed: "token=abc\npassword: SECRET123""#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows percent-encoded newline before password",
            r#"POST /v1/login: 400: {"msg":"token=abc%0Apassword: SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows double-escaped unicode newline before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\\u000apassword: SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows escaped newline and unicode colon before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\npassword\u003a SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where earlier token swallows unicode-escaped backslash and n before password",
            r#"{"msg":"token=abc\u005cnpassword: SECRET123"}"#,
            r#"{"msg":"[REDACTED_SENSITIVE_TEXT]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows unicode-escaped backslash and n before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\u005cnpassword: SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where earlier token swallows percent-escaped backslash and n before password",
            r#"{"msg":"token=abc%5Cnpassword: SECRET123"}"#,
            r#"{"msg":"[REDACTED_SENSITIVE_TEXT]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows hex-escaped backslash and n before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\x5cnpassword: SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where earlier token swallows unicode-escaped backslash and unicode n before password",
            r#"POST /v1/login: 400: {"msg":"token=abc\u005c\u006epassword: SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where password colon is followed by escaped backspace and space",
            r#"{"msg":"password:\b SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "prefixed JSON object where password colon is followed by escaped backspace and space",
            r#"POST /v1/login: 400: {"msg":"password:\b SECRET123"}"#,
            RSTP,
            RSTP,
        ),
        kc_content(
            "plain text where password colon is followed by escaped backspace and space",
            r"password:\b SECRET123",
            RSTP,
            RSTP,
        ),
        kc_content(
            "JSON object where password colon is followed by unicode backspace and space",
            r#"{"msg":"password:\u0008 SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "JSON object where password colon is followed by unicode vertical tab and space",
            r#"{"msg":"password:\u000b SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
            RSTP,
        ),
        kc_content(
            "JSON object where password colon is followed by unicode NUL and space",
            r#"{"msg":"password:\u0000 SECRET123"}"#,
            r#"{"msg":"password:[REDACTED]"}"#,
            RSTP,
        ),
        KeyNameCase {
            name: "Go struct field ending in key name followed by space, closing brace, backspace byte and secret",
            text: "failed: {Password:abc-token: }\u{8}SECRET123".into(),
            want: "failed: {Password:[REDACTED]",
            want_content: "failed: {Password:[REDACTED]",
            want_response: "text: 'failed: {Password:[REDACTED]",
        },
        kc_content(
            "Go struct field ending in key name followed by space, backspace byte and closing brace",
            "failed: {Password:abc-token: \u{8}}",
            "failed: {Password:[REDACTED] \u{8}}",
            "failed: {Password:[REDACTED] \u{8}}",
        ),
        kc_content(
            "caller-supplied redaction placeholders splitting secret after escaped newline",
            r"password:\n [REDACTED]a[REDACTED]u[REDACTED]t[REDACTED]h[REDACTED] authorization=1 authorization=2",
            RSTP,
            RSTP,
        ),
    ]
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_value_ending_in_key_name() {
    let secrets = ["SECRET123", "hunter2", "s3cr3t", "bob pw", "OTHER pass"];
    for tt in key_name_cases() {
        let name = tt.name;
        let tp = setup_test_plugin().await;
        let p = tp.plugin.clone();
        let m_ctx = tp.ctx.clone();
        let mut errs = CallbackErrors::default();
        errs.add(
            (p.on_user_message_callback().unwrap())(
                m_ctx.invocation(),
                text_content("user", &tt.text),
            )
            .await,
        );
        let (_, r) = (p.before_model_callback().unwrap())(
            m_ctx.context(),
            LlmRequest {
                model: "gemini-test".into(),
                contents: vec![text_content("user", &tt.text)],
                ..LlmRequest::default()
            },
        )
        .await;
        errs.add(r);
        let mut input = Map::new();
        input.insert("input".into(), json!(tt.text));
        let mut output = Map::new();
        output.insert("output".into(), json!(tt.text));
        errs.add(
            (p.before_tool_callback().unwrap())(
                m_ctx.context(),
                fake_tool("lookup"),
                input.clone(),
            )
            .await,
        );
        errs.add(
            (p.after_tool_callback().unwrap())(
                m_ctx.context(),
                fake_tool("lookup"),
                Map::new(),
                output,
                None,
            )
            .await,
        );
        errs.add(
            (p.on_tool_error_callback().unwrap())(
                m_ctx.context(),
                fake_tool("db"),
                input,
                tool_err(&tt.text),
            )
            .await,
        );
        let mut ev = Event {
            id: "event-1".into(),
            author: "test-agent".into(),
            branch: tt.text.clone(),
            ..Event::default()
        };
        ev.response.content = Some(text_content("model", &tt.text));
        errs.add((p.on_event_callback().unwrap())(m_ctx.invocation(), ev).await);
        errs.check();
        (p.after_run_callback().unwrap())(m_ctx.invocation()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let want_content = if tt.want_content.is_empty() {
            tt.want
        } else {
            tt.want_content
        };
        let want_response = if tt.want_response.is_empty() {
            format!("text: '{want_content}'")
        } else {
            tt.want_response.to_owned()
        };
        let want_v = json!(want_content);
        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let got = json_at(&user_row, "content", &["text_summary"]);
        assert_eq!(
            got.as_ref(),
            Some(&want_v),
            "{name}: USER_MESSAGE_RECEIVED content.text_summary = {got:?}, want {want_content:?}"
        );
        let parts = parts_of(&user_row);
        assert!(
            one_part_with(&parts, "text", want_content),
            "{name}: USER_MESSAGE_RECEIVED content_parts = {parts:?}, want text {want_content:?}"
        );
        let llm_row = only_row_of_type(&rows, EVENT_LLM_REQUEST);
        let prompt = json_at(&llm_row, "content", &["prompt"]);
        let prompt_ok = prompt
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|p| p.len() == 1 && p[0].get("content") == Some(&want_v));
        assert!(
            prompt_ok,
            "{name}: LLM_REQUEST content.prompt = {prompt:?}, want content {want_content:?}"
        );
        let parts = parts_of(&llm_row);
        assert!(
            one_part_with(&parts, "text", want_content),
            "{name}: LLM_REQUEST content_parts = {parts:?}, want text {want_content:?}"
        );
        let start_row = only_row_of_type(&rows, EVENT_TOOL_STARTING);
        let got = json_at(&start_row, "content", &["args", "input"]);
        assert_eq!(
            got.as_ref(),
            Some(&want_v),
            "{name}: TOOL_STARTING content.args.input = {got:?}, want {want_content:?}"
        );
        let tool_row = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
        let got = json_at(&tool_row, "content", &["result", "output"]);
        assert_eq!(
            got.as_ref(),
            Some(&want_v),
            "{name}: TOOL_COMPLETED content.result.output = {got:?}, want {want_content:?}"
        );
        let err_row = only_row_of_type(&rows, EVENT_TOOL_ERROR);
        let got = err_row.get("error_message");
        assert_eq!(
            got,
            Some(&RowValue::Str(tt.want.to_owned())),
            "{name}: TOOL_ERROR error_message = {got:?}, want {:?}",
            tt.want
        );
        let got = json_at(&err_row, "content", &["args", "input"]);
        assert_eq!(
            got.as_ref(),
            Some(&want_v),
            "{name}: TOOL_ERROR content.args.input = {got:?}, want {want_content:?}"
        );
        let ev_row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
        let got = json_at(&ev_row, "content", &["response"]);
        assert_eq!(
            got.as_ref(),
            Some(&json!(want_response)),
            "{name}: AGENT_RESPONSE content.response = {got:?}, want {want_response:?}"
        );
        let parts = parts_of(&ev_row);
        assert!(
            one_part_with(&parts, "text", want_content),
            "{name}: AGENT_RESPONSE content_parts = {parts:?}, want text {want_content:?}"
        );
        let got = json_at(&ev_row, "attributes", &["adk", "branch"]);
        assert_eq!(
            got.as_ref(),
            Some(&want_v),
            "{name}: AGENT_RESPONSE attributes.adk.branch = {got:?}, want {want_content:?}"
        );
        for row in &rows {
            let s = format!("{row:?}");
            for secret in secrets {
                assert!(
                    !s.contains(secret),
                    "{name}: {:?} row leaked {secret}: {s}",
                    row.get("event_type")
                );
            }
        }
    }
}

// TestFileDataURICredentialsAreRedacted checks that credentials in the path, a nested URL, a
// query value or the fragment of a FileData URI do not reach content_parts.uri.
#[tokio::test]
async fn test_file_data_uri_credentials_are_redacted() {
    let uris = [
        (
            "https://example.com/app#/cb?access_token=SECRET123",
            "https://example.com/app#/cb?access_token=%5BREDACTED%5D",
        ),
        (
            "https://example.com/login?next=https://api.example.com/cb?access_token=SECRET123",
            "https://example.com/login?next=https%3A%2F%2Fapi.example.com%2Fcb%3Faccess_token%3D%5BREDACTED%5D",
        ),
        (
            "https://example.com/token:SECRET123/file.png",
            "https://example.com/token:%5BREDACTED%5D/file.png",
        ),
        (
            "https://example.com/a?a=1;access_token=SECRET123",
            "https://example.com/a?a=1%3Baccess_token%3D%5BREDACTED%5D",
        ),
    ];
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let mut content = Content {
        role: "user".into(),
        parts: Vec::new(),
    };
    for (uri, _) in &uris {
        content.parts.push(Part {
            file_data: Some(FileData {
                mime_type: "image/png".into(),
                file_uri: (*uri).into(),
                ..FileData::default()
            }),
            ..Part::default()
        });
    }
    let mut errs = CallbackErrors::default();
    errs.add((p.on_user_message_callback().unwrap())(tp.ctx.invocation(), content).await);
    errs.check();
    (p.after_run_callback().unwrap())(tp.ctx.invocation()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
    let parts = parts_of(&user_row);
    assert_eq!(
        parts.len(),
        uris.len(),
        "content_parts = {parts:?}, want {} parts",
        uris.len()
    );
    for (i, (_, want)) in uris.iter().enumerate() {
        let got = parts[i].get("uri");
        assert_eq!(
            got,
            Some(&RowValue::Str((*want).into())),
            "content_parts[{i}].uri = {got:?}, want {want:?}"
        );
    }
    let got = user_row.get("is_truncated");
    assert_eq!(
        got,
        Some(&RowValue::Bool(true)),
        "is_truncated = {got:?}, want true"
    );
}

// TestTextWithoutCredentialsIsUnchanged checks that text that mentions an authorization scheme
// or a sensitive word, but carries no credential, is written unchanged and is not marked as
// truncated.
#[tokio::test]
async fn test_text_without_credentials_is_unchanged() {
    let texts = [
        "Write a basic HTTP server in Go",
        "I'm the bearer of bad news",
        "basic auth not configured",
        "[Basic usage](https://example.com/docs)",
        r#"[Q] What does "token" mean?"#,
        "See https://example.com/search?q=basic%20recipes for ideas",
        r#""Hello," she said [laughing]. "Bye.""#,
        r#""What does [this] mean?""#,
    ];
    for text in texts {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let mut errs = CallbackErrors::default();
        errs.add(
            (p.on_user_message_callback().unwrap())(
                tp.ctx.invocation(),
                text_content("user", text),
            )
            .await,
        );
        errs.add(
            (p.on_tool_error_callback().unwrap())(
                tp.ctx.context(),
                fake_tool("lookup"),
                Map::new(),
                tool_err(text),
            )
            .await,
        );
        errs.check();
        (p.after_run_callback().unwrap())(tp.ctx.invocation()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let got = json_at(&user_row, "content", &["text_summary"]);
        assert_eq!(
            got,
            Some(json!(text)),
            "{text}: USER_MESSAGE_RECEIVED content.text_summary = {got:?}, want {text:?}"
        );
        let parts = parts_of(&user_row);
        assert!(
            one_part_with(&parts, "text", text),
            "{text}: USER_MESSAGE_RECEIVED content_parts = {parts:?}, want text {text:?}"
        );
        let got = user_row.get("is_truncated");
        assert_eq!(
            got,
            Some(&RowValue::Bool(false)),
            "{text}: USER_MESSAGE_RECEIVED is_truncated = {got:?}, want false"
        );
        let got = only_row_of_type(&rows, EVENT_TOOL_ERROR)
            .get("error_message")
            .cloned();
        assert_eq!(
            got,
            Some(RowValue::Str(text.into())),
            "{text}: TOOL_ERROR error_message = {got:?}, want {text:?}"
        );
    }

    // file URI
    {
        const URI: &str = "https://example.com/search?q=basic%20recipes";
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let mut errs = CallbackErrors::default();
        errs.add(
            (p.on_user_message_callback().unwrap())(
                tp.ctx.invocation(),
                Content {
                    role: "user".into(),
                    parts: vec![Part {
                        file_data: Some(FileData {
                            mime_type: "text/html".into(),
                            file_uri: URI.into(),
                            ..FileData::default()
                        }),
                        ..Part::default()
                    }],
                },
            )
            .await,
        );
        errs.check();
        (p.after_run_callback().unwrap())(tp.ctx.invocation()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let parts = parts_of(&user_row);
        assert!(
            one_part_with(&parts, "uri", URI),
            "content_parts = {parts:?}, want uri {URI:?}"
        );
        let got = user_row.get("is_truncated");
        assert_eq!(
            got,
            Some(&RowValue::Bool(false)),
            "is_truncated = {got:?}, want false"
        );
    }

    // large user message with pattern words and backslashes
    for (name, text, prefix) in [(
        "52KB prose",
        format!(
            "{}{} end",
            "design review notes; ".repeat(2_500),
            r"\".repeat(256)
        ),
        "design review notes; ",
    )] {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let start = Instant::now();
        let mut errs = CallbackErrors::default();
        errs.add(
            (p.on_user_message_callback().unwrap())(
                tp.ctx.invocation(),
                text_content("user", &text),
            )
            .await,
        );
        errs.check();
        let d = start.elapsed();
        assert!(
            d <= Duration::from_secs(3),
            "{name}: OnUserMessageCallback took {d:?} for a {}-byte text",
            text.len()
        );
        (p.after_run_callback().unwrap())(tp.ctx.invocation()).await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let user_row = only_row_of_type(&rows, EVENT_USER_MESSAGE_RECEIVED);
        let got = json_at(&user_row, "content", &["text_summary"]);
        let s = got.as_ref().and_then(Value::as_str).unwrap_or_default();
        assert!(
            s.starts_with(prefix),
            "{name}: USER_MESSAGE_RECEIVED content.text_summary = {s:?}, want prefix {prefix:?}"
        );
    }
}

// parity: adapted: Go's mockInvocationContext has no activation; each mCtx is one gadk invocation context, and its tool callbacks run in contexts derived from it (one activation per mCtx, as Go's nil activation key gives one call-key scope)
#[tokio::test]
async fn test_tool_error_deduplication_recovered_missing_and_scoped() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let mut m_ctx1 = tp.ctx.clone();
    m_ctx1.session_id = "sess-1".into();
    m_ctx1.invocation_id = "inv-1".into();
    m_ctx1.function_call_id = "call-shared".into();

    let mut m_ctx2 = MockCtx {
        cancel: m_ctx1.cancel.clone(),
        agent_name: "test-agent".into(),
        session_id: "sess-2".into(),
        invocation_id: "inv-2".into(),
        user_id: "user-2".into(),
        function_call_id: "call-shared".into(),
        ..MockCtx::default()
    };

    // One invocation context per Go mockInvocationContext: Go's mock carries no activation, and
    // every gadk context built anew is an activation of its own (which scopes the call keys).
    let ictx1 = MockCtx {
        function_call_id: String::new(),
        ..m_ctx1.clone()
    }
    .invocation();
    let ictx2 = MockCtx {
        function_call_id: String::new(),
        ..m_ctx2.clone()
    }
    .invocation();
    let tool_ctx = |ictx: &InvocationContext, m: &MockCtx| {
        ictx.clone().into_context().with_delta(ContextDelta {
            function_call_id: Some(m.function_call_id.clone()),
            ..ContextDelta::default()
        })
    };

    let lookup_tool = fake_tool("lookup");
    let mut args = Map::new();
    args.insert("q".into(), json!("weather"));
    let tool_e = tool_err("lookup failed");
    let on_tool_error = p.on_tool_error_callback().unwrap();
    let after_tool = p.after_tool_callback().unwrap();

    let mut errs = CallbackErrors::default();
    // 1. Interleaved tool errors across two invocations with the same callID ("call-shared"):
    // Each invocation must log exactly 1 TOOL_ERROR (2 total), without deleting the other's
    // marker.
    errs.add(
        on_tool_error(
            tool_ctx(&ictx1, &m_ctx1),
            Arc::clone(&lookup_tool),
            args.clone(),
            Arc::clone(&tool_e),
        )
        .await,
    );
    errs.add(
        on_tool_error(
            tool_ctx(&ictx2, &m_ctx2),
            Arc::clone(&lookup_tool),
            args.clone(),
            Arc::clone(&tool_e),
        )
        .await,
    );
    errs.add(
        after_tool(
            tool_ctx(&ictx1, &m_ctx1),
            Arc::clone(&lookup_tool),
            args.clone(),
            Map::new(),
            Some(Arc::clone(&tool_e)),
        )
        .await,
    );
    errs.add(
        after_tool(
            tool_ctx(&ictx2, &m_ctx2),
            Arc::clone(&lookup_tool),
            args.clone(),
            Map::new(),
            Some(Arc::clone(&tool_e)),
        )
        .await,
    );

    // 2. Recovered tool error on mCtx1 (OnToolError followed by AfterTool with err == nil),
    // followed by a fallback-only AfterTool error on the same tool with empty callID:
    // The recovered marker must be consumed so the subsequent fallback error is NOT suppressed.
    m_ctx1.function_call_id = String::new();
    errs.add(
        on_tool_error(
            tool_ctx(&ictx1, &m_ctx1),
            Arc::clone(&lookup_tool),
            args.clone(),
            tool_err("transient error"),
        )
        .await,
    );
    let mut ok = Map::new();
    ok.insert("ok".into(), json!(true));
    errs.add(
        after_tool(
            tool_ctx(&ictx1, &m_ctx1),
            Arc::clone(&lookup_tool),
            args.clone(),
            ok,
            None,
        )
        .await,
    );
    errs.add(
        after_tool(
            tool_ctx(&ictx1, &m_ctx1),
            Arc::clone(&lookup_tool),
            args.clone(),
            Map::new(),
            Some(tool_err("fallback error")),
        )
        .await,
    );

    // 3. Missing-tool error on mCtx2 (OnToolError without AfterTool) before AfterRunCallback:
    // AfterRunCallback must clean up the leftover marker for mCtx2 so a later run with the same
    // IDs can log fallback errors.
    m_ctx2.function_call_id = String::new();
    errs.add(
        on_tool_error(
            tool_ctx(&ictx2, &m_ctx2),
            fake_tool("missing"),
            args.clone(),
            tool_err("not found"),
        )
        .await,
    );
    errs.check();
    (p.after_run_callback().unwrap())(ictx2.clone()).await;

    // Now a fallback-only AfterTool error on "missing" in mCtx2 must still be logged!
    errs.add(
        after_tool(
            tool_ctx(&ictx2, &m_ctx2),
            fake_tool("missing"),
            args.clone(),
            Map::new(),
            Some(tool_err("fallback after run cleanup")),
        )
        .await,
    );
    errs.check();
    (p.after_run_callback().unwrap())(ictx1.clone()).await;

    let rows = tp
        .collect_rows(|rows| rows_of_type(rows, EVENT_INVOCATION_COMPLETED).len() >= 2)
        .await;
    let tool_errors = rows_of_type(&rows, EVENT_TOOL_ERROR);
    assert_eq!(
        tool_errors.len(),
        6,
        "TOOL_ERROR count = {}, want 6 (got types: {:?})",
        tool_errors.len(),
        event_types(&rows)
    );
}

// parity: adapted: Go's bareInvocationContext (agent and session only, no AgentName/SessionID accessors) → gadk has one InvocationContext type, whose identity comes from its agent and session; an invocation context over a custom agent and a session with Go's fields
#[test]
fn test_identity_of_bare_invocation_context() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let _guard = rt.enter();
    let m = MockCtx {
        agent_name: "bare-agent".into(),
        session_id: "sess-bare".into(),
        user_id: "user-bare".into(),
        app_name: "app-bare".into(),
        invocation_id: "inv-bare".into(),
        branch: "root.bare".into(),
        ..MockCtx::default()
    };
    let got: Identity = identity_of(&CallbackCtx::from_invocation(&m.invocation()));
    assert!(
        got.agent_name == "bare-agent"
            && got.session_id == "sess-bare"
            && got.invocation_id == "inv-bare"
            && got.user_id == "user-bare"
            && got.app_name == "app-bare"
            && got.branch == "root.bare",
        "identityOf(bareInvocationContext) = {got:?}"
    );
}

// parity: adapted: Go reads the linked ADK module's version from debug.BuildInfo (adkVersionFromBuildInfo); gadk has no build info and writes its own crate version (ledgered divergence, plugin/version.rs) — the version is the tag without its "v", as Go's tagged cases
#[test]
fn test_adk_version_from_build_info() {
    assert_eq!(ADK_MODULE_PATH, "google.golang.org/adk/v2");
    let got = adk_version();
    assert_eq!(got, env!("CARGO_PKG_VERSION"), "adkVersion() = {got:?}");
    assert!(
        !got.is_empty() && !got.starts_with('v') && got != "(devel)",
        "adkVersion() = {got:?}, want a version without the leading v"
    );
}

#[test]
fn test_cache_type() {
    let tests: [(&str, Option<GenerateContentResponseUsageMetadata>, &str); 3] = [
        ("no usage", None, "none"),
        (
            "no cached tokens",
            Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                ..GenerateContentResponseUsageMetadata::default()
            }),
            "none",
        ),
        (
            "cached tokens",
            Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                cached_content_token_count: 1,
                ..GenerateContentResponseUsageMetadata::default()
            }),
            "implicit",
        ),
    ];
    for (name, usage, want) in tests {
        let got = cache_type(usage.as_ref());
        assert_eq!(
            got, want,
            "{name}: cacheType({usage:?}) = {got:?}, want {want:?}"
        );
    }
}

// parity: adapted: Go's nil plugin and nil *BigQueryAgentAnalyticsPlugin receiver → a plugin without the analytics state (Rust has no nil receiver); Go's &BatchProcessor{counters: counters} → a never-started processor whose counters are replaced by the plugin's
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_big_query_agent_analytics_plugin_drop_stats() {
    assert!(
        as_bigquery_agent_analytics_plugin(&Plugin::new(PluginConfig::default())).is_none(),
        "AsBigQueryAgentAnalyticsPlugin(nil) = true, want false"
    );

    let formatter: ContentFormatter = Arc::new(|_v: &Value, event_type: &str| {
        if event_type == EVENT_USER_MESSAGE_RECEIVED {
            return Err("formatter error".into());
        }
        Ok(json!("ok"))
    });
    let tp = new_test_plugin_with_config(|c| c.content_formatter = Some(formatter)).await;
    let p = &tp.plugin;

    let bqaap =
        as_bigquery_agent_analytics_plugin(p).expect("AsBigQueryAgentAnalyticsPlugin(p) failed");
    assert_eq!(
        bqaap.drop_stats(),
        DropStats::default(),
        "initial DropStats() want zero DropStats"
    );

    (p.on_user_message_callback().unwrap())(tp.ctx.invocation(), text_content("user", "secret"))
        .await
        .expect("OnUserMessageCallback error");
    (p.after_run_callback().unwrap())(tp.ctx.invocation()).await;
    let _ = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;

    let want = DropStats {
        formatter_failure_drops: 1,
        total_drops: 0,
        ..DropStats::default()
    };
    assert_eq!(
        bqaap.drop_stats(),
        want,
        "BigQueryAgentAnalyticsPlugin.DropStats() mismatch"
    );

    let counters = bqaap.state.counters.clone().expect("drop counters");
    let processor_with = |config: Config| {
        let counters = Arc::clone(&counters);
        async move {
            let mut bp = queue_processor(&config).await;
            Arc::get_mut(&mut bp)
                .expect("an unshared processor")
                .counters = counters;
            bp
        }
    };
    // Verify a recovered panic inside logEvent increments AppendFailureDrops and TotalDrops
    // once.
    let config = Config::default();
    let ap = AnalyticsPlugin {
        processor: Some(processor_with(config.clone()).await),
        config,
        ..AnalyticsPlugin::default()
    };
    ap.log_event(
        &CallbackCtx::background(),
        &Identity::default(),
        EVENT_USER_MESSAGE_RECEIVED,
        None,
        Some(&mut |_: &mut EventData| panic!("boom in callback")),
    );
    let want_after_panic = DropStats {
        append_failure_drops: 1,
        formatter_failure_drops: 1,
        total_drops: 1,
        ..DropStats::default()
    };
    assert_eq!(
        bqaap.drop_stats(),
        want_after_panic,
        "DropStats() after recovered panic in logEvent mismatch"
    );

    // Verify a recovered panic inside logSessionEvent also increments AppendFailureDrops and
    // TotalDrops once.
    ap.log_session_event(
        &CallbackCtx::background(),
        &Identity::default(),
        EVENT_STATE_DELTA,
        RowSpan::default(),
        Some(&mut |_: &mut EventData| panic!("boom in logSessionEvent")),
    );
    let want_after_session_panic = DropStats {
        append_failure_drops: 2,
        formatter_failure_drops: 1,
        total_drops: 2,
        ..DropStats::default()
    };
    assert_eq!(
        bqaap.drop_stats(),
        want_after_session_panic,
        "DropStats() after recovered panic in logSessionEvent mismatch"
    );

    // Verify a disabled analyticsPlugin does not increment AppendFailureDrops on panic.
    let disabled_cfg = Config {
        enabled: false,
        ..Config::default()
    };
    let disabled_ap = AnalyticsPlugin {
        processor: Some(processor_with(disabled_cfg.clone()).await),
        config: disabled_cfg,
        ..AnalyticsPlugin::default()
    };
    disabled_ap.log_event(
        &CallbackCtx::background(),
        &Identity::default(),
        EVENT_USER_MESSAGE_RECEIVED,
        None,
        Some(&mut |_: &mut EventData| panic!("boom in disabled plugin")),
    );
    assert_eq!(
        bqaap.drop_stats(),
        want_after_session_panic,
        "DropStats() after panic in disabled plugin mismatch"
    );
}

/// Go's `newWriteClient(t)` / `newTestWriteClient(t)`: a write client of a new fake server.
fn new_write_client() -> (FakeWriteServer, WriteClient) {
    let server = FakeWriteServer::new();
    let wc = server.client().expect("NewBigQueryWriteClient");
    (server, wc)
}

// parity: adapted: Go's gRPC write server on a TCP listener → the in-process FakeWriteServer; Go's mockTransport HTTP client → a ScriptedClient
#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_with_clients_closes_write_client_on_error() {
    let ctx = Context::background();

    // rejects nil writeClient when plugin is enabled
    {
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            ..Config::default()
        };
        let err = new_bigquery_agent_analytics_plugin_with_clients(&ctx, cfg, None, None)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            err.contains("writeClient must not be nil"),
            "NewBigQueryAgentAnalyticsPluginWithClients(writeClient=nil) error = {err:?}, want error containing \"writeClient must not be nil\""
        );
    }

    // succeeds with bqClient == nil for pre-provisioned append-only deployments
    {
        let (_server, wc) = new_write_client();
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            ..Config::default()
        };
        let p = new_bigquery_agent_analytics_plugin_with_clients(&ctx, cfg, None, Some(wc))
            .await
            .expect("NewBigQueryAgentAnalyticsPluginWithClients(bqClient=nil) error, want nil");
        (p.close().unwrap())().await.expect("p.Close() error");
    }

    // closes writeClient when table metadata returns non-retryable HTTP 400
    {
        let http = mock_transport(Some(Box::new(|_| {
            FakeResponse::json_status(400, r#"{"error":{"code":400,"message":"bad request"}}"#)
        })));
        let bq = bq_client_with("test-project", http);
        let (_server, wc) = new_write_client();
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            ..Config::default()
        };
        assert!(
            new_bigquery_agent_analytics_plugin_with_clients(&ctx, cfg, Some(bq), Some(wc.clone()))
                .await
                .is_err(),
            "NewBigQueryAgentAnalyticsPluginWithClients() error = nil, want HTTP 400 error"
        );
        // Verify writeClient was closed by attempting to open an AppendRows stream on it.
        assert!(
            wc.append_rows(&ctx).await.is_err(),
            "writeClient remained open after constructor error (resource leak)"
        );
    }

    // closes writeClient when validateConfig fails
    {
        let (_server, wc) = new_write_client();
        let cfg = Config {
            payload_column_denylist: vec!["not_a_valid_column".into()],
            ..Config::default()
        };
        assert!(
            new_bigquery_agent_analytics_plugin_with_clients(&ctx, cfg, None, Some(wc.clone()))
                .await
                .is_err(),
            "NewBigQueryAgentAnalyticsPluginWithClients() error = nil, want validation error"
        );
        assert!(
            wc.append_rows(&ctx).await.is_err(),
            "writeClient remained open after validateConfig error (resource leak)"
        );
    }
}

// parity: adapted: Go's gRPC committedStreamTestServer → the in-process FakeWriteServer with the batch-processor support's committed_stream_server; Go's mockTransport → a ScriptedClient (its requests replace the closure's captured PATCH fields)
#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_with_clients_upgrades_existing_outdated_table_via_http()
 {
    let ctx = Context::background();

    let http = mock_transport(Some(Box::new(|r: &RecordedRequest| {
        if r.method == "GET" && r.path.contains("/datasets/test_dataset/tables/test-table") {
            return FakeResponse::json(
                r#"{
                    "etag": "etag-http-1",
                    "schema": {
                        "fields": [
                            {"name": "timestamp", "type": "TIMESTAMP", "mode": "REQUIRED"}
                        ]
                    }
                }"#,
            )
            .header("Etag", "etag-http-1");
        }
        if r.method == "PATCH" && r.path.contains("/datasets/test_dataset/tables/test-table") {
            return FakeResponse::json(r#"{"etag":"etag-http-2"}"#);
        }
        if r.path.contains("/jobs") {
            return FakeResponse::json(
                r#"{"jobReference":{"projectId":"test-project","jobId":"test-job"},"status":{"state":"DONE"}}"#,
            );
        }
        FakeResponse::json("{}")
    })));
    let bq = bq_client_with("test-project", Arc::clone(&http));

    let server = FakeWriteServer::new();
    let created_stream_name =
        "projects/test-project/datasets/test_dataset/tables/test-table/streams/committed-http-1";
    let _state = committed_stream_server(
        &server,
        CommittedState {
            created_stream_name: created_stream_name.into(),
            ..CommittedState::default()
        },
    );
    let write_client = server.client().expect("NewBigQueryWriteClient");

    let cfg = Config {
        project_id: "test-project".into(),
        dataset_id: "test_dataset".into(),
        table_name: "test-table".into(),
        exactly_once_delivery: true,
        ..Config::default()
    };
    let p =
        new_bigquery_agent_analytics_plugin_with_clients(&ctx, cfg, Some(bq), Some(write_client))
            .await
            .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");

    let patches: Vec<RecordedRequest> = http
        .requests()
        .into_iter()
        .filter(|r| {
            r.method == "PATCH" && r.path.contains("/datasets/test_dataset/tables/test-table")
        })
        .collect();
    assert_eq!(patches.len(), 1, "PATCH calls = {}, want 1", patches.len());
    let patch_if_match = patches[0].header("If-Match");
    assert_eq!(
        patch_if_match, "etag-http-1",
        "If-Match header = {patch_if_match:?}, want \"etag-http-1\""
    );

    let parsed: Value =
        serde_json::from_slice(&patches[0].body).expect("json.Unmarshal(patchBody) error");
    let got = lookup(&parsed, &["labels", SCHEMA_VERSION_KEY]).cloned();
    assert_eq!(
        got,
        Some(json!(SCHEMA_VERSION)),
        "patched label {SCHEMA_VERSION_KEY:?} = {got:?}, want {SCHEMA_VERSION:?}"
    );
    let fields = lookup(&parsed, &["schema", "fields"])
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(
        fields,
        events_schema().len(),
        "patched field count = {fields}, want {}",
        events_schema().len()
    );

    (p.close().unwrap())().await.expect("p.Close() error");

    let creates = server.create_calls();
    assert_eq!(
        creates.len(),
        1,
        "CreateWriteStream calls = {}, want 1",
        creates.len()
    );
    let got = creates[0]
        .request
        .write_stream
        .as_ref()
        .map_or(0, |w| w.r#type);
    assert_eq!(
        got,
        crate::storagepb::write_stream::Type::Committed as i32,
        "CreateWriteStream type = {got}, want COMMITTED"
    );
    let finals: Vec<String> = server
        .finalize_calls()
        .into_iter()
        .map(|c| c.request.name)
        .collect();
    assert_eq!(
        finals,
        vec![created_stream_name.to_owned()],
        "FinalizeWriteStream reqs = {finals:?}, want [{created_stream_name:?}]"
    );
}

// ---------------------------------------------------------------------------------------------
// TestBigQueryAgentAnalyticsPlugin_PluginRegistryCleanupAndDisabledConstructors
// ---------------------------------------------------------------------------------------------

/// Go `pluginHolder`: an application object that holds a plugin and is also handed to it: as
/// its Logger, as the receiver of its ContentFormatter, or in the transport of its BigQuery
/// client. It refers to the plugin weakly (a Rust `Arc` back-reference would be a reference
/// cycle, which no collector breaks).
#[derive(Default)]
struct PluginHolder {
    plugin: parking_lot::Mutex<Weak<Plugin>>,
}

impl Logger for PluginHolder {
    fn printf(&self, _: fmt::Arguments<'_>) {}
    fn println(&self, _: fmt::Arguments<'_>) {}
}

impl PluginHolder {
    /// Go `(*pluginHolder).format`.
    fn format(&self, payload: &Value) -> Value {
        payload.clone()
    }

    /// Go `(*pluginHolder).roundTrip`.
    fn round_trip(&self) -> FakeResponse {
        FakeResponse::json("{}")
    }
}

/// Go's `mutate func(*Config)`.
type Mutate = Box<dyn FnOnce(&mut Config)>;

/// Go `newHolderTestPlugin(t, ctx, transport, mutate)`: an enabled plugin whose views are
/// created in the background through a BigQuery client over `transport`, writing rows to a fake
/// write server. (Go's returned close function closes the clients the caller owns; here the
/// caller drops them.)
async fn new_holder_test_plugin(
    ctx: &Context,
    transport: Arc<ScriptedClient>,
    mutate: Option<Mutate>,
) -> (Plugin, Arc<BigQueryClient>, FakeWriteServer) {
    let bq = bq_client_with("test-project", transport);
    let server = FakeWriteServer::new();
    let write_client = server.client().expect("NewBigQueryWriteClient");
    let mut cfg = Config {
        enabled: true,
        project_id: "test-project".into(),
        dataset_id: "test_dataset".into(),
        table_name: "test-table".into(),
        ..Config::default()
    };
    if let Some(mutate) = mutate {
        mutate(&mut cfg);
    }
    let p = new_bigquery_agent_analytics_plugin_with_clients(
        ctx,
        cfg,
        Some(Arc::clone(&bq)),
        Some(write_client),
    )
    .await
    .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
    (p, bq, server)
}

/// Go `waitPluginCollected(key)`: waits (five seconds at most) until every `Weak` is dead, i.e.
/// the plugin's state slot (Go's registry entry) and what the plugin held are freed.
async fn wait_collected(weaks: &[&dyn Fn() -> bool]) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if weaks.iter().all(|dead| dead()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// The plugin's state slot (Go's `pluginRegistry` entry), weakly.
fn registry_entry(p: &Plugin) -> Weak<PluginState> {
    Arc::downgrade(
        &p.state::<PluginState>()
            .expect("pluginRegistry missing entry"),
    )
}

// parity: adapted: Go's weak-pointer pluginRegistry, emptied by the GC → the plugin's state slot (Plugin::state), dropped with the plugin: "collected" = the state slot's Weak is dead once the test drops the plugin; Rust has no cycle collector, so the pluginHolder refers to the plugin through a Weak and the case checks that the holder (the plugin's Logger / formatter / transport) is freed too; gadk's Context carries no values, so the constructor-context back-reference case puts the holder in the hanging job transport
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
async fn test_big_query_agent_analytics_plugin_plugin_registry_cleanup_and_disabled_constructors() {
    let ctx = Context::background();

    // enabled plugin remains reachable via AsBigQueryAgentAnalyticsPlugin after Close
    {
        let tp = new_test_plugin_with_config(|c| c.queue_max_size = 1).await;
        let p = &tp.plugin;
        assert!(
            as_bigquery_agent_analytics_plugin(p).is_some(),
            "AsBigQueryAgentAnalyticsPlugin() = false before Close, want true"
        );
        tp.close().await.expect("p.Close() error");
        // A caller holding only the Plugin can still call AsBigQueryAgentAnalyticsPlugin(p)
        // after Close().
        let bqaap = as_bigquery_agent_analytics_plugin(p).expect(
            "AsBigQueryAgentAnalyticsPlugin() = false after Close, want true while p is reachable",
        );
        let _ = (p.on_user_message_callback().unwrap())(
            tp.ctx.invocation(),
            text_content("user", "after_close"),
        )
        .await;
        let want = DropStats {
            shutdown_drops: 1,
            total_drops: 1,
            ..DropStats::default()
        };
        assert_eq!(bqaap.drop_stats(), want, "DropStats() after Close mismatch");
    }

    // disabled via NewBigQueryAgentAnalyticsPluginWithConfig remains reachable after Close
    {
        let p = new_bigquery_agent_analytics_plugin_with_config(
            &ctx,
            Config {
                enabled: false,
                ..Config::default()
            },
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithConfig() error");
        let bqaap = as_bigquery_agent_analytics_plugin(&p)
            .expect("AsBigQueryAgentAnalyticsPlugin() = false before Close, want true");
        assert_eq!(
            bqaap.drop_stats(),
            DropStats::default(),
            "DropStats() want zero"
        );
        (p.close().unwrap())().await.expect("p.Close() error");
        assert!(
            as_bigquery_agent_analytics_plugin(&p).is_some(),
            "AsBigQueryAgentAnalyticsPlugin() = false after Close, want true while p is reachable"
        );
    }

    // disabled via NewBigQueryAgentAnalyticsPluginWithClients remains reachable after Close
    {
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &ctx,
            Config {
                enabled: false,
                ..Config::default()
            },
            None,
            None,
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
        assert!(
            as_bigquery_agent_analytics_plugin(&p).is_some(),
            "AsBigQueryAgentAnalyticsPlugin() = false before Close, want true"
        );
        (p.close().unwrap())().await.expect("p.Close() error");
        assert!(
            as_bigquery_agent_analytics_plugin(&p).is_some(),
            "AsBigQueryAgentAnalyticsPlugin() = false after Close, want true while p is reachable"
        );
    }

    // unreachable plugin entry is removed from pluginRegistry after GC
    for (name, want_stats) in [
        ("disabled plugin", false),
        ("enabled plugin after Close", true),
    ] {
        let key = {
            let p = if want_stats {
                let (_server, write_client) = new_write_client();
                let cfg = Config {
                    project_id: "test-project".into(),
                    dataset_id: "test_dataset".into(),
                    ..Config::default()
                };
                let p = new_bigquery_agent_analytics_plugin_with_clients(
                    &ctx,
                    cfg,
                    None,
                    Some(write_client),
                )
                .await
                .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
                (p.close().unwrap())().await.expect("p.Close() error");
                p
            } else {
                new_bigquery_agent_analytics_plugin_with_config(
                    &ctx,
                    Config {
                        enabled: false,
                        ..Config::default()
                    },
                )
                .await
                .expect("NewBigQueryAgentAnalyticsPluginWithConfig() error")
            };
            let key = registry_entry(&p);
            let state = key
                .upgrade()
                .expect("pluginRegistry missing entry before GC");
            let got_stats = state.counters.is_some();
            assert_eq!(
                got_stats, want_stats,
                "{name}: pluginRegistry entry holds drop counters = {got_stats}, want {want_stats}"
            );
            key
        };
        assert!(
            wait_collected(&[&|| key.upgrade().is_none()]).await,
            "{name}: pluginRegistry still contained entry for unreachable plugin after GC"
        );
    }

    // closed disabled plugins are garbage collected and cleaned from pluginRegistry
    {
        const COUNT: usize = 16;
        let mut weak_keys = Vec::with_capacity(COUNT);
        for _ in 0..COUNT {
            let p = new_bigquery_agent_analytics_plugin_with_config(
                &ctx,
                Config {
                    enabled: false,
                    ..Config::default()
                },
            )
            .await
            .expect("failed to create disabled plugin");
            (p.close().unwrap())()
                .await
                .expect("failed to close disabled plugin");
            weak_keys.push(registry_entry(&p));
        }
        let all_clean = || weak_keys.iter().all(|wk| wk.upgrade().is_none());
        wait_collected(&[&all_clean]).await;
        for (i, wk) in weak_keys.iter().enumerate() {
            assert!(
                wk.upgrade().is_none(),
                "pluginRegistry still retains key for instance {i} after GC"
            );
        }
    }

    // closed enabled plugins are garbage collected and cleaned from pluginRegistry
    {
        let (weak_key, weak_bq) = {
            let bq = bq_client_with(
                "test-proj",
                mock_transport(Some(Box::new(|_| FakeResponse::json("{}")))),
            );
            let server = FakeWriteServer::new();
            let write_client = server.client().expect("failed to create write client");
            let config = Config {
                enabled: true,
                project_id: "test-proj".into(),
                dataset_id: "test_dataset".into(),
                table_name: "test-table".into(),
                disable_schema_upgrade: true,
                disable_create_views: true,
                ..Config::default()
            };
            let p = new_bigquery_agent_analytics_plugin_with_clients(
                &ctx,
                config,
                Some(Arc::clone(&bq)),
                Some(write_client),
            )
            .await
            .expect("failed to create plugin");
            (p.close().unwrap())().await.expect("p.Close() error");
            (registry_entry(&p), Arc::downgrade(&bq))
        };
        let collected = wait_collected(&[&|| weak_key.upgrade().is_none(), &|| {
            weak_bq.upgrade().is_none()
        }])
        .await;
        assert!(
            collected,
            "expected closed plugin to be collected and not retained by pluginRegistry"
        );
    }

    // An application can hand the plugin an object that holds the plugin, such as a per-tenant
    // struct used as the Logger. The registry holds its values for as long as the plugin lives,
    // so a value that reached such an object would keep the plugin alive for the life of the
    // process. That holds for a plugin that is dropped without Close too, even though its
    // BatchProcessor task then keeps running.
    type NewPlugin = Box<
        dyn Fn(
            Arc<PluginHolder>,
        ) -> futures::future::LocalBoxFuture<
            'static,
            (Plugin, Arc<BigQueryClient>, FakeWriteServer),
        >,
    >;
    let cases: Vec<(&str, NewPlugin, bool)> = vec![
        (
            "closed enabled plugin with circular Logger back-reference is collected",
            Box::new(|h: Arc<PluginHolder>| {
                Box::pin(async move {
                    new_holder_test_plugin(
                        &Context::background(),
                        mock_transport(None),
                        Some(Box::new(move |c: &mut Config| {
                            c.logger = Some(h as Arc<dyn Logger>);
                        })),
                    )
                    .await
                })
            }),
            false,
        ),
        (
            "closed enabled plugin with ContentFormatter method receiver back-reference is collected",
            Box::new(|h: Arc<PluginHolder>| {
                Box::pin(async move {
                    new_holder_test_plugin(
                        &Context::background(),
                        mock_transport(None),
                        Some(Box::new(move |c: &mut Config| {
                            c.content_formatter =
                                Some(Arc::new(move |payload: &Value, _: &str| {
                                    Ok(h.format(payload))
                                }));
                        })),
                    )
                    .await
                })
            }),
            false,
        ),
        (
            "closed enabled plugin with constructor context back-reference is collected after Close cancels view creation",
            Box::new(|h: Arc<PluginHolder>| {
                Box::pin(async move {
                    let started = Arc::new(AtomicBool::new(false));
                    let job_started = Arc::new(tokio::sync::Notify::new());
                    let (s, js) = (Arc::clone(&started), Arc::clone(&job_started));
                    let transport = ScriptedClient::new(move |r| {
                        let _holder = &h;
                        if r.method == "POST" && r.path.contains("/jobs") {
                            if !s.swap(true, Ordering::SeqCst) {
                                js.notify_one();
                            }
                            return FakeResponse::never();
                        }
                        FakeResponse::json("{}")
                    });
                    let out = new_holder_test_plugin(&Context::background(), transport, None).await;
                    tokio::time::timeout(Duration::from_secs(3), job_started.notified())
                        .await
                        .expect("timed out waiting for startup view creation to send a job");
                    out
                })
            }),
            false,
        ),
        (
            "closed enabled plugin with BigQuery client transport back-reference is collected",
            Box::new(|h: Arc<PluginHolder>| {
                Box::pin(async move {
                    let transport = ScriptedClient::new(move |_| h.round_trip());
                    new_holder_test_plugin(&Context::background(), transport, None).await
                })
            }),
            false,
        ),
        (
            "unclosed enabled plugin with DisableCreateViews and BigQuery client transport back-reference is collected",
            Box::new(|h: Arc<PluginHolder>| {
                Box::pin(async move {
                    let transport = ScriptedClient::new(move |_| h.round_trip());
                    new_holder_test_plugin(
                        &Context::background(),
                        transport,
                        Some(Box::new(|c: &mut Config| c.disable_create_views = true)),
                    )
                    .await
                })
            }),
            true,
        ),
    ];
    for (name, new_plugin_fn, leave_open) in cases {
        let (key, holder) = {
            let h = Arc::new(PluginHolder::default());
            let (p, bq, server) = new_plugin_fn(Arc::clone(&h)).await;
            let p = Arc::new(p);
            *h.plugin.lock() = Arc::downgrade(&p);
            if !leave_open {
                (p.close().unwrap())().await.expect("Close() error");
            }
            // Go's closeOwned: the caller's BigQuery client and the write server.
            drop((bq, server));
            (registry_entry(&p), Arc::downgrade(&h))
        };
        let collected =
            wait_collected(&[&|| key.upgrade().is_none(), &|| holder.upgrade().is_none()]).await;
        assert!(
            collected,
            "{name}: after drop: plugin state freed = {}, holder freed = {}; want both",
            key.upgrade().is_none(),
            holder.upgrade().is_none()
        );
    }

    // The disabled case of Go's back-reference table: the disabled plugin keeps no Logger.
    {
        let h = Arc::new(PluginHolder::default());
        let (key, holder) = {
            let p = new_bigquery_agent_analytics_plugin_with_config(
                &ctx,
                Config {
                    enabled: false,
                    logger: Some(Arc::clone(&h) as Arc<dyn Logger>),
                    ..Config::default()
                },
            )
            .await
            .expect("NewBigQueryAgentAnalyticsPluginWithConfig() error");
            let p = Arc::new(p);
            *h.plugin.lock() = Arc::downgrade(&p);
            (p.close().unwrap())().await.expect("Close() error");
            (registry_entry(&p), Arc::downgrade(&h))
        };
        drop(h);
        let collected =
            wait_collected(&[&|| key.upgrade().is_none(), &|| holder.upgrade().is_none()]).await;
        assert!(
            collected,
            "closed disabled plugin with circular Logger back-reference is collected: after drop: plugin state freed = {}, holder freed = {}; want both",
            key.upgrade().is_none(),
            holder.upgrade().is_none()
        );
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_add_content_formatter_failure_when_content_denied_records_drop_for_content_parts() {
    let ctx = Context::background();
    let failing = |text: &'static str| -> ContentFormatter {
        Arc::new(move |_: &Value, _: &str| Err(text.into()))
    };

    // content denied but content_parts enabled records formatter failure drop and omits
    // content_parts
    {
        let cfg = Config {
            payload_column_denylist: vec![COLUMN_CONTENT.into()],
            content_formatter: Some(failing("boom")),
            ..Config::default()
        };
        let bp = BatchProcessor::new(&ctx, None, "test_stream", cfg.clone())
            .await
            .expect("NewBatchProcessor() error");
        let denied = validate_config(&cfg).expect("validateConfig() error");
        let p = AnalyticsPlugin {
            config: cfg,
            processor: Some(Arc::clone(&bp)),
            denied_columns: denied,
            ..AnalyticsPlugin::default()
        };

        let mut row = QueuedRow::new(Row::new());
        let mut ed = EventData {
            content: Some(Payload::Content(text_content("user", "hello"))),
            ..EventData::default()
        };
        let truncated = p.add_content(&mut row, EVENT_USER_MESSAGE_RECEIVED, &mut ed);
        assert!(
            !truncated,
            "addContent() truncated = true, want false when ContentFormatter fails"
        );
        let got = ed.attributes.get("formatter_failed");
        assert_eq!(
            got,
            Some(&json!(true)),
            "ed.attributes[\"formatter_failed\"] = {got:?}, want true"
        );
        assert!(
            !row.row.contains_key(COLUMN_CONTENT),
            "row[{COLUMN_CONTENT:?}] = {:?}, want omitted when columnContent is denied",
            row.row.get(COLUMN_CONTENT)
        );
        assert!(
            !row.row.contains_key(COLUMN_CONTENT_PARTS),
            "row[{COLUMN_CONTENT_PARTS:?}] = {:?}, want omitted when ContentFormatter fails",
            row.row.get(COLUMN_CONTENT_PARTS)
        );
        let want = DropStats {
            formatter_failure_drops: 1,
            total_drops: 0,
            ..DropStats::default()
        };
        assert_eq!(bp.drop_stats(), want, "DropStats() mismatch");
    }

    // both content and content_parts denied skips formatter and records zero drops
    {
        let cfg = Config {
            payload_column_denylist: vec![COLUMN_CONTENT.into(), COLUMN_CONTENT_PARTS.into()],
            content_formatter: Some(failing("boom")),
            ..Config::default()
        };
        let bp = BatchProcessor::new(&ctx, None, "test_stream", cfg.clone())
            .await
            .expect("NewBatchProcessor() error");
        let denied = validate_config(&cfg).expect("validateConfig() error");
        let p = AnalyticsPlugin {
            config: cfg,
            processor: Some(Arc::clone(&bp)),
            denied_columns: denied,
            ..AnalyticsPlugin::default()
        };

        let mut row = QueuedRow::new(Row::new());
        let mut ed = EventData {
            content: Some(Payload::Content(text_content("user", "hello"))),
            ..EventData::default()
        };
        assert!(
            !p.add_content(&mut row, EVENT_USER_MESSAGE_RECEIVED, &mut ed),
            "addContent() truncated = true, want false when both content and content_parts are denied"
        );
        assert_eq!(
            bp.drop_stats(),
            DropStats::default(),
            "DropStats() want zero drops when both content and content_parts are denied"
        );
    }

    // content denied and multimodal content off still runs formatter and records failure on
    // LLM_REQUEST
    {
        let cfg = Config {
            payload_column_denylist: vec![COLUMN_CONTENT.into()],
            disable_multi_modal_content: true,
            content_formatter: Some(failing("formatter boom")),
            ..Config::default()
        };
        let bp = BatchProcessor::new(&ctx, None, "test_stream", cfg.clone())
            .await
            .expect("NewBatchProcessor() error");
        let denied = validate_config(&cfg).expect("validateConfig() error");
        let p = AnalyticsPlugin {
            config: cfg,
            processor: Some(Arc::clone(&bp)),
            denied_columns: denied,
            ..AnalyticsPlugin::default()
        };

        let mut row = QueuedRow::new(Row::new());
        let mut ed = EventData {
            content: Some(Payload::LlmRequest(Box::new(LlmRequest {
                model: "gemini-2.5-flash".into(),
                ..LlmRequest::default()
            }))),
            ..EventData::default()
        };
        assert!(
            !p.add_content(&mut row, EVENT_LLM_REQUEST, &mut ed),
            "addContent() truncated = true, want false when ContentFormatter fails"
        );
        let got = ed.attributes.get("formatter_failed");
        assert_eq!(
            got,
            Some(&json!(true)),
            "ed.attributes[\"formatter_failed\"] = {got:?}, want true"
        );
        assert!(
            ed.err_msg.contains("content_formatter raised"),
            "ed.errMsg = {:?}, want formatter failure note",
            ed.err_msg
        );
        let want = DropStats {
            formatter_failure_drops: 1,
            total_drops: 0,
            ..DropStats::default()
        };
        assert_eq!(bp.drop_stats(), want, "DropStats() mismatch");
    }
}

// parity: adapted: gadk's REST BigQuery client has no Close (closeBQClient is not a seam): "closed" = the plugin released its reference (Arc strong count back to the caller's) and a caller's client stays usable; Go's missing credentials file (a write-client error) → an endpoint the write client rejects (gadk reads credentials files only from the process environment)
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_new_big_query_agent_analytics_plugin_with_config_bq_client_lifecycle() {
    let ctx = Context::background();
    let ok_http_client = || {
        mock_transport(Some(Box::new(|r: &RecordedRequest| {
            if r.method == "GET" && r.path.contains("/tables/") {
                return FakeResponse::json(
                    r#"{"type":"TABLE","labels":{"adk_schema_version":"2"}}"#,
                );
            }
            FakeResponse::json("{}")
        })))
    };

    // closes internal bqClient when writeClient creation fails
    {
        let logger = Arc::new(RecordingLogger::default());
        let cfg = Config {
            project_id: "test-project".into(),
            disable_create_views: true,
            logger: Some(Arc::clone(&logger) as Arc<dyn Logger>),
            client_options: ClientOptions {
                http_client: Some(ok_http_client()),
                without_authentication: true,
                endpoint: Some("http://bad host".into()),
                ..ClientOptions::default()
            },
            ..Config::default()
        };
        let err = new_bigquery_agent_analytics_plugin_with_config(&ctx, cfg)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            err.contains("failed to create BigQuery write client"),
            "NewBigQueryAgentAnalyticsPluginWithConfig() error = {err:?}, want writeClient error"
        );
        assert!(
            !logger.contains("failed to close bigquery client"),
            "the REST BigQuery client has nothing to close, got:\n{logger}"
        );
    }

    // closes internal bqClient when table verification fails
    {
        let fail_http_client = mock_transport(Some(Box::new(|_| {
            FakeResponse::json_status(400, r#"{"error":{"code":400,"message":"bad request"}}"#)
        })));
        let bq = bq_client_with("test-project", fail_http_client);
        let (_server, write_client) = new_write_client();
        let cfg = Config {
            project_id: "test-project".into(),
            disable_create_views: true,
            ..Config::default()
        };
        assert!(
            new_plugin(&ctx, cfg, Some(Arc::clone(&bq)), Some(write_client), true)
                .await
                .is_err(),
            "newBigQueryAgentAnalyticsPlugin() error = nil, want table check error"
        );
        assert_eq!(
            Arc::strong_count(&bq),
            1,
            "the plugin kept its owned BigQuery client on table verification failure"
        );
    }

    // closes internal bqClient on plugin Close but does not close caller-supplied bqClient
    {
        let internal_bq = bq_client_with("test-project", ok_http_client());
        let (_server, internal_write) = new_write_client();
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            disable_create_views: true,
            ..Config::default()
        };
        let p = new_plugin(
            &ctx,
            cfg.clone(),
            Some(Arc::clone(&internal_bq)),
            Some(internal_write),
            true,
        )
        .await
        .expect("newBigQueryAgentAnalyticsPlugin(ownBQClient=true) error");
        assert!(
            Arc::strong_count(&internal_bq) > 1,
            "the plugin released its owned BigQuery client before Close()"
        );
        (p.close().unwrap())().await.expect("p.Close() error");
        assert_eq!(
            Arc::strong_count(&internal_bq),
            1,
            "the plugin kept its owned BigQuery client after Close()"
        );

        // Public NewBigQueryAgentAnalyticsPluginWithConfig owns the clients it builds, and
        // p.Close() releases them.
        let http_srv = gadk_model::rest::test_util::FakeServer::start(|r: &RecordedRequest| {
            if r.method == "GET" && r.path.contains("/tables/") {
                return FakeResponse::json(
                    r#"{"type":"TABLE","labels":{"adk_schema_version":"2"}}"#,
                );
            }
            FakeResponse::json("{}")
        });
        let public_cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            disable_create_views: true,
            client_options: ClientOptions {
                endpoint: Some(http_srv.url()),
                without_authentication: true,
                ..ClientOptions::default()
            },
            ..Config::default()
        };
        let p_public = new_bigquery_agent_analytics_plugin_with_config(&ctx, public_cfg)
            .await
            .expect("NewBigQueryAgentAnalyticsPluginWithConfig() error");
        (p_public.close().unwrap())()
            .await
            .expect("pPublic.Close() error");

        // Caller-supplied bqClient via NewBigQueryAgentAnalyticsPluginWithClients must NOT be
        // closed by p.Close().
        let caller_bq = bq_client_with("test-project", ok_http_client());
        let (_server2, caller_write) = new_write_client();
        let p2 = new_bigquery_agent_analytics_plugin_with_clients(
            &ctx,
            cfg,
            Some(Arc::clone(&caller_bq)),
            Some(caller_write),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
        (p2.close().unwrap())().await.expect("p2.Close() error");
        assert_eq!(
            Arc::strong_count(&caller_bq),
            1,
            "the plugin kept the caller's BigQuery client after Close()"
        );
        assert!(
            caller_bq
                .get_table(&ctx, "test-project", "test_dataset", "test-table")
                .await
                .is_ok(),
            "caller-supplied BigQuery client unusable after closing WithClients plugin (caller owns bqClient)"
        );
    }
}
