// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! Port of yaml.v3 v3.0.1 `resolve.go`: implicit tag resolution, short/long tags, `!!binary`
//! base64 and timestamps. Also home to `encode.go`'s `isBase60Float` / `isOldBool` (the
//! YAML 1.1 forms `stringv` quotes) and the slices of Go's `strconv` /
//! `time.Parse` that `resolve` relies on (`ParseInt`, `ParseUint`, `ParseFloat`, the four
//! `allowedTimestampFormats` layouts).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};

use crate::error::{Error, failf};
use crate::value::GoValue;

/// Go: resolve.go `resolveTable` (built in `init`): the class of a scalar's first byte —
/// `S` sign, `D` digit, `M` in `resolveMap`, `.` float (maybe in the map), 0 otherwise.
const RESOLVE_TABLE: [u8; 256] = resolve_table();

const fn resolve_table() -> [u8; 256] {
    // Go: resolve.go init
    let mut t = [0u8; 256];
    t[b'+' as usize] = b'S'; // Sign
    t[b'-' as usize] = b'S';
    let digits = b"0123456789";
    let mut i = 0;
    while i < digits.len() {
        t[digits[i] as usize] = b'D'; // Digit
        i += 1;
    }
    let in_map = b"yYnNtTfFoO~";
    let mut i = 0;
    while i < in_map.len() {
        t[in_map[i] as usize] = b'M'; // In map
        i += 1;
    }
    t[b'.' as usize] = b'.'; // Float (potentially in map)
    t
}

/// Go: resolve.go `resolveMap` (built in `init` from `resolveMapList`): the value and tag of
/// the scalars resolved by lookup.
fn resolve_map(s: &str) -> Option<(GoValue, &'static str)> {
    // Go: resolve.go init (resolveMapList)
    Some(match s {
        "true" | "True" | "TRUE" => (GoValue::Bool(true), BOOL_TAG),
        "false" | "False" | "FALSE" => (GoValue::Bool(false), BOOL_TAG),
        "" | "~" | "null" | "Null" | "NULL" => (GoValue::Null, NULL_TAG),
        ".nan" | ".NaN" | ".NAN" => (GoValue::Float(f64::NAN), FLOAT_TAG),
        ".inf" | ".Inf" | ".INF" => (GoValue::Float(f64::INFINITY), FLOAT_TAG),
        "+.inf" | "+.Inf" | "+.INF" => (GoValue::Float(f64::INFINITY), FLOAT_TAG),
        "-.inf" | "-.Inf" | "-.INF" => (GoValue::Float(f64::NEG_INFINITY), FLOAT_TAG),
        "<<" => (GoValue::String("<<".to_string()), MERGE_TAG),
        _ => return None,
    })
}

/// Go: `nullTag`.
pub(crate) const NULL_TAG: &str = "!!null";
/// Go: `boolTag`.
pub(crate) const BOOL_TAG: &str = "!!bool";
/// Go: `strTag`.
pub(crate) const STR_TAG: &str = "!!str";
/// Go: `intTag`.
pub(crate) const INT_TAG: &str = "!!int";
/// Go: `floatTag`.
pub(crate) const FLOAT_TAG: &str = "!!float";
/// Go: `timestampTag`.
pub(crate) const TIMESTAMP_TAG: &str = "!!timestamp";
/// Go: `seqTag`.
pub(crate) const SEQ_TAG: &str = "!!seq";
/// Go: `mapTag`.
pub(crate) const MAP_TAG: &str = "!!map";
/// Go: `binaryTag`.
pub(crate) const BINARY_TAG: &str = "!!binary";
/// Go: `mergeTag`.
pub(crate) const MERGE_TAG: &str = "!!merge";

/// The short tags `longTags` / `shortTags` are built from (resolve.go `init`).
const KNOWN_TAGS: [&str; 10] = [
    NULL_TAG,
    BOOL_TAG,
    STR_TAG,
    INT_TAG,
    FLOAT_TAG,
    TIMESTAMP_TAG,
    SEQ_TAG,
    MAP_TAG,
    BINARY_TAG,
    MERGE_TAG,
];

/// Go: `longTagPrefix`.
pub(crate) const LONG_TAG_PREFIX: &str = "tag:yaml.org,2002:";

/// Go: resolve.go `shortTag` — `tag:yaml.org,2002:x` → `!!x`; other tags unchanged.
pub(crate) fn short_tag(tag: &str) -> String {
    // Go: resolve.go shortTag
    if let Some(rest) = tag.strip_prefix(LONG_TAG_PREFIX) {
        // Go's shortTags map: the known tags' long forms.
        if let Some(stag) = KNOWN_TAGS.iter().find(|stag| stag[2..] == *rest) {
            return (*stag).to_string();
        }
        return format!("!!{rest}");
    }
    tag.to_string()
}

/// Go: resolve.go `longTag` — `!!x` → `tag:yaml.org,2002:x`; other tags unchanged.
pub(crate) fn long_tag(tag: &str) -> String {
    // Go: resolve.go longTag
    if let Some(rest) = tag.strip_prefix("!!") {
        // Go's longTags map: the known tags' long forms.
        if let Some(stag) = KNOWN_TAGS.iter().find(|stag| **stag == tag) {
            return format!("{LONG_TAG_PREFIX}{}", &stag[2..]);
        }
        return format!("{LONG_TAG_PREFIX}{rest}");
    }
    tag.to_string()
}

/// Go: resolve.go `resolvableTag`.
pub(crate) fn resolvable_tag(tag: &str) -> bool {
    // Go: resolve.go resolvableTag
    matches!(
        tag,
        "" | STR_TAG | BOOL_TAG | INT_TAG | FLOAT_TAG | NULL_TAG | TIMESTAMP_TAG
    )
}

