// Copyright 2026 Chris Olson
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

//! The free-form YAML value types as yaml.v3 decodes them ([`GoMap`], [`GoValue`]) and the
//! conversions the configurable factories and the conformance plugins share:
//!
//! - [`go_value_to_json`] / [`go_map_to_json`]: the JSON encoding of the value (as gadk's
//!   JSON-based APIs receive it), with errors for unsupported values;
//! - [`json_to_go_value`] / [`json_map_to_go`]: the JSON decoding into a dynamic value (every
//!   number a `float64`), i.e. what a free-form map holds for a JSON-decoded value;
//! - [`go_sprint`] / [`go_type_name`]: the display text and type name of a yaml-decoded value,
//!   as they appear in ADK's error messages.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, FixedOffset, Timelike};
use gadk_goyaml::GoValue;
use serde_json::{Map, Number, Value};

/// A free-form map as yaml.v3 decodes it: values are [`GoValue`]s (`int`, `float64`, …
/// keep their dynamic types). `BTreeMap` iterates sorted, as the encoders and the display
/// text do.
pub type GoMap = BTreeMap<String, GoValue>;

/// The JSON encoding of `v`, read back as a JSON value: `int`/`uint64` stay integers,
/// `float64` becomes a JSON number (`NaN`/`±Inf` fail), a non-UTF-8 string has each invalid
/// byte replaced by U+FFFD, a timestamp becomes its RFC 3339 text, and a
/// `map[interface{}]interface{}` fails with an unsupported-type error.
pub fn go_value_to_json(v: &GoValue) -> Result<Value, String> {
    Ok(match v {
        GoValue::Null => Value::Null,
        GoValue::Bool(b) => Value::Bool(*b),
        GoValue::Int(i) => Value::Number(Number::from(*i)),
        GoValue::Uint(u) => Value::Number(Number::from(*u)),
        GoValue::Float(f) => match Number::from_f64(*f) {
            Some(n) => Value::Number(n),
            None => {
                return Err(format!(
                    "json: unsupported value: {}",
                    gadk_platform::go_format_float_v(*f)
                ));
            }
        },
        GoValue::String(s) => Value::String(s.clone()),
        GoValue::Bytes(b) => Value::String(go_json_string_of_bytes(b)),
        GoValue::Timestamp(t) => {
            if !(0..=9999).contains(&t.year()) {
                return Err("json: error calling MarshalJSON for type time.Time: \
                            Time.MarshalJSON: year outside of range [0,9999]"
                    .to_owned());
            }
            Value::String(gadk_platform::go_format_rfc3339_nano(t))
        }
        GoValue::Seq(items) => Value::Array(
            items
                .iter()
                .map(go_value_to_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        GoValue::Map(entries) => {
            let mut m = Map::new();
            for (k, v) in entries {
                m.insert(k.clone(), go_value_to_json(v)?);
            }
            Value::Object(m)
        }
        GoValue::MapAny(_) => {
            return Err("json: unsupported type: map[interface {}]interface {}".to_owned());
        }
    })
}

/// [`go_value_to_json`] of every entry of a map.
pub fn go_map_to_json(m: &GoMap) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, v) in m {
        out.insert(k.clone(), go_value_to_json(v)?);
    }
    Ok(out)
}

/// Decodes JSON into a dynamic value: `null` → nil, numbers → `float64`, arrays →
/// `[]interface{}`, objects → `map[string]interface{}` (in the JSON map's key order).
pub fn json_to_go_value(v: &Value) -> GoValue {
    match v {
        Value::Null => GoValue::Null,
        Value::Bool(b) => GoValue::Bool(*b),
        Value::Number(n) => GoValue::Float(n.as_f64().unwrap_or_default()),
        Value::String(s) => GoValue::String(s.clone()),
        Value::Array(items) => GoValue::Seq(items.iter().map(json_to_go_value).collect()),
        Value::Object(m) => GoValue::Map(
            m.iter()
                .map(|(k, v)| (k.clone(), json_to_go_value(v)))
                .collect(),
        ),
    }
}

/// [`json_to_go_value`] of every entry of a JSON object.
pub fn json_map_to_go(m: &Map<String, Value>) -> GoMap {
    m.iter()
        .map(|(k, v)| (k.clone(), json_to_go_value(v)))
        .collect()
}

