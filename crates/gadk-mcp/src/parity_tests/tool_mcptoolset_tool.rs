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

//! Port of Go `tool/mcptoolset/tool_test.go` (package `mcptoolset`; Task 3).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pretty_assertions::assert_eq;
use rmcp::model::{CallToolResult, ContentBlock, Resource, ResourceContents};
use serde_json::{Value, json};

use crate::format::format_mcp_content;
use crate::go_mime::has_mime_charset_parameter;

/// Go's `[]byte` payload as rmcp carries it: standard base64.
fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn text(t: &str) -> ContentBlock {
    ContentBlock::text(t)
}

/// Go `&mcp.EmbeddedResource{Resource: &mcp.ResourceContents{URI, MIMEType, Text}}`.
fn embedded_text(uri: &str, mime_type: &str, text: &str) -> ContentBlock {
    ContentBlock::resource(ResourceContents::text(text, uri).with_mime_type(mime_type))
}

/// Go `&mcp.EmbeddedResource{Resource: &mcp.ResourceContents{URI, MIMEType, Blob}}`.
fn embedded_blob(uri: &str, mime_type: &str, blob: &[u8]) -> ContentBlock {
    ContentBlock::resource(ResourceContents::blob(b64(blob), uri).with_mime_type(mime_type))
}

fn success(content: Vec<ContentBlock>) -> CallToolResult {
    CallToolResult::success(content)
}

/// The result shaping of Go `mcpTool.Run` after `CallTool` returns `result` (spec R6):
/// `IsError` → `Tool execution failed.` (+ ` Details: <formatted>`), else a non-null
/// `StructuredContent` → `{"output": structured}`, else `{"output": formatMCPContent(Content)}`.
fn run_shape(result: &CallToolResult) -> Result<Value, String> {
    if result.is_error == Some(true) {
        let details = format_mcp_content(&result.content);
        let mut msg = "Tool execution failed.".to_owned();
        if !details.is_empty() {
            msg += " Details: ";
            msg += &details;
        }
        return Err(msg);
    }
    if let Some(structured) = result.structured_content.as_ref().filter(|v| !v.is_null()) {
        return Ok(json!({ "output": structured }));
    }
    Ok(json!({ "output": format_mcp_content(&result.content) }))
}

