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

//! A parser for the regular-expression dialect of ADK's tool search (RE2 syntax with Perl flags,
//! the dialect of Go's `regexp/syntax`), and the few UTF-8 and string helpers `ranksearch` needs,
//! so that the regex-or-keywords classification, the pattern-size estimate and the "pattern not
//! run" notes see the same parse trees and error codes as ADK.
//!
//! What is implemented:
//! - [`parse`] with all its error codes and expression texts, on byte strings (a byte string may
//!   hold invalid UTF-8, which is reported as `invalid UTF-8`);
//! - the parse-time tree shaping `ranksearch` inspects: literal concatenation (`maybe_concat`),
//!   single-rune classes becoming literals, `collapse`, `swap_vertical_bar`, `clean_alt` and the
//!   four `factor` rounds of an alternation;
//! - character classes as flat `[lo, hi, lo, hi, ...]` rune vectors, with simple case folding.
//!
//! What is not implemented:
//! - the literal and POSIX-only modes (`ranksearch` parses with [`PERL`] only; the flag checks are
//!   kept, so other flag sets parse as in that dialect apart from `LITERAL`);
//! - the `expression nests too deeply` and `expression too large` limits, which a query of at most
//!   200 runes cannot reach (the parse tree is at most a few hundred levels deep and its compiled
//!   size is far below the 3.3 million instruction limit);
//! - Unicode tables of its own: `\p{...}` classes and case folding come from `regex-syntax`, so
//!   they follow its Unicode version, and `\p{...}` names resolve through its general categories
//!   and scripts, which also accept a few names the dialect rejects (four-letter script codes such
//!   as `Grek`).
//!
//! [`to_hir`] turns a parse tree into a `regex-syntax` HIR with the dialect's matching semantics
//! (ASCII `\b`, `\d`, `\s`, `\w` and POSIX classes, `$` as `\z`, case folding already applied), so
//! the `regex` crate matches what ADK's tool search would.

use regex_syntax::hir::{
    Capture, Class, ClassUnicode, ClassUnicodeRange, Dot, Hir, HirKind, Look, Repetition,
};

/// The largest rune (the largest Unicode code point).
const MAX_RUNE: i32 = 0x10_FFFF;
/// The rune that stands for an invalid byte (U+FFFD).
const RUNE_ERROR: i32 = 0xFFFD;
/// The smallest rune involved in case folding.
const MIN_FOLD: i32 = 0x0041;
/// The largest rune involved in case folding.
const MAX_FOLD: i32 = 0x1_E943;
/// Ranges up to this many runes are folded rune by rune, in orbit order (see
/// [`append_folded_range`]).
const BRUTE_FORCE_FOLD: i32 = 64;

/// Parse flags.
pub(crate) type Flags = u16;
/// Case-insensitive match.
pub(crate) const FOLD_CASE: Flags = 1 << 0;
/// Allow character classes like `[^a-z]` and `[[:space:]]` to match newline.
pub(crate) const CLASS_NL: Flags = 1 << 2;
/// Allow `.` to match newline.
pub(crate) const DOT_NL: Flags = 1 << 3;
/// Treat `^` and `$` as only matching at beginning and end of text.
pub(crate) const ONE_LINE: Flags = 1 << 4;
/// Make repetition operators default to non-greedy.
pub(crate) const NON_GREEDY: Flags = 1 << 5;
/// Allow Perl extensions.
pub(crate) const PERL_X: Flags = 1 << 6;
/// Allow `\p{Han}`, `\P{Han}` for Unicode group and negation.
pub(crate) const UNICODE_GROUPS: Flags = 1 << 7;
/// An `EndText` that was `$`, not `\z`.
pub(crate) const WAS_DOLLAR: Flags = 1 << 8;
/// The flags for syntax as close to Perl as possible. `ranksearch` parses with these flags.
pub(crate) const PERL: Flags = CLASS_NL | ONE_LINE | PERL_X | UNICODE_GROUPS;

/// The operator of a parse-tree node, in an order that matters to `swap_vertical_bar` and
/// `factor`. The last two are parse-stack pseudo-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Op {
    NoMatch,
    EmptyMatch,
    Literal,
    CharClass,
    AnyCharNotNl,
    AnyChar,
    BeginLine,
    EndLine,
    BeginText,
    EndText,
    WordBoundary,
    NoWordBoundary,
    Capture,
    Star,
    Plus,
    Quest,
    Repeat,
    Concat,
    Alternate,
    LeftParen,
    VerticalBar,
}

impl Op {
    /// Whether this is a parse-stack pseudo-op.
    fn is_pseudo(self) -> bool {
        self >= Op::LeftParen
    }
}

/// A node of a parse tree. `rune` holds a literal's runes, or a class's ranges as a flat
/// `[lo, hi, lo, hi, ...]` vector. Runes are `i32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Regexp {
    pub(crate) op: Op,
    pub(crate) flags: Flags,
    pub(crate) sub: Vec<Regexp>,
    pub(crate) rune: Vec<i32>,
    pub(crate) min: i32,
    pub(crate) max: i32,
    pub(crate) cap: i32,
    pub(crate) name: Vec<u8>,
}

impl Regexp {
    fn new(op: Op) -> Self {
        Regexp {
            op,
            flags: 0,
            sub: Vec::new(),
            rune: Vec::new(),
            min: 0,
            max: 0,
            cap: 0,
            name: Vec::new(),
        }
    }

    /// Structural equality of two nodes.
    fn equal(&self, y: &Regexp) -> bool {
        if self.op != y.op {
            return false;
        }
        match self.op {
            Op::EndText => self.flags & WAS_DOLLAR == y.flags & WAS_DOLLAR,
            Op::Literal | Op::CharClass => {
                self.flags & FOLD_CASE == y.flags & FOLD_CASE && self.rune == y.rune
            }
            Op::Alternate | Op::Concat => {
                self.sub.len() == y.sub.len()
                    && self.sub.iter().zip(&y.sub).all(|(a, b)| a.equal(b))
            }
            Op::Star | Op::Plus | Op::Quest => {
                self.flags & NON_GREEDY == y.flags & NON_GREEDY && first_sub_equal(self, y)
            }
            Op::Repeat => {
                self.flags & NON_GREEDY == y.flags & NON_GREEDY
                    && self.min == y.min
                    && self.max == y.max
                    && first_sub_equal(self, y)
            }
            Op::Capture => self.cap == y.cap && self.name == y.name && first_sub_equal(self, y),
            _ => true,
        }
    }
}

fn first_sub_equal(x: &Regexp, y: &Regexp) -> bool {
    match (x.sub.first(), y.sub.first()) {
        (Some(a), Some(b)) => a.equal(b),
        (None, None) => true,
        _ => false,
    }
}

/// The reason a parse failed. `invalid character class` and `expression nests too deeply` are
/// never produced for the queries `ranksearch` parses (see the module docs), so they are absent;
/// `Large` is produced by `ranksearch` when the `regex` crate refuses a pattern that parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorCode {
    InvalidCharRange,
    InvalidEscape,
    InvalidNamedCapture,
    InvalidPerlOp,
    InvalidRepeatOp,
    InvalidRepeatSize,
    InvalidUtf8,
    MissingBracket,
    MissingParen,
    MissingRepeatArgument,
    TrailingBackslash,
    UnexpectedParen,
    Large,
}

