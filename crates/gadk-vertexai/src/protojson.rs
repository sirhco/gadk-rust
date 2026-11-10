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

//! The protobuf JSON (protojson) and URL details of Go's generated REST client: Timestamp and
//! bytes encoding, int64 values, the two aiplatform enums the session events carry, and Go's
//! `url.QueryEscape`, `url.Values.Encode` and `URL.EscapedPath`.

#![allow(dead_code)]

use base64::Engine as _;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::{DecodePaddingMode, general_purpose};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Go `url.QueryEscape` (shared with the other gadk crates).
pub(crate) use gadk_platform::go_query_escape;

/// The first second protojson accepts: `0001-01-01T00:00:00Z`.
const MIN_TIMESTAMP_SECONDS: i64 = -62_135_596_800;
/// The last second protojson accepts: `9999-12-31T23:59:59Z`.
const MAX_TIMESTAMP_SECONDS: i64 = 253_402_300_799;

/// protojson `google.protobuf.Timestamp` encoding: RFC 3339 in UTC with `Z`, and 0, 3, 6 or 9
/// fraction digits (`…:05Z`, `….120Z`, `….123456Z`, `….123456789Z`). Go refuses a time
/// outside years 1–9999; gadk's times are inside it, so this formats whatever it is given.
pub(crate) fn format_timestamp(t: &DateTime<Utc>) -> String {
    let base = t.format("%Y-%m-%dT%H:%M:%S");
    let nanos = t.timestamp_subsec_nanos() % 1_000_000_000;
    if nanos == 0 {
        format!("{base}Z")
    } else if nanos.is_multiple_of(1_000_000) {
        format!("{base}.{:03}Z", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        format!("{base}.{:06}Z", nanos / 1_000)
    } else {
        format!("{base}.{nanos:09}Z")
    }
}

/// protojson `google.protobuf.Timestamp` decoding: Go `time.Parse(time.RFC3339Nano, s)` (any
/// offset; `.` or `,` before the fraction), then protojson's checks — at most 9 fraction
/// digits after a `.`, and an instant within years 1–9999.
///
/// # Errors
///
/// protojson's text, `invalid google.protobuf.Timestamp value "{s}"` or
/// `google.protobuf.Timestamp value out of range: "{s}"`.
pub(crate) fn parse_timestamp(s: &str) -> Result<DateTime<Utc>, String> {
    let quoted = serde_json::to_string(s).unwrap_or_default();
    let invalid = || format!("invalid google.protobuf.Timestamp value {quoted}");
    let t = gadk_platform::go_parse_rfc3339(s).ok_or_else(invalid)?;
    let secs = t.timestamp();
    if !(MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&secs) {
        return Err(format!(
            "google.protobuf.Timestamp value out of range: {quoted}"
        ));
    }
    // protojson: the span from the last '.' to the zone ('Z', '-' or '+') holds at most 9 digits.
    if let (Some(dot), Some(zone)) = (s.rfind('.'), s.rfind(['Z', '-', '+']))
        && zone >= dot
        && zone - dot > ".999999999".len()
    {
        return Err(invalid());
    }
    Ok(t)
}

/// protojson `bytes` encoding: standard base64 with padding.
pub(crate) fn encode_bytes(b: &[u8]) -> String {
    general_purpose::STANDARD.encode(b)
}

/// protojson `bytes` decoding: the URL-safe alphabet when the text holds `-` or `_`, else the
/// standard one; unpadded when the length is not a multiple of 4, else padded. Like Go's
/// decoder it skips `\r` and `\n` and ignores non-zero trailing bits.
///
/// # Errors
///
/// The base64 decoder's message when the text is not valid base64.
pub(crate) fn decode_bytes(s: &str) -> Result<Vec<u8>, String> {
    let alphabet = if s.contains(['-', '_']) {
        &base64::alphabet::URL_SAFE
    } else {
        &base64::alphabet::STANDARD
    };
    let padding = if s.len().is_multiple_of(4) {
        DecodePaddingMode::RequireCanonical
    } else {
        DecodePaddingMode::RequireNone
    };
    let engine = GeneralPurpose::new(
        alphabet,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(padding)
            .with_decode_allow_trailing_bits(true),
    );
    let cleaned: String = s.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
    engine.decode(cleaned).map_err(|e| e.to_string())
}

/// aiplatform `ExecutableCode.Language` (aiplatformpb `ExecutableCode_Language_name`).
const LANGUAGES: [(i64, &str); 2] = [(0, "LANGUAGE_UNSPECIFIED"), (1, "PYTHON")];

/// aiplatform `CodeExecutionResult.Outcome` (aiplatformpb `CodeExecutionResult_Outcome_name`).
const OUTCOMES: [(i64, &str); 4] = [
    (0, "OUTCOME_UNSPECIFIED"),
    (1, "OUTCOME_OK"),
    (2, "OUTCOME_FAILED"),
    (3, "OUTCOME_DEADLINE_EXCEEDED"),
];

fn enum_number(table: &[(i64, &str)], name: &str) -> i64 {
    table
        .iter()
        .find(|(_, n)| *n == name)
        .map_or(0, |(number, _)| *number)
}

/// An enum field's name from its protojson value: a number (Go `Enum.String()`: the name, or
/// the decimal number when it has none), a name (kept as it is), or absent / `null` (the zero
/// value's name).
fn enum_name(table: &[(i64, &str)], v: &Value) -> String {
    let number = match v {
        Value::String(name) => return name.clone(),
        Value::Null => 0,
        other => match int64_value(other) {
            Some(n) => n,
            None => return other.to_string(),
        },
    };
    table
        .iter()
        .find(|(n, _)| *n == number)
        .map_or_else(|| number.to_string(), |(_, name)| (*name).to_owned())
}

/// The protojson number of an `ExecutableCode.Language` name: `LANGUAGE_UNSPECIFIED` 0,
/// `PYTHON` 1; an unknown name is 0.
pub(crate) fn language_number(name: &str) -> i64 {
    enum_number(&LANGUAGES, name)
}

/// The name of an `ExecutableCode.Language` value given as a number or a name; an unknown
/// number is its decimal string (Go `Enum.String()`).
pub(crate) fn language_name(v: &Value) -> String {
    enum_name(&LANGUAGES, v)
}

/// The protojson number of a `CodeExecutionResult.Outcome` name: `OUTCOME_UNSPECIFIED` 0,
/// `OUTCOME_OK` 1, `OUTCOME_FAILED` 2, `OUTCOME_DEADLINE_EXCEEDED` 3; an unknown name is 0.
pub(crate) fn outcome_number(name: &str) -> i64 {
    enum_number(&OUTCOMES, name)
}

/// The name of a `CodeExecutionResult.Outcome` value given as a number or a name; an unknown
/// number is its decimal string (Go `Enum.String()`).
pub(crate) fn outcome_name(v: &Value) -> String {
    enum_name(&OUTCOMES, v)
}

/// Go `url.Values.Encode` of the values `pairs` adds in order: keys sorted bytewise, a key's
/// values in the order added, each `QueryEscape(key)=QueryEscape(value)`, joined by `&`.
pub(crate) fn go_values_encode(pairs: &[(&str, String)]) -> String {
    let mut sorted: Vec<&(&str, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0)); // stable: values keep their order within a key
    sorted
        .iter()
        .map(|(k, v)| format!("{}={}", go_query_escape(k), go_query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Go `(*url.URL).EscapedPath` of a URL whose `RawPath` is empty (`escape(path, encodePath)`):
/// ASCII letters, digits and `-._~$&+,/:;=@` are kept, every other byte becomes `%XX`
/// (upper-case hex, per UTF-8 byte). The path `*` alone is kept as it is (Go issue 11202).
pub(crate) fn go_escaped_path(path: &str) -> String {
    const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";
    if path == "*" {
        return path.to_owned();
    }
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~$&+,/:;=@".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(UPPER_HEX[usize::from(b >> 4)]));
            out.push(char::from(UPPER_HEX[usize::from(b & 15)]));
        }
    }
    out
}

/// An int64 field's protojson value (a JSON string, or a number) as an `i64`, as protojson
/// decodes it: the text must be a JSON number with no surrounding space whose value is a
/// whole number in range (`"3"`, `3`, `"3.0"`, `"1e2"` all decode; `" 3"`, `"3.5"`, `"03"`
/// do not).
pub(crate) fn int64_value(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => go_int_text(s),
        Value::Number(n) => n.as_i64().or_else(|| {
            if n.is_f64() {
                go_int_text(&n.to_string())
            } else {
                None // a u64 above i64::MAX
            }
        }),
        _ => None,
    }
}

