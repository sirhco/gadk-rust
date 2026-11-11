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

/// Go `time.ParseDuration` (`gadk_platform::go_parse_duration`) as a [`TimeDelta`]: a signed
/// sequence of decimal numbers with optional fraction and a unit suffix (`ns`, `us`/`µs`/`μs`,
/// `ms`, `s`, `m`, `h`), e.g. `"300ms"`, `"-1.5h"`, `"2h45m"`. `Err` is Go's error text.
pub fn parse_go_duration(orig: &str) -> Result<TimeDelta, String> {
    gadk_platform::go_parse_duration(orig).map(|d| TimeDelta::nanoseconds(d.0))
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
