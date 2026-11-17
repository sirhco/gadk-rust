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

//! Time parsing and formatting shared by the gadk crates: RFC 3339 parsing
//! ([`go_parse_rfc3339`]) and formatting ([`go_format_rfc3339_nano`]), a signed nanosecond
//! duration ([`GoDuration`]), duration-string parsing ([`go_parse_duration`]) and formatting
//! ([`go_format_duration`]).

use std::fmt;

use chrono::{DateTime, NaiveDate, TimeDelta, TimeZone, Timelike, Utc};

/// A duration as signed nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GoDuration(pub i64);

impl GoDuration {
    /// One nanosecond.
    pub const NANOSECOND: GoDuration = GoDuration(1);
    /// One microsecond.
    pub const MICROSECOND: GoDuration = GoDuration(1_000);
    /// One millisecond.
    pub const MILLISECOND: GoDuration = GoDuration(1_000_000);
    /// One second.
    pub const SECOND: GoDuration = GoDuration(1_000_000_000);
    /// One minute.
    pub const MINUTE: GoDuration = GoDuration(60 * 1_000_000_000);
    /// One hour.
    pub const HOUR: GoDuration = GoDuration(3600 * 1_000_000_000);

    /// `s` seconds, saturating at the `i64` range.
    pub const fn from_secs(s: i64) -> GoDuration {
        GoDuration(s.saturating_mul(1_000_000_000))
    }

    /// `None` for a negative duration.
    pub fn to_std(self) -> Option<std::time::Duration> {
        u64::try_from(self.0)
            .ok()
            .map(std::time::Duration::from_nanos)
    }

    /// Saturates at `i64::MAX` nanoseconds.
    pub fn from_std(d: std::time::Duration) -> GoDuration {
        GoDuration(i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
    }
}

/// Displays as [`go_format_duration`] (e.g. `1m30s`).
impl fmt::Display for GoDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&go_format_duration(*self))
    }
}

/// Parses a duration string: a possibly signed sequence of decimal numbers, each with an
/// optional fraction and a unit suffix (`ns`, `us`/`µs`/`μs`, `ms`, `s`, `m`, `h`), such as
/// `"300ms"`, `"-1.5h"` or `"2h45m"`. `Err` is the message, e.g. `time: invalid duration "x"`,
/// `time: missing unit in duration "1"`, `time: unknown unit "d" in duration "1d"`.
pub fn go_parse_duration(s: &str) -> Result<GoDuration, String> {
    parse_duration_bytes(s.as_bytes())
}

const LIMIT: u64 = 1 << 63;

/// The error message for a malformed duration: `time: {message} "<quoted input>"`.
fn duration_error(message: &str, orig: &[u8]) -> String {
    format!("time: {message} {}", time_quote(orig))
}

