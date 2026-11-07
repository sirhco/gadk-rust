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

//! Go standard-library formatting the auth error texts reproduce byte for byte: `time.Duration`'s
//! `String()` and `hex.EncodeToString`. Go's `%q` is `gadk_agent::go_quote`.

use std::time::Duration;

/// Go `time.Duration.String()`: "0s"; under 1s the largest of ns/µs/ms that keeps an integer
/// part, with fractional digits trimmed ("500ms", "1.5µs"); from 1s up "[Nh][Nm]S[.fff]s" with
/// h and m written once a larger unit is present ("30s", "1.5s", "1m30s", "1h0m0s").
/// Values used in 2b: 30s, 10s, 5s, 1h0m0s, 500ms, 100ms, 50ms, 20ms, 10ms.
pub(crate) fn go_duration(d: Duration) -> String {
    let u = d.as_nanos();
    if u == 0 {
        return "0s".to_string();
    }
    if u < 1_000_000_000 {
        let (unit, prec) = if u < 1_000 {
            ("ns", 0)
        } else if u < 1_000_000 {
            ("µs", 3)
        } else {
            ("ms", 6)
        };
        let (int, frac) = split_frac(u, prec);
        return format!("{int}{frac}{unit}");
    }
    let (secs, frac) = split_frac(u, 9);
    let s = secs % 60;
    let mins = secs / 60;
    if mins == 0 {
        return format!("{s}{frac}s");
    }
    let m = mins % 60;
    let h = mins / 60;
    if h == 0 {
        format!("{m}m{s}{frac}s")
    } else {
        format!("{h}h{m}m{s}{frac}s")
    }
}

/// Go `fmtFrac`: `v / 10^prec` and the fraction as ".ddd" with trailing zeros dropped (empty, no
/// point, when the fraction is zero).
fn split_frac(v: u128, prec: u32) -> (u128, String) {
    let p = 10u128.pow(prec);
    let (int, frac) = (v / p, v % p);
    if frac == 0 {
        return (int, String::new());
    }
    let digits = format!("{frac:0width$}", width = prec as usize);
    (int, format!(".{}", digits.trim_end_matches('0')))
}

/// Lowercase hex of `bytes` (Go `hex.EncodeToString`), replacing the `hex` crate.
#[allow(dead_code)] // first used by Tasks 8 and 9 (gcp cache slot and nonce)
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn go_duration_matches_go() {
        // Expected values are Go's `time.Duration.String()` output for the same durations.
        let cases = [
            (Duration::ZERO, "0s"),
            (Duration::from_nanos(1), "1ns"),
            (Duration::from_nanos(999), "999ns"),
            (Duration::from_nanos(1_500), "1.5µs"),
            (Duration::from_micros(1), "1µs"),
            (Duration::from_millis(10), "10ms"),
            (Duration::from_millis(20), "20ms"),
            (Duration::from_millis(50), "50ms"),
            (Duration::from_millis(100), "100ms"),
            (Duration::from_millis(500), "500ms"),
            (Duration::from_nanos(1_234_567), "1.234567ms"),
            (Duration::from_secs(1), "1s"),
            (Duration::from_millis(1_500), "1.5s"),
            (Duration::from_secs(5), "5s"),
            (Duration::from_secs(10), "10s"),
            (Duration::from_secs(30), "30s"),
            (Duration::from_secs(60), "1m0s"),
            (Duration::from_secs(90), "1m30s"),
            (Duration::from_secs(3600), "1h0m0s"),
            (Duration::new(7384, 5_000_000), "2h3m4.005s"),
            (Duration::new(1, 1), "1.000000001s"),
        ];
        for (d, want) in cases {
            assert_eq!(go_duration(d), want, "{d:?}");
        }
    }

    #[test]
    fn hex_encode_matches_go() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
