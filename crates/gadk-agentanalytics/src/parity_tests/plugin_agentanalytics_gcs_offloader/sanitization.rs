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

//! `gcs_offloader_test.go` (ADK-Go `0990f9e`, #2018): the object name sanitization and bounds
//! (`TestCleanGCSPrefix`, `TestGCSObjectPath_SanitizationAndBounds`,
//! `TestBatchProcessor_ObjectPathCachesEffectivePrefix`).
//!
//! gadk has no `cleanGCSPrefix` yet, so [`clean_gcs_prefix`] reads the effective prefix off
//! [`gcs_object_path`]'s result (the object name minus its fixed tail); the 8a fix task
//! replaces it, [`super::path_segment_hash_suffix`] and the size constants with the production
//! items.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;

use super::super::plugin_agentanalytics_batch_processor::support::utc;
use super::{
    MAX_GCS_PREFIX_BYTES, MAX_GCS_REWRITTEN_PREFIX_BYTES, PATH_SEGMENT_HASH_SUFFIX_LEN, bare,
    path_segment_hash_suffix,
};
use crate::config::Config;
use crate::formatter::{PendingUpload, TEXT_UPLOAD_CONTENT_TYPE};
use crate::offload::uploads::gcs_object_path;
use crate::row::{Row, RowValue};

/// Cloud Storage's 1,024-byte object name limit (Go's test pins it as `wantObjectLimit`).
const WANT_OBJECT_LIMIT: usize = 1024;

fn ts() -> DateTime<Utc> {
    utc(2026, 3, 4, 5, 6, 7, 0)
}

/// Go `cleanGCSPrefix(prefix)`, read off the name [`gcs_object_path`] gives a short part.
pub(super) fn clean_gcs_prefix(prefix: &str) -> String {
    const TAIL: &str = "2026-03-04/t/s_e_c0_p0";
    let got = gcs_object_path(prefix, ts(), "t", "s", "e", 0, 0, "");
    match got.strip_suffix(TAIL) {
        Some("") => String::new(),
        Some(lead) => lead.strip_suffix('/').unwrap_or(lead).to_owned(),
        None => panic!("gcs_object_path({prefix:?}) = {got:?} does not end in {TAIL:?}"),
    }
}

/// Go `isUnsafeGCSPrefixRune(r)`.
fn is_unsafe_gcs_prefix_rune(r: char) -> bool {
    r < '\u{20}' || ('\u{7f}'..='\u{9f}').contains(&r) || r == '\u{2028}' || r == '\u{2029}'
}

/// Go's `s[:n]` of a string whose first `n` bytes are ASCII.
fn cut(s: &str, n: usize) -> &str {
    &s[..n]
}