/// Go: resolve.go `yamlStyleFloat`,
/// `^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$`.
fn yaml_style_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    } else {
        if digits(&mut i) == 0 {
            return false;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            digits(&mut i);
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// Go: resolve.go `resolve` — the resolved short tag and Go value of the scalar text `input`
/// under `tag` (Go's `interface{}` result as a [`GoValue`]: `nil` → `Null`, `int`/`int64` →
/// `Int`, `uint64` → `Uint`, `float64` → `Float`, `string` → `String`, `time.Time` →
/// `Timestamp`). The `Err` is the deferred `failf("cannot decode %s `%s` as a %s", …)`.
pub(crate) fn resolve(tag: &str, input: &str) -> Result<(String, GoValue), Error> {
    // Go: resolve.go resolve
    let tag = short_tag(tag);
    if !resolvable_tag(&tag) {
        return Ok((tag, GoValue::String(input.to_string())));
    }

    let (rtag, out) = resolve_value(&tag, input);

    // Go's deferred check of the resolved tag against the requested one.
    if tag.is_empty() || tag == rtag || tag == STR_TAG || tag == BINARY_TAG {
        return Ok((rtag.to_string(), out));
    }
    if tag == FLOAT_TAG && rtag == INT_TAG {
        // Go's `case int64:` / `case int:` (both `GoValue::Int`); a `uint64` falls through.
        if let GoValue::Int(v) = out {
            return Ok((FLOAT_TAG.to_string(), GoValue::Float(v as f64)));
        }
    }
    Err(failf(format_args!(
        "cannot decode {} `{}` as a {}",
        short_tag(rtag),
        input,
        short_tag(&tag)
    )))
}

/// The body of Go's `resolve` before its deferred check: `tag` is short and resolvable.
fn resolve_value(tag: &str, input: &str) -> (&'static str, GoValue) {
    // Go: resolve.go resolve (body)

    // Any data is accepted as a !!str or !!binary.
    // Otherwise, the prefix is enough of a hint about what it might be.
    let hint = match input.as_bytes().first() {
        None => b'N',
        Some(&c) => RESOLVE_TABLE[usize::from(c)],
    };
    if hint != 0 && tag != STR_TAG && tag != BINARY_TAG {
        // Handle things we can lookup in a map.
        if let Some((value, rtag)) = resolve_map(input) {
            return (rtag, value);
        }

        // Base 60 floats are a bad idea, were dropped in YAML 1.2, and
        // are purposefully unsupported here. They're still quoted on
        // the way out for compatibility with other parser, though.

        match hint {
            b'M' => {
                // We've already checked the map above.
            }
            b'.' => {
                // Not in the map, so maybe a normal float.
                if let Some(floatv) = parse_float(input) {
                    return (FLOAT_TAG, GoValue::Float(floatv));
                }
            }
            b'D' | b'S' => {
                // Int, float, or timestamp.
                // Only try values as a timestamp if the value is unquoted or there's an explicit
                // !!timestamp tag.
                if (tag.is_empty() || tag == TIMESTAMP_TAG)
                    && let Some(t) = parse_timestamp(input)
                {
                    return (TIMESTAMP_TAG, GoValue::Timestamp(t));
                }

                let plain = input.replace('_', "");
                // Go returns `int(intv)` when it fits an `int`, else `intv`; both are
                // `GoValue::Int` (Go's `int` is 64-bit).
                if let Some(intv) = parse_int(&plain, 0) {
                    return (INT_TAG, GoValue::Int(intv));
                }
                if let Some(uintv) = parse_uint(&plain, 0) {
                    return (INT_TAG, GoValue::Uint(uintv));
                }
                if yaml_style_float(&plain)
                    && let Some(floatv) = parse_float(&plain)
                {
                    return (FLOAT_TAG, GoValue::Float(floatv));
                }
                if let Some(rest) = plain.strip_prefix("0b") {
                    if let Some(intv) = parse_int(rest, 2) {
                        return (INT_TAG, GoValue::Int(intv));
                    }
                    if let Some(uintv) = parse_uint(rest, 2) {
                        return (INT_TAG, GoValue::Uint(uintv));
                    }
                } else if let Some(rest) = plain.strip_prefix("-0b")
                    && let Some(intv) = parse_int(&format!("-{rest}"), 2)
                {
                    return (INT_TAG, GoValue::Int(intv));
                }
                // Octals as introduced in version 1.2 of the spec.
                // Octals from the 1.1 spec, spelled as 0777, are still
                // decoded by default in v3 as well for compatibility.
                // May be dropped in v4 depending on how usage evolves.
                if let Some(rest) = plain.strip_prefix("0o") {
                    if let Some(intv) = parse_int(rest, 8) {
                        return (INT_TAG, GoValue::Int(intv));
                    }
                    if let Some(uintv) = parse_uint(rest, 8) {
                        return (INT_TAG, GoValue::Uint(uintv));
                    }
                } else if let Some(rest) = plain.strip_prefix("-0o")
                    && let Some(intv) = parse_int(&format!("-{rest}"), 8)
                {
                    return (INT_TAG, GoValue::Int(intv));
                }
            }
            _ => panic!(
                "internal error: missing handler for resolver table: {} (with {})",
                char::from(hint),
                input
            ),
        }
    }
    (STR_TAG, GoValue::String(input.to_string()))
}

/// Go: resolve.go `encodeBase64` — base64 broken into 70-character lines (each followed by a
/// newline when there is more than one line). `s` is a Go string: any bytes.
pub(crate) fn encode_base64(s: &[u8]) -> String {
    // Go: resolve.go encodeBase64
    const LINE_LEN: usize = 70;
    let encoded = STANDARD.encode(s);
    let enc_len = encoded.len();
    let lines = enc_len / LINE_LEN + 1;
    let mut out = String::with_capacity(enc_len + lines);
    let mut i = 0;
    while i < enc_len {
        let j = (i + LINE_LEN).min(enc_len);
        out.push_str(&encoded[i..j]);
        if lines > 1 {
            out.push('\n');
        }
        i += LINE_LEN;
    }
    out
}

/// Go: resolve.go `allowedTimestampFormats` — a subset of the formats allowed by the regular
/// expression defined at <http://yaml.org/type/timestamp.html>, each as the chunks Go's
/// `time.nextStdChunk` splits it into.
const ALLOWED_TIMESTAMP_FORMATS: [&[Chunk]; 4] = [
    // "2006-1-2T15:4:5.999999999Z07:00": RCF3339Nano with short date fields.
    &[
        Chunk::LongYear,
        Chunk::Literal("-"),
        Chunk::NumMonth,
        Chunk::Literal("-"),
        Chunk::Day,
        Chunk::Literal("T"),
        Chunk::Hour,
        Chunk::Literal(":"),
        Chunk::Minute,
        Chunk::Literal(":"),
        Chunk::Second,
        Chunk::FracSecond9,
        Chunk::Iso8601ColonTz,
    ],
    // "2006-1-2t15:4:5.999999999Z07:00": RFC3339Nano with short date fields and lower-case "t".
    &[
        Chunk::LongYear,
        Chunk::Literal("-"),
        Chunk::NumMonth,
        Chunk::Literal("-"),
        Chunk::Day,
        Chunk::Literal("t"),
        Chunk::Hour,
        Chunk::Literal(":"),
        Chunk::Minute,
        Chunk::Literal(":"),
        Chunk::Second,
        Chunk::FracSecond9,
        Chunk::Iso8601ColonTz,
    ],
    // "2006-1-2 15:4:5.999999999": space separated with no time zone.
    &[
        Chunk::LongYear,
        Chunk::Literal("-"),
        Chunk::NumMonth,
        Chunk::Literal("-"),
        Chunk::Day,
        Chunk::Literal(" "),
        Chunk::Hour,
        Chunk::Literal(":"),
        Chunk::Minute,
        Chunk::Literal(":"),
        Chunk::Second,
        Chunk::FracSecond9,
    ],
    // "2006-1-2": date only.
    &[
        Chunk::LongYear,
        Chunk::Literal("-"),
        Chunk::NumMonth,
        Chunk::Literal("-"),
        Chunk::Day,
    ],
    // Notable exception: time.Parse cannot handle: "2001-12-14 21:59:43.10 -5"
    // from the set of examples.
];

/// Go: resolve.go `parseTimestamp` — `Some` for the `allowedTimestampFormats`
/// (no zone = UTC).
pub(crate) fn parse_timestamp(s: &str) -> Option<DateTime<FixedOffset>> {
    // Go: resolve.go parseTimestamp
    // TODO write code to check all the formats supported by
    // http://yaml.org/type/timestamp.html instead of using time.Parse.

    // Quick check: all date formats start with YYYY-.
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i != 4 || i == b.len() || b[i] != b'-' {
        return None;
    }
    ALLOWED_TIMESTAMP_FORMATS
        .iter()
        .find_map(|format| time_parse(format, s))
}

/// Go: encode.go `isBase60Float` — YAML 1.1 base-60 notation, which `stringv` quotes on
/// output (`^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+(?:\.[0-9_]*)?$`).
pub(crate) fn is_base60_float(s: &str) -> bool {
    // Go: encode.go isBase60Float
    // Fast path.
    let b = s.as_bytes();
    let Some(&c) = b.first() else {
        return false;
    };
    if !(c == b'+' || c == b'-' || c.is_ascii_digit()) || !b.contains(&b':') {
        return false;
    }
    // Do the full match.
    let mut i = 0;
    if b[i] == b'+' || b[i] == b'-' {
        i += 1;
    }
    if i == b.len() || !b[i].is_ascii_digit() {
        return false;
    }
    i += 1;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    let mut groups = 0;
    while i < b.len() && b[i] == b':' {
        i += 1;
        // `[0-5]?[0-9]`: two digits when the first is 0-5 and a digit follows, else one.
        if i + 1 < b.len() && (b'0'..=b'5').contains(&b[i]) && b[i + 1].is_ascii_digit() {
            i += 2;
        } else if i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        } else {
            return false;
        }
        groups += 1;
    }
    if groups == 0 {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    i == b.len()
}

/// Go: encode.go `isOldBool` — YAML 1.1 bool notation, which `stringv` quotes on output so
/// the output stays valid for YAML 1.1 parsers.
pub(crate) fn is_old_bool(s: &str) -> bool {
    // Go: encode.go isOldBool
    matches!(
        s,
        "y" | "Y"
            | "yes"
            | "Yes"
            | "YES"
            | "on"
            | "On"
            | "ON"
            | "n"
            | "N"
            | "no"
            | "No"
            | "NO"
            | "off"
            | "Off"
            | "OFF"
    )
}

// ---------------------------------------------------------------------------------------------
// Go's strconv: the ParseInt / ParseUint / ParseFloat behaviour `resolve` depends on (only
// success matters there, so failures are `None`).
// ---------------------------------------------------------------------------------------------

/// Go: strconv `lower` — ASCII lower case of a letter (other bytes garbled harmlessly).
fn lower(c: u8) -> u8 {
    c | (b'x' - b'X')
}

/// Go: `strconv.ParseUint(s, base, 64)` (`base` 0 or 2..=36); `None` on any error.
fn parse_uint(s: &str, base: u32) -> Option<u64> {
    // Go: strconv/atoi.go ParseUint
    let s0 = s.as_bytes();
    if s0.is_empty() {
        return None;
    }
    let base0 = base == 0;
    let mut b = s0;
    let mut base = u64::from(base);
    if base0 {
        // Look for octal, hex prefix.
        base = 10;
        if b[0] == b'0' {
            if b.len() >= 3 && lower(b[1]) == b'b' {
                base = 2;
                b = &b[2..];
            } else if b.len() >= 3 && lower(b[1]) == b'o' {
                base = 8;
                b = &b[2..];
            } else if b.len() >= 3 && lower(b[1]) == b'x' {
                base = 16;
                b = &b[2..];
            } else {
                base = 8;
                b = &b[1..];
            }
        }
    } else if !(2..=36).contains(&base) {
        return None;
    }
    // Cutoff is the smallest number such that cutoff*base > maxUint64.
    let cutoff = u64::MAX / base + 1;
    let mut underscores = false;
    let mut n: u64 = 0;
    for &c in b {
        let d = if c == b'_' && base0 {
            underscores = true;
            continue;
        } else if c.is_ascii_digit() {
            c - b'0'
        } else if lower(c).is_ascii_lowercase() {
            lower(c) - b'a' + 10
        } else {
            return None;
        };
        if u64::from(d) >= base {
            return None;
        }
        if n >= cutoff {
            // n*base overflows
            return None;
        }
        n *= base;
        n = n.checked_add(u64::from(d))?;
    }
    if underscores && !underscore_ok(s0) {
        return None;
    }
    Some(n)
}

/// Go: `strconv.ParseInt(s, base, 64)`; `None` on any error.
fn parse_int(s: &str, base: u32) -> Option<i64> {
    // Go: strconv/atoi.go ParseInt
    if s.is_empty() {
        return None;
    }
    // Pick off leading sign.
    let (neg, s) = match s.as_bytes()[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    // Convert unsigned and check range.
    let un = parse_uint(s, base)?;
    const CUTOFF: u64 = 1 << 63;
    if !neg && un >= CUTOFF {
        return None;
    }
    if neg && un > CUTOFF {
        return None;
    }
    if neg {
        Some(0i64.wrapping_sub_unsigned(un))
    } else {
        i64::try_from(un).ok()
    }
}

/// Go: strconv `underscoreOK` — underscores only between digits or between a base prefix
/// and a digit.
fn underscore_ok(s: &[u8]) -> bool {
    // Go: strconv/atoi.go underscoreOK
    // saw tracks the last character (class) we saw:
    // ^ for beginning of number,
    // 0 for a digit or base prefix,
    // _ for an underscore,
    // ! for none of the above.
    let mut saw = b'^';
    let mut i = 0;
    let mut s = s;

    // Optional sign.
    if !s.is_empty() && (s[0] == b'-' || s[0] == b'+') {
        s = &s[1..];
    }

    // Optional base prefix.
    let mut hex = false;
    if s.len() >= 2 && s[0] == b'0' && matches!(lower(s[1]), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0'; // base prefix counts as a digit for "underscore as digit separator"
        hex = lower(s[1]) == b'x';
    }

    // Number proper.
    while i < s.len() {
        // Digits are always okay.
        if s[i].is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(s[i])) {
            saw = b'0';
            i += 1;
            continue;
        }
        // Underscore must follow digit.
        if s[i] == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
            i += 1;
            continue;
        }
        // Underscore must also be followed by digit.
        if saw == b'_' {
            return false;
        }
        // Saw non-digit, non-underscore.
        saw = b'!';
        i += 1;
    }
    saw != b'_'
}

/// Go: `strconv.ParseFloat(s, 64)`; `None` on any error (syntax, or `ErrRange` on overflow).
/// `resolve` passes only texts that start with `.` or match `yamlStyleFloat`, so a
/// hexadecimal mantissa (`0x1p-2`) never reaches it; that form is validated as Go does but
/// not converted (`None`).
fn parse_float(s: &str) -> Option<f64> {
    // Go: strconv/atof.go ParseFloat (atof64)
    if let Some(f) = special(s) {
        return Some(f);
    }
    let (hex, n) = read_float(s.as_bytes())?;
    if n != s.len() || hex {
        return None;
    }
    // Go's decimal conversion is correctly rounded, as is Rust's; underscores were validated.
    let f: f64 = s.replace('_', "").parse().ok()?;
    if f.is_infinite() {
        // Go: ErrRange (the value overflows float64).
        return None;
    }
    Some(f)
}

/// Go: strconv `special` — `±Inf`/`±Infinity`/`NaN` (any case) when that is all of `s`
/// (`ParseFloat` rejects a longer `s`).
fn special(s: &str) -> Option<f64> {
    // Go: strconv/atof.go special
    let b = s.as_bytes();
    let first = *b.first()?;
    let (sign, rest) = match first {
        b'+' => (1.0, &b[1..]),
        b'-' => (-1.0, &b[1..]),
        b'i' | b'I' => (1.0, b),
        b'n' | b'N' => {
            return b.eq_ignore_ascii_case(b"nan").then_some(f64::NAN);
        }
        _ => return None,
    };
    // Anything longer than "inf" is ok, but if we don't have "infinity", only consume "inf".
    if rest.eq_ignore_ascii_case(b"inf") || rest.eq_ignore_ascii_case(b"infinity") {
        return Some(sign * f64::INFINITY);
    }
    None
}

/// Go: strconv `readFloat`'s syntax check: `Some((hex, n))` when the first `n` bytes of `s`
/// form a float (`n` may be short of `s.len()`, which `ParseFloat` rejects).
fn read_float(s: &[u8]) -> Option<(bool, usize)> {
    // Go: strconv/atof.go readFloat
    let mut underscores = false;
    let mut i = 0;

    // optional sign
    if i >= s.len() {
        return None;
    }
    if s[i] == b'+' || s[i] == b'-' {
        i += 1;
    }

    // digits
    let mut hex = false;
    let mut exp_char = b'e';
    if i + 2 < s.len() && s[i] == b'0' && lower(s[i + 1]) == b'x' {
        i += 2;
        exp_char = b'p';
        hex = true;
    }
    let mut sawdot = false;
    let mut sawdigits = false;
    while i < s.len() {
        let c = s[i];
        if c == b'_' {
            underscores = true;
        } else if c == b'.' {
            if sawdot {
                break;
            }
            sawdot = true;
        } else if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(c)) {
            sawdigits = true;
        } else {
            break;
        }
        i += 1;
    }
    if !sawdigits {
        return None;
    }

    // optional exponent moves decimal point.
    if i < s.len() && lower(s[i]) == exp_char {
        i += 1;
        if i >= s.len() {
            return None;
        }
        if s[i] == b'+' || s[i] == b'-' {
            i += 1;
        }
        if i >= s.len() || !s[i].is_ascii_digit() {
            return None;
        }
        while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'_') {
            if s[i] == b'_' {
                underscores = true;
            }
            i += 1;
        }
    } else if hex {
        // Must have exponent.
        return None;
    }

    if underscores && !underscore_ok(&s[..i]) {
        return None;
    }
    Some((hex, i))
}

