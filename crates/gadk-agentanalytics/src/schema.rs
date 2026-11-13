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

//! Port of ADK-Go `plugin/agentanalytics/schema.go`: the events table's BigQuery schema
//! ([`events_schema`]) and Arrow schema ([`arrow_schema`], [`serialized_arrow_schema`]), their
//! payload-column projections, and the additive schema merge ([`upgrade_table_schema`],
//! [`SchemaMismatchError`]).
//!
//! [`FieldSchema`] has the fields of Go's `bigquery.FieldSchema`; its serde form is the BigQuery
//! REST `TableFieldSchema` JSON Go's client sends and reads (`FieldSchema.toBQ` /
//! `bqToFieldSchema`: `mode` `REPEATED` / `REQUIRED` or omitted, int64 fields as JSON strings,
//! empty fields omitted).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

/// Go `timestampField`: the partitioning column.
pub(crate) const TIMESTAMP_FIELD: &str = "timestamp";
/// Go `eventIDField`.
pub(crate) const EVENT_ID_FIELD: &str = "event_id";

/// Go `bigquery.StringFieldType`.
pub const FIELD_TYPE_STRING: &str = "STRING";
/// Go `bigquery.TimestampFieldType`.
pub const FIELD_TYPE_TIMESTAMP: &str = "TIMESTAMP";
/// Go `bigquery.JSONFieldType`.
pub const FIELD_TYPE_JSON: &str = "JSON";
/// Go `bigquery.RecordFieldType`.
pub const FIELD_TYPE_RECORD: &str = "RECORD";
/// Go `bigquery.IntegerFieldType`.
pub const FIELD_TYPE_INTEGER: &str = "INTEGER";
/// Go `bigquery.BooleanFieldType`.
pub const FIELD_TYPE_BOOLEAN: &str = "BOOLEAN";

/// A BigQuery table schema (Go `bigquery.Schema`, a list of fields).
pub type Schema = Vec<FieldSchema>;

/// The policy tags of a field (Go `bigquery.PolicyTagList`; REST `policyTags`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyTagList {
    /// Go `Names`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// The element type of a RANGE field (Go `bigquery.RangeElementType`; REST `rangeElementType`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeElementType {
    /// Go `Type`.
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub field_type: String,
}

/// One field of a BigQuery schema (Go `bigquery.FieldSchema`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldSchema {
    /// Go `Name`.
    pub name: String,
    /// Go `Description`.
    pub description: String,
    /// Go `Repeated`.
    pub repeated: bool,
    /// Go `Required`.
    pub required: bool,
    /// Go `Type` (`STRING`, `RECORD`, …).
    pub field_type: String,
    /// Go `PolicyTags`.
    pub policy_tags: Option<PolicyTagList>,
    /// Go `Schema`: the nested fields of a RECORD.
    pub schema: Schema,
    /// Go `MaxLength`.
    pub max_length: i64,
    /// Go `Precision`.
    pub precision: i64,
    /// Go `Scale`.
    pub scale: i64,
    /// Go `DefaultValueExpression`.
    pub default_value_expression: String,
    /// Go `Collation`.
    pub collation: String,
    /// Go `RangeElementType`.
    pub range_element_type: Option<RangeElementType>,
    /// Go `RoundingMode`.
    pub rounding_mode: String,
    /// Go `TimestampPrecision` (sent only when > 0).
    pub timestamp_precision: i64,
}

/// A REST `TableSchema` (`{"fields": [...]}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    /// The fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Schema,
}

/// Go's `json:",string"` int64 encoding of the REST client: a JSON string; reads a string or a
/// number.
mod int64_string {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Str(String),
        Num(i64),
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        match Raw::deserialize(d)? {
            Raw::Num(n) => Ok(n),
            Raw::Str(s) => s.parse().map_err(D::Error::custom),
        }
    }

    pub(super) mod option {
        use serde::{Deserialize, Deserializer, Serializer};

        pub(in super::super) fn serialize<S: Serializer>(
            v: &Option<i64>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match v {
                Some(n) => super::serialize(n, s),
                None => s.serialize_none(),
            }
        }

        pub(in super::super) fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<i64>, D::Error> {
            #[derive(Deserialize)]
            struct Wrap(#[serde(with = "super")] i64);
            Ok(Option::<Wrap>::deserialize(d)?.map(|Wrap(n)| n))
        }
    }
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// The REST `TableFieldSchema` written for a [`FieldSchema`] (Go `FieldSchema.toBQ`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireFieldRef<'a> {
    #[serde(skip_serializing_if = "str::is_empty")]
    collation: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    default_value_expression: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    #[serde(skip_serializing_if = "<[FieldSchema]>::is_empty")]
    fields: &'a [FieldSchema],
    #[serde(skip_serializing_if = "is_zero", with = "int64_string")]
    max_length: i64,
    #[serde(skip_serializing_if = "str::is_empty")]
    mode: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_tags: Option<&'a PolicyTagList>,
    #[serde(skip_serializing_if = "is_zero", with = "int64_string")]
    precision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    range_element_type: Option<&'a RangeElementType>,
    #[serde(skip_serializing_if = "str::is_empty")]
    rounding_mode: &'a str,
    #[serde(skip_serializing_if = "is_zero", with = "int64_string")]
    scale: i64,
    #[serde(skip_serializing_if = "Option::is_none", with = "int64_string::option")]
    timestamp_precision: Option<i64>,
    #[serde(rename = "type", skip_serializing_if = "str::is_empty")]
    field_type: &'a str,
}

