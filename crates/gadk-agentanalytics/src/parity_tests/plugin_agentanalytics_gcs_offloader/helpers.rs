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

//! `gcs_offloader_test.go`: the pure helpers (`extensionForMIME`, `isSafeExtension`,
//! `pathSegment`, `textPreview`, `objectRef`, `gcsMetadataDetails`), the default concurrency
//! and the offloading limits of `applyDefaults`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gadk_genai::Content;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::super::plugin_agentanalytics_batch_processor::support::{image_content, queued_row};
use super::{
    Barrier, MAX_PATH_SEGMENT_BYTES, PATH_SEGMENT_HASH_SUFFIX_LEN, as_off, bare, hooked, opts,
    path_segment_hash_suffix, resolve,
};
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GCS_UPLOAD_TIMEOUT};
use crate::context::Context;
use crate::error::Error;
use crate::formatter::object_ref;
use crate::offload::TEXT_PREVIEW_SUFFIX;
use crate::offload::uploads::{
    extension_for_mime, gcs_metadata_details, is_safe_extension, path_segment, text_preview,
};

// parity: adapted: gadk has no mime.AddExtensionType (it answers from Go's built-in MIME table, not the host's), so the two registered made-up types become two built-in types
#[test]
fn test_extension_for_mime() {
    let tests = [
        ("image/png", ".png"),
        ("image/jpeg", ".jpg"),
        ("image/gif", ".gif"),
        ("image/webp", ".webp"),
        ("application/pdf", ".pdf"),
        ("application/json", ".json"),
        ("audio/mpeg", ".mp3"),
        ("audio/wav", ".wav"),
        ("video/mp4", ".mp4"),
        ("video/webm", ".webm"),
        ("text/plain", ".txt"),
        // Case, blanks and parameters do not matter.
        ("IMAGE/PNG", ".png"),
        (" image/png ", ".png"),
        ("text/plain; charset=utf-8", ".txt"),
        ("image/jpeg;q=0.5", ".jpg"),
        // image/jpeg is where the table above differs from the answer of the host, so these
        // only pass if the table is asked with a clean type.
        ("IMAGE/JPEG", ".jpg"),
        (" image/jpeg ", ".jpg"),
        ("Image/Jpeg ; charset=x", ".jpg"),
        // Go's built-in table answers the other types it knows (the first sorted extension).
        ("image/svg+xml", ".svg"),
        ("text/html; charset=utf-8", ".ehtml"),
        // Types without an extension.
        ("application/x-no-such-type", ".bin"),
        ("not a mime type", ".bin"),
        ("", ".bin"),
    ];
    for (mime_type, want) in tests {
        assert_eq!(
            extension_for_mime(mime_type),
            want,
            "extensionForMIME({mime_type:?})"
        );
    }
}

#[test]
fn test_is_safe_extension() {
    let sixteen = format!(".{}", "a".repeat(16));
    let seventeen = format!(".{}", "a".repeat(17));
    let tests: [(&str, bool); 27] = [
        (".png", true),
        (".PNG", true),
        (".7z", true),
        (".a", true),
        (&sixteen, true),
        (&seventeen, false),
        ("", false),
        (".", false),
        ("png", false),
        (".tar.gz", false),
        (".p/ng", false),
        (".p ng", false),
        (".png\n", false),
        (".pñg", false),
        (".p_g", false),
        (".p-g", false),
        // The edges of the character classes, and the characters next to them.
        (".z", true),
        (".A", true),
        (".Z", true),
        (".0", true),
        (".9", true),
        (".`", false),
        (".{", false),
        (".@", false),
        (".[", false),
        ("./", false),
        (".:", false),
    ];
    for (ext, want) in tests {
        assert_eq!(is_safe_extension(ext), want, "isSafeExtension({ext:?})");
    }
}

