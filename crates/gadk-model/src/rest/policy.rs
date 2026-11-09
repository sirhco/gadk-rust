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

//! The SDKs' retry policy (ports of anthropic-sdk-go v1.81.0 and openai-go v3.82.0
//! `internal/requestconfig`: `shouldRetry`, `retryHeader`, `shouldRetryDroppedBody`,
//! `parseRetryAfterHeader`, `retryDelay`).

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use http::{HeaderMap, StatusCode};

/// Both SDKs' default retry count (`MaxRetries` 2: up to 3 attempts).
pub const DEFAULT_MAX_RETRIES: u32 = 2;

/// Both SDKs' backoff cap (anthropic-sdk-go `maxDelay`, openai-go `DefaultMaxServerDelay`).
pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(8);

/// openai-go's limit on a server-directed wait (`defaultMaxRetryAfterDelay`).
pub const DEFAULT_MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

/// The first backoff (SDK `0.5 * time.Second`).
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// How a server-directed wait (`Retry-After-Ms`, `Retry-After`) is honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfterRule {
    /// anthropic-sdk-go: any parsed wait is honoured (a negative or NaN one is zero, `+Inf`
    /// saturates); an unparsable value falls through to the next header, then to backoff.
    Unbounded,
    /// openai-go: a wait above the limit, or a numeric overflow (`1e400`), stops retrying and
    /// the response is returned; `NaN`, `±Inf` and negative numbers are skipped (the next
    /// header is tried); a past RFC 1123 date waits zero. `StopAbove(Duration::ZERO)` means
    /// [`DEFAULT_MAX_RETRY_AFTER`], as Go treats `maxDelay <= 0`.
    StopAbove(Duration),
}

/// The server-directed decision for one retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfter {
    /// Wait this long, then retry.
    Wait(Duration),
    /// Do not retry: the response that asked for the wait ends the call.
    Stop,
}

/// A retry policy: how many retries, the backoff cap, the `Retry-After` rule, and whether a
/// body that fails to read is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt.
    pub max_retries: u32,
    /// The backoff cap: retry `n` waits `min(0.5s·2ⁿ, max_backoff)` minus a jitter in
    /// `[0, delay/4)`.
    pub max_backoff: Duration,
    /// How `Retry-After-Ms` / `Retry-After` are honoured.
    pub retry_after: RetryAfterRule,
    /// Whether a body that fails while being read (a non-streaming reply, or a final error
    /// status) is retried per [`should_retry_dropped_body`] (anthropic-sdk-go) or returned as
    /// it is (openai-go reads bodies only after its loop).
    pub retry_dropped_body: bool,
}

impl RetryPolicy {
    /// anthropic-sdk-go v1.81.0: 2 retries, 8 s backoff cap, any server wait honoured, a
    /// dropped body retried.
    pub const ANTHROPIC: Self = Self {
        max_retries: DEFAULT_MAX_RETRIES,
        max_backoff: DEFAULT_MAX_BACKOFF,
        retry_after: RetryAfterRule::Unbounded,
        retry_dropped_body: true,
    };

    /// openai-go v3.82.0: 2 retries, 8 s backoff cap, server waits up to 2 minutes, a dropped
    /// body never retried.
    pub const OPENAI: Self = Self {
        max_retries: DEFAULT_MAX_RETRIES,
        max_backoff: DEFAULT_MAX_BACKOFF,
        retry_after: RetryAfterRule::StopAbove(DEFAULT_MAX_RETRY_AFTER),
        retry_dropped_body: false,
    };

    /// SDK `retryDelay` before retry number `retry_count` (0-based): the server's decision
    /// when [`parse_retry_after`] finds one (`headers` is `None` after a connection error or a
    /// timed-out attempt), else `min(0.5s·2^retry_count, max_backoff)` minus a uniform jitter
    /// in `[0, delay/4)` (`rand`).
    pub fn retry_delay(&self, headers: Option<&HeaderMap>, retry_count: u32) -> RetryAfter {
        if let Some(decision) =
            headers.and_then(|h| parse_retry_after(h, Utc::now(), self.retry_after))
        {
            return decision;
        }
        let delay = INITIAL_BACKOFF
            .checked_mul(2u32.checked_pow(retry_count).unwrap_or(u32::MAX))
            .map_or(self.max_backoff, |d| d.min(self.max_backoff));
        let quarter = u64::try_from(delay.as_nanos() / 4).unwrap_or(u64::MAX);
        if quarter == 0 {
            return RetryAfter::Wait(delay);
        }
        RetryAfter::Wait(delay - Duration::from_nanos(rand::random_range(0..quarter)))
    }
}

