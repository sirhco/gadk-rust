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

//! Port of Go `auth/gcp/redact_reference_test.go` (package `gcp`).
//!
//! `redact` was rewritten seven times over one Go review and six of those rewrites shipped a
//! way for the acting user to survive, so the property is settled here by exhaustion against a
//! definition simple enough to be read and believed.
//!
//! Go's `testing.Short()` bounds are not ported: Rust has no short mode, so both tests always
//! run at Go's full bounds.

use crate::gcp::redact::{go_to_lower, redact, redact_lowered};

/// Every string over `alphabet` of length `0..=n`, in Go's `gen` order (pre-order: the prefix,
/// then each extension).
pub(super) fn words(alphabet: &[&str], n: usize) -> Vec<String> {
    fn gen_words(alphabet: &[&str], prefix: String, n: usize, out: &mut Vec<String>) {
        out.push(prefix.clone());
        if n == 0 {
            return;
        }
        for c in alphabet {
            gen_words(alphabet, format!("{prefix}{c}"), n - 1, out);
        }
    }
    let mut out = Vec::new();
    gen_words(alphabet, String::new(), n, &mut out);
    out
}

/// Go `reference`: the obviously-correct definition. Mark every byte covered by any occurrence
/// of any secret in the lowered text, then emit the uncovered runs separated by one marker per
/// maximal covered run. Quadratic, and it exists only to say what the answer is. `pub(super)`
/// so the `FuzzRedact` port checks against it too.
pub(super) fn reference(s: &str, values: &[&str]) -> String {
    let lowered: Vec<String> = values
        .iter()
        .filter(|v| !v.is_empty())
        .map(|v| go_to_lower(v))
        .collect();
    if lowered.is_empty() {
        return s.to_owned();
    }
    let ls = go_to_lower(s);
    let ls = ls.as_bytes();
    let mut covered = vec![false; ls.len()];
    let mut hit = false;
    for lv in &lowered {
        let lv = lv.as_bytes();
        let mut i = 0;
        while i + lv.len() <= ls.len() {
            if &ls[i..i + lv.len()] == lv {
                hit = true;
                for c in &mut covered[i..i + lv.len()] {
                    *c = true;
                }
            }
            i += 1;
        }
    }
    if !hit {
        return s.to_owned();
    }
    let mut b: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < ls.len() {
        if covered[i] {
            b.extend_from_slice(b"[redacted]");
            while i < ls.len() && covered[i] {
                i += 1;
            }
            continue;
        }
        b.push(ls[i]);
        i += 1;
    }
    // Covered runs start and end where a valid-UTF-8 value does, so what is left is whole chars.
    String::from_utf8(b).expect("reference output is UTF-8")
}

/// Go `referenceBounded`: the same coverage as `reference`, but only positions before `limit`
/// are emitted, and a covered run reaching the limit still collapses to exactly one marker.
/// Reports whether a marker was written. `limit` is Go's `int`, so `-1` is expressible here.
fn reference_bounded(ls: &str, limit: i64, lowered: &[String]) -> (String, bool) {
    let ls = ls.as_bytes();
    let limit = limit.min(ls.len() as i64);
    if limit <= 0 {
        return (String::new(), false);
    }
    let limit = limit as usize;
    let mut covered = vec![false; ls.len()];
    for lv in lowered {
        if lv.is_empty() {
            continue;
        }
        let lv = lv.as_bytes();
        let mut i = 0;
        while i + lv.len() <= ls.len() {
            if &ls[i..i + lv.len()] == lv {
                for c in &mut covered[i..i + lv.len()] {
                    *c = true;
                }
            }
            i += 1;
        }
    }
    let mut b: Vec<u8> = Vec::new();
    let mut hit = false;
    let mut i = 0;
    while i < limit {
        if covered[i] {
            b.extend_from_slice(b"[redacted]");
            hit = true;
            while i < ls.len() && covered[i] {
                i += 1;
            }
            continue;
        }
        b.push(ls[i]);
        i += 1;
    }
    if !hit {
        return (String::new(), false);
    }
    (
        String::from_utf8(b).expect("reference output is UTF-8"),
        true,
    )
}

