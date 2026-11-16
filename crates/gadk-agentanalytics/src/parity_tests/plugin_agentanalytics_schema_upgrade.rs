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

//! Port of ADK-Go `plugin/agentanalytics/schema_upgrade_test.go` (ledger 7b-1).
//!
//! Go's `mockTableOps` is [`MockTableOps`] (`bigquery::fake`); Go's `*googleapi.Error{Code}` is
//! [`FakeError::Api`] and `status.Error(codes.X, …)` is [`FakeError::Status`] (the crate
//! `Error::Status` variant). Go subtests are blocks (or table rows) inside one test fn, each
//! asserting with the subtest's name.

use std::collections::{BTreeMap, BTreeSet};

use pretty_assertions::assert_eq;

use crate::bigquery::fake::{FakeError, MockTableOps, MockTableOpsState};
use crate::config::{Config, SCHEMA_VERSION, SCHEMA_VERSION_KEY};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error};
use crate::schema::{
    FIELD_TYPE_INTEGER, FIELD_TYPE_JSON, FIELD_TYPE_RECORD, FIELD_TYPE_STRING,
    FIELD_TYPE_TIMESTAMP, FieldSchema, Schema, events_schema, upgrade_table_schema,
};
use crate::schema_upgrade::{DAY_PARTITIONING_TYPE, TableMetadata, ensure_table_and_schema};

/// Go `&bq.FieldSchema{Name: name, Type: field_type}`.
fn field(name: &str, field_type: &str) -> FieldSchema {
    FieldSchema {
        name: name.to_owned(),
        field_type: field_type.to_owned(),
        ..FieldSchema::default()
    }
}

/// Go `&bq.FieldSchema{Name: name, Type: field_type, Required: true}`.
fn required(name: &str, field_type: &str) -> FieldSchema {
    FieldSchema {
        required: true,
        ..field(name, field_type)
    }
}

/// Go `&googleapi.Error{Code: code, Message: message}`.
fn api(code: u16, message: &str) -> FakeError {
    FakeError::Api {
        code,
        message: message.to_owned(),
    }
}

/// Go `status.Error(code, message)`.
fn status(code: Code, message: &str) -> FakeError {
    FakeError::Status {
        code,
        message: message.to_owned(),
    }
}

/// Go `metadataResponse{md: &bq.TableMetadata{Schema: schema, ETag: etag}}`.
fn md(schema: Schema, etag: &str) -> Result<TableMetadata, FakeError> {
    Ok(TableMetadata {
        schema,
        etag: etag.to_owned(),
        ..TableMetadata::default()
    })
}

/// Go `ensureTableAndSchema(context.Background(), table, cfg, nil)`.
async fn ensure(table: &MockTableOps, cfg: &Config) -> Result<(), Error> {
    ensure_table_and_schema(&Context::background(), table, cfg, &BTreeSet::new()).await
}

/// Go `errors.As(err, &apiErr) && apiErr.Code == code`.
fn has_api_code(err: &Error, code: u16) -> bool {
    matches!(err.find(&mut |e| matches!(e, Error::Api(_))), Some(Error::Api(a)) if a.code == code)
}

/// Go `bq.Schema{{Name: "timestamp", Type: bq.TimestampFieldType, Required: true}}`.
fn outdated() -> Schema {
    vec![required("timestamp", FIELD_TYPE_TIMESTAMP)]
}