impl ErrorCode {
    /// The error-code text.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidCharRange => "invalid character class range",
            ErrorCode::InvalidEscape => "invalid escape sequence",
            ErrorCode::InvalidNamedCapture => "invalid named capture",
            ErrorCode::InvalidPerlOp => "invalid or unsupported Perl syntax",
            ErrorCode::InvalidRepeatOp => "invalid nested repetition operator",
            ErrorCode::InvalidRepeatSize => "invalid repeat count",
            ErrorCode::InvalidUtf8 => "invalid UTF-8",
            ErrorCode::MissingBracket => "missing closing ]",
            ErrorCode::MissingParen => "missing closing )",
            ErrorCode::MissingRepeatArgument => "missing argument to repetition operator",
            ErrorCode::TrailingBackslash => "trailing backslash at end of expression",
            ErrorCode::UnexpectedParen => "unexpected )",
            ErrorCode::Large => "expression too large",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parse error: the code and the offending expression, as bytes (it may hold invalid UTF-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Error {
    pub(crate) code: ErrorCode,
    pub(crate) expr: Vec<u8>,
}

fn error(code: ErrorCode, expr: &[u8]) -> Error {
    Error {
        code,
        expr: expr.to_vec(),
    }
}

/// A char as a rune.
pub(crate) fn rune(c: char) -> i32 {
    // A char is at most 0x10FFFF, so it always fits.
    i32::try_from(u32::from(c)).unwrap_or(RUNE_ERROR)
}

/// A rune as a char, if it is a Unicode scalar value.
fn to_char(r: i32) -> Option<char> {
    u32::try_from(r).ok().and_then(char::from_u32)
}

// ---------------------------------------------------------------------------------------------
// UTF-8 and string helpers on byte strings.
// ---------------------------------------------------------------------------------------------

/// Decodes the first rune of `s` and its width; `(RuneError, 1)` for an invalid byte and
/// `(RuneError, 0)` for an empty `s`.
pub(crate) fn decode_rune(s: &[u8]) -> (i32, usize) {
    let Some(&b0) = s.first() else {
        return (RUNE_ERROR, 0);
    };
    let width = match b0 {
        0x00..=0x7F => return (i32::from(b0), 1),
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (RUNE_ERROR, 1),
    };
    match s
        .get(..width)
        .and_then(|p| std::str::from_utf8(p).ok())
        .and_then(|p| p.chars().next())
    {
        Some(c) => (rune(c), width),
        None => (RUNE_ERROR, 1),
    }
}

/// Decodes the last rune of `s` and its width; `(RuneError, 1)` for an invalid byte, `(RuneError,
/// 0)` for an empty `s`.
fn decode_last_rune(s: &[u8]) -> (i32, usize) {
    let end = s.len();
    let Some(&last) = s.last() else {
        return (RUNE_ERROR, 0);
    };
    if last < 0x80 {
        return (i32::from(last), 1);
    }
    let lim = end.saturating_sub(4);
    let mut start = end - 1;
    while start > lim {
        start -= 1;
        if s[start] & 0xC0 != 0x80 {
            break;
        }
    }
    let (r, size) = decode_rune(&s[start..]);
    if start + size != end {
        return (RUNE_ERROR, 1);
    }
    (r, size)
}

/// Whether `r` is white space (the White_Space property, which `char::is_whitespace` follows).
fn is_space(r: i32) -> bool {
    to_char(r).is_some_and(char::is_whitespace)
}

/// Trims white space from both ends. An invalid byte decodes as `RuneError`, which is not a space.
pub(crate) fn trim_space(s: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < s.len() {
        let (r, size) = decode_rune(&s[start..]);
        if !is_space(r) {
            break;
        }
        start += size;
    }
    let mut end = s.len();
    while end > start {
        let (r, size) = decode_last_rune(&s[start..end]);
        if !is_space(r) {
            break;
        }
        end -= size;
    }
    &s[start..end]
}

/// Whether `s` contains any white space.
pub(crate) fn contains_space(s: &[u8]) -> bool {
    let mut t = s;
    while !t.is_empty() {
        let (r, size) = decode_rune(t);
        if is_space(r) {
            return true;
        }
        t = &t[size..];
    }
    false
}

/// The number of runes in `s`: an invalid byte counts as one rune.
pub(crate) fn rune_count(s: &[u8]) -> usize {
    let mut t = s;
    let mut n = 0;
    while !t.is_empty() {
        let (_, size) = decode_rune(t);
        t = &t[size..];
        n += 1;
    }
    n
}

/// Converts `s` to a `String`: each run of invalid bytes becomes one `replacement`.
pub(crate) fn to_valid_utf8(s: &[u8], replacement: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut invalid = false;
    let mut t = s;
    while !t.is_empty() {
        let (r, size) = decode_rune(t);
        t = &t[size..];
        match to_char(r) {
            Some(c) if !(r == RUNE_ERROR && size == 1) => {
                invalid = false;
                out.push(c);
            }
            _ => {
                if !invalid {
                    invalid = true;
                    out.push_str(replacement);
                }
            }
        }
    }
    out
}

/// Fails with `invalid UTF-8` when `s` holds an invalid byte.
fn check_utf8(s: &[u8]) -> Result<(), Error> {
    let mut t = s;
    while !t.is_empty() {
        let (r, size) = decode_rune(t);
        if r == RUNE_ERROR && size == 1 {
            return Err(error(ErrorCode::InvalidUtf8, t));
        }
        t = &t[size..];
    }
    Ok(())
}

/// Decodes the next rune, failing with `invalid UTF-8` on an invalid byte.
fn next_rune(s: &[u8]) -> Result<(i32, &[u8]), Error> {
    let (c, size) = decode_rune(s);
    if c == RUNE_ERROR && size == 1 {
        return Err(error(ErrorCode::InvalidUtf8, s));
    }
    Ok((c, &s[size..]))
}

fn index_byte(s: &[u8], b: u8) -> Option<usize> {
    s.iter().position(|&c| c == b)
}

fn index_of(s: &[u8], sep: &[u8]) -> Option<usize> {
    s.windows(sep.len()).position(|w| w == sep)
}

// ---------------------------------------------------------------------------------------------
// Unicode tables and case folding, from regex-syntax.
// ---------------------------------------------------------------------------------------------

/// The ranges of a `regex-syntax` Unicode property such as `gc=L` or `sc=Greek`, as a flat rune
/// vector, or `None` if `regex-syntax` does not know the property.
pub(crate) fn property_ranges(property: &str) -> Option<Vec<i32>> {
    let hir = regex_syntax::ParserBuilder::new()
        .build()
        .parse(&format!("\\p{{{property}}}"))
        .ok()?;
    match hir.kind() {
        HirKind::Class(Class::Unicode(cls)) => Some(
            cls.iter()
                .flat_map(|r| [rune(r.start()), rune(r.end())])
                .collect(),
        ),
        _ => None,
    }
}

/// Reports whether `r` is in the clean class `class`.
pub(crate) fn in_char_class(r: i32, class: &[i32]) -> bool {
    let pairs = class.len() / 2;
    let (mut lo, mut hi) = (0, pairs);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if r > class[2 * mid + 1] {
            lo = mid + 1;
        } else if r < class[2 * mid] {
            hi = mid;
        } else {
            return true;
        }
    }
    false
}

/// The `regex-syntax` ranges covering the scalar values in `lo..=hi` (surrogates are skipped).
fn char_ranges(lo: i32, hi: i32) -> Vec<ClassUnicodeRange> {
    let mut out = Vec::with_capacity(2);
    let lo = lo.max(0);
    let hi = hi.min(MAX_RUNE);
    for (a, b) in [(lo, hi.min(0xD7FF)), (lo.max(0xE000), hi)] {
        if a <= b
            && let (Some(a), Some(b)) = (to_char(a), to_char(b))
        {
            out.push(ClassUnicodeRange::new(a, b));
        }
    }
    out
}

/// The `(lo, hi)` ranges of a flat class vector.
fn pairs(class: &[i32]) -> impl Iterator<Item = (i32, i32)> + '_ {
    class.as_chunks::<2>().0.iter().map(|&[lo, hi]| (lo, hi))
}

fn class_unicode(class: &[i32]) -> ClassUnicode {
    ClassUnicode::new(pairs(class).flat_map(|(lo, hi)| char_ranges(lo, hi)))
}

/// The case-fold orbit of `r` (all runes that simple case folding cycles through from `r`, `r`
/// included), sorted.
fn orbit(r: i32) -> Vec<i32> {
    let Some(c) = to_char(r) else {
        return vec![r];
    };
    let mut cls = ClassUnicode::new([ClassUnicodeRange::new(c, c)]);
    if cls.try_case_fold_simple().is_err() {
        return vec![r];
    }
    cls.iter()
        .flat_map(|rg| rune(rg.start())..=rune(rg.end()))
        .collect()
}

/// Whether `a` and `b` form a two-rune fold orbit.
fn fold_pair(a: i32, b: i32) -> bool {
    a != b && orbit(a) == [a.min(b), a.max(b)]
}

/// The smallest rune fold-equivalent to `r`.
fn min_fold_rune(r: i32) -> i32 {
    if !(MIN_FOLD..=MAX_FOLD).contains(&r) {
        return r;
    }
    orbit(r).first().copied().unwrap_or(r)
}

/// The runes fold-equivalent to some rune of `class` (`class` included), as ranges.
fn fold_closure(class: &[i32]) -> Vec<i32> {
    let mut cls = class_unicode(class);
    if cls.try_case_fold_simple().is_err() {
        return class.to_vec();
    }
    cls.iter()
        .flat_map(|r| [rune(r.start()), rune(r.end())])
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Character classes.
// ---------------------------------------------------------------------------------------------

/// The classes of `\d`, `\s` and `\w`.
const CODE_DIGIT: &[i32] = &[0x30, 0x39];
const CODE_SPACE: &[i32] = &[0x9, 0xa, 0xc, 0xd, 0x20, 0x20];
const CODE_WORD: &[i32] = &[0x30, 0x39, 0x41, 0x5a, 0x5f, 0x5f, 0x61, 0x7a];

/// The sign and class of a Perl class escape.
fn perl_group(name: &[u8]) -> Option<(i32, &'static [i32])> {
    Some(match name {
        b"\\d" => (1, CODE_DIGIT),
        b"\\D" => (-1, CODE_DIGIT),
        b"\\s" => (1, CODE_SPACE),
        b"\\S" => (-1, CODE_SPACE),
        b"\\w" => (1, CODE_WORD),
        b"\\W" => (-1, CODE_WORD),
        _ => return None,
    })
}

/// The sign and class of a POSIX class name such as `[:alpha:]`.
fn posix_group(name: &[u8]) -> Option<(i32, &'static [i32])> {
    let (sign, base) = match name.strip_prefix(b"[:^") {
        Some(rest) => (-1, rest),
        None => (1, name.strip_prefix(b"[:")?),
    };
    let class: &'static [i32] = match base {
        b"alnum:]" => &[0x30, 0x39, 0x41, 0x5a, 0x61, 0x7a],
        b"alpha:]" => &[0x41, 0x5a, 0x61, 0x7a],
        b"ascii:]" => &[0x0, 0x7f],
        b"blank:]" => &[0x9, 0x9, 0x20, 0x20],
        b"cntrl:]" => &[0x0, 0x1f, 0x7f, 0x7f],
        b"digit:]" => &[0x30, 0x39],
        b"graph:]" => &[0x21, 0x7e],
        b"lower:]" => &[0x61, 0x7a],
        b"print:]" => &[0x20, 0x7e],
        b"punct:]" => &[0x21, 0x2f, 0x3a, 0x40, 0x5b, 0x60, 0x7b, 0x7e],
        b"space:]" => &[0x9, 0xd, 0x20, 0x20],
        b"upper:]" => &[0x41, 0x5a],
        b"word:]" => &[0x30, 0x39, 0x41, 0x5a, 0x5f, 0x5f, 0x61, 0x7a],
        b"xdigit:]" => &[0x30, 0x39, 0x41, 0x46, 0x61, 0x66],
        _ => return None,
    };
    Some((sign, class))
}

/// Appends the range `[lo, hi]` to a class vector, merging it into one of the last two ranges when
/// they touch.
fn append_range(r: &mut Vec<i32>, lo: i32, hi: i32) {
    let n = r.len();
    for i in [2, 4] {
        if n >= i {
            let (rlo, rhi) = (r[n - i], r[n - i + 1]);
            if lo <= rhi + 1 && rlo <= hi + 1 {
                if lo < rlo {
                    r[n - i] = lo;
                }
                if hi > rhi {
                    r[n - i + 1] = hi;
                }
                return;
            }
        }
    }
    r.push(lo);
    r.push(hi);
}

/// Appends the range `[lo, hi]` and everything fold-equivalent to it. Short ranges add each rune
/// and then its orbit, so the (not yet cleaned) vector has a fixed order; long ranges add the
/// folded closure as ranges, which gives the same set.
fn append_folded_range(r: &mut Vec<i32>, lo: i32, hi: i32) {
    if lo <= MIN_FOLD && hi >= MAX_FOLD {
        // Range is full: folding can't add more.
        append_range(r, lo, hi);
        return;
    }
    if hi < MIN_FOLD || lo > MAX_FOLD {
        // Range is outside folding possibilities.
        append_range(r, lo, hi);
        return;
    }
    let (mut lo, mut hi) = (lo, hi);
    if lo < MIN_FOLD {
        append_range(r, lo, MIN_FOLD - 1);
        lo = MIN_FOLD;
    }
    if hi > MAX_FOLD {
        append_range(r, MAX_FOLD + 1, hi);
        hi = MAX_FOLD;
    }
    if hi - lo < BRUTE_FORCE_FOLD {
        for c in lo..=hi {
            append_range(r, c, c);
            // The orbit of `c` in ascending order from `c`, wrapping around.
            let o = orbit(c);
            for &f in o
                .iter()
                .filter(|&&f| f > c)
                .chain(o.iter().filter(|&&f| f < c))
            {
                append_range(r, f, f);
            }
        }
    } else {
        append_range(r, lo, hi);
        let folded = fold_closure(&[lo, hi]);
        for (a, b) in pairs(&folded) {
            append_range(r, a, b);
        }
    }
}