/// Parses a duration over bytes (which need not be UTF-8).
fn parse_duration_bytes(orig: &[u8]) -> Result<GoDuration, String> {
    let invalid = || duration_error("invalid duration", orig);
    let mut s = orig;
    let mut neg = false;
    if let Some(&c) = s.first()
        && (c == b'-' || c == b'+')
    {
        neg = c == b'-';
        s = &s[1..];
    }
    if s == b"0" {
        return Ok(GoDuration(0));
    }
    if s.is_empty() {
        return Err(invalid());
    }
    let mut d: u64 = 0;
    while let Some(&first) = s.first() {
        if !(first == b'.' || first.is_ascii_digit()) {
            return Err(invalid());
        }
        // Consume `[0-9]*`.
        let before = s.len();
        let mut v: u64 = 0;
        while let Some(&c) = s.first() {
            if !c.is_ascii_digit() {
                break;
            }
            if v > LIMIT / 10 {
                return Err(invalid());
            }
            v = v * 10 + u64::from(c - b'0');
            if v > LIMIT {
                return Err(invalid());
            }
            s = &s[1..];
        }
        let pre = before != s.len();
        // Consume `(\.[0-9]*)?`.
        let mut f: u64 = 0;
        let mut scale: f64 = 1.0;
        let mut post = false;
        if s.first() == Some(&b'.') {
            s = &s[1..];
            let before = s.len();
            let mut overflow = false;
            while let Some(&c) = s.first() {
                if !c.is_ascii_digit() {
                    break;
                }
                s = &s[1..];
                if overflow {
                    continue;
                }
                if f > (LIMIT - 1) / 10 {
                    overflow = true;
                    continue;
                }
                let y = f * 10 + u64::from(c - b'0');
                if y > LIMIT {
                    overflow = true;
                    continue;
                }
                f = y;
                scale *= 10.0;
            }
            post = before != s.len();
        }
        if !pre && !post {
            // No digits (e.g. ".s" or "-.s").
            return Err(invalid());
        }
        // Consume the unit.
        let i = s
            .iter()
            .position(|&c| c == b'.' || c.is_ascii_digit())
            .unwrap_or(s.len());
        if i == 0 {
            return Err(duration_error("missing unit in duration", orig));
        }
        let (u, rest) = s.split_at(i);
        s = rest;
        let unit: u64 = match u {
            b"ns" => 1,
            // "us", "µs" (U+00B5 micro sign), "μs" (U+03BC Greek mu).
            b"us" | b"\xc2\xb5s" | b"\xce\xbcs" => 1_000,
            b"ms" => 1_000_000,
            b"s" => 1_000_000_000,
            b"m" => 60 * 1_000_000_000,
            b"h" => 3600 * 1_000_000_000,
            _ => {
                let message = format!("unknown unit {} in duration", time_quote(u));
                return Err(duration_error(&message, orig));
            }
        };
        if v > LIMIT / unit {
            return Err(invalid());
        }
        v *= unit;
        if f > 0 {
            // `v += (f as f64 * (unit / scale)) as u64`, a truncating conversion;
            // the addend is at most 3.6e12 (ns per hour), so neither cast loses range.
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let frac = (f as f64 * (unit as f64 / scale)) as u64;
            v += frac;
            if v > LIMIT {
                return Err(invalid());
            }
        }
        // The u64 addition wraps: both terms are at most 2^63, so only 2^63 + 2^63 wraps,
        // to 0, which then passes the check (`"9223372036854775808ns"` parses twice as 0).
        d = d.wrapping_add(v);
        if d > LIMIT {
            return Err(invalid());
        }
    }
    if neg {
        // Negation wraps 2^63 to `i64::MIN`.
        return Ok(GoDuration(0i64.wrapping_sub_unsigned(d)));
    }
    i64::try_from(d).map(GoDuration).map_err(|_| invalid())
}

