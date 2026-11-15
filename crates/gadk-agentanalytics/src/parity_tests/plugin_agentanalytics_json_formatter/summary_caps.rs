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

//! `json_formatter_test.go` (ADK-Go `0990f9e`, #2246): the response summary caps,
//! `TestResponseMessageSummaryLengthCap` and `TestResponseMessageSummaryFunctionNameCap`.

use std::sync::Arc;
use std::time::Duration;

use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use super::super::plugin_agentanalytics_batch_processor::support::content_parts_of;
use super::super::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    Mc, TestPlugin, json_object_column, new_test_plugin_with_config, only_row_of_type,
    until_event_type,
};
use crate::bigquery::fake::{FakeError, FakeOffloader};
use crate::config::{Config, GcsUploadRequest};
use crate::event_types::{EVENT_AGENT_RESPONSE, EVENT_INVOCATION_COMPLETED, EVENT_LLM_RESPONSE};
use crate::formatter::{
    ContentSummaryBuilder, MessageSummary, TRUNCATION_SUFFIX, format_response_content,
};
use crate::redaction::{COLUMN_CONTENT, REDACTED_VALUE};
use crate::row::{Row, RowExt, RowValue};

/// Go `responseSummarySlack`: the runes a non-negative limit gains for a response summary.
const RESPONSE_SUMMARY_SLACK: usize = 4096;

/// Go `responseTextParts(n, size, r)`.
fn response_text_parts(n: usize, size: usize, r: &str) -> Vec<Part> {
    (0..n).map(|_| Part::from_text(r.repeat(size))).collect()
}

/// Go `responseTextPiece(text)`.
fn response_text_piece(text: &str) -> String {
    format!("text: '{text}'")
}

/// Go `joinedResponsePieces(piece, n)`.
fn joined_response_pieces(piece: &str, n: usize) -> String {
    vec![piece; n].join(" | ")
}