#[tokio::test]
async fn test_ensure_table_and_schema_not_found_creates_table_with_version_label() {
    for disable_upgrade in [true, false] {
        let name = format!("DisableSchemaUpgrade={disable_upgrade}");
        let cfg = Config {
            disable_schema_upgrade: disable_upgrade,
            table_name: "agent_events".to_owned(),
            clustering_fields: vec!["event_type".to_owned(), "agent".to_owned()],
            ..Config::default()
        };

        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![Err(api(404, "not found"))],
            ..MockTableOpsState::default()
        });

        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.create_calls, 1, "{name}: createCalls");
        assert_eq!(st.update_calls, 0, "{name}: updateCalls");
        let created = st.created_meta.unwrap();
        assert_eq!(
            created.labels.get(SCHEMA_VERSION_KEY).map(String::as_str),
            Some(SCHEMA_VERSION),
            "{name}: created table label"
        );
        let tp = created.time_partitioning.unwrap();
        assert_eq!(tp.field, "timestamp", "{name}: TimePartitioning.Field");
        assert_eq!(
            tp.partition_type, DAY_PARTITIONING_TYPE,
            "{name}: TimePartitioning.Type"
        );
        assert_eq!(
            created.clustering.unwrap().fields,
            cfg.clustering_fields,
            "{name}: Clustering.Fields"
        );
        assert_eq!(
            created.schema.len(),
            events_schema().len(),
            "{name}: created schema field count"
        );
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_conflict_on_create_recovery() {
    {
        let name = "DisableSchemaUpgrade=true treats 409 as success without re-fetching";
        let cfg = Config {
            disable_schema_upgrade: true,
            ..Config::default()
        };
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![Err(api(404, ""))],
            create_err: Some(api(409, "already exists")),
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.metadata_calls, 1, "{name}: metadataCalls");
        assert_eq!(st.update_calls, 0, "{name}: updateCalls");
    }

    {
        let name = "default config re-fetches metadata and skips Update when clean";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![Err(api(404, "")), md(events_schema(), "etag-after-409")],
            create_err: Some(api(409, "already exists")),
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.metadata_calls, 2, "{name}: metadataCalls");
        assert_eq!(st.update_calls, 0, "{name}: updateCalls");
    }

    {
        let name = "default config re-fetches metadata and upgrades outdated schema";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![Err(api(404, "")), md(outdated(), "etag-after-409")],
            create_err: Some(api(409, "already exists")),
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.update_calls, 1, "{name}: updateCalls");
        assert_eq!(st.updates[0].1, "etag-after-409", "{name}: update etag");
    }

    {
        let name = "default config fails closed on 403 re-fetch failure after 409";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![Err(api(404, "")), Err(api(403, "access denied"))],
            create_err: Some(status(Code::AlreadyExists, "already exists")),
            ..MockTableOpsState::default()
        });
        let err = ensure(&table, &cfg).await.unwrap_err();
        assert!(
            has_api_code(&err, 403),
            "{name}: error = {err}, want wrapped 403 googleapi.Error"
        );
    }

    {
        let name = "default config propagates non-403 re-fetch failure after 409";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![
                Err(api(404, "")),
                Err(FakeError::Context(ContextError::Canceled)),
            ],
            create_err: Some(status(Code::AlreadyExists, "already exists")),
            ..MockTableOpsState::default()
        });
        let err = ensure(&table, &cfg).await.unwrap_err();
        assert!(
            err.is_context(ContextError::Canceled),
            "{name}: error = {err}, want wrapped context.Canceled"
        );
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_permission_denied_fails_closed() {
    struct Case {
        name: &'static str,
        disable_upgrade: bool,
        table: MockTableOpsState,
        want_err: bool,
    }
    let cases = [
        Case {
            name: "HTTP 403 on Metadata with DisableSchemaUpgrade=true returns error",
            disable_upgrade: true,
            table: MockTableOpsState {
                metadata_seq: vec![Err(api(403, "access denied"))],
                ..MockTableOpsState::default()
            },
            want_err: true,
        },
        Case {
            name: "gRPC PermissionDenied on Metadata with default config returns error",
            disable_upgrade: false,
            table: MockTableOpsState {
                metadata_seq: vec![Err(status(Code::PermissionDenied, "denied"))],
                ..MockTableOpsState::default()
            },
            want_err: true,
        },
        Case {
            name: "HTTP 403 on Create after 404 returns error because table is known missing",
            disable_upgrade: true,
            table: MockTableOpsState {
                metadata_seq: vec![Err(api(404, ""))],
                create_err: Some(api(403, "cannot create")),
                ..MockTableOpsState::default()
            },
            want_err: true,
        },
        Case {
            name: "HTTP 403 on Update during schema upgrade returns error because schema is known incomplete",
            disable_upgrade: false,
            table: MockTableOpsState {
                metadata_seq: vec![md(outdated(), "etag-1")],
                update_seq: vec![Err(api(403, "cannot update"))],
                ..MockTableOpsState::default()
            },
            want_err: true,
        },
        Case {
            name: "HTTP 500 on Metadata returns error",
            disable_upgrade: false,
            table: MockTableOpsState {
                metadata_seq: vec![Err(api(500, "internal"))],
                ..MockTableOpsState::default()
            },
            want_err: true,
        },
        Case {
            name: "HTTP 500 on Create after 404 returns error",
            disable_upgrade: true,
            table: MockTableOpsState {
                metadata_seq: vec![Err(api(404, ""))],
                create_err: Some(api(500, "internal")),
                ..MockTableOpsState::default()
            },
            want_err: true,
        },
    ];

    for tt in cases {
        let cfg = Config {
            disable_schema_upgrade: tt.disable_upgrade,
            ..Config::default()
        };
        let table = MockTableOps::new(tt.table);
        let res = ensure(&table, &cfg).await;
        assert_eq!(
            res.is_err(),
            tt.want_err,
            "{}: error = {res:?}, wantErr {}",
            tt.name,
            tt.want_err
        );
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_no_op_schema_does_not_call_update() {
    let mut custom: Schema = vec![field("custom_user_col", FIELD_TYPE_STRING)];
    custom.extend(events_schema().into_iter().map(|f| FieldSchema {
        name: f.name.to_uppercase(),
        ..f
    }));
    let nullable_ts: Schema = events_schema()
        .into_iter()
        .map(|f| {
            if f.name == "timestamp" {
                FieldSchema {
                    required: false,
                    ..f
                }
            } else {
                f
            }
        })
        .collect();

    type Mutate = fn(&mut Config);
    let cases: [(&str, TableMetadata, Option<Mutate>); 5] = [
        (
            "exact schema with nil labels does not call Update",
            TableMetadata {
                schema: events_schema(),
                etag: "etag-clean".to_owned(),
                ..TableMetadata::default()
            },
            None,
        ),
        (
            "exact schema with stale adk_schema_version label does not call Update",
            TableMetadata {
                schema: events_schema(),
                labels: BTreeMap::from([(SCHEMA_VERSION_KEY.to_owned(), "1".to_owned())]),
                etag: "etag-stale-label".to_owned(),
                ..TableMetadata::default()
            },
            None,
        ),
        (
            "case-insensitive field names plus extra user column does not call Update",
            TableMetadata {
                schema: custom,
                etag: "etag-custom".to_owned(),
                ..TableMetadata::default()
            },
            None,
        ),
        (
            "outdated schema with DisableSchemaUpgrade=true does not call Update",
            TableMetadata {
                schema: vec![field("timestamp", FIELD_TYPE_TIMESTAMP)],
                etag: "etag-disable-flag".to_owned(),
                ..TableMetadata::default()
            },
            Some(|c: &mut Config| c.disable_schema_upgrade = true),
        ),
        (
            "existing table with NULLABLE timestamp and DisableSchemaUpgrade=false does not call Update",
            TableMetadata {
                schema: nullable_ts,
                etag: "etag-nullable-ts".to_owned(),
                ..TableMetadata::default()
            },
            None,
        ),
    ];

    for (name, md, mutate) in cases {
        let mut cfg = Config::default();
        if let Some(mutate) = mutate {
            mutate(&mut cfg);
        }
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![Ok(md)],
            ..MockTableOpsState::default()
        });
        if let Err(err) = ensure(&table, &cfg).await {
            panic!("{name}: ensureTableAndSchema() error = {err}");
        }
        assert_eq!(
            table.state().update_calls,
            0,
            "{name}: must never call Update when schema is unchanged or upgrade is disabled"
        );
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_additive_top_level_and_nested_record_upgrade() {
    // A legacy v1 table schema that:
    // 1. Has a custom column at index 0 ("custom_tenant_id") to verify order preservation.
    // 2. Lacks top-level "event_id".
    // 3. Has "content_parts" RECORD, but without nested "object_ref" or "storage_mode".
    let mut legacy_schema: Schema = vec![field("custom_tenant_id", FIELD_TYPE_STRING)];
    for f in events_schema() {
        if f.name == "event_id" {
            continue;
        }
        if f.name == "content_parts" {
            legacy_schema.push(FieldSchema {
                schema: vec![
                    field("mime_type", FIELD_TYPE_STRING),
                    field("uri", FIELD_TYPE_STRING),
                    field("text", FIELD_TYPE_STRING),
                    field("part_index", FIELD_TYPE_INTEGER),
                    field("part_attributes", FIELD_TYPE_STRING),
                ],
                ..f
            });
            continue;
        }
        legacy_schema.push(f);
    }

    let cfg = Config::default();
    let table = MockTableOps::new(MockTableOpsState {
        metadata_seq: vec![md(legacy_schema, "etag-v1")],
        ..MockTableOpsState::default()
    });

    ensure(&table, &cfg).await.unwrap();
    let st = table.state();
    assert_eq!(st.update_calls, 1, "updateCalls");

    let (update, etag) = &st.updates[0];
    assert_eq!(etag, "etag-v1");

    let updated_schema = update.schema.as_ref().unwrap();
    // Custom column at index 0 must be preserved.
    assert_eq!(updated_schema[0].name, "custom_tenant_id");

    // Missing top-level "event_id" must be appended at the end as NULLABLE.
    let last_field = updated_schema.last().unwrap();
    assert!(
        last_field.name == "event_id"
            && last_field.field_type == FIELD_TYPE_STRING
            && !last_field.required,
        "appended field = {last_field:?}, want NULLABLE STRING event_id"
    );

    // "content_parts" must have been updated in-place with nested "object_ref" and "storage_mode".
    let content_parts = updated_schema
        .iter()
        .find(|f| f.name == "content_parts")
        .expect("content_parts missing from updatedSchema");
    let sub_names: BTreeMap<&str, &FieldSchema> = content_parts
        .schema
        .iter()
        .map(|sf| (sf.name.as_str(), sf))
        .collect();
    let obj_ref = sub_names.get("object_ref");
    let obj_ref = match obj_ref {
        Some(f) if f.field_type == FIELD_TYPE_RECORD && !f.required => *f,
        _ => panic!("content_parts.object_ref = {obj_ref:?}, want NULLABLE RECORD"),
    };
    assert_eq!(obj_ref.schema.len(), 4, "len(object_ref.Schema)");
    for rf in &obj_ref.schema {
        assert!(
            !rf.required,
            "object_ref.{} Required = true, want false",
            rf.name
        );
    }
    let sm = sub_names.get("storage_mode");
    assert!(
        matches!(sm, Some(f) if f.field_type == FIELD_TYPE_STRING && !f.required),
        "content_parts.storage_mode = {sm:?}, want NULLABLE STRING"
    );
}

#[tokio::test]
async fn test_ensure_table_and_schema_precondition_failed_retries_with_fresh_e_tag() {
    {
        let name = "succeeds on retry with fresh ETag";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(outdated(), "etag-1"), md(outdated(), "etag-2")],
            update_seq: vec![
                Err(api(412, "etag mismatch")),
                md(events_schema(), "etag-3"),
            ],
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.metadata_calls, 2, "{name}: metadataCalls");
        assert_eq!(st.update_calls, 2, "{name}: updateCalls");
        let etags: Vec<&str> = st.updates.iter().map(|(_, e)| e.as_str()).collect();
        assert_eq!(etags, ["etag-1", "etag-2"], "{name}: update etags");
    }

    {
        let name = "stops without second Update when winning replica already upgraded schema";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(outdated(), "etag-1"), md(events_schema(), "etag-2")],
            update_seq: vec![Err(status(Code::FailedPrecondition, "etag mismatch"))],
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.metadata_calls, 2, "{name}: metadataCalls");
        assert_eq!(st.update_calls, 1, "{name}: updateCalls");
    }

    {
        let name = "returns error after exhausting 2 precondition retries";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![
                md(outdated(), "etag-1"),
                md(outdated(), "etag-2"),
                md(outdated(), "etag-3"),
            ],
            update_seq: vec![Err(api(412, "")), Err(api(412, "")), Err(api(412, ""))],
            ..MockTableOpsState::default()
        });
        let res = ensure(&table, &cfg).await;
        assert!(
            res.is_err(),
            "{name}: want precondition error after exhausting retries"
        );
        assert_eq!(
            table.state().update_calls,
            3,
            "{name}: updateCalls (1 initial + 2 retries)"
        );
    }

    {
        let name = "returns error on 403 metadata re-fetch failure after 412";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(outdated(), "etag-1"), Err(api(403, ""))],
            update_seq: vec![Err(api(412, ""))],
            ..MockTableOpsState::default()
        });
        let err = ensure(&table, &cfg).await.unwrap_err();
        assert!(
            has_api_code(&err, 403),
            "{name}: error = {err}, want wrapped 403 googleapi.Error"
        );
    }

    {
        let name = "returns error on non-403 metadata re-fetch failure after 412";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(outdated(), "etag-1"), Err(api(500, ""))],
            update_seq: vec![Err(api(412, ""))],
            ..MockTableOpsState::default()
        });
        let res = ensure(&table, &cfg).await;
        assert!(res.is_err(), "{name}: want re-fetch error");
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_incompatible_schema_returns_error() {
    let content_parts = |sub: FieldSchema| FieldSchema {
        repeated: true,
        schema: vec![sub],
        ..field("content_parts", FIELD_TYPE_RECORD)
    };
    let cases: [(&str, Schema, &str); 5] = [
        (
            "top-level field type mismatch",
            vec![
                required("timestamp", FIELD_TYPE_TIMESTAMP),
                field("event_id", FIELD_TYPE_INTEGER),
            ],
            "incompatible BigQuery schema field event_id: existing=INTEGER/NULLABLE, desired=STRING/NULLABLE",
        ),
        (
            "top-level field repeated mismatch",
            vec![
                required("timestamp", FIELD_TYPE_TIMESTAMP),
                field("content_parts", FIELD_TYPE_RECORD),
            ],
            "incompatible BigQuery schema field content_parts: existing=RECORD/NULLABLE, desired=RECORD/REPEATED",
        ),
        (
            "top-level REQUIRED field where writer allows NULL",
            vec![
                required("timestamp", FIELD_TYPE_TIMESTAMP),
                required("content", FIELD_TYPE_JSON),
            ],
            "incompatible BigQuery schema field content: existing=JSON/REQUIRED, desired=JSON/NULLABLE",
        ),
        (
            "nested RECORD subfield type mismatch",
            vec![
                required("timestamp", FIELD_TYPE_TIMESTAMP),
                content_parts(field("part_index", FIELD_TYPE_STRING)),
            ],
            "incompatible BigQuery schema field content_parts.part_index: existing=STRING/NULLABLE, desired=INTEGER/NULLABLE",
        ),
        (
            "nested RECORD subfield REQUIRED where writer allows NULL",
            vec![
                required("timestamp", FIELD_TYPE_TIMESTAMP),
                content_parts(required("mime_type", FIELD_TYPE_STRING)),
            ],
            "incompatible BigQuery schema field content_parts.mime_type: existing=STRING/REQUIRED, desired=STRING/NULLABLE",
        ),
    ];

    for (name, schema, want) in cases {
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(schema, "etag-bad")],
            ..MockTableOpsState::default()
        });
        let err = match ensure(&table, &cfg).await {
            Ok(()) => {
                panic!("{name}: error = nil, want incompatible schema error containing {want:?}")
            }
            Err(err) => err,
        };
        assert_eq!(table.state().update_calls, 0, "{name}: updateCalls");
        assert!(
            err.to_string().contains(want),
            "{name}: error {:?} does not contain {want:?}",
            err.to_string()
        );
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_struct_literal_config_defaults_to_auto_schema_upgrade() {
    {
        let name =
            "struct literal Config without DefaultConfig upgrades outdated schema by default";
        let cfg = Config {
            project_id: "p".to_owned(),
            dataset_id: "d".to_owned(),
            table_name: "t".to_owned(),
            enabled: true,
            ..Config::zero()
        }
        .apply_defaults();
        assert!(
            !cfg.disable_schema_upgrade,
            "{name}: cfg.DisableSchemaUpgrade = true for struct-literal Config{{}}, want false"
        );
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(outdated(), "etag-struct")],
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        assert_eq!(table.state().update_calls, 1, "{name}: updateCalls");
    }

    {
        let name = "struct literal Config with DisableSchemaUpgrade=true skips upgrade";
        let cfg = Config {
            project_id: "p".to_owned(),
            dataset_id: "d".to_owned(),
            table_name: "t".to_owned(),
            enabled: true,
            disable_schema_upgrade: true,
            ..Config::zero()
        }
        .apply_defaults();
        assert!(
            cfg.disable_schema_upgrade,
            "{name}: cfg.DisableSchemaUpgrade = false when DisableSchemaUpgrade=true, want true"
        );
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(outdated(), "etag-struct")],
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        assert_eq!(table.state().update_calls, 0, "{name}: updateCalls");
    }

    {
        let name = "missing REQUIRED field timestamp is appended as NULLABLE";
        let cfg = Config::default();
        let table = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![md(
                vec![field("event_id", FIELD_TYPE_STRING)],
                "etag-missing-ts",
            )],
            ..MockTableOpsState::default()
        });
        ensure(&table, &cfg).await.unwrap();
        let st = table.state();
        assert_eq!(st.update_calls, 1, "{name}: updateCalls");
        let ts_field = st.updates[0]
            .0
            .schema
            .as_ref()
            .unwrap()
            .iter()
            .find(|f| f.name == "timestamp")
            .cloned();
        let Some(ts_field) = ts_field else {
            panic!("{name}: timestamp field missing from upgraded schema");
        };
        assert!(
            !ts_field.required,
            "{name}: appended timestamp field has Required = true, want false (BigQuery forbids adding REQUIRED columns)"
        );
    }
}