/// Quotes `s` in double quotes, `"` and `\` backslash-escaped, and every byte of a control
/// character or a non-ASCII character written as `\xNN` (an invalid byte is one character of
/// width 1; an encoded U+FFFD is three bytes).
fn time_quote(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut i = 0;
    while i < s.len() {
        let (c, width) = decode_rune(&s[i..]);
        if !(' '..'\u{80}').contains(&c) {
            for b in &s[i..i + width] {
                out.push_str(&format!("\\x{b:02x}"));
            }
        } else {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        i += width;
    }
    out.push('"');
    out
}

/// The first character of `s` and its byte width; an invalid byte is U+FFFD of width 1. `s`
/// must not be empty.
fn decode_rune(s: &[u8]) -> (char, usize) {
    let valid = s
        .utf8_chunks()
        .next()
        .map(|chunk| chunk.valid())
        .unwrap_or_default();
    match valid.chars().next() {
        Some(c) => (c, c.len_utf8()),
        None => (char::REPLACEMENT_CHARACTER, 1),
    }
}

/// Formats a duration: `"0s"`; under one second the largest of ns, µs and ms that keeps an
/// integer part (`"1.5µs"`, `"500ms"`); from one second up `[Nh][Nm]S[.fff]s` with hours and
/// minutes written once a larger unit is present (`"1m0s"`, `"1h0m0s"`); a leading `-` for a
/// negative duration.
pub fn go_format_duration(d: GoDuration) -> String {
    let mut u = d.0.unsigned_abs();
    let mut out = String::new();
    if u < 1_000_000_000 {
        let (unit, prec) = match u {
            0 => return "0s".to_string(),
            1..1_000 => ("ns", 0),
            1_000..1_000_000 => ("µs", 3),
            _ => ("ms", 6),
        };
        let frac = fmt_frac(&mut u, prec);
        out.push_str(&format!("{u}{frac}{unit}"));
    } else {
        let frac = fmt_frac(&mut u, 9);
        let secs = u % 60;
        u /= 60;
        if u > 0 {
            let mins = u % 60;
            u /= 60;
            if u > 0 {
                out.push_str(&format!("{u}h"));
            }
            out.push_str(&format!("{mins}m"));
        }
        out.push_str(&format!("{secs}{frac}s"));
    }
    if d.0 < 0 {
        out.insert(0, '-');
    }
    out
}

/// Divides `v` by `10^prec` and returns the fraction as `.ddd` with trailing zeros dropped
/// (empty, without the point, when the fraction is zero).
fn fmt_frac(v: &mut u64, prec: u32) -> String {
    let p = 10u64.pow(prec);
    let frac = *v % p;
    *v /= p;
    if frac == 0 {
        return String::new();
    }
    let digits = format!("{frac:0width$}", width = prec as usize);
    format!(".{}", digits.trim_end_matches('0'))
}

/// Parses an RFC 3339 timestamp (`YYYY-MM-DDThh:mm:ss`, optional fraction, then `Z` or an
/// offset), reduced to the instant.
/// The accepted forms are:
/// - exactly four year digits and two month and day digits, with the day checked against the
///   month (leap years included);
/// - a one- or two-digit hour (0-23), and two-digit minutes and seconds (0-59, so a leap
///   second fails);
/// - an optional fraction after '.' or ',' (digits past the ninth are dropped);
/// - 'Z', or ±hh:mm with hh ≤ 24 and mm ≤ 60. 'T' and 'Z' are case-sensitive, and nothing may
///   follow the zone.
pub fn go_parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    let s = s.as_bytes();
    let digits = |at: usize, n: usize| -> Option<u32> {
        s.get(at..at + n)?.iter().try_fold(0u32, |acc, &b| {
            b.is_ascii_digit().then(|| acc * 10 + u32::from(b - b'0'))
        })
    };
    let literal = |at: usize, want: u8| (s.get(at) == Some(&want)).then_some(());

    let year = digits(0, 4)?;
    literal(4, b'-')?;
    let month = digits(5, 2)?;
    literal(7, b'-')?;
    let day = digits(8, 2)?;
    literal(10, b'T')?;
    let hour_len = if s.get(12).is_some_and(u8::is_ascii_digit) {
        2
    } else {
        1
    };
    let hour = digits(11, hour_len)?;
    let mut i = 11 + hour_len;
    literal(i, b':')?;
    let min = digits(i + 1, 2)?;
    literal(i + 3, b':')?;
    let sec = digits(i + 4, 2)?;
    i += 6;

    let mut nanos = 0u32;
    if matches!(s.get(i), Some(b'.' | b',')) && s.get(i + 1).is_some_and(u8::is_ascii_digit) {
        let start = i + 1;
        let mut end = start;
        while s.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let used = &s[start..end.min(start + 9)];
        nanos = used
            .iter()
            .fold(0, |acc, &b| acc * 10 + u32::from(b - b'0'));
        for _ in used.len()..9 {
            nanos *= 10;
        }
        i = end;
    }

    let offset = match s.get(i) {
        Some(b'Z') => {
            i += 1;
            0
        }
        Some(&sign @ (b'+' | b'-')) => {
            literal(i + 3, b':')?;
            let hh = digits(i + 1, 2)?;
            let mm = digits(i + 4, 2)?;
            if hh > 24 || mm > 60 {
                return None;
            }
            i += 6;
            let off = i64::from(hh * 3600 + mm * 60);
            if sign == b'-' { -off } else { off }
        }
        _ => return None,
    };
    if i != s.len() || !(1..=12).contains(&month) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?;
    let at = date.and_hms_nano_opt(hour, min, sec, nanos)?.and_utc();
    at.checked_sub_signed(TimeDelta::try_seconds(offset)?)
}

