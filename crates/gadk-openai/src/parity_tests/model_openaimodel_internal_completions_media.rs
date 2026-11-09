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

//! Port of ADK-Go `model/openaimodel/internal/completions/media_test.go`.
//!
//! `wire_content` re-marshals the decoded content as Go does (`json.Marshal` of a
//! `map[string]any`: sorted keys, HTML-escaped), so the expected strings are Go's bytes.

use bytes::Bytes;
use gadk_genai::{Blob, Content, GenerateContentConfig, Part};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::error::ErrorKind;

use super::model_openaimodel_internal_completions_request::{
    build_err, call_part, content, field, file_data, inline_part, request_wire, response_part,
    text, user_req, wire_messages,
};

/// Go `wireContent`: the content of the one user message `req` builds, as the JSON Go's
/// `json.Marshal` writes for it.
fn wire_content(req: &LlmRequest) -> String {
    let msgs = wire_messages(&request_wire(req));
    assert_eq!(
        msgs.len(),
        1,
        "got {} messages, want 1: {msgs:?}",
        msgs.len()
    );
    assert_eq!(field(&msgs[0], "role"), json!("user"), "role, want user");
    let raw = gadk_model::to_go_json(&field(&msgs[0], "content"))
        .unwrap_or_else(|err| panic!("marshal content: {err}"));
    String::from_utf8(raw).unwrap_or_else(|err| panic!("marshal content: {err}"))
}

/// Go `&genai.Part{InlineData: &genai.Blob{MIMEType: mime_type, Data: data, DisplayName:
/// display_name}}`.
fn named_inline_part(mime_type: &str, data: &'static [u8], display_name: &str) -> Part {
    Part {
        inline_data: Some(Blob {
            mime_type: mime_type.to_string(),
            data: Bytes::from_static(data),
            display_name: display_name.to_string(),
        }),
        ..Part::default()
    }
}

/// Go `&genai.Part{FileData: &genai.FileData{MIMEType: mime_type, FileURI: file_uri}}`.
fn file_part(mime_type: &str, file_uri: &str) -> Part {
    Part {
        file_data: Some(file_data(mime_type, file_uri)),
        ..Part::default()
    }
}