// parity: adapted: cleanGCSPrefix is read off gcs_object_path's name (gadk has none yet); a Rust &str is valid UTF-8, so the "a byte that is not UTF-8" case and the closing \xff / \xfe check are left out
#[test]
#[ignore = "8a bug: GCSPathPrefix values with '~', control or line-separator characters, a leading .well-known/acme-challenge or over 750 bytes are not rewritten to <base>~<sha256> (Go #2018 cleanGCSPrefix)"]
fn test_clean_gcs_prefix() {
    fn h(s: impl AsRef<[u8]>) -> String {
        path_segment_hash_suffix(s)
    }
    let a = |n: usize| "a".repeat(n);
    // "aa" and 260 three-byte runes (782 bytes): the cut at 402 bytes falls inside the 134th
    // rune and backs up to 401 bytes.
    let cjk = format!("aa{}", "日".repeat(260));
    let tests: Vec<(&str, String, String)> = vec![
        ("empty", String::new(), String::new()),
        ("only slashes", "///".into(), String::new()),
        (
            "leading and trailing slashes are trimmed",
            "/logs/tenant/".into(),
            "logs/tenant".into(),
        ),
        (
            "spaces, equals signs and Unicode are kept",
            "tenant=acme/logs v1/テナント".into(),
            "tenant=acme/logs v1/テナント".into(),
        ),
        ("three dots are a name", "a/.../b".into(), "a/.../b".into()),
        (
            "an empty inner segment is kept",
            "logs//tenant".into(),
            "logs//tenant".into(),
        ),
        ("a dot segment is kept", "a/./b".into(), "a/./b".into()),
        (
            "a dot-dot segment is kept",
            "logs/../tenant".into(),
            "logs/../tenant".into(),
        ),
        (
            "only dot segments are kept after trimming slashes",
            "/./../".into(),
            "./..".into(),
        ),
        (
            "a backslash is kept",
            r"logs\prod".into(),
            r"logs\prod".into(),
        ),
        (
            "a tilde is reserved and rewritten",
            "~team/logs~v1".into(),
            format!("_team/logs_v1{}", h("~team/logs~v1")),
        ),
        (
            "an ACME path that does not lead is kept",
            "logs/.well-known/acme-challenge".into(),
            "logs/.well-known/acme-challenge".into(),
        ),
        (
            "a .well-known without acme-challenge is kept",
            ".well-known/x".into(),
            ".well-known/x".into(),
        ),
        (
            "a .well-known/acme-challenge-x lookalike segment is kept",
            ".well-known/acme-challenge-x".into(),
            ".well-known/acme-challenge-x".into(),
        ),
        (
            "a .well-known//acme-challenge with empty second segment is kept",
            ".well-known//acme-challenge".into(),
            ".well-known//acme-challenge".into(),
        ),
        (
            "402 bytes with tilde keeps all 402 sanitized bytes",
            format!("{}~", a(401)),
            format!("{}_{}", a(401), h(format!("{}~", a(401)))),
        ),
        (
            "403 bytes with tilde cuts readable base to 402 bytes",
            format!("{}~", a(402)),
            format!("{}{}", a(402), h(format!("{}~", a(402)))),
        ),
        (
            "467 bytes with tilde cuts readable base to 402 bytes",
            format!("{}~", a(466)),
            format!("{}{}", a(402), h(format!("{}~", a(466)))),
        ),
        ("513 bytes (kept under 750-byte cap)", a(513), a(513)),
        ("exactly 750 bytes", a(750), a(750)),
        ("751 bytes", a(751), format!("{}{}", a(402), h(a(751)))),
        ("900 bytes", a(900), format!("{}{}", a(402), h(a(900)))),
        (
            "the cut backs up to a rune boundary",
            cjk.clone(),
            format!("aa{}{}", "日".repeat(133), h(&cjk)),
        ),
        ("a tab", "logs\tx".into(), format!("logs_x{}", h("logs\tx"))),
        (
            "a newline",
            "logs\nx".into(),
            format!("logs_x{}", h("logs\nx")),
        ),
        (
            "a carriage return",
            "logs\rx".into(),
            format!("logs_x{}", h("logs\rx")),
        ),
        (
            "DEL",
            "logs\x7fx".into(),
            format!("logs_x{}", h("logs\x7fx")),
        ),
        (
            "a C1 control character",
            "logs\u{9b}x".into(),
            format!("logs_x{}", h("logs\u{9b}x")),
        ),
        (
            "next line",
            "logs\u{85}x".into(),
            format!("logs_x{}", h("logs\u{85}x")),
        ),
        (
            "line separator",
            "logs\u{2028}x".into(),
            format!("logs_x{}", h("logs\u{2028}x")),
        ),
        (
            "paragraph separator",
            "logs\u{2029}x".into(),
            format!("logs_x{}", h("logs\u{2029}x")),
        ),
        // skipped case: "a byte that is not UTF-8" — a Rust &str is valid UTF-8.
        (
            "the reserved ACME path",
            ".well-known/acme-challenge/x".into(),
            format!(
                "_well-known/acme-challenge/x{}",
                h(".well-known/acme-challenge/x")
            ),
        ),
        (
            "the reserved ACME path in upper case",
            ".WELL-KNOWN/ACME-CHALLENGE".into(),
            format!(
                "_well-known/ACME-CHALLENGE{}",
                h(".WELL-KNOWN/ACME-CHALLENGE")
            ),
        ),
        (
            "the reserved ACME path with leading and trailing slashes",
            "//.well-known/acme-challenge//".into(),
            format!(
                "_well-known/acme-challenge{}",
                h(".well-known/acme-challenge")
            ),
        ),
        (
            "the reserved ACME path with Kelvin sign",
            ".well-\u{212a}nown/acme-challenge/x".into(),
            format!(
                "_well-known/acme-challenge/x{}",
                h(".well-\u{212a}nown/acme-challenge/x")
            ),
        ),
    ];
    for (name, prefix, want) in tests {
        let got = clean_gcs_prefix(&prefix);
        assert_eq!(got, want, "{name}: cleanGCSPrefix({prefix:?})");
        assert!(
            got.len() <= MAX_GCS_PREFIX_BYTES,
            "{name}: cleanGCSPrefix({prefix:?}) is {} bytes, want at most {MAX_GCS_PREFIX_BYTES}",
            got.len()
        );
        let trimmed = prefix.trim_matches('/');
        if got == trimmed {
            assert!(!got.contains('~'), "{name}: {got:?} kept '~' verbatim");
            assert_eq!(
                clean_gcs_prefix(&got),
                got,
                "{name}: rewritten on a second pass"
            );
        } else {
            assert!(
                got.len() <= MAX_GCS_REWRITTEN_PREFIX_BYTES,
                "{name}: rewritten prefix is {} bytes, want at most {MAX_GCS_REWRITTEN_PREFIX_BYTES}",
                got.len()
            );
            let suffix = h(trimmed);
            let Some(base) = got.strip_suffix(&suffix) else {
                panic!("{name}: {got:?} rewritten without the hash suffix of {trimmed:?}");
            };
            assert!(
                !base.contains('~'),
                "{name}: {got:?} has '~' before its hash suffix"
            );
            assert_ne!(
                clean_gcs_prefix(&got),
                got,
                "{name}: raw and rewritten prefix namespaces must be disjoint"
            );
        }
    }
}

