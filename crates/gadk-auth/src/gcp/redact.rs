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

/// redact.go:107 `redactedForError`. Prepares service-controlled text so that an error may
/// carry it: the caller's own identifiers removed and the source capped, or, where neither can
/// be shown to have worked, [`WITHHELD_TEXT`] instead of any of the response.
///
/// Contract: no secret is recoverable from the returned string by this module's decoder, or
/// nothing is returned. The choice between the two candidates is made on the OUTPUT, never on
/// a property of `s` and its decoded copy, because the service writes both.
pub(crate) fn redacted_for_error(s: &str, secrets: &[&str]) -> String {
    if secrets.iter().any(|v| v.len() > MAX_SCRUBBABLE_SECRET) {
        return WITHHELD_TEXT.to_owned();
    }
    if let Some(out) = showable(s, secrets) {
        return out;
    }
    let u = unescape_json(s);
    if u != s
        && let Some(out) = showable(&u, secrets)
    {
        return out;
    }
    // Neither candidate can be shown clean. The status code, the resource and the sentinel
    // all survive in the error around this.
    WITHHELD_TEXT.to_owned()
}

/// redact.go:156 `fitsStraddleWindow`: whether the whole-text check can examine `s` whole.
fn fits_straddle_window(s: &str) -> bool {
    s.len() <= MAX_STRADDLE_WINDOW
}

/// redact.go:161 `showable`: scrubs `s` for an error; `None` when the result cannot be shown.
pub(crate) fn showable(s: &str, secrets: &[&str]) -> Option<String> {
    let (out, truncated) = redact_within_limit(s, secrets);
    // Asked of the WHOLE text as well, and only when the cap actually cut: an escaped spelling
    // straddling the cut is sliced in half and matches nothing in the visible part. Past the
    // window there is no answer, so the response is withheld unexamined.
    if truncated && (!fits_straddle_window(s) || recoverable(&redact(s, secrets), secrets)) {
        return None;
    }
    // Checked AFTER the cap, so what is examined is exactly what is returned: the ellipsis is
    // appended text, and it can finish a secret the untruncated string only started.
    if recoverable(&out, secrets) {
        return None;
    }
    Some(out)
}

/// redact.go:200 `recoverable`: whether any secret can be read out of `x`, literally or once
/// this module's decoder has been run over it to a fixpoint. The marker this scrub writes is
/// excluded by POSITION: an occurrence belongs to us only when every byte of it came from a
/// marker. Fails closed when either decode cannot finish.
pub(crate) fn recoverable(x: &str, secrets: &[&str]) -> bool {
    let mut forms: Vec<String> = Vec::new();
    for v in secrets {
        if v.is_empty() {
            continue;
        }
        let (decoded, done) = decode_fully(v);
        if !done {
            // Not finishing the decode means not knowing what this text says, and "unknown"
            // has to answer the same way as "yes".
            return true;
        }
        for form in [go_to_lower(v), go_to_lower(&decoded)] {
            if !form.is_empty() {
                forms.push(form);
            }
        }
    }
    if forms.is_empty() {
        return false;
    }
    // Decoded once for the whole text rather than once per marker-separated part.
    let (decoded, done) = decode_fully(x);
    if !done {
        return true;
    }
    for text in [go_to_lower(x), go_to_lower(&decoded)] {
        // Lowered first, then scanned: the marker is already lower case, so it survives the
        // fold unchanged and its offsets are the folded copy's own.
        let ours = marker_bytes(&text);
        for form in &forms {
            if readable_outside_markers(&text, ours.as_deref(), form) {
                return true;
            }
        }
    }
    false
}

/// redact.go:250 `markerBytes`: marks every byte of `text` belonging to an occurrence of the
/// marker, and returns `None` when there is none.
fn marker_bytes(text: &str) -> Option<Vec<bool>> {
    let mut i = text.find(REDACTED_MARKER)?;
    let mut ours = vec![false; text.len()];
    loop {
        ours[i..i + REDACTED_MARKER.len()].fill(true);
        match text[i + REDACTED_MARKER.len()..].find(REDACTED_MARKER) {
            None => break,
            Some(next) => i += REDACTED_MARKER.len() + next,
        }
    }
    Some(ours)
}

/// redact.go:278 `readableOutsideMarkers`: whether `form` occurs anywhere in `text` other than
/// wholly inside bytes this scrub wrote. Overlapping occurrences are walked one start at a
/// time rather than skipped past.
///
/// Go steps `i = at + 1`, a byte. Here `i` moves to the next char boundary after `at`, which
/// skips only positions inside the char at `at`; `form` is non-empty valid UTF-8, so no
/// occurrence can start there, and the matches found are Go's.
fn readable_outside_markers(text: &str, ours: Option<&[bool]>, form: &str) -> bool {
    let mut i = 0;
    while i + form.len() <= text.len() {
        let Some(j) = text[i..].find(form) else {
            return false;
        };
        let at = i + j;
        if !all_ours(ours, at, at + form.len()) {
            return true;
        }
        i = at + 1;
        while !text.is_char_boundary(i) {
            i += 1;
        }
    }
    false
}

