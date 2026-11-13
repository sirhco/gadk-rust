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

//! Go's `map[string]any` / `any` as yaml.v3 decodes them ([`GoMap`], [`GoValue`]) and the
//! conversions the configurable factories and the conformance plugins share:
//!
//! - [`go_value_to_json`] / [`go_map_to_json`]: Go `json.Marshal` of the value (as gadk's
//!   JSON-based APIs receive it), with Go's errors for unsupported values;
//! - [`json_to_go_value`] / [`json_map_to_go`]: Go `json.Unmarshal` into `any` (every number
//!   a `float64`), i.e. what Go's `map[string]any` holds for a JSON-decoded value;
//! - [`go_sprint`] / [`go_type_name`]: Go's `%v` and `%T` of a yaml-decoded value.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, FixedOffset, Timelike};
use gadk_goyaml::GoValue;
use serde_json::{Map, Number, Value};

/// Go `map[string]any` as yaml.v3 decodes it: values are [`GoValue`]s (`int`, `float64`, …
/// keep Go's dynamic types). Go maps are unordered; `BTreeMap` iterates sorted, as Go's
/// encoders and `fmt` do.
pub type GoMap = BTreeMap<String, GoValue>;

/// Go `json.Marshal(v)` read back as a JSON value: `int`/`uint64` stay integers, `float64`
/// becomes a JSON number (`NaN`/`±Inf` fail as in Go), a non-UTF-8 Go string has each invalid
/// byte replaced by U+FFFD (Go's `encoding/json`), a `time.Time` becomes its RFC 3339 text, and
/// a `map[interface{}]interface{}` fails with Go's unsupported-type error.
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

/// [`go_value_to_json`] of every entry of a Go `map[string]any`.
pub fn go_map_to_json(m: &GoMap) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, v) in m {
        out.insert(k.clone(), go_value_to_json(v)?);
    }
    Ok(out)
}

/// Go `json.Unmarshal` into `any`: `null` → nil, numbers → `float64`, arrays →
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

/// [`json_to_go_value`] of every entry of a JSON object (Go `map[string]any` from JSON).
pub fn json_map_to_go(m: &Map<String, Value>) -> GoMap {
    m.iter()
        .map(|(k, v)| (k.clone(), json_to_go_value(v)))
        .collect()
}

/// Go `fmt.Sprint(v)` (`%v`) of a yaml-decoded value: `<nil>`, `true`, `42`, floats as
/// `strconv.FormatFloat(f, 'g', -1, 64)`, strings verbatim, `[a b]`, `map[k:v]` with sorted
/// keys, and `time.Time`'s `String()` form.
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
            let mut sorted: Vec<(String, String)> = entries
                .iter()
                .map(|(k, v)| (go_sprint(k), go_sprint(v)))
                .collect();
            sorted.sort();
            format!(
                "map[{}]",
                sorted
                    .iter()
                    .map(|(k, v)| format!("{k}:{v}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    }
}

/// Go `fmt.Sprintf("%T", v)` of a yaml-decoded `interface{}` value.
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

/// Go `encoding/json`'s string encoding of a Go string holding `b`: each byte that is not part
/// of a valid UTF-8 sequence becomes U+FFFD (Go decodes it as `RuneError` of width 1).
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

/// Go `time.Time.String()` (`2006-01-02 15:04:05.999999999 -0700 MST`) of a parsed yaml.v3
/// timestamp: the zone abbreviation is `UTC` for a zero offset and the numeric offset otherwise
/// (Go's `FixedZone("", off)` prints its offset as its name).
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
