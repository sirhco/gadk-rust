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

//! Go `time` parsing shared by the gadk crates: `time.Parse(time.RFC3339, s)`.

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};

/// Go `time.Parse(time.RFC3339, s)`, reduced to the instant. That is Go's general layout
/// parser for "2006-01-02T15:04:05Z07:00", which is what decides when its RFC 3339 fast path
/// declines:
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