/// Appends one rune, with its case-fold orbit when `flags` fold case.
fn append_literal(r: &mut Vec<i32>, x: i32, flags: Flags) {
    if flags & FOLD_CASE != 0 {
        append_folded_range(r, x, x);
    } else {
        append_range(r, x, x);
    }
}

/// Appends every range of the class `x`.
fn append_class(r: &mut Vec<i32>, x: &[i32]) {
    for (lo, hi) in pairs(x) {
        append_range(r, lo, hi);
    }
}

/// Appends the complement of the class `x`.
fn append_negated_class(r: &mut Vec<i32>, x: &[i32]) {
    let mut next_lo = 0;
    for (lo, hi) in pairs(x) {
        if next_lo < lo {
            append_range(r, next_lo, lo - 1);
        }
        next_lo = hi + 1;
    }
    if next_lo <= MAX_RUNE {
        append_range(r, next_lo, MAX_RUNE);
    }
}

/// Sorts the ranges, merges them, and eliminates duplicates.
fn clean_class(r: &mut Vec<i32>) {
    let mut ranges: Vec<(i32, i32)> = pairs(r).collect();
    // Sort by lo increasing, hi decreasing to break ties.
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    r.clear();
    for (lo, hi) in ranges {
        if let Some(last_hi) = r.last_mut()
            && lo <= *last_hi + 1
        {
            if hi > *last_hi {
                *last_hi = hi;
            }
            continue;
        }
        r.push(lo);
        r.push(hi);
    }
}

/// Replaces a clean class with its complement.
fn negate_class(r: &mut Vec<i32>) {
    let mut out = Vec::with_capacity(r.len() + 2);
    let mut next_lo = 0;
    for (lo, hi) in pairs(r) {
        if next_lo < lo {
            out.push(next_lo);
            out.push(lo - 1);
        }
        next_lo = hi + 1;
    }
    if next_lo <= MAX_RUNE {
        out.push(next_lo);
        out.push(MAX_RUNE);
    }
    *r = out;
}

/// Cleans a class node's ranges, turning a full class into `.` (any char) and a class of all but
/// `\n` into `.` (any char but newline).
fn clean_alt(re: &mut Regexp) {
    if re.op == Op::CharClass {
        clean_class(&mut re.rune);
        if re.rune == [0, MAX_RUNE] {
            re.rune.clear();
            re.op = Op::AnyChar;
        } else if re.rune == [0, '\n' as i32 - 1, '\n' as i32 + 1, MAX_RUNE] {
            re.rune.clear();
            re.op = Op::AnyCharNotNl;
        }
    }
}

/// Whether `re` is a single-rune literal, a class, `.`, or `(?s:.)`.
fn is_char_class(re: &Regexp) -> bool {
    re.op == Op::Literal && re.rune.len() == 1
        || matches!(re.op, Op::CharClass | Op::AnyCharNotNl | Op::AnyChar)
}

/// Whether the single-char node `re` matches `r`.
fn match_rune(re: &Regexp, r: i32) -> bool {
    match re.op {
        Op::Literal => re.rune.len() == 1 && re.rune[0] == r,
        Op::CharClass => pairs(&re.rune).any(|(lo, hi)| lo <= r && r <= hi),
        Op::AnyCharNotNl => r != '\n' as i32,
        Op::AnyChar => true,
        _ => false,
    }
}

/// Merges `src` into `dst` (`dst = dst|src`), where `dst.op >= src.op`.
fn merge_char_class(dst: &mut Regexp, src: &Regexp) {
    match dst.op {
        Op::AnyChar => {}
        Op::AnyCharNotNl => {
            if match_rune(src, '\n' as i32) {
                dst.op = Op::AnyChar;
            }
        }
        Op::CharClass => {
            if src.op == Op::Literal {
                if let Some(&r) = src.rune.first() {
                    append_literal(&mut dst.rune, r, src.flags);
                }
            } else {
                append_class(&mut dst.rune, &src.rune);
            }
        }
        Op::Literal => {
            let (Some(&d), Some(&s)) = (dst.rune.first(), src.rune.first()) else {
                return;
            };
            if s == d && src.flags == dst.flags {
                return;
            }
            dst.op = Op::CharClass;
            dst.rune.clear();
            append_literal(&mut dst.rune, d, dst.flags);
            append_literal(&mut dst.rune, s, src.flags);
        }
        _ => {}
    }
}

/// Whether the nested repeat counts under `re` multiply to no more than `n`.
fn repeat_is_valid(re: &Regexp, mut n: i32) -> bool {
    if re.op == Op::Repeat {
        let mut m = re.max;
        if m == 0 {
            return true;
        }
        if m < 0 {
            m = re.min;
        }
        if m > n {
            return false;
        }
        if m > 0 {
            n /= m;
        }
    }
    re.sub.iter().all(|sub| repeat_is_valid(sub, n))
}

// --------------------------------------------------------------------------------------------- The
// parser.
// ---------------------------------------------------------------------------------------------

struct Parser<'a> {
    flags: Flags,
    stack: Vec<Regexp>,
    num_cap: i32,
    whole_regexp: &'a [u8],
}

/// Parses a byte string. `ranksearch` passes [`PERL`].
pub(crate) fn parse(s: &[u8], flags: Flags) -> Result<Regexp, Error> {
    let mut p = Parser {
        flags,
        stack: Vec::new(),
        num_cap: 0,
        whole_regexp: s,
    };
    let mut t = s;
    let mut last_repeat: &[u8] = &[];
    while let Some(&c0) = t.first() {
        let mut repeat: &[u8] = &[];
        match c0 {
            b'(' => {
                if p.flags & PERL_X != 0 && t.get(1) == Some(&b'?') {
                    // Flag changes and non-capturing groups.
                    t = p.parse_perl_flags(t)?;
                } else {
                    p.num_cap += 1;
                    let mut re = Regexp::new(Op::LeftParen);
                    re.flags = p.flags;
                    re.cap = p.num_cap;
                    p.push(re);
                    t = &t[1..];
                }
            }
            b'|' => {
                p.parse_vertical_bar();
                t = &t[1..];
            }
            b')' => {
                p.parse_right_paren()?;
                t = &t[1..];
            }
            b'^' => {
                if p.flags & ONE_LINE != 0 {
                    p.op(Op::BeginText);
                } else {
                    p.op(Op::BeginLine);
                }
                t = &t[1..];
            }
            b'$' => {
                if p.flags & ONE_LINE != 0 {
                    let mut re = Regexp::new(Op::EndText);
                    re.flags = p.flags | WAS_DOLLAR;
                    p.push(re);
                } else {
                    p.op(Op::EndLine);
                }
                t = &t[1..];
            }
            b'.' => {
                if p.flags & DOT_NL != 0 {
                    p.op(Op::AnyChar);
                } else {
                    p.op(Op::AnyCharNotNl);
                }
                t = &t[1..];
            }
            b'[' => {
                t = p.parse_class(t)?;
            }
            b'*' | b'+' | b'?' => {
                let before = t;
                let op = match c0 {
                    b'*' => Op::Star,
                    b'+' => Op::Plus,
                    _ => Op::Quest,
                };
                let after = &t[1..];
                t = p.repeat(op, 0, 0, before, after, last_repeat)?;
                repeat = before;
            }
            b'{' => {
                let before = t;
                match parse_repeat(t) {
                    None => {
                        // If the repeat cannot be parsed, { is a literal.
                        p.literal('{' as i32);
                        t = &t[1..];
                    }
                    Some((min, max, after)) => {
                        if !(0..=1000).contains(&min) || max > 1000 || max >= 0 && min > max {
                            // Numbers were too big, or max is present and min > max.
                            return Err(error(
                                ErrorCode::InvalidRepeatSize,
                                &before[..before.len() - after.len()],
                            ));
                        }
                        t = p.repeat(Op::Repeat, min, max, before, after, last_repeat)?;
                        repeat = before;
                    }
                }
            }
            b'\\' => {
                t = p.parse_backslash(t)?;
            }
            _ => {
                let (c, rest) = next_rune(t)?;
                p.literal(c);
                t = rest;
            }
        }
        last_repeat = repeat;
    }

    p.concat();
    if p.swap_vertical_bar() {
        // pop vertical bar
        p.stack.pop();
    }
    p.alternate();

    if p.stack.len() != 1 {
        return Err(error(ErrorCode::MissingParen, s));
    }
    p.stack
        .pop()
        .ok_or_else(|| error(ErrorCode::MissingParen, s))
}

/// Parses `{min}` (max = min), `{min,}` (max = -1) or `{min,max}`. `None` if `s` is not of that
/// form; `min == -1` if a number is too big.
fn parse_repeat(s: &[u8]) -> Option<(i32, i32, &[u8])> {
    let s = s.strip_prefix(b"{")?;
    let (mut min, mut s) = parse_int(s)?;
    let max;
    match s.first()? {
        b',' => {
            s = &s[1..];
            if *s.first()? == b'}' {
                max = -1;
            } else {
                let (m, rest) = parse_int(s)?;
                s = rest;
                max = m;
                if max < 0 {
                    // `parse_int` found too big a number
                    min = -1;
                }
            }
        }
        _ => max = min,
    }
    let rest = s.strip_prefix(b"}")?;
    Some((min, max, rest))
}

/// Parses a decimal integer without leading zeros; -1 if it is too big.
fn parse_int(s: &[u8]) -> Option<(i32, &[u8])> {
    if !s.first()?.is_ascii_digit() {
        return None;
    }
    // Disallow leading zeros.
    if s.len() >= 2 && s[0] == b'0' && s[1].is_ascii_digit() {
        return None;
    }
    let digits = s.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut n: i32 = 0;
    for &d in &s[..digits] {
        // Avoid overflow.
        if n >= 100_000_000 {
            n = -1;
            break;
        }
        n = n * 10 + i32::from(d - b'0');
    }
    Some((n, &s[digits..]))
}

/// Whether `name` is a valid capture name: `[A-Za-z0-9_]+`.
fn is_valid_capture_name(name: &[u8]) -> bool {
    !name.is_empty() && name.iter().all(|&c| c == b'_' || c.is_ascii_alphanumeric())
}

/// The value of a hex digit, or -1.
fn unhex(c: i32) -> i32 {
    match u8::try_from(c) {
        Ok(b @ b'0'..=b'9') => i32::from(b - b'0'),
        Ok(b @ b'a'..=b'f') => i32::from(b - b'a') + 10,
        Ok(b @ b'A'..=b'F') => i32::from(b - b'A') + 10,
        _ => -1,
    }
}