/// Go `TestRedactLoweredMatchesReferenceAtEveryBound`: the exhaustive comparison re-run at
/// every possible bound. Bodies to eight bytes over two letters, secrets of one to four, every
/// pair of secrets and every bound from -1 to `len(body) + 1`. Inputs are pre-lowered.
///
/// `redact_lowered` takes a `usize` limit, so Go's `-1` is passed as `0`; Go's
/// `if limit <= 0` gives both the same answer, and the reference still sees `-1`.
#[test]
fn test_redact_lowered_matches_reference_at_every_bound() {
    let max_len = 8;
    let words = words(&["a", "b"], max_len);

    let secrets: Vec<&String> = words
        .iter()
        .filter(|w| !w.is_empty() && w.len() <= 4)
        .collect();

    let (mut checked, mut mismatches) = (0usize, 0usize);
    let mut reports: Vec<String> = Vec::new();
    for body in &words {
        for s1 in &secrets {
            for s2 in &secrets {
                let lowered = vec![(*s1).clone(), (*s2).clone()];
                // Every bound, and one on each side of the text.
                for limit in -1..=(body.len() as i64 + 1) {
                    checked += 1;
                    let (got, got_hit) =
                        redact_lowered(body, usize::try_from(limit).unwrap_or(0), &lowered);
                    let (want, want_hit) = reference_bounded(body, limit, &lowered);
                    if got == want && got_hit == want_hit {
                        continue;
                    }
                    if mismatches < 5 {
                        reports.push(format!(
                            "redact_lowered({body:?}, {limit}, {lowered:?}) = ({got:?}, {got_hit}), \
                             reference says ({want:?}, {want_hit})"
                        ));
                    }
                    mismatches += 1;
                }
            }
        }
    }
    eprintln!("{checked} combinations checked, {mismatches} mismatches");
    assert_eq!(
        mismatches,
        0,
        "{mismatches} combinations disagree with the reference:\n{}",
        reports.join("\n")
    );
}

/// Go `TestRedactMatchesReferenceExhaustively`: `redact` against `reference` over every string
/// of length <= 4 over {a,b,A,B} crossed with every secret pair of length <= 3 (the empty
/// secret included): 2,463,725 combinations. Marker-adjacency differences are counted apart
/// from substantive ones, and both must be zero.
#[test]
fn test_redact_matches_reference_exhaustively() {
    // Length 4 is where the depth-2 extension chain lives: redact("aaaa","aa","aa") grows
    // end 2 -> 3 -> 4.
    let max_len = 4;
    let words = words(&["a", "b", "A", "B"], max_len);

    let secrets: Vec<&str> = words
        .iter()
        .filter(|w| w.len() < max_len)
        .map(String::as_str)
        .collect();

    fn collapse(x: &str) -> String {
        let mut x = x.to_owned();
        while x.contains("[redacted][redacted]") {
            x = x.replace("[redacted][redacted]", "[redacted]");
        }
        x
    }

    let (mut checked, mut mismatches, mut adjacency_only) = (0usize, 0usize, 0usize);
    let mut reports: Vec<String> = Vec::new();
    for body in &words {
        for s1 in &secrets {
            for s2 in &secrets {
                checked += 1;
                let got = redact(body, &[s1, s2]);
                let want = reference(body, &[s1, s2]);
                if got == want {
                    continue;
                }
                if collapse(&got) == collapse(&want) {
                    if adjacency_only < 3 {
                        reports.push(format!(
                            "ADJACENCY redact({body:?}, {s1:?}, {s2:?}) = {got:?}, reference says \
                             {want:?} — same bytes redacted, different marker count, so the \
                             run-merge is gone"
                        ));
                    }
                    adjacency_only += 1;
                    continue;
                }
                if mismatches < 5 {
                    reports.push(format!(
                        "SUBSTANTIVE redact({body:?}, {s1:?}, {s2:?}) = {got:?}, reference says {want:?}"
                    ));
                }
                mismatches += 1;
            }
        }
    }
    eprintln!(
        "{checked} combinations checked, {mismatches} substantive mismatches, \
         {adjacency_only} marker-adjacency differences"
    );
    assert_eq!(checked, 2_463_725, "the enumeration changed size");
    assert_eq!(
        mismatches,
        0,
        "{mismatches} substantive mismatches:\n{}",
        reports.join("\n")
    );
    assert_eq!(
        adjacency_only,
        0,
        "{adjacency_only} combinations differ from the reference in marker count; redact must \
         emit one marker per redacted run:\n{}",
        reports.join("\n")
    );
}
