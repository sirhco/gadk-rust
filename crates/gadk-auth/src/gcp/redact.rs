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

//! Service-text redaction (port of ADK-Go `auth/gcp/redact.go`).
//!
//! This is the scrub: everything that turns service-controlled text into text an error may
//! carry. It shares `gcp` with the credential client only because the client is its one caller;
//! nothing in it is specific to GCP.
//!
//! Go works on bytes and this port works on `&str`: a service body is decoded with
//! `String::from_utf8_lossy` before it reaches this module. Every offset this module slices at
//! is a char boundary: match starts and ends of a non-empty valid-UTF-8 needle, cuts that
//! [`visible_limit`] backs up to a rune start, and escape boundaries, which are ASCII. The one
//! Go loop that steps by a single byte (`readableOutsideMarkers`, `i = at + 1`) steps to the
//! next char boundary instead, which finds the same matches: a needle that is valid UTF-8 can
//! only start at a char boundary of a valid-UTF-8 haystack. `endOfRun`'s `HasPrefix(ls[k:], lv)`
//! is tested on `as_bytes()`, where `k` may fall inside a char.
//!
//! Every lowering goes through [`go_to_lower`], Go's `strings.ToLower`.

// Nothing outside this module and its tests calls it until Task 8 (`client.rs` `do_post`,
// `map_credential` and the connector operation result). Task 10 removes this allow.
#![allow(dead_code)]

/// maxErrorBody caps service-controlled text carried into an error.
pub(crate) const MAX_ERROR_BODY: usize = 1024; // redact.go:29
/// Replaces service text that could not be shown free of the caller's own identifiers. It
/// names no secret and is constant, so it leaks nothing itself.
pub(crate) const WITHHELD_TEXT: &str =
    "[withheld: could not be shown free of the request's own identifiers]"; // redact.go:33
/// What [`redact`] writes in place of a removed value.
pub(crate) const REDACTED_MARKER: &str = "[redacted]"; // redact.go:36
/// Bounds the caller-supplied values this module will match against a response. Above it,
/// nothing is shown at all: matching costs O(body x value) and half-scrubbing is not an
/// option, so the bound fails closed. Measured on the raw value while matching runs on the
/// lowered copy (redact.go:38-61).
pub(crate) const MAX_SCRUBBABLE_SECRET: usize = 4096; // redact.go:62
/// Bounds how much of the response the whole-text check reads: the visible window plus room
/// for one occurrence's escaped spelling. A response running past it is withheld rather than
/// examined in part (redact.go:126-147).
pub(crate) const MAX_STRADDLE_WINDOW: usize = MAX_ERROR_BODY + 8 * MAX_SCRUBBABLE_SECRET; // redact.go:148
/// Bounds [`decode_fully`]. It counts reads; text that needed exactly this many changing
/// passes is reported unfinished, which is the fail-closed direction (redact.go:307-316).
pub(crate) const MAX_DECODE_PASSES: usize = 64; // redact.go:317

/// Go `utf8.UTFMax`.
const UTF_MAX: usize = 4;

/// redact.go:107. Cycle 2 of Task 5 replaces this.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn redacted_for_error(s: &str, secrets: &[&str]) -> String {
    todo!("Task 5")
}
/// redact.go:161. Cycle 2 of Task 5 replaces this.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn showable(s: &str, secrets: &[&str]) -> Option<String> {
    todo!("Task 5")
}
/// redact.go:200. Cycle 2 of Task 5 replaces this.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn recoverable(x: &str, secrets: &[&str]) -> bool {
    todo!("Task 5")
}
/// redact.go:342. Cycle 2 of Task 5 replaces this.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn decode_fully(s: &str) -> (String, bool) {
    todo!("Task 5")
}
/// redact.go:379. Cycle 2 of Task 5 replaces this.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn unescape_json(s: &str) -> String {
    todo!("Task 5")
}

/// Go `utf8.RuneStart`.
fn rune_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

