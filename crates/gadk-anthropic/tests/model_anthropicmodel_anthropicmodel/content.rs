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

//! Ports of TestSamplePNG1x1_DecodesAsValidPNG, TestMultimodalSupport,
//! TestFunctionResponseStructSerializationAndEmptyResult, TestExecutableCodeAndURISchemePrivacy,
//! TestOutputSchemaAndStructuredOutput (Task 11).
//!
//! Each Go test is one `fn test_*`; each Go `t.Run` subtest is an `async fn` named after it and
//! called in Go's order against the test's shared fake server (Go shares one `httptest` server
//! across subtests); each table case carries Go's subtest name in every assertion message.
//! Go's `lastBody` (the last request body, `json.Unmarshal`ed with errors ignored) is
//! `srv.last_request().json()`; indexing a `serde_json::Value` yields `Null` where Go's
//! ignored type assertions yield `nil`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use bytes::Bytes;
use gadk_anthropic::{Backend, ClientConfig, Effort, ErrorKind};
use gadk_genai::{
    CodeExecutionResult, Content, ExecutableCode, FunctionCall, FunctionResponse,
    FunctionResponseBlob, FunctionResponseFileData, FunctionResponsePart, GenerateContentConfig,
    Language, Outcome, Part, ROLE_MODEL, ROLE_USER, Schema, Type,
};
use gadk_model::{Llm, LlmRequest, LlmResponse};
use serde_json::{Map, Value, json};
use tracing::Level;

use crate::harness::{
    FakeResponse, FakeServer, LogCapture, MODEL, SAMPLE_PNG_1X1, anthropic_config, anthropic_error,
    generate, generate_last, generate_ok, new_model, sample_png, sse_lines, text_message_json,
    text_request, vertex_config,
};

/// Go `len(x.([]any))` with the type assertion's error ignored: 0 unless `v` is an array.
fn len_of(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// Go `&model.LLMRequest{Contents: genai.Text(text), Config: cfg}`.
fn text_request_with(text: &str, cfg: GenerateContentConfig) -> LlmRequest {
    LlmRequest {
        config: Some(cfg),
        ..text_request(text)
    }
}

/// Go `&model.LLMRequest{Contents: contents}`.
fn contents_request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest {
        contents,
        ..Default::default()
    }
}

/// Go `&genai.Content{Role: role, Parts: parts}`.
fn content(role: &str, parts: Vec<Part>) -> Content {
    Content {
        role: role.to_string(),
        parts,
    }
}

/// Go `&genai.Part{FunctionCall: &genai.FunctionCall{ID: id, Name: name}}`.
fn function_call_part(id: &str, name: &str) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.to_string(),
            name: name.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `&genai.Part{FunctionResponse: &genai.FunctionResponse{ID, Name, Response, Parts}}`.