/// Go `Header.Get`: the first value of `name`, `""` when absent (or not visible ASCII, which
/// none of the values compared or reported here can match anyway).
pub fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// SDK `retryHeader`: `Some(true)` / `Some(false)` for `x-should-retry: true` / `false`, else
/// `None`.
pub fn retry_header(headers: &HeaderMap) -> Option<bool> {
    match header_str(headers, "x-should-retry") {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// SDK `shouldRetry` for a received response: the `x-should-retry` header wins, else 408, 409,
/// 429 and every status ≥ 500. (A connection error, with no response, is always retried: the
/// request body is a `Bytes`, which the SDKs' `GetBody` rule always allows to resend.)
pub fn should_retry(status: StatusCode, headers: &HeaderMap) -> bool {
    if let Some(retry) = retry_header(headers) {
        return retry;
    }
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::CONFLICT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.as_u16() >= 500
}

/// anthropic-sdk-go `shouldRetryDroppedBody`: a body that could not be read is retried like a
/// connection error unless the response said `x-should-retry: false`.
pub fn should_retry_dropped_body(headers: &HeaderMap) -> bool {
    retry_header(headers) != Some(false)
}

/// Why [`go_parse_float`] rejected its input (Go `strconv.NumError.Err`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GoParseFloatError {
    /// `strconv.ErrSyntax`.
    Syntax,
    /// `strconv.ErrRange`: the value overflows `f64`; `value` is `±Inf`, as Go returns it.
    Range {
        /// `f64::INFINITY` or `f64::NEG_INFINITY`.
        value: f64,
    },
}

/// Go `strconv.ParseFloat(s, 64)` for the decimal forms a header carries: an out-of-range
/// value (`1e400`) is [`GoParseFloatError::Range`] with `±Inf`, as Go reports `ErrRange`;
/// `inf`/`infinity`/`nan` (any case, optional sign) are accepted as Go accepts them. (Go's
/// hexadecimal floats and underscores are not.)
pub fn go_parse_float(s: &str) -> Result<f64, GoParseFloatError> {
    let v: f64 = s.parse().map_err(|_| GoParseFloatError::Syntax)?;
    if v.is_infinite() {
        let word = s.trim_start_matches(['+', '-']).to_ascii_lowercase();
        if word != "inf" && word != "infinity" {
            return Err(GoParseFloatError::Range { value: v });
        }
    }
    Ok(v)
}

/// Go `time.Duration(nanos)` followed by anthropic-sdk-go `retryDelay`'s `max(0, d)`: NaN and
/// negative waits are zero; a wait past `i64::MAX` nanoseconds saturates there.
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

/// Close to, not identical to, Go `time.Parse(time.RFC1123, v)` (`Mon, 02 Jan 2006 15:04:05
/// MST`): the weekday is checked for syntax only, and a zone abbreviation (3 to 5 letters) is
/// read as UTC, as Go does for `GMT`/`UTC` and for any abbreviation the local zone does not
/// define. Known differences: case rules (Go matches the weekday case-insensitively and wants
/// an upper-case zone), the zone shapes Go accepts (`GMT+3`, `ChST`), digit-count rules for the
/// numeric fields (one-digit forms), and abbreviations of the local zone (Go applies that
/// zone's offset; gadk reads UTC).
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
/// (float seconds, or an RFC 1123 date measured from `now`), under `rule`. `None` when neither
/// header gives a decision (the caller backs off).
///
/// - [`RetryAfterRule::Unbounded`] (anthropic-sdk-go): the first header that parses as a float
///   wins (negative or NaN → zero, `+Inf` saturates at `i64::MAX` ns); a `Retry-After` that is
///   not a float is read as a date (a past date → zero).
/// - [`RetryAfterRule::StopAbove`] (openai-go): an overflowing number (`+Inf` with
///   `ErrRange`) → [`RetryAfter::Stop`]; `NaN`, `±Inf` and negative numbers are skipped; a
///   number above the limit → `Stop`, equal to it → the limit; a date at or before `now` → zero,
///   beyond the limit → `Stop`.
pub fn parse_retry_after(
    headers: &HeaderMap,
    now: DateTime<Utc>,
    rule: RetryAfterRule,
) -> Option<RetryAfter> {
    match rule {
        RetryAfterRule::Unbounded => parse_unbounded(headers, now),
        RetryAfterRule::StopAbove(limit) => parse_stop_above(headers, now, limit),
    }
}

fn parse_unbounded(headers: &HeaderMap, now: DateTime<Utc>) -> Option<RetryAfter> {
    let ms = header_str(headers, "retry-after-ms");
    if !ms.is_empty()
        && let Ok(v) = go_parse_float(ms)
    {
        return Some(RetryAfter::Wait(duration_from_nanos(v * 1e6)));
    }
    let secs = header_str(headers, "retry-after");
    if secs.is_empty() {
        return None;
    }
    if let Ok(v) = go_parse_float(secs) {
        return Some(RetryAfter::Wait(duration_from_nanos(v * 1e9)));
    }
    let at = parse_rfc1123(secs)?;
    Some(RetryAfter::Wait(
        (at - now).to_std().unwrap_or(Duration::ZERO),
    ))
}

fn parse_stop_above(
    headers: &HeaderMap,
    now: DateTime<Utc>,
    limit: Duration,
) -> Option<RetryAfter> {
    let limit = if limit.is_zero() {
        DEFAULT_MAX_RETRY_AFTER
    } else {
        limit
    };
    // (header, nanoseconds per unit, whether a date is accepted)
    for (name, unit, date) in [("retry-after-ms", 1e6, false), ("retry-after", 1e9, true)] {
        let v = header_str(headers, name);
        if v.is_empty() {
            continue;
        }
        match go_parse_float(v) {
            Err(GoParseFloatError::Range { value }) if value == f64::INFINITY => {
                return Some(RetryAfter::Stop);
            }
            Ok(n) => {
                if n.is_nan() || n.is_infinite() || n < 0.0 {
                    continue;
                }
                let maximum = limit.as_nanos() as f64 / unit;
                if n > maximum {
                    return Some(RetryAfter::Stop);
                }
                if n == maximum {
                    return Some(RetryAfter::Wait(limit));
                }
                return Some(RetryAfter::Wait(Duration::from_nanos((n * unit) as u64)));
            }
            Err(_) => {}
        }
        if date && let Some(at) = parse_rfc1123(v) {
            let Ok(wait) = (at - now).to_std() else {
                return Some(RetryAfter::Wait(Duration::ZERO));
            };
            if wait > limit {
                return Some(RetryAfter::Stop);
            }
            return Some(RetryAfter::Wait(wait));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use http::HeaderValue;

    /// Header name/value pairs.
    type Pairs = &'static [(&'static str, &'static str)];

    const OPENAI_RULE: RetryAfterRule = RetryAfterRule::StopAbove(DEFAULT_MAX_RETRY_AFTER);

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

    fn wait(d: Duration) -> Option<RetryAfter> {
        Some(RetryAfter::Wait(d))
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
    fn go_parse_float_cases() {
        let cases: [(&str, Result<f64, GoParseFloatError>); 12] = [
            ("2", Ok(2.0)),
            ("1.5", Ok(1.5)),
            ("1e1", Ok(10.0)),
            ("-3", Ok(-3.0)),
            (".5", Ok(0.5)),
            ("inf", Ok(f64::INFINITY)),
            ("+Inf", Ok(f64::INFINITY)),
            ("-Infinity", Ok(f64::NEG_INFINITY)),
            (
                "1e400",
                Err(GoParseFloatError::Range {
                    value: f64::INFINITY,
                }),
            ),
            (
                "-1e400",
                Err(GoParseFloatError::Range {
                    value: f64::NEG_INFINITY,
                }),
            ),
            ("soon", Err(GoParseFloatError::Syntax)),
            ("", Err(GoParseFloatError::Syntax)),
        ];
        for (input, want) in cases {
            assert_eq!(go_parse_float(input), want, "{input}");
        }
        assert!(go_parse_float("NaN").unwrap().is_nan());
        assert!(go_parse_float("-nan").unwrap().is_nan());
    }

    #[test]
    fn anthropic_parse_retry_after_cases() {
        let cases: [(&str, Pairs, Option<RetryAfter>); 17] = [
            ("absent", &[], None),
            (
                "ms_integer",
                &[("retry-after-ms", "1500")],
                wait(Duration::from_millis(1500)),
            ),
            (
                "ms_fraction",
                &[("retry-after-ms", "0.5")],
                wait(Duration::from_micros(500)),
            ),
            (
                "ms_wins_over_seconds",
                &[("retry-after-ms", "10"), ("retry-after", "30")],
                wait(Duration::from_millis(10)),
            ),
            (
                "ms_unparsable_falls_back_to_seconds",
                &[("retry-after-ms", "soon"), ("retry-after", "3")],
                wait(Duration::from_secs(3)),
            ),
            (
                "ms_empty_falls_back_to_seconds",
                &[("retry-after-ms", ""), ("retry-after", "3")],
                wait(Duration::from_secs(3)),
            ),
            (
                "seconds_integer",
                &[("retry-after", "2")],
                wait(Duration::from_secs(2)),
            ),
            (
                "seconds_fraction",
                &[("retry-after", "1.5")],
                wait(Duration::from_millis(1500)),
            ),
            (
                "seconds_exponent",
                &[("retry-after", "1e1")],
                wait(Duration::from_secs(10)),
            ),
            (
                "seconds_negative_is_zero",
                &[("retry-after", "-3")],
                wait(Duration::ZERO),
            ),
            (
                "ms_negative_is_zero",
                &[("retry-after-ms", "-1")],
                wait(Duration::ZERO),
            ),
            (
                "seconds_nan_is_zero",
                &[("retry-after", "NaN")],
                wait(Duration::ZERO),
            ),
            (
                "seconds_out_of_range_is_unparsable",
                &[("retry-after", "1e400")],
                None,
            ),
            (
                "date_in_ten_seconds",
                &[("retry-after", "Sat, 07 Nov 2026 12:00:10 GMT")],
                wait(Duration::from_secs(10)),
            ),
            (
                "date_weekday_is_syntax_only",
                &[("retry-after", "Mon, 07 Nov 2026 12:01:00 GMT")],
                wait(Duration::from_secs(60)),
            ),
            (
                "date_in_the_past_is_zero",
                &[("retry-after", "Sat, 07 Nov 2026 11:59:00 GMT")],
                wait(Duration::ZERO),
            ),
            ("garbage", &[("retry-after", "later")], None),
        ];
        for (name, h, want) in cases {
            assert_eq!(
                parse_retry_after(&headers(h), now(), RetryAfterRule::Unbounded),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn anthropic_parse_retry_after_infinity_saturates() {
        let h = headers(&[("retry-after", "+Inf")]);
        assert_eq!(
            parse_retry_after(&h, now(), RetryAfterRule::Unbounded),
            wait(Duration::from_nanos(i64::MAX.unsigned_abs()))
        );
    }

    #[test]
    fn openai_parse_retry_after_cases() {
        let stop = Some(RetryAfter::Stop);
        let cases: [(&str, Pairs, Option<RetryAfter>); 26] = [
            ("absent", &[], None),
            (
                "ms_integer",
                &[("retry-after-ms", "1500")],
                wait(Duration::from_millis(1500)),
            ),
            (
                "ms_fraction",
                &[("retry-after-ms", "0.5")],
                wait(Duration::from_micros(500)),
            ),
            (
                "ms_wins_over_seconds",
                &[("retry-after-ms", "10"), ("retry-after", "30")],
                wait(Duration::from_millis(10)),
            ),
            (
                "ms_unparsable_falls_back_to_seconds",
                &[("retry-after-ms", "soon"), ("retry-after", "3")],
                wait(Duration::from_secs(3)),
            ),
            (
                "seconds_integer",
                &[("retry-after", "2")],
                wait(Duration::from_secs(2)),
            ),
            (
                "seconds_at_the_limit_waits_the_limit",
                &[("retry-after", "120")],
                wait(Duration::from_secs(120)),
            ),
            (
                "seconds_above_the_limit_stop",
                &[("retry-after", "121")],
                stop,
            ),
            (
                "ms_at_the_limit_waits_the_limit",
                &[("retry-after-ms", "120000")],
                wait(Duration::from_secs(120)),
            ),
            (
                "ms_above_the_limit_stop",
                &[("retry-after-ms", "120001"), ("retry-after", "1")],
                stop,
            ),
            ("seconds_overflow_stops", &[("retry-after", "1e400")], stop),
            (
                "ms_overflow_stops_before_seconds",
                &[("retry-after-ms", "1e400"), ("retry-after", "1")],
                stop,
            ),
            (
                "seconds_negative_overflow_is_unparsable",
                &[("retry-after", "-1e400")],
                None,
            ),
            ("seconds_inf_is_skipped", &[("retry-after", "inf")], None),
            (
                "seconds_plus_inf_is_skipped",
                &[("retry-after", "+Inf")],
                None,
            ),
            ("seconds_nan_is_skipped", &[("retry-after", "NaN")], None),
            (
                "seconds_negative_is_skipped",
                &[("retry-after", "-3")],
                None,
            ),
            (
                "ms_inf_falls_through_to_seconds",
                &[("retry-after-ms", "inf"), ("retry-after", "3")],
                wait(Duration::from_secs(3)),
            ),
            (
                "ms_nan_falls_through_to_seconds",
                &[("retry-after-ms", "NaN"), ("retry-after", "3")],
                wait(Duration::from_secs(3)),
            ),
            (
                "ms_negative_falls_through_to_seconds",
                &[("retry-after-ms", "-1"), ("retry-after", "2")],
                wait(Duration::from_secs(2)),
            ),
            (
                "date_in_ten_seconds",
                &[("retry-after", "Sat, 07 Nov 2026 12:00:10 GMT")],
                wait(Duration::from_secs(10)),
            ),
            (
                "date_in_the_past_is_zero",
                &[("retry-after", "Sat, 07 Nov 2026 11:59:00 GMT")],
                wait(Duration::ZERO),
            ),
            (
                "date_at_the_limit_waits",
                &[("retry-after", "Sat, 07 Nov 2026 12:02:00 GMT")],
                wait(Duration::from_secs(120)),
            ),
            (
                "date_beyond_the_limit_stops",
                &[("retry-after", "Sat, 07 Nov 2026 12:02:01 GMT")],
                stop,
            ),
            (
                "ms_date_is_not_accepted",
                &[("retry-after-ms", "Sat, 07 Nov 2026 12:00:10 GMT")],
                None,
            ),
            ("garbage", &[("retry-after", "later")], None),
        ];
        for (name, h, want) in cases {
            assert_eq!(
                parse_retry_after(&headers(h), now(), OPENAI_RULE),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn openai_zero_limit_is_the_default_limit() {
        let rule = RetryAfterRule::StopAbove(Duration::ZERO);
        let h = headers(&[("retry-after", "120")]);
        assert_eq!(
            parse_retry_after(&h, now(), rule),
            wait(Duration::from_secs(120))
        );
        let h = headers(&[("retry-after", "121")]);
        assert_eq!(parse_retry_after(&h, now(), rule), Some(RetryAfter::Stop));
    }

    #[test]
    fn parse_retry_after_rejects_non_rfc1123_dates() {
        for rule in [RetryAfterRule::Unbounded, OPENAI_RULE] {
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
                assert_eq!(parse_retry_after(&h, now(), rule), None, "{rule:?} {v}");
            }
        }
    }

    #[test]
    fn retry_delay_uses_the_servers_wait() {
        let h = headers(&[("retry-after-ms", "250")]);
        for policy in [RetryPolicy::ANTHROPIC, RetryPolicy::OPENAI] {
            for n in 0..4 {
                assert_eq!(
                    policy.retry_delay(Some(&h), n),
                    RetryAfter::Wait(Duration::from_millis(250)),
                    "retry {n}"
                );
            }
        }
        let zero = headers(&[("retry-after", "0")]);
        assert_eq!(
            RetryPolicy::ANTHROPIC.retry_delay(Some(&zero), 3),
            RetryAfter::Wait(Duration::ZERO)
        );
    }

    #[test]
    fn retry_delay_stops_or_waits_per_rule() {
        let long = headers(&[("retry-after", "121")]);
        assert_eq!(
            RetryPolicy::ANTHROPIC.retry_delay(Some(&long), 0),
            RetryAfter::Wait(Duration::from_secs(121))
        );
        assert_eq!(
            RetryPolicy::OPENAI.retry_delay(Some(&long), 0),
            RetryAfter::Stop
        );
    }

    fn assert_backoff(policy: RetryPolicy, headers: Option<&HeaderMap>) {
        let bases_ms = [500u64, 1000, 2000, 4000, 8000, 8000, 8000];
        for (n, base_ms) in bases_ms.into_iter().enumerate() {
            let base = Duration::from_millis(base_ms);
            let floor = base - base / 4;
            let n = u32::try_from(n).unwrap();
            let mut distinct = std::collections::HashSet::new();
            for _ in 0..200 {
                let RetryAfter::Wait(d) = policy.retry_delay(headers, n) else {
                    panic!("retry {n}: want a wait");
                };
                assert!(
                    d > floor && d <= base,
                    "retry {n}: {d:?} not in ({floor:?}, {base:?}]"
                );
                distinct.insert(d);
            }
            assert!(distinct.len() > 1, "retry {n}: no jitter");
        }
    }

    #[test]
    fn retry_delay_backoff_bounds_with_jitter() {
        let no_wait = headers(&[("x-should-retry", "true")]);
        for policy in [RetryPolicy::ANTHROPIC, RetryPolicy::OPENAI] {
            assert_backoff(policy, None);
            assert_backoff(policy, Some(&no_wait));
        }
    }

    #[test]
    fn openai_skipped_waits_fall_back_to_backoff() {
        for v in ["inf", "+Inf", "NaN", "-5"] {
            let h =
                HeaderMap::from_iter([(http::header::RETRY_AFTER, HeaderValue::from_static(v))]);
            assert_backoff(RetryPolicy::OPENAI, Some(&h));
        }
    }

    #[test]
    fn retry_delay_caps_huge_retry_counts() {
        let RetryAfter::Wait(d) = RetryPolicy::ANTHROPIC.retry_delay(None, u32::MAX) else {
            panic!("want a wait");
        };
        assert!(
            d > Duration::from_secs(6) && d <= DEFAULT_MAX_BACKOFF,
            "{d:?}"
        );
    }

    #[test]
    fn retry_delay_small_backoff_cap_has_no_jitter_panic() {
        let policy = RetryPolicy {
            max_backoff: Duration::ZERO,
            ..RetryPolicy::OPENAI
        };
        assert_eq!(
            policy.retry_delay(None, 0),
            RetryAfter::Wait(Duration::ZERO)
        );
        let policy = RetryPolicy {
            max_backoff: Duration::from_nanos(3),
            ..RetryPolicy::OPENAI
        };
        assert_eq!(
            policy.retry_delay(None, 5),
            RetryAfter::Wait(Duration::from_nanos(3))
        );
    }

    #[test]
    fn policy_constants() {
        assert_eq!(DEFAULT_MAX_RETRIES, 2);
        assert_eq!(
            RetryPolicy::ANTHROPIC,
            RetryPolicy {
                max_retries: 2,
                max_backoff: Duration::from_secs(8),
                retry_after: RetryAfterRule::Unbounded,
                retry_dropped_body: true,
            }
        );
        assert_eq!(
            RetryPolicy::OPENAI,
            RetryPolicy {
                max_retries: 2,
                max_backoff: Duration::from_secs(8),
                retry_after: RetryAfterRule::StopAbove(Duration::from_secs(120)),
                retry_dropped_body: false,
            }
        );
    }
}
