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

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, FieldRef, Fields, TimeUnit};

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

fn bq_field(name: &str, field_type: &str, description: &str) -> FieldSchema {
    FieldSchema {
        name: name.to_owned(),
        field_type: field_type.to_owned(),
        description: description.to_owned(),
        ..FieldSchema::default()
    }
}

fn bq_required_field(name: &str, field_type: &str, description: &str) -> FieldSchema {
    FieldSchema {
        required: true,
        ..bq_field(name, field_type, description)
    }
}

fn bq_record_field(name: &str, description: &str, schema: Schema) -> FieldSchema {
    FieldSchema {
        schema,
        ..bq_field(name, FIELD_TYPE_RECORD, description)
    }
}

fn bq_repeated_record_field(name: &str, description: &str, schema: Schema) -> FieldSchema {
    FieldSchema {
        repeated: true,
        ..bq_record_field(name, description, schema)
    }
}

/// Go `EventsSchema()`: the BigQuery schema of the events table (names, types, modes,
/// descriptions, nesting and order as Go's).
pub fn events_schema() -> Schema {
    vec![
        bq_required_field(
            TIMESTAMP_FIELD,
            FIELD_TYPE_TIMESTAMP,
            "The UTC timestamp when the event occurred. Used for ordering events within a session.",
        ),
        bq_field(
            EVENT_ID_FIELD,
            FIELD_TYPE_STRING,
            "A unique identifier assigned before enqueue. Storage Write API retries preserve this value so duplicate rows can be identified reliably.",
        ),
        bq_field(
            "event_type",
            FIELD_TYPE_STRING,
            "The category of the event (e.g., 'LLM_REQUEST', 'TOOL_CALL', 'AGENT_RESPONSE'). Helps in filtering specific types of interactions.",
        ),
        bq_field(
            "agent",
            FIELD_TYPE_STRING,
            "The name of the agent that generated this event. Useful for multi-agent systems.",
        ),
        bq_field(
            "session_id",
            FIELD_TYPE_STRING,
            "A unique identifier for the entire conversation session. Used to group all events belonging to a single user interaction.",
        ),
        bq_field(
            "invocation_id",
            FIELD_TYPE_STRING,
            "A unique identifier for a single turn or execution within a session. Groups related events like LLM request and response.",
        ),
        bq_field(
            "user_id",
            FIELD_TYPE_STRING,
            "The identifier of the end-user participating in the session, if available.",
        ),
        bq_field(
            "trace_id",
            FIELD_TYPE_STRING,
            "OpenTelemetry trace ID for distributed tracing across services.",
        ),
        bq_field(
            "span_id",
            FIELD_TYPE_STRING,
            "Unique identifier for this specific operation or span within the trace (invocation, agent run, model call, or tool call). When EnableOTelCorrelation is set, raw ambient OpenTelemetry span IDs are also written to attributes.otel.span_id.",
        ),
        bq_field(
            "parent_span_id",
            FIELD_TYPE_STRING,
            "Identifier of the parent span in the execution tree, establishing causality and hierarchy across invocations, agent runs, model calls, and tool calls.",
        ),
        bq_field(
            "content",
            FIELD_TYPE_JSON,
            "The primary payload of the event, stored as a JSON string. The structure depends on the event_type (e.g., prompt text for LLM_REQUEST, tool output for TOOL_RESPONSE).",
        ),
        bq_repeated_record_field(
            "content_parts",
            "For multi-modal events, contains a list of content parts (text, images, etc.).",
            vec![
                bq_field(
                    "mime_type",
                    FIELD_TYPE_STRING,
                    "The MIME type of the content part (e.g., 'text/plain', 'image/png').",
                ),
                bq_field(
                    "uri",
                    FIELD_TYPE_STRING,
                    "The URI of the content part if stored externally (e.g., GCS bucket path).",
                ),
                bq_record_field(
                    "object_ref",
                    "The ObjectRef of the content part if stored externally.",
                    vec![
                        bq_field("uri", FIELD_TYPE_STRING, "The URI of the object."),
                        bq_field("version", FIELD_TYPE_STRING, "The version of the object."),
                        bq_field(
                            "authorizer",
                            FIELD_TYPE_STRING,
                            "The authorizer for the object.",
                        ),
                        bq_field(
                            "details",
                            FIELD_TYPE_JSON,
                            "Additional details about the object.",
                        ),
                    ],
                ),
                bq_field(
                    "text",
                    FIELD_TYPE_STRING,
                    "The raw text content if the part is text-based.",
                ),
                bq_field(
                    "part_index",
                    FIELD_TYPE_INTEGER,
                    "The zero-based index of this part within the content.",
                ),
                bq_field(
                    "part_attributes",
                    FIELD_TYPE_STRING,
                    "Additional metadata for this content part as a JSON object (serialized to string).",
                ),
                bq_field(
                    "storage_mode",
                    FIELD_TYPE_STRING,
                    "Indicates how the content part is stored (e.g., 'INLINE', 'GCS_REFERENCE', 'EXTERNAL_URI').",
                ),
            ],
        ),
        bq_field(
            "attributes",
            FIELD_TYPE_JSON,
            "A JSON object containing arbitrary key-value pairs for additional event metadata. Includes enrichment fields like 'root_agent_name' (turn orchestration), 'model' (request model), 'model_version' (response version), and 'usage_metadata' (detailed token counts). May also carry 'otel' (best-effort ambient Cloud Trace span/trace ids) and 'custom_metadata' (allowlisted event.custom_metadata keys).",
        ),
        bq_field(
            "latency_ms",
            FIELD_TYPE_JSON,
            "A JSON object containing latency measurements, such as 'total_ms' and 'time_to_first_token_ms'.",
        ),
        bq_field(
            "status",
            FIELD_TYPE_STRING,
            "The outcome of the event, typically 'OK' or 'ERROR'.",
        ),
        bq_field(
            "error_message",
            FIELD_TYPE_STRING,
            "Diagnostic message for errors and model termination details; may be populated on rows whose status is 'OK', such as LLM_RESPONSE rows and rows whose content_formatter failed.",
        ),
        bq_field(
            "is_truncated",
            FIELD_TYPE_BOOLEAN,
            "Boolean flag indicating if the content or metadata payload was truncated because it exceeded the maximum allowed size. Set when 'content', captured 'custom_metadata', or A2A metadata is truncated; redaction of sensitive keys does not set this flag.",
        ),
    ]
}