#[test]
fn test_path_segment() {
    fn h(s: impl AsRef<str>) -> String {
        path_segment_hash_suffix(s.as_ref())
    }
    let a = |n: usize| "a".repeat(n);
    let tests: Vec<(&str, String, String)> = vec![
        (
            "a trace id",
            "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            "4bf92f3577b34da6a3ce929d0e0e4736".into(),
        ),
        (
            "all the safe characters",
            "aZ09-_.x".into(),
            "aZ09-_.x".into(),
        ),
        ("empty", String::new(), "fallback".into()),
        ("a slash", "a/b".into(), format!("a_b{}", h("a/b"))),
        ("a path", "../x".into(), format!(".._x{}", h("../x"))),
        (
            "blanks and control characters",
            "a b\tc\nd".into(),
            format!("a_b_c_d{}", h("a b\tc\nd")),
        ),
        (
            "one underscore per rune",
            "é?*".into(),
            format!("___{}", h("é?*")),
        ),
        (
            "only unsafe characters",
            "///".into(),
            format!("___{}", h("///")),
        ),
        (
            "the current directory",
            ".".into(),
            format!("fallback{}", h(".")),
        ),
        (
            "the parent directory",
            "..".into(),
            format!("fallback{}", h("..")),
        ),
        ("only dots", "....".into(), format!("fallback{}", h("...."))),
        ("dots next to a name", "a..".into(), "a..".into()),
        (
            "a dot name that is not a path element",
            ".x".into(),
            ".x".into(),
        ),
        (
            "the edges of the character classes",
            "azAZ09".into(),
            "azAZ09".into(),
        ),
        (
            "just outside the character classes",
            "`{@[/: ".into(),
            format!("_______{}", h("`{@[/: ")),
        ),
        ("exactly 128 safe ASCII bytes", a(128), a(128)),
        (
            "129 safe ASCII bytes is truncated with hash",
            a(129),
            format!(
                "{}{}",
                a(MAX_PATH_SEGMENT_BYTES - PATH_SEGMENT_HASH_SUFFIX_LEN),
                h(a(129))
            ),
        ),
        (
            "tilde is outside the unchanged safe alphabet",
            "a~b".into(),
            format!("a_b{}", h("a~b")),
        ),
    ];
    for (name, id, want) in tests {
        let got = path_segment(&id, "fallback");
        assert_eq!(got, want, "{name}");
        assert!(
            id.is_empty() || got.len() <= MAX_PATH_SEGMENT_BYTES,
            "{name}: len(pathSegment({id:?}, \"fallback\")) = {}, exceeds {MAX_PATH_SEGMENT_BYTES}",
            got.len()
        );
    }
}

// parity: adapted: a Rust &str is valid UTF-8, so the two invalid-UTF-8 cases have no input
#[test]
fn test_text_preview() {
    let a = |n: usize| "a".repeat(n);
    let e = |n: usize| "é".repeat(n);
    let tests = [
        (
            "short",
            "hello".to_owned(),
            format!("hello{TEXT_PREVIEW_SUFFIX}"),
        ),
        ("empty", String::new(), TEXT_PREVIEW_SUFFIX.to_owned()),
        (
            "exactly the limit",
            a(200),
            format!("{}{TEXT_PREVIEW_SUFFIX}", a(200)),
        ),
        (
            "one over the limit",
            a(201),
            format!("{}{TEXT_PREVIEW_SUFFIX}", a(200)),
        ),
        ("long", a(250), format!("{}{TEXT_PREVIEW_SUFFIX}", a(200))),
        // The limit counts runes, so a rune is never cut in half.
        (
            "multibyte runes",
            e(250),
            format!("{}{TEXT_PREVIEW_SUFFIX}", e(200)),
        ),
        (
            "multibyte runes within the limit",
            e(200),
            format!("{}{TEXT_PREVIEW_SUFFIX}", e(200)),
        ),
    ];
    for (name, text, want) in tests {
        assert_eq!(text_preview(&text), want, "{name}");
    }
}

#[test]
fn test_object_ref() {
    let tests = [
        (
            "with an authorizer",
            "us.conn",
            json!({"uri": "gs://b/o.png", "details": "{}", "authorizer": "us.conn"}),
        ),
        (
            "without an authorizer",
            "",
            json!({"uri": "gs://b/o.png", "details": "{}"}),
        ),
    ];
    for (name, authorizer, want) in tests {
        // There is never a version, as adk-python writes none.
        assert_eq!(
            Value::Object(object_ref("gs://b/o.png", "{}", authorizer)),
            want,
            "{name}"
        );
    }
}