// ---------------------------------------------------------------------------------------------
// Go's time.Parse, for the chunks of `allowedTimestampFormats` (time/format.go).
// ---------------------------------------------------------------------------------------------

/// One element of a Go time layout as `time.nextStdChunk` splits it (only the kinds the
/// `allowedTimestampFormats` layouts contain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chunk {
    /// Layout text matched literally (a space matches any run of spaces: Go's `skip`).
    Literal(&'static str),
    /// `2006` (`stdLongYear`).
    LongYear,
    /// `1` (`stdNumMonth`).
    NumMonth,
    /// `2` (`stdDay`).
    Day,
    /// `15` (`stdHour`).
    Hour,
    /// `4` (`stdMinute`).
    Minute,
    /// `5` (`stdSecond`).
    Second,
    /// `.999999999` (`stdFracSecond9`).
    FracSecond9,
    /// `Z07:00` (`stdISO8601ColonTZ`).
    Iso8601ColonTz,
}

/// Go: time `isDigit`.
fn is_digit(s: &[u8], i: usize) -> bool {
    i < s.len() && s[i].is_ascii_digit()
}

/// Go: time `getnum` — one or two digits (`fixed`: exactly two).
fn getnum(s: &[u8], fixed: bool) -> Option<(u32, &[u8])> {
    // Go: time/format.go getnum
    if !is_digit(s, 0) {
        return None;
    }
    if !is_digit(s, 1) {
        if fixed {
            return None;
        }
        return Some((u32::from(s[0] - b'0'), &s[1..]));
    }
    Some((
        u32::from(s[0] - b'0') * 10 + u32::from(s[1] - b'0'),
        &s[2..],
    ))
}

/// Go: time `cutspace`.
fn cutspace(mut s: &[u8]) -> &[u8] {
    while let [b' ', rest @ ..] = s {
        s = rest;
    }
    s
}

/// Go: time `skip` — removes `prefix` from `value`, treating runs of spaces as equivalent.
fn skip<'a>(mut value: &'a [u8], prefix: &str) -> Option<&'a [u8]> {
    // Go: time/format.go skip
    let mut prefix = prefix.as_bytes();
    while let Some(&p) = prefix.first() {
        if p == b' ' {
            if !value.is_empty() && value[0] != b' ' {
                return None;
            }
            prefix = cutspace(prefix);
            value = cutspace(value);
            continue;
        }
        if value.is_empty() || value[0] != p {
            return None;
        }
        prefix = &prefix[1..];
        value = &value[1..];
    }
    Some(value)
}

/// Go: time `parseNanoseconds` — the fraction `value[1..nbytes]` (at most 9 digits used) in
/// nanoseconds; `value[0]` is `.` or `,`.
fn parse_nanoseconds(value: &[u8], nbytes: usize) -> Option<u32> {
    // Go: time/format.go parseNanoseconds
    if value[0] != b'.' && value[0] != b',' {
        return None;
    }
    let nbytes = nbytes.min(10);
    let digits = &value[1..nbytes];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        // Go's atoi fails on an empty or non-digit string.
        return None;
    }
    let mut ns: u32 = 0;
    for &d in digits {
        ns = ns * 10 + u32::from(d - b'0');
    }
    // We need nanoseconds, which means scaling by the number
    // of missing digits in the format, maximum length 10.
    for _ in 0..(10 - nbytes) {
        ns *= 10;
    }
    Some(ns)
}

