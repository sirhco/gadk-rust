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

//! `GoDuration`, `go_parse_duration` and `go_format_duration` against Go's `time` package:
//! ports of `TestDurationString`, `TestParseDuration`, `TestParseDurationErrors` and
//! `TestParseDurationRoundTrip` (`src/time/time_test.go`), plus full error texts from Go 1.27.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_platform::{GoDuration, go_format_duration, go_parse_duration};

const NS: i64 = 1;
const US: i64 = 1_000;
const MS: i64 = 1_000_000;
const SEC: i64 = 1_000_000_000;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;

/// Go `durationTests` / `TestDurationString`, including the negated rows.
#[test]
fn test_duration_string() {
    let cases: &[(&str, i64)] = &[
        ("0s", 0),
        ("1ns", NS),
        ("1.1µs", 1100 * NS),
        ("2.2ms", 2200 * US),
        ("3.3s", 3300 * MS),
        ("4m5s", 4 * MIN + 5 * SEC),
        ("4m5.001s", 4 * MIN + 5001 * MS),
        ("5h6m7.001s", 5 * HOUR + 6 * MIN + 7001 * MS),
        ("8m0.000000001s", 8 * MIN + NS),
        ("2562047h47m16.854775807s", i64::MAX),
        ("-2562047h47m16.854775808s", i64::MIN),
    ];
    for (want, d) in cases {
        assert_eq!(go_format_duration(GoDuration(*d)), *want, "Duration({d})");
        if *d > 0 {
            assert_eq!(
                go_format_duration(GoDuration(-d)),
                format!("-{want}"),
                "Duration({})",
                -d
            );
        }
    }
}

/// The values the launcher and gadk-auth print (`(default 15s)`, `1h0m0s`, …); includes the
/// rows of gadk-auth's former `go_duration_matches_go`.
#[test]
fn duration_string_of_common_values() {
    let cases: &[(i64, &str)] = &[
        (999, "999ns"),
        (1_000, "1µs"),
        (1_500, "1.5µs"),
        (1_234_567, "1.234567ms"),
        (10 * MS, "10ms"),
        (100 * MS, "100ms"),
        (500 * MS, "500ms"),
        (1_500 * MS, "1.5s"),
        (2 * SEC, "2s"),
        (15 * SEC, "15s"),
        (30 * SEC, "30s"),
        (60 * SEC, "1m0s"),
        (90 * SEC, "1m30s"),
        (120 * SEC, "2m0s"),
        (HOUR, "1h0m0s"),
        (7384 * SEC + 5 * MS, "2h3m4.005s"),
        (SEC + 1, "1.000000001s"),
        (-1, "-1ns"),
    ];
    for (d, want) in cases {
        assert_eq!(go_format_duration(GoDuration(*d)), *want);
        assert_eq!(GoDuration(*d).to_string(), *want, "Display");
    }
}

/// Go `parseDurationTests` / `TestParseDuration`.
#[test]
fn test_parse_duration() {
    let cases: &[(&str, i64)] = &[
        // simple
        ("0", 0),
        ("5s", 5 * SEC),
        ("30s", 30 * SEC),
        ("1478s", 1478 * SEC),
        // sign
        ("-5s", -5 * SEC),
        ("+5s", 5 * SEC),
        ("-0", 0),
        ("+0", 0),
        // decimal
        ("5.0s", 5 * SEC),
        ("5.6s", 5 * SEC + 600 * MS),
        ("5.s", 5 * SEC),
        (".5s", 500 * MS),
        ("1.0s", SEC),
        ("1.00s", SEC),
        ("1.004s", SEC + 4 * MS),
        ("1.0040s", SEC + 4 * MS),
        ("100.00100s", 100 * SEC + MS),
        // different units
        ("10ns", 10 * NS),
        ("11us", 11 * US),
        ("12µs", 12 * US), // U+00B5
        ("12μs", 12 * US), // U+03BC
        ("13ms", 13 * MS),
        ("14s", 14 * SEC),
        ("15m", 15 * MIN),
        ("16h", 16 * HOUR),
        // composite durations
        ("3h30m", 3 * HOUR + 30 * MIN),
        ("10.5s4m", 4 * MIN + 10 * SEC + 500 * MS),
        ("-2m3.4s", -(2 * MIN + 3 * SEC + 400 * MS)),
        (
            "1h2m3s4ms5us6ns",
            HOUR + 2 * MIN + 3 * SEC + 4 * MS + 5 * US + 6 * NS,
        ),
        ("39h9m14.425s", 39 * HOUR + 9 * MIN + 14 * SEC + 425 * MS),
        // large value
        ("52763797000ns", 52763797000 * NS),
        // more than 9 digits after decimal point, see https://golang.org/issue/6617
        ("0.3333333333333333333h", 20 * MIN),
        // 9007199254740993 = 1<<53+1 cannot be stored precisely in a float64
        ("9007199254740993ns", (1 << 53) + 1),
        // largest duration that can be represented by int64 in nanoseconds
        ("9223372036854775807ns", i64::MAX),
        ("9223372036854775.807us", i64::MAX),
        ("9223372036s854ms775us807ns", i64::MAX),
        ("-9223372036854775808ns", i64::MIN),
        ("-9223372036854775.808us", i64::MIN),
        ("-9223372036s854ms775us808ns", i64::MIN),
        // largest negative value
        ("-9223372036854775808ns", i64::MIN),
        // largest negative round trip value, see https://golang.org/issue/48629
        ("-2562047h47m16.854775808s", i64::MIN),
        // huge string; issue 15011.
        ("0.100000000000000000000h", 6 * MIN),
        // This value tests the first overflow check in leadingFraction.
        (
            "0.830103483285477580700h",
            49 * MIN + 48 * SEC + 372539827 * NS,
        ),
    ];
    for (input, want) in cases {
        assert_eq!(
            go_parse_duration(input),
            Ok(GoDuration(*want)),
            "ParseDuration({input:?})"
        );
    }
}