// parity: adapted: McpTool::run is Task 5's, so each row runs format_mcp_content through Go Run's result shaping (run_shape); 7 rows rmcp's ContentBlock cannot represent are noted in the table
#[test]
fn test_mcp_tool_run_content() {
    let resource_size: u64 = 1 << 20;
    let tests: Vec<(&str, CallToolResult, Result<Value, &str>)> = vec![
        (
            "text only remains compatible",
            success(vec![text("first"), text("second")]),
            Ok(json!({"output": "firstsecond"})),
        ),
        (
            "GitHub file response includes embedded text",
            success(vec![
                text("successfully downloaded text file"),
                embedded_text(
                    "repo://owner/project/main/file.go",
                    "text/plain",
                    "package example\n",
                ),
            ]),
            Ok(json!({"output": "successfully downloaded text file\n\
                [MCP embedded resource: uri=\"repo://owner/project/main/file.go\", mimeType=\"text/plain\"]\n\
                package example\n"})),
        ),
        (
            "multiline embedded body is separated from following text",
            success(vec![
                embedded_blob("file:///multiline.txt", "text/plain", b"line1\nline2"),
                text("tail"),
            ]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///multiline.txt\", \
                mimeType=\"text/plain\"]\nline1\nline2\ntail"}),
            ),
        ),
        // Row "embedded text reports an accompanying blob" is not ported: rmcp's
        // ResourceContents is either text or blob, so a resource cannot carry both.
        (
            "text MIME blob is decoded",
            success(vec![embedded_blob(
                "file:///response.json",
                "application/problem+json; charset=utf-8",
                br#"{"status":"ok"}"#,
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///response.json\", \
                mimeType=\"application/problem+json; charset=utf-8\"]\n{\"status\":\"ok\"}"}),
            ),
        ),
        (
            "binary resource is represented by metadata",
            success(vec![embedded_blob(
                "file:///report.pdf",
                "application/pdf",
                &[1, 2, 3, 4],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///report.pdf\", \
                mimeType=\"application/pdf\", size=4 bytes]"}),
            ),
        ),
        (
            "empty blob is represented by metadata without size",
            success(vec![embedded_blob(
                "file:///empty.pdf",
                "application/pdf",
                &[],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///empty.pdf\", \
                mimeType=\"application/pdf\"]"}),
            ),
        ),
        (
            "empty text blob has no trailing newline",
            success(vec![embedded_blob("file:///empty.txt", "text/plain", &[])]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///empty.txt\", \
                mimeType=\"text/plain\"]"}),
            ),
        ),
        (
            "unsupported text charset is represented by metadata",
            success(vec![embedded_blob(
                "file:///utf16.txt",
                "text/plain; charset=utf-16",
                b"hi",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///utf16.txt\", \
                mimeType=\"text/plain; charset=utf-16\", size=2 bytes]"}),
            ),
        ),
        (
            "unsupported extended charset is represented by metadata",
            success(vec![embedded_blob(
                "file:///utf16-star.txt",
                "text/plain; charset*=utf-16",
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///utf16-star.txt\", \
                mimeType=\"text/plain; charset*=utf-16\", size=4 bytes]"}),
            ),
        ),
        (
            "unterminated quoted parameter is represented by metadata",
            success(vec![embedded_blob(
                "file:///unterminated.txt",
                r#"text/plain; name="a; charset=utf-16"#,
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///unterminated.txt\", \
                mimeType=\"text/plain; name=\\\"a; charset=utf-16\", size=4 bytes]"}),
            ),
        ),
        (
            "mid-token quotes are represented by metadata",
            success(vec![embedded_blob(
                "file:///mid-token.txt",
                r#"text/plain; name=a"; charset=utf-16; x="b"#,
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///mid-token.txt\", ".to_owned()
                + r#"mimeType="text/plain; name=a\"; charset=utf-16; x=\"b", size=4 bytes]"#}),
            ),
        ),
        (
            "quoted charset name is represented by metadata",
            success(vec![embedded_blob(
                "file:///quoted-name.txt",
                r#"text/plain; "charset"=utf-16"#,
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///quoted-name.txt\", ".to_owned()
                + r#"mimeType="text/plain; \"charset\"=utf-16", size=4 bytes]"#}),
            ),
        ),
        (
            "empty parameter name before quote is represented by metadata",
            success(vec![embedded_blob(
                "file:///empty-name.txt",
                r#"text/plain; ="a; charset=utf-16""#,
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///empty-name.txt\", ".to_owned()
                + r#"mimeType="text/plain; =\"a; charset=utf-16\"", size=4 bytes]"#}),
            ),
        ),
        (
            "invalid parameter name before quote is represented by metadata",
            success(vec![embedded_blob(
                "file:///invalid-name.txt",
                r#"text/plain; bad name="a; charset=utf-16""#,
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///invalid-name.txt\", ".to_owned()
                + r#"mimeType="text/plain; bad name=\"a; charset=utf-16\"", size=4 bytes]"#}),
            ),
        ),
        (
            "carriage return inside quoted value is represented by metadata",
            success(vec![embedded_blob(
                "file:///quoted-cr.txt",
                "text/plain; name=\"a\r; charset=utf-16\"",
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///quoted-cr.txt\", ".to_owned()
                + r#"mimeType="text/plain; name=\"a\r; charset=utf-16\"", size=4 bytes]"#}),
            ),
        ),
        (
            "line feed inside quoted value is represented by metadata",
            success(vec![embedded_blob(
                "file:///quoted-lf.txt",
                "text/plain; name=\"a\n; charset=utf-16\"",
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///quoted-lf.txt\", ".to_owned()
                + r#"mimeType="text/plain; name=\"a\n; charset=utf-16\"", size=4 bytes]"#}),
            ),
        ),
        (
            "charset text inside quoted value does not block decoding",
            success(vec![embedded_blob(
                "file:///quoted-value.txt",
                r#"text/plain; name="foo;charset=utf-16" invalid"#,
                b"hello",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///quoted-value.txt\", ".to_owned()
                + r#"mimeType="text/plain; name=\"foo;charset=utf-16\" invalid"]"#
                + "\nhello"}),
            ),
        ),
        (
            "US-ASCII text blob is decoded",
            success(vec![embedded_blob(
                "file:///ascii.txt",
                "text/plain; charset=us-ascii",
                b"hello",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///ascii.txt\", \
                mimeType=\"text/plain; charset=us-ascii\"]\nhello"}),
            ),
        ),
        (
            "ASCII alias text blob is decoded",
            success(vec![embedded_blob(
                "file:///ascii-alias.txt",
                "text/plain; charset=ascii",
                b"hello",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///ascii-alias.txt\", \
                mimeType=\"text/plain; charset=ascii\"]\nhello"}),
            ),
        ),
        (
            "form URL encoded blob is decoded",
            success(vec![embedded_blob(
                "file:///form.txt",
                "application/x-www-form-urlencoded",
                b"name=alice&active=true",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///form.txt\", \
                mimeType=\"application/x-www-form-urlencoded\"]\nname=alice&active=true"}),
            ),
        ),
        (
            "non-ASCII byte is represented by metadata",
            success(vec![embedded_blob(
                "file:///non-ascii.txt",
                "text/plain; charset=us-ascii",
                &[0x80],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///non-ascii.txt\", \
                mimeType=\"text/plain; charset=us-ascii\", size=1 bytes]"}),
            ),
        ),
        (
            "malformed charset is represented by metadata",
            success(vec![embedded_blob(
                "file:///utf16.txt",
                r#"text/plain; charset="utf-16"#,
                &[0x68, 0x00, 0x69, 0x00],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///utf16.txt\", \
                mimeType=\"text/plain; charset=\\\"utf-16\", size=4 bytes]"}),
            ),
        ),
        (
            "text blob with malformed unrelated parameter is decoded",
            success(vec![embedded_blob(
                "file:///notes.txt",
                "text/plain; name=my file.txt",
                b"hello",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///notes.txt\", \
                mimeType=\"text/plain; name=my file.txt\"]\nhello"}),
            ),
        ),
        (
            "JSON blob with malformed unrelated parameter is decoded",
            success(vec![embedded_blob(
                "file:///response.json",
                "application/json; profile=https://example.com/schema",
                br#"{"status":"ok"}"#,
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///response.json\", \
                mimeType=\"application/json; profile=https://example.com/schema\"]\n{\"status\":\"ok\"}"}),
            ),
        ),
        (
            "charset-like parameter name does not block decoding",
            success(vec![embedded_blob(
                "file:///notes.txt",
                "text/plain; x-charset-note=",
                b"hello",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///notes.txt\", \
                mimeType=\"text/plain; x-charset-note=\"]\nhello"}),
            ),
        ),
        (
            "malformed media type is represented by metadata",
            success(vec![embedded_blob(
                "file:///invalid.txt",
                "text/plain extra",
                b"hello",
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///invalid.txt\", \
                mimeType=\"text/plain extra\", size=5 bytes]"}),
            ),
        ),
        (
            "invalid UTF-8 text blob is represented by metadata",
            success(vec![embedded_blob(
                "file:///invalid.txt",
                "text/plain",
                &[0xc3, 0x28],
            )]),
            Ok(
                json!({"output": "[MCP embedded resource: uri=\"file:///invalid.txt\", \
                mimeType=\"text/plain\", size=2 bytes]"}),
            ),
        ),
        (
            "resource link includes available metadata",
            success(vec![ContentBlock::resource_link(
                Resource::new("https://example.com/archive.txt", "archive.txt")
                    .with_title("Archive")
                    .with_description("Large text file")
                    .with_mime_type("text/plain")
                    .with_size(resource_size),
            )]),
            Ok(
                json!({"output": "[MCP resource link: uri=\"https://example.com/archive.txt\", \
                mimeType=\"text/plain\", name=\"archive.txt\", title=\"Archive\", \
                description=\"Large text file\", size=1048576 bytes]"}),
            ),
        ),
        (
            "resource link without metadata uses a plain label",
            success(vec![ContentBlock::resource_link(Resource::new("", ""))]),
            Ok(json!({"output": "[MCP resource link]"})),
        ),
        (
            "image and audio are represented in order",
            success(vec![
                text("media:"),
                ContentBlock::image(b64(&[1, 2, 3]), "image/png"),
                ContentBlock::audio(b64(&[4, 5]), "audio/wav"),
                text(""),
                text("done"),
            ]),
            Ok(
                json!({"output": "media:\n[MCP image: mimeType=\"image/png\", size=3 bytes]\n\
                [MCP audio: mimeType=\"audio/wav\", size=2 bytes]\ndone"}),
            ),
        ),
        (
            "image without MIME type omits MIME metadata",
            success(vec![ContentBlock::image(b64(&[1, 2]), "")]),
            Ok(json!({"output": "[MCP image: size=2 bytes]"})),
        ),
        (
            "empty leading block does not add a separator",
            success(vec![text(""), ContentBlock::image(b64(&[1]), "image/png")]),
            Ok(json!({"output": "[MCP image: mimeType=\"image/png\", size=1 bytes]"})),
        ),
        // Row "unsupported content is represented in order" is not ported: rmcp's ContentBlock
        // has no variant outside text/image/audio/resource/resource_link to build one from
        // (format_mcp_content still has Go's `[MCP content: unsupported]` arm).
        (
            "existing newline precedes non-text content",
            success(vec![
                text("downloaded\n"),
                ContentBlock::resource_link(Resource::new("https://example.com/file", "")),
            ]),
            Ok(json!({"output": "downloaded\n\
                [MCP resource link: uri=\"https://example.com/file\"]"})),
        ),
        (
            "existing newline follows non-text content",
            success(vec![
                ContentBlock::image(b64(&[1]), "image/png"),
                text("\nnotes"),
            ]),
            Ok(json!({"output": "[MCP image: mimeType=\"image/png\", size=1 bytes]\nnotes"})),
        ),
        (
            "structured output takes precedence over mixed content",
            {
                let mut result = success(vec![
                    text("downloaded"),
                    embedded_text(
                        "repo://owner/project/file.txt",
                        "text/plain",
                        "file contents",
                    ),
                ]);
                result.structured_content = Some(json!({"sha": "abc123"}));
                result
            },
            Ok(json!({"output": {"sha": "abc123"}})),
        ),
        (
            "structured output with text only remains compatible",
            {
                let mut result = success(vec![text(r#"{"status":"ok"}"#)]);
                result.structured_content = Some(json!({"status": "ok"}));
                result
            },
            Ok(json!({"output": {"status": "ok"}})),
        ),
        (
            "error response includes non-text details",
            CallToolResult::error(vec![
                text("download failed"),
                ContentBlock::resource_link(
                    Resource::new("https://example.com/error", "").with_mime_type("text/plain"),
                ),
            ]),
            Err("Tool execution failed. Details: download failed\n\
                [MCP resource link: uri=\"https://example.com/error\", mimeType=\"text/plain\"]"),
        ),
        (
            "error response without details omits details suffix",
            CallToolResult::error(vec![text("")]),
            Err("Tool execution failed."),
        ),
        // Go's nil Content and empty Content are both an empty Vec here.
        (
            "nil content returns empty output",
            success(Vec::new()),
            Ok(json!({"output": ""})),
        ),
        (
            "empty content returns empty output",
            success(vec![]),
            Ok(json!({"output": ""})),
        ),
        (
            "all-empty text content returns empty output",
            success(vec![text(""), text("")]),
            Ok(json!({"output": ""})),
        ),
        // Rows "untyped nil content is unavailable", "untyped nil content is separated from
        // following text", "typed nil text content is separated from following text", "typed
        // nil content does not panic" and "nil resource does not panic" are not ported: rmcp's
        // ContentBlock (and EmbeddedResource's resource) cannot be nil.
    ];

    for (name, result, want) in tests {
        let want = want.map_err(str::to_owned);
        assert_eq!(run_shape(&result), want, "{name}");
    }
}

#[test]
fn test_has_mime_charset_parameter() {
    let tests: &[(&str, &str, bool)] = &[
        (
            "extended charset parameter",
            r#"text/plain; charset*=UTF-16''x; name="a"#,
            true,
        ),
        (
            "charset after malformed parameter",
            "text/plain; name=my file.txt; charset=utf-16",
            true,
        ),
        (
            "uppercase charset parameter",
            "text/plain; CHARSET=utf-16; name=a",
            true,
        ),
        (
            "uppercase extended charset after quoted parameter",
            r#"text/plain; name="a"; CHARSET*=UTF-16"#,
            true,
        ),
        (
            "backslash outside quoted parameter",
            r"text/plain; \; charset=utf-16",
            true,
        ),
        (
            "unterminated quoted parameter is ambiguous",
            r#"text/plain; name="a"#,
            true,
        ),
        (
            "mid-token quotes are ambiguous",
            r#"text/plain; name=a"; charset=utf-16; x="b"#,
            true,
        ),
        (
            "quoted charset name is ambiguous",
            r#"text/plain; "charset"=utf-16"#,
            true,
        ),
        (
            "empty parameter name before quote is ambiguous",
            r#"text/plain; ="a; charset=utf-16""#,
            true,
        ),
        (
            "invalid parameter name before quote is ambiguous",
            r#"text/plain; bad name="a; charset=utf-16""#,
            true,
        ),
        (
            "carriage return inside quoted value is ambiguous",
            "text/plain; name=\"a\r; charset=utf-16\"",
            true,
        ),
        (
            "line feed inside quoted value is ambiguous",
            "text/plain; name=\"a\n; charset=utf-16\"",
            true,
        ),
        (
            "backslash before carriage return inside quoted value is ambiguous",
            "text/plain; name=\"a\\\r; charset=utf-16\"",
            true,
        ),
        (
            "backslash before line feed inside quoted value is ambiguous",
            "text/plain; name=\"a\\\n; charset=utf-16\"",
            true,
        ),
        (
            "charset text inside quoted value",
            r#"text/plain; name="foo;charset=utf-16" invalid"#,
            false,
        ),
        (
            "whitespace before quoted value",
            "text/plain; name= \t\"foo;charset=utf-16\" invalid",
            false,
        ),
        (
            "quoted value after unrelated parameter",
            r#"text/plain; name=a; note="foo;charset=utf-16" invalid"#,
            false,
        ),
        (
            "equals inside quoted value",
            r#"text/plain; name="a=b;charset=utf-16" invalid"#,
            false,
        ),
        (
            "escaped quote inside quoted value",
            r#"text/plain; name="a\";charset=utf-16" invalid"#,
            false,
        ),
        (
            "extra equals before quote is ambiguous",
            r#"text/plain; name=a="; charset=utf-16; x="b"#,
            true,
        ),
        (
            "quote after closed value is ambiguous",
            r#"text/plain; name="a""; charset=utf-16; x="b"#,
            true,
        ),
    ];

    for &(name, mime_type, want) in tests {
        assert_eq!(
            has_mime_charset_parameter(mime_type),
            want,
            "{name}: has_mime_charset_parameter({mime_type:?})"
        );
    }
}
