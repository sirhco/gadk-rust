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

//! Port of Go `auth/gcp/redact_fuzz_test.go` (package `gcp`).
//!
//! Go's `FuzzRedact` drives the two-secret shape every call site uses, seeded with the inputs
//! that broke earlier revisions (a literal scrub, an ASCII-only fold, an offset splice, one
//! pass per value, and two range-merge bugs). Here it is deterministic (spec R10): the seed
//! corpus, then 10,000 inputs from an in-test xorshift64 PRNG, each checked against Go's fuzz
//! property and against the reference redactor.

use super::auth_gcp_redact_reference::reference;
use crate::gcp::redact::{go_to_lower, redact};

/// The xorshift64 seed spec R10 fixes.
const SEED: u64 = 0x2b2b_2b2b_2b2b_2b2b;
/// How many generated inputs follow the seed corpus.
const GENERATED: usize = 10_000;

/// Marsaglia's xorshift64 (shifts 13, 7, 17).
struct XorShift64(u64);

impl XorShift64 {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n`; `n > 0`.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// The pieces generated bodies and secrets are built from: both cases of the letters, the
/// marker and pieces of it, the escape characters, the length-changing folds U+0130 and U+023A
/// (and U+2C65, what U+023A lowers to), other multi-byte and non-BMP chars, and an identifier.
const PIECES: &[&str] = &[
    "a",
    "b",
    "A",
    "B",
    "e",
    "E",
    "u",
    " ",
    "[",
    "]",
    "\\",
    "/",
    "@",
    ".",
    "\u{130}",
    "\u{23a}",
    "\u{2c65}",
    "é",
    "É",
    "\u{1F600}",
    "[redacted]",
    "red",
    "ted]",
    "alice",
    "ALICE",
    "Alice@",
];

fn pieces(rng: &mut XorShift64, max: usize) -> String {
    let n = rng.below(max + 1);
    (0..n).map(|_| PIECES[rng.below(PIECES.len())]).collect()
}

/// A secret for body `s`: empty a quarter of the time, a quarter of the time fresh pieces, and
/// otherwise a run of `s` cut at char boundaries, so it occurs in the body (and in the
/// lowered body whenever lowering keeps it).
fn secret_for(rng: &mut XorShift64, s: &str) -> String {
    match rng.below(4) {
        0 => String::new(),
        1 => pieces(rng, 3),
        _ => {
            let mut bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
            bounds.push(s.len());
            if bounds.len() < 2 {
                return pieces(rng, 3);
            }
            let lo = rng.below(bounds.len() - 1);
            let hi = lo + 1 + rng.below(bounds.len() - 1 - lo);
            s[bounds[lo]..bounds[hi]].to_owned()
        }
    }
}

/// Go `strings.Contains` over bytes: the fragments below may be cut inside a char.
fn contains_bytes(hay: &str, needle: &[u8]) -> bool {
    needle.is_empty() || hay.as_bytes().windows(needle.len()).any(|w| w == needle)
}

/// Go's fuzz body, plus the reference comparison spec R10 adds. Panics on a violation.
fn check(s: &str, a: &str, b: &str) {
    let ls = go_to_lower(s);
    let got = redact(s, &[a, b]);

    // Spec R10: the scan equals the obviously-correct definition byte for byte.
    let want = reference(s, &[a, b]);
    assert_eq!(
        got, want,
        "redact({s:?}, {a:?}, {b:?}) = {got:?}, reference says {want:?}"
    );

    // Go checks survival only over valid UTF-8; every &str is, so it always runs here.
    // Checked between the markers rather than across them.
    let lowered_got = go_to_lower(&got);
    let between: Vec<&str> = lowered_got.split("[redacted]").collect();
    let survives = |needle: &[u8]| between.iter().any(|part| contains_bytes(part, needle));
    for secret in [a, b] {
        let lsec = go_to_lower(secret);
        if secret.is_empty() || !ls.contains(lsec.as_str()) {
            continue;
        }
        assert!(
            !survives(lsec.as_bytes()),
            "redact({s:?}, {a:?}, {b:?}) = {got:?}, {secret:?} survives"
        );
        // A fragment is a leak too. The longest proper prefix and suffix are the ones a bad
        // splice or an unmerged overlap produce; a fragment that appears in the input outside
        // the secret is not evidence of anything. Cut on bytes, as Go cuts.
        if lsec.len() < 3 {
            continue;
        }
        let elsewhere: Vec<&str> = ls.split(lsec.as_str()).collect();
        let in_input_outside_the_secret =
            |needle: &[u8]| elsewhere.iter().any(|part| contains_bytes(part, needle));
        let lb = lsec.as_bytes();
        for frag in [&lb[..lb.len() - 1], &lb[1..]] {
            assert!(
                in_input_outside_the_secret(frag) || !survives(frag),
                "redact({s:?}, {a:?}, {b:?}) = {got:?}, the fragment {:?} of {secret:?} survives",
                String::from_utf8_lossy(frag)
            );
        }
    }
    // Go's "valid UTF-8 in and invalid out" check is vacuous: `got` is a String.

    // Each match costs at most one marker for the bytes it consumes, so a rewrite that nests
    // markers or rescans its own output shows up as growth.
    assert!(
        got.len() <= 10 * ls.len() + "[redacted]".len(),
        "redact({s:?}, {a:?}, {b:?}) grew {} bytes to {}",
        s.len(),
        got.len()
    );
}

// parity: adapted: deterministic (spec R10): the Go seed corpus plus 10,000 xorshift64 inputs seeded 0x2b2b_2b2b_2b2b_2b2b, checked against the reference redactor; the non-UTF-8 seed enters through String::from_utf8_lossy and the "valid UTF-8 in, invalid out" check is vacuous for &str
#[test]
fn fuzz_redact() {
    let lossy = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let seeds: Vec<[String; 3]> = vec![
        [
            "error https://app.example.test/cb user e".into(),
            "https://app.example.test/cb".into(),
            "e".into(),
        ],
        [
            "bad continueUri: https://example.test/cb?user=alice".into(),
            "alice".into(),
            "https://example.test/cb?user=alice".into(),
        ],
        [
            "invalid: https://app.example.test/cb?login=alice@example.test".into(),
            "alice@example.test".into(),
            "https://app.example.test/cb?login=al".into(),
        ],
        [
            "\u{130}\u{130}\u{130}\u{130}\u{130}alice\u{23a}\u{23a}\u{23a}\u{23a}\u{23a}".into(),
            "alice".into(),
            String::new(),
        ],
        [
            "Alice@Example.test echoed as alice@example.test".into(),
            "alice@example.test".into(),
            String::new(),
        ],
        ["xalice".into(), "xa".into(), "alice".into()],
        [
            "https://cb.test/u/alice@example.test/alice@example.test".into(),
            "alice@example.test".into(),
            "https://cb.test/u/alice@example.test/a".into(),
        ],
        ["aaa".into(), "aa".into(), String::new()],
        // Go: {"\xdb\xd0[", "0", "\xc2["}; a &str cannot hold those bytes.
        [lossy(b"\xdb\xd0["), "0".into(), lossy(b"\xc2[")],
        [
            r"invalid userId: alice\ud83d\ude00".into(),
            "alice\u{1F600}".into(),
            String::new(),
        ],
        [
            r"bad continueUri: https:\/\/app.example.test\/cb".into(),
            "https://app.example.test/cb".into(),
            String::new(),
        ],
    ];
    for [s, a, b] in &seeds {
        check(s, a, b);
    }

    let mut rng = XorShift64(SEED);
    let mut hits = 0usize;
    for _ in 0..GENERATED {
        let s = pieces(&mut rng, 16);
        let a = secret_for(&mut rng, &s);
        let b = secret_for(&mut rng, &s);
        check(&s, &a, &b);
        if redact(&s, &[&a, &b]) != s {
            hits += 1;
        }
    }
    // Guards the generator: inputs that never redact anything would pass every check above.
    assert!(
        hits >= GENERATED / 4,
        "only {hits} of {GENERATED} generated inputs redacted anything"
    );
}