/// Go `parseDurationErrorTests` / `TestParseDurationErrors` (the error contains the quoted
/// input). The rows whose input is not valid UTF-8 (`"\x85\x85"`, `"\xffff"`,
/// `"hello \xffff world"`) cannot be a `&str`; they are unit tests of the byte-level parser in
/// `src/go_time.rs`.
#[test]
fn test_parse_duration_errors() {
    let cases: &[(&str, &str)] = &[
        // invalid
        ("", r#""""#),
        ("3", r#""3""#),
        ("-", r#""-""#),
        ("s", r#""s""#),
        (".", r#"".""#),
        ("-.", r#""-.""#),
        (".s", r#"".s""#),
        ("+.s", r#""+.s""#),
        ("1d", r#""1d""#),
        ("\u{FFFD}", r#""\xef\xbf\xbd""#),
        (
            "\u{FFFD} hello \u{FFFD} world",
            r#""\xef\xbf\xbd hello \xef\xbf\xbd world""#,
        ),
        // overflow
        ("9223372036854775810ns", r#""9223372036854775810ns""#),
        ("9223372036854775808ns", r#""9223372036854775808ns""#),
        ("-9223372036854775809ns", r#""-9223372036854775809ns""#),
        ("9223372036854776us", r#""9223372036854776us""#),
        ("3000000h", r#""3000000h""#),
        ("9223372036854775.808us", r#""9223372036854775.808us""#),
        (
            "9223372036854ms775us808ns",
            r#""9223372036854ms775us808ns""#,
        ),
    ];
    for (input, expect) in cases {
        let err = go_parse_duration(input).unwrap_err();
        assert!(
            err.contains(expect),
            "ParseDuration({input:?}) = {err:?}, error does not contain {expect:?}"
        );
    }
}

/// Go `ParseDuration`'s full error texts (`parseDurationError.Error()` with `time.quote`).
#[test]
fn parse_duration_error_texts_match_go() {
    let cases: &[(&str, &str)] = &[
        ("", r#"time: invalid duration """#),
        ("x", r#"time: invalid duration "x""#),
        ("3", r#"time: missing unit in duration "3""#),
        ("1.5", r#"time: missing unit in duration "1.5""#),
        ("1d", r#"time: unknown unit "d" in duration "1d""#),
        (
            "1s2days",
            r#"time: unknown unit "days" in duration "1s2days""#,
        ),
        ("5 s", r#"time: unknown unit " s" in duration "5 s""#),
        (
            "1é",
            r#"time: unknown unit "\xc3\xa9" in duration "1\xc3\xa9""#,
        ),
        ("1\"\\", r#"time: unknown unit "\"\\" in duration "1\"\\""#),
        ("1\n", r#"time: unknown unit "\x0a" in duration "1\x0a""#),
        ("3000000h", r#"time: invalid duration "3000000h""#),
    ];
    for (input, want) in cases {
        assert_eq!(go_parse_duration(input), Err(want.to_string()), "{input:?}");
    }
}

/// Go `TestParseDurationRoundTrip` (the random rows become a fixed sweep of millisecond
/// multiples).
#[test]
fn test_parse_duration_round_trip() {
    for d0 in [GoDuration(i64::MAX), GoDuration(i64::MIN)] {
        let s = go_format_duration(d0);
        assert_eq!(go_parse_duration(&s), Ok(d0), "round-trip {d0:?} => {s:?}");
    }
    let mut x: i64 = 1;
    for _ in 0..100 {
        x = (x * 48271) % 2_147_483_647; // Park–Miller, stands in for rand.Int31()
        let d0 = GoDuration(x * MS);
        let s = go_format_duration(d0);
        assert_eq!(go_parse_duration(&s), Ok(d0), "round-trip {d0:?} => {s:?}");
    }
}

/// The `GoDuration` conversions of Appendix C.2.
#[test]
fn go_duration_conversions() {
    assert_eq!(GoDuration::from_secs(15), GoDuration(15 * SEC));
    assert_eq!(GoDuration::from_secs(i64::MAX), GoDuration(i64::MAX));
    assert_eq!(GoDuration::from_secs(i64::MIN), GoDuration(i64::MIN));
    assert_eq!(
        GoDuration(1_500 * MS).to_std(),
        Some(std::time::Duration::from_millis(1_500))
    );
    assert_eq!(GoDuration(0).to_std(), Some(std::time::Duration::ZERO));
    assert_eq!(GoDuration(-1).to_std(), None);
    assert_eq!(
        GoDuration::from_std(std::time::Duration::from_secs(2)),
        GoDuration(2 * SEC)
    );
    assert_eq!(
        GoDuration::from_std(std::time::Duration::MAX),
        GoDuration(i64::MAX)
    );
    assert_eq!(GoDuration::default(), GoDuration(0));
    assert_eq!(
        (GoDuration::MINUTE, GoDuration::HOUR),
        (GoDuration(MIN), GoDuration(HOUR))
    );
}