/// Whether `c` is an ASCII letter or digit.
fn is_alnum(c: i32) -> bool {
    u8::try_from(c).is_ok_and(|b| b.is_ascii_alphanumeric())
}

/// Canonicalizes a Unicode table name: a leading uppercase letter, then lowercase letters, without
/// `_`, `-` or spaces.
fn canonical_name(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    let mut first = true;
    for &c in name {
        match c {
            b'_' | b'-' | b' ' => {}
            _ if first => {
                out.push(c.to_ascii_uppercase());
                first = false;
            }
            _ => out.push(c.to_ascii_lowercase()),
        }
    }
    out
}

/// The table named `name` and its sign. The special names (`Any`, `Assigned`, `Ascii`) are handled
/// here; general categories and scripts come from `regex-syntax`.
fn unicode_table(name: &[u8]) -> Option<(Vec<i32>, i32)> {
    let name = canonical_name(name);
    match name.as_slice() {
        b"Any" => return Some((vec![0, MAX_RUNE], 1)),
        b"Assigned" => return Some((property_ranges("gc=Cn")?, -1)),
        b"Ascii" => return Some((vec![0, 0x7F], 1)),
        _ => {}
    }
    // Names are letters and digits; regex-syntax would also strip an "is" prefix.
    if name.is_empty()
        || !name.iter().all(u8::is_ascii_alphanumeric)
        || name.len() >= 2 && name[..2].eq_ignore_ascii_case(b"is")
    {
        return None;
    }
    let name = std::str::from_utf8(&name).ok()?;
    property_ranges(&format!("gc={name}"))
        .or_else(|| property_ranges(&format!("sc={name}")))
        .map(|table| (table, 1))
}

