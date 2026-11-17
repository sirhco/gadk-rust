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

//! MCP result content as model-facing text ([`format_mcp_content`]); the `[MCP …]` labels match the
//! text ADK servers and clients produce.
//!
//! rmcp keeps image, audio and blob payloads as base64 strings, so sizes here are the decoded
//! byte counts. The decoder uses the standard alphabet with padding required, ignores `\r` and
//! `\n`, and allows non-zero trailing bits. Invalid base64 does not fail the call: such a payload
//! counts as 0 bytes.

use std::borrow::Cow;
use std::collections::BTreeMap;

use base64::Engine as _;
use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, PAD};
use gadk_agent::go_quote;
use rmcp::model::{ContentBlock, ResourceContents};

use crate::go_mime::{
    MediaTypeError, go_to_lower, has_mime_charset_parameter, is_text_media_type, parse_media_type,
};

/// The base64 decoding configuration: standard alphabet, padding required, non-zero trailing bits
/// accepted.
const GO_STD_ENCODING: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    PAD.with_decode_allow_trailing_bits(true),
);

/// One rendered block.
struct FormattedBlock<'a> {
    text: Cow<'a, str>,
    is_plain: bool,
}

impl<'a> FormattedBlock<'a> {
    fn labelled(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            is_plain: false,
        }
    }
}

/// Formats MCP content: text as is; embedded resources, resource links, images and audio as
/// `[MCP …]` labels (with a decoded text body for a text resource or a text blob); blocks joined
/// by a newline unless both are plain text or the boundary already has a newline.
pub fn format_mcp_content(contents: &[ContentBlock]) -> String {
    let formatted: Vec<FormattedBlock<'_>> = contents
        .iter()
        .map(|content| match content {
            ContentBlock::Text(text) => FormattedBlock {
                text: Cow::Borrowed(text.text.as_str()),
                is_plain: true,
            },
            ContentBlock::Resource(embedded) => {
                FormattedBlock::labelled(format_embedded_resource(&embedded.resource))
            }
            ContentBlock::ResourceLink(link) => FormattedBlock::labelled(format_resource_link(
                &link.uri,
                link.mime_type.as_deref().unwrap_or(""),
                &link.name,
                link.title.as_deref().unwrap_or(""),
                link.description.as_deref().unwrap_or(""),
                link.size,
            )),
            ContentBlock::Image(image) => FormattedBlock::labelled(format_media_content(
                "image",
                &image.mime_type,
                decode_base64(&image.data).len(),
            )),
            ContentBlock::Audio(audio) => FormattedBlock::labelled(format_media_content(
                "audio",
                &audio.mime_type,
                decode_base64(&audio.data).len(),
            )),
            _ => FormattedBlock::labelled("[MCP content: unsupported]"),
        })
        .collect();

    let mut result = String::new();
    let mut previous: Option<&FormattedBlock<'_>> = None;
    for block in &formatted {
        if block.text.is_empty() {
            continue;
        }
        if let Some(prev) = previous
            && (!prev.is_plain || !block.is_plain)
            && !prev.text.ends_with('\n')
            && !block.text.starts_with('\n')
        {
            result.push('\n');
        }
        result.push_str(&block.text);
        previous = Some(block);
    }
    result
}

/// Formats an embedded resource. rmcp splits text and blob contents, so a resource never has both:
/// text contents with a non-empty `text` render label and body; with an empty `text` only the
/// label.
fn format_embedded_resource(resource: &ResourceContents) -> String {
    match resource {
        ResourceContents::TextResourceContents {
            uri,
            mime_type,
            text,
            ..
        } => {
            let attributes = resource_attributes(uri, mime_type.as_deref().unwrap_or(""));
            if !text.is_empty() {
                return format_content_with_body("embedded resource", &attributes, text);
            }
            format_content_label("embedded resource", &attributes)
        }
        ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } => {
            let mime_type = mime_type.as_deref().unwrap_or("");
            let mut attributes = resource_attributes(uri, mime_type);
            let blob = decode_base64(blob);
            if let Some(text) = decode_text_blob(&blob, mime_type) {
                return format_content_with_body("embedded resource", &attributes, &text);
            }
            if !blob.is_empty() {
                attributes.push(format!("size={} bytes", blob.len()));
            }
            format_content_label("embedded resource", &attributes)
        }
        // A resource kind this crate cannot read.
        _ => "[MCP embedded resource: unavailable]".to_owned(),
    }
}

/// Formats a resource link; empty attributes are omitted.
fn format_resource_link(
    uri: &str,
    mime_type: &str,
    name: &str,
    title: &str,
    description: &str,
    size: Option<u64>,
) -> String {
    let mut attributes = resource_attributes(uri, mime_type);
    if !name.is_empty() {
        attributes.push(format!("name={}", go_quote(name)));
    }
    if !title.is_empty() {
        attributes.push(format!("title={}", go_quote(title)));
    }
    if !description.is_empty() {
        attributes.push(format!("description={}", go_quote(description)));
    }
    if let Some(size) = size {
        attributes.push(format!("size={size} bytes"));
    }
    format_content_label("resource link", &attributes)
}