/// The REST `TableFieldSchema` read into a [`FieldSchema`] (Go `bqToFieldSchema`); unknown
/// properties are ignored, as Go's struct ignores them.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireField {
    collation: String,
    default_value_expression: String,
    description: String,
    fields: Vec<FieldSchema>,
    #[serde(with = "int64_string")]
    max_length: i64,
    mode: String,
    name: String,
    policy_tags: Option<PolicyTagList>,
    #[serde(with = "int64_string")]
    precision: i64,
    range_element_type: Option<RangeElementType>,
    rounding_mode: String,
    #[serde(with = "int64_string")]
    scale: i64,
    #[serde(with = "int64_string::option")]
    timestamp_precision: Option<i64>,
    #[serde(rename = "type")]
    field_type: String,
}

impl Serialize for FieldSchema {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mode = if self.repeated {
            "REPEATED"
        } else if self.required {
            "REQUIRED"
        } else {
            ""
        };
        WireFieldRef {
            collation: &self.collation,
            default_value_expression: &self.default_value_expression,
            description: &self.description,
            fields: &self.schema,
            max_length: self.max_length,
            mode,
            name: &self.name,
            policy_tags: self.policy_tags.as_ref(),
            precision: self.precision,
            range_element_type: self.range_element_type.as_ref(),
            rounding_mode: &self.rounding_mode,
            scale: self.scale,
            timestamp_precision: (self.timestamp_precision > 0).then_some(self.timestamp_precision),
            field_type: &self.field_type,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for FieldSchema {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = WireField::deserialize(d)?;
        Ok(FieldSchema {
            name: w.name,
            description: w.description,
            repeated: w.mode == "REPEATED",
            required: w.mode == "REQUIRED",
            field_type: w.field_type,
            policy_tags: w.policy_tags,
            schema: w.fields,
            max_length: w.max_length,
            precision: w.precision,
            scale: w.scale,
            default_value_expression: w.default_value_expression,
            collation: w.collation,
            range_element_type: w.range_element_type,
            rounding_mode: w.rounding_mode,
            timestamp_precision: w.timestamp_precision.unwrap_or(0),
        })
    }
}

/// Go `EventsSchema()`: the BigQuery schema of the events table (names, types, modes,
/// descriptions, nesting and order as Go's).
pub fn events_schema() -> Schema {
    todo!("Task 3")
}

/// Go `ArrowSchema()`: the Arrow schema of the Storage Write API rows (timestamp µs, UTF-8
/// strings, the `content_parts` list of structs, `google:sqlType:json` extension metadata on the
/// JSON columns).
pub fn arrow_schema() -> ::arrow_schema::Schema {
    todo!("Task 3")
}

/// Go `projectEventsSchema(denied)`: [`events_schema`] without the denied top-level columns.
pub fn project_events_schema(denied: &BTreeSet<String>) -> Schema {
    todo!("Task 3")
}

/// Go `projectArrowSchema(denied)`: [`arrow_schema`] without the denied top-level columns.
pub fn project_arrow_schema(denied: &BTreeSet<String>) -> ::arrow_schema::Schema {
    todo!("Task 3")
}

/// Go `SerializedArrowSchema()`: [`arrow_schema`] as an Arrow IPC stream (schema message then
/// the end-of-stream marker, as Go's `ipc.NewWriter` + `Close`).
pub fn serialized_arrow_schema() -> Result<Vec<u8>, Error> {
    todo!("Task 3")
}

/// Go `serializeArrowSchema(schema)`: `schema` as an Arrow IPC stream with no batches.
pub(crate) fn serialize_arrow_schema(schema: &::arrow_schema::Schema) -> Result<Vec<u8>, Error> {
    todo!("Task 3")
}

/// Go `upgradeTableSchema(existing, desired)`: the additive merge of `desired` into `existing`
/// and whether it changed anything (Go `mergeSchemaFields(existing, desired, "")`).
pub fn upgrade_table_schema(
    existing: &[FieldSchema],
    desired: &[FieldSchema],
) -> Result<(Schema, bool), SchemaMismatchError> {
    todo!("Task 3")
}

/// Go `mergeSchemaFields(existing, desired, prefix)`.
fn merge_schema_fields(
    existing: &[FieldSchema],
    desired: &[FieldSchema],
    prefix: &str,
) -> Result<(Schema, bool), SchemaMismatchError> {
    todo!("Task 3")
}

/// Go `cloneNullableFieldSchema(f)`: `f` and its nested fields with `required` cleared.
fn clone_nullable_field_schema(f: &FieldSchema) -> FieldSchema {
    todo!("Task 3")
}

/// Go `fieldMode(required, repeated)`: `REPEATED`, `REQUIRED` or `NULLABLE`.
fn field_mode(required: bool, repeated: bool) -> &'static str {
    todo!("Task 3")
}