// parity: adapted: Go's int ordinals are usize (usize::MAX has the 20 digits of Go's math.MinInt); the "distinct non-ASCII event ids upload both payloads" subtest and the uploads of the "prefix sanitization and reserved paths" subtest (fake Cloud Storage) are left out (the 8a fix task ports them); a Rust &str is valid UTF-8, so the "invalid UTF-8 extension" subtest and the invalid-byte cases are left out
#[test]
#[ignore = "8a bug: gcs_object_path neither hash-suffixes altered or long ids, caps the name at 1,024 bytes, hashes unsafe extensions nor rewrites unsafe prefixes (Go #2018)"]
fn test_gcs_object_path_sanitization_and_bounds() {
    fn h(s: impl AsRef<[u8]>) -> String {
        path_segment_hash_suffix(s)
    }
    let ts = ts();
    let path = |prefix: &str, trace: &str, span: &str, event: &str, ext: &str| {
        gcs_object_path(prefix, ts, trace, span, event, 0, 0, ext)
    };

    // non-ASCII and sanitized ids do not collide
    let e_hash = format!("_{}", h("é"));
    let ab_hash = format!("a_b{}", h("a/b"));
    let long_a = format!("{}5606", "a".repeat(128));
    let long_b = format!("{}91879", "a".repeat(128));
    let pairs: [(&str, &str); 16] = [
        ("é", "ü"),
        ("é", "__4a99557e"),
        ("é", &e_hash),
        ("session-日本", "session-韓国"),
        ("日本", "韓国"),
        ("日本", "__"),
        (".", ".."),
        (".", ""),
        ("..", ""),
        ("é?*", "///"),
        ("é?*", "___"),
        ("a/b", "a_b"),
        ("a/b", "a_b_c14cddc0"),
        ("a/b", &ab_hash),
        (&long_a, &long_b),
        (
            "\u{ac01}\u{ac01}\u{ac02}\u{ac05}\u{ac09}",
            "\u{ac01}\u{ac05}\u{ac0f}\u{ac08}\u{ac07}",
        ),
    ];
    for (e0, e1) in pairs {
        let p0 = path("logs", "trace-1", "span-1", e0, ".txt");
        let p1 = path("logs", "trace-1", "span-1", e1, ".txt");
        assert_ne!(
            p0, p1,
            "eventID {e0:?} and {e1:?} collided on a GCS object path"
        );
    }

    // long ids and prefixes stay within 1024 bytes without colliding
    let long_a = "a".repeat(1100);
    let long_b = format!("{}-b-{}", "a".repeat(1000), "z".repeat(100));
    let long_multi_seg_prefix = "segment123/".repeat(120);
    let long_utf8 = "日本".repeat(600);
    let (la, lm, lu) = (
        long_a.as_str(),
        long_multi_seg_prefix.as_str(),
        long_utf8.as_str(),
    );
    for (name, prefix, trace, span, event, ext) in [
        ("long_event_id", "logs", "trace", "span", la, ".txt"),
        ("long_trace_id", "logs", la, "span", "event", ".txt"),
        ("long_span_id", "logs", "trace", la, "event", ".txt"),
        (
            "long_single_segment_prefix",
            la,
            "trace",
            "span",
            "event",
            ".txt",
        ),
        (
            "long_multi_segment_prefix",
            lm,
            "trace",
            "span",
            "event",
            ".txt",
        ),
        ("all_long_ascii", lm, la, la, la, ".json"),
        ("all_long_utf8", lu, lu, lu, lu, ".bin"),
    ] {
        let got = path(prefix, trace, span, event, ext);
        assert!(
            got.len() <= WANT_OBJECT_LIMIT,
            "{name}: len(gcsObjectPath) = {}",
            got.len()
        );
        assert!(
            got.ends_with(ext),
            "{name}: gcsObjectPath = {got:?}, want suffix {ext:?}"
        );
    }
    assert_ne!(
        path("logs", "trace", "span", &long_a, ".txt"),
        path("logs", "trace", "span", &long_b, ".txt"),
        "distinct 1100-byte event IDs sharing 1000-byte prefix collided"
    );
    let safe_ext = ".abcdefghijklmnop";
    let max_prefix = "p".repeat(750);
    let max_ord = usize::MAX;
    let want_plugin_max = 865 + 2 * max_ord.to_string().len();
    let got = gcs_object_path(
        &max_prefix,
        ts,
        &"a".repeat(32),
        &"b".repeat(16),
        &"c".repeat(32),
        max_ord,
        max_ord,
        safe_ext,
    );
    assert!(
        got.len() == want_plugin_max
            && got.len() <= 905
            && got.starts_with(&format!("{max_prefix}/"))
            && got.ends_with(safe_ext),
        "gcsObjectPath with 750-byte prefix and plugin hex IDs is {} bytes, want {want_plugin_max}",
        got.len()
    );
    let got = gcs_object_path(
        &max_prefix,
        ts,
        &long_a,
        &long_a,
        &long_a,
        max_ord,
        max_ord,
        safe_ext,
    );
    assert!(
        got.len() == WANT_OBJECT_LIMIT && got.ends_with(safe_ext),
        "longest unchanged prefix, direct-call parts and a safe extension: {} bytes, want {WANT_OBJECT_LIMIT}",
        got.len()
    );
    let want_rewritten_direct_max = 886 + 2 * max_ord.to_string().len();
    let got = gcs_object_path(
        &long_a, ts, &long_a, &long_a, &long_a, max_ord, max_ord, safe_ext,
    );
    assert!(
        got.len() == want_rewritten_direct_max && got.len() <= 926 && got.ends_with(safe_ext),
        "rewritten prefix, direct-call parts and a safe extension: {} bytes, want {want_rewritten_direct_max}",
        got.len()
    );
    let (trace128, span128, event128) = ("t".repeat(128), "s".repeat(128), "e".repeat(128));
    for ext in [
        ".txt".to_owned(),
        safe_ext.to_owned(),
        String::new(),
        "x".repeat(1100),
        format!(".{}", "x".repeat(1100)),
    ] {
        let got = path(&max_prefix, &trace128, &span128, &event128, &ext);
        assert_eq!(
            got.len(),
            WANT_OBJECT_LIMIT,
            "ext len {}: len(gcsObjectPath)",
            ext.len()
        );
    }
    let event128_a = format!("{}a", "e".repeat(127));
    let event128_b = format!("{}b", "e".repeat(127));
    for ext in [".txt", safe_ext] {
        let uncapped_a =
            format!("{max_prefix}/2026-03-04/{trace128}/{span128}_{event128_a}_c0_p0{ext}");
        let uncapped_b =
            format!("{max_prefix}/2026-03-04/{trace128}/{span128}_{event128_b}_c0_p0{ext}");
        let n = WANT_OBJECT_LIMIT - PATH_SEGMENT_HASH_SUFFIX_LEN - ext.len();
        assert_eq!(
            cut(&uncapped_a, n),
            cut(&uncapped_b, n),
            "ext {ext:?}: inputs differ before the cut"
        );
        let want_a = format!("{}{}{ext}", cut(&uncapped_a, n), h(&uncapped_a));
        let want_b = format!("{}{}{ext}", cut(&uncapped_b, n), h(&uncapped_b));
        let got_a = path(&max_prefix, &trace128, &span128, &event128_a, ext);
        let got_b = path(&max_prefix, &trace128, &span128, &event128_b, ext);
        assert_ne!(got_a, got_b, "ext {ext:?}: overflow names collided");
        assert_eq!(got_a, want_a, "ext {ext:?}: gotA");
        assert_eq!(got_b, want_b, "ext {ext:?}: gotB");
    }
    for ext in [
        format!(".{}", "x".repeat(1100)),
        format!(".{}y", "x".repeat(1099)),
    ] {
        let uncapped = format!("logs/2026-03-04/trace/span_event_c0_p0{ext}");
        let want = format!(
            "{}{}",
            cut(&uncapped, WANT_OBJECT_LIMIT - PATH_SEGMENT_HASH_SUFFIX_LEN),
            h(&uncapped)
        );
        assert_eq!(
            path("logs", "trace", "span", "event", &ext),
            want,
            "gcsObjectPath with a {}-byte extension",
            ext.len()
        );
    }

    // an extension cannot make a name equal to a hashed one
    let path_with = |ext: &str| path("logs", "trace", "span", "event", ext);
    let base = path_with("");
    let p1 = path_with(&format!(".{}", "x".repeat(1100)));
    assert!(
        p1.starts_with(&base) && p1.len() == WANT_OBJECT_LIMIT,
        "name with an oversized extension is {} bytes, want {WANT_OBJECT_LIMIT} starting with {base:?}",
        p1.len()
    );
    let ext2 = &p1[base.len()..];
    let p2 = path_with(ext2);
    assert_ne!(
        p2, p1,
        "name with extension {ext2:?} is the name an oversized extension makes"
    );
    assert!(
        p2.len() <= WANT_OBJECT_LIMIT,
        "len(name with extension {ext2:?}) = {}",
        p2.len()
    );
    assert_eq!(
        p2,
        format!(
            "{}{}",
            cut(&p1, WANT_OBJECT_LIMIT - PATH_SEGMENT_HASH_SUFFIX_LEN),
            h(&p1)
        ),
        "name with extension {ext2:?}"
    );
    let base_hash = h(&base);
    let base_hash_readable = format!("_{}", &base_hash[1..]);
    for (ext, readable) in [
        (".a~b", ".a_b"),
        ("~", "_"),
        (base_hash.as_str(), base_hash_readable.as_str()),
        (".a b", ".a b"),
        ("png", "png"),
        (".png/..", ".png_.."),
        (".", "."),
    ] {
        let got = path_with(ext);
        assert_eq!(
            got,
            format!("{base}{readable}{}", h(format!("{base}{ext}"))),
            "name with extension {ext:?}"
        );
        assert_ne!(
            path_with(&got[base.len()..]),
            got,
            "names with extensions {ext:?} and its hashed form"
        );
    }
    for ext in ["", ".txt", ".abcdefghijklmnop"] {
        assert_eq!(
            path_with(ext),
            format!("{base}{ext}"),
            "name with extension {ext:?}"
        );
    }

    // skipped case: "an invalid UTF-8 extension gives a valid name" — a Rust &str is valid UTF-8.

    // an unsafe extension cannot put a line break or control character in the name
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut check = |desc: &str, name: &str| {
        assert!(
            name.len() <= WANT_OBJECT_LIMIT,
            "{desc}: len(name) = {}",
            name.len()
        );
        if let Some((i, r)) = name
            .char_indices()
            .find(|&(_, r)| r.is_control() || r == '\u{2028}' || r == '\u{2029}' || r == '\\')
        {
            panic!("{desc}: name has {r:?} at byte {i}");
        }
        if let Some(prev) = names.insert(name.to_owned(), desc.to_owned()) {
            panic!("{prev} and {desc} both give the name {name:?}");
        }
    };
    for (name, ext, readable) in [
        ("a carriage return", ".png\r", ".png_"),
        ("a line feed", ".png\n", ".png_"),
        ("a carriage return and a line feed", ".png\r\n", ".png__"),
        ("a line feed and a carriage return", ".png\n\r", ".png__"),
        ("a line feed alone", "\n", "_"),
        ("a line feed in the middle", ".p\nng", ".p_ng"),
        ("NUL", ".png\0", ".png_"),
        ("a tab", ".png\t", ".png_"),
        ("the last C0 control character", ".png\x1f", ".png_"),
        ("DEL", ".png\x7f", ".png_"),
        ("the first C1 control character", ".png\u{80}", ".png_"),
        ("next line", ".png\u{85}", ".png_"),
        ("the last C1 control character", ".png\u{9f}", ".png_"),
        ("line separator", ".png\u{2028}", ".png_"),
        ("paragraph separator", ".png\u{2029}", ".png_"),
        ("a backslash", ".png\\", ".png_"),
        ("a tilde", ".png~", ".png_"),
        ("a slash", ".png/", ".png_"),
        ("a slash before a dot-dot", ".png/..", ".png_.."),
        ("a slash alone", "/", "_"),
        // skipped case: "line breaks around an invalid byte" — a Rust &str is valid UTF-8.
        ("the replacement as it is", ".png_", ".png_"),
        ("two replacements as they are", ".png__", ".png__"),
        ("a literal U+FFFD", ".png\u{fffd}", ".png\u{fffd}"),
        ("spaces and Unicode", ".a b日本é", ".a b日本é"),
    ] {
        let got = path_with(ext);
        assert_eq!(
            got,
            format!("{base}{readable}{}", h(format!("{base}{ext}"))),
            "{name}"
        );
        check(name, &got);
    }
    for (name, ext, readable) in [
        (
            "line breaks",
            format!(".{}", "\r\n".repeat(600)),
            format!("{base}.{}", "__".repeat(600)),
        ),
        (
            "line separators between letters",
            format!(".{}", "\u{2028}a".repeat(500)),
            format!("{base}.{}", "_a".repeat(500)),
        ),
    ] {
        let want = format!(
            "{}{}",
            cut(&readable, WANT_OBJECT_LIMIT - PATH_SEGMENT_HASH_SUFFIX_LEN),
            h(format!("{base}{ext}"))
        );
        let got = path_with(&ext);
        assert_eq!(got, want, "name with an oversized extension of {name}");
        check(&format!("an oversized extension of {name}"), &got);
    }
    let multi = "日\r".repeat(400);
    let want_multi = format!("{base}{}{}", "日_".repeat(230), h(format!("{base}{multi}")));
    let got_multi = path_with(&multi);
    assert_eq!(
        got_multi, want_multi,
        "oversized extension of three-byte runes and carriage returns"
    );
    check(
        "an oversized extension of three-byte runes and carriage returns",
        &got_multi,
    );

    // prefix sanitization and reserved paths
    let a750 = "a".repeat(750);
    let a751 = "a".repeat(751);
    let lead = |p: String| format!("{p}/2026-03-04/");
    for (prefix, want_lead) in [
        ("tenant=acme", lead("tenant=acme".into())),
        ("logs v1", lead("logs v1".into())),
        ("logs/テナント", lead("logs/テナント".into())),
        ("logs/.../tenant", lead("logs/.../tenant".into())),
        ("a/.../b", lead("a/.../b".into())),
        ("../x", lead("../x".into())),
        ("a/../b", lead("a/../b".into())),
        ("a//b", lead("a//b".into())),
        (".", lead(".".into())),
        ("..", lead("..".into())),
        ("a/./b", lead("a/./b".into())),
        (r"team\prod/v1", lead(r"team\prod/v1".into())),
        (
            r"~team\prod/v~1",
            lead(format!(r"_team\prod/v_1{}", h(r"~team\prod/v~1"))),
        ),
        (
            ".well-known/acme-challenge-x",
            lead(".well-known/acme-challenge-x".into()),
        ),
        (&a750, lead(a750.clone())),
        (&a751, lead(format!("{}{}", "a".repeat(402), h(&a751)))),
        (
            ".well-known/acme-challenge/x",
            lead(format!(
                "_well-known/acme-challenge/x{}",
                h(".well-known/acme-challenge/x")
            )),
        ),
        (
            "/.well-known/ACME-CHALLENGE/x",
            lead(format!(
                "_well-known/ACME-CHALLENGE/x{}",
                h(".well-known/ACME-CHALLENGE/x")
            )),
        ),
        (
            "logs\ninjected/part\r\n2\0",
            lead(format!(
                "logs_injected/part__2_{}",
                h("logs\ninjected/part\r\n2\0")
            )),
        ),
        (
            "logs\u{2028}sep\u{2029}",
            lead(format!("logs_sep_{}", h("logs\u{2028}sep\u{2029}"))),
        ),
        // skipped case: "logs/\xff" — a Rust &str is valid UTF-8.
    ] {
        let got = path(prefix, "trace", "span", "event", ".txt");
        assert!(
            got.starts_with(&want_lead),
            "gcsObjectPath(prefix={prefix:?}) = {got:?}, want prefix {want_lead:?}"
        );
        assert!(
            got.len() <= WANT_OBJECT_LIMIT,
            "gcsObjectPath(prefix={prefix:?}) is {} bytes",
            got.len()
        );
        let mut segs = got.split('/');
        let (seg0, seg1) = (segs.next().unwrap_or(""), segs.next().unwrap_or(""));
        assert!(
            !(seg0.eq_ignore_ascii_case(".well-known")
                && seg1.eq_ignore_ascii_case("acme-challenge")),
            "gcsObjectPath(prefix={prefix:?}) = {got:?} begins with the reserved ACME segments"
        );
        assert!(
            !got.chars().any(is_unsafe_gcs_prefix_rune),
            "gcsObjectPath(prefix={prefix:?}) = {got:?} contains an unsafe rune"
        );
    }
    let over_limit_raw = "a".repeat(MAX_GCS_PREFIX_BYTES + 1);
    let distinct_prefixes = [
        String::new(),
        ".".into(),
        "..".into(),
        "./..".into(),
        "...".into(),
        "logs/tenant".into(),
        "logs/.../tenant".into(),
        "logs/../tenant".into(),
        "logs/./tenant".into(),
        "logs//tenant".into(),
        r"team\prod".into(),
        r"~team\prod".into(),
        clean_gcs_prefix(r"~team\prod"),
        r"_team\prod".into(),
        ".well-known/acme-challenge-x".into(),
        ".well-known/acme-challenge/x".into(),
        clean_gcs_prefix(".well-known/acme-challenge/x"),
        "_well-known/acme-challenge/x".into(),
        "logs\tx".into(),
        "logs\nx".into(),
        clean_gcs_prefix("logs\nx"),
        "logs_x".into(),
        // skipped case: "\xff" and its rewrite — a Rust &str is valid UTF-8.
        "_".into(),
        over_limit_raw.clone(),
        clean_gcs_prefix(&over_limit_raw),
    ];
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for p in distinct_prefixes {
        let got = path(&p, "trace", "span", "event", ".txt");
        if let Some(prev) = seen.insert(got.clone(), p.clone()) {
            panic!("distinct prefixes {prev:?} and {p:?} collided on GCS object path {got:?}");
        }
    }
}