/// Formats an image or audio block: its optional MIME type and decoded size.
fn format_media_content(kind: &str, mime_type: &str, size: usize) -> String {
    let mut attributes = Vec::with_capacity(2);
    if !mime_type.is_empty() {
        attributes.push(format!("mimeType={}", go_quote(mime_type)));
    }
    attributes.push(format!("size={size} bytes"));
    format_content_label(kind, &attributes)
}

/// The `uri` and `mimeType` attributes of a resource, each omitted when empty.
fn resource_attributes(uri: &str, mime_type: &str) -> Vec<String> {
    let mut attributes = Vec::with_capacity(2);
    if !uri.is_empty() {
        attributes.push(format!("uri={}", go_quote(uri)));
    }
    if !mime_type.is_empty() {
        attributes.push(format!("mimeType={}", go_quote(mime_type)));
    }
    attributes
}

/// A label followed by a newline and `body`.
fn format_content_with_body(kind: &str, attributes: &[String], body: &str) -> String {
    format_content_label(kind, attributes) + "\n" + body
}

/// The `[MCP <kind>: <attributes>]` label, or `[MCP <kind>]` without attributes.
fn format_content_label(kind: &str, attributes: &[String]) -> String {
    if attributes.is_empty() {
        return format!("[MCP {kind}]");
    }
    format!("[MCP {kind}: {}]", attributes.join(", "))
}

/// The bytes of a base64 payload; invalid base64 is empty (0 bytes).
fn decode_base64(data: &str) -> Vec<u8> {
    let data: Cow<'_, str> = if data.contains(['\r', '\n']) {
        Cow::Owned(data.replace(['\r', '\n'], ""))
    } else {
        Cow::Borrowed(data)
    };
    GO_STD_ENCODING.decode(data.as_bytes()).unwrap_or_default()
}

/// Decodes `blob` as text when `mime_type` is a text type with an acceptable charset and the bytes
/// are valid UTF-8 (or ASCII for an ASCII charset).
pub(crate) fn decode_text_blob(blob: &[u8], mime_type: &str) -> Option<String> {
    if blob.is_empty() {
        return None;
    }

    let declares_charset = has_mime_charset_parameter(mime_type);
    let (media_type, params) = match parse_media_type(mime_type) {
        Ok(parsed) => parsed,
        // An invalid parameter is tolerated (media type, no parameters) unless the value declares a
        // charset.
        Err((MediaTypeError::InvalidParameter, media_type)) if !declares_charset => {
            (media_type, BTreeMap::new())
        }
        Err(_) => return None,
    };
    if !is_text_media_type(&media_type) {
        return None;
    }

    let charset = params.get("charset");
    if declares_charset && charset.is_none() {
        return None;
    }
    let charset = go_to_lower(charset.map_or("", String::as_str));
    match charset.as_str() {
        "" | "utf-8" | "utf8" => {}
        "us-ascii" | "ascii" => {
            if !blob.is_ascii() {
                return None;
            }
        }
        _ => return None,
    }
    std::str::from_utf8(blob).ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte counts Go 1.27 `base64.StdEncoding.DecodeString` gives; Go's errors are 0 here.
    #[test]
    fn decode_base64_matches_go_std_encoding() {
        for (input, want) in [
            ("", 0),
            ("AQID", 3),
            ("AQI=", 2),
            ("AQ==", 1),
            ("AQ", 0),
            ("AQ=", 0),
            ("AR==", 1),
            ("AQID\r\nBA==", 4),
            ("!!!!", 0),
            ("AQ==AQ==", 0),
            (" AQ==", 0),
        ] {
            assert_eq!(decode_base64(input).len(), want, "{input:?}");
        }
    }

    #[test]
    fn invalid_base64_counts_as_zero_bytes() {
        let contents = [
            ContentBlock::image("not base64!", "image/png"),
            ContentBlock::resource(
                ResourceContents::blob("not base64!", "file:///x.bin")
                    .with_mime_type("application/octet-stream"),
            ),
        ];
        assert_eq!(
            format_mcp_content(&contents),
            "[MCP image: mimeType=\"image/png\", size=0 bytes]\n\
             [MCP embedded resource: uri=\"file:///x.bin\", mimeType=\"application/octet-stream\"]"
        );
    }

    #[test]
    fn decode_text_blob_charset_rules() {
        assert_eq!(
            decode_text_blob(b"hi", "text/plain; charset=UTF-8").as_deref(),
            Some("hi")
        );
        assert_eq!(
            decode_text_blob(b"hi", "text/plain; charset=utf8").as_deref(),
            Some("hi")
        );
        assert_eq!(
            decode_text_blob(b"hi", "text/plain; charset*=utf-8''utf-8").as_deref(),
            Some("hi")
        );
        // A declared charset Go's parser cannot read blocks decoding.
        assert_eq!(
            decode_text_blob(b"hi", "text/plain; charset*=utf-16''x"),
            None
        );
        assert_eq!(decode_text_blob(b"hi", "application/pdf"), None);
        assert_eq!(decode_text_blob(b"", "text/plain"), None);
        assert_eq!(
            decode_text_blob("h\u{e9}".as_bytes(), "text/plain; charset=ascii"),
            None
        );
        assert_eq!(
            decode_text_blob("h\u{e9}".as_bytes(), "text/plain").as_deref(),
            Some("h\u{e9}")
        );
    }
}