impl Parser<'_> {
    /// Pushes `re`, turning a single-rune class into a literal and concatenating literals
    /// incrementally.
    fn push(&mut self, mut re: Regexp) {
        let r = &re.rune;
        if re.op == Op::CharClass && r.len() == 2 && r[0] == r[1] {
            // Single rune.
            if self.maybe_concat(r[0], self.flags & !FOLD_CASE) {
                return;
            }
            re.op = Op::Literal;
            re.rune.truncate(1);
            re.flags = self.flags & !FOLD_CASE;
        } else if re.op == Op::CharClass
            && (r.len() == 4 && r[0] == r[1] && r[2] == r[3] && fold_pair(r[0], r[2])
                || r.len() == 2 && r[0] + 1 == r[1] && fold_pair(r[0], r[1]))
        {
            // Case-insensitive rune like [Aa] or [Δδ].
            if self.maybe_concat(r[0], self.flags | FOLD_CASE) {
                return;
            }
            // Rewrite as (case-insensitive) literal.
            re.op = Op::Literal;
            re.rune.truncate(1);
            re.flags = self.flags | FOLD_CASE;
        } else {
            // Incremental concatenation.
            self.maybe_concat(-1, 0);
        }
        self.stack.push(re);
    }

    /// Merges the top two literals; if `r >= 0`, reuses the freed top node to push `r` with `flags`
    /// and reports true.
    fn maybe_concat(&mut self, r: i32, flags: Flags) -> bool {
        let n = self.stack.len();
        if n < 2 {
            return false;
        }
        let (re1, re2) = (&self.stack[n - 1], &self.stack[n - 2]);
        if re1.op != Op::Literal
            || re2.op != Op::Literal
            || re1.flags & FOLD_CASE != re2.flags & FOLD_CASE
        {
            return false;
        }
        // Push re1 into re2.
        let moved = std::mem::take(&mut self.stack[n - 1].rune);
        self.stack[n - 2].rune.extend(moved);
        // Reuse re1 if possible.
        if r >= 0 {
            let re1 = &mut self.stack[n - 1];
            re1.rune = vec![r];
            re1.flags = flags;
            return true;
        }
        self.stack.pop();
        false
    }

    /// Pushes a literal rune.
    fn literal(&mut self, r: i32) {
        let mut re = Regexp::new(Op::Literal);
        re.flags = self.flags;
        let r = if self.flags & FOLD_CASE != 0 {
            min_fold_rune(r)
        } else {
            r
        };
        re.rune = vec![r];
        self.push(re);
    }

    /// Pushes a node with operator `op`.
    fn op(&mut self, op: Op) {
        let mut re = Regexp::new(op);
        re.flags = self.flags;
        self.push(re);
    }

    /// Replaces the top stack element with itself repeated. `before` starts at the repetition
    /// operator and `after` follows it; returns the updated `after`.
    fn repeat<'s>(
        &mut self,
        op: Op,
        min: i32,
        max: i32,
        before: &'s [u8],
        after: &'s [u8],
        last_repeat: &[u8],
    ) -> Result<&'s [u8], Error> {
        let mut after = after;
        let mut flags = self.flags;
        if self.flags & PERL_X != 0 {
            if after.first() == Some(&b'?') {
                after = &after[1..];
                flags ^= NON_GREEDY;
            }
            if !last_repeat.is_empty() {
                // In Perl it is not allowed to stack repetition operators: a** is a syntax
                // error, not a doubled star, and a++ means something else entirely.
                let end = last_repeat.len().saturating_sub(after.len());
                return Err(error(ErrorCode::InvalidRepeatOp, &last_repeat[..end]));
            }
        }
        let expr = &before[..before.len() - after.len()];
        match self.stack.last() {
            Some(sub) if !sub.op.is_pseudo() => {}
            _ => return Err(error(ErrorCode::MissingRepeatArgument, expr)),
        }
        let Some(sub) = self.stack.pop() else {
            return Err(error(ErrorCode::MissingRepeatArgument, expr));
        };
        let mut re = Regexp::new(op);
        re.min = min;
        re.max = max;
        re.flags = flags;
        re.sub = vec![sub];
        let invalid = op == Op::Repeat && (min >= 2 || max >= 2) && !repeat_is_valid(&re, 1000);
        self.stack.push(re);
        if invalid {
            return Err(error(ErrorCode::InvalidRepeatSize, expr));
        }
        Ok(after)
    }

    /// The index of the first element above the topmost pseudo-op.
    fn above_pseudo(&self) -> usize {
        self.stack
            .iter()
            .rposition(|re| re.op.is_pseudo())
            .map_or(0, |i| i + 1)
    }

    /// Replaces the top of the stack (above the topmost `|` or `(`) with its concatenation.
    fn concat(&mut self) {
        self.maybe_concat(-1, 0);
        let i = self.above_pseudo();
        let subs = self.stack.split_off(i);
        if subs.is_empty() {
            self.push(Regexp::new(Op::EmptyMatch));
        } else {
            let re = self.collapse(subs, Op::Concat);
            self.push(re);
        }
    }

    /// Replaces the top of the stack (above the topmost `(`) with its alternation.
    fn alternate(&mut self) {
        let i = self.above_pseudo();
        let mut subs = self.stack.split_off(i);
        // Make sure top class is clean. All the others already are (see `swap_vertical_bar`).
        if let Some(last) = subs.last_mut() {
            clean_alt(last);
        }
        if subs.is_empty() {
            self.push(Regexp::new(Op::NoMatch));
        } else {
            let re = self.collapse(subs, Op::Alternate);
            self.push(re);
        }
    }

    /// Applies `op` to `subs`, hoisting nested nodes of the same op.
    fn collapse(&mut self, mut subs: Vec<Regexp>, op: Op) -> Regexp {
        if subs.len() == 1 {
            return subs.pop().unwrap_or_else(|| Regexp::new(Op::EmptyMatch));
        }
        let mut re = Regexp::new(op);
        for sub in subs {
            if sub.op == op {
                re.sub.extend(sub.sub);
            } else {
                re.sub.push(sub);
            }
        }
        if op == Op::Alternate {
            re.sub = self.factor(std::mem::take(&mut re.sub));
            if re.sub.len() == 1
                && let Some(only) = re.sub.pop()
            {
                return only;
            }
        }
        re
    }

    /// Factors common prefixes from an alternation list.
    fn factor(&mut self, sub: Vec<Regexp>) -> Vec<Regexp> {
        if sub.len() < 2 {
            return sub;
        }

        // Round 1: Factor out common literal prefixes.
        let mut out: Vec<Regexp> = Vec::new();
        let mut run: Vec<Regexp> = Vec::new();
        let mut str: Vec<i32> = Vec::new();
        let mut strflags: Flags = 0;
        for re in sub {
            let (istr, iflags) = leading_string(&re);
            if iflags == strflags {
                let same = str.iter().zip(istr).take_while(|(a, b)| a == b).count();
                if same > 0 {
                    // Matches at least one rune in current range. Keep going around.
                    str.truncate(same);
                    run.push(re);
                    continue;
                }
            }
            self.flush_literal_prefix(&mut out, std::mem::take(&mut run), &str, strflags);
            str = istr.to_vec();
            strflags = iflags;
            run.push(re);
        }
        self.flush_literal_prefix(&mut out, run, &str, strflags);
        let sub = out;

        // Round 2: Factor out common simple prefixes, just the first piece of each
        // concatenation, when it is a character class or a fixed repeat of one.
        let mut out: Vec<Regexp> = Vec::new();
        let mut run: Vec<Regexp> = Vec::new();
        let mut first: Option<Regexp> = None;
        for re in sub {
            let ifirst = leading_regexp(&re).cloned();
            if let (Some(f), Some(i)) = (&first, &ifirst)
                && f.equal(i)
                && (is_char_class(f)
                    || f.op == Op::Repeat
                        && f.min == f.max
                        && f.sub.first().is_some_and(is_char_class))
            {
                run.push(re);
                continue;
            }
            self.flush_leading_regexp(&mut out, std::mem::take(&mut run), first.take());
            first = ifirst;
            run.push(re);
        }
        self.flush_leading_regexp(&mut out, run, first);
        let sub = out;

        // Round 3: Collapse runs of single literals into character classes.
        let mut out: Vec<Regexp> = Vec::new();
        let mut run: Vec<Regexp> = Vec::new();
        for re in sub {
            if is_char_class(&re) {
                run.push(re);
                continue;
            }
            flush_char_classes(&mut out, std::mem::take(&mut run));
            out.push(re);
        }
        flush_char_classes(&mut out, run);
        let sub = out;

        // Round 4: Collapse runs of empty matches into a single empty match.
        let mut out: Vec<Regexp> = Vec::with_capacity(sub.len());
        for re in sub {
            if re.op == Op::EmptyMatch && out.last().is_some_and(|l| l.op == Op::EmptyMatch) {
                continue;
            }
            out.push(re);
        }
        out
    }

    /// Round 1 of `factor`: emits a run that shares the literal prefix `str`.
    fn flush_literal_prefix(
        &mut self,
        out: &mut Vec<Regexp>,
        mut run: Vec<Regexp>,
        str: &[i32],
        strflags: Flags,
    ) {
        if run.len() <= 1 {
            // Nothing to do, or just one: don't bother factoring.
            out.extend(run);
            return;
        }
        // Construct factored form: prefix(suffix1|suffix2|...)
        let mut prefix = Regexp::new(Op::Literal);
        prefix.flags = strflags;
        prefix.rune = str.to_vec();
        run = run
            .into_iter()
            .map(|re| remove_leading_string(re, str.len()))
            .collect();
        let suffix = self.collapse(run, Op::Alternate); // recurse
        let mut re = Regexp::new(Op::Concat);
        re.sub = vec![prefix, suffix];
        out.push(re);
    }

    /// Round 2 of `factor`: emits a run that shares the leading regexp `first`.
    fn flush_leading_regexp(
        &mut self,
        out: &mut Vec<Regexp>,
        run: Vec<Regexp>,
        first: Option<Regexp>,
    ) {
        let prefix = match first {
            Some(prefix) if run.len() > 1 => prefix,
            _ => {
                out.extend(run);
                return;
            }
        };
        let run: Vec<Regexp> = run.into_iter().map(remove_leading_regexp).collect();
        let suffix = self.collapse(run, Op::Alternate); // recurse
        let mut re = Regexp::new(Op::Concat);
        re.sub = vec![prefix, suffix];
        out.push(re);
    }

    /// Handles a `|`: finishes the concatenation before it and keeps a vertical bar on the stack.
    fn parse_vertical_bar(&mut self) {
        self.concat();
        // The concatenation we just parsed is on top of the stack. If it sits above an vertical
        // bar, swap it below. Otherwise, push a new vertical bar.
        if !self.swap_vertical_bar() {
            self.op(Op::VerticalBar);
        }
    }

    /// Swaps the concatenation on top of the stack below an existing vertical bar, merging adjacent
    /// single-char alternatives into one class. Reports whether it swapped.
    fn swap_vertical_bar(&mut self) -> bool {
        let n = self.stack.len();
        // If above and below vertical bar are literal or char class, can merge into a single
        // char class.
        if n >= 3
            && self.stack[n - 2].op == Op::VerticalBar
            && is_char_class(&self.stack[n - 1])
            && is_char_class(&self.stack[n - 3])
        {
            if let Some(mut re1) = self.stack.pop() {
                // Make re3 the more complex of the two.
                if re1.op > self.stack[n - 3].op {
                    std::mem::swap(&mut re1, &mut self.stack[n - 3]);
                }
                merge_char_class(&mut self.stack[n - 3], &re1);
            }
            return true;
        }
        if n >= 2 && self.stack[n - 2].op == Op::VerticalBar {
            if n >= 3 {
                // Now out of reach. Clean opportunistically.
                clean_alt(&mut self.stack[n - 3]);
            }
            self.stack.swap(n - 2, n - 1);
            return true;
        }
        false
    }

    /// Handles a `)`: closes the innermost group.
    fn parse_right_paren(&mut self) -> Result<(), Error> {
        self.concat();
        if self.swap_vertical_bar() {
            // pop vertical bar
            self.stack.pop();
        }
        self.alternate();
        if self.stack.len() < 2 {
            return Err(error(ErrorCode::UnexpectedParen, self.whole_regexp));
        }
        let (Some(re1), Some(mut re2)) = (self.stack.pop(), self.stack.pop()) else {
            return Err(error(ErrorCode::UnexpectedParen, self.whole_regexp));
        };
        if re2.op != Op::LeftParen {
            return Err(error(ErrorCode::UnexpectedParen, self.whole_regexp));
        }
        // Restore flags at time of paren.
        self.flags = re2.flags;
        if re2.cap == 0 {
            // Just for grouping.
            self.push(re1);
        } else {
            re2.op = Op::Capture;
            re2.sub = vec![re1];
            self.push(re2);
        }
        Ok(())
    }

    /// Parses a flag setting or non-capturing group or both, like `(?i)` or `(?:` or `(?i:`, or a
    /// named capture. `s` starts with `(?`.
    fn parse_perl_flags<'s>(&mut self, s: &'s [u8]) -> Result<&'s [u8], Error> {
        let t = s;
        let starts_with_p = t.len() > 4 && t[2] == b'P' && t[3] == b'<';
        let starts_with_name = t.len() > 3 && t[2] == b'<';
        if starts_with_p || starts_with_name {
            // position of expr start
            let expr_start = if starts_with_name { 3 } else { 4 };
            // Pull out name.
            let Some(end) = index_byte(t, b'>') else {
                check_utf8(t)?;
                return Err(error(ErrorCode::InvalidNamedCapture, s));
            };
            let capture = &t[..=end]; // "(?P<name>" or "(?<name>"
            let name = t.get(expr_start..end).unwrap_or_default(); // "name"
            check_utf8(name)?;
            if !is_valid_capture_name(name) {
                return Err(error(ErrorCode::InvalidNamedCapture, capture));
            }
            // Like ordinary capture, but named.
            self.num_cap += 1;
            let mut re = Regexp::new(Op::LeftParen);
            re.flags = self.flags;
            re.cap = self.num_cap;
            re.name = name.to_vec();
            self.push(re);
            return Ok(&t[end + 1..]);
        }

        // Non-capturing group. Might also twiddle Perl flags.
        let mut t = &t[2..]; // skip (?
        let mut flags = self.flags;
        let mut sign = 1;
        let mut saw_flag = false;
        while !t.is_empty() {
            let (c, rest) = next_rune(t)?;
            t = rest;
            match u8::try_from(c).unwrap_or(0) {
                // Flags.
                b'i' => {
                    flags |= FOLD_CASE;
                    saw_flag = true;
                }
                b'm' => {
                    flags &= !ONE_LINE;
                    saw_flag = true;
                }
                b's' => {
                    flags |= DOT_NL;
                    saw_flag = true;
                }
                b'U' => {
                    flags |= NON_GREEDY;
                    saw_flag = true;
                }
                // Switch to negation.
                b'-' => {
                    if sign < 0 {
                        break;
                    }
                    sign = -1;
                    // Invert flags so that | above turn into &^ and vice versa. We'll invert
                    // flags again before using it below.
                    flags = !flags;
                    saw_flag = false;
                }
                // End of flags, starting group or not.
                b':' | b')' => {
                    if sign < 0 {
                        if !saw_flag {
                            break;
                        }
                        flags = !flags;
                    }
                    if c == ':' as i32 {
                        // Open new group
                        self.op(Op::LeftParen);
                    }
                    self.flags = flags;
                    return Ok(t);
                }
                _ => break,
            }
        }
        Err(error(ErrorCode::InvalidPerlOp, &s[..s.len() - t.len()]))
    }

    /// The `\` case of the parse loop.
    fn parse_backslash<'s>(&mut self, t: &'s [u8]) -> Result<&'s [u8], Error> {
        if self.flags & PERL_X != 0 && t.len() >= 2 {
            match t[1] {
                b'A' => {
                    self.op(Op::BeginText);
                    return Ok(&t[2..]);
                }
                b'b' => {
                    self.op(Op::WordBoundary);
                    return Ok(&t[2..]);
                }
                b'B' => {
                    self.op(Op::NoWordBoundary);
                    return Ok(&t[2..]);
                }
                b'C' => {
                    // any byte; not supported
                    return Err(error(ErrorCode::InvalidEscape, &t[..2]));
                }
                b'Q' => {
                    // \Q ... \E: the ... is always literals
                    let rest = &t[2..];
                    let (mut lit, after) = match index_of(rest, b"\\E") {
                        Some(i) => (&rest[..i], &rest[i + 2..]),
                        None => (rest, &rest[rest.len()..]),
                    };
                    while !lit.is_empty() {
                        let (c, r) = next_rune(lit)?;
                        self.literal(c);
                        lit = r;
                    }
                    return Ok(after);
                }
                b'z' => {
                    self.op(Op::EndText);
                    return Ok(&t[2..]);
                }
                _ => {}
            }
        }

        // Look for Unicode character group like \p{Han}
        if t.len() >= 2 && (t[1] == b'p' || t[1] == b'P') {
            let mut r = Vec::new();
            if let Some(rest) = self.parse_unicode_class(t, &mut r)? {
                let mut re = Regexp::new(Op::CharClass);
                re.flags = self.flags;
                re.rune = r;
                self.push(re);
                return Ok(rest);
            }
        }

        // Perl character class escape.
        let mut r = Vec::new();
        if let Some(rest) = self.parse_perl_class_escape(t, &mut r) {
            let mut re = Regexp::new(Op::CharClass);
            re.flags = self.flags;
            re.rune = r;
            self.push(re);
            return Ok(rest);
        }

        // Ordinary single-character escape.
        let (c, rest) = parse_escape(t)?;
        self.literal(c);
        Ok(rest)
    }

    /// Appends a leading `\d`-style class to `r`.
    fn parse_perl_class_escape<'s>(&self, s: &'s [u8], r: &mut Vec<i32>) -> Option<&'s [u8]> {
        if self.flags & PERL_X == 0 || s.len() < 2 || s[0] != b'\\' {
            return None;
        }
        let g = perl_group(&s[..2])?;
        self.append_group(r, g);
        Some(&s[2..])
    }

    /// Appends a leading `[:alnum:]`-style class to `r`.
    fn parse_named_class<'s>(
        &self,
        s: &'s [u8],
        r: &mut Vec<i32>,
    ) -> Result<Option<&'s [u8]>, Error> {
        if s.len() < 2 || s[0] != b'[' || s[1] != b':' {
            return Ok(None);
        }
        let Some(i) = index_of(&s[2..], b":]") else {
            return Ok(None);
        };
        let i = i + 2;
        let (name, rest) = (&s[..i + 2], &s[i + 2..]);
        let Some(g) = posix_group(name) else {
            return Err(error(ErrorCode::InvalidCharRange, name));
        };
        self.append_group(r, g);
        Ok(Some(rest))
    }

    /// Appends a class group, folding case and negating it as its sign asks.
    fn append_group(&self, r: &mut Vec<i32>, (sign, class): (i32, &[i32])) {
        if self.flags & FOLD_CASE == 0 {
            if sign < 0 {
                append_negated_class(r, class);
            } else {
                append_class(r, class);
            }
        } else {
            let mut tmp = fold_closure(class);
            clean_class(&mut tmp);
            if sign < 0 {
                append_negated_class(r, &tmp);
            } else {
                append_class(r, &tmp);
            }
        }
    }

    /// Appends a leading `\p{Han}`-style class to `r`.
    fn parse_unicode_class<'s>(
        &self,
        s: &'s [u8],
        r: &mut Vec<i32>,
    ) -> Result<Option<&'s [u8]>, Error> {
        if self.flags & UNICODE_GROUPS == 0
            || s.len() < 2
            || s[0] != b'\\'
            || s[1] != b'p' && s[1] != b'P'
        {
            return Ok(None);
        }
        // Committed to parse or return error.
        let mut sign = if s[1] == b'P' { -1 } else { 1 };
        let (c, mut t) = next_rune(&s[2..])?;
        let (seq, mut name);
        if c != '{' as i32 {
            // Single-letter name.
            seq = &s[..s.len() - t.len()];
            name = &seq[2..];
        } else {
            // Name is in braces.
            let Some(end) = index_byte(s, b'}') else {
                check_utf8(s)?;
                return Err(error(ErrorCode::InvalidCharRange, s));
            };
            seq = &s[..=end];
            t = &s[end + 1..];
            name = s.get(3..end).unwrap_or_default();
            check_utf8(name)?;
        }
        // Group can have leading negation too. \p{^Han} == \P{Han}, \P{^Han} == \p{Han}.
        if let Some(rest) = name.strip_prefix(b"^") {
            sign = -sign;
            name = rest;
        }
        let Some((table, tsign)) = unicode_table(name) else {
            return Err(error(ErrorCode::InvalidCharRange, seq));
        };
        if tsign < 0 {
            sign = -sign;
        }
        let table = if self.flags & FOLD_CASE == 0 {
            table
        } else {
            // Merge and clean the table and its fold-equivalents.
            let mut tmp = fold_closure(&table);
            append_class(&mut tmp, &table);
            clean_class(&mut tmp);
            tmp
        };
        if sign > 0 {
            append_class(r, &table);
        } else {
            append_negated_class(r, &table);
        }
        Ok(Some(t))
    }

    /// Parses a character class at the start of `s` and pushes it.
    fn parse_class<'s>(&mut self, s: &'s [u8]) -> Result<&'s [u8], Error> {
        let mut t = &s[1..]; // chop [
        let mut sign = 1;
        let mut class: Vec<i32> = Vec::new();
        if t.first() == Some(&b'^') {
            sign = -1;
            t = &t[1..];
            // If character class does not match \n, add it here, so that negation later will
            // do the right thing.
            if self.flags & CLASS_NL == 0 {
                class.extend(['\n' as i32, '\n' as i32]);
            }
        }
        let mut first = true; // ] and - are okay as first char in class
        while t.is_empty() || t[0] != b']' || first {
            // POSIX: - is only okay unescaped as first or last in class.
            // Perl: - is okay anywhere.
            if t.first() == Some(&b'-')
                && self.flags & PERL_X == 0
                && !first
                && (t.len() == 1 || t[1] != b']')
            {
                let (_, size) = decode_rune(&t[1..]);
                return Err(error(ErrorCode::InvalidCharRange, &t[..1 + size]));
            }
            first = false;

            // Look for POSIX [:alnum:] etc.
            if t.len() > 2
                && t[0] == b'['
                && t[1] == b':'
                && let Some(nt) = self.parse_named_class(t, &mut class)?
            {
                t = nt;
                continue;
            }

            // Look for Unicode character group like \p{Han}.
            if let Some(nt) = self.parse_unicode_class(t, &mut class)? {
                t = nt;
                continue;
            }

            // Look for Perl character class symbols (extension).
            if let Some(nt) = self.parse_perl_class_escape(t, &mut class) {
                t = nt;
                continue;
            }

            // Single character or simple range.
            let rng = t;
            let (lo, rest) = parse_class_char(t, s)?;
            t = rest;
            let mut hi = lo;
            // [a-] means (a|-) so check for final ].
            if t.len() >= 2 && t[0] == b'-' && t[1] != b']' {
                t = &t[1..];
                let (h, rest) = parse_class_char(t, s)?;
                hi = h;
                t = rest;
                if hi < lo {
                    return Err(error(
                        ErrorCode::InvalidCharRange,
                        &rng[..rng.len() - t.len()],
                    ));
                }
            }
            if self.flags & FOLD_CASE == 0 {
                append_range(&mut class, lo, hi);
            } else {
                append_folded_range(&mut class, lo, hi);
            }
        }
        t = &t[1..]; // chop ]

        clean_class(&mut class);
        if sign < 0 {
            negate_class(&mut class);
        }
        let mut re = Regexp::new(Op::CharClass);
        re.flags = self.flags;
        re.rune = class;
        self.push(re);
        Ok(t)
    }
}

