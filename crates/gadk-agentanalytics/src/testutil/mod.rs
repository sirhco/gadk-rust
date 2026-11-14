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

//! 7b-2 test support (feature `testutil`): the in-process BigQuery Write fake
//! ([`fake_write_server`]) and the probe's *row specs* (`scripts/gen-go-agentanalytics`
//! `SCENARIO.md` § 7b-2 fixture formats → Row specs) as [`Row`]s.

use serde_json::Value;

use crate::row::{Row, RowValue};

pub mod fake_write_server;

/// The [`Row`] of a probe row spec: an object of value specs, or `null` (Go's nil map, an
/// empty row).
///
/// # Errors
///
/// A text naming the first malformed value spec.
pub fn row_from_spec(spec: &Value) -> Result<Row, String> {
    match spec {
        Value::Null => Ok(Row::new()),
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| value_from_spec(v).map(|v| (k.clone(), v)))
            .collect::<Result<Row, String>>()
            .map_err(|e| format!("{e} (row spec {spec})")),
        _ => Err(format!("row spec is neither an object nor null: {spec}")),
    }
}

/// The [`RowValue`] of a probe value spec `{"t": tag, "v": value}`: `str` → `Str`, `ts` →
/// `Timestamp`, `bool` → `Bool`, `int64` / `int` → `Int`, `float64` → `Float`, `null` → `Null`,
/// `parts` → `Parts` (a `null` element is an empty row, a `null` list an empty one), `map` →
/// `Map`; `int32`, `any_list`, `str_map` and `bytes` (Go types no assertion accepts) →
/// `Other("<tag>:<value>")`.
///
/// # Errors
///
/// A text naming the malformed spec.
pub fn value_from_spec(spec: &Value) -> Result<RowValue, String> {
    let bad = || format!("bad value spec {spec}");
    let tag = spec.get("t").and_then(Value::as_str).ok_or_else(bad)?;
    let v = spec.get("v").unwrap_or(&Value::Null);
    Ok(match tag {
        "str" => RowValue::Str(v.as_str().ok_or_else(bad)?.to_owned()),
        "ts" => RowValue::Timestamp(
            v.as_str()
                .and_then(gadk_platform::go_parse_rfc3339)
                .ok_or_else(bad)?,
        ),
        "bool" => RowValue::Bool(v.as_bool().ok_or_else(bad)?),
        "int64" | "int" => RowValue::Int(v.as_i64().ok_or_else(bad)?),
        "float64" => RowValue::Float(v.as_f64().ok_or_else(bad)?),
        "null" => RowValue::Null,
        "parts" => match v {
            Value::Null => RowValue::Parts(Vec::new()),
            Value::Array(items) => {
                RowValue::Parts(items.iter().map(row_from_spec).collect::<Result<_, _>>()?)
            }
            _ => return Err(bad()),
        },
        "map" => match v {
            Value::Object(_) => RowValue::Map(row_from_spec(v)?),
            _ => return Err(bad()),
        },
        "int32" | "any_list" | "str_map" | "bytes" => RowValue::Other(format!("{tag}:{v}")),
        _ => return Err(bad()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use chrono::{TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn row_specs_map_to_row_values() {
        let row = row_from_spec(&json!({
            "s": {"t": "str", "v": "x"},
            "t": {"t": "ts", "v": "2026-03-04T23:30:00-05:00"},
            "b": {"t": "bool", "v": true},
            "i": {"t": "int", "v": 4},
            "j": {"t": "int64", "v": -1},
            "f": {"t": "float64", "v": 2.9},
            "n": {"t": "null"},
            "p": {"t": "parts", "v": [{"text": {"t": "str", "v": "a"}}, null]},
            "q": {"t": "parts", "v": null},
            "m": {"t": "map", "v": {"uri": {"t": "str", "v": "gs://b"}}},
            "o": {"t": "int32", "v": 5},
        }))
        .unwrap();
        assert_eq!(row["s"], RowValue::from("x"));
        assert_eq!(
            row["t"],
            RowValue::Timestamp(Utc.with_ymd_and_hms(2026, 3, 5, 4, 30, 0).unwrap())
        );
        assert_eq!(row["b"], RowValue::Bool(true));
        assert_eq!(row["i"], RowValue::Int(4));
        assert_eq!(row["j"], RowValue::Int(-1));
        assert_eq!(row["f"], RowValue::Float(2.9));
        assert_eq!(row["n"], RowValue::Null);
        assert_eq!(
            row["p"],
            RowValue::Parts(vec![
                [("text".to_owned(), RowValue::from("a"))].into(),
                Row::new()
            ])
        );
        assert_eq!(row["q"], RowValue::Parts(Vec::new()));
        assert_eq!(
            row["m"],
            RowValue::Map([("uri".to_owned(), RowValue::from("gs://b"))].into())
        );
        assert_eq!(row["o"], RowValue::Other("int32:5".into()));
        assert_eq!(row_from_spec(&Value::Null).unwrap(), Row::new());
        assert!(row_from_spec(&json!({"x": {"t": "nope"}})).is_err());
    }
}
