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

//! Port of Go `auth/gcp/unescape_test.go` (package `gcp`).
//!
//! `unescape_json` is reached by neither the fuzz port nor the exhaustive differential: both
//! drive `redact`, which does not call it. These tests pin it directly, and pin how
//! `redacted_for_error` chooses between the raw and the decoded candidate.

use super::auth_gcp_redact_reference::words;
use crate::gcp::redact::{
    MAX_DECODE_PASSES, REDACTED_MARKER, WITHHELD_TEXT, decode_fully, go_to_lower, recoverable,
    redacted_for_error, unescape_json,
};

/// The oracle Go's `json.Unmarshal([]byte(`"`+w+`"`), &want)` is replaced by: serde_json's
/// string decoder. It rejects a lone surrogate, which `encoding/json` decodes to U+FFFD.
fn oracle(w: &str) -> Option<String> {
    serde_json::from_str::<String>(&format!("\"{w}\"")).ok()
}

/// Go `hasForeignEscape`: whether `s` carries a JSON escape this module does not decode, which
/// is every one except `\\`, `\/` and `\uXXXX`.
fn has_foreign_escape(s: &str) -> bool {
    let s = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'\\' || i + 1 >= s.len() {
            i += 1;
            continue;
        }
        match s[i + 1] {
            b'\\' | b'/' => i += 2,
            b'u' => i += 6,
            _ => return true,
        }
    }
    false
}

/// Go `TestUnescapeJSONMatchesEncodingJSON`: every string of length <= 6 over `\/un0dA8`
/// against the standard decoder. Inputs the oracle rejects and inputs carrying an escape this
/// module deliberately does not decode are skipped and counted.
// parity: adapted: the oracle is serde_json::from_str::<String>, which rejects lone surrogates that encoding/json decodes to U+FFFD; those inputs are skipped as rejected by the oracle, and the escaped >= 1000 / foreign > 0 floors still hold
#[test]
fn test_unescape_json_matches_encoding_json() {
    // "n" is the one symbol that forms an escape this module deliberately does not decode,
    // which is what exercises the has_foreign_escape filter.
    let words = words(&["\\", "/", "u", "n", "0", "d", "A", "8"], 6);

    let (mut checked, mut escaped, mut foreign, mut rejected) = (0usize, 0usize, 0usize, 0usize);
    for w in &words {
        let Some(want) = oracle(w) else {
            rejected += 1;
            continue;
        };
        // \n, \t and friends are real JSON and deliberately not decoded here.
        if has_foreign_escape(w) {
            foreign += 1;
            continue;
        }
        checked += 1;
        if w.contains('\\') {
            escaped += 1;
        }
        let got = unescape_json(w);
        assert_eq!(
            got, want,
            "unescape_json({w:?}) = {got:?}, the oracle says {want:?}"
        );
    }
    eprintln!(
        "{checked} inputs compared ({escaped} of them escaped), {rejected} rejected by the \
         oracle, {foreign} skipped as foreign escapes"
    );
    assert!(
        escaped >= 1000,
        "only {escaped} comparable inputs contained an escape, so the alphabet stopped producing them"
    );
    assert!(
        foreign > 0,
        "no input was skipped as a foreign escape, so that filter went unexercised"
    );
}

/// Go `TestUnescapeJSONMatchesEncodingJSONOnPairs`: whole escape tokens, up to five of them,
/// so surrogate pairs (12 bytes) are compared against the oracle too.
// parity: adapted: same oracle substitution (serde_json rejects the lone surrogates encoding/json decodes to U+FFFD), so Go's "the oracle rejected" fatal becomes a counted skip; a complete pair must still be compared
#[test]
fn test_unescape_json_matches_encoding_json_on_pairs() {
    let words = words(&[r"\ud83d", r"\ude00", r"\u0041", r"\\", r"\/", "q"], 5);

    let (mut compared, mut rejected, mut pairs) = (0usize, 0usize, 0usize);
    for w in &words {
        let Some(want) = oracle(w) else {
            rejected += 1;
            continue;
        };
        compared += 1;
        if w.contains(r"\ud83d\ude00") {
            pairs += 1;
        }
        let got = unescape_json(w);
        assert_eq!(
            got, want,
            "unescape_json({w:?}) = {got:?}, the oracle says {want:?}"
        );
    }
    eprintln!(
        "{} token sequences, {compared} compared against the oracle, {rejected} rejected by it",
        words.len()
    );
    // Guards the reason this test exists: a token set that stopped producing adjacent
    // surrogate halves would still pass every assertion above.
    assert!(
        pairs > 0,
        "no compared input contained a complete surrogate pair, so the pair path went uncompared"
    );
}