fn function_response_part(
    id: &str,
    name: &str,
    response: Value,
    parts: Vec<FunctionResponsePart>,
) -> Part {
    let response: Map<String, Value> = match response {
        Value::Object(m) => m,
        other => panic!("function response must be a JSON object, got {other}"),
    };
    Part {
        function_response: Some(FunctionResponse {
            id: id.to_string(),
            name: name.to_string(),
            response,
            parts,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `&genai.FunctionResponsePart{InlineData: &genai.FunctionResponseBlob{MIMEType, Data}}`.
fn fr_inline(mime_type: &str, data: Bytes) -> FunctionResponsePart {
    FunctionResponsePart {
        inline_data: Some(FunctionResponseBlob {
            mime_type: mime_type.to_string(),
            data,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `&genai.FunctionResponsePart{FileData: &genai.FunctionResponseFileData{MIMEType, FileURI}}`.
fn fr_file(mime_type: &str, file_uri: &str) -> FunctionResponsePart {
    FunctionResponsePart {
        file_data: Some(FunctionResponseFileData {
            mime_type: mime_type.to_string(),
            file_uri: file_uri.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go `&genai.Schema{Type: t}`.
fn schema_of(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Default::default()
    }
}

/// Go's `{city: string, temp_c: integer}` object schema with both fields required.
fn city_temp_schema() -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: [
            ("city".to_string(), schema_of(Type::String)),
            ("temp_c".to_string(), schema_of(Type::Integer)),
        ]
        .into_iter()
        .collect(),
        required: vec!["city".to_string(), "temp_c".to_string()],
        ..Default::default()
    }
}

/// Go `&genai.Schema{Type: genai.TypeArray, Items: &genai.Schema{Type: genai.TypeString}}`.
fn string_array_schema() -> Schema {
    Schema {
        r#type: Some(Type::Array),
        items: Some(Box::new(schema_of(Type::String))),
        ..Default::default()
    }
}

/// The text of every part of every non-partial response (Go `testutil.CollectTextParts` over
/// the runner's events; the runner yields the model's final responses in non-streaming mode).
fn collect_text_parts(resps: &[LlmResponse]) -> Vec<String> {
    resps
        .iter()
        .filter(|r| !r.partial)
        .filter_map(|r| r.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter(|p| !p.text.is_empty())
        .map(|p| p.text.clone())
        .collect()
}

// ------------------------------------------------------------- TestSamplePNG1x1_DecodesAsValidPNG

/// CRC-32 (IEEE 802.3, reflected polynomial 0xEDB88320) as PNG chunks use it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One PNG chunk: (type, data).
type PngChunk<'a> = (&'a [u8], &'a [u8]);

/// Checks the PNG signature and every chunk's length and CRC (the checks Go's `png.Decode`
/// makes before it decodes pixels), returning the chunks in order.
fn parse_png(data: &[u8]) -> Result<Vec<PngChunk<'_>>, String> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if data.len() < 8 || data[..8] != SIGNATURE {
        return Err("png: invalid format: not a PNG file".to_string());
    }
    let mut chunks = Vec::new();
    let mut rest = &data[8..];
    while !rest.is_empty() {
        if rest.len() < 12 {
            return Err("unexpected EOF".to_string());
        }
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if rest.len() < 12 + len {
            return Err("unexpected EOF".to_string());
        }
        let ty = &rest[4..8];
        let body = &rest[8..8 + len];
        let want =
            u32::from_be_bytes([rest[8 + len], rest[9 + len], rest[10 + len], rest[11 + len]]);
        if crc32(&rest[4..8 + len]) != want {
            return Err("png: invalid format: invalid checksum".to_string());
        }
        chunks.push((ty, body));
        rest = &rest[12 + len..];
    }
    Ok(chunks)
}

// parity: adapted: no PNG decoder dependency (ruling R-c); Go's png.Decode + Bounds() become a hand-written check of the signature, every chunk CRC-32, the IHDR (1x1, 8-bit RGB), the zlib header of IDAT and the closing IEND
#[test]
fn test_sample_png1x1_decodes_as_valid_png() {
    let chunks = parse_png(&SAMPLE_PNG_1X1)
        .unwrap_or_else(|e| panic!("png.Decode(samplePNG1x1) error = {e}"));
    let types: Vec<&[u8]> = chunks.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        types,
        vec![&b"IHDR"[..], &b"IDAT"[..], &b"IEND"[..]],
        "samplePNG1x1 chunk types"
    );

    let ihdr = chunks[0].1;
    assert_eq!(ihdr.len(), 13, "IHDR length");
    let width = u32::from_be_bytes([ihdr[0], ihdr[1], ihdr[2], ihdr[3]]);
    let height = u32::from_be_bytes([ihdr[4], ihdr[5], ihdr[6], ihdr[7]]);
    assert!(
        width == 1 && height == 1,
        "samplePNG1x1 bounds = {width}x{height}, want 1x1"
    );
    // Bit depth 8, colour type 2 (truecolour), deflate, adaptive filtering, no interlace.
    assert_eq!(
        &ihdr[8..],
        &[8, 2, 0, 0, 0],
        "IHDR depth/colour/method fields"
    );

    let idat = chunks[1].1;
    assert!(idat.len() >= 2, "IDAT too short for a zlib header");
    assert_eq!(idat[0] & 0x0f, 8, "zlib compression method is deflate");
    assert_eq!(
        (u16::from(idat[0]) * 256 + u16::from(idat[1])) % 31,
        0,
        "zlib header check bits"
    );
    assert!(chunks[2].1.is_empty(), "IEND carries no data");
}

#[test]
fn crc32_matches_known_vectors() {
    // The standard check value of CRC-32/ISO-HDLC and the CRC of "IEND".
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b"IEND"), 0xAE42_6082);
}

#[test]
fn parse_png_rejects_corruption() {
    let mut bad_sig = SAMPLE_PNG_1X1;
    bad_sig[1] = b'X';
    assert_eq!(
        parse_png(&bad_sig).unwrap_err(),
        "png: invalid format: not a PNG file"
    );
    let mut bad_crc = SAMPLE_PNG_1X1;
    bad_crc[29] ^= 0xff; // first byte of IHDR's CRC
    assert_eq!(
        parse_png(&bad_crc).unwrap_err(),
        "png: invalid format: invalid checksum"
    );
    assert_eq!(
        parse_png(&SAMPLE_PNG_1X1[..40]).unwrap_err(),
        "unexpected EOF"
    );
}

// -------------------------------------------------------------------------- TestMultimodalSupport

/// Go's `newBackendModel(t, backend, handler)` (the log handler is the caller's
/// [`LogCapture`], installed around the call).
async fn new_backend_model(srv: &FakeServer, backend: Backend) -> Arc<dyn Llm> {
    let cfg = match backend {
        Backend::Anthropic => anthropic_config(&srv.url()),
        _ => vertex_config(&srv.url()),
    };
    new_model(MODEL, cfg).await
}

/// One `TestMultimodalSupport` table case.
struct MultimodalCase {
    name: &'static str,
    part: Part,
    want_anthropic_sent: bool,
    want_vertex_sent: bool,
}

fn multimodal_cases() -> Vec<MultimodalCase> {
    let case = |name, part, want_anthropic_sent, want_vertex_sent| MultimodalCase {
        name,
        part,
        want_anthropic_sent,
        want_vertex_sent,
    };
    let bytes = |b: &'static [u8]| Bytes::from_static(b);
    vec![
        case(
            "inline_image_png",
            Part::from_bytes(sample_png(), "image/png; charset=binary"),
            true,
            true,
        ),
        case(
            "inline_image_jpeg",
            Part::from_bytes(bytes(b"jpeg-bytes"), "image/jpeg"),
            true,
            true,
        ),
        case(
            "inline_image_gif",
            Part::from_bytes(bytes(b"gif-bytes"), "image/gif"),
            true,
            true,
        ),
        case(
            "inline_image_webp",
            Part::from_bytes(bytes(b"webp-bytes"), "image/webp"),
            true,
            true,
        ),
        case(
            "inline_pdf",
            Part::from_bytes(bytes(b"%PDF-1.4"), "application/pdf"),
            true,
            true,
        ),
        case(
            "inline_plain_text",
            Part::from_bytes(bytes(b"plain document text"), "text/plain; charset=utf-8"),
            true,
            true,
        ),
        case(
            "https_url_image",
            Part::from_uri("https://example.com/secret-image.png", "image/png"),
            true,
            false,
        ),
        case(
            "https_url_pdf",
            Part::from_uri("https://example.com/secret-doc.pdf", "application/pdf"),
            true,
            false,
        ),
        case(
            "https_url_plain_text_unsupported",
            Part::from_uri("https://example.com/notes.txt", "text/plain"),
            false,
            false,
        ),
        case(
            "gs_uri_unsupported",
            Part::from_uri("gs://my-secret-bucket/doc.pdf", "application/pdf"),
            false,
            false,
        ),
        case(
            "inline_audio_unsupported",
            Part::from_bytes(bytes(b"SECRET_AUDIO_BYTES"), "audio/mp3"),
            false,
            false,
        ),
        case(
            "inline_video_unsupported",
            Part::from_bytes(bytes(b"SECRET_VIDEO_BYTES"), "video/mp4"),
            false,
            false,
        ),
        case(
            "inline_octet_stream_unsupported",
            Part::from_bytes(bytes(b"SECRET_BINARY"), "application/octet-stream"),
            false,
            false,
        ),
    ]
}

/// Go's per-case subtest `fmt.Sprintf("%s_%s", tc.name, backend)`.
async fn multimodal_table_case(srv: &FakeServer, tc: &MultimodalCase, backend: Backend) {
    let name = format!("{}_{}", tc.name, backend.as_str());
    let want_sent = match backend {
        Backend::Anthropic => tc.want_anthropic_sent,
        _ => tc.want_vertex_sent,
    };

    let logs = LogCapture::new();
    let _guard = logs.set_default();
    let m = new_backend_model(srv, backend).await;

    // Send alongside a text part so we can verify whether the media block was included or
    // skipped with WARN.
    let req = contents_request(vec![content(
        ROLE_USER,
        vec![tc.part.clone(), Part::from_text("Describe this.")],
    )]);
    generate_ok(m.as_ref(), req, false).await;

    let last_body = srv.last_request().json();
    let content_blocks = &last_body["messages"][0]["content"];
    let records = logs.events();

    if want_sent {
        assert_eq!(
            len_of(content_blocks),
            2,
            "{name}: sent {} content blocks, want 2",
            len_of(content_blocks)
        );
        assert_eq!(
            records.len(),
            0,
            "{name}: logged {} warnings, want 0: {:?}",
            records.len(),
            logs.rendered()
        );
    } else {
        assert_eq!(
            len_of(content_blocks),
            1,
            "{name}: sent {} content blocks, want 1 (only text)",
            len_of(content_blocks)
        );
        assert!(
            records.len() == 1 && records[0].level == Level::WARN,
            "{name}: logged records = {:?}, want 1 WARN record",
            logs.rendered()
        );
        // Ensure no secret URL or payload leaked into log message or attributes.
        for (k, v) in &records[0].fields {
            assert!(
                !(v.contains("example.com")
                    || v.contains("my-secret-bucket")
                    || v.contains("SECRET_")),
                "{name}: log attr {k}={v:?} leaked sensitive data"
            );
        }
    }
}

async fn multimodal_newest_message_empty_after_skipping_unsupported_fails_without_network_call(
    srv: &FakeServer,
) {
    let name = "newest_message_empty_after_skipping_unsupported_fails_without_network_call";
    let logs = LogCapture::new();
    let _guard = logs.set_default();
    let m = new_backend_model(srv, Backend::Anthropic).await;
    let before_count = srv.hits();

    let req = contents_request(vec![
        Content::from_text("Earlier valid message", ROLE_USER),
        Content::from_text("Earlier assistant reply", ROLE_MODEL),
        content(
            ROLE_USER,
            vec![Part::from_bytes(Bytes::from_static(b"audio"), "audio/wav")],
        ),
    ]);
    for item in generate(m.as_ref(), req, false).await {
        let err = match item {
            Ok(resp) => panic!(
                "{name}: GenerateContent() error = <nil> (response {resp:?}), want wrapping ErrNoContent and ErrUnsupportedContent"
            ),
            Err(e) => e,
        };
        let ae = anthropic_error(&err);
        assert!(
            ae.is(ErrorKind::NoContent) && ae.is(ErrorKind::UnsupportedContent),
            "{name}: GenerateContent() error = {err}, want wrapping ErrNoContent and ErrUnsupportedContent"
        );
        assert!(
            err.to_string().contains("mime_type=audio/wav"),
            "{name}: error = {:?}, want diagnostic containing mime_type=audio/wav",
            err.to_string()
        );
    }
    assert_eq!(
        srv.hits(),
        before_count,
        "{name}: network was called ({before_count} -> {}) when newest message was empty",
        srv.hits()
    );
    assert_eq!(
        logs.events().len(),
        1,
        "{name}: expected 1 WARN log, got {}",
        logs.events().len()
    );
}

async fn multimodal_earlier_unsupported_message_omitted_and_session_continues(srv: &FakeServer) {
    let name = "earlier_unsupported_message_omitted_and_session_continues";
    let logs = LogCapture::new();
    let _guard = logs.set_default();
    let m = new_backend_model(srv, Backend::Anthropic).await;

    let req = contents_request(vec![
        content(
            ROLE_USER,
            vec![Part::from_bytes(Bytes::from_static(b"audio"), "audio/wav")],
        ),
        Content::from_text("Valid follow-up question", ROLE_USER),
    ]);
    generate_ok(m.as_ref(), req, false).await;
    assert_eq!(
        logs.events().len(),
        1,
        "{name}: expected 1 WARN log, got {}",
        logs.events().len()
    );
}

async fn multimodal_function_response_media_parts(srv: &FakeServer) {
    let name = "function_response_media_parts";
    let logs = LogCapture::new();
    let _guard = logs.set_default();
    let m = new_backend_model(srv, Backend::Anthropic).await;

    let req = contents_request(vec![
        Content::from_text("Render chart", ROLE_USER),
        content(
            ROLE_MODEL,
            vec![function_call_part("toolu_chart", "render_chart")],
        ),
        content(
            ROLE_USER,
            vec![function_response_part(
                "toolu_chart",
                "render_chart",
                json!({"status": "done"}),
                vec![
                    fr_inline("image/png", sample_png()),
                    fr_inline("application/pdf", Bytes::from_static(b"%PDF-1.4")),
                    fr_file("image/png", "https://example.com/chart.png"),
                    fr_file("application/pdf", "gs://bucket/chart.pdf"),
                    fr_inline("audio/ogg", Bytes::from_static(b"audio")),
                ],
            )],
        ),
    ]);
    generate_ok(m.as_ref(), req, false).await;
    assert_eq!(
        logs.events().len(),
        2,
        "{name}: expected 2 WARN logs for unsupported gs:// URI and audio in FunctionResponse.Parts, got {}",
        logs.events().len()
    );
    let last_body = srv.last_request().json();
    let tr_content = &last_body["messages"][2]["content"][0]["content"];
    assert_eq!(
        len_of(tr_content),
        4,
        "{name}: tool_result content has {} blocks, want 4 (text, inline image, inline pdf, https image URL)",
        len_of(tr_content)
    );
    let url_img_block = &tr_content[3];
    let url_img_src = &url_img_block["source"];
    assert!(
        url_img_block["type"] == "image"
            && url_img_src["type"] == "url"
            && url_img_src["url"] == "https://example.com/chart.png",
        "{name}: tool_result block[3] = {url_img_block}, want url image block"
    );
}

// parity: adapted: Go's per-model slog handler is a per-case tracing LogCapture (D10; the model takes no Logger); Vertex's TokenSource is static_token_credentials
#[tokio::test]
async fn test_multimodal_support() {
    let srv = FakeServer::json(text_message_json("msg_m", "ok", 10, 1));

    for tc in multimodal_cases() {
        for backend in [Backend::Anthropic, Backend::Vertex] {
            multimodal_table_case(&srv, &tc, backend).await;
        }
    }

    multimodal_newest_message_empty_after_skipping_unsupported_fails_without_network_call(&srv)
        .await;
    multimodal_earlier_unsupported_message_omitted_and_session_continues(&srv).await;
    multimodal_function_response_media_parts(&srv).await;
}

// -------------------------------------------- TestFunctionResponseStructSerializationAndEmptyResult

// parity: adapted: Go's typed []catalogItem{Name, Price} under "result" is the JSON value it marshals to (gadk FunctionResponse.response is a serde_json map)
#[tokio::test]
async fn test_function_response_struct_serialization_and_empty_result() {
    let srv = FakeServer::json(text_message_json("msg_fr", "ok", 10, 1));
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    let req = contents_request(vec![
        Content::from_text("List items and ping", ROLE_USER),
        content(
            ROLE_MODEL,
            vec![
                function_call_part("toolu_items", "list_items"),
                function_call_part("toolu_empty", "noop"),
            ],
        ),
        content(
            ROLE_USER,
            vec![
                function_response_part(
                    "toolu_items",
                    "list_items",
                    json!({
                        "result": [
                            {"name": "apple", "price": 3},
                            {"name": "pear", "price": 4},
                        ],
                    }),
                    Vec::new(),
                ),
                function_response_part("toolu_empty", "noop", json!({}), Vec::new()),
            ],
        ),
    ]);
    generate_ok(m.as_ref(), req, false).await;

    let last_body = srv.last_request().json();
    let user_blocks = &last_body["messages"][2]["content"];
    assert_eq!(
        len_of(user_blocks),
        2,
        "len(userBlocks) = {}, want 2",
        len_of(user_blocks)
    );

    // 1. Typed slice of structs under "result" must serialize as JSON, not Go fmt.Sprint syntax.
    let tr_items_content = &user_blocks[0]["content"];
    assert_eq!(
        len_of(tr_items_content),
        1,
        "trItems content = {tr_items_content}, want 1 text block"
    );
    let got_text = tr_items_content[0]["text"].as_str().unwrap_or("");
    let want_json = r#"[{"name":"apple","price":3},{"name":"pear","price":4}]"#;
    assert_eq!(
        got_text, want_json,
        "tool_result text = {got_text:?}, want {want_json:?}"
    );

    // 2. Empty FunctionResponse must omit content on tool_result rather than sending
    // [{"type":"text","text":""}].
    let tr_empty = &user_blocks[1];
    if let Some(content_val) = tr_empty.get("content") {
        panic!(
            "empty FunctionResponse tool_result had content = {content_val}, want content omitted"
        );
    }
}

// ----------------------------------------------------------- TestExecutableCodeAndURISchemePrivacy

// parity: adapted: Go's per-model slog handlers are tracing LogCaptures (D10; the model takes no Logger)
#[tokio::test]
async fn test_executable_code_and_uri_scheme_privacy() {
    let srv = FakeServer::json(text_message_json("msg_ec", "ok", 5, 1));

    {
        let log_handler = LogCapture::new();
        let _guard = log_handler.set_default();
        let m = new_model(MODEL, anthropic_config(&srv.url())).await;

        let req = contents_request(vec![content(
            ROLE_USER,
            vec![
                Part::from_uri(
                    "reports/q3-acquisition-plan.pdf?next=https://x.example/y",
                    "application/pdf",
                ),
                Part {
                    executable_code: Some(ExecutableCode {
                        language: Some(Language::Python),
                        code: "print(1)".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Part {
                    executable_code: Some(ExecutableCode {
                        language: Some(Language::Unspecified),
                        code: "echo hi".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Part {
                    code_execution_result: Some(CodeExecutionResult {
                        outcome: Some(Outcome::Ok),
                        output: "1".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        )]);
        generate_ok(m.as_ref(), req, false).await;

        // Verify relative path with query string containing :// logs uri_scheme="unknown"
        // without leaking path.
        let records = log_handler.events();
        assert_eq!(
            records.len(),
            1,
            "logged {} records, want 1: {:?}",
            records.len(),
            log_handler.rendered()
        );
        assert_eq!(
            records[0].field("uri_scheme").unwrap_or(""),
            "unknown",
            "uri_scheme = {:?}, want \"unknown\"",
            records[0].field("uri_scheme")
        );
        for (k, v) in &records[0].fields {
            assert!(
                !(v.contains("reports") || v.contains("q3-acquisition")),
                "log attr {k}={v:?} leaked file path"
            );
        }

        // Verify ExecutableCode and CodeExecutionResult fenced blocks place opening fence on a
        // new line.
        let last_body = srv.last_request().json();
        let blocks = &last_body["messages"][0]["content"];
        assert_eq!(len_of(blocks), 3, "sent {} blocks, want 3", len_of(blocks));
        let b0 = blocks[0]["text"].as_str().unwrap_or("");
        let b1 = blocks[1]["text"].as_str().unwrap_or("");
        let b2 = blocks[2]["text"].as_str().unwrap_or("");
        assert_eq!(b0, "Code:\n```python\nprint(1)\n```", "block[0]");
        assert_eq!(b1, "Code:\n```\necho hi\n```", "block[1]");
        assert_eq!(b2, "Execution Result:\n```code_output\n1\n```", "block[2]");
    }

    // Verify malformed MIMEType (e.g. URL with query token and newline) and host:port URI
    // without :// are sanitized to "invalid" and "unknown" in both warnings and
    // ErrUnsupportedContent errors.
    let malicious_log_handler = LogCapture::new();
    let _guard = malicious_log_handler.set_default();
    let m_malicious = new_model(MODEL, anthropic_config(&srv.url())).await;
    let malicious_req = contents_request(vec![content(
        ROLE_USER,
        vec![
            Part::from_bytes(
                Bytes::from_static(b"payload"),
                "https://evil.example/x?token=s3cret\ninjected",
            ),
            Part::from_uri(
                "my-secret-host.internal:8443/path?tok=s3cret",
                "application/pdf",
            ),
            Part::from_uri(
                "my-secret-host.internal:8443/path?tok=s3cret\ninjected",
                "application/pdf",
            ),
            Part::from_uri("db-host:5432/x?tok=s3cret", "application/pdf"),
        ],
    )]);
    let got_malicious_err = generate(m_malicious.as_ref(), malicious_req, false)
        .await
        .into_iter()
        .next_back()
        .and_then(Result::err)
        .expect("GenerateContent(malicious mime/uri) error = <nil>, want ErrNoContent + ErrUnsupportedContent");
    let ae = anthropic_error(&got_malicious_err);
    assert!(
        ae.is(ErrorKind::NoContent) && ae.is(ErrorKind::UnsupportedContent),
        "GenerateContent(malicious mime/uri) error = {got_malicious_err}, want ErrNoContent + ErrUnsupportedContent"
    );
    let msg = got_malicious_err.to_string();
    assert!(
        !(msg.contains("evil.example")
            || msg.contains("s3cret")
            || msg.contains("my-secret-host")
            || msg.contains("db-host")
            || msg.contains('\n')),
        "error string {msg:?} leaked untrusted MIMEType or FileURI content"
    );
    assert!(
        msg.contains("mime_type=invalid") && msg.contains("uri_scheme=unknown"),
        "error string = {msg:?}, want mime_type=invalid and uri_scheme=unknown"
    );
    let malicious_records = malicious_log_handler.events();
    assert_eq!(
        malicious_records.len(),
        4,
        "logged {} records, want 4: {:?}",
        malicious_records.len(),
        malicious_log_handler.rendered()
    );
    assert_eq!(
        malicious_records[0].field("mime_type"),
        Some("invalid"),
        "record[0] mime_type"
    );
    assert_eq!(
        malicious_records[1].field("uri_scheme"),
        Some("unknown"),
        "record[1] uri_scheme"
    );
    assert_eq!(
        malicious_records[2].field("uri_scheme"),
        Some("unknown"),
        "record[2] uri_scheme"
    );
    assert_eq!(
        malicious_records[3].field("uri_scheme"),
        Some("unknown"),
        "record[3] uri_scheme"
    );
}

// ------------------------------------------------------------- TestOutputSchemaAndStructuredOutput

/// Go's handler for the main server: SSE when the request streams, JSON otherwise.
fn weather_server() -> FakeServer {
    FakeServer::start(|req| {
        if req.is_stream() {
            return FakeResponse::sse(sse_lines(&[
                r#"event: message_start"#,
                r#"data: {"type":"message_start","message":{"id":"msg_os_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
                r#""#,
                r#"event: content_block_start"#,
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"{\"city\":"}}"#,
                r#""#,
                r#"event: content_block_delta"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\"Warsaw\",\"temp_c\":18}"}}"#,
                r#""#,
                r#"event: content_block_stop"#,
                r#"data: {"type":"content_block_stop","index":0}"#,
                r#""#,
                r#"event: message_delta"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":10}}"#,
                r#""#,
                r#"event: message_stop"#,
                r#"data: {"type":"message_stop"}"#,
                r#""#,
            ]));
        }
        FakeResponse::json(text_message_json(
            "msg_os",
            r#"{"city":"Warsaw","temp_c":18}"#,
            10,
            10,
        ))
    })
}

/// Go's `arraySrv` in `non_object_root_schema_wrapped_and_unwrapped`.
fn city_list_server() -> FakeServer {
    FakeServer::start(|req| {
        if req.is_stream() {
            return FakeResponse::sse(sse_lines(&[
                r#"event: message_start"#,
                r#"data: {"type":"message_start","message":{"id":"msg_arr_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":6,"output_tokens":1}}}"#,
                r#""#,
                r#"event: content_block_start"#,
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"{\"result\":"}}"#,
                r#""#,
                r#"event: content_block_delta"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"[\"Warsaw\",\"Krakow\"]}"}}"#,
                r#""#,
                r#"event: content_block_stop"#,
                r#"data: {"type":"content_block_stop","index":0}"#,
                r#""#,
                r#"event: message_delta"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":8}}"#,
                r#""#,
                r#"event: message_stop"#,
                r#"data: {"type":"message_stop"}"#,
                r#""#,
            ]));
        }
        FakeResponse::json(text_message_json(
            "msg_arr",
            r#"{"result":["Warsaw","Krakow"]}"#,
            6,
            8,
        ))
    })
}

/// The single text of a response with exactly one part, or `None`.
fn single_text(resp: Option<&LlmResponse>) -> Option<&str> {
    let parts = &resp?.content.as_ref()?.parts;
    (parts.len() == 1).then(|| parts[0].text.as_str())
}

async fn output_schema_processor_request_shape(srv: &FakeServer, m: &dyn Llm) {
    let name = "output_schema_processor_request_shape";
    // Exact shape produced by internal/llminternal/basic_processor.go:53-56.
    let req = text_request_with(
        "Return JSON",
        GenerateContentConfig {
            response_mime_type: "application/json".to_string(),
            response_schema: Some(city_temp_schema()),
            ..Default::default()
        },
    );

    let got = generate_last(m, req.clone(), false).await;
    assert!(
        got.content.as_ref().is_some_and(|c| c.parts.len() == 1),
        "{name}: unexpected response: {got:?}"
    );
    let last_body = srv.last_request().json();
    let format_map = &last_body["output_config"]["format"];
    assert_eq!(
        format_map["type"], "json_schema",
        "{name}: output_config.format.type = {}, want \"json_schema\"",
        format_map["type"]
    );
    let schema_map = &format_map["schema"];
    assert!(
        schema_map["type"] == "object" && schema_map["additionalProperties"] == false,
        "{name}: output_config.format.schema = {schema_map}, want type=object additionalProperties=false"
    );

    // Object root schema (unwrapped) streams partial text deltas as usual.
    let mut partial_texts: Vec<String> = Vec::new();
    let mut final_stream_resp: Option<LlmResponse> = None;
    for resp in generate_ok(m, req, true).await {
        if resp.partial {
            for p in resp.content.iter().flat_map(|c| c.parts.iter()) {
                partial_texts.push(p.text.clone());
            }
        } else {
            final_stream_resp = Some(resp);
        }
    }
    let want = r#"{"city":"Warsaw","temp_c":18}"#;
    assert!(
        partial_texts.len() == 2 && partial_texts.concat() == want,
        "{name}: object schema partialTexts = {partial_texts:?}, want 2 chunks joining to {want:?}"
    );
    assert_eq!(
        single_text(final_stream_resp.as_ref()),
        Some(want),
        "{name}: object schema finalStreamResp = {final_stream_resp:?}, want {want:?}"
    );
}

async fn output_schema_response_json_schema_with_nested_objects_and_effort(srv: &FakeServer) {
    let name = "response_json_schema_with_nested_objects_and_effort";
    let m_effort = new_model(
        MODEL,
        ClientConfig {
            effort: Some(Effort::XHigh),
            ..anthropic_config(&srv.url())
        },
    )
    .await;

    let req = text_request_with(
        "Return nested JSON",
        GenerateContentConfig {
            response_json_schema: Some(json!({
                "type": "OBJECT",
                "properties": {
                    "items": {
                        "type": "ARRAY",
                        "items": {
                            "type": "OBJECT",
                            "properties": {
                                "name": {"type": "STRING"},
                            },
                        },
                    },
                },
            })),
            ..Default::default()
        },
    );
    generate_ok(m_effort.as_ref(), req, false).await;
    let last_body = srv.last_request().json();
    let out_cfg = &last_body["output_config"];
    assert_eq!(
        out_cfg["effort"], "xhigh",
        "{name}: output_config.effort = {}, want \"xhigh\"",
        out_cfg["effort"]
    );
    let item_schema = &out_cfg["format"]["schema"]["properties"]["items"]["items"];
    assert!(
        item_schema["type"] == "object" && item_schema["additionalProperties"] == false,
        "{name}: nested item schema = {item_schema}, want type=object additionalProperties=false"
    );
}

async fn output_schema_non_object_root_schema_wrapped_and_unwrapped() {
    let name = "non_object_root_schema_wrapped_and_unwrapped";
    let array_srv = city_list_server();
    let m_array = new_model(MODEL, anthropic_config(&array_srv.url())).await;

    let req = text_request_with(
        "List two Polish cities",
        GenerateContentConfig {
            response_mime_type: "application/json".to_string(),
            response_schema: Some(string_array_schema()),
            ..Default::default()
        },
    );

    for stream_mode in [false, true] {
        let mut partial_resps: Vec<LlmResponse> = Vec::new();
        let mut final_resp: Option<LlmResponse> = None;
        for resp in generate(m_array.as_ref(), req.clone(), stream_mode).await {
            let resp = resp.unwrap_or_else(|e| {
                panic!("{name}: GenerateContent(stream={stream_mode}) error = {e}")
            });
            if resp.partial {
                partial_resps.push(resp);
            } else {
                final_resp = Some(resp);
            }
        }
        assert!(
            partial_resps.is_empty(),
            "{name}: stream={stream_mode} emitted {} partial responses for wrapped schema ({partial_resps:?}), want 0",
            partial_resps.len()
        );
        let wrapped_req_body = array_srv.last_request().json();
        let schema_map = &wrapped_req_body["output_config"]["format"]["schema"];
        assert!(
            schema_map["type"] == "object" && schema_map["additionalProperties"] == false,
            "{name}: stream={stream_mode} wrapped schema = {schema_map}, want root type=object additionalProperties=false"
        );
        let result_schema = &schema_map["properties"]["result"];
        assert_eq!(
            result_schema["type"], "array",
            "{name}: stream={stream_mode} wrapped properties.result = {result_schema}, want type=array"
        );
        assert!(
            final_resp
                .as_ref()
                .and_then(|r| r.content.as_ref())
                .is_some_and(|c| c.parts.len() == 1),
            "{name}: stream={stream_mode} unexpected final response: {final_resp:?}"
        );
        let got_text = single_text(final_resp.as_ref()).unwrap_or("");
        assert_eq!(
            got_text, r#"["Warsaw","Krakow"]"#,
            "{name}: stream={stream_mode} unwrapped text"
        );
    }

    // Verify wrapped non-object root schema hoists $defs and definitions to the wrapper root
    // and rewrites root-relative $ref pointers (# and #/...) while preserving #/$defs/... and
    // #/definitions/... pointers.
    let defs_req = text_request_with(
        "List cities",
        GenerateContentConfig {
            response_mime_type: "application/json".to_string(),
            response_json_schema: Some(json!({
                "type": "array",
                "items": {
                    "anyOf": [
                        {"type": "string"},
                        {"$ref": "#/$defs/CityAlias"},
                        {"$ref": "#/definitions/LegacyCity"},
                        {"$ref": "#"},
                    ],
                },
                "additionalItems": {
                    "$ref": "#/items",
                },
                "$defs": {
                    "CityAlias": {"type": "string"},
                },
                "definitions": {
                    "LegacyCity": {"type": "string"},
                },
            })),
            ..Default::default()
        },
    );
    for item in generate(m_array.as_ref(), defs_req, false).await {
        if let Err(e) = item {
            panic!("{name}: GenerateContent(wrapped schema with $defs and $ref) error = {e}");
        }
    }
    let wrapped_req_body = array_srv.last_request().json();
    let schema_map = &wrapped_req_body["output_config"]["format"]["schema"];
    assert!(
        schema_map["$defs"].is_object(),
        "{name}: expected $defs hoisted to wrapped schema root, got {schema_map}"
    );
    assert!(
        schema_map["definitions"].is_object(),
        "{name}: expected definitions hoisted to wrapped schema root, got {schema_map}"
    );
    let result_schema = &schema_map["properties"]["result"];
    assert!(
        result_schema.get("$defs").is_none(),
        "{name}: expected $defs removed from properties.result, got {result_schema}"
    );
    assert!(
        result_schema.get("definitions").is_none(),
        "{name}: expected definitions removed from properties.result, got {result_schema}"
    );
    assert_eq!(
        result_schema["additionalItems"]["$ref"], "#/properties/result/items",
        "{name}: additionalItems.$ref"
    );
    let any_of = &result_schema["items"]["anyOf"];
    assert_eq!(
        len_of(any_of),
        4,
        "{name}: len(anyOf) = {}, want 4",
        len_of(any_of)
    );
    assert_eq!(
        any_of[1]["$ref"], "#/$defs/CityAlias",
        "{name}: anyOf[1].$ref"
    );
    assert_eq!(
        any_of[2]["$ref"], "#/definitions/LegacyCity",
        "{name}: anyOf[2].$ref"
    );
    assert_eq!(
        any_of[3]["$ref"], "#/properties/result",
        "{name}: anyOf[3].$ref"
    );
}

async fn output_schema_empty_response_json_schema_rejected(m: &dyn Llm) {
    let name = "empty_response_json_schema_rejected";
    let req = text_request_with(
        "Return JSON",
        GenerateContentConfig {
            response_json_schema: Some(json!({})),
            ..Default::default()
        },
    );
    for item in generate(m, req, false).await {
        let is_invalid_config = match &item {
            Ok(_) => false,
            Err(e) => anthropic_error(e).is(ErrorKind::InvalidConfig),
        };
        assert!(
            is_invalid_config,
            "{name}: empty ResponseJsonSchema error = {:?}, want ErrInvalidConfig",
            item.err().map(|e| e.to_string())
        );
    }
}

/// Go `llmagent.New(Config{OutputSchema: schema, Model: m})` + `testutil.NewTestAgentRunner` +
/// `CollectTextParts(tr.Run(t, session, msg))`: the request the agent's basic processor sends
/// for a tool-less agent with an output schema (`basic_processor.go:53-56`) and the text of the
/// model's final responses.
async fn run_output_schema_agent(m: &dyn Llm, schema: Schema, msg: &str) -> Vec<String> {
    let req = text_request_with(
        msg,
        GenerateContentConfig {
            response_schema: Some(schema),
            response_mime_type: "application/json".to_string(),
            ..Default::default()
        },
    );
    collect_text_parts(&generate_ok(m, req, false).await)
}

async fn output_schema_llmagent_with_output_schema_end_to_end(m: &dyn Llm) {
    let name = "llmagent_with_output_schema_end_to_end";
    let texts = run_output_schema_agent(m, city_temp_schema(), "Weather in Warsaw?").await;
    assert!(
        !texts.is_empty() && texts.concat().contains("Warsaw"),
        "{name}: unexpected texts = {texts:?}, want JSON containing Warsaw"
    );

    // Non-object (array) OutputSchema on llmagent end-to-end.
    let array_srv = FakeServer::json(text_message_json(
        "msg_arr_agent",
        r#"{"result":["Warsaw","Krakow"]}"#,
        6,
        8,
    ));
    let m_array = new_model(MODEL, anthropic_config(&array_srv.url())).await;
    let array_texts = run_output_schema_agent(
        m_array.as_ref(),
        string_array_schema(),
        "List two Polish cities",
    )
    .await;
    assert_eq!(
        array_texts.concat(),
        r#"["Warsaw","Krakow"]"#,
        "{name}: array OutputSchema agent output"
    );
}

// parity: adapted: llmagent_with_output_schema_end_to_end drives the model with the request llmagent's basic processor builds for a tool-less OutputSchema agent (gadk-agent is not a dependency of gadk-anthropic; the runner path is Task 13's TestAgentRunner port)
#[tokio::test]
async fn test_output_schema_and_structured_output() {
    let srv = weather_server();
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    output_schema_processor_request_shape(&srv, m.as_ref()).await;
    output_schema_response_json_schema_with_nested_objects_and_effort(&srv).await;
    output_schema_non_object_root_schema_wrapped_and_unwrapped().await;
    output_schema_empty_response_json_schema_rejected(m.as_ref()).await;
    output_schema_llmagent_with_output_schema_end_to_end(m.as_ref()).await;
}