#[tokio::test]
async fn test_ensure_table_and_schema_incompatible_schema_error_wraps_table_name() {
    let cfg = Config {
        table_name: "my_custom_table".to_owned(),
        ..Config::default()
    };
    let table = MockTableOps::new(MockTableOpsState {
        metadata_seq: vec![md(
            vec![field("event_id", FIELD_TYPE_INTEGER)],
            "etag-incompatible",
        )],
        ..MockTableOpsState::default()
    });
    let err = ensure(&table, &cfg).await.unwrap_err();
    let want_substr = "incompatible BigQuery table my_custom_table schema";
    assert!(
        err.to_string().contains(want_substr),
        "error = {:?}, want substring {want_substr:?}",
        err.to_string()
    );
}

#[test]
fn test_upgrade_table_schema_existing_nullable_desired_required() {
    let existing = vec![field("timestamp", FIELD_TYPE_TIMESTAMP)];
    let desired = vec![required("timestamp", FIELD_TYPE_TIMESTAMP)];
    let (merged, changed) = upgrade_table_schema(&existing, &desired).unwrap();
    assert!(
        !changed,
        "upgradeTableSchema() changed = true, want false (existing column preserved as-is)"
    );
    assert!(
        merged.len() == 1 && !merged[0].required,
        "merged schema = {merged:?}, want existing NULLABLE column preserved"
    );
}

#[test]
fn test_upgrade_table_schema_case_insensitive_nested_record_lookup() {
    let existing = vec![FieldSchema {
        repeated: true,
        schema: vec![field("Part_Index", FIELD_TYPE_INTEGER)],
        ..field("Content_Parts", FIELD_TYPE_RECORD)
    }];
    let desired = vec![FieldSchema {
        repeated: true,
        schema: vec![
            field("part_index", FIELD_TYPE_INTEGER),
            field("new_subfield", FIELD_TYPE_STRING),
        ],
        ..field("content_parts", FIELD_TYPE_RECORD)
    }];
    let (merged, changed) = upgrade_table_schema(&existing, &desired).unwrap();
    assert!(
        changed,
        "upgradeTableSchema() changed = false, want true (new_subfield added)"
    );
    assert!(
        merged.len() == 1 && merged[0].schema.len() == 2,
        "merged subfields count = {}, want 2",
        merged.first().map_or(0, |f| f.schema.len())
    );
}
