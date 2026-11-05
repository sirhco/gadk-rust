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

//! Serde helpers that reproduce Go encoding/json `omitempty` and `[]byte` rules,
//! plus Go `time.Duration` string parsing/formatting used by genai's custom marshalers.
use base64::engine::general_purpose::STANDARD;
use chrono::TimeDelta;

/// Go value-type `omitempty`: true when `v` equals the type's zero value.
pub fn is_zero<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// `[]byte` ⇄ standard base64 string. Use with `#[serde(default, skip_serializing_if = "bytes::Bytes::is_empty", with = "crate::serde_util::base64_bytes")]`.
pub mod base64_bytes {
    use super::STANDARD;
    use base64::Engine as _;
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &Bytes, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(b))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Bytes, D::Error> {
        let s = Option::<String>::deserialize(d)?.unwrap_or_default();
        STANDARD
            .decode(s.as_bytes())
            .map(Bytes::from)
            .map_err(serde::de::Error::custom)
    }
}

/// `Option<Bytes>` variant; `None` is skipped by `skip_serializing_if = "Option::is_none"`.
pub mod base64_bytes_opt {
    use super::STANDARD;
    use base64::Engine as _;
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &Option<Bytes>, s: S) -> Result<S::Ok, S::Error> {
        match b {
            Some(b) => s.serialize_str(&STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Bytes>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(s) => STANDARD
                .decode(s.as_bytes())
                .map(|v| Some(Bytes::from(v)))
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

const NS: u64 = 1;
const US: u64 = 1_000;
const MS: u64 = 1_000_000;
const SEC: u64 = 1_000_000_000;
const MIN: u64 = 60 * SEC;
const HOUR: u64 = 60 * MIN;
const LIMIT: u64 = 1 << 63;

fn duration_err(orig: &str, what: &str) -> String {
    format!("time: {what} duration {orig:?}")
}

/// Port of Go `time.ParseDuration`: a signed sequence of decimal numbers with
/// optional fraction and a unit suffix (`ns`, `us`/`µs`/`μs`, `ms`, `s`, `m`, `h`),
/// e.g. `"300ms"`, `"-1.5h"`, `"2h45m"`. Errors on malformed input or int64 overflow.
pub fn parse_go_duration(orig: &str) -> Result<TimeDelta, String> {
    let mut s = orig.as_bytes();
    let mut neg = false;
    if let Some(&c) = s.first()
        && (c == b'-' || c == b'+')
    {
        neg = c == b'-';
        s = &s[1..];
    }
    if s == b"0" {
        return Ok(TimeDelta::zero());
    }
    if s.is_empty() {
        return Err(duration_err(orig, "invalid"));
    }
    let mut d: u64 = 0;
    while !s.is_empty() {
        if !(s[0] == b'.' || s[0].is_ascii_digit()) {
            return Err(duration_err(orig, "invalid"));
        }
        // Integer part.
        let before = s.len();
        let mut v: u64 = 0;
        while let Some(&c) = s.first() {
            if !c.is_ascii_digit() {
                break;
            }
            if v > LIMIT / 10 {
                return Err(duration_err(orig, "invalid"));
            }
            v = v * 10 + u64::from(c - b'0');
            if v > LIMIT {
                return Err(duration_err(orig, "invalid"));
            }
            s = &s[1..];
        }
        let pre = before != s.len();
        // Fraction part.
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
            return Err(duration_err(orig, "invalid"));
        }
        // Unit.
        let i = s
            .iter()
            .position(|&c| c == b'.' || c.is_ascii_digit())
            .unwrap_or(s.len());
        if i == 0 {
            return Err(duration_err(orig, "missing unit in"));
        }
        let unit = match &s[..i] {
            b"ns" => NS,
            // "us", "µs" (U+00B5 micro sign), "μs" (U+03BC Greek mu).
            b"us" | b"\xc2\xb5s" | b"\xce\xbcs" => US,
            b"ms" => MS,
            b"s" => SEC,
            b"m" => MIN,
            b"h" => HOUR,
            _ => return Err(duration_err(orig, "unknown unit in")),
        };
        s = &s[i..];
        if v > LIMIT / unit {
            return Err(duration_err(orig, "invalid"));
        }
        v *= unit;
        if f > 0 {
            // Go: v += uint64(float64(f) * (float64(unit) / scale)); truncating cast.
            let frac = (f as f64 * (unit as f64 / scale)) as u64;
            v = v.saturating_add(frac);
            if v > LIMIT {
                return Err(duration_err(orig, "invalid"));
            }
        }
        d = d.saturating_add(v);
        if d > LIMIT {
            return Err(duration_err(orig, "invalid"));
        }
    }
    let nanos = if neg {
        if d == LIMIT {
            i64::MIN
        } else {
            // d < 2^63 here, so the cast is lossless.
            -i64::try_from(d).map_err(|_| duration_err(orig, "invalid"))?
        }
    } else {
        i64::try_from(d).map_err(|_| duration_err(orig, "invalid"))?
    };
    Ok(TimeDelta::nanoseconds(nanos))
}

/// Go `fmt.Sprintf("%.0fs", d.Seconds())`: whole seconds, ties rounded to even.
pub fn format_go_seconds(d: TimeDelta) -> String {
    let secs = d.num_seconds() as f64 + f64::from(d.subsec_nanos()) / 1e9;
    format!("{secs:.0}s")
}

/// Decodes an optional Go duration string; `None` and `""` are the zero duration.
pub(crate) fn decode_duration<E: serde::de::Error>(s: Option<String>) -> Result<TimeDelta, E> {
    match s.as_deref() {
        None | Some("") => Ok(TimeDelta::zero()),
        Some(s) => parse_go_duration(s).map_err(E::custom),
    }
}

/// Go `time.Duration` field behind genai's `InternalDurationJSON` custom marshalers
/// (e.g. `LiveServerGoAway.TimeLeft`): decoded with [`parse_go_duration`] (`""` and
/// `null` are zero), encoded as whole seconds with [`format_go_seconds`]. Use with
/// `#[serde(default, skip_serializing_if = "TimeDelta::is_zero", with = "crate::serde_util::go_duration")]`
/// so the zero duration is omitted, as genai does.
pub mod go_duration {
    use chrono::TimeDelta;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &TimeDelta, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::format_go_seconds(*d))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TimeDelta, D::Error> {
        super::decode_duration(Option::<String>::deserialize(d)?)
    }
}

/// A JSON string or number holding an `int64` (decode side of Go's `,string` option).
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum I64OrString {
    Int(i64),
    Str(String),
}

impl I64OrString {
    fn into_i64<E: serde::de::Error>(self) -> Result<i64, E> {
        match self {
            Self::Int(v) => Ok(v),
            Self::Str(s) => s.parse().map_err(E::custom),
        }
    }
}

/// Go `int64` with the `,string` JSON option: encoded as a quoted decimal string.
/// Decoding also accepts a bare JSON number. Use with
/// `#[serde(default, skip_serializing_if = "is_zero", with = "crate::serde_util::i64_string")]`.
pub mod i64_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        match Option::<super::I64OrString>::deserialize(d)? {
            Some(v) => v.into_i64(),
            None => Ok(0),
        }
    }
}

/// Go `*int64` with the `,string` JSON option; `None` is skipped by
/// `skip_serializing_if = "Option::is_none"`.
pub mod i64_string_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_str(&v.to_string()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
        Option::<super::I64OrString>::deserialize(d)?
            .map(super::I64OrString::into_i64)
            .transpose()
    }
}
