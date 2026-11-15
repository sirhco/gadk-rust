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

use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value};

use crate::formatter::{
    ContentSummaryBuilder, MessageSummary, TRUNCATION_SUFFIX, format_response_content,
};
use crate::redaction::{COLUMN_CONTENT, REDACTED_VALUE};

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

/// The `response` of a row's `content` column (Go `jsonObjectColumn(t, row, columnContent)`).
fn content_response(row: &Map<String, Value>) -> String {
    let text = row
        .get(COLUMN_CONTENT)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("row has no {COLUMN_CONTENT} column"));
    let content: Value = serde_json::from_str(text).unwrap();
    content["response"].as_str().unwrap_or_default().to_owned()
}

// parity: adapted: the LLM_RESPONSE / AGENT_RESPONSE row subtests (responseRowsOf, small MaxContentLen) are left out; their rows write this same format_response_content result (8a fix task ports them when it un-ignores)
#[test]
#[ignore = "8a bug: a response summary is not capped at maxLength + 4096 runes (Go responseSummarySlack, #2246)"]
fn test_response_message_summary_length_cap() {
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

// parity: adapted: the plugin-row subtests (LLM_RESPONSE row, CaptureFullMessageContent, MaxContentLen -1, failed text upload with content denied) are left out; the rows write this same format_response_content result (8a fix task ports them when it un-ignores)
#[test]
#[ignore = "8a bug: response summary function names are neither redacted nor capped at 256 runes (Go maxFunctionNameSummaryLength, #2246)"]
fn test_response_message_summary_function_name_cap() {
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
}