/// Round 3 of `factor`: emits a run of single literals and classes as one class.
fn flush_char_classes(out: &mut Vec<Regexp>, mut run: Vec<Regexp>) {
    if run.len() <= 1 {
        out.extend(run);
        return;
    }
    // Make new char class. Start with most complex regexp in run[0].
    let mut max = 0;
    for j in 1..run.len() {
        if run[max].op < run[j].op
            || run[max].op == run[j].op && run[max].rune.len() < run[j].rune.len()
        {
            max = j;
        }
    }
    run.swap(0, max);
    let mut rest = run.split_off(1);
    if let Some(mut dst) = run.pop() {
        for src in &rest {
            merge_char_class(&mut dst, src);
        }
        clean_alt(&mut dst);
        out.push(dst);
    }
    rest.clear();
}

/// The leading literal string of `re` and its fold flag.
fn leading_string(re: &Regexp) -> (&[i32], Flags) {
    let re = match re.sub.first() {
        Some(first) if re.op == Op::Concat => first,
        _ => re,
    };
    if re.op != Op::Literal {
        return (&[], 0);
    }
    (&re.rune, re.flags & FOLD_CASE)
}

/// Removes the first `n` leading runes of `re`.
fn remove_leading_string(mut re: Regexp, n: usize) -> Regexp {
    if re.op == Op::Concat && !re.sub.is_empty() {
        // Removing a leading string in a concatenation might simplify the concatenation.
        let first = remove_leading_string(re.sub.remove(0), n);
        if first.op == Op::EmptyMatch {
            match re.sub.len() {
                0 => {
                    // Impossible but handle.
                    re.op = Op::EmptyMatch;
                    re.sub.clear();
                }
                1 => {
                    if let Some(only) = re.sub.pop() {
                        return only;
                    }
                }
                _ => {}
            }
        } else {
            re.sub.insert(0, first);
        }
        return re;
    }
    if re.op == Op::Literal {
        re.rune.drain(..n.min(re.rune.len()));
        if re.rune.is_empty() {
            re.op = Op::EmptyMatch;
        }
    }
    re
}

/// The leading regexp of `re`.
fn leading_regexp(re: &Regexp) -> Option<&Regexp> {
    if re.op == Op::EmptyMatch {
        return None;
    }
    if re.op == Op::Concat
        && let Some(first) = re.sub.first()
    {
        if first.op == Op::EmptyMatch {
            return None;
        }
        return Some(first);
    }
    Some(re)
}

/// Removes the leading regexp of `re`.
fn remove_leading_regexp(mut re: Regexp) -> Regexp {
    if re.op == Op::Concat && !re.sub.is_empty() {
        re.sub.remove(0);
        match re.sub.len() {
            0 => {
                re.op = Op::EmptyMatch;
                re.sub.clear();
            }
            1 => {
                if let Some(only) = re.sub.pop() {
                    return only;
                }
            }
            _ => {}
        }
        return re;
    }
    Regexp::new(Op::EmptyMatch)
}

/// Parses one char of a class, which may be an escape.
fn parse_class_char<'s>(s: &'s [u8], whole_class: &[u8]) -> Result<(i32, &'s [u8]), Error> {
    if s.is_empty() {
        return Err(error(ErrorCode::MissingBracket, whole_class));
    }
    // Allow regular escape sequences even though many need not be escaped in this context.
    if s[0] == b'\\' {
        return parse_escape(s);
    }
    next_rune(s)
}

/// Parses the escape sequence at the start of `s` and returns its rune.
fn parse_escape(s: &[u8]) -> Result<(i32, &[u8]), Error> {
    let t = &s[1..];
    if t.is_empty() {
        return Err(error(ErrorCode::TrailingBackslash, b""));
    }
    let (c, mut t) = next_rune(t)?;
    let invalid = |t: &[u8]| error(ErrorCode::InvalidEscape, &s[..s.len() - t.len()]);
    let Ok(b) = u8::try_from(c) else {
        return Err(invalid(t));
    };
    match b {
        // Octal escapes.
        b'1'..=b'7' | b'0' => {
            // Single non-zero digit is a backreference; not supported
            if b != b'0' && !t.first().is_some_and(|d| (b'0'..=b'7').contains(d)) {
                return Err(invalid(t));
            }
            // Consume up to three octal digits; already have one.
            let mut r = c - '0' as i32;
            for _ in 1..3 {
                match t.first() {
                    Some(&d @ b'0'..=b'7') => {
                        r = r * 8 + i32::from(d - b'0');
                        t = &t[1..];
                    }
                    _ => break,
                }
            }
            Ok((r, t))
        }
        // Hexadecimal escapes.
        b'x' => {
            if t.is_empty() {
                return Err(invalid(t));
            }
            let (c, rest) = next_rune(t)?;
            t = rest;
            if c == '{' as i32 {
                // Any number of digits in braces. We require only hex digits, and at least one.
                let mut nhex = 0;
                let mut r: i32 = 0;
                loop {
                    if t.is_empty() {
                        return Err(invalid(t));
                    }
                    let (c, rest) = next_rune(t)?;
                    t = rest;
                    if c == '}' as i32 {
                        break;
                    }
                    let v = unhex(c);
                    if v < 0 {
                        return Err(invalid(t));
                    }
                    r = r * 16 + v;
                    if r > MAX_RUNE {
                        return Err(invalid(t));
                    }
                    nhex += 1;
                }
                if nhex == 0 {
                    return Err(invalid(t));
                }
                return Ok((r, t));
            }
            // Easy case: two hex digits.
            let x = unhex(c);
            let (c, rest) = next_rune(t)?;
            t = rest;
            let y = unhex(c);
            if x < 0 || y < 0 {
                return Err(invalid(t));
            }
            Ok((x * 16 + y, t))
        }
        // C escapes. There is no case 'b', to avoid misparsing the Perl word-boundary \b.
        b'a' => Ok((7, t)),
        b'f' => Ok((12, t)),
        b'n' => Ok((10, t)),
        b'r' => Ok((13, t)),
        b't' => Ok((9, t)),
        b'v' => Ok((11, t)),
        // Escaped non-word characters are always themselves.
        _ if c < 0x80 && !is_alnum(c) => Ok((c, t)),
        _ => Err(invalid(t)),
    }
}

