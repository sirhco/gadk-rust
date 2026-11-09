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

//! Port of ADK-Go `model/openaimodel/internal/responses/servertext_test.go`.

use pretty_assertions::assert_eq;

use super::model_openaimodel_internal_responses_stream::decode_event;
use crate::error::ErrorKind;
use crate::responses::response::{convert_response, failed_response_error, reports_failure};
use crate::responses::stream::StreamTranslator;
use crate::responses::wire::{
    Response, ResponseError, ResponseOutputContent, ResponseOutputItem, status,
};
use crate::shared::servertext::MAX_SERVER_TEXT_RUNES;

/// Go `unicode.IsControl(r) || r == '\u2028' || r == '\u2029'`: a rune that splits a line.
fn splits_line(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// A failed response with `id`, error `code` and `message` (Go's literal in these tests).
fn failed(id: &str, code: &str, message: &str) -> Response {
    Response {
        id: Some(id.into()),
        status: Some(status::FAILED.into()),
        error: Some(ResponseError {
            code: code.into(),
            message: message.into(),
            ..ResponseError::default()
        }),
        ..Response::default()
    }
}

/// Pins what no server-chosen string may do to the error built from it: carry something into
/// an operator's log that splits a line, or grow without bound.
// parity: adapted: Go's fuzz target runs here over its seed corpus only (no fuzzing engine); the invariants are Go's
#[test]
fn fuzz_failed_response_error() {
    let seeds: Vec<(String, String, String)> = vec![
        (
            "upstream exploded".into(),
            "server_error".into(),
            "resp_123".into(),
        ),
        (
            "line\r\nERROR forged".into(),
            "code\nnope".into(),
            "id\r\nforged".into(),
        ),
        (
            "\u{1b}[2J\u{1b}[1;1H".into(),
            "\u{7f}".into(),
            "\u{0}".into(),
        ),
        (
            "sep\u{2028}sep\u{2029}".into(),
            "\u{85}".into(),
            "\u{1f642}".into(),
        ),
        (
            "A".repeat(1 << 12),
            "B".repeat(1 << 12),
            "C".repeat(1 << 12),
        ),
        // The worst case for the bound below: an unprintable astral rune is the longest thing
        // %q can produce from one rune.
        (
            "\u{10ffff}".repeat(1 << 10),
            "\u{10ffff}".repeat(1 << 10),
            "\u{10ffff}".repeat(1 << 10),
        ),
    ];
    // Derived rather than guessed: each field is clipped to the cap plus its truncation
    // marker, %q renders the worst rune it can be made of (an unprintable astral one) as the
    // ten characters of \U0010ffff, and wraps the result in two quotes. Three such fields,
    // plus the labels and the sentinel, is the ceiling.
    let max_rendered = 3 * (10 * (MAX_SERVER_TEXT_RUNES + 1) + 2) + 64;
    for (msg, code, id) in seeds {
        let err = failed_response_error(&failed(&id, &code, &msg));
        let got = err.to_string();
        for c in got.chars() {
            assert!(
                !splits_line(c),
                "failedResponseError({msg:?}, {code:?}, {id:?}) = {got:?}, which carries U+{:04X}",
                c as u32
            );
        }
        let n = got.chars().count();
        assert!(
            n <= max_rendered,
            "failedResponseError() produced {n} runes, want at most {max_rendered}"
        );
        assert!(
            err.is(ErrorKind::ResponseFailed),
            "failedResponseError() = {err}, want it to wrap shared.ErrResponseFailed"
        );
    }
}

/// Spells out on worked examples what the fuzz target asserts in general.
#[test]
fn test_failed_response_error_server_text() {
    // control characters cannot forge a line
    {
        let got = failed_response_error(&failed(
            "resp_1\r\nid forged",
            "server_error\u{1b}[2J",
            "line1\r\nERROR forged line 2\tand a tab\u{1b}[2J",
        ))
        .to_string();
        assert!(
            !got.contains(['\r', '\n', '\t', '\u{1b}']),
            "control characters cannot forge a line: error {got:?} still carries a raw control character"
        );
        for want in [r"\r\n", r"\t", r"\x1b"] {
            assert!(
                got.contains(want),
                "control characters cannot forge a line: error {got:?} does not render {want} visibly"
            );
        }
    }

    // line separators cannot forge a line either
    {
        let got = failed_response_error(&Response {
            id: None,
            ..failed("", "", "before\u{2028}after\u{2029}end")
        })
        .to_string();
        assert!(
            !got.contains(['\u{2028}', '\u{2029}']),
            "line separators cannot forge a line either: error {got:?} still carries a raw line separator"
        );
    }

    // a huge message is capped
    {
        let got = failed_response_error(&failed("", "", &"A".repeat(1 << 20))).to_string();
        // Runes, because that is the unit the cap counts in.
        let (n, max) = (got.chars().count(), MAX_SERVER_TEXT_RUNES + 64);
        assert!(
            n <= max,
            "a huge message is capped: error is {n} runes, want at most {max}"
        );
        assert!(
            got.ends_with("\u{2026}\""),
            "a huge message is capped: error {got:?} does not mark that it was truncated"
        );
    }

    // a huge id and code are capped
    {
        let got = failed_response_error(&failed(
            &"\u{1f642}".repeat(1 << 16),
            &"\u{4e16}".repeat(1 << 16),
            "",
        ))
        .to_string();
        // Non-ASCII on purpose. Both runes are printable, so %q emits them as themselves (one
        // rune, several bytes each), and a bound counted in bytes would trip on the encoding
        // rather than on any real growth.
        let (n, max) = (got.chars().count(), 3 * (MAX_SERVER_TEXT_RUNES + 1) + 64);
        assert!(
            n <= max,
            "a huge id and code are capped: error is {n} runes, want at most {max}"
        );
    }

    // a short message is quoted, not altered
    {
        let msg = "upstream exploded (code 429): retry";
        let got = failed_response_error(&failed("", "", msg)).to_string();
        assert_eq!(
            got,
            format!(r#"openai: response failed: "{msg}""#),
            "a short message is quoted, not altered"
        );
    }
}

/// Pins the predicate to the renderer. A status-less body whose error is nothing but
/// whitespace says nothing, so it is a turn: reading the raw value here while the renderer
/// reads the clipped one would discard the output and report a failure naming nothing.
#[test]
fn test_reports_failure_whitespace_only_error() {
    let resp = Response {
        id: Some("resp_123".into()),
        error: Some(ResponseError {
            code: " \t ".into(),
            message: "  \n ".into(),
            ..ResponseError::default()
        }),
        output: Some(vec![ResponseOutputItem {
            kind: "message".into(),
            content: vec![ResponseOutputContent {
                kind: "output_text".into(),
                text: "a whole answer".into(),
                ..ResponseOutputContent::default()
            }],
            ..ResponseOutputItem::default()
        }]),
        ..Response::default()
    };
    assert!(
        !reports_failure(&resp),
        "reportsFailure() = true for an error object of nothing but whitespace, want false"
    );
    let got = convert_response(Some(&resp))
        .unwrap_or_else(|err| panic!("convertResponse() err = {err}, want the turn converted"));
    assert_eq!(
        got.candidates[0].content.as_ref().unwrap().parts[0].text,
        "a whole answer",
        "text, want the output the server produced"
    );
}

/// Pins the generic stream error to the same treatment as a failed response body: the text is
/// the server's, so it is capped and quoted rather than interpolated bare.
#[test]
fn test_stream_translator_error_event_server_text() {
    // nothing that splits a line survives
    {
        let event = decode_event(
            r#"{"type":"error","message":"boom\r\nERROR forged\u001b[2J\u2028sep\u0085"}"#,
        );
        let err = StreamTranslator::new().process(&event).expect_err(
            "nothing that splits a line survives: process() err = nil, want the stream error",
        );
        let got = err.to_string();
        for c in got.chars() {
            assert!(
                !splits_line(c),
                "nothing that splits a line survives: process() err = {got:?} carries U+{:04X}",
                c as u32
            );
        }
    }

    // an error event with no message still errors
    {
        let event = decode_event(r#"{"type":"error"}"#);
        let err = StreamTranslator::new().process(&event).expect_err(
            "an error event with no message still errors: process() err = nil, want the stream error even with nothing to quote",
        );
        assert_eq!(
            err.to_string(),
            "openai stream error",
            "an error event with no message still errors"
        );
    }

    // a huge message is capped
    {
        // Non-ASCII, so the bound is measured against the cap's own unit rather than against
        // an encoding that happens to be one byte wide.
        let event = decode_event(&format!(
            r#"{{"type":"error","message":"{}"}}"#,
            "\u{4e16}".repeat(1 << 12)
        ));
        let err = StreamTranslator::new()
            .process(&event)
            .expect_err("a huge message is capped: process() err = nil, want the stream error");
        let (n, max) = (err.to_string().chars().count(), MAX_SERVER_TEXT_RUNES + 64);
        assert!(
            n <= max,
            "a huge message is capped: process() err is {n} runes, want at most {max}"
        );
    }

    // a short message is quoted, not altered. The literal, because every other assertion here
    // is a property: without this row the prefix could be reworded and the package would stay
    // green.
    {
        let event = decode_event(r#"{"type":"error","message":"boom"}"#);
        let err = StreamTranslator::new().process(&event).expect_err(
            "a short message is quoted, not altered: process() err = nil, want the stream error",
        );
        assert_eq!(
            err.to_string(),
            r#"openai stream error: "boom""#,
            "a short message is quoted, not altered"
        );
    }
}