/// redact.go:454 `visibleLimit`: how many bytes of `s` an error may show, and whether `s` ran
/// past that. A cut backs up at most `UTF_MAX - 1` bytes to a rune boundary.
pub(crate) fn visible_limit(s: &str) -> (usize, bool) {
    const MAX: usize = MAX_ERROR_BODY;
    if s.len() <= MAX {
        return (s.len(), false);
    }
    let b = s.as_bytes();
    let mut cut = MAX;
    let mut i = 0;
    while i < UTF_MAX - 1 && cut > 0 && !rune_start(b[cut]) {
        cut -= 1;
        i += 1;
    }
    // Go's fallback for a body that is not UTF-8. A `&str` always has a rune start within
    // three bytes, so this never fires here; it is kept so the logic stays Go's.
    if !rune_start(b[cut]) {
        cut = MAX;
    }
    (cut, true)
}

/// redact.go:482 `truncateForError`: caps an error body and reports whether it cut. The text
/// and the flag come from one place so a caller cannot pair a flag with the wrong string.
pub(crate) fn truncate_for_error(s: &str) -> (String, bool) {
    let (cut, truncated) = visible_limit(s);
    if !truncated {
        return (s.to_owned(), false);
    }
    (format!("{}...", &s[..cut]), true)
}

/// redact.go:520 `redact`: removes caller-supplied values from a service-controlled string,
/// ignoring case. Where it removes anything, the text it returns is lowercased. Every value is
/// matched in ONE left-to-right pass; overlapping candidates resolve earliest-first, then
/// longest. Text with no match is returned untouched; empty values are dropped.
pub(crate) fn redact(s: &str, values: &[&str]) -> String {
    let lowered = lowered_values(values);
    if lowered.is_empty() {
        return s.to_owned();
    }
    let ls = go_to_lower(s);
    let (out, hit) = redact_lowered(&ls, ls.len(), &lowered);
    if !hit {
        return s.to_owned();
    }
    out
}

/// redact.go:558 `redactWithinLimit`: [`redact`] with the error-body cap applied to the text it
/// READS rather than to the text it returns, and the result capped on its own length as well,
/// with one ellipsis however many bounds cut.
pub(crate) fn redact_within_limit(s: &str, values: &[&str]) -> (String, bool) {
    let lowered = lowered_values(values);
    if lowered.is_empty() {
        return truncate_for_error(s);
    }
    let ls = go_to_lower(s);
    let (limit, truncated) = visible_limit(&ls);
    let (mut out, hit) = redact_lowered(&ls, limit, &lowered);
    if !hit {
        // s is what is returned here, so s is what the flag has to describe.
        return truncate_for_error(s);
    }
    let (cut, over) = visible_limit(&out);
    if over {
        out.truncate(cut);
    }
    if truncated || over {
        out.push_str("...");
    }
    (out, truncated || over)
}

/// redact.go:588 `loweredValues`: drops the empty values and lowercases the rest. Dropping the
/// empty ones is load-bearing: the empty string matches at the cursor forever.
pub(crate) fn lowered_values(values: &[&str]) -> Vec<String> {
    values
        .iter()
        .filter(|v| !v.is_empty())
        .map(|v| go_to_lower(v))
        .collect()
}

/// redact.go:607 `redactLowered`: the scan over `ls`, already lowered, with `lowered`
/// non-empty and lowercased. It emits no byte of `ls` at or past `limit`, and reports whether
/// it wrote a marker. Cost is O(len(ls) x total value length).
///
/// Go's `limit` is an `int` and `limit <= 0` returns `("", false)`; here `0` does. A `limit`
/// inside a char is lowered to that char's start so no slice can panic; no caller passes one
/// ([`visible_limit`] and `ls.len()` are char boundaries).
pub(crate) fn redact_lowered(ls: &str, limit: usize, lowered: &[String]) -> (String, bool) {
    let mut limit = limit.min(ls.len());
    while !ls.is_char_boundary(limit) {
        limit -= 1;
    }
    if limit == 0 {
        return (String::new(), false);
    }

    // next[i] is where lowered[i] matches at or after pos, or None once exhausted.
    let mut next: Vec<Option<usize>> = lowered.iter().map(|lv| ls.find(lv.as_str())).collect();

    let mut b = String::new();
    let (mut pos, mut hit) = (0usize, false);
    while let Some((at, which)) = earliest_match(&next, lowered) {
        if at >= limit {
            // What is left starts at or past the bound and so cannot reach the output.
            break;
        }
        let end = end_of_run(ls, at, at + lowered[which].len(), lowered);

        // One marker per redacted run, not per match. `hit` doubles as "something is already
        // emitted", which makes at == pos mean "this range touches the last one".
        if at > pos || !hit {
            b.push_str(&ls[pos..at]);
            b.push_str(REDACTED_MARKER);
        }
        pos = end;
        hit = true;
        if pos >= limit {
            // The run reaches the bound: it was removed whole rather than cut.
            return (b, true);
        }
        advance_matches(ls, pos, lowered, &mut next);
    }
    if !hit {
        return (String::new(), false);
    }
    b.push_str(&ls[pos..limit]);
    (b, true)
}