// parity: adapted: gadk's batch processor config is fixed at construction, so the check that replaces Config.GCSPathPrefix after construction and Go's allocation counts (runtime.MemStats, testing.AllocsPerRun) are left out; the path checks stay
#[test]
#[ignore = "8a bug: BatchProcessor::object_path uses the raw GCSPathPrefix (no effective prefix resolved and cached at construction), so a 4 MiB prefix makes a 4 MiB name and a control character reaches it (Go #2018)"]
fn test_batch_processor_object_path_caches_effective_prefix() {
    let ts = ts();
    let mut row = Row::new();
    row.insert("timestamp".into(), RowValue::from(ts));
    row.insert("trace_id".into(), RowValue::from("trace-1"));
    row.insert("span_id".into(), RowValue::from("span-1"));
    row.insert("event_id".into(), RowValue::from("event-1"));
    let upload = PendingUpload {
        content_ord: 0,
        part_idx: 0,
        mime_type: TEXT_UPLOAD_CONTENT_TYPE.into(),
        ..PendingUpload::default()
    };
    for (name, prefix) in [
        ("4MiB_raw_prefix", format!("tenant/{}", "a".repeat(4 << 20))),
        ("control_char_prefix", "logs\nx".to_owned()),
        ("slash_only_4MiB_prefix", "/".repeat(4 << 20)),
    ] {
        let bp = bare(Config {
            gcs_path_prefix: prefix.clone(),
            ..Config::default().apply_defaults()
        });
        assert!(
            bp.config.gcs_path_prefix == prefix,
            "{name}: the raw prefix was not kept"
        );
        let want_clean = clean_gcs_prefix(&prefix);
        let want_path = gcs_object_path(&prefix, ts, "trace-1", "span-1", "event-1", 0, 0, ".txt");
        let double_normalized = gcs_object_path(
            &want_clean,
            ts,
            "trace-1",
            "span-1",
            "event-1",
            0,
            0,
            ".txt",
        );
        for i in 0..3 {
            let got = bp.object_path(&row, &upload);
            assert!(
                got == want_path,
                "{name}: call {i}: bp.objectPath() is not gcsObjectPath(prefix)"
            );
            assert!(
                want_clean.is_empty() || got != double_normalized,
                "{name}: call {i}: bp.objectPath() double-normalized the effective prefix"
            );
        }
    }
}