#[test]
fn test_build_params_media_on_the_wire() {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G'];
    const PDF: &[u8] = b"%PDF";
    const PNG_URL: &str = "data:image/png;base64,iVBORw==";
    const PDF_URL: &str = "data:application/pdf;base64,JVBERg==";
    let image =
        format!(r#"{{"image_url":{{"detail":"auto","url":"{PNG_URL}"}},"type":"image_url"}}"#);

    let tests: Vec<(&str, Vec<Part>, String)> = vec![
        (
            "text then inline image keeps its order",
            vec![text("describe this:"), inline_part("image/png", PNG)],
            format!(r#"[{{"text":"describe this:","type":"text"}},{image}]"#),
        ),
        (
            "image then text keeps its order",
            vec![inline_part("image/png", PNG), text("what is it?")],
            format!(r#"[{image},{{"text":"what is it?","type":"text"}}]"#),
        ),
        (
            "text and media on one part: text first",
            vec![Part {
                text: "look".to_string(),
                ..inline_part("image/png", PNG)
            }],
            format!(r#"[{{"text":"look","type":"text"}},{image}]"#),
        ),
        (
            "image by url",
            vec![file_part("image/jpeg", "https://example.com/a.jpg")],
            r#"[{"image_url":{"detail":"auto","url":"https://example.com/a.jpg"},"type":"image_url"}]"#
                .to_string(),
        ),
        (
            "inline file with a display name",
            vec![named_inline_part("application/pdf", PDF, "report.pdf")],
            format!(
                r#"[{{"file":{{"file_data":"{PDF_URL}","filename":"report.pdf"}},"type":"file"}}]"#
            ),
        ),
        (
            "inline file without a display name gets the shared fallback",
            vec![inline_part("application/pdf", PDF)],
            format!(
                r#"[{{"file":{{"file_data":"{PDF_URL}","filename":"inline_data"}},"type":"file"}}]"#
            ),
        ),
        (
            "uploaded file is a file id with no filename",
            vec![Part {
                file_data: Some(gadk_genai::FileData {
                    display_name: "report.pdf".to_string(),
                    ..file_data("application/pdf", "file-abc123")
                }),
                ..Part::default()
            }],
            r#"[{"file":{"file_id":"file-abc123"},"type":"file"}]"#.to_string(),
        ),
        (
            "text between two media parts",
            vec![
                inline_part("image/png", PNG),
                text("versus"),
                named_inline_part("application/pdf", PDF, "b.pdf"),
            ],
            format!(
                r#"[{image},{{"text":"versus","type":"text"}},{{"file":{{"file_data":"{PDF_URL}","filename":"b.pdf"}},"type":"file"}}]"#
            ),
        ),
    ];
    for (name, parts, want) in tests {
        let got = wire_content(&user_req(parts));
        assert_eq!(got, want, "{name}: content");
    }
}

#[test]
fn test_build_params_media_refused() {
    const SECRET_URL: &str = "https://example.com/doc.pdf?X-Amz-Signature=SECRET";
    let png = || inline_part("image/png", &[1]);
    let ok_response = || response_part("c1", "f", json!({"ok": true}));
    let system_instruction_req = |part: Part| LlmRequest {
        contents: vec![content("user", vec![text("hi")])],
        config: Some(GenerateContentConfig {
            system_instruction: Some(Content {
                parts: vec![part],
                ..Content::default()
            }),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    let one_turn = |role: &str, parts: Vec<Part>| LlmRequest {
        contents: vec![content(role, parts)],
        ..LlmRequest::default()
    };

    // (name, request, wantIs, wantMsg)
    let tests: Vec<(&str, LlmRequest, Option<ErrorKind>, &str)> = vec![
        (
            // the MIME type, not the field it would have gone in
            "audio is named by the type the part declared",
            user_req(vec![inline_part("audio/wav", &[1])]),
            Some(ErrorKind::UnsupportedMimeType),
            "audio/wav",
        ),
        (
            "video",
            user_req(vec![file_part("video/mp4", "https://example.com/v.mp4")]),
            Some(ErrorKind::UnsupportedMimeType),
            "video/mp4",
        ),
        (
            "uploaded image has no image_url field",
            user_req(vec![file_part("image/png", "file-abc123")]),
            None,
            "unsupported media source for an image on Chat Completions: file_id",
        ),
        (
            "file by url has no field and its uri stays out of the error",
            user_req(vec![file_part("application/pdf", SECRET_URL)]),
            None,
            "unsupported media source for a file on Chat Completions: url",
        ),
        (
            "media with no mime type",
            user_req(vec![inline_part("", &[1])]),
            Some(ErrorKind::MediaMimeTypeRequired),
            "inline data",
        ),
        (
            "media on a replayed model turn",
            one_turn("model", vec![png()]),
            Some(ErrorKind::MediaOnAssistantTurn),
            "image/png",
        ),
        (
            "media in a system turn",
            one_turn("system", vec![png()]),
            None,
            "media in a system message is not supported: image/png",
        ),
        (
            "media in a developer turn",
            one_turn("developer", vec![png()]),
            None,
            "media in a developer message is not supported: image/png",
        ),
        (
            // The system instruction becomes a text-only system message, so the image would
            // be dropped while its text goes out.
            "text and inline data on one system instruction part",
            system_instruction_req(Part {
                text: "describe it".to_string(),
                ..inline_part("image/png", &[1])
            }),
            None,
            "system instruction part: InlineData",
        ),
        (
            "text and file data on one system instruction part",
            system_instruction_req(Part {
                text: "describe it".to_string(),
                ..file_part("application/pdf", SECRET_URL)
            }),
            None,
            "system instruction part: FileData",
        ),
        (
            "media beside a function response",
            LlmRequest {
                contents: vec![
                    content("model", vec![call_part("c1", "f", Value::Null)]),
                    content(
                        "user",
                        vec![Part {
                            inline_data: png().inline_data,
                            ..ok_response()
                        }],
                    ),
                ],
                ..LlmRequest::default()
            },
            None,
            "media on a function response part is not supported: image/png",
        ),
        (
            "function call beside media in a user turn",
            LlmRequest {
                contents: vec![
                    content("user", vec![call_part("c1", "f", Value::Null), png()]),
                    content("user", vec![ok_response()]),
                ],
                ..LlmRequest::default()
            },
            None,
            "function call in a user turn",
        ),
    ];
    for (name, req, want_is, want_msg) in tests {
        let err = build_err(&req);
        if let Some(kind) = want_is {
            assert!(err.is(kind), "{name}: err = {err}, want errors.Is {kind}");
        }
        let msg = err.to_string();
        assert!(
            msg.contains(want_msg),
            "{name}: err = {msg:?}, want it to contain {want_msg:?}"
        );
        assert!(
            !msg.contains("SECRET"),
            "{name}: err = {msg:?} leaks the file uri"
        );
    }
}

/// The media path left the plain-text request as it was: one string, joined with newlines.
#[test]
fn test_build_params_text_only_turn_stays_a_string() {
    let msgs = wire_messages(&request_wire(&user_req(vec![text("a"), text("b")])));
    assert_eq!(
        field(&msgs[0], "content"),
        json!("a\nb"),
        "want the joined string"
    );
}