/// Go: `time.Parse(layout, value)` for a layout given as its chunks; `None` on any error.
fn time_parse(layout: &[Chunk], value: &str) -> Option<DateTime<FixedOffset>> {
    // Go: time/format.go parse
    let mut value = value.as_bytes();
    let (mut year, mut month, mut day) = (0i32, 0u32, 0u32);
    let (mut hour, mut min, mut sec, mut nsec) = (0u32, 0u32, 0u32, 0u32);
    // Go's `z` (UTC from `Z`) and `zoneOffset` (-1 = none): `Some(seconds east)`.
    let mut zone_offset: Option<i32> = None;

    for (k, chunk) in layout.iter().enumerate() {
        match *chunk {
            Chunk::Literal(prefix) => value = skip(value, prefix)?,
            Chunk::LongYear => {
                if value.len() < 4 || !is_digit(value, 0) {
                    return None;
                }
                // Go's atoi of value[0:4] (parseTimestamp checked the four digits).
                let p = std::str::from_utf8(&value[..4]).ok()?;
                year = p.parse().ok()?;
                value = &value[4..];
            }
            Chunk::NumMonth => {
                let (m, rest) = getnum(value, false)?;
                if m == 0 || 12 < m {
                    return None; // month out of range
                }
                month = m;
                value = rest;
            }
            Chunk::Day => {
                // Note that we allow any one- or two-digit day here.
                // The month, day, year combination is validated after we've completed parsing.
                let (d, rest) = getnum(value, false)?;
                day = d;
                value = rest;
            }
            Chunk::Hour => {
                let (h, rest) = getnum(value, false)?;
                if 24 <= h {
                    return None; // hour out of range
                }
                hour = h;
                value = rest;
            }
            Chunk::Minute => {
                let (m, rest) = getnum(value, false)?;
                if 60 <= m {
                    return None; // minute out of range
                }
                min = m;
                value = rest;
            }
            Chunk::Second => {
                let (s, rest) = getnum(value, false)?;
                if 60 <= s {
                    return None; // second out of range
                }
                sec = s;
                value = rest;
                // Special case: do we have a fractional second but no
                // fractional second in the format?
                if value.len() >= 2 && (value[0] == b'.' || value[0] == b',') && is_digit(value, 1)
                {
                    // Go: `_, std, _ = nextStdChunk(layout)`, the next non-literal chunk.
                    let next = layout[k + 1..]
                        .iter()
                        .find(|c| !matches!(c, Chunk::Literal(_)));
                    if next != Some(&Chunk::FracSecond9) {
                        // No fractional second in the layout but we have one in the input.
                        let mut n = 2;
                        while n < value.len() && is_digit(value, n) {
                            n += 1;
                        }
                        nsec = parse_nanoseconds(value, n)?;
                        value = &value[n..];
                    }
                    // Else: fractional second in the layout; proceed normally.
                }
            }
            Chunk::FracSecond9 => {
                if value.len() < 2
                    || !(value[0] == b'.' || value[0] == b',')
                    || !value[1].is_ascii_digit()
                {
                    // Fractional second omitted.
                    continue;
                }
                // Take any number of digits, even more than asked for,
                // because it is what the stdSecond case would do.
                let mut i = 0;
                while i + 1 < value.len() && value[i + 1].is_ascii_digit() {
                    i += 1;
                }
                nsec = parse_nanoseconds(value, 1 + i)?;
                value = &value[1 + i..];
            }
            Chunk::Iso8601ColonTz => {
                if !value.is_empty() && value[0] == b'Z' {
                    value = &value[1..];
                    zone_offset = Some(0); // UTC
                    continue;
                }
                if value.len() < 6 || value[3] != b':' {
                    return None;
                }
                let (sign, hh, mm) = (value[0], &value[1..3], &value[4..6]);
                value = &value[6..];
                let (hr, _) = getnum(hh, true)?;
                let (mm, _) = getnum(mm, true)?;
                // The range test use > rather than >=,
                // as some people do write offsets of 24 hours
                // or 60 minutes or 60 seconds.
                if hr > 24 || mm > 60 {
                    return None;
                }
                let offset = i32::try_from((hr * 60 + mm) * 60).ok()?;
                zone_offset = Some(match sign {
                    b'+' => offset,
                    b'-' => -offset,
                    _ => return None,
                });
            }
        }
    }
    if !value.is_empty() {
        return None; // extra text
    }

    // Validate the day of the month (and build the date).
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let datetime = date.and_hms_nano_opt(hour, min, sec, nsec)?;
    // Divergence (ledger 6c-1): Go accepts zone offsets up to 24:60 (25h); chrono's
    // `FixedOffset` must be under 24h, so such a timestamp does not resolve here.
    let offset = FixedOffset::east_opt(zone_offset.unwrap_or(0))?;
    offset.from_local_datetime(&datetime).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolved value as text: `null`, `bool true`, `int 10`, `uint 18446744073709551615`,
    /// `float <IEEE bits as 16 hex digits>` (`float NaN` for any NaN), `str <s>`,
    /// `ts <RFC3339Nano>` (the form the Go generator printed the expectations in).
    fn show(v: &GoValue) -> String {
        match v {
            GoValue::Null => "null".to_string(),
            GoValue::Bool(b) => format!("bool {b}"),
            GoValue::Int(i) => format!("int {i}"),
            GoValue::Uint(u) => format!("uint {u}"),
            GoValue::Float(f) if f.is_nan() => "float NaN".to_string(),
            GoValue::Float(f) => format!("float {:016x}", f.to_bits()),
            GoValue::String(s) => format!("str {s}"),
            GoValue::Timestamp(t) => format!("ts {}", gadk_platform::go_format_rfc3339_nano(t)),
            other => panic!("resolve never yields {other:?}"),
        }
    }

    type Row = (
        &'static str,
        &'static str,
        Result<(&'static str, &'static str), &'static str>,
    );

    fn check(rows: &[Row]) {
        for (tag, input, want) in rows {
            let got = resolve(tag, input)
                .map(|(rtag, v)| (rtag, show(&v)))
                .map_err(|e| e.to_string());
            let want = want
                .map(|(rtag, v)| (rtag.to_string(), v.to_string()))
                .map_err(str::to_string);
            assert_eq!(got, want, "resolve({tag:?}, {input:?})");
        }
    }

    /// Go's `resolve` on untagged scalars: every resolve-relevant `data` value of
    /// decode_test.go `unmarshalTests` (scalars into `interface{}`, timestamps, the
    /// 1.1-bool rows into `bool`, the commented-out base-60 and timestamp rows), node_test.go
    /// scalar values, then the resolve map, ints, floats, timestamps and strings around every
    /// branch. Expectations were printed by resolve.go itself (copied into a Go `main`,
    /// Go 1.27.2) as `(tag, input, Ok((rtag, show(value))) | Err(text))`.
    #[test]
    fn resolve_untagged_matches_go() {
        check(&[
            ("", "hi", Ok(("!!str", "str hi"))),
            ("", "true", Ok(("!!bool", "bool true"))),
            ("", "10", Ok(("!!int", "int 10"))),
            ("", "0b10", Ok(("!!int", "int 2"))),
            ("", "0xA", Ok(("!!int", "int 10"))),
            ("", "4294967296", Ok(("!!int", "int 4294967296"))),
            ("", "0.1", Ok(("!!float", "float 3fb999999999999a"))),
            ("", ".1", Ok(("!!float", "float 3fb999999999999a"))),
            ("", ".Inf", Ok(("!!float", "float 7ff0000000000000"))),
            ("", "-.Inf", Ok(("!!float", "float fff0000000000000"))),
            ("", "-10", Ok(("!!int", "int -10"))),
            ("", "-.1", Ok(("!!float", "float bfb999999999999a"))),
            ("", "123", Ok(("!!int", "int 123"))),
            ("", "6.8523e+5", Ok(("!!float", "float 4124e95c00000000"))),
            (
                "",
                "685.230_15e+03",
                Ok(("!!float", "float 4124e95c4ccccccd")),
            ),
            ("", "685_230.15", Ok(("!!float", "float 4124e95c4ccccccd"))),
            ("", "-.inf", Ok(("!!float", "float fff0000000000000"))),
            ("", "false", Ok(("!!bool", "bool false"))),
            ("", "True", Ok(("!!bool", "bool true"))),
            ("", "False", Ok(("!!bool", "bool false"))),
            ("", "TRUE", Ok(("!!bool", "bool true"))),
            ("", "FALSE", Ok(("!!bool", "bool false"))),
            ("", "on", Ok(("!!str", "str on"))),
            ("", "y", Ok(("!!str", "str y"))),
            ("", "Off", Ok(("!!str", "str Off"))),
            ("", "No", Ok(("!!str", "str No"))),
            ("", "other", Ok(("!!str", "str other"))),
            ("", "685230", Ok(("!!int", "int 685230"))),
            ("", "+685_230", Ok(("!!int", "int 685230"))),
            ("", "02472256", Ok(("!!int", "int 685230"))),
            ("", "-02472256", Ok(("!!int", "int -685230"))),
            ("", "0o2472256", Ok(("!!int", "int 685230"))),
            ("", "-0o2472256", Ok(("!!int", "int -685230"))),
            ("", "0x_0A_74_AE", Ok(("!!int", "int 685230"))),
            (
                "",
                "0b1010_0111_0100_1010_1110",
                Ok(("!!int", "int 685230")),
            ),
            ("", "-0b101010", Ok(("!!int", "int -42"))),
            (
                "",
                "-0b1000000000000000000000000000000000000000000000000000000000000000",
                Ok(("!!int", "int -9223372036854775808")),
            ),
            ("", "", Ok(("!!null", "null"))),
            ("", "~", Ok(("!!null", "null"))),
            ("", "null", Ok(("!!null", "null"))),
            ("", "null key", Ok(("!!str", "str null key"))),
            ("", "1.2.3.4", Ok(("!!str", "str 1.2.3.4"))),
            (
                "",
                "2015-02-24T18:19:39Z",
                Ok(("!!timestamp", "ts 2015-02-24T18:19:39Z")),
            ),
            (
                "",
                "2015-01-01",
                Ok(("!!timestamp", "ts 2015-01-01T00:00:00Z")),
            ),
            (
                "",
                "2015-02-24T18:19:39.12Z",
                Ok(("!!timestamp", "ts 2015-02-24T18:19:39.12Z")),
            ),
            (
                "",
                "2015-2-3T3:4:5Z",
                Ok(("!!timestamp", "ts 2015-02-03T03:04:05Z")),
            ),
            (
                "",
                "2015-02-24t18:19:39Z",
                Ok(("!!timestamp", "ts 2015-02-24T18:19:39Z")),
            ),
            (
                "",
                "2015-02-24 18:19:39",
                Ok(("!!timestamp", "ts 2015-02-24T18:19:39Z")),
            ),
            (
                "",
                "2001-12-14 21:59:43.10 -5",
                Ok(("!!str", "str 2001-12-14 21:59:43.10 -5")),
            ),
            (
                "",
                "2001-12-14 \t\t \t21:59:43.10 \t Z",
                Ok(("!!str", "str 2001-12-14 \t\t \t21:59:43.10 \t Z")),
            ),
            ("", "123456e1", Ok(("!!float", "float 4132d68000000000"))),
            ("", "123456E1", Ok(("!!float", "float 4132d68000000000"))),
            ("", "-b_c", Ok(("!!str", "str -b_c"))),
            ("", "+b_c", Ok(("!!str", "str +b_c"))),
            (
                "",
                "50cent_of_dollar",
                Ok(("!!str", "str 50cent_of_dollar")),
            ),
            ("", "BAR!", Ok(("!!str", "str BAR!"))),
            ("", "there", Ok(("!!str", "str there"))),
            ("", "A", Ok(("!!str", "str A"))),
            ("", "1", Ok(("!!int", "int 1"))),
            ("", "190:20:30.15", Ok(("!!str", "str 190:20:30.15"))),
            ("", "190:20:30", Ok(("!!str", "str 190:20:30"))),
            ("", ".NaN", Ok(("!!float", "float NaN"))),
            ("", "<<", Ok(("!!str", "str <<"))),
            ("", "a", Ok(("!!str", "str a"))),
            ("", "b", Ok(("!!str", "str b"))),
            ("", "c", Ok(("!!str", "str c"))),
            ("", ".nan", Ok(("!!float", "float NaN"))),
            ("", "0.1000", Ok(("!!float", "float 3fb999999999999a"))),
            ("", "2", Ok(("!!int", "int 2"))),
            ("", "foo", Ok(("!!str", "str foo"))),
            ("", "b c", Ok(("!!str", "str b c"))),
            ("", "str", Ok(("!!str", "str str"))),
            ("", "Null", Ok(("!!null", "null"))),
            ("", "NULL", Ok(("!!null", "null"))),
            ("", ".NAN", Ok(("!!float", "float NaN"))),
            ("", ".inf", Ok(("!!float", "float 7ff0000000000000"))),
            ("", ".INF", Ok(("!!float", "float 7ff0000000000000"))),
            ("", "+.inf", Ok(("!!float", "float 7ff0000000000000"))),
            ("", "+.Inf", Ok(("!!float", "float 7ff0000000000000"))),
            ("", "+.INF", Ok(("!!float", "float 7ff0000000000000"))),
            ("", "-.INF", Ok(("!!float", "float fff0000000000000"))),
            ("", "nULL", Ok(("!!str", "str nULL"))),
            ("", "tRUE", Ok(("!!str", "str tRUE"))),
            ("", ".iNf", Ok(("!!str", "str .iNf"))),
            ("", "+.nan", Ok(("!!str", "str +.nan"))),
            ("", "-.nan", Ok(("!!str", "str -.nan"))),
            ("", "0", Ok(("!!int", "int 0"))),
            ("", "-0", Ok(("!!int", "int 0"))),
            ("", "+0", Ok(("!!int", "int 0"))),
            ("", "00", Ok(("!!int", "int 0"))),
            ("", "08", Ok(("!!float", "float 4020000000000000"))),
            ("", "0x", Ok(("!!str", "str 0x"))),
            ("", "0b", Ok(("!!str", "str 0b"))),
            ("", "0o", Ok(("!!str", "str 0o"))),
            ("", "0b2", Ok(("!!str", "str 0b2"))),
            ("", "0o8", Ok(("!!str", "str 0o8"))),
            ("", "0xG", Ok(("!!str", "str 0xG"))),
            ("", "1_000", Ok(("!!int", "int 1000"))),
            ("", "_1", Ok(("!!str", "str _1"))),
            ("", "1_", Ok(("!!int", "int 1"))),
            ("", "1__0", Ok(("!!int", "int 10"))),
            (
                "",
                "9223372036854775807",
                Ok(("!!int", "int 9223372036854775807")),
            ),
            (
                "",
                "9223372036854775808",
                Ok(("!!int", "uint 9223372036854775808")),
            ),
            (
                "",
                "-9223372036854775808",
                Ok(("!!int", "int -9223372036854775808")),
            ),
            (
                "",
                "-9223372036854775809",
                Ok(("!!float", "float c3e0000000000000")),
            ),
            (
                "",
                "18446744073709551615",
                Ok(("!!int", "uint 18446744073709551615")),
            ),
            (
                "",
                "18446744073709551616",
                Ok(("!!float", "float 43f0000000000000")),
            ),
            (
                "",
                "0xFFFFFFFFFFFFFFFF",
                Ok(("!!int", "uint 18446744073709551615")),
            ),
            (
                "",
                "0x10000000000000000",
                Ok(("!!str", "str 0x10000000000000000")),
            ),
            (
                "",
                "0b1111111111111111111111111111111111111111111111111111111111111111",
                Ok(("!!int", "uint 18446744073709551615")),
            ),
            (
                "",
                "-0b1111111111111111111111111111111111111111111111111111111111111111",
                Ok((
                    "!!str",
                    "str -0b1111111111111111111111111111111111111111111111111111111111111111",
                )),
            ),
            (
                "",
                "0o1777777777777777777777",
                Ok(("!!int", "uint 18446744073709551615")),
            ),
            (
                "",
                "0o2000000000000000000000",
                Ok(("!!str", "str 0o2000000000000000000000")),
            ),
            (
                "",
                "-0o1777777777777777777777",
                Ok(("!!str", "str -0o1777777777777777777777")),
            ),
            (
                "",
                "-0o1000000000000000000000",
                Ok(("!!int", "int -9223372036854775808")),
            ),
            (
                "",
                "-0o1000000000000000000001",
                Ok(("!!str", "str -0o1000000000000000000001")),
            ),
            ("", "0O17", Ok(("!!int", "int 15"))),
            ("", "0B11", Ok(("!!int", "int 3"))),
            ("", "0X1f", Ok(("!!int", "int 31"))),
            ("", "-0x10", Ok(("!!int", "int -16"))),
            ("", "+0x10", Ok(("!!int", "int 16"))),
            ("", "-0B11", Ok(("!!int", "int -3"))),
            ("", "-0O17", Ok(("!!int", "int -15"))),
            ("", "1e3", Ok(("!!float", "float 408f400000000000"))),
            ("", "-1e3", Ok(("!!float", "float c08f400000000000"))),
            ("", "+1", Ok(("!!int", "int 1"))),
            ("", "+", Ok(("!!str", "str +"))),
            ("", "-", Ok(("!!str", "str -"))),
            ("", "+-1", Ok(("!!str", "str +-1"))),
            ("", "--1", Ok(("!!str", "str --1"))),
            ("", "1-", Ok(("!!str", "str 1-"))),
            ("", "1+", Ok(("!!str", "str 1+"))),
            ("", "0b_1", Ok(("!!int", "int 1"))),
            ("", "0x_1", Ok(("!!int", "int 1"))),
            ("", "-0b_1", Ok(("!!int", "int -1"))),
            ("", "-0o_7", Ok(("!!int", "int -7"))),
            ("", "0o_7", Ok(("!!int", "int 7"))),
            ("", "1.", Ok(("!!float", "float 3ff0000000000000"))),
            ("", "-1.", Ok(("!!float", "float bff0000000000000"))),
            ("", "1.e5", Ok(("!!float", "float 40f86a0000000000"))),
            ("", ".5e1", Ok(("!!float", "float 4014000000000000"))),
            ("", "-.5e-1", Ok(("!!float", "float bfa999999999999a"))),
            ("", "1e", Ok(("!!str", "str 1e"))),
            ("", "1e+", Ok(("!!str", "str 1e+"))),
            ("", "1e400", Ok(("!!str", "str 1e400"))),
            ("", "-1e400", Ok(("!!str", "str -1e400"))),
            ("", "1e-400", Ok(("!!float", "float 0000000000000000"))),
            ("", "1E5", Ok(("!!float", "float 40f86a0000000000"))),
            ("", "1.5E+3", Ok(("!!float", "float 4097700000000000"))),
            ("", "0.0", Ok(("!!float", "float 0000000000000000"))),
            ("", "-0.0", Ok(("!!float", "float 8000000000000000"))),
            ("", "+0.0", Ok(("!!float", "float 0000000000000000"))),
            ("", "1_000.5", Ok(("!!float", "float 408f440000000000"))),
            ("", "1.000_5", Ok(("!!float", "float 3ff0020c49ba5e35"))),
            ("", "._5", Ok(("!!str", "str ._5"))),
            ("", ".5_", Ok(("!!str", "str .5_"))),
            ("", ".1_0", Ok(("!!float", "float 3fb999999999999a"))),
            ("", "1e1_0", Ok(("!!float", "float 4202a05f20000000"))),
            ("", "1_e1", Ok(("!!float", "float 4024000000000000"))),
            ("", "1._5", Ok(("!!float", "float 3ff8000000000000"))),
            ("", "0x1p-2", Ok(("!!str", "str 0x1p-2"))),
            ("", "0x1.8p1", Ok(("!!str", "str 0x1.8p1"))),
            ("", ".0x1p1", Ok(("!!str", "str .0x1p1"))),
            ("", "1.5x", Ok(("!!str", "str 1.5x"))),
            ("", "1.5.5", Ok(("!!str", "str 1.5.5"))),
            ("", "..5", Ok(("!!str", "str ..5"))),
            ("", ".", Ok(("!!str", "str ."))),
            ("", ".e1", Ok(("!!str", "str .e1"))),
            ("", ".5e", Ok(("!!str", "str .5e"))),
            ("", "Infinity", Ok(("!!str", "str Infinity"))),
            ("", "inf", Ok(("!!str", "str inf"))),
            ("", "-inf", Ok(("!!str", "str -inf"))),
            ("", "+Infinity", Ok(("!!str", "str +Infinity"))),
            ("", "nan", Ok(("!!str", "str nan"))),
            ("", "NaN", Ok(("!!str", "str NaN"))),
            (
                "",
                "123456789012345678901234567890",
                Ok(("!!float", "float 45f8ee90ff6c373e")),
            ),
            ("", "1e308", Ok(("!!float", "float 7fe1ccf385ebc8a0"))),
            (
                "",
                "1.7976931348623157e308",
                Ok(("!!float", "float 7fefffffffffffff")),
            ),
            (
                "",
                "1.7976931348623159e308",
                Ok(("!!str", "str 1.7976931348623159e308")),
            ),
            ("", "4.9e-324", Ok(("!!float", "float 0000000000000001"))),
            ("", "2.4e-324", Ok(("!!float", "float 0000000000000000"))),
            ("", "2.5e-324", Ok(("!!float", "float 0000000000000001"))),
            ("", "0.1e-1000", Ok(("!!float", "float 0000000000000000"))),
            (
                "",
                "2001-12-14t21:59:43.10-05:00",
                Ok(("!!timestamp", "ts 2001-12-14T21:59:43.1-05:00")),
            ),
            (
                "",
                "2001-12-14T21:59:43.10+05:30",
                Ok(("!!timestamp", "ts 2001-12-14T21:59:43.1+05:30")),
            ),
            (
                "",
                "2001-12-14 21:59:43.10",
                Ok(("!!timestamp", "ts 2001-12-14T21:59:43.1Z")),
            ),
            (
                "",
                "2001-12-14  21:59:43",
                Ok(("!!timestamp", "ts 2001-12-14T21:59:43Z")),
            ),
            (
                "",
                "2001-12-14",
                Ok(("!!timestamp", "ts 2001-12-14T00:00:00Z")),
            ),
            (
                "",
                "2001-1-1",
                Ok(("!!timestamp", "ts 2001-01-01T00:00:00Z")),
            ),
            ("", "2001-13-01", Ok(("!!str", "str 2001-13-01"))),
            ("", "2001-02-29", Ok(("!!str", "str 2001-02-29"))),
            (
                "",
                "2000-02-29",
                Ok(("!!timestamp", "ts 2000-02-29T00:00:00Z")),
            ),
            ("", "2001-12-32", Ok(("!!str", "str 2001-12-32"))),
            ("", "2001-12-00", Ok(("!!str", "str 2001-12-00"))),
            ("", "2001-00-10", Ok(("!!str", "str 2001-00-10"))),
            (
                "",
                "2001-12-14T24:00:00Z",
                Ok(("!!str", "str 2001-12-14T24:00:00Z")),
            ),
            (
                "",
                "2001-12-14T23:60:00Z",
                Ok(("!!str", "str 2001-12-14T23:60:00Z")),
            ),
            (
                "",
                "2001-12-14T23:59:60Z",
                Ok(("!!str", "str 2001-12-14T23:59:60Z")),
            ),
            (
                "",
                "2001-12-14T23:59:59.1234567891Z",
                Ok(("!!timestamp", "ts 2001-12-14T23:59:59.123456789Z")),
            ),
            (
                "",
                "2001-12-14T23:59:59,5Z",
                Ok(("!!timestamp", "ts 2001-12-14T23:59:59.5Z")),
            ),
            (
                "",
                "2001-12-14T23:59:59.Z",
                Ok(("!!str", "str 2001-12-14T23:59:59.Z")),
            ),
            (
                "",
                "2001-12-14T23:59:59z",
                Ok(("!!str", "str 2001-12-14T23:59:59z")),
            ),
            (
                "",
                "2001-12-14T23:59:59",
                Ok(("!!str", "str 2001-12-14T23:59:59")),
            ),
            (
                "",
                "2001-12-14T23:59:59+0500",
                Ok(("!!str", "str 2001-12-14T23:59:59+0500")),
            ),
            (
                "",
                "2001-12-14T23:59:59+05:00",
                Ok(("!!timestamp", "ts 2001-12-14T23:59:59+05:00")),
            ),
            // Divergence (ledger 6c-1): Go "ts 2001-12-14T23:59:59-24:00" (a zone offset of
            // 24h or more, which chrono cannot represent).
            (
                "",
                "2001-12-14T23:59:59-24:00",
                Ok(("!!str", "str 2001-12-14T23:59:59-24:00")),
            ),
            (
                "",
                "2001-12-14T23:59:59+25:00",
                Ok(("!!str", "str 2001-12-14T23:59:59+25:00")),
            ),
            (
                "",
                "2001-12-14T23:59:59+05:61",
                Ok(("!!str", "str 2001-12-14T23:59:59+05:61")),
            ),
            // Divergence (ledger 6c-1): Go "ts 2001-12-14T23:59:59+24:00" (a zone offset of
            // 24h or more, which chrono cannot represent).
            (
                "",
                "2001-12-14T23:59:59+24:00",
                Ok(("!!str", "str 2001-12-14T23:59:59+24:00")),
            ),
            // Divergence (ledger 6c-1): Go "ts 2001-12-14T23:59:59+25:00" (a zone offset of
            // 24h or more, which chrono cannot represent).
            (
                "",
                "2001-12-14T23:59:59+24:60",
                Ok(("!!str", "str 2001-12-14T23:59:59+24:60")),
            ),
            (
                "",
                "2001-12-14T23:59:59-00:30",
                Ok(("!!timestamp", "ts 2001-12-14T23:59:59-00:30")),
            ),
            (
                "",
                "2001-12-14T23:59:59+05:60",
                Ok(("!!timestamp", "ts 2001-12-14T23:59:59+06:00")),
            ),
            (
                "",
                "2001-12-14T1:2:3Z",
                Ok(("!!timestamp", "ts 2001-12-14T01:02:03Z")),
            ),
            (
                "",
                "2001-12-14T01:02:03.000000001Z",
                Ok(("!!timestamp", "ts 2001-12-14T01:02:03.000000001Z")),
            ),
            (
                "",
                "2001-12-14 1:2:3.5",
                Ok(("!!timestamp", "ts 2001-12-14T01:02:03.5Z")),
            ),
            ("", "2001-12-14 1:2", Ok(("!!str", "str 2001-12-14 1:2"))),
            ("", "2001-12-14x", Ok(("!!str", "str 2001-12-14x"))),
            ("", "2001-12-14 ", Ok(("!!str", "str 2001-12-14 "))),
            ("", "2001-12-14T", Ok(("!!str", "str 2001-12-14T"))),
            ("", "201-12-14", Ok(("!!str", "str 201-12-14"))),
            ("", "20011-12-14", Ok(("!!str", "str 20011-12-14"))),
            ("", "2001/12/14", Ok(("!!str", "str 2001/12/14"))),
            ("", "2001-012-14", Ok(("!!str", "str 2001-012-14"))),
            ("", "2001-12-140", Ok(("!!str", "str 2001-12-140"))),
            (
                "",
                "0000-01-01",
                Ok(("!!timestamp", "ts 0000-01-01T00:00:00Z")),
            ),
            (
                "",
                "9999-12-31T23:59:59.999999999Z",
                Ok(("!!timestamp", "ts 9999-12-31T23:59:59.999999999Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43.10+05:30 ",
                Ok(("!!str", "str 2001-12-14T21:59:43.10+05:30 ")),
            ),
            (
                "",
                "2001-12-14 21:59:43.10Z",
                Ok(("!!str", "str 2001-12-14 21:59:43.10Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43.10 Z",
                Ok(("!!str", "str 2001-12-14T21:59:43.10 Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43.10+05:30x",
                Ok(("!!str", "str 2001-12-14T21:59:43.10+05:30x")),
            ),
            (
                "",
                "2001-12-14T21:59:43.10-00:00",
                Ok(("!!timestamp", "ts 2001-12-14T21:59:43.1Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43.10+00:00",
                Ok(("!!timestamp", "ts 2001-12-14T21:59:43.1Z")),
            ),
            (
                "",
                "2001-1-2T3:4:5.999999999999Z",
                Ok(("!!timestamp", "ts 2001-01-02T03:04:05.999999999Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43.Z",
                Ok(("!!str", "str 2001-12-14T21:59:43.Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43.1.2Z",
                Ok(("!!str", "str 2001-12-14T21:59:43.1.2Z")),
            ),
            (
                "",
                "2001-12-14T21:59:43+5:30",
                Ok(("!!str", "str 2001-12-14T21:59:43+5:30")),
            ),
            (
                "",
                "2001-12-14T21:59:43+05:3",
                Ok(("!!str", "str 2001-12-14T21:59:43+05:3")),
            ),
            (
                "",
                "2001-12-14T21:59:43+05-30",
                Ok(("!!str", "str 2001-12-14T21:59:43+05-30")),
            ),
            (
                "",
                "2001-12-14\t21:59:43",
                Ok(("!!str", "str 2001-12-14\t21:59:43")),
            ),
            ("", "-2001-12-14", Ok(("!!str", "str -2001-12-14"))),
            ("", "+2001-12-14", Ok(("!!str", "str +2001-12-14"))),
            (
                "",
                "2001-12-14T21:59:43.10Z07:00",
                Ok(("!!str", "str 2001-12-14T21:59:43.10Z07:00")),
            ),
            ("", "x", Ok(("!!str", "str x"))),
            ("", "yes", Ok(("!!str", "str yes"))),
            ("", "no", Ok(("!!str", "str no"))),
            ("", "1:20", Ok(("!!str", "str 1:20"))),
            ("", "-1:20", Ok(("!!str", "str -1:20"))),
            ("", "1:1", Ok(("!!str", "str 1:1"))),
            ("", "1:60", Ok(("!!str", "str 1:60"))),
            ("", "1:5:9.5", Ok(("!!str", "str 1:5:9.5"))),
            ("", "12_3", Ok(("!!int", "int 123"))),
            ("", "+-", Ok(("!!str", "str +-"))),
            ("", "-.", Ok(("!!str", "str -."))),
            ("", "+.", Ok(("!!str", "str +."))),
            ("", "-a", Ok(("!!str", "str -a"))),
            ("", "+a", Ok(("!!str", "str +a"))),
            ("", "1a", Ok(("!!str", "str 1a"))),
            ("", "é", Ok(("!!str", "str é"))),
            ("", "日本", Ok(("!!str", "str 日本"))),
            ("", "y ", Ok(("!!str", "str y "))),
            ("", " 1", Ok(("!!str", "str  1"))),
        ]);
    }

    /// Go's `resolve` under explicit tags: each resolvable and non-resolvable tag (short and
    /// long forms, `!`, local tags) over a fixed set of inputs, then the tagged rows of
    /// decode_test.go (`v: !!float '1.1'`, `!!float 0`, `!!float -1`, `!!null ''`,
    /// `%TAG !y! tag:yaml.org,2002:` + `!y!int '1'`, `! test`, `!!str 2015-01-01`,
    /// `!!timestamp "2015-01-01"`, unmarshalErrorTests `!!float 'error'`, unmarshalerTests
    /// `!!foo 'BAR!'`) and node_test.go (`!!str 123`, `!!binary gIGC`).
    #[test]
    fn resolve_tagged_matches_go() {
        check(&[
            ("!!str", "", Ok(("!!str", "str "))),
            ("!!str", "1", Ok(("!!str", "str 1"))),
            ("!!str", "-1", Ok(("!!str", "str -1"))),
            ("!!str", "1.5", Ok(("!!str", "str 1.5"))),
            ("!!str", "0x10", Ok(("!!str", "str 0x10"))),
            ("!!str", "true", Ok(("!!str", "str true"))),
            ("!!str", "~", Ok(("!!str", "str ~"))),
            ("!!str", "null", Ok(("!!str", "str null"))),
            ("!!str", ".inf", Ok(("!!str", "str .inf"))),
            ("!!str", ".nan", Ok(("!!str", "str .nan"))),
            ("!!str", "x", Ok(("!!str", "str x"))),
            ("!!str", "2015-01-01", Ok(("!!str", "str 2015-01-01"))),
            (
                "!!str",
                "18446744073709551615",
                Ok(("!!str", "str 18446744073709551615")),
            ),
            ("!!str", "<<", Ok(("!!str", "str <<"))),
            ("!!str", "1e400", Ok(("!!str", "str 1e400"))),
            ("!!int", "", Err("yaml: cannot decode !!null `` as a !!int")),
            ("!!int", "1", Ok(("!!int", "int 1"))),
            ("!!int", "-1", Ok(("!!int", "int -1"))),
            (
                "!!int",
                "1.5",
                Err("yaml: cannot decode !!float `1.5` as a !!int"),
            ),
            ("!!int", "0x10", Ok(("!!int", "int 16"))),
            (
                "!!int",
                "true",
                Err("yaml: cannot decode !!bool `true` as a !!int"),
            ),
            (
                "!!int",
                "~",
                Err("yaml: cannot decode !!null `~` as a !!int"),
            ),
            (
                "!!int",
                "null",
                Err("yaml: cannot decode !!null `null` as a !!int"),
            ),
            (
                "!!int",
                ".inf",
                Err("yaml: cannot decode !!float `.inf` as a !!int"),
            ),
            (
                "!!int",
                ".nan",
                Err("yaml: cannot decode !!float `.nan` as a !!int"),
            ),
            (
                "!!int",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!int"),
            ),
            (
                "!!int",
                "2015-01-01",
                Err("yaml: cannot decode !!str `2015-01-01` as a !!int"),
            ),
            (
                "!!int",
                "18446744073709551615",
                Ok(("!!int", "uint 18446744073709551615")),
            ),
            (
                "!!int",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!int"),
            ),
            (
                "!!int",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!int"),
            ),
            (
                "!!float",
                "",
                Err("yaml: cannot decode !!null `` as a !!float"),
            ),
            ("!!float", "1", Ok(("!!float", "float 3ff0000000000000"))),
            ("!!float", "-1", Ok(("!!float", "float bff0000000000000"))),
            ("!!float", "1.5", Ok(("!!float", "float 3ff8000000000000"))),
            ("!!float", "0x10", Ok(("!!float", "float 4030000000000000"))),
            (
                "!!float",
                "true",
                Err("yaml: cannot decode !!bool `true` as a !!float"),
            ),
            (
                "!!float",
                "~",
                Err("yaml: cannot decode !!null `~` as a !!float"),
            ),
            (
                "!!float",
                "null",
                Err("yaml: cannot decode !!null `null` as a !!float"),
            ),
            ("!!float", ".inf", Ok(("!!float", "float 7ff0000000000000"))),
            ("!!float", ".nan", Ok(("!!float", "float NaN"))),
            (
                "!!float",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!float"),
            ),
            (
                "!!float",
                "2015-01-01",
                Err("yaml: cannot decode !!str `2015-01-01` as a !!float"),
            ),
            (
                "!!float",
                "18446744073709551615",
                Err("yaml: cannot decode !!int `18446744073709551615` as a !!float"),
            ),
            (
                "!!float",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!float"),
            ),
            (
                "!!float",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!float"),
            ),
            (
                "!!bool",
                "",
                Err("yaml: cannot decode !!null `` as a !!bool"),
            ),
            (
                "!!bool",
                "1",
                Err("yaml: cannot decode !!int `1` as a !!bool"),
            ),
            (
                "!!bool",
                "-1",
                Err("yaml: cannot decode !!int `-1` as a !!bool"),
            ),
            (
                "!!bool",
                "1.5",
                Err("yaml: cannot decode !!float `1.5` as a !!bool"),
            ),
            (
                "!!bool",
                "0x10",
                Err("yaml: cannot decode !!int `0x10` as a !!bool"),
            ),
            ("!!bool", "true", Ok(("!!bool", "bool true"))),
            (
                "!!bool",
                "~",
                Err("yaml: cannot decode !!null `~` as a !!bool"),
            ),
            (
                "!!bool",
                "null",
                Err("yaml: cannot decode !!null `null` as a !!bool"),
            ),
            (
                "!!bool",
                ".inf",
                Err("yaml: cannot decode !!float `.inf` as a !!bool"),
            ),
            (
                "!!bool",
                ".nan",
                Err("yaml: cannot decode !!float `.nan` as a !!bool"),
            ),
            (
                "!!bool",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!bool"),
            ),
            (
                "!!bool",
                "2015-01-01",
                Err("yaml: cannot decode !!str `2015-01-01` as a !!bool"),
            ),
            (
                "!!bool",
                "18446744073709551615",
                Err("yaml: cannot decode !!int `18446744073709551615` as a !!bool"),
            ),
            (
                "!!bool",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!bool"),
            ),
            (
                "!!bool",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!bool"),
            ),
            ("!!null", "", Ok(("!!null", "null"))),
            (
                "!!null",
                "1",
                Err("yaml: cannot decode !!int `1` as a !!null"),
            ),
            (
                "!!null",
                "-1",
                Err("yaml: cannot decode !!int `-1` as a !!null"),
            ),
            (
                "!!null",
                "1.5",
                Err("yaml: cannot decode !!float `1.5` as a !!null"),
            ),
            (
                "!!null",
                "0x10",
                Err("yaml: cannot decode !!int `0x10` as a !!null"),
            ),
            (
                "!!null",
                "true",
                Err("yaml: cannot decode !!bool `true` as a !!null"),
            ),
            ("!!null", "~", Ok(("!!null", "null"))),
            ("!!null", "null", Ok(("!!null", "null"))),
            (
                "!!null",
                ".inf",
                Err("yaml: cannot decode !!float `.inf` as a !!null"),
            ),
            (
                "!!null",
                ".nan",
                Err("yaml: cannot decode !!float `.nan` as a !!null"),
            ),
            (
                "!!null",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!null"),
            ),
            (
                "!!null",
                "2015-01-01",
                Err("yaml: cannot decode !!str `2015-01-01` as a !!null"),
            ),
            (
                "!!null",
                "18446744073709551615",
                Err("yaml: cannot decode !!int `18446744073709551615` as a !!null"),
            ),
            (
                "!!null",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!null"),
            ),
            (
                "!!null",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!null"),
            ),
            (
                "!!timestamp",
                "",
                Err("yaml: cannot decode !!null `` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "1",
                Err("yaml: cannot decode !!int `1` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "-1",
                Err("yaml: cannot decode !!int `-1` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "1.5",
                Err("yaml: cannot decode !!float `1.5` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "0x10",
                Err("yaml: cannot decode !!int `0x10` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "true",
                Err("yaml: cannot decode !!bool `true` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "~",
                Err("yaml: cannot decode !!null `~` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "null",
                Err("yaml: cannot decode !!null `null` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                ".inf",
                Err("yaml: cannot decode !!float `.inf` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                ".nan",
                Err("yaml: cannot decode !!float `.nan` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "2015-01-01",
                Ok(("!!timestamp", "ts 2015-01-01T00:00:00Z")),
            ),
            (
                "!!timestamp",
                "18446744073709551615",
                Err("yaml: cannot decode !!int `18446744073709551615` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!timestamp"),
            ),
            (
                "!!timestamp",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!timestamp"),
            ),
            ("!!binary", "", Ok(("!!binary", "str "))),
            ("!!binary", "1", Ok(("!!binary", "str 1"))),
            ("!!binary", "-1", Ok(("!!binary", "str -1"))),
            ("!!binary", "1.5", Ok(("!!binary", "str 1.5"))),
            ("!!binary", "0x10", Ok(("!!binary", "str 0x10"))),
            ("!!binary", "true", Ok(("!!binary", "str true"))),
            ("!!binary", "~", Ok(("!!binary", "str ~"))),
            ("!!binary", "null", Ok(("!!binary", "str null"))),
            ("!!binary", ".inf", Ok(("!!binary", "str .inf"))),
            ("!!binary", ".nan", Ok(("!!binary", "str .nan"))),
            ("!!binary", "x", Ok(("!!binary", "str x"))),
            ("!!binary", "2015-01-01", Ok(("!!binary", "str 2015-01-01"))),
            (
                "!!binary",
                "18446744073709551615",
                Ok(("!!binary", "str 18446744073709551615")),
            ),
            ("!!binary", "<<", Ok(("!!binary", "str <<"))),
            ("!!binary", "1e400", Ok(("!!binary", "str 1e400"))),
            (
                "tag:yaml.org,2002:int",
                "",
                Err("yaml: cannot decode !!null `` as a !!int"),
            ),
            ("tag:yaml.org,2002:int", "1", Ok(("!!int", "int 1"))),
            ("tag:yaml.org,2002:int", "-1", Ok(("!!int", "int -1"))),
            (
                "tag:yaml.org,2002:int",
                "1.5",
                Err("yaml: cannot decode !!float `1.5` as a !!int"),
            ),
            ("tag:yaml.org,2002:int", "0x10", Ok(("!!int", "int 16"))),
            (
                "tag:yaml.org,2002:int",
                "true",
                Err("yaml: cannot decode !!bool `true` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                "~",
                Err("yaml: cannot decode !!null `~` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                "null",
                Err("yaml: cannot decode !!null `null` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                ".inf",
                Err("yaml: cannot decode !!float `.inf` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                ".nan",
                Err("yaml: cannot decode !!float `.nan` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                "2015-01-01",
                Err("yaml: cannot decode !!str `2015-01-01` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                "18446744073709551615",
                Ok(("!!int", "uint 18446744073709551615")),
            ),
            (
                "tag:yaml.org,2002:int",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:int",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!int"),
            ),
            (
                "tag:yaml.org,2002:float",
                "",
                Err("yaml: cannot decode !!null `` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "1",
                Ok(("!!float", "float 3ff0000000000000")),
            ),
            (
                "tag:yaml.org,2002:float",
                "-1",
                Ok(("!!float", "float bff0000000000000")),
            ),
            (
                "tag:yaml.org,2002:float",
                "1.5",
                Ok(("!!float", "float 3ff8000000000000")),
            ),
            (
                "tag:yaml.org,2002:float",
                "0x10",
                Ok(("!!float", "float 4030000000000000")),
            ),
            (
                "tag:yaml.org,2002:float",
                "true",
                Err("yaml: cannot decode !!bool `true` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "~",
                Err("yaml: cannot decode !!null `~` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "null",
                Err("yaml: cannot decode !!null `null` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                ".inf",
                Ok(("!!float", "float 7ff0000000000000")),
            ),
            (
                "tag:yaml.org,2002:float",
                ".nan",
                Ok(("!!float", "float NaN")),
            ),
            (
                "tag:yaml.org,2002:float",
                "x",
                Err("yaml: cannot decode !!str `x` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "2015-01-01",
                Err("yaml: cannot decode !!str `2015-01-01` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "18446744073709551615",
                Err("yaml: cannot decode !!int `18446744073709551615` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "<<",
                Err("yaml: cannot decode !!str `<<` as a !!float"),
            ),
            (
                "tag:yaml.org,2002:float",
                "1e400",
                Err("yaml: cannot decode !!str `1e400` as a !!float"),
            ),
            ("!", "", Ok(("!", "str "))),
            ("!", "1", Ok(("!", "str 1"))),
            ("!", "-1", Ok(("!", "str -1"))),
            ("!", "1.5", Ok(("!", "str 1.5"))),
            ("!", "0x10", Ok(("!", "str 0x10"))),
            ("!", "true", Ok(("!", "str true"))),
            ("!", "~", Ok(("!", "str ~"))),
            ("!", "null", Ok(("!", "str null"))),
            ("!", ".inf", Ok(("!", "str .inf"))),
            ("!", ".nan", Ok(("!", "str .nan"))),
            ("!", "x", Ok(("!", "str x"))),
            ("!", "2015-01-01", Ok(("!", "str 2015-01-01"))),
            (
                "!",
                "18446744073709551615",
                Ok(("!", "str 18446744073709551615")),
            ),
            ("!", "<<", Ok(("!", "str <<"))),
            ("!", "1e400", Ok(("!", "str 1e400"))),
            ("!foo", "", Ok(("!foo", "str "))),
            ("!foo", "1", Ok(("!foo", "str 1"))),
            ("!foo", "-1", Ok(("!foo", "str -1"))),
            ("!foo", "1.5", Ok(("!foo", "str 1.5"))),
            ("!foo", "0x10", Ok(("!foo", "str 0x10"))),
            ("!foo", "true", Ok(("!foo", "str true"))),
            ("!foo", "~", Ok(("!foo", "str ~"))),
            ("!foo", "null", Ok(("!foo", "str null"))),
            ("!foo", ".inf", Ok(("!foo", "str .inf"))),
            ("!foo", ".nan", Ok(("!foo", "str .nan"))),
            ("!foo", "x", Ok(("!foo", "str x"))),
            ("!foo", "2015-01-01", Ok(("!foo", "str 2015-01-01"))),
            (
                "!foo",
                "18446744073709551615",
                Ok(("!foo", "str 18446744073709551615")),
            ),
            ("!foo", "<<", Ok(("!foo", "str <<"))),
            ("!foo", "1e400", Ok(("!foo", "str 1e400"))),
            ("!!foo", "", Ok(("!!foo", "str "))),
            ("!!foo", "1", Ok(("!!foo", "str 1"))),
            ("!!foo", "-1", Ok(("!!foo", "str -1"))),
            ("!!foo", "1.5", Ok(("!!foo", "str 1.5"))),
            ("!!foo", "0x10", Ok(("!!foo", "str 0x10"))),
            ("!!foo", "true", Ok(("!!foo", "str true"))),
            ("!!foo", "~", Ok(("!!foo", "str ~"))),
            ("!!foo", "null", Ok(("!!foo", "str null"))),
            ("!!foo", ".inf", Ok(("!!foo", "str .inf"))),
            ("!!foo", ".nan", Ok(("!!foo", "str .nan"))),
            ("!!foo", "x", Ok(("!!foo", "str x"))),
            ("!!foo", "2015-01-01", Ok(("!!foo", "str 2015-01-01"))),
            (
                "!!foo",
                "18446744073709551615",
                Ok(("!!foo", "str 18446744073709551615")),
            ),
            ("!!foo", "<<", Ok(("!!foo", "str <<"))),
            ("!!foo", "1e400", Ok(("!!foo", "str 1e400"))),
            ("tag:yaml.org,2002:foo", "", Ok(("!!foo", "str "))),
            ("tag:yaml.org,2002:foo", "1", Ok(("!!foo", "str 1"))),
            ("tag:yaml.org,2002:foo", "-1", Ok(("!!foo", "str -1"))),
            ("tag:yaml.org,2002:foo", "1.5", Ok(("!!foo", "str 1.5"))),
            ("tag:yaml.org,2002:foo", "0x10", Ok(("!!foo", "str 0x10"))),
            ("tag:yaml.org,2002:foo", "true", Ok(("!!foo", "str true"))),
            ("tag:yaml.org,2002:foo", "~", Ok(("!!foo", "str ~"))),
            ("tag:yaml.org,2002:foo", "null", Ok(("!!foo", "str null"))),
            ("tag:yaml.org,2002:foo", ".inf", Ok(("!!foo", "str .inf"))),
            ("tag:yaml.org,2002:foo", ".nan", Ok(("!!foo", "str .nan"))),
            ("tag:yaml.org,2002:foo", "x", Ok(("!!foo", "str x"))),
            (
                "tag:yaml.org,2002:foo",
                "2015-01-01",
                Ok(("!!foo", "str 2015-01-01")),
            ),
            (
                "tag:yaml.org,2002:foo",
                "18446744073709551615",
                Ok(("!!foo", "str 18446744073709551615")),
            ),
            ("tag:yaml.org,2002:foo", "<<", Ok(("!!foo", "str <<"))),
            ("tag:yaml.org,2002:foo", "1e400", Ok(("!!foo", "str 1e400"))),
            ("!!float", "1.1", Ok(("!!float", "float 3ff199999999999a"))),
            ("!!float", "0", Ok(("!!float", "float 0000000000000000"))),
            ("!!float", "-1", Ok(("!!float", "float bff0000000000000"))),
            ("!!null", "", Ok(("!!null", "null"))),
            ("tag:yaml.org,2002:int", "1", Ok(("!!int", "int 1"))),
            ("!", "test", Ok(("!", "str test"))),
            ("!!str", "2015-01-01", Ok(("!!str", "str 2015-01-01"))),
            (
                "!!timestamp",
                "2015-01-01",
                Ok(("!!timestamp", "ts 2015-01-01T00:00:00Z")),
            ),
            (
                "!!float",
                "error",
                Err("yaml: cannot decode !!str `error` as a !!float"),
            ),
            ("!!foo", "BAR!", Ok(("!!foo", "str BAR!"))),
            ("!!str", "123", Ok(("!!str", "str 123"))),
            ("!!binary", "gIGC", Ok(("!!binary", "str gIGC"))),
        ]);
    }

    #[test]
    fn resolve_failure_is_failf_syntax() {
        let err = resolve("!!int", "x").unwrap_err();
        assert!(
            matches!(&err, Error::Syntax(m) if m == "yaml: cannot decode !!str `x` as a !!int")
        );
        // The long form of the requested tag is reported short, as Go's shortTag(tag).
        let err = resolve("tag:yaml.org,2002:bool", "1").unwrap_err();
        assert_eq!(err.to_string(), "yaml: cannot decode !!int `1` as a !!bool");
    }

    #[test]
    fn short_and_long_tags() {
        for stag in KNOWN_TAGS {
            let ltag = long_tag(stag);
            assert_eq!(ltag, format!("tag:yaml.org,2002:{}", &stag[2..]));
            assert_eq!(short_tag(&ltag), stag);
        }
        assert_eq!(short_tag("tag:yaml.org,2002:foo"), "!!foo");
        assert_eq!(long_tag("!!foo"), "tag:yaml.org,2002:foo");
        assert_eq!(short_tag("tag:yaml.org,2002:"), "!!");
        assert_eq!(long_tag("!!"), "tag:yaml.org,2002:");
        for other in ["", "!", "!foo", "tag:example.com,2000:x", "!!str"] {
            assert_eq!(short_tag(other), other);
        }
        for other in ["", "!", "!foo", "tag:yaml.org,2002:str"] {
            assert_eq!(long_tag(other), other);
        }
    }

    #[test]
    fn resolvable_tags() {
        for tag in [
            "",
            STR_TAG,
            BOOL_TAG,
            INT_TAG,
            FLOAT_TAG,
            NULL_TAG,
            TIMESTAMP_TAG,
        ] {
            assert!(resolvable_tag(tag), "{tag}");
        }
        for tag in [
            SEQ_TAG,
            MAP_TAG,
            BINARY_TAG,
            MERGE_TAG,
            "!",
            "!foo",
            "tag:yaml.org,2002:str",
        ] {
            assert!(!resolvable_tag(tag), "{tag}");
        }
    }

    #[test]
    fn resolve_table_classes() {
        assert_eq!(RESOLVE_TABLE[usize::from(b'+')], b'S');
        assert_eq!(RESOLVE_TABLE[usize::from(b'-')], b'S');
        assert_eq!(RESOLVE_TABLE[usize::from(b'7')], b'D');
        for c in b"yYnNtTfFoO~" {
            assert_eq!(RESOLVE_TABLE[usize::from(*c)], b'M');
        }
        assert_eq!(RESOLVE_TABLE[usize::from(b'.')], b'.');
        let classified = RESOLVE_TABLE.iter().filter(|c| **c != 0).count();
        assert_eq!(classified, 2 + 10 + 11 + 1);
    }

    /// Go's `encodeBase64` of `n` bytes `i*7+3` (printed by resolve.go): one line up to 70
    /// characters, then 70-character lines each ending in a newline (70 exactly already
    /// counts as two lines in Go's arithmetic).
    #[test]
    fn encode_base64_matches_go() {
        let rows: &[(usize, &str)] = &[
            (0, ""),
            (1, "Aw=="),
            (2, "Awo="),
            (3, "AwoR"),
            (
                51,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1ph",
            ),
            (
                52,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1phaA\n==\n",
            ),
            (
                53,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1phaG\n8=\n",
            ),
            (
                54,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1phaG\n92\n",
            ),
            (
                105,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1phaG\n92fYSLkpmgp661vMPK0djf5u30+wIJEBceJSwzOkFIT1ZdZGtyeYCHjpWco6qxuL/GzdTb\n",
            ),
            (
                106,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1phaG\n92fYSLkpmgp661vMPK0djf5u30+wIJEBceJSwzOkFIT1ZdZGtyeYCHjpWco6qxuL/GzdTb\n4g==\n",
            ),
            (
                200,
                "AwoRGB8mLTQ7QklQV15lbHN6gYiPlp2kq7K5wMfO1dzj6vH4/wYNFBsiKTA3PkVMU1phaG\n92fYSLkpmgp661vMPK0djf5u30+wIJEBceJSwzOkFIT1ZdZGtyeYCHjpWco6qxuL/GzdTb\n4unw9/4FDBMaISgvNj1ES1JZYGdudXyDipGYn6attLvCydDX3uXs8/oBCA8WHSQrMjlAR0\n5VXGNqcXh/ho2Um6KpsLe+xczT2uHo7/b9BAsSGSAnLjU8Q0pRWF9mbXQ=\n",
            ),
        ];
        for (n, want) in rows {
            let bytes: Vec<u8> = (0..*n).map(|i| (i * 7 + 3) as u8).collect();
            assert_eq!(encode_base64(&bytes), *want, "{n} bytes");
        }
    }

    /// encode.go `isBase60Float` (printed by Go), incl. encode_test.go's `1:1`.
    #[test]
    fn is_base60_float_matches_go() {
        let rows: &[(&str, bool)] = &[
            ("", false),
            ("1:1", true),
            ("1:20", true),
            ("-1:20", true),
            ("+1:20", true),
            ("190:20:30.15", true),
            ("190:20:30", true),
            ("1:60", false),
            ("1:59", true),
            ("1:5", true),
            ("1:05", true),
            ("1:005", false),
            ("1_0:20", true),
            ("_1:20", false),
            ("1:20.", true),
            ("1:20._5", true),
            ("1:20.5_", true),
            ("1:2a", false),
            ("a:1", false),
            ("1::2", false),
            ("1:", false),
            (":1", false),
            ("1:20:", false),
            ("1.5:20", false),
            ("0:0", true),
            ("9:9:9:9", true),
            ("1:6", true),
            ("1:06", true),
            ("1:69", false),
            ("1:20.5.5", false),
            ("1", false),
        ];
        for (s, want) in rows {
            assert_eq!(is_base60_float(s), *want, "{s:?}");
        }
    }

    /// encode.go `isOldBool` (printed by Go).
    #[test]
    fn is_old_bool_matches_go() {
        let rows: &[(&str, bool)] = &[
            ("y", true),
            ("Y", true),
            ("yes", true),
            ("Yes", true),
            ("YES", true),
            ("on", true),
            ("On", true),
            ("ON", true),
            ("n", true),
            ("N", true),
            ("no", true),
            ("No", true),
            ("NO", true),
            ("off", true),
            ("Off", true),
            ("OFF", true),
            ("yEs", false),
            ("oN", false),
            ("true", false),
            ("", false),
            ("o", false),
            ("nO", false),
        ];
        for (s, want) in rows {
            assert_eq!(is_old_bool(s), *want, "{s:?}");
        }
    }

    #[test]
    fn parse_timestamp_offsets_and_fractions() {
        let t = parse_timestamp("2001-12-14t21:59:43.10-05:00").unwrap();
        assert_eq!(t.offset().local_minus_utc(), -5 * 3600);
        assert_eq!(t.timestamp(), 1_008_385_183);
        assert_eq!(t.timestamp_subsec_nanos(), 100_000_000);
        // No zone: UTC.
        let t = parse_timestamp("2015-02-24 18:19:39").unwrap();
        assert_eq!(t.offset().local_minus_utc(), 0);
        assert_eq!(t.timestamp(), 1_424_801_979);
        // A comma is a fraction separator too; digits past the ninth are dropped.
        let t = parse_timestamp("2015-2-3T3:4:5,1234567899Z").unwrap();
        assert_eq!(t.timestamp_subsec_nanos(), 123_456_789);
        // Runs of spaces match the layout's single space.
        assert!(parse_timestamp("2015-02-24   18:19:39").is_some());
        assert!(parse_timestamp("2015-02-24").is_some());
        for bad in [
            "2015",
            "2015-",
            "15-02-24",
            "2015-02-24T",
            "2015-02-30",
            "2015-02-24 18:19",
        ] {
            assert!(parse_timestamp(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn time_parse_chunks() {
        assert_eq!(getnum(b"7x", false), Some((7, &b"x"[..])));
        assert_eq!(getnum(b"07x", true), Some((7, &b"x"[..])));
        assert_eq!(getnum(b"7x", true), None);
        assert_eq!(getnum(b"x", false), None);
        assert_eq!(skip(b"  a", " "), Some(&b"a"[..]));
        assert_eq!(skip(b"a", " "), None);
        assert_eq!(skip(b"", " "), Some(&b""[..]));
        assert_eq!(skip(b"Tx", "T"), Some(&b"x"[..]));
        assert_eq!(skip(b"tx", "T"), None);
        assert_eq!(parse_nanoseconds(b".5", 2), Some(500_000_000));
        assert_eq!(parse_nanoseconds(b",000000001", 10), Some(1));
        assert_eq!(parse_nanoseconds(b".12345678912", 12), Some(123_456_789));
        assert_eq!(parse_nanoseconds(b"x5", 2), None);
    }

    #[test]
    fn strconv_ports() {
        assert_eq!(parse_int("-0x10", 0), Some(-16));
        assert_eq!(parse_int("0b101", 0), Some(5));
        assert_eq!(parse_int("0o17", 0), Some(15));
        assert_eq!(parse_int("017", 0), Some(15));
        assert_eq!(parse_int("1_000", 0), Some(1000));
        assert_eq!(parse_int("1__000", 0), None);
        assert_eq!(parse_int("1_000", 10), None);
        assert_eq!(parse_int("101", 2), Some(5));
        assert_eq!(parse_int("-9223372036854775808", 0), Some(i64::MIN));
        assert_eq!(parse_int("9223372036854775808", 0), None);
        assert_eq!(parse_int("", 0), None);
        assert_eq!(parse_int("+", 0), None);
        assert_eq!(parse_uint("18446744073709551615", 0), Some(u64::MAX));
        assert_eq!(parse_uint("18446744073709551616", 0), None);
        assert_eq!(parse_uint("0x", 0), None);
        assert_eq!(parse_uint("0", 0), Some(0));
        assert_eq!(parse_uint("7", 37), None);
        assert!(underscore_ok(b"0x_1f"));
        assert!(!underscore_ok(b"_1"));
        assert!(!underscore_ok(b"1_"));
        assert!(underscore_ok(b"-1_0"));
        assert_eq!(parse_float("1.5"), Some(1.5));
        assert_eq!(parse_float(".5e1"), Some(5.0));
        assert_eq!(parse_float("1_0.5"), Some(10.5));
        assert_eq!(parse_float("+Inf"), Some(f64::INFINITY));
        assert_eq!(parse_float("-infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_float("nan").is_some_and(f64::is_nan));
        assert_eq!(parse_float("+nan"), None);
        assert_eq!(parse_float("infx"), None);
        assert_eq!(parse_float("1e400"), None);
        assert_eq!(parse_float("1e-400"), Some(0.0));
        assert_eq!(parse_float("0x1p-2"), None);
        assert_eq!(read_float(b"0x1p-2"), Some((true, 6)));
        assert_eq!(read_float(b"0x1"), None);
        assert_eq!(read_float(b"1.5x"), Some((false, 3)));
        assert_eq!(read_float(b"."), None);
        assert!(yaml_style_float("-1.5e+3"));
        assert!(yaml_style_float(".5"));
        assert!(yaml_style_float("1."));
        assert!(!yaml_style_float("."));
        assert!(!yaml_style_float("1e"));
        assert!(!yaml_style_float("1_0"));
    }
}