#[test]
fn test_gcs_metadata_details() {
    let tests = [
        (
            "image/png",
            7,
            r#"{"gcs_metadata":{"content_type":"image/png","size":7}}"#,
        ),
        (
            "text/plain",
            0,
            r#"{"gcs_metadata":{"content_type":"text/plain","size":0}}"#,
        ),
        (
            "video/mp4",
            123_456_789,
            r#"{"gcs_metadata":{"content_type":"video/mp4","size":123456789}}"#,
        ),
    ];
    for (content_type, size, want) in tests {
        assert_eq!(
            gcs_metadata_details(content_type, size),
            want,
            "gcsMetadataDetails({content_type:?}, {size})"
        );
    }

    // Characters that JSON escapes still make valid JSON that reads back.
    let odd = r#"a"b\c<d>&e"#;
    let decoded: Value = serde_json::from_str(&gcs_metadata_details(odd, 5))
        .unwrap_or_else(|e| panic!("gcsMetadataDetails({odd:?}, 5) is not JSON: {e}"));
    assert_eq!(decoded["gcs_metadata"]["content_type"], odd);
    assert_eq!(decoded["gcs_metadata"]["size"], 5);
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_default_concurrency_is_eight() {
    const WANT: usize = 8;
    let contents: Vec<Content> = (0..12_u8).map(|i| image_content(&[i])).collect();
    let mut row = queued_row(&opts(50), &contents);
    // The first uploads wait for each other, which shows that WANT of them run at once; the
    // pause that follows gives more a chance to start if the default were higher. Once that
    // wait fails the other uploads fail at once, instead of waiting for their turn to fail.
    let gate = Barrier::new(WANT);
    let gate_failed = Arc::new(AtomicBool::new(false));
    let off = hooked(move |_, _| {
        let (gate, gate_failed) = (Arc::clone(&gate), Arc::clone(&gate_failed));
        Box::pin(async move {
            if gate_failed.load(Ordering::SeqCst) {
                return Err(Error::msg("the first uploads did not run together"));
            }
            if let Err(err) = gate.wait().await {
                gate_failed.store(true, Ordering::SeqCst);
                return Err(err);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(())
        })
    });
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        logger: Some(Arc::new(RecordingLogger::default())),
        ..Config::zero()
    });

    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;

    assert_eq!(off.max_concurrent(), WANT, "uploads at the same time");
    assert_eq!(off.uploads().len(), contents.len(), "uploads made");
}

// TestApplyDefaults_GCSLimits pins the documented defaults of the limits of the offloading, and
// which values select them: any value that is not positive does.
#[test]
fn test_apply_defaults_gcs_limits() {
    const WANT_INLINE: i64 = 32 * 1024;
    const WANT_UPLOADS: i64 = 8;
    const WANT_PENDING: i64 = 256 << 20;
    let tests = [
        (
            "zero values",
            0,
            0,
            0,
            WANT_INLINE,
            WANT_UPLOADS,
            WANT_PENDING,
        ),
        (
            "negative values",
            -1,
            -5,
            -10,
            WANT_INLINE,
            WANT_UPLOADS,
            WANT_PENDING,
        ),
        ("the smallest positive values", 1, 1, 1, 1, 1, 1),
        (
            "larger values",
            1 << 20,
            64,
            512 << 20,
            1 << 20,
            64,
            512 << 20,
        ),
    ];
    for (name, inline, uploads, pending, want_inline, want_uploads, want_pending) in tests {
        let got = Config {
            max_inline_text_size: inline,
            gcs_max_concurrent_uploads: uploads,
            gcs_max_pending_upload_bytes: pending,
            ..Config::zero()
        }
        .apply_defaults();
        assert_eq!(
            got.max_inline_text_size, want_inline,
            "{name}: MaxInlineTextSize"
        );
        assert_eq!(
            got.gcs_max_concurrent_uploads, want_uploads,
            "{name}: GCSMaxConcurrentUploads"
        );
        assert_eq!(
            got.gcs_max_pending_upload_bytes, want_pending,
            "{name}: GCSMaxPendingUploadBytes"
        );
    }

    // DefaultConfig
    let got = Config::default();
    assert_eq!(
        got.max_inline_text_size, WANT_INLINE,
        "DefaultConfig: MaxInlineTextSize"
    );
    assert_eq!(
        got.gcs_max_concurrent_uploads, WANT_UPLOADS,
        "DefaultConfig: GCSMaxConcurrentUploads"
    );
    assert_eq!(
        got.gcs_upload_timeout, GCS_UPLOAD_TIMEOUT,
        "DefaultConfig: GCSUploadTimeout"
    );
    assert_eq!(
        got.gcs_max_pending_upload_bytes, WANT_PENDING,
        "DefaultConfig: GCSMaxPendingUploadBytes"
    );
}