// ---------------------------------------------------------------------------------------------
// Matching: a parse tree as a regex-syntax HIR.
// ---------------------------------------------------------------------------------------------

/// Converts a parse tree to a `regex-syntax` HIR that matches what ADK's tool search matches:
/// classes are already case folded by the parser, a case-folded literal becomes a class per rune,
/// `\b` is the ASCII word boundary, and a rune that is not a Unicode scalar value (a surrogate
/// written as `\x{D800}`) matches nothing. Feed `to_hir(&re).to_string()` to `regex::Regex::new`.
pub(crate) fn to_hir(re: &Regexp) -> Hir {
    let greedy = re.flags & NON_GREEDY == 0;
    let rep = |min: u32, max: Option<u32>| {
        let mut sub = re.sub.first().map_or_else(Hir::empty, to_hir);
        if matches!(sub.kind(), HirKind::Repetition(_)) {
            // The HIR printer writes a repetition of a repetition as `a+?`, which reads back as
            // a non-greedy `+`; a group keeps it a repetition of a repetition.
            sub = Hir::capture(Capture {
                index: 1,
                name: None,
                sub: Box::new(sub),
            });
        }
        Hir::repetition(Repetition {
            min,
            max,
            greedy,
            sub: Box::new(sub),
        })
    };
    match re.op {
        Op::NoMatch | Op::LeftParen | Op::VerticalBar => Hir::fail(),
        Op::EmptyMatch => Hir::empty(),
        Op::Literal => Hir::concat(
            re.rune
                .iter()
                .map(|&r| literal_hir(r, re.flags & FOLD_CASE != 0))
                .collect(),
        ),
        Op::CharClass => Hir::class(Class::Unicode(class_unicode(&re.rune))),
        Op::AnyCharNotNl => Hir::dot(Dot::AnyCharExceptLF),
        Op::AnyChar => Hir::dot(Dot::AnyChar),
        Op::BeginLine => Hir::look(Look::StartLF),
        Op::EndLine => Hir::look(Look::EndLF),
        Op::BeginText => Hir::look(Look::Start),
        Op::EndText => Hir::look(Look::End),
        Op::WordBoundary => Hir::look(Look::WordAscii),
        Op::NoWordBoundary => Hir::look(Look::WordAsciiNegate),
        Op::Capture => re.sub.first().map_or_else(Hir::empty, to_hir),
        Op::Star => rep(0, None),
        Op::Plus => rep(1, None),
        Op::Quest => rep(0, Some(1)),
        Op::Repeat => rep(
            u32::try_from(re.min).unwrap_or(0),
            u32::try_from(re.max).ok(),
        ),
        Op::Concat => Hir::concat(re.sub.iter().map(to_hir).collect()),
        Op::Alternate => Hir::alternation(re.sub.iter().map(to_hir).collect()),
    }
}