/// Formats `t` as RFC 3339 with up to nanosecond precision, in its own
/// zone: the fraction's trailing zeros trimmed (the fraction dropped when zero), `Z` for a zero
/// offset, `±hh:mm` otherwise. Callers that marshal to JSON must check for themselves that the
/// year is within 0..=9999.
pub fn go_format_rfc3339_nano<Tz: TimeZone>(t: &DateTime<Tz>) -> String {
    let t = t.fixed_offset();
    let mut s = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let nanos = t.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    if t.offset().local_minus_utc() == 0 {
        s.push('Z');
    } else {
        s.push_str(&t.format("%:z").to_string());
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    /// The rows of Go `TestParseDurationErrors` (time_test.go) whose input is not valid UTF-8,
    /// which `go_parse_duration(&str)` cannot receive: the byte-level parser and Go's
    /// `time.quote` give Go's full error text.
    #[test]
    fn parse_duration_errors_on_non_utf8_input() {
        let cases: &[(&[u8], &str)] = &[
            (b"\x85\x85", r#"time: invalid duration "\x85\x85""#),
            (b"\xffff", r#"time: invalid duration "\xffff""#),
            (
                b"hello \xffff world",
                r#"time: invalid duration "hello \xffff world""#,
            ),
            (b"1\xff", r#"time: unknown unit "\xff" in duration "1\xff""#),
        ];
        for (input, want) in cases {
            assert_eq!(parse_duration_bytes(input).unwrap_err(), *want, "{input:?}");
        }
    }

    /// Go `time.RFC3339Nano`: trimmed fraction, `Z` for UTC, `±hh:mm` otherwise.
    #[test]
    fn go_format_rfc3339_nano_matches_go() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let cst = FixedOffset::west_opt(6 * 3600).unwrap();
        let at = |o: FixedOffset, ns: u32| {
            o.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
                .unwrap()
                .with_nanosecond(ns)
                .unwrap()
        };
        assert_eq!(go_format_rfc3339_nano(&at(utc, 0)), "2026-01-02T03:04:05Z");
        assert_eq!(
            go_format_rfc3339_nano(&at(cst, 256_916_000)),
            "2026-01-02T03:04:05.256916-06:00"
        );
        assert_eq!(
            go_format_rfc3339_nano(&at(utc, 1)),
            "2026-01-02T03:04:05.000000001Z"
        );
        let t = Utc.with_ymd_and_hms(2026, 7, 17, 10, 0, 0).unwrap();
        assert_eq!(
            go_format_rfc3339_nano(&(t + TimeDelta::microseconds(1500))),
            "2026-07-17T10:00:00.0015Z"
        );
    }

    /// Every row's expectation is what Go's `time.Parse(time.RFC3339, input)` returned
    /// (go1.27, module go 1.26), as a UTC instant; `None` is a parse error (the zero time).
    #[test]
    fn go_parse_rfc3339_matches_go_time_parse() {
        let cases: &[(&str, Option<&str>)] = &[
            ("2999-01-01T00:00:00Z", Some("2999-01-01T00:00:00Z")),
            ("2999-01-01t00:00:00Z", None),
            ("2999-01-01T00:00:00z", None),
            ("2999-01-01 00:00:00Z", None),
            ("2016-12-31T23:59:60Z", None),
            (
                "2999-01-01T00:00:00.123+05:30",
                Some("2998-12-31T18:30:00.123Z"),
            ),
            ("2999-01-01T00:00:00,5Z", Some("2999-01-01T00:00:00.5Z")),
            ("2999-01-01T24:00:00Z", None),
            ("2999-01-01T00:00:00+24:00", Some("2998-12-31T00:00:00Z")),
            ("2999-02-29T00:00:00Z", None),
            ("2000-02-29T00:00:00Z", Some("2000-02-29T00:00:00Z")),
            (
                "2999-01-01T00:00:00.123456789123Z",
                Some("2999-01-01T00:00:00.123456789Z"),
            ),
            ("2999-01-01T00:00:00+05:60", Some("2998-12-31T18:00:00Z")),
            ("2999-01-01T00:00:00+25:00", None),
            ("2999-01-01T00:00:00.Z", None),
            ("2999-01-01T00:00:00", None),
            ("0000-01-01T00:00:00Z", Some("0000-01-01T00:00:00Z")),
            ("2999-01-01T00:00:00-01:30", Some("2999-01-01T01:30:00Z")),
            ("2999-1-01T00:00:00Z", None),
            ("2999-01-01T0:00:00Z", Some("2999-01-01T00:00:00Z")),
            ("2999-01-01T00:00:00+0530", None),
            ("2999-01-01T00:00:00 Z", None),
            ("2999-01-01T00:00:00Z ", None),
            ("+2999-01-01T00:00:00Z", None),
            ("2999-01-01T00:00:00.5", None),
            (
                "2999-01-01T00:00:00.000000001Z",
                Some("2999-01-01T00:00:00.000000001Z"),
            ),
            ("", None),
            ("not-a-time", None),
        ];
        for (input, want) in cases {
            let want = want.map(|w| DateTime::parse_from_rfc3339(w).unwrap().with_timezone(&Utc));
            assert_eq!(go_parse_rfc3339(input), want, "go_parse_rfc3339({input:?})");
        }
    }
}