/// The display text of a yaml-decoded value: `<nil>`, `true`, `42`, floats in the shortest
/// `%g` form that round-trips (64-bit), strings verbatim, `[a b]`, `map[k:v]` with sorted
/// keys, and a timestamp's `2006-01-02 15:04:05.999999999 -0700 MST` form.
pub fn go_sprint(v: &GoValue) -> String {
    match v {
        GoValue::Null => "<nil>".to_owned(),
        GoValue::Bool(b) => b.to_string(),
        GoValue::Int(i) => i.to_string(),
        GoValue::Uint(u) => u.to_string(),
        GoValue::Float(f) => gadk_platform::go_format_float_v(*f),
        GoValue::String(s) => s.clone(),
        GoValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        GoValue::Timestamp(t) => go_time_string(t),
        GoValue::Seq(items) => format!(
            "[{}]",
            items.iter().map(go_sprint).collect::<Vec<_>>().join(" ")
        ),
        GoValue::Map(entries) => {
            let mut sorted: Vec<&(String, GoValue)> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            format!(
                "map[{}]",
                sorted
                    .iter()
                    .map(|(k, v)| format!("{k}:{}", go_sprint(v)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        GoValue::MapAny(entries) => {
            let mut sorted: Vec<&(GoValue, GoValue)> = entries.iter().collect();
            sorted.sort_by(|a, b| go_fmtsort_interface_cmp(&a.0, &b.0));
            format!(
                "map[{}]",
                sorted
                    .iter()
                    .map(|(k, v)| format!("{}:{}", go_sprint(k), go_sprint(v)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    }
}

/// The order of two `interface{}` map keys in the display text: nil first, then by dynamic
/// type, then by value within a type (numbers numerically with NaN first, strings bytewise,
/// `false` before `true`, timestamps by nanoseconds then seconds).
///
/// The type order — `string`, `uint64`, `int`, `float64`, `bool`, timestamp — is the one
/// ADK's loader is observed to produce (and a standalone yaml.v3 program). Keys that
/// cannot occur in a yaml.v3 map (sequences, maps) sort last.
fn go_fmtsort_interface_cmp(a: &GoValue, b: &GoValue) -> Ordering {
    fn type_rank(v: &GoValue) -> u8 {
        match v {
            GoValue::Null => 0,
            GoValue::String(_) | GoValue::Bytes(_) => 1,
            GoValue::Uint(_) => 2,
            GoValue::Int(_) => 3,
            GoValue::Float(_) => 4,
            GoValue::Bool(_) => 5,
            GoValue::Timestamp(_) => 6,
            GoValue::Seq(_) | GoValue::Map(_) | GoValue::MapAny(_) => 7,
        }
    }
    fn str_bytes(v: &GoValue) -> &[u8] {
        match v {
            GoValue::String(s) => s.as_bytes(),
            GoValue::Bytes(b) => b,
            _ => &[],
        }
    }
    let by_type = type_rank(a).cmp(&type_rank(b));
    if by_type != Ordering::Equal {
        return by_type;
    }
    match (a, b) {
        (GoValue::Uint(x), GoValue::Uint(y)) => x.cmp(y),
        (GoValue::Int(x), GoValue::Int(y)) => x.cmp(y),
        // NaN before every other value, -0 equal to 0.
        (GoValue::Float(x), GoValue::Float(y)) => match (x.is_nan(), y.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        },
        (GoValue::Bool(x), GoValue::Bool(y)) => x.cmp(y),
        // `wall` holds the nanoseconds (no monotonic reading), `ext` the seconds.
        // Two timestamp keys at the same instant tie and keep document order (ADK's loader
        // would then order them by time zone).
        (GoValue::Timestamp(x), GoValue::Timestamp(y)) => x
            .timestamp_subsec_nanos()
            .cmp(&y.timestamp_subsec_nanos())
            .then(x.timestamp().cmp(&y.timestamp())),
        _ => str_bytes(a).cmp(str_bytes(b)),
    }
}

/// The type name of a yaml-decoded `interface{}` value (`int`, `string`, `[]interface {}`, …).
pub fn go_type_name(v: &GoValue) -> &'static str {
    match v {
        GoValue::Null => "<nil>",
        GoValue::Bool(_) => "bool",
        GoValue::Int(_) => "int",
        GoValue::Uint(_) => "uint64",
        GoValue::Float(_) => "float64",
        GoValue::String(_) | GoValue::Bytes(_) => "string",
        GoValue::Timestamp(_) => "time.Time",
        GoValue::Seq(_) => "[]interface {}",
        GoValue::Map(_) => "map[string]interface {}",
        GoValue::MapAny(_) => "map[interface {}]interface {}",
    }
}

/// The string encoding of a byte string `b`: each byte that is not part of a valid UTF-8
/// sequence becomes U+FFFD (one replacement per invalid byte).
fn go_json_string_of_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        out.push_str(chunk.valid());
        for _ in chunk.invalid() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
    out
}

/// A parsed yaml.v3 timestamp as `2006-01-02 15:04:05.999999999 -0700 MST`: the zone
/// abbreviation is `UTC` for a zero offset and the numeric offset otherwise (a fixed zone
/// without a name prints its offset as its name).
fn go_time_string(t: &DateTime<FixedOffset>) -> String {
    let mut s = t.format("%Y-%m-%d %H:%M:%S").to_string();
    let nanos = t.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    let offset = t.format("%z").to_string();
    if t.offset().local_minus_utc() == 0 {
        s.push_str(" +0000 UTC");
    } else {
        s.push(' ');
        s.push_str(&offset);
        s.push(' ');
        s.push_str(&offset);
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn go_value_to_json_keeps_go_types() {
        let v = GoValue::Map(vec![
            ("i".into(), GoValue::Int(2)),
            ("f".into(), GoValue::Float(2.5)),
            ("u".into(), GoValue::Uint(u64::MAX)),
            (
                "s".into(),
                GoValue::Seq(vec![GoValue::Null, GoValue::Bool(true)]),
            ),
            ("b".into(), GoValue::Bytes(vec![b'a', 0xff, b'b'])),
        ]);
        assert_eq!(
            go_value_to_json(&v).unwrap(),
            json!({"i": 2, "f": 2.5, "u": u64::MAX, "s": [null, true], "b": "a\u{fffd}b"})
        );
    }

    #[test]
    fn go_value_to_json_rejects_what_go_rejects() {
        assert_eq!(
            go_value_to_json(&GoValue::Float(f64::NAN)).unwrap_err(),
            "json: unsupported value: NaN"
        );
        assert_eq!(
            go_value_to_json(&GoValue::Float(f64::INFINITY)).unwrap_err(),
            "json: unsupported value: +Inf"
        );
        assert_eq!(
            go_value_to_json(&GoValue::MapAny(vec![])).unwrap_err(),
            "json: unsupported type: map[interface {}]interface {}"
        );
    }

    #[test]
    fn go_value_to_json_formats_timestamps_as_rfc3339_nano() {
        let t = DateTime::parse_from_rfc3339("2001-12-14T21:59:43.10-05:00").unwrap();
        assert_eq!(
            go_value_to_json(&GoValue::Timestamp(t)).unwrap(),
            json!("2001-12-14T21:59:43.1-05:00")
        );
    }

    #[test]
    fn json_round_trip_makes_numbers_float64() {
        let m = json_map_to_go(
            json!({"a": 1, "b": [1.5, "x"], "c": {"d": null}})
                .as_object()
                .unwrap(),
        );
        assert_eq!(m["a"], GoValue::Float(1.0));
        assert_eq!(
            m["b"],
            GoValue::Seq(vec![GoValue::Float(1.5), GoValue::String("x".into())])
        );
        assert_eq!(m["c"], GoValue::Map(vec![("d".into(), GoValue::Null)]));
        assert_eq!(
            go_map_to_json(&m).unwrap(),
            *json!({"a": 1.0, "b": [1.5, "x"], "c": {"d": null}})
                .as_object()
                .unwrap()
        );
    }

    /// Go's `fmt.Sprint` of the yaml.v3-decoded mapping (captured from Go's `adkcli` as the
    /// McpToolset `server_params.args[0]` error, at ADK-Go `0990f9e`): nil, then
    /// strings, uint64, int (numeric: 9 before 10), float64 (NaN, -Inf first), bool, time.Time.
    #[test]
    fn go_sprint_orders_interface_keys_like_fmtsort() {
        let v: GoValue = gadk_goyaml::unmarshal(
            b"{10: a, 9: b, x: c, -1: d, 2.5: e, true: f, false: g, ~: h, 18446744073709551615: i, B: j, 1.0e+1: k, .nan: l, 2001-12-14: m, -.inf: n, 2001-12-14T01:02:03Z: o}",
            &gadk_goyaml::Options::default(),
        )
        .unwrap();
        assert_eq!(
            go_sprint(&v),
            "map[<nil>:h B:j x:c 18446744073709551615:i -1:d 9:b 10:a NaN:l -Inf:n 2.5:e 10:k false:g true:f 2001-12-14 00:00:00 +0000 UTC:m 2001-12-14 01:02:03 +0000 UTC:o]"
        );
    }

    #[test]
    fn go_sprint_and_type_name_match_go_fmt() {
        let v = GoValue::Map(vec![
            ("z".into(), GoValue::Int(1)),
            (
                "a".into(),
                GoValue::Seq(vec![GoValue::Float(1e6), GoValue::Null]),
            ),
        ]);
        assert_eq!(go_sprint(&v), "map[a:[1e+06 <nil>] z:1]");
        assert_eq!(go_type_name(&v), "map[string]interface {}");
        assert_eq!(go_type_name(&GoValue::Int(1)), "int");
        assert_eq!(go_type_name(&GoValue::Float(1.0)), "float64");
        assert_eq!(go_type_name(&GoValue::Null), "<nil>");
        assert_eq!(go_type_name(&GoValue::Seq(vec![])), "[]interface {}");
        let t = DateTime::parse_from_rfc3339("2001-12-14T21:59:43.10-05:00").unwrap();
        assert_eq!(
            go_sprint(&GoValue::Timestamp(t)),
            "2001-12-14 21:59:43.1 -0500 -0500"
        );
        let utc = DateTime::parse_from_rfc3339("2002-12-14T00:00:00Z").unwrap();
        assert_eq!(
            go_sprint(&GoValue::Timestamp(utc)),
            "2002-12-14 00:00:00 +0000 UTC"
        );
    }
}