/// The Arrow field metadata Go puts on the JSON columns
/// (`ARROW:extension:name` = `google:sqlType:json`).
fn json_metadata() -> HashMap<String, String> {
    HashMap::from([(
        "ARROW:extension:name".to_owned(),
        "google:sqlType:json".to_owned(),
    )])
}

/// A nullable UTF-8 Arrow field (Go `arrow.BinaryTypes.String`, `Nullable: true`).
fn utf8_field(name: &str) -> Field {
    Field::new(name, DataType::Utf8, true)
}

/// A nullable UTF-8 Arrow field carrying the JSON extension metadata.
fn json_field(name: &str) -> Field {
    utf8_field(name).with_metadata(json_metadata())
}

/// Go `ArrowSchema()`: the Arrow schema of the Storage Write API rows (timestamp µs, UTF-8
/// strings, the `content_parts` list of structs, `google:sqlType:json` extension metadata on the
/// JSON columns).
pub fn arrow_schema() -> ::arrow_schema::Schema {
    let object_ref = DataType::Struct(Fields::from(vec![
        utf8_field("uri"),
        utf8_field("version"),
        utf8_field("authorizer"),
        json_field("details"),
    ]));
    let content_part = DataType::Struct(Fields::from(vec![
        utf8_field("mime_type"),
        utf8_field("uri"),
        Field::new("object_ref", object_ref, true),
        utf8_field("text"),
        Field::new("part_index", DataType::Int64, true),
        utf8_field("part_attributes"),
        utf8_field("storage_mode"),
    ]));
    ::arrow_schema::Schema::new(vec![
        Field::new(
            TIMESTAMP_FIELD,
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        utf8_field(EVENT_ID_FIELD),
        utf8_field("event_type"),
        utf8_field("agent"),
        utf8_field("session_id"),
        utf8_field("invocation_id"),
        utf8_field("user_id"),
        utf8_field("trace_id"),
        utf8_field("span_id"),
        utf8_field("parent_span_id"),
        json_field("content"),
        Field::new(
            "content_parts",
            DataType::List(Arc::new(Field::new("element", content_part, true))),
            true,
        ),
        json_field("attributes"),
        json_field("latency_ms"),
        utf8_field("status"),
        utf8_field("error_message"),
        Field::new("is_truncated", DataType::Boolean, true),
    ])
}

/// Go `projectEventsSchema(denied)`: [`events_schema`] without the denied top-level columns.
pub fn project_events_schema(denied: &BTreeSet<String>) -> Schema {
    let schema = events_schema();
    if denied.is_empty() {
        return schema;
    }
    schema
        .into_iter()
        .filter(|f| !denied.contains(&f.name))
        .collect()
}

/// Go `projectArrowSchema(denied)`: [`arrow_schema`] without the denied top-level columns.
pub fn project_arrow_schema(denied: &BTreeSet<String>) -> ::arrow_schema::Schema {
    let schema = arrow_schema();
    if denied.is_empty() {
        return schema;
    }
    let fields: Vec<FieldRef> = schema
        .fields()
        .iter()
        .filter(|f| !denied.contains(f.name()))
        .cloned()
        .collect();
    ::arrow_schema::Schema::new(fields)
}

/// Go `SerializedArrowSchema()`: [`arrow_schema`] as an Arrow IPC stream (schema message then
/// the end-of-stream marker, as Go's `ipc.NewWriter` + `Close`).
pub fn serialized_arrow_schema() -> Result<Vec<u8>, Error> {
    serialize_arrow_schema(&arrow_schema())
}

/// Go `serializeArrowSchema(schema)`: `schema` as an Arrow IPC stream with no batches.
///
/// Divergence (ledger 7b-1): the bytes are arrow-rs's encoding, not arrow-go's. Both are one
/// continuation-framed schema message then the end-of-stream marker and decode to the same
/// schema (spec R3), but the flatbuffer table layout and padding differ (events schema: 1864
/// bytes here, 1880 from Go), so the bytes are not compared.
pub(crate) fn serialize_arrow_schema(schema: &::arrow_schema::Schema) -> Result<Vec<u8>, Error> {
    let mut writer = StreamWriter::try_new(Vec::new(), schema)?;
    writer.finish()?;
    Ok(writer.into_inner()?)
}

/// Go `upgradeTableSchema(existing, desired)`: the additive merge of `desired` into `existing`
/// and whether it changed anything (Go `mergeSchemaFields(existing, desired, "")`). Existing
/// column order is kept; missing fields (and nested RECORD sub-fields) are appended as
/// NULLABLE.
pub fn upgrade_table_schema(
    existing: &[FieldSchema],
    desired: &[FieldSchema],
) -> Result<(Schema, bool), SchemaMismatchError> {
    merge_schema_fields(existing, desired, "")
}

/// Go `mergeSchemaFields(existing, desired, prefix)`. Names match case-insensitively (Go
/// `strings.ToLower`); with duplicate names the last existing field wins, as Go's map does.
fn merge_schema_fields(
    existing: &[FieldSchema],
    desired: &[FieldSchema],
    prefix: &str,
) -> Result<(Schema, bool), SchemaMismatchError> {
    let existing_by_name: HashMap<String, &FieldSchema> = existing
        .iter()
        .map(|f| (f.name.to_lowercase(), f))
        .collect();
    let mut new_fields: Schema = Vec::new();
    let mut updated_records: HashMap<String, FieldSchema> = HashMap::new();

    for df in desired {
        let key = df.name.to_lowercase();
        let Some(ef) = existing_by_name.get(&key) else {
            new_fields.push(clone_nullable_field_schema(df));
            continue;
        };
        let field_path = if prefix.is_empty() {
            df.name.clone()
        } else {
            format!("{prefix}.{}", df.name)
        };
        if ef.field_type != df.field_type
            || (ef.required && !df.required)
            || ef.repeated != df.repeated
        {
            return Err(SchemaMismatchError {
                field: field_path,
                got_type: ef.field_type.clone(),
                want_type: df.field_type.clone(),
                got_req: ef.required,
                want_req: df.required,
                got_rep: ef.repeated,
                want_rep: df.repeated,
            });
        }
        if df.field_type == FIELD_TYPE_RECORD && !df.schema.is_empty() {
            let (merged_sub, sub_changed) =
                merge_schema_fields(&ef.schema, &df.schema, &field_path)?;
            if sub_changed {
                let mut clone = (*ef).clone();
                clone.schema = merged_sub;
                updated_records.insert(key, clone);
            }
        }
    }

    if new_fields.is_empty() && updated_records.is_empty() {
        return Ok((existing.to_vec(), false));
    }

    let mut merged: Schema = Vec::with_capacity(existing.len() + new_fields.len());
    for ef in existing {
        match updated_records.get(&ef.name.to_lowercase()) {
            Some(updated) => merged.push(updated.clone()),
            None => merged.push(ef.clone()),
        }
    }
    merged.extend(new_fields);
    Ok((merged, true))
}

/// Go `cloneNullableFieldSchema(f)`: `f` and its nested fields with `required` cleared.
fn clone_nullable_field_schema(f: &FieldSchema) -> FieldSchema {
    FieldSchema {
        required: false,
        schema: f.schema.iter().map(clone_nullable_field_schema).collect(),
        ..f.clone()
    }
}

/// Go `fieldMode(required, repeated)`: `REPEATED`, `REQUIRED` or `NULLABLE`.
fn field_mode(required: bool, repeated: bool) -> &'static str {
    if repeated {
        "REPEATED"
    } else if required {
        "REQUIRED"
    } else {
        "NULLABLE"
    }
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
        write!(
            f,
            "incompatible BigQuery schema field {}: existing={}/{}, desired={}/{}",
            self.field,
            self.got_type,
            field_mode(self.got_req, self.got_rep),
            self.want_type,
            field_mode(self.want_req, self.want_rep),
        )
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

    fn denied(cols: &[&str]) -> BTreeSet<String> {
        cols.iter().map(|c| (*c).to_owned()).collect()
    }

    #[test]
    fn events_schema_shape() {
        let s = events_schema();
        assert_eq!(s.len(), 17);
        assert_eq!(s[0].name, TIMESTAMP_FIELD);
        assert!(s[0].required && s[0].field_type == FIELD_TYPE_TIMESTAMP);
        assert_eq!(s[1].name, EVENT_ID_FIELD);
        let parts = s.iter().find(|f| f.name == "content_parts").unwrap();
        assert!(parts.repeated && !parts.required);
        assert_eq!(parts.schema.len(), 7);
        let object_ref = parts
            .schema
            .iter()
            .find(|f| f.name == "object_ref")
            .unwrap();
        assert_eq!(object_ref.field_type, FIELD_TYPE_RECORD);
        assert!(!object_ref.repeated);
        assert_eq!(object_ref.schema[3].field_type, FIELD_TYPE_JSON);
        // Only `timestamp` is REQUIRED and only `content_parts` REPEATED (top level).
        assert_eq!(s.iter().filter(|f| f.required).count(), 1);
        assert_eq!(s.iter().filter(|f| f.repeated).count(), 1);
    }

    #[test]
    fn arrow_schema_mirrors_events_schema_columns() {
        let arrow = arrow_schema();
        let names: Vec<&str> = arrow.fields().iter().map(|f| f.name().as_str()).collect();
        let bq: Vec<String> = events_schema().into_iter().map(|f| f.name).collect();
        assert_eq!(names, bq);
        assert!(arrow.metadata().is_empty());
        assert!(!arrow.field(0).is_nullable());
        assert!(arrow.fields().iter().skip(1).all(|f| f.is_nullable()));
        for name in ["content", "attributes", "latency_ms"] {
            let f = arrow.field_with_name(name).unwrap();
            assert_eq!(
                f.metadata().get("ARROW:extension:name").map(String::as_str),
                Some("google:sqlType:json")
            );
        }
    }

    #[test]
    fn projections_drop_only_denied_top_level_columns() {
        assert_eq!(project_events_schema(&BTreeSet::new()), events_schema());
        assert_eq!(project_arrow_schema(&BTreeSet::new()), arrow_schema());
        let d = denied(&["content", "latency_ms", "not_a_column"]);
        let bq: Vec<String> = project_events_schema(&d)
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(bq.len(), 15);
        assert!(!bq.iter().any(|n| n == "content" || n == "latency_ms"));
        let arrow = project_arrow_schema(&d);
        let names: Vec<&str> = arrow.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names, bq);
        // Nested fields of the same name are kept.
        let all = project_events_schema(&denied(&["uri", "details"]));
        assert_eq!(all, events_schema());
    }

    #[test]
    fn serialized_arrow_schema_is_schema_then_eos() {
        let bytes = serialized_arrow_schema().unwrap();
        assert_eq!(&bytes[..4], &[0xff; 4]);
        assert_eq!(
            &bytes[bytes.len() - 8..],
            &[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]
        );
        let reader =
            arrow_ipc::reader::StreamReader::try_new(std::io::Cursor::new(&bytes), None).unwrap();
        assert_eq!(reader.schema().as_ref(), &arrow_schema());
    }

    #[test]
    fn field_mode_prefers_repeated() {
        assert_eq!(field_mode(false, false), "NULLABLE");
        assert_eq!(field_mode(true, false), "REQUIRED");
        assert_eq!(field_mode(false, true), "REPEATED");
        assert_eq!(field_mode(true, true), "REPEATED");
    }

    #[test]
    fn schema_mismatch_error_text_matches_go() {
        let err = SchemaMismatchError {
            field: "content_parts.mime_type".into(),
            got_type: FIELD_TYPE_STRING.into(),
            want_type: FIELD_TYPE_STRING.into(),
            got_req: true,
            want_req: false,
            got_rep: false,
            want_rep: false,
        };
        assert_eq!(
            err.to_string(),
            "incompatible BigQuery schema field content_parts.mime_type: \
             existing=STRING/REQUIRED, desired=STRING/NULLABLE"
        );
    }

    #[test]
    fn upgrade_identical_schema_is_unchanged() {
        let s = events_schema();
        let (merged, changed) = upgrade_table_schema(&s, &s).unwrap();
        assert!(!changed);
        assert_eq!(merged, s);
    }

    #[test]
    fn upgrade_appends_missing_fields_as_nullable_keeping_existing_order() {
        let existing = vec![field("b", FIELD_TYPE_STRING), field("a", FIELD_TYPE_STRING)];
        let desired = vec![
            field("a", FIELD_TYPE_STRING),
            FieldSchema {
                required: true,
                schema: vec![FieldSchema {
                    required: true,
                    ..field("inner", FIELD_TYPE_STRING)
                }],
                ..field("rec", FIELD_TYPE_RECORD)
            },
            field("b", FIELD_TYPE_STRING),
        ];
        let (merged, changed) = upgrade_table_schema(&existing, &desired).unwrap();
        assert!(changed);
        assert_eq!(
            merged,
            vec![
                field("b", FIELD_TYPE_STRING),
                field("a", FIELD_TYPE_STRING),
                FieldSchema {
                    schema: vec![field("inner", FIELD_TYPE_STRING)],
                    ..field("rec", FIELD_TYPE_RECORD)
                },
            ]
        );
    }

    #[test]
    fn upgrade_merges_nested_record_case_insensitively() {
        let existing = vec![FieldSchema {
            repeated: true,
            description: "kept".into(),
            schema: vec![FieldSchema {
                required: true,
                ..field("MIME_TYPE", FIELD_TYPE_STRING)
            }],
            ..field("Content_Parts", FIELD_TYPE_RECORD)
        }];
        let desired = vec![FieldSchema {
            repeated: true,
            schema: vec![
                FieldSchema {
                    required: true,
                    ..field("mime_type", FIELD_TYPE_STRING)
                },
                field("uri", FIELD_TYPE_STRING),
            ],
            ..field("content_parts", FIELD_TYPE_RECORD)
        }];
        let (merged, changed) = upgrade_table_schema(&existing, &desired).unwrap();
        assert!(changed);
        assert_eq!(
            merged,
            vec![FieldSchema {
                repeated: true,
                description: "kept".into(),
                schema: vec![
                    FieldSchema {
                        required: true,
                        ..field("MIME_TYPE", FIELD_TYPE_STRING)
                    },
                    field("uri", FIELD_TYPE_STRING),
                ],
                ..field("Content_Parts", FIELD_TYPE_RECORD)
            }]
        );
    }

    #[test]
    fn upgrade_allows_existing_nullable_where_desired_required() {
        let existing = vec![field("timestamp", FIELD_TYPE_TIMESTAMP)];
        let desired = vec![FieldSchema {
            required: true,
            ..field("timestamp", FIELD_TYPE_TIMESTAMP)
        }];
        let (merged, changed) = upgrade_table_schema(&existing, &desired).unwrap();
        assert!(!changed);
        assert_eq!(merged, existing);
    }

    #[test]
    fn upgrade_rejects_incompatible_fields_with_go_texts() {
        let cases: Vec<(Schema, Schema, &str)> = vec![
            (
                vec![field("event_id", FIELD_TYPE_INTEGER)],
                vec![field("event_id", FIELD_TYPE_STRING)],
                "incompatible BigQuery schema field event_id: existing=INTEGER/NULLABLE, \
                 desired=STRING/NULLABLE",
            ),
            (
                vec![field("content_parts", FIELD_TYPE_RECORD)],
                vec![FieldSchema {
                    repeated: true,
                    ..field("content_parts", FIELD_TYPE_RECORD)
                }],
                "incompatible BigQuery schema field content_parts: existing=RECORD/NULLABLE, \
                 desired=RECORD/REPEATED",
            ),
            (
                vec![FieldSchema {
                    required: true,
                    ..field("content", FIELD_TYPE_JSON)
                }],
                vec![field("content", FIELD_TYPE_JSON)],
                "incompatible BigQuery schema field content: existing=JSON/REQUIRED, \
                 desired=JSON/NULLABLE",
            ),
            (
                vec![FieldSchema {
                    repeated: true,
                    schema: vec![field("part_index", FIELD_TYPE_STRING)],
                    ..field("content_parts", FIELD_TYPE_RECORD)
                }],
                vec![FieldSchema {
                    repeated: true,
                    schema: vec![field("part_index", FIELD_TYPE_INTEGER)],
                    ..field("content_parts", FIELD_TYPE_RECORD)
                }],
                "incompatible BigQuery schema field content_parts.part_index: \
                 existing=STRING/NULLABLE, desired=INTEGER/NULLABLE",
            ),
        ];
        for (existing, desired, want) in cases {
            let err = upgrade_table_schema(&existing, &desired).unwrap_err();
            assert_eq!(err.to_string(), want);
        }
    }

    #[test]
    fn upgrade_against_empty_existing_adds_whole_events_schema_nullable() {
        let (merged, changed) = upgrade_table_schema(&[], &events_schema()).unwrap();
        assert!(changed);
        assert_eq!(merged.len(), 17);
        assert!(!merged[0].required);
        assert!(merged[11].repeated);
    }
}