fn literal_hir(r: i32, fold: bool) -> Hir {
    let Some(c) = to_char(r) else {
        return Hir::fail();
    };
    if fold {
        let mut cls = ClassUnicode::new([ClassUnicodeRange::new(c, c)]);
        // Folding only fails without regex-syntax's unicode-case feature; the literal then
        // matches itself only.
        let _ = cls.try_case_fold_simple();
        Hir::class(Class::Unicode(cls))
    } else {
        let mut buf = [0u8; 4];
        Hir::literal(c.encode_utf8(&mut buf).as_bytes())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Want<'a> {
        /// Parsed; Go `patternSize` and `usesRepeat` of the tree.
        Ok(i64, bool),
        /// Go `syntax.Error` code text and `Expr`.
        Err(&'a str, &'a [u8]),
    }

    fn ok(size: i64, uses_repeat: bool) -> Want<'static> {
        Want::Ok(size, uses_repeat)
    }

    fn err(code: &'static str, expr: &'static [u8]) -> Want<'static> {
        Want::Err(code, expr)
    }

    /// Every pattern with a regex metacharacter that Go's ranksearch tests pass to Rank or
    /// compileQuery, and the "(?i)" + pattern that compileQuery compiles for each one that
    /// parses, with the outcome of Go's `syntax.Parse(pattern, syntax.Perl)` (go1.27.2).
    #[test]
    fn parse_matches_go_for_every_ranksearch_test_pattern() {
        let table: &[(&[u8], Want<'static>)] = &[
            (b"(unclosed", err("missing closing )", b"(unclosed")),
            (b"get_.*_book", ok(12, true)),
            (b"^t$", ok(4, false)),
            (b"how do I list books?", ok(21, false)),
            (b"list the books.", ok(16, false)),
            (b"books (all of them)", ok(19, false)),
            (b"list 2*3 books", ok(15, true)),
            (b"^get_.*_record$", ok(16, true)),
            (b"which tool?", ok(12, false)),
            (b"book.*", ok(7, true)),
            (b"getuser.*", ok(10, true)),
            (b"(V2", err("missing closing )", b"(V2")),
            (b".{0,100}x", ok(202, false)),
            (b".{0,999}x", ok(1999, false)),
            (b"fixes things {0,999}", ok(2011, false)),
            (b"(?:fix_tool){0,20}", ok(180, false)),
            (b"(?:fix_tool){0,30}", ok(270, false)),
            (b"(?:x?){150,}", ok(450, true)),
            (b"node.js", ok(8, false)),
            (b"C++", err("invalid nested repetition operator", b"++")),
            (b"invoices.", ok(10, false)),
            (b"v1.2", ok(5, false)),
            (b"(scripts)", ok(8, false)),
            (b"node(js)", ok(8, false)),
            (b"invoice(s)", ok(10, false)),
            (b"convert()", ok(10, false)),
            (b"PDF?", ok(5, false)),
            (b"*node", err("missing argument to repetition operator", b"*")),
            (b"(node", err("missing closing )", b"(node")),
            (b"node)", err("unexpected )", b"node)")),
            (b"node\\", err("trailing backslash at end of expression", b"")),
            (b"Objective-C++", err("invalid nested repetition operator", b"++")),
            (b"C++17", err("invalid nested repetition operator", b"++")),
            (b"(C++)", err("invalid nested repetition operator", b"++")),
            (b"(+js)", err("missing argument to repetition operator", b"+")),
            (b"node)(js", err("unexpected )", b"node)(js")),
            (b"node+\xdc", err("invalid UTF-8", b"\xdc")),
            (b"convert())", err("unexpected )", b"convert())")),
            (b"node]", ok(5, false)),
            (b"{node}", ok(6, false)),
            (b"sources?", ok(9, false)),
            (b"node+", ok(6, false)),
            (b"c+", ok(2, false)),
            (b"iOS?", ok(5, false)),
            (b"nodeScripts?", ok(13, false)),
            (b"node-scripts?", ok(14, false)),
            (b"node.+js", ok(9, true)),
            (b"node.+", ok(7, true)),
            (b"node.{2}js", ok(11, false)),
            (b"node.{,2}js", ok(12, false)),
            (b"node\\x2ejs", ok(7, false)),
            (b"node[a-z]js", ok(8, false)),
            (b"node[.]js", ok(7, false)),
            (b"node\\d", ok(6, false)),
            (b"node\\Djs", ok(8, false)),
            (b"node\\sjs", ok(8, false)),
            (b"node\\Sjs", ok(8, false)),
            (b"node\\Wjs", ok(8, false)),
            (b"node\\pLjs", ok(8, false)),
            (b"node\\PLjs", ok(8, false)),
            (b"node|deno", ok(9, false)),
            (b"node|node", ok(6, false)),
            (b"^node", ok(6, false)),
            (b"\\Anode", ok(6, false)),
            (b"js$", ok(4, false)),
            (b"js\\z", ok(4, false)),
            (b"no$de", ok(6, false)),
            (b"js\\b", ok(4, false)),
            (b"js\\B", ok(4, false)),
            (b"(?m)^node", ok(6, false)),
            (b"(?:run)?js", ok(7, true)),
            (b"node(?:s)?", ok(7, false)),
            (b"(?i)node", ok(4, false)),
            (b"(?:node)", ok(4, false)),
            (b"node\\.js", ok(7, false)),
            (b"\\Qnode\\E", ok(4, false)),
            (b"node_scripts?", ok(14, false)),
            (b"node_script+", ok(13, false)),
            (b"(node)_js", ok(9, false)),
            (b"node.*js", ok(9, true)),
            (b"node.?js", ok(9, true)),
            (b"nodes*js", ok(9, true)),
            (b"nodes*", ok(7, true)),
            (b"nodes+js", ok(9, true)),
            (b"node?js", ok(8, true)),
            (b"(run)?js", ok(8, true)),
            (b"node.(js)?", ok(10, true)),
            (b"node.(js?)", ok(11, true)),
            (b"nodes*j?", ok(9, true)),
            (b"^(?!delete).*_record$", err("invalid or unsupported Perl syntax", b"(?!")),
            (b"_record\\Z", err("invalid escape sequence", b"\\Z")),
            (b"^get_[a-z]{1,2000}$", err("invalid repeat count", b"{1,2000}")),
            (b"[z-a]_record", err("invalid character class range", b"z-a")),
            (b"(?<!delete)_record$", err("invalid named capture", b"(?<!delete)_record$")),
            (b"^get_[a-z_+_record$", err("missing closing ]", b"[a-z_+_record$")),
            (b"^get\\", err("trailing backslash at end of expression", b"")),
            (b"$\xdc", err("invalid UTF-8", b"\xdc")),
            (b"^get_.*+_record$", err("invalid nested repetition operator", b"*+")),
            (b"get_users*+_record", err("invalid nested repetition operator", b"*+")),
            (b"^(get|list_.*_record$", err("missing closing )", b"^(get|list_.*_record$")),
            (b"^(get|delete)_.*)$", err("unexpected )", b"^(get|delete)_.*)$")),
            (b"^getuser)", err("unexpected )", b"^getuser)")),
            (b"getuser)$", err("unexpected )", b"getuser)$")),
            (b"(*FAIL)", err("missing argument to repetition operator", b"*")),
            (b"getx.+*record", err("invalid nested repetition operator", b"+*")),
            (b"getusers*+record", err("invalid nested repetition operator", b"*+")),
            (b"getuser?+record", err("invalid nested repetition operator", b"?+")),
            (b"getx\\w++record", err("invalid nested repetition operator", b"++")),
            (b"getx{2}+", err("invalid nested repetition operator", b"{2}+")),
            (b"get_users**_record", err("invalid nested repetition operator", b"**")),
            (b"getusers**record", err("invalid nested repetition operator", b"**")),
            (b"(get_users*_record", err("missing closing )", b"(get_users*_record")),
            (b"get_users*_record)", err("unexpected )", b"get_users*_record)")),
            (b"*getusers*record", err("missing argument to repetition operator", b"*")),
            (b"getusers*record\\", err("trailing backslash at end of expression", b"")),
            (b"getusers*record\xdc", err("invalid UTF-8", b"\xdc")),
            (b"getx.**record", err("invalid nested repetition operator", b"**")),
            (b"getx.?*record", err("invalid nested repetition operator", b"?*")),
            (b"node.(js?", err("missing closing )", b"node.(js?")),
            (b"node.js)?", err("unexpected )", b"node.js)?")),
            (b"getx+)+record", err("unexpected )", b"getx+)+record")),
            (b"getx.\xff++record", err("invalid UTF-8", b"\xff++record")),
            (b"(\xff*FAIL)", err("invalid UTF-8", b"\xff*FAIL)")),
            (b"getx.++record", err("invalid nested repetition operator", b"++")),
            (b"*+getusers", err("missing argument to repetition operator", b"*")),
            (b"?+getusers", err("missing argument to repetition operator", b"?")),
            (b"(getusers(+record)", err("missing argument to repetition operator", b"+")),
            (b"getusers)(+record", err("unexpected )", b"getusers)(+record")),
            (b"*node*", err("missing argument to repetition operator", b"*")),
            (b"tel(+48", err("missing argument to repetition operator", b"+")),
            (b"(beta?", err("missing closing )", b"(beta?")),
            (b"why???", err("invalid nested repetition operator", b"???")),
            (b"delete (?!x) record", err("invalid or unsupported Perl syntax", b"(?!")),
            (b"(((((((((node)))))", err("missing closing )", b"(((((((((node)))))")),
            (b"((((((((((node)))))", err("missing closing )", b"((((((((((node)))))")),
            (b"^get_forecast$", ok(15, false)),
            (b"forecast?", ok(10, false)),
            (b"o1.2", ok(5, false)),
            (b"fore.ast", ok(9, false)),
            (b"(forest", err("missing closing )", b"(forest")),
            (b"how is fore.ast?", ok(17, false)),
            (b".{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}QQQQQQQ", ok(1999, false)),
            (b"[B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf]", ok(11, false)),
            (b"(\xdc", err("invalid UTF-8", b"\xdc")),
            (b"(?i)get_.*_book", ok(12, true)),
            (b"(?i)^t$", ok(4, false)),
            (b"(?i)how do I list books?", ok(21, false)),
            (b"(?i)list the books.", ok(16, false)),
            (b"(?i)books (all of them)", ok(19, false)),
            (b"(?i)list 2*3 books", ok(15, true)),
            (b"(?i)^get_.*_record$", ok(16, true)),
            (b"(?i)which tool?", ok(12, false)),
            (b"(?i)book.*", ok(7, true)),
            (b"(?i)getuser.*", ok(10, true)),
            (b"(?i).{0,100}x", ok(202, false)),
            (b"(?i).{0,999}x", ok(1999, false)),
            (b"(?i)fixes things {0,999}", ok(2011, false)),
            (b"(?i)(?:fix_tool){0,20}", ok(180, false)),
            (b"(?i)(?:fix_tool){0,30}", ok(270, false)),
            (b"(?i)(?:x?){150,}", ok(450, true)),
            (b"(?i)node.js", ok(8, false)),
            (b"(?i)invoices.", ok(10, false)),
            (b"(?i)v1.2", ok(5, false)),
            (b"(?i)(scripts)", ok(8, false)),
            (b"(?i)node(js)", ok(8, false)),
            (b"(?i)invoice(s)", ok(10, false)),
            (b"(?i)convert()", ok(10, false)),
            (b"(?i)PDF?", ok(5, false)),
            (b"(?i)node]", ok(5, false)),
            (b"(?i){node}", ok(6, false)),
            (b"(?i)sources?", ok(9, false)),
            (b"(?i)node+", ok(6, false)),
            (b"(?i)c+", ok(2, false)),
            (b"(?i)iOS?", ok(5, false)),
            (b"(?i)nodeScripts?", ok(13, false)),
            (b"(?i)node-scripts?", ok(14, false)),
            (b"(?i)node.+js", ok(9, true)),
            (b"(?i)node.+", ok(7, true)),
            (b"(?i)node.{2}js", ok(11, false)),
            (b"(?i)node.{,2}js", ok(12, false)),
            (b"(?i)node\\x2ejs", ok(7, false)),
            (b"(?i)node[a-z]js", ok(8, false)),
            (b"(?i)node[.]js", ok(8, false)),
            (b"(?i)node\\d", ok(6, false)),
            (b"(?i)node\\Djs", ok(8, false)),
            (b"(?i)node\\sjs", ok(8, false)),
            (b"(?i)node\\Sjs", ok(8, false)),
            (b"(?i)node\\Wjs", ok(8, false)),
            (b"(?i)node\\pLjs", ok(8, false)),
            (b"(?i)node\\PLjs", ok(8, false)),
            (b"(?i)node|deno", ok(9, false)),
            (b"(?i)node|node", ok(6, false)),
            (b"(?i)^node", ok(6, false)),
            (b"(?i)\\Anode", ok(6, false)),
            (b"(?i)js$", ok(4, false)),
            (b"(?i)js\\z", ok(4, false)),
            (b"(?i)no$de", ok(6, false)),
            (b"(?i)js\\b", ok(4, false)),
            (b"(?i)js\\B", ok(4, false)),
            (b"(?i)(?m)^node", ok(6, false)),
            (b"(?i)(?:run)?js", ok(7, true)),
            (b"(?i)node(?:s)?", ok(7, false)),
            (b"(?i)(?i)node", ok(4, false)),
            (b"(?i)(?:node)", ok(4, false)),
            (b"(?i)node\\.js", ok(7, false)),
            (b"(?i)\\Qnode\\E", ok(4, false)),
            (b"(?i)node_scripts?", ok(14, false)),
            (b"(?i)node_script+", ok(13, false)),
            (b"(?i)(node)_js", ok(9, false)),
            (b"(?i)node.*js", ok(9, true)),
            (b"(?i)node.?js", ok(9, true)),
            (b"(?i)nodes*js", ok(9, true)),
            (b"(?i)nodes*", ok(7, true)),
            (b"(?i)nodes+js", ok(9, true)),
            (b"(?i)node?js", ok(8, true)),
            (b"(?i)(run)?js", ok(8, true)),
            (b"(?i)node.(js)?", ok(10, true)),
            (b"(?i)node.(js?)", ok(11, true)),
            (b"(?i)nodes*j?", ok(9, true)),
            (b"(?i)^get_forecast$", ok(15, false)),
            (b"(?i)forecast?", ok(10, false)),
            (b"(?i)o1.2", ok(5, false)),
            (b"(?i)fore.ast", ok(9, false)),
            (b"(?i)how is fore.ast?", ok(17, false)),
            (b"(?i).{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}.{0,999}QQQQQQQ", ok(1999, false)),
            (b"(?i)[B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf][B-\xf4\x8f\xbf\xbf]", ok(11, false)),
        ];
        for (pattern, want) in table {
            let parsed = parse(pattern, PERL);
            let got = match &parsed {
                Ok(re) => Want::Ok(
                    super::super::pattern_size(re),
                    super::super::uses_repeat(re),
                ),
                Err(e) => Want::Err(e.code.as_str(), &e.expr),
            };
            assert_eq!(&got, want, "parse({:?})", String::from_utf8_lossy(pattern));
        }
    }

    fn matcher(pattern: &str) -> regex::Regex {
        let re = parse(pattern.as_bytes(), PERL).unwrap();
        regex::Regex::new(&to_hir(&re).to_string()).unwrap()
    }

    #[test]
    fn to_hir_matches_with_go_semantics() {
        for (pattern, text, want) in [
            // Go-only syntax means what it means in Go.
            (r"\Qa.b\E", "a.b", true),
            (r"\Qa.b\E", "axb", false),
            (r"x.{,2}y", "x.{,2}y", true),
            (r"x.{,2}y", "xy", false),
            (r"a[&&b]", "a&", true),
            (r"\<", "<", true),
            // Perl classes and \b are ASCII; $ is the end of the text.
            (r"\d", "\u{663}", false),
            (r"\w", "é", false),
            (r"x\b", "xé", true),
            (r"x$", "x\n", false),
            (r"(?m)x$", "x\ny", true),
            (r"[^a]", "\n", true),
            (r".", "\n", false),
            (r"(?s).", "\n", true),
            // Case folding follows Go's simple folding, applied by the parser.
            (r"(?i)k", "\u{212a}", true),
            (r"(?i)[^k]", "K", false),
            (r"(?i)\W", "\u{17f}", false),
            (r"(?i:a)B", "Ab", false),
            // A repetition of a repetition stays one.
            (r"(a+)?", "", true),
            (r"(?:a{2}){2}", "aaaa", true),
            // A surrogate rune matches nothing.
            (r"\x{D800}|z", "z", true),
        ] {
            assert_eq!(
                matcher(pattern).is_match(text),
                want,
                "{pattern:?} on {text:?}"
            );
        }
    }

    #[test]
    fn go_string_helpers_follow_go() {
        assert_eq!(decode_rune(b"\xe2\x82\xac!"), (0x20ac, 3));
        assert_eq!(decode_rune(b"\xe2\x82"), (RUNE_ERROR, 1));
        assert_eq!(decode_rune(b"\xed\xa0\x80"), (RUNE_ERROR, 1));
        assert_eq!(decode_rune(b""), (RUNE_ERROR, 0));
        assert_eq!(decode_last_rune(b"a\xe2\x82\xac"), (0x20ac, 3));
        assert_eq!(decode_last_rune(b"a\x82\xac"), (RUNE_ERROR, 1));
        assert_eq!(trim_space(b" \t\xc2\xa0x y\xe3\x80\x80\n"), b"x y");
        assert_eq!(trim_space(b"\xff "), b"\xff");
        assert!(contains_space(b"a\xc2\x85b") && !contains_space(b"a\xffb"));
        assert_eq!(rune_count(b"a\xff\xfe\xe2\x82\xac"), 4);
        assert_eq!(
            to_valid_utf8(b"a\xff\xfeb\xffc", "\u{fffd}"),
            "a\u{fffd}b\u{fffd}c"
        );
        assert_eq!(to_valid_utf8(b"a\xff\xfeb", ""), "ab");
    }
}