/// redact.go:665 `earliestMatch`: the leftmost pending match and, on a tie, the longest, as
/// `(at, which)`; `None` once every value is exhausted.
fn earliest_match(next: &[Option<usize>], lowered: &[String]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (i, n) in next.iter().enumerate() {
        let Some(n) = *n else {
            continue;
        };
        best = match best {
            None => Some((n, i)),
            Some((at, which)) if n < at || (n == at && lowered[i].len() > lowered[which].len()) => {
                Some((n, i))
            }
            keep => keep,
        };
    }
    best
}

/// redact.go:696 `endOfRun`: extends a redacted range while ANY occurrence starts inside it,
/// scanned position by position, and returns where the run ends. The condition re-reads `end`
/// each step, so an extension made at `k` is picked up by the same walk.
fn end_of_run(ls: &str, at: usize, mut end: usize, lowered: &[String]) -> usize {
    let b = ls.as_bytes();
    let mut k = at + 1;
    while k < end {
        for lv in lowered {
            if k + lv.len() > end && b[k..].starts_with(lv.as_bytes()) {
                end = k + lv.len();
            }
        }
        k += 1;
    }
    end
}

/// redact.go:710 `advanceMatches`: moves every pending match the cursor has passed forward to
/// its next occurrence, or retires it.
fn advance_matches(ls: &str, pos: usize, lowered: &[String], next: &mut [Option<usize>]) {
    for (i, lv) in lowered.iter().enumerate() {
        match next[i] {
            Some(n) if n < pos => {}
            _ => continue,
        }
        next[i] = ls[pos..].find(lv.as_str()).map(|j| pos + j);
    }
}

/// Go `strings.ToLower` on valid UTF-8: per char `unicode.ToLower`, i.e. Rust's
/// `char::to_lowercase` except U+0130 → 'i' (Rust gives "i\u{307}"); no final-sigma context.
/// Every lowering above goes through this.
pub(crate) fn go_to_lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\u{130}' {
            out.push('i');
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_to_lower_matches_go_strings_to_lower() {
        // Go: strings.ToLower of each input.
        assert_eq!(go_to_lower("Alice@Example.TEST"), "alice@example.test");
        // U+0130 folds to one byte in Go; Rust's char::to_lowercase gives "i\u{307}".
        assert_eq!(go_to_lower("\u{130}X"), "ix");
        // U+023A grows from two bytes to three (U+2C65).
        assert_eq!(go_to_lower("\u{23a}"), "\u{2c65}");
        // No final-sigma context: Go lowers every Σ to σ, where str::to_lowercase writes ς.
        assert_eq!(go_to_lower("ΑΣ"), "ασ");
        assert_eq!(go_to_lower(""), "");
    }

    #[test]
    fn visible_limit_backs_up_to_a_char_boundary() {
        // 1023 ASCII bytes then a two-byte é straddling the 1024 cap.
        let s = format!("{}é", "x".repeat(MAX_ERROR_BODY - 1));
        assert_eq!(visible_limit(&s), (MAX_ERROR_BODY - 1, true));
        let (t, cut) = truncate_for_error(&s);
        assert!(cut);
        assert_eq!(t, format!("{}...", "x".repeat(MAX_ERROR_BODY - 1)));
        assert_eq!(visible_limit("short"), (5, false));
    }
}
