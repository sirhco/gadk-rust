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

//! Decoding of `jobs.query` / `jobs.getQueryResults` rows (`f`/`v` cells) into typed JSON.

use chrono::{DateTime, SecondsFormat};
use serde_json::{Map, Number, Value};

use crate::error::BigQueryError;

/// The largest integer a JSON number carries exactly in an f64 (2^53).
const MAX_SAFE_INT: i64 = 1 << 53;

/// Every row as a column-name → value map, typed by `schema` (see the crate docs for the rules).
pub(crate) fn decode_rows(
    schema: Option<&Value>,
    rows: Option<&Value>,
) -> Result<Vec<Map<String, Value>>, BigQueryError> {
    let (Some(schema), Some(rows)) = (schema, rows) else {
        return Ok(Vec::new());
    };
    let fields = fields_of(schema);
    let Some(rows) = rows.as_array() else {
        return Err(BigQueryError::Decode("rows is not an array".into()));
    };
    rows.iter().map(|row| decode_record(&fields, row)).collect()
}

/// The `fields` array of a schema or RECORD field.
fn fields_of(v: &Value) -> Vec<Value> {
    v.get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// One `{"f": [{"v": …}, …]}` record as an object.
fn decode_record(fields: &[Value], record: &Value) -> Result<Map<String, Value>, BigQueryError> {
    let cells = record
        .get("f")
        .and_then(Value::as_array)
        .ok_or_else(|| BigQueryError::Decode("a row has no cells".into()))?;
    let mut out = Map::new();
    for (field, cell) in fields.iter().zip(cells) {
        let name = field
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let v = cell.get("v").unwrap_or(&Value::Null);
        out.insert(name.to_owned(), decode_field(field, v)?);
    }
    Ok(out)
}

/// One cell value of `field`, honouring `mode: REPEATED`.
fn decode_field(field: &Value, v: &Value) -> Result<Value, BigQueryError> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    if field.get("mode").and_then(Value::as_str) == Some("REPEATED") {
        let items = v
            .as_array()
            .ok_or_else(|| BigQueryError::Decode("a repeated cell is not an array".into()))?;
        return items
            .iter()
            .map(|item| decode_single(field, item.get("v").unwrap_or(&Value::Null)))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    decode_single(field, v)
}

/// One non-repeated value of `field`.
fn decode_single(field: &Value, v: &Value) -> Result<Value, BigQueryError> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    let kind = field
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("STRING");
    if matches!(kind, "RECORD" | "STRUCT") {
        return decode_record(&fields_of(field), v).map(Value::Object);
    }
    let Some(text) = v.as_str() else {
        return Ok(v.clone());
    };
    Ok(match kind {
        "INTEGER" | "INT64" => match text.parse::<i64>() {
            Ok(n) if (-MAX_SAFE_INT..=MAX_SAFE_INT).contains(&n) => Value::from(n),
            _ => Value::from(text),
        },
        "FLOAT" | "FLOAT64" => text
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map_or_else(|| Value::from(text), Value::Number),
        "BOOLEAN" | "BOOL" => match text {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            other => Value::from(other),
        },
        "TIMESTAMP" => text
            .parse::<i64>()
            .ok()
            .and_then(DateTime::from_timestamp_micros)
            .map_or_else(
                || Value::from(text),
                |t| Value::from(t.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
            ),
        _ => Value::from(text),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;

    fn one(field: Value, cell: Value) -> Value {
        let schema = json!({ "fields": [field] });
        let rows = json!([{ "f": [{ "v": cell }] }]);
        let mut out = decode_rows(Some(&schema), Some(&rows)).unwrap();
        out.remove(0).remove("c").unwrap()
    }

    fn f(kind: &str) -> Value {
        json!({ "name": "c", "type": kind })
    }

    #[test]
    fn integers_are_numbers_within_2_pow_53_else_strings() {
        assert_eq!(one(f("INTEGER"), json!("42")), json!(42));
        assert_eq!(
            one(f("INT64"), json!("-9007199254740992")),
            json!(-9007199254740992_i64)
        );
        assert_eq!(
            one(f("INT64"), json!("9007199254740993")),
            json!("9007199254740993")
        );
    }

    #[test]
    fn floats_are_numbers_and_non_finite_are_strings() {
        assert_eq!(one(f("FLOAT"), json!("1.5")), json!(1.5));
        assert_eq!(one(f("FLOAT64"), json!("NaN")), json!("NaN"));
        assert_eq!(one(f("FLOAT64"), json!("Infinity")), json!("Infinity"));
    }

    #[test]
    fn numerics_stay_strings() {
        assert_eq!(one(f("NUMERIC"), json!("1.10")), json!("1.10"));
        assert_eq!(
            one(f("BIGNUMERIC"), json!("12345678901234567890.5")),
            json!("12345678901234567890.5")
        );
    }

    #[test]
    fn booleans() {
        assert_eq!(one(f("BOOLEAN"), json!("true")), json!(true));
        assert_eq!(one(f("BOOL"), json!("false")), json!(false));
    }

    #[test]
    fn timestamps_from_int64_micros_are_rfc3339() {
        assert_eq!(
            one(f("TIMESTAMP"), json!("1763553798824855")),
            json!("2025-11-19T12:03:18.824855Z")
        );
        assert_eq!(
            one(f("TIMESTAMP"), json!("0")),
            json!("1970-01-01T00:00:00Z")
        );
    }

    #[test]
    fn other_scalars_are_strings_as_given() {
        for kind in [
            "STRING",
            "DATE",
            "DATETIME",
            "TIME",
            "BYTES",
            "GEOGRAPHY",
            "JSON",
        ] {
            assert_eq!(one(f(kind), json!("x")), json!("x"), "{kind}");
        }
    }

    #[test]
    fn nulls() {
        assert_eq!(one(f("INT64"), Value::Null), Value::Null);
    }

    #[test]
    fn records_and_repeated() {
        let rec = json!({ "name": "c", "type": "RECORD", "fields": [
            { "name": "n", "type": "INT64" },
            { "name": "tags", "type": "STRING", "mode": "REPEATED" },
        ]});
        let cell = json!({ "f": [ { "v": "7" }, { "v": [ { "v": "a" }, { "v": "b" } ] } ] });
        assert_eq!(one(rec, cell), json!({ "n": 7, "tags": ["a", "b"] }));

        let rep = json!({ "name": "c", "type": "INT64", "mode": "REPEATED" });
        assert_eq!(
            one(rep, json!([ { "v": "1" }, { "v": "2" } ])),
            json!([1, 2])
        );
    }

    #[test]
    fn missing_schema_or_rows_is_empty() {
        assert!(decode_rows(None, None).unwrap().is_empty());
        let schema = json!({ "fields": [ { "name": "c", "type": "INT64" } ] });
        assert!(decode_rows(Some(&schema), None).unwrap().is_empty());
    }

    #[test]
    fn a_row_without_cells_is_a_decode_error() {
        let schema = json!({ "fields": [ { "name": "c", "type": "INT64" } ] });
        let err = decode_rows(Some(&schema), Some(&json!([{ "x": 1 }]))).unwrap_err();
        assert!(matches!(err, BigQueryError::Decode(_)), "{err:?}");
    }
}