/// protojson's integer reading of a JSON number's text (`internal/encoding/json`
/// `parseNumberParts` + `normalizeToIntString`, then `strconv.ParseInt(…, 10, 64)`).
fn go_int_text(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    let neg = b.first() == Some(&b'-');
    if neg {
        i += 1;
    }
    // Integer part: "0" (stored empty) or [1-9][0-9]*.
    let intp: &[u8] = match b.get(i) {
        Some(b'0') => {
            i += 1;
            &[]
        }
        Some(b'1'..=b'9') => {
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            &b[start..i]
        }
        _ => return None,
    };
    // Fraction: '.' and one or more digits, trailing zeros dropped.
    let mut frac: &[u8] = &[];
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        let mut end = start;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            return None;
        }
        frac = &b[start..end];
        while let [rest @ .., b'0'] = frac {
            frac = rest;
        }
        i = end;
    }
    // Exponent: 'e' or 'E', an optional sign and one or more digits.
    let mut exp: i64 = 0;
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let start = i + 1;
        let mut end = start;
        if matches!(b.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        let digits_start = end;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits_start {
            return None;
        }
        exp = std::str::from_utf8(&b[start..end])
            .ok()?
            .parse::<i32>()
            .ok()?
            .into();
        i = end;
    }
    if i != b.len() {
        return None;
    }

    if intp.is_empty() && frac.is_empty() {
        return Some(0);
    }
    let num: Vec<u8> = if exp >= 0 {
        let fill = usize::try_from(exp).ok()?;
        if frac.len() > fill || intp.len() + fill > 20 {
            return None;
        }
        let mut num = [intp, frac].concat();
        num.resize(intp.len() + fill, b'0');
        num
    } else {
        if !frac.is_empty() {
            return None;
        }
        let index = usize::try_from(i64::try_from(intp.len()).ok()? + exp).ok()?;
        if intp[index..].iter().any(|&d| d != b'0') {
            return None;
        }
        intp[..index].to_vec()
    };
    let digits = std::str::from_utf8(&num).ok()?;
    if neg {
        format!("-{digits}").parse().ok()
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn at(nanos: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap()
            + chrono::TimeDelta::nanoseconds(i64::from(nanos))
    }

    /// Expected values are Go's `protojson.Marshal(timestamppb.New(t))` (protobuf v1.36.12).
    #[test]
    fn format_timestamp_matches_protojson() {
        for (nanos, want) in [
            (0, "2025-01-02T03:04:05Z"),
            (120_000_000, "2025-01-02T03:04:05.120Z"),
            (123_000_000, "2025-01-02T03:04:05.123Z"),
            (100_000_000, "2025-01-02T03:04:05.100Z"),
            (123_456_000, "2025-01-02T03:04:05.123456Z"),
            (1_000, "2025-01-02T03:04:05.000001Z"),
            (123_456_789, "2025-01-02T03:04:05.123456789Z"),
            (1, "2025-01-02T03:04:05.000000001Z"),
        ] {
            assert_eq!(format_timestamp(&at(nanos)), want, "nanos={nanos}");
        }
        // Go's zero time (gadk's default event timestamp).
        assert_eq!(
            format_timestamp(&gadk_session::zero_time()),
            "0001-01-01T00:00:00Z"
        );
    }

    /// Expected values are Go's `protojson.Unmarshal` into a `timestamppb.Timestamp`
    /// (protobuf v1.36.12, go1.27): `None` = an error.
    #[test]
    fn parse_timestamp_matches_protojson() {
        let cases: &[(&str, Option<&str>)] = &[
            ("2025-01-02T03:04:05Z", Some("2025-01-02T03:04:05Z")),
            ("2025-01-02T03:04:05.1Z", Some("2025-01-02T03:04:05.1Z")),
            (
                "2025-01-02T03:04:05.123456789Z",
                Some("2025-01-02T03:04:05.123456789Z"),
            ),
            ("2025-01-02T03:04:05.1234567891Z", None),
            ("2025-01-02T03:04:05.12345678912+01:00", None),
            ("2025-01-02T04:04:05+01:00", Some("2025-01-02T03:04:05Z")),
            (
                "2025-01-02T03:04:05.5-02:30",
                Some("2025-01-02T05:34:05.5Z"),
            ),
            ("2025-01-02T03:04:05-00:00", Some("2025-01-02T03:04:05Z")),
            ("2025-01-02T03:04:05,5Z", Some("2025-01-02T03:04:05.5Z")),
            ("2025-01-02T03:04:05+24:00", Some("2025-01-01T03:04:05Z")),
            ("2025-01-02T03:04:05+01:60", Some("2025-01-02T01:04:05Z")),
            ("2025-01-02T03:04:05.000Z", Some("2025-01-02T03:04:05Z")),
            ("2024-02-29T00:00:00Z", Some("2024-02-29T00:00:00Z")),
            ("0001-01-01T00:00:00Z", Some("0001-01-01T00:00:00Z")),
            (
                "9999-12-31T23:59:59.999999999Z",
                Some("9999-12-31T23:59:59.999999999Z"),
            ),
            ("0000-12-31T23:59:59Z", None),
            ("0001-01-01T00:00:00+01:00", None),
            ("9999-12-31T23:59:59-01:00", None),
            ("2025-01-02t03:04:05z", None),
            ("2025-01-02T03:04:05z", None),
            ("2025-01-02 03:04:05Z", None),
            ("2025-01-02T03:04:05", None),
            ("2025-01-02T03:04:05.Z", None),
            ("2025-01-02T24:00:00Z", None),
            ("2025-01-02T23:59:60Z", None),
            ("2025-02-30T03:04:05Z", None),
            ("2025-13-02T03:04:05Z", None),
            ("2025-1-02T03:04:05Z", None),
            ("+2025-01-02T03:04:05Z", None),
            ("2025-01-02T03:04:05Z ", None),
            ("2025-01-02T03:04:05+0100", None),
            ("2025-01-02T03:04:05+01", None),
            ("garbage", None),
        ];
        for (input, want) in cases {
            let got = parse_timestamp(input).ok();
            let want = want.map(|w| DateTime::parse_from_rfc3339(w).unwrap().with_timezone(&Utc));
            assert_eq!(got, want, "parse_timestamp({input:?})");
        }
        assert_eq!(
            parse_timestamp("garbage").unwrap_err(),
            r#"invalid google.protobuf.Timestamp value "garbage""#
        );
        assert_eq!(
            parse_timestamp("0000-12-31T23:59:59Z").unwrap_err(),
            r#"google.protobuf.Timestamp value out of range: "0000-12-31T23:59:59Z""#
        );
        // Round trip.
        for nanos in [0, 120_000_000, 123_456_000, 123_456_789] {
            assert_eq!(
                parse_timestamp(&format_timestamp(&at(nanos))).unwrap(),
                at(nanos)
            );
        }
    }

    /// Expected values are Go's protojson encoding / decoding of an `aiplatformpb.Blob.data`.
    #[test]
    fn bytes_match_protojson() {
        assert_eq!(encode_bytes(&[0xfb, 0xff, 0xfe, b'a']), "+//+YQ==");
        assert_eq!(encode_bytes(b""), "");
        for input in ["+//+YQ==", "-__-YQ==", "+//+YQ", "-__-YQ"] {
            assert_eq!(
                decode_bytes(input).unwrap(),
                vec![0xfb, 0xff, 0xfe, b'a'],
                "{input}"
            );
        }
        for input in ["YQ=", "Y", "@@", "+/_-"] {
            assert!(decode_bytes(input).is_err(), "{input}");
        }
        assert_eq!(decode_bytes("").unwrap(), Vec::<u8>::new());
        // Go skips line breaks (after choosing the padding by the raw length) and accepts
        // non-zero trailing bits.
        assert_eq!(decode_bytes("aGVs\r\nbG8").unwrap(), b"hello");
        assert!(decode_bytes("aGVs\r\nbG8=").is_err());
        assert_eq!(decode_bytes("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_bytes("YR==").unwrap(), b"a");
    }

    /// aiplatformpb v1.127.0 `ExecutableCode_Language` / `CodeExecutionResult_Outcome`.
    #[test]
    fn enum_tables_match_aiplatformpb() {
        assert_eq!(language_number("LANGUAGE_UNSPECIFIED"), 0);
        assert_eq!(language_number("PYTHON"), 1);
        assert_eq!(language_number("COBOL"), 0);
        assert_eq!(language_name(&json!(1)), "PYTHON");
        assert_eq!(language_name(&json!(0)), "LANGUAGE_UNSPECIFIED");
        assert_eq!(language_name(&json!("PYTHON")), "PYTHON");
        assert_eq!(language_name(&Value::Null), "LANGUAGE_UNSPECIFIED");
        // Go `ExecutableCode_Language(7).String()` = "7".
        assert_eq!(language_name(&json!(7)), "7");

        for (name, number) in [
            ("OUTCOME_UNSPECIFIED", 0),
            ("OUTCOME_OK", 1),
            ("OUTCOME_FAILED", 2),
            ("OUTCOME_DEADLINE_EXCEEDED", 3),
        ] {
            assert_eq!(outcome_number(name), number);
            assert_eq!(outcome_name(&json!(number)), name);
            assert_eq!(outcome_name(&json!(name)), name);
        }
        assert_eq!(outcome_number("OUTCOME_MAYBE"), 0);
        assert_eq!(outcome_name(&json!(9)), "9");
    }

    /// Expected values are Go's `url.QueryEscape` (net/url, go1.27).
    #[test]
    fn query_escape_matches_go() {
        for (input, want) in [
            (
                "projects/p/locations/l/reasoningEngines/1/sessions/s 1",
                "projects%2Fp%2Flocations%2Fl%2FreasoningEngines%2F1%2Fsessions%2Fs+1",
            ),
            ("json;enum-encoding=int", "json%3Benum-encoding%3Dint"),
            ("a+b/c~d_e.f-g", "a%2Bb%2Fc~d_e.f-g"),
            ("\u{fc}\u{263a}", "%C3%BC%E2%98%BA"),
        ] {
            assert_eq!(go_query_escape(input), want, "{input}");
        }
    }

    /// Expected values are Go's `url.Values.Encode` after `Add`ing the pairs in order.
    #[test]
    fn values_encode_matches_go() {
        let alt = || ("$alt", "json;enum-encoding=int".to_owned());
        assert_eq!(go_values_encode(&[]), "");
        assert_eq!(
            go_values_encode(&[alt()]),
            "%24alt=json%3Benum-encoding%3Dint"
        );
        assert_eq!(
            go_values_encode(&[("sessionId", "my session".to_owned()), alt()]),
            "%24alt=json%3Benum-encoding%3Dint&sessionId=my+session"
        );
        assert_eq!(
            go_values_encode(&[
                ("pageToken", "tok/+=".to_owned()),
                ("filter", r#"userId="user 1""#.to_owned()),
                ("filter", "b".to_owned()),
                alt(),
                ("Z", String::new()),
            ]),
            "%24alt=json%3Benum-encoding%3Dint&Z=&filter=userId%3D%22user+1%22&filter=b&pageToken=tok%2F%2B%3D"
        );
        assert_eq!(
            go_values_encode(&[
                (
                    "filter",
                    r#"timestamp>="2025-01-02T03:04:05+00:00""#.to_owned()
                ),
                alt(),
            ]),
            "%24alt=json%3Benum-encoding%3Dint&filter=timestamp%3E%3D%222025-01-02T03%3A04%3A05%2B00%3A00%22"
        );
    }

    /// Expected values are Go's `URL.EscapedPath` after `baseUrl.Path += path`.
    #[test]
    fn escaped_path_matches_go() {
        for (input, want) in [
            (
                "/v1beta1/projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696/sessions/s1:appendEvent",
                "/v1beta1/projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696/sessions/s1:appendEvent",
            ),
            (
                "/ui/projects/p/locations/l/operations/123",
                "/ui/projects/p/locations/l/operations/123",
            ),
            (
                "/v1beta1/projects/p/sessions/a b/c?d#e%f",
                "/v1beta1/projects/p/sessions/a%20b/c%3Fd%23e%25f",
            ),
            (
                "/x/\u{fc}\u{263a}/[x]!$&'()*+,;=:@~",
                "/x/%C3%BC%E2%98%BA/%5Bx%5D%21$&%27%28%29%2A+,;=:@~",
            ),
            ("/a\"<>\\^`{|}\t", "/a%22%3C%3E%5C%5E%60%7B%7C%7D%09"),
            ("*", "*"),
        ] {
            assert_eq!(go_escaped_path(input), want, "{input}");
        }
        // The kept ASCII set, byte by byte (Go: escape(string(b), encodePath) == string(b)).
        let kept: String = (0u8..128)
            .filter(|&b| b != b'*')
            .map(char::from)
            .filter(|c| go_escaped_path(&format!("/{c}")) == format!("/{c}"))
            .collect();
        assert_eq!(
            kept,
            "$&+,-./0123456789:;=@ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~"
        );
    }

    /// Expected values are Go's protojson decoding of an int32/int64 field (protobuf v1.36.12).
    #[test]
    fn int64_value_matches_protojson() {
        for (v, want) in [
            (json!("3"), Some(3)),
            (json!(3), Some(3)),
            (json!("3.0"), Some(3)),
            (json!(3.0), Some(3)),
            (json!("1e2"), Some(100)),
            (json!(1e2), Some(100)),
            (json!("-42"), Some(-42)),
            (json!("0"), Some(0)),
            (json!("-0"), Some(0)),
            (json!("0.0e5"), Some(0)),
            (json!("10e-1"), Some(1)),
            (json!("1.5e1"), Some(15)),
            (json!("9223372036854775807"), Some(i64::MAX)),
            (json!("-9223372036854775808"), Some(i64::MIN)),
            (json!(i64::MIN), Some(i64::MIN)),
            (json!("9223372036854775808"), None),
            (json!(9_223_372_036_854_775_808u64), None),
            (json!(" 3"), None),
            (json!("3 "), None),
            (json!("3.5"), None),
            (json!("5e-1"), None),
            (json!("03"), None),
            (json!("+3"), None),
            (json!("3."), None),
            (json!(".5"), None),
            (json!("1e"), None),
            (json!("0x10"), None),
            (json!(""), None),
            (json!(1e300), None),
            (json!(true), None),
            (Value::Null, None),
        ] {
            assert_eq!(int64_value(&v), want, "{v}");
        }
    }
}