/// redact.go:295 `allOurs`: whether every byte of `[lo, hi)` came from a marker. A text with
/// no marker in it owns nothing, so nothing is excused.
fn all_ours(ours: Option<&[bool]>, lo: usize, hi: usize) -> bool {
    match ours {
        None => false,
        Some(ours) => ours[lo..hi].iter().all(|&b| b),
    }
}

/// redact.go:342 `decodeFully`: applies [`unescape_json`] until the text stops changing, and
/// reports whether it got there within [`MAX_DECODE_PASSES`] reads. `false` means the caller
/// must treat the text as though a secret were in it.
pub(crate) fn decode_fully(s: &str) -> (String, bool) {
    let mut s = s.to_owned();
    for _ in 0..MAX_DECODE_PASSES {
        let u = unescape_json(&s);
        if u == s {
            return (s, true);
        }
        s = u;
    }
    (s, false)
}

/// redact.go:379 `unescapeJSON`: decodes the JSON escapes that can hide a caller-supplied value
/// from a substring scrub (`\uXXXX`, a surrogate pair as one rune, `\/` and `\\`) and leaves
/// every other one alone, a malformed escape included, which stays verbatim. An unpaired
/// surrogate becomes U+FFFD, as Go's `strings.Builder.WriteRune` writes it.
pub(crate) fn unescape_json(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut b = String::with_capacity(s.len());
    // `i` is always a char boundary: it starts at 0, every escape is ASCII, and the copy-through
    // branch below ends at a backslash or at the end.
    let mut i = 0;
    while i < s.len() {
        if has_escape(bytes, i, b'\\') {
            b.push('\\');
            i += SHORT_ESCAPE_LEN;
            continue;
        }
        if has_escape(bytes, i, b'/') {
            b.push('/');
            i += SHORT_ESCAPE_LEN;
            continue;
        }
        let Some(r) = decode_unicode_escape(bytes, i) else {
            // Not an escape this function decodes, or a malformed one. Either way the bytes
            // are copied through rather than dropped. Go copies one byte per iteration; every
            // escape starts with a backslash, so copying up to the next one is the same.
            if bytes[i] == b'\\' {
                b.push('\\');
                i += 1;
            } else {
                let end = s[i..].find('\\').map_or(s.len(), |j| i + j);
                b.push_str(&s[i..end]);
                i = end;
            }
            continue;
        };
        // A non-BMP rune arrives as a surrogate PAIR, and the two halves have to be decoded
        // together: apart, each becomes U+FFFD and the rune survives the scrub whole.
        if is_surrogate(r)
            && let Some(lo) = decode_unicode_escape(bytes, i + UNICODE_ESCAPE_LEN)
            && let Some(paired) = decode_surrogate_pair(r, lo)
        {
            b.push(paired);
            i += SURROGATE_PAIR_LEN;
            continue;
        }
        b.push(char::from_u32(r).unwrap_or(char::REPLACEMENT_CHARACTER));
        i += UNICODE_ESCAPE_LEN;
    }
    b
}

/// Widths in bytes of the escapes [`unescape_json`] decodes (redact.go:424-428).
const SHORT_ESCAPE_LEN: usize = 2; // \\ and \/
const UNICODE_ESCAPE_LEN: usize = 6; // \uXXXX
const SURROGATE_PAIR_LEN: usize = 2 * UNICODE_ESCAPE_LEN; // \uD83D\uDE00

/// Go `utf16.IsSurrogate`.
fn is_surrogate(r: u32) -> bool {
    (0xD800..0xE000).contains(&r)
}

/// Go `utf16.DecodeRune`, with `None` for its `utf8.RuneError` result.
fn decode_surrogate_pair(r1: u32, r2: u32) -> Option<char> {
    if (0xD800..0xDC00).contains(&r1) && (0xDC00..0xE000).contains(&r2) {
        char::from_u32((((r1 - 0xD800) << 10) | (r2 - 0xDC00)) + 0x10000)
    } else {
        None
    }
}

/// redact.go:431 `hasEscape`: whether `s[i..]` begins with a backslash followed by `c`.
fn has_escape(s: &[u8], i: usize, c: u8) -> bool {
    i + SHORT_ESCAPE_LEN <= s.len() && s[i] == b'\\' && s[i + 1] == c
}

/// redact.go:440 `decodeUnicodeEscape`: decodes the `\uXXXX` at `s[i..]`, if that is what is
/// there. Exactly four hex digits parse, as `strconv.ParseUint(_, 16, 32)` admits: no sign,
/// no `0x`, no underscores.
fn decode_unicode_escape(s: &[u8], i: usize) -> Option<u32> {
    if !has_escape(s, i, b'u') || i + UNICODE_ESCAPE_LEN > s.len() {
        return None;
    }
    let mut n = 0u32;
    for &d in &s[i + SHORT_ESCAPE_LEN..i + UNICODE_ESCAPE_LEN] {
        n = n * 16 + char::from(d).to_digit(16)?;
    }
    Some(n)
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

    #[test]
    fn readable_outside_markers_steps_over_multibyte_chars() {
        // A form that only matches after a multi-byte char: the char-boundary step must not
        // panic and must still find the second occurrence outside the marker.
        let text = "é[redacted]éa";
        let ours = marker_bytes(text);
        assert!(readable_outside_markers(text, ours.as_deref(), "éa"));
        assert!(!readable_outside_markers(text, ours.as_deref(), "ed"));
    }
}
