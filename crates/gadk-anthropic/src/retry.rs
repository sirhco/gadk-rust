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

//! The SDK retry policy (port of anthropic-sdk-go v1.81.0 `internal/requestconfig`:
//! `shouldRetry`, `retryHeader`, `shouldRetryDroppedBody`, `parseRetryAfterHeader`,
//! `retryDelay`).

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use http::{HeaderMap, StatusCode};

/// The SDK's default retry count (`MaxRetries` 2: up to 3 attempts).
pub(crate) const DEFAULT_MAX_RETRIES: u32 = 2;

/// The backoff cap (SDK `maxDelay`).
const MAX_BACKOFF: Duration = Duration::from_secs(8);

/// The first backoff (SDK `0.5 * time.Second`).
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Go `Header.Get`: the first value of `name`, `""` when absent (or not visible ASCII, which
/// none of the values compared here can match anyway).
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// SDK `retryHeader`: `Some(true)` / `Some(false)` for `x-should-retry: true` / `false`, else
/// `None`.
pub(crate) fn retry_header(headers: &HeaderMap) -> Option<bool> {
    match header_str(headers, "x-should-retry") {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// SDK `shouldRetry` for a received response: the `x-should-retry` header wins, else 408, 409,
/// 429 and every status ≥ 500. (A connection error, with no response, is always retried: the
/// request body is a `Bytes`, which the SDK's `GetBody` rule always allows to resend.)
pub(crate) fn should_retry(status: StatusCode, headers: &HeaderMap) -> bool {
    if let Some(retry) = retry_header(headers) {
        return retry;
    }
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::CONFLICT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.as_u16() >= 500
}

/// SDK `shouldRetryDroppedBody`: a body that could not be read is retried like a connection
/// error unless the response said `x-should-retry: false`.
pub(crate) fn should_retry_dropped_body(headers: &HeaderMap) -> bool {
    retry_header(headers) != Some(false)
}

/// Go `strconv.ParseFloat(s, 64)` for the decimal forms a header carries: an out-of-range
/// value (`1e400`) is an error, as Go reports `ErrRange`; `inf`/`infinity`/`nan` (any case,
/// optional sign) are accepted as Go accepts them. (Go's hexadecimal floats are not.)
fn go_parse_float(s: &str) -> Option<f64> {
    let v: f64 = s.parse().ok()?;
    if v.is_infinite() {
        let word = s.trim_start_matches(['+', '-']).to_ascii_lowercase();
        if word != "inf" && word != "infinity" {
            return None;
        }
    }
    Some(v)
}

/// Go `time.Duration(nanos)` followed by `retryDelay`'s `max(0, d)`: NaN and negative waits
/// are zero; a wait past `i64::MAX` nanoseconds saturates there.
fn duration_from_nanos(nanos: f64) -> Duration {
    if nanos.is_nan() || nanos <= 0.0 {
        return Duration::ZERO;
    }
    // `i64::MAX as f64` rounds up to 2^63; anything at or above it saturates.
    if nanos >= i64::MAX as f64 {
        return Duration::from_nanos(i64::MAX.unsigned_abs());
    }
    // In range: `as` truncates the fraction, as Go's conversion does.
    Duration::from_nanos(nanos as u64)
}

/// Go `time.Parse(time.RFC1123, v)` (`Mon, 02 Jan 2006 15:04:05 MST`): the weekday is checked
/// for syntax only, and a zone abbreviation (3 to 5 letters) is read as UTC, as Go does for
/// `GMT`/`UTC` and for any abbreviation the local zone does not define.
fn parse_rfc1123(v: &str) -> Option<DateTime<Utc>> {
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let (weekday, rest) = v.split_at_checked(3)?;
    if !WEEKDAYS.contains(&weekday) {
        return None;
    }
    let rest = rest.strip_prefix(", ")?;
    let (stamp, zone) = rest.rsplit_once(' ')?;
    if !(3..=5).contains(&zone.len()) || !zone.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(stamp, "%d %b %Y %H:%M:%S").ok()?;
    Some(naive.and_utc())
}

/// SDK `parseRetryAfterHeader`: `Retry-After-Ms` (float milliseconds), then `Retry-After`
/// (float seconds, or an RFC 1123 date measured from `now`). A negative wait is
/// `Duration::ZERO` (the SDK's `retryDelay` clamps it with `max(0, …)`).
pub(crate) fn parse_retry_after(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    let ms = header_str(headers, "retry-after-ms");
    if !ms.is_empty()
        && let Some(v) = go_parse_float(ms)
    {
        return Some(duration_from_nanos(v * 1e6));
    }
    let secs = header_str(headers, "retry-after");
    if secs.is_empty() {
        return None;
    }
    if let Some(v) = go_parse_float(secs) {
        return Some(duration_from_nanos(v * 1e9));
    }
    let at = parse_rfc1123(secs)?;
    Some((at - now).to_std().unwrap_or(Duration::ZERO))
}

/// SDK `retryDelay` before retry number `retry_count` (0-based): the server's wait when
/// [`parse_retry_after`] finds one (`headers` is `None` after a connection error), else
/// `min(0.5s·2^retry_count, 8s)` minus a uniform jitter in `[0, delay/4)` (`rand`).
pub(crate) fn retry_delay(headers: Option<&HeaderMap>, retry_count: u32) -> Duration {
    if let Some(wait) = headers.and_then(|h| parse_retry_after(h, Utc::now())) {
        return wait;
    }
    let delay = INITIAL_BACKOFF
        .checked_mul(2u32.checked_pow(retry_count).unwrap_or(u32::MAX))
        .map_or(MAX_BACKOFF, |d| d.min(MAX_BACKOFF));
    // delay ≥ 0.5s, so delay/4 is a non-empty range of nanoseconds.
    let quarter = u64::try_from(delay.as_nanos() / 4).unwrap_or(u64::MAX);
    delay - Duration::from_nanos(rand::random_range(0..quarter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use http::HeaderValue;

    /// Header name/value pairs.
    type Pairs = &'static [(&'static str, &'static str)];

    fn headers(pairs: Pairs) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap()
    }

    #[test]
    fn retry_header_cases() {
        let cases: [(&str, Pairs, Option<bool>); 6] = [
            ("true", &[("x-should-retry", "true")], Some(true)),
            ("false", &[("x-should-retry", "false")], Some(false)),
            (
                "capitalised_value_is_unset",
                &[("x-should-retry", "True")],
                None,
            ),
            ("one_is_unset", &[("x-should-retry", "1")], None),
            ("empty_is_unset", &[("x-should-retry", "")], None),
            ("absent", &[], None),
        ];
        for (name, h, want) in cases {
            assert_eq!(retry_header(&headers(h)), want, "{name}");
        }
    }

    #[test]
    fn retry_header_reads_the_first_value() {
        let h = headers(&[("x-should-retry", "false"), ("x-should-retry", "true")]);
        assert_eq!(retry_header(&h), Some(false));
    }

    #[test]
    fn should_retry_statuses_and_header_override() {
        let none: Pairs = &[];
        let yes: Pairs = &[("x-should-retry", "true")];
        let no: Pairs = &[("x-should-retry", "false")];
        let cases: [(&str, u16, Pairs, bool); 16] = [
            ("200", 200, none, false),
            ("201", 201, none, false),
            ("400", 400, none, false),
            ("401", 401, none, false),
            ("403", 403, none, false),
            ("404", 404, none, false),
            ("408_request_timeout", 408, none, true),
            ("409_conflict", 409, none, true),
            ("429_too_many_requests", 429, none, true),
            ("500", 500, none, true),
            ("503", 503, none, true),
            ("529_overloaded", 529, none, true),
            ("500_with_false", 500, no, false),
            ("429_with_false", 429, no, false),
            ("400_with_true", 400, yes, true),
            ("200_with_true", 200, yes, true),
        ];
        for (name, status, h, want) in cases {
            let status = StatusCode::from_u16(status).unwrap();
            assert_eq!(should_retry(status, &headers(h)), want, "{name}");
        }
    }

    #[test]
    fn should_retry_dropped_body_cases() {
        assert!(should_retry_dropped_body(&headers(&[])), "no header");
        assert!(
            should_retry_dropped_body(&headers(&[("x-should-retry", "true")])),
            "true"
        );
        assert!(
            !should_retry_dropped_body(&headers(&[("x-should-retry", "false")])),
            "false"
        );
        assert!(
            should_retry_dropped_body(&headers(&[("x-should-retry", "maybe")])),
            "unrecognised value"
        );
    }

    #[test]
    fn parse_retry_after_cases() {
        let cases: [(&str, Pairs, Option<Duration>); 17] = [
            ("absent", &[], None),
            (
                "ms_integer",
                &[("retry-after-ms", "1500")],
                Some(Duration::from_millis(1500)),
            ),
            (
                "ms_fraction",
                &[("retry-after-ms", "0.5")],
                Some(Duration::from_micros(500)),
            ),
            (
                "ms_wins_over_seconds",
                &[("retry-after-ms", "10"), ("retry-after", "30")],
                Some(Duration::from_millis(10)),
            ),
            (
                "ms_unparsable_falls_back_to_seconds",
                &[("retry-after-ms", "soon"), ("retry-after", "3")],
                Some(Duration::from_secs(3)),
            ),
            (
                "ms_empty_falls_back_to_seconds",
                &[("retry-after-ms", ""), ("retry-after", "3")],
                Some(Duration::from_secs(3)),
            ),
            (
                "seconds_integer",
                &[("retry-after", "2")],
                Some(Duration::from_secs(2)),
            ),
            (
                "seconds_fraction",
                &[("retry-after", "1.5")],
                Some(Duration::from_millis(1500)),
            ),
            (
                "seconds_exponent",
                &[("retry-after", "1e1")],
                Some(Duration::from_secs(10)),
            ),
            (
                "seconds_negative_is_zero",
                &[("retry-after", "-3")],
                Some(Duration::ZERO),
            ),
            (
                "ms_negative_is_zero",
                &[("retry-after-ms", "-1")],
                Some(Duration::ZERO),
            ),
            (
                "seconds_nan_is_zero",
                &[("retry-after", "NaN")],
                Some(Duration::ZERO),
            ),
            (
                "seconds_out_of_range_is_unparsable",
                &[("retry-after", "1e400")],
                None,
            ),
            (
                "date_in_ten_seconds",
                &[("retry-after", "Sat, 07 Nov 2026 12:00:10 GMT")],
                Some(Duration::from_secs(10)),
            ),
            (
                "date_weekday_is_syntax_only",
                &[("retry-after", "Mon, 07 Nov 2026 12:01:00 GMT")],
                Some(Duration::from_secs(60)),
            ),
            (
                "date_in_the_past_is_zero",
                &[("retry-after", "Sat, 07 Nov 2026 11:59:00 GMT")],
                Some(Duration::ZERO),
            ),
            ("garbage", &[("retry-after", "later")], None),
        ];
        for (name, h, want) in cases {
            assert_eq!(parse_retry_after(&headers(h), now()), want, "{name}");
        }
    }

    #[test]
    fn parse_retry_after_rejects_non_rfc1123_dates() {
        for v in [
            "2026-11-07T12:00:10Z",
            "Sat, 07 Nov 2026 12:00:10",
            "Sat, 07 Nov 2026 12:00:10 +0000",
            "Xyz, 07 Nov 2026 12:00:10 GMT",
            "Saturday, 07-Nov-26 12:00:10 GMT",
        ] {
            let h = HeaderMap::from_iter([(
                http::header::RETRY_AFTER,
                HeaderValue::from_str(v).unwrap(),
            )]);
            assert_eq!(parse_retry_after(&h, now()), None, "{v}");
        }
    }

    #[test]
    fn parse_retry_after_infinity_saturates() {
        let h = headers(&[("retry-after", "+Inf")]);
        assert_eq!(
            parse_retry_after(&h, now()),
            Some(Duration::from_nanos(i64::MAX.unsigned_abs()))
        );
    }

    #[test]
    fn retry_delay_uses_the_servers_wait() {
        let h = headers(&[("retry-after-ms", "250")]);
        for n in 0..4 {
            assert_eq!(
                retry_delay(Some(&h), n),
                Duration::from_millis(250),
                "retry {n}"
            );
        }
        let zero = headers(&[("retry-after", "0")]);
        assert_eq!(retry_delay(Some(&zero), 3), Duration::ZERO);
    }

    #[test]
    fn retry_delay_backoff_bounds_with_jitter() {
        let no_wait = headers(&[("x-should-retry", "true")]);
        let bases_ms = [500u64, 1000, 2000, 4000, 8000, 8000, 8000];
        for (n, base_ms) in bases_ms.into_iter().enumerate() {
            let base = Duration::from_millis(base_ms);
            let floor = base - base / 4;
            let n = u32::try_from(n).unwrap();
            let mut distinct = std::collections::HashSet::new();
            for _ in 0..200 {
                for h in [None, Some(&no_wait)] {
                    let d = retry_delay(h, n);
                    assert!(
                        d > floor && d <= base,
                        "retry {n}: {d:?} not in ({floor:?}, {base:?}]"
                    );
                    distinct.insert(d);
                }
            }
            assert!(distinct.len() > 1, "retry {n}: no jitter");
        }
    }

    #[test]
    fn retry_delay_caps_huge_retry_counts() {
        let d = retry_delay(None, u32::MAX);
        assert!(d > Duration::from_secs(6) && d <= MAX_BACKOFF, "{d:?}");
    }

    #[test]
    fn default_max_retries_is_two() {
        assert_eq!(DEFAULT_MAX_RETRIES, 2);
    }
}