/// Go `string([]rune(s)[:n])`.
fn runes(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn summary(parts: Vec<Part>, max_length: i64, redact: bool) -> (String, bool) {
    format_response_content(
        Some(&Content {
            role: String::new(),
            parts,
        }),
        max_length,
        redact,
    )
}

/// Go `p.AfterModelCallback()(mCtx, &model.LLMResponse{Content: content}, nil)`, which must
/// not fail.
async fn after_model(tp: &TestPlugin, m: &Mc, content: Content) {
    let res = LlmResponse {
        content: Some(content),
        ..LlmResponse::default()
    };
    (tp.plugin.after_model_callback().unwrap())(m.context(), res, None)
        .await
        .1
        .unwrap_or_else(|e| panic!("AfterModelCallback error: {e}"));
}

/// Go `p.OnEventCallback()(mCtx, ev)`, which must not fail.
async fn on_event(tp: &TestPlugin, m: &Mc, ev: Event) {
    (tp.plugin.on_event_callback().unwrap())(m.invocation(), ev)
        .await
        .unwrap_or_else(|e| panic!("OnEventCallback error: {e}"));
}

/// Go `p.AfterRunCallback()(mCtx)` and `collectRows(t, requests,
/// untilEventType(EventInvocationCompleted))`.
async fn finish(tp: &TestPlugin, m: &Mc) -> Vec<Row> {
    (tp.plugin.after_run_callback().unwrap())(m.invocation()).await;
    tp.collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await
}

fn model_content(parts: Vec<Part>) -> Content {
    Content {
        role: "model".into(),
        parts,
    }
}

/// Go `responseRowsOf(t, content, maxLength)`: `content` written as an LLM_RESPONSE and as an
/// AGENT_RESPONSE through a plugin whose MaxContentLen is `max_length`; (LLM, agent) rows.
async fn response_rows_of(content: Content, max_length: i64) -> (Row, Row) {
    let tp = new_test_plugin_with_config(|c| c.max_content_len = max_length).await;
    let m = Mc::of(&tp.ctx);
    after_model(&tp, &m, content.clone()).await;
    let mut ev = Event {
        author: "test-agent".into(),
        ..Event::default()
    };
    ev.response.content = Some(content);
    on_event(&tp, &m, ev).await;
    let rows = finish(&tp, &m).await;
    (
        only_row_of_type(&rows, EVENT_LLM_RESPONSE),
        only_row_of_type(&rows, EVENT_AGENT_RESPONSE),
    )
}

/// A written row's `content.response` (Go `jsonObjectColumn(t, row, columnContent)["response"]`).
fn row_response(row: &Row) -> String {
    json_object_column(row, COLUMN_CONTENT)
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The `response` of a row's `content` column (Go `jsonObjectColumn(t, row, columnContent)`).
fn content_response(row: &Map<String, Value>) -> String {
    let text = row
        .get(COLUMN_CONTENT)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("row has no {COLUMN_CONTENT} column"));
    let content: Value = serde_json::from_str(text).unwrap();
    content["response"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn test_response_message_summary_length_cap() {
    let full_length = 4096 - 2 * "text: ''".len() - " | ".len();
    let full_piece = response_text_piece(&"a".repeat(full_length));
    let two_full = joined_response_pieces(&full_piece, 2);
    let small_piece = response_text_piece(&"a".repeat(100));
    let forty_small = joined_response_pieces(&small_piece, 40);
    let huge_piece = response_text_piece(&"a".repeat(5000));
    let wide_length = 4000;
    let wide_piece = response_text_piece(&"é".repeat(wide_length));
    let three_wide = joined_response_pieces(&wide_piece, 3);

    let mut beyond = response_text_parts(2, full_length, "a");
    beyond.push(Part {
        function_call: Some(FunctionCall {
            name: "f".into(),
            ..FunctionCall::default()
        }),
        ..Part::default()
    });
    let tests: Vec<(&str, Vec<Part>, i64, String, bool)> = vec![
        (
            "a part cut by the part limit is not cut again",
            response_text_parts(1, 30, "a"),
            10,
            response_text_piece(&format!("{}{TRUNCATION_SUFFIX}", "a".repeat(10))),
            true,
        ),
        (
            "parts that fill the cap exactly are kept whole",
            response_text_parts(2, full_length, "a"),
            full_length as i64,
            two_full.clone(),
            false,
        ),
        (
            "a part beyond the cap is cut away",
            beyond,
            full_length as i64,
            format!("{two_full}{TRUNCATION_SUFFIX}"),
            true,
        ),
        (
            "many small parts are cut at the cap",
            response_text_parts(40, 100, "a"),
            100,
            format!("{}{TRUNCATION_SUFFIX}", runes(&forty_small, 100 + 4096)),
            true,
        ),
        (
            "multi-byte runes are cut on a rune boundary",
            response_text_parts(3, wide_length, "é"),
            wide_length as i64,
            format!(
                "{}{TRUNCATION_SUFFIX}",
                runes(&three_wide, wide_length + RESPONSE_SUMMARY_SLACK)
            ),
            true,
        ),
        (
            "a negative maxLength leaves the summary uncapped",
            response_text_parts(2, 5000, "a"),
            -1,
            joined_response_pieces(&huge_piece, 2),
            false,
        ),
    ];
    for (name, parts, max_length, want, want_trunc) in tests {
        let n = parts.len();
        let (got, truncated) = summary(parts, max_length, true);
        assert!(
            got == want && truncated == want_trunc,
            "{name}: formatResponseContent({n} parts, {max_length}) = ({} runes, {truncated}), want ({} runes, {want_trunc})",
            got.chars().count(),
            want.chars().count()
        );
    }

    // a summary never exceeds maxLength plus the slack
    for max_length in [0usize, 1, 7, 100, RESPONSE_SUMMARY_SLACK, 5000] {
        for n in [1usize, 2, 3, 10, 50] {
            for size in [
                (max_length / 2).max(1),
                max_length.max(1),
                3 * max_length + 1,
            ] {
                let (got, truncated) =
                    summary(response_text_parts(n, size, "a"), max_length as i64, true);
                let mut piece_runes = "text: ''".len() + size.min(max_length);
                if size > max_length {
                    piece_runes += TRUNCATION_SUFFIX.len();
                }
                let mut want_runes = n * piece_runes + (n - 1) * " | ".len();
                let mut want_trunc = size > max_length;
                let limit = max_length + RESPONSE_SUMMARY_SLACK;
                if want_runes > limit {
                    want_runes = limit + TRUNCATION_SUFFIX.len();
                    want_trunc = true;
                }
                let got_runes = got.chars().count();
                assert!(
                    got_runes == want_runes && truncated == want_trunc,
                    "maxLength {max_length}, {n} parts of {size} runes: got ({got_runes} runes, {truncated}), want ({want_runes} runes, {want_trunc})"
                );
            }
        }
    }

    // LLM_RESPONSE and AGENT_RESPONSE rows beyond the cap are cut
    {
        let (llm, agent) =
            response_rows_of(model_content(response_text_parts(40, 100, "a")), 100).await;
        for (event_type, row, want) in [
            (
                EVENT_LLM_RESPONSE,
                &llm,
                format!(
                    "{}{TRUNCATION_SUFFIX}",
                    runes(&forty_small, 100 + RESPONSE_SUMMARY_SLACK)
                ),
            ),
            (
                EVENT_AGENT_RESPONSE,
                &agent,
                format!("{}{TRUNCATION_SUFFIX}", runes(&forty_small, 100)),
            ),
        ] {
            let got = row_response(row);
            assert!(
                got == want,
                "{event_type} content.response has {} runes, want {}",
                got.chars().count(),
                want.chars().count()
            );
            assert_eq!(
                row.bool("is_truncated"),
                Some(true),
                "{event_type} is_truncated"
            );
        }
    }

    // LLM_RESPONSE row within the cap is kept whole
    {
        let (llm, agent) =
            response_rows_of(model_content(response_text_parts(3, 100, "a")), 100).await;
        let want = joined_response_pieces(&small_piece, 3);
        assert_eq!(row_response(&llm), want, "LLM_RESPONSE content.response");
        assert_eq!(
            llm.bool("is_truncated"),
            Some(false),
            "LLM_RESPONSE is_truncated"
        );
        let want = format!("{}{TRUNCATION_SUFFIX}", runes(&want, 100));
        assert_eq!(
            row_response(&agent),
            want,
            "AGENT_RESPONSE content.response"
        );
        assert_eq!(
            agent.bool("is_truncated"),
            Some(true),
            "AGENT_RESPONSE is_truncated"
        );
    }

    // small MaxContentLen still gets the full 4096-rune slack
    {
        const SMALL_MAX_LEN: usize = 50;
        let tiny_piece = response_text_piece(&"a".repeat(SMALL_MAX_LEN));
        let eighty_tiny = joined_response_pieces(&tiny_piece, 80);
        let (llm, agent) = response_rows_of(
            model_content(response_text_parts(80, SMALL_MAX_LEN, "a")),
            SMALL_MAX_LEN as i64,
        )
        .await;
        let got_llm = row_response(&llm);
        let want_llm = format!(
            "{}{TRUNCATION_SUFFIX}",
            runes(&eighty_tiny, SMALL_MAX_LEN + 4096)
        );
        let want_llm_runes = SMALL_MAX_LEN + 4096 + TRUNCATION_SUFFIX.len(); // 4160
        assert!(
            got_llm.chars().count() == want_llm_runes && got_llm == want_llm,
            "LLM_RESPONSE content.response = {} runes, want {want_llm_runes} runes ending in {TRUNCATION_SUFFIX:?}",
            got_llm.chars().count()
        );
        assert_eq!(
            llm.bool("is_truncated"),
            Some(true),
            "LLM_RESPONSE is_truncated"
        );
        let want_agent = format!("{}{TRUNCATION_SUFFIX}", runes(&eighty_tiny, SMALL_MAX_LEN));
        assert_eq!(
            row_response(&agent),
            want_agent,
            "AGENT_RESPONSE content.response"
        );
        assert_eq!(
            agent.bool("is_truncated"),
            Some(true),
            "AGENT_RESPONSE is_truncated"
        );
    }

    // rebuilt LLM_RESPONSE summary after failed upload is capped
    let max_len = 100;
    let parts = response_text_parts(40, max_len, "a");
    let mut b = ContentSummaryBuilder {
        is_llm_response: true,
        max_len: max_len as i64,
        redact: true,
        messages: vec![MessageSummary::new_response(
            Some(&Content {
                role: String::new(),
                parts,
            }),
            0,
            max_len as i64,
            true,
        )],
        ..ContentSummaryBuilder::default()
    };
    for i in 0..39 {
        b.omit_text_from_summary(0, i);
    }
    let (initial, initial_trunc) = b.build_content();
    b.needs_rebuild = false;
    let got = initial
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        got == small_piece && !initial_trunc,
        "initial buildContent() = ({got:?}, {initial_trunc}), want ({small_piece:?}, false)"
    );
    let fallback = "a".repeat(max_len);
    for i in 0..39 {
        b.retain_inline_summary(0, i, &fallback, false);
    }
    let mut row = Map::new();
    row.insert("is_truncated".into(), Value::Bool(false));
    b.apply_to_row(&mut row);
    let want = format!("{}{TRUNCATION_SUFFIX}", runes(&forty_small, max_len + 4096));
    let got = content_response(&row);
    assert!(
        got == want,
        "rebuilt content.response has {} runes, want {}",
        got.chars().count(),
        want.chars().count()
    );
    assert_eq!(
        row["is_truncated"],
        Value::Bool(true),
        "rebuilt is_truncated"
    );
}

#[tokio::test]
async fn test_response_message_summary_function_name_cap() {
    let at_cap = "n".repeat(256);
    let over_cap = "n".repeat(257);
    let wide_at_cap = "ñ".repeat(256);
    let secret = "SYNTHETIC_SECRET_0123456789";
    let credential = format!("lookup api_key={secret}");
    let over_cap_credential = format!("{over_cap} api_key={secret}");
    let boundary_credential = format!("{} api_key={secret}", "n".repeat(245));
    let call = |name: &str| {
        vec![Part {
            function_call: Some(FunctionCall {
                name: name.into(),
                ..FunctionCall::default()
            }),
            ..Part::default()
        }]
    };
    let resp = |name: &str| {
        vec![Part {
            function_response: Some(FunctionResponse {
                name: name.into(),
                ..FunctionResponse::default()
            }),
            ..Part::default()
        }]
    };
    let cut = |prefix: &str| format!("{prefix}{at_cap}{TRUNCATION_SUFFIX}");
    let boundary_redacted = format!("{} api_key={REDACTED_VALUE}", "n".repeat(245));
    let mut mixed = vec![Part::from_text("hi")];
    mixed.extend(call(&over_cap));
    mixed.extend(resp(&over_cap));

    // (name, parts, maxLength, noRedact, want, wantTrunc)
    let tests: Vec<(&str, Vec<Part>, i64, bool, String, bool)> = vec![
        (
            "call name at the cap is kept",
            call(&at_cap),
            100000,
            false,
            format!("call: {at_cap}"),
            false,
        ),
        (
            "response name at the cap is kept",
            resp(&at_cap),
            100000,
            false,
            format!("resp: {at_cap}"),
            false,
        ),
        (
            "call name over the cap is cut",
            call(&over_cap),
            100000,
            false,
            cut("call: "),
            true,
        ),
        (
            "response name over the cap is cut",
            resp(&over_cap),
            100000,
            false,
            cut("resp: "),
            true,
        ),
        (
            "multi-byte call name is cut by runes",
            call(&format!("{wide_at_cap}ñ")),
            100000,
            false,
            format!("call: {wide_at_cap}{TRUNCATION_SUFFIX}"),
            true,
        ),
        (
            "call name cap does not follow a smaller maxLength",
            call(&over_cap),
            10,
            false,
            cut("call: "),
            true,
        ),
        (
            "response name cap does not follow a smaller maxLength",
            resp(&over_cap),
            10,
            false,
            cut("resp: "),
            true,
        ),
        (
            "call name cap holds at zero maxLength",
            call(&over_cap),
            0,
            false,
            cut("call: "),
            true,
        ),
        (
            "response name cap holds at zero maxLength",
            resp(&over_cap),
            0,
            false,
            cut("resp: "),
            true,
        ),
        (
            "call name is kept whole under an unlimited maxLength",
            call(&over_cap),
            -1,
            false,
            format!("call: {over_cap}"),
            false,
        ),
        (
            "response name is kept whole under an unlimited maxLength",
            resp(&over_cap),
            -1,
            false,
            format!("resp: {over_cap}"),
            false,
        ),
        (
            "credential in a long call name is redacted under an unlimited maxLength",
            call(&over_cap_credential),
            -1,
            false,
            format!("call: {over_cap} api_key={REDACTED_VALUE}"),
            true,
        ),
        (
            "credential in a long response name is redacted under an unlimited maxLength",
            resp(&over_cap_credential),
            -1,
            false,
            format!("resp: {over_cap} api_key={REDACTED_VALUE}"),
            true,
        ),
        (
            "credential in a long call name is kept under an unlimited maxLength when redaction is off",
            call(&over_cap_credential),
            -1,
            true,
            format!("call: {over_cap_credential}"),
            false,
        ),
        (
            "credential in a long response name is kept under an unlimited maxLength when redaction is off",
            resp(&over_cap_credential),
            -1,
            true,
            format!("resp: {over_cap_credential}"),
            false,
        ),
        (
            "credential in a call name is redacted",
            call(&credential),
            100000,
            false,
            format!("call: lookup api_key={REDACTED_VALUE}"),
            true,
        ),
        (
            "credential in a response name is redacted",
            resp(&credential),
            100000,
            false,
            format!("resp: lookup api_key={REDACTED_VALUE}"),
            true,
        ),
        (
            "credential crossing the call name cap is redacted before cutting",
            call(&boundary_credential),
            100000,
            false,
            format!("call: {}{TRUNCATION_SUFFIX}", &boundary_redacted[..256]),
            true,
        ),
        (
            "credential in a call name is kept when redaction is off",
            call(&credential),
            100000,
            true,
            format!("call: {credential}"),
            false,
        ),
        (
            "credential in a response name is kept when redaction is off",
            resp(&credential),
            100000,
            true,
            format!("resp: {credential}"),
            false,
        ),
        (
            "cut names keep their place among the other parts",
            mixed,
            100000,
            false,
            format!(
                "text: 'hi' | call: {at_cap}{TRUNCATION_SUFFIX} | resp: {at_cap}{TRUNCATION_SUFFIX}"
            ),
            true,
        ),
    ];
    for (name, parts, max_length, no_redact, want, want_trunc) in tests {
        let (got, truncated) = summary(parts, max_length, !no_redact);
        assert_eq!((&got, truncated), (&want, want_trunc), "{name}");
        assert!(
            no_redact || !got.contains("SYNTHETIC"),
            "{name}: summary {got:?} leaked the secret"
        );
        assert!(
            max_length >= 0 || !got.contains(TRUNCATION_SUFFIX),
            "{name}: unlimited summary {got:?} unexpectedly contains {TRUNCATION_SUFFIX:?}"
        );
    }

    // LLM_RESPONSE row cuts a long call name
    {
        let tp = new_test_plugin_with_config(|c| c.max_content_len = 100000).await;
        let m = Mc::of(&tp.ctx);
        let mut parts = call(&over_cap);
        parts.push(Part::from_text("done"));
        after_model(&tp, &m, model_content(parts)).await;
        let row = only_row_of_type(&finish(&tp, &m).await, EVENT_LLM_RESPONSE);
        assert_eq!(
            row_response(&row),
            format!("call: {at_cap}{TRUNCATION_SUFFIX} | text: 'done'"),
            "LLM_RESPONSE content.response"
        );
        assert_eq!(
            row.bool("is_truncated"),
            Some(true),
            "LLM_RESPONSE is_truncated"
        );
    }

    // CaptureFullMessageContent keeps long function names and summaries whole
    {
        let over_cap_call = "c".repeat(257);
        let over_cap_resp = "r".repeat(257);
        let tp = new_test_plugin_with_config(|c| {
            c.max_content_len = 1;
            c.capture_full_message_content = true;
        })
        .await;
        let m = Mc::of(&tp.ctx);
        let mut parts = call(&over_cap_call);
        parts.extend(resp(&over_cap_resp));
        after_model(&tp, &m, model_content(parts)).await;
        // OnEventCallback skips events carrying a FunctionCall and filters AGENT_RESPONSE to
        // the visible text parts, so the full text summary stays whole despite MaxContentLen 1.
        let mut ev = Event {
            id: "ev-full-agent-resp".into(),
            author: "test-agent".into(),
            actions: EventActions {
                skip_summarization: true,
                ..EventActions::default()
            },
            ..Event::default()
        };
        ev.response.content = Some(model_content(vec![
            Part {
                function_response: Some(FunctionResponse {
                    name: over_cap_resp.clone(),
                    response: json!({"ok": true}).as_object().cloned().unwrap_or_default(),
                    ..FunctionResponse::default()
                }),
                ..Part::default()
            },
            Part::from_text(over_cap.clone()),
        ]));
        on_event(&tp, &m, ev).await;
        let rows = finish(&tp, &m).await;

        let llm_row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
        let got_llm = row_response(&llm_row);
        assert_eq!(
            got_llm,
            format!("call: {over_cap_call} | resp: {over_cap_resp}"),
            "LLM_RESPONSE content.response"
        );
        assert!(
            !got_llm.contains(TRUNCATION_SUFFIX),
            "LLM_RESPONSE content.response unexpectedly truncated"
        );
        assert_eq!(
            llm_row.bool("is_truncated"),
            Some(false),
            "LLM_RESPONSE is_truncated"
        );

        let agent_row = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
        let got_agent = row_response(&agent_row);
        assert_eq!(
            got_agent,
            format!("text: '{over_cap}'"),
            "AGENT_RESPONSE content.response"
        );
        assert!(
            !got_agent.contains(TRUNCATION_SUFFIX),
            "AGENT_RESPONSE content.response unexpectedly truncated"
        );
        assert_eq!(
            agent_row.bool("is_truncated"),
            Some(false),
            "AGENT_RESPONSE is_truncated"
        );
    }

    // MaxContentLen -1 without CaptureFullMessageContent keeps long function names and
    // summaries whole
    {
        let tp = new_test_plugin_with_config(|c| {
            c.max_content_len = -1;
            c.capture_full_message_content = false;
        })
        .await;
        let m = Mc::of(&tp.ctx);
        let call_name_300 = "f".repeat(300);
        let mut parts = call(&call_name_300);
        parts.extend(response_text_parts(40, 5000, "a"));
        after_model(&tp, &m, model_content(parts)).await;
        let row = only_row_of_type(&finish(&tp, &m).await, EVENT_LLM_RESPONSE);
        let got = row_response(&row);
        let want = format!(
            "call: {call_name_300} | {}",
            joined_response_pieces(&response_text_piece(&"a".repeat(5000)), 40)
        );
        assert!(
            got == want,
            "LLM_RESPONSE content.response = {} runes, want {} runes",
            got.chars().count(),
            want.chars().count()
        );
        assert!(
            !got.contains(TRUNCATION_SUFFIX),
            "LLM_RESPONSE content.response unexpectedly contains {TRUNCATION_SUFFIX:?}"
        );
        assert_eq!(
            row.bool("is_truncated"),
            Some(false),
            "LLM_RESPONSE is_truncated"
        );
    }

    // failed text upload with content denied ignores summary-only function-name cap
    {
        let call_name_300 = "c".repeat(300);
        let resp_name_300 = "r".repeat(300);
        let text50 = "t".repeat(50);
        let text400 = "t".repeat(400);
        let mut parts_a = call(&call_name_300);
        parts_a.extend(resp(&resp_name_300));
        parts_a.push(Part::from_text(text50.clone()));
        let with_text = |text: &str| {
            let mut parts = call(&call_name_300);
            parts.push(Part::from_text(text));
            parts
        };
        let cases: Vec<(&str, Vec<String>, i64, Vec<Part>, &str, Vec<String>, bool)> = vec![
            (
                "content denied with 300-rune names and uncut fallback keeps is_truncated false",
                vec![COLUMN_CONTENT.into()],
                500,
                parts_a,
                "",
                vec![
                    format!("Function: {call_name_300}"),
                    format!("Function response: {resp_name_300}"),
                    text50.clone(),
                ],
                false,
            ),
            (
                "content denied with uncut 300-rune call name and cut text fallback sets is_truncated true",
                vec![COLUMN_CONTENT.into()],
                350,
                with_text(&text400),
                "",
                vec![
                    format!("Function: {call_name_300}"),
                    format!("{}{TRUNCATION_SUFFIX}", &text400[..350]),
                ],
                true,
            ),
            (
                "content denied with MaxContentLen 30 cuts fallback and sets is_truncated true",
                vec![COLUMN_CONTENT.into()],
                30,
                with_text(&text50),
                "",
                vec![
                    format!("Function: {}{TRUNCATION_SUFFIX}", &call_name_300[..30]),
                    format!("{}{TRUNCATION_SUFFIX}", &text50[..30]),
                ],
                true,
            ),
            (
                "content written with 300-rune call name and uncut fallback sets is_truncated true",
                Vec::new(),
                500,
                with_text(&text50),
                "content",
                vec![format!("Function: {call_name_300}"), text50.clone()],
                true,
            ),
        ];
        let want_content_written = format!(
            "call: {}{TRUNCATION_SUFFIX} | text: '{text50}'",
            &call_name_300[..256]
        );
        for (name, denylist, max_content_len, parts, want_content, want_parts_text, want_trunc) in
            cases
        {
            let failing_off = Arc::new(FakeOffloader::new(Arc::new(|_: &GcsUploadRequest| {
                Err(FakeError::Message("upload failed".into()))
            })));
            let tp = new_test_plugin_with_config(|c: &mut Config| {
                c.gcs_offloader = Some(failing_off.clone());
                c.batch_flush_intv = Duration::from_millis(10);
                c.payload_column_denylist = denylist;
                c.max_content_len = max_content_len;
                c.max_inline_text_size = 20;
            })
            .await;
            let m = Mc::of(&tp.ctx);
            after_model(&tp, &m, model_content(parts)).await;
            let row = only_row_of_type(&finish(&tp, &m).await, EVENT_LLM_RESPONSE);
            if want_content.is_empty() {
                assert!(
                    matches!(row.get(COLUMN_CONTENT), None | Some(RowValue::Null)),
                    "{name}: row[{COLUMN_CONTENT:?}] = {:?}, want omitted when content is denied",
                    row.get(COLUMN_CONTENT)
                );
            } else {
                assert_eq!(
                    row_response(&row),
                    want_content_written,
                    "{name}: LLM_RESPONSE content.response"
                );
            }
            let got_parts = content_parts_of(&row);
            assert_eq!(
                got_parts.len(),
                want_parts_text.len(),
                "{name}: content_parts"
            );
            for (i, want_text) in want_parts_text.iter().enumerate() {
                assert_eq!(
                    got_parts[i].str("text").unwrap_or_default(),
                    want_text,
                    "{name}: content_parts[{i}].text"
                );
            }
            assert_eq!(
                row.bool("is_truncated"),
                Some(want_trunc),
                "{name}: LLM_RESPONSE is_truncated"
            );
        }
    }
}