/// An existing field whose type or mode the desired schema cannot be merged into (Go
/// `*schemaMismatchError`). `Display`: `incompatible BigQuery schema field <path>:
/// existing=<TYPE>/<MODE>, desired=<TYPE>/<MODE>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaMismatchError {
    /// The dotted field path.
    pub field: String,
    /// The existing field's type.
    pub got_type: String,
    /// The desired field's type.
    pub want_type: String,
    /// The existing field's `required`.
    pub got_req: bool,
    /// The desired field's `required`.
    pub want_req: bool,
    /// The existing field's `repeated`.
    pub got_rep: bool,
    /// The desired field's `repeated`.
    pub want_rep: bool,
}

impl fmt::Display for SchemaMismatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("Task 3")
    }
}

impl std::error::Error for SchemaMismatchError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn field(name: &str, field_type: &str) -> FieldSchema {
        FieldSchema {
            name: name.into(),
            field_type: field_type.into(),
            ..FieldSchema::default()
        }
    }

    #[test]
    fn field_schema_serializes_like_go_to_bq() {
        let record = FieldSchema {
            description: "parts".into(),
            repeated: true,
            schema: vec![
                FieldSchema {
                    required: true,
                    ..field("mime_type", FIELD_TYPE_STRING)
                },
                FieldSchema {
                    max_length: 10,
                    precision: 3,
                    scale: 1,
                    timestamp_precision: 12,
                    policy_tags: Some(PolicyTagList {
                        names: vec!["t".into()],
                    }),
                    ..field("n", "NUMERIC")
                },
            ],
            ..field("content_parts", FIELD_TYPE_RECORD)
        };
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            json!({
                "description": "parts",
                "fields": [
                    {"mode": "REQUIRED", "name": "mime_type", "type": "STRING"},
                    {"maxLength": "10", "name": "n", "policyTags": {"names": ["t"]},
                     "precision": "3", "scale": "1", "timestampPrecision": "12", "type": "NUMERIC"}
                ],
                "mode": "REPEATED",
                "name": "content_parts",
                "type": "RECORD"
            })
        );
        // Repeated wins over required (Go checks Repeated first).
        let both = FieldSchema {
            repeated: true,
            required: true,
            ..field("x", FIELD_TYPE_STRING)
        };
        assert_eq!(serde_json::to_value(&both).unwrap()["mode"], "REPEATED");
    }

    #[test]
    fn field_schema_deserializes_like_go_bq_to_field_schema() {
        let schema: TableSchema = serde_json::from_value(json!({
            "fields": [
                {"name": "timestamp", "type": "TIMESTAMP", "mode": "REQUIRED"},
                {"name": "status", "type": "STRING", "mode": "NULLABLE", "unknownProp": 1},
                {"name": "n", "type": "NUMERIC", "maxLength": "7", "precision": 5,
                 "timestampPrecision": "6",
                 "rangeElementType": {"type": "DATE"}},
                {"name": "r", "type": "RECORD", "mode": "REPEATED",
                 "fields": [{"name": "a", "type": "STRING"}]}
            ]
        }))
        .unwrap();
        let f = &schema.fields;
        assert!(f[0].required && !f[0].repeated);
        assert!(!f[1].required && !f[1].repeated);
        assert_eq!(f[2].max_length, 7);
        assert_eq!(f[2].precision, 5);
        assert_eq!(f[2].timestamp_precision, 6);
        assert_eq!(
            f[2].range_element_type,
            Some(RangeElementType {
                field_type: "DATE".into()
            })
        );
        assert!(f[3].repeated);
        assert_eq!(f[3].schema, vec![field("a", FIELD_TYPE_STRING)]);
        // NULLABLE is not written back.
        assert_eq!(
            serde_json::to_value(&f[1]).unwrap(),
            json!({"name": "status", "type": "STRING"})
        );
        let empty: TableSchema = serde_json::from_value(json!({})).unwrap();
        assert!(empty.fields.is_empty());
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
    }
}