/// Go `TestUnescapeJSONLeavesTheRestAlone`: the malformed shapes no oracle speaks for. Nothing
/// panics, nothing is dropped, and what cannot be decoded comes back as it went in.
#[test]
fn test_unescape_json_leaves_the_rest_alone() {
    let cases: &[(&str, &str, &str)] = &[
        ("a high surrogate at the end", r"\ud83d", "\u{FFFD}"),
        (
            "a high surrogate then a truncated escape",
            r"\ud83d\ude0",
            "\u{FFFD}\\ude0",
        ),
        (
            "a high surrogate then a non-escape",
            r"\ud83dX",
            "\u{FFFD}X",
        ),
        (
            "a high surrogate then a non-surrogate",
            r"\ud83d\u0041",
            "\u{FFFD}A",
        ),
        ("two high surrogates", r"\ud83d\ud83d", "\u{FFFD}\u{FFFD}"),
        ("a low surrogate first", r"\ude00\ud83d", "\u{FFFD}\u{FFFD}"),
        (
            "a second escape that is not hex",
            r"\ud83d\uZZZZ",
            "\u{FFFD}\\uZZZZ",
        ),
        ("non-hex in the first escape", r"\uZZZZ", r"\uZZZZ"),
        ("a bare backslash-u at the end", r"\u", r"\u"),
        ("an escape this package does not decode", r"\n", r"\n"),
        ("a lone trailing backslash", r"abc\", r"abc\"),
        ("no backslash at all", "plain text", "plain text"),
    ];
    for (name, input, want) in cases {
        let got = unescape_json(input);
        assert_eq!(
            &got, want,
            "{name}: unescape_json({input:?}) = {got:?}, want {want:?}"
        );
    }
}

/// Go `TestUnescapeJSONNeverGrows`.
#[test]
fn test_unescape_json_never_grows() {
    let repeated = r"\u0040".repeat(1000);
    for input in [
        r"\ud83d\ude00",
        r"\u0041",
        r"\/",
        r"\\",
        r"\uFFFF",
        repeated.as_str(),
    ] {
        let got = unescape_json(input);
        assert!(
            got.len() <= input.len(),
            "unescape_json({input:?}) grew {} bytes to {}",
            input.len(),
            got.len()
        );
    }
}

/// Go `TestServiceTextReturnsOnlyWhatItCanShowClean`: the choice between the two candidate
/// outputs is made on the output, and these rows hold it in place.
#[test]
fn test_service_text_returns_only_what_it_can_show_clean() {
    let long = format!("{}alice@example.testy", "x".repeat(1006));
    let cases: Vec<(&str, &str, Vec<&str>, &str)> = vec![
        (
            "the same secret escaped once and plain once",
            r"invalid userId: alice\u0040example.test (normalized: alice@example.test)",
            vec!["alice@example.test"],
            "invalid userid: [redacted] (normalized: [redacted])",
        ),
        (
            "the decode reveals the only secret",
            r"bad user alice\u0040example.test",
            vec!["alice@example.test"],
            "bad user [redacted]",
        ),
        (
            "the decode reveals nothing and would rewrite the text",
            r"invalid path C:\\logs for alice@example.test",
            vec!["alice@example.test"],
            r"invalid path c:\\logs for [redacted]",
        ),
        (
            "no candidate is clean, so nothing is returned",
            r"bad user alice\/bob, uri https:\/\/app.test\/cb",
            vec![r"alice\/bob", "https://app.test/cb"],
            WITHHELD_TEXT,
        ),
        (
            "a decoy occurrence the decode destroys",
            r"invalid userId: alice\u0040example.test (also \u004alice@example.test)",
            vec!["alice@example.test"],
            "invalid userid: [redacted] (also jlice@example.test)",
        ),
        (
            "a secret made of letters the marker also contains",
            "denied",
            vec!["e"],
            "d[redacted]ni[redacted]d",
        ),
        (
            "the cap's ellipsis completes the secret",
            long.as_str(),
            vec!["alice@example.test."],
            WITHHELD_TEXT,
        ),
    ];
    for (name, input, secrets, want) in &cases {
        let got = redacted_for_error(input, secrets);
        assert_eq!(
            &got, want,
            "{name}: redacted_for_error({input:?}) = {got:?}, want {want:?}"
        );
    }
}

/// Go `strings.Index` over bytes, for the frozen oracle below, which steps one byte at a time
/// exactly as Go's does.
fn index_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Go `readable`: the test's own oracle for "an attacker can read a secret out of this
/// string". A frozen COPY of `recoverable` (exclude the marker by position, decode then fold,
/// the same `unescape_json`), so it catches a future weakening of `recoverable`, not a blind
/// spot the two share today. Decoding runs to a fixpoint under its own explicit bound.
fn readable(x: &str, secrets: &[&str]) -> bool {
    let decode = |s: &str| -> String {
        let mut s = s.to_owned();
        for _ in 0..64 {
            let u = unescape_json(&s);
            if u == s {
                return s;
            }
            s = u;
        }
        panic!("decoding {x:?} never reached a fixpoint in 64 passes");
    };
    let marker = b"[redacted]";
    for text in [go_to_lower(x), go_to_lower(&decode(x))] {
        let text = text.as_bytes();
        let mut ours = vec![false; text.len()];
        let mut i = 0;
        while let Some(j) = index_bytes(&text[i..], marker) {
            for o in &mut ours[i + j..i + j + marker.len()] {
                *o = true;
            }
            i += j + marker.len();
        }
        for v in secrets {
            for form in [go_to_lower(v), go_to_lower(&decode(v))] {
                if form.is_empty() {
                    continue;
                }
                let form = form.as_bytes();
                let mut i = 0;
                while i + form.len() <= text.len() {
                    let Some(j) = index_bytes(&text[i..], form) else {
                        break;
                    };
                    let at = i + j;
                    if !ours[at..at + form.len()].iter().all(|&o| o) {
                        return true;
                    }
                    i = at + 1;
                }
            }
        }
    }
    false
}

/// Go `TestServiceTextNeverReturnsARecoverableSecret`: the invariant every earlier revision of
/// `redactedForError` violated, checked over every body of one to three pieces an adversary can
/// assemble from the pieces that broke them. Both outcomes have to occur.
#[test]
fn test_service_text_never_returns_a_recoverable_secret() {
    const USER: &str = "alice@example.test";
    const URI: &str = "https://app.test/cb";
    let padding = "x".repeat(600);
    let pieces: Vec<&str> = vec![
        "",
        USER,                        // plain
        r"alice\u0040example.test",  // escaped
        r"alice\\u0040example.test", // doubly escaped: one decode pass is not enough
        r"\u004alice@example.test",  // decoy: decodes to Jlice@…, contains the plain form
        URI,                         // the other secret, plain
        r"https:\/\/app.test\/cb",   // the other secret, escaped
        padding.as_str(),            // pushes a later piece past the 1024-byte cap
    ];
    let secrets = [USER, URI];

    fn gen_bodies(
        prefix: String,
        n: usize,
        pieces: &[&str],
        secrets: &[&str],
        checked: &mut usize,
        withheld: &mut usize,
    ) {
        if n == 0 {
            *checked += 1;
            let got = redacted_for_error(&prefix, secrets);
            if got == WITHHELD_TEXT {
                *withheld += 1;
                return;
            }
            assert!(
                !readable(&got, secrets),
                "redacted_for_error({prefix:?}) = {got:?}, out of which a secret is still readable"
            );
            return;
        }
        for p in pieces {
            gen_bodies(
                format!("{prefix}{p}"),
                n - 1,
                pieces,
                secrets,
                checked,
                withheld,
            );
        }
    }
    let (mut checked, mut withheld) = (0usize, 0usize);
    for n in 1..=3 {
        gen_bodies(
            String::new(),
            n,
            &pieces,
            &secrets,
            &mut checked,
            &mut withheld,
        );
    }

    eprintln!("{checked} bodies checked, {withheld} withheld");
    assert_ne!(
        withheld, checked,
        "every body was withheld, so the clean path went unexercised"
    );
    assert_ne!(
        withheld, 0,
        "no body was withheld, so the fail-closed path went unexercised"
    );
}

/// Go `TestDecodeFullyStopsAtItsPassBound`: `\u005c` decodes to a backslash that re-forms the
/// introducer for the next one, so the loop needs one pass per five bytes. The bound falls one
/// short of the constant, because a text is only reported finished by a read that changes
/// nothing. And `recoverable` fails closed on both of its arms.
#[test]
fn test_decode_fully_stops_at_its_pass_bound() {
    let chain = |n: usize| format!(r"\u005c{}", "u005c".repeat(n));

    // Well inside a pass bound, and a fixpoint it can actually reach.
    let (got, done) = decode_fully(&chain(4));
    assert!(
        done,
        "decode_fully() gave up on 5 passes' worth of input, got {got:?}"
    );
    // The last input reported finished, and the first one not.
    let (_, done) = decode_fully(&chain(MAX_DECODE_PASSES - 2));
    assert!(
        done,
        "decode_fully() gave up {} changing passes in, want it to report the fixpoint",
        MAX_DECODE_PASSES - 1
    );
    let (_, done) = decode_fully(&chain(MAX_DECODE_PASSES - 1));
    assert!(
        !done,
        "decode_fully() reported a fixpoint {MAX_DECODE_PASSES} changing passes in, want it to \
         run out of reads"
    );
    // Past it. What matters is the flag, not the text.
    let (_, done) = decode_fully(&chain(MAX_DECODE_PASSES + 50));
    assert!(
        !done,
        "decode_fully() reported a fixpoint on input needing more than MAX_DECODE_PASSES"
    );
    // The caller fails closed on it, on BOTH the text it examines and the secrets.
    let long = chain(MAX_DECODE_PASSES + 50);
    assert!(
        recoverable(&long, &["nowhere-in-this-string"]),
        "recoverable() = false on text it could not finish decoding, want it to assume the worst"
    );
    assert!(
        recoverable("carries no escape at all", &[long.as_str()]),
        "recoverable() = false on a secret it could not finish decoding, want it to assume the worst"
    );
}

/// Go `TestDecodeFullyShrinksWheneverItChanges`: the termination argument `decode_fully` rests
/// on. An equal length that still changed would loop forever.
#[test]
fn test_decode_fully_shrinks_whenever_it_changes() {
    for input in [
        r"\\u0040",
        r"\u0040",
        r"\/",
        r"\\",
        r"\ud83d\ude00",
        r"\uZZZZ",
        "plain",
        "",
        r"\\\\u0040example",
        r"a\\b\/c\u0041d",
    ] {
        let u = unescape_json(input);
        assert!(
            u == input || u.len() < input.len(),
            "unescape_json({input:?}) = {u:?} changed without shrinking, {} to {} bytes",
            input.len(),
            u.len()
        );
    }
}

/// Go `TestSecretSpanningOurOwnMarkerIsNotShown`: a value whose own bytes contain or abut the
/// marker must not come back readable; the controls must come back shown.
#[test]
fn test_secret_spanning_our_own_marker_is_not_shown() {
    let cases: &[(&str, &str, &[&str], bool)] = &[
        (
            "the marker sits inside the value",
            r"\u0061[redacted]b",
            &["a[redacted]b"],
            false,
        ),
        (
            "the value abuts the marker and shares its opening",
            r"\u0061lice[redacted]",
            &["alice[red"],
            false,
        ),
        (
            "the value begins inside our marker and runs into service text",
            "uabc",
            &["u", "ted]a"],
            false,
        ),
        (
            "CONTROL a one-character value that occurs inside the marker",
            "denied for user e, retry later",
            &["e"],
            true,
        ),
        (
            "CONTROL an ordinary value, ordinary body",
            "denied for alice@example.test",
            &["alice@example.test"],
            true,
        ),
    ];
    for (name, body, secrets, must_show) in cases {
        let got = redacted_for_error(body, secrets);
        eprintln!("{name}: returned {got:?}");
        assert!(
            !(*must_show && got == WITHHELD_TEXT),
            "{name}: withheld, want the body shown with the value redacted out of it"
        );
        if got == WITHHELD_TEXT {
            continue;
        }
        assert!(
            got.contains(REDACTED_MARKER),
            "{name}: nothing was redacted from {got:?}"
        );
        let (decoded, _) = decode_fully(&got);
        for secret in *secrets {
            // A value that fits inside the marker is readable out of it by construction.
            if REDACTED_MARKER.contains(go_to_lower(secret).as_str()) {
                continue;
            }
            assert!(
                !go_to_lower(&decoded).contains(go_to_lower(secret).as_str()),
                "{name}: {secret:?} is readable out of {decoded:?}"
            );
        }
    }
}
