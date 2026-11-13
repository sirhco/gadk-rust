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

//! Port of ADK-Go `plugin/agentanalytics/schema_upgrade.go`: the [`TableMetadataOps`]
//! abstraction over the table metadata RPCs (Go `tableMetadataOps`; the REST implementation is
//! [`TableOps`](crate::bigquery::TableOps), Go's `bqTableAdapter`), table creation and the
//! additive schema upgrade ([`ensure_table_and_schema`], [`maybe_upgrade_schema`]). Go's
//! `hasErrorCode` and the `is*Error` classifiers live in [`crate::error`].
//!
//! [`TableMetadata`] / [`TableMetadataUpdate`] are the parts of Go's `bigquery.TableMetadata` /
//! `bigquery.TableMetadataToUpdate` the package reads or writes.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::config::{Config, SCHEMA_VERSION, SCHEMA_VERSION_KEY};
use crate::context::Context;
use crate::error::{Error, is_conflict_error, is_not_found_error, is_precondition_failed_error};
use crate::schema::{
    FieldSchema, Schema, TIMESTAMP_FIELD, project_events_schema, upgrade_table_schema,
};

/// Go `maxPreconditionRetries`: re-reads after an etag precondition failure.
pub(crate) const MAX_PRECONDITION_RETRIES: usize = 2;

/// Go `bigquery.DayPartitioningType`.
pub const DAY_PARTITIONING_TYPE: &str = "DAY";
/// Go `bigquery.ViewTable` (`TableMetadata.Type` of a logical view).
pub const VIEW_TABLE_TYPE: &str = "VIEW";

/// Go `bigquery.TimePartitioning`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimePartitioning {
    /// Go `Type` (`DAY`, `HOUR`, …).
    pub partition_type: String,
    /// Go `Expiration` (zero: none).
    pub expiration: Duration,
    /// Go `Field`.
    pub field: String,
    /// Go `RequirePartitionFilter`.
    pub require_partition_filter: bool,
}

/// Go `bigquery.Clustering`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clustering {
    /// Go `Fields`.
    pub fields: Vec<String>,
}

/// The table metadata the package reads and creates (Go `bigquery.TableMetadata` subset).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableMetadata {
    /// Go `Schema`.
    pub schema: Schema,
    /// Go `TimePartitioning`.
    pub time_partitioning: Option<TimePartitioning>,
    /// Go `Clustering`.
    pub clustering: Option<Clustering>,
    /// Go `Labels`.
    pub labels: BTreeMap<String, String>,
    /// Go `ETag` (read-only).
    pub etag: String,
    /// Go `Type` (`TABLE`, `VIEW`, `MATERIALIZED_VIEW`, …; read-only; empty when unknown).
    pub table_type: String,
    /// Go `ViewQuery`.
    pub view_query: String,
}

/// A metadata update (Go `bigquery.TableMetadataToUpdate` subset: `Schema` and `SetLabel`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableMetadataUpdate {
    /// Go `Schema` (`None`: unchanged).
    pub schema: Option<Schema>,
    /// The labels set by Go `SetLabel`.
    pub set_labels: BTreeMap<String, String>,
}

impl TableMetadataUpdate {
    /// Go `SetLabel(name, value)`.
    pub fn set_label(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.set_labels.insert(name.into(), value.into());
    }
}

/// The table metadata RPCs (Go `tableMetadataOps`), so startup and schema-upgrade paths can be
/// tested without BigQuery.
#[async_trait::async_trait]
pub trait TableMetadataOps: Send + Sync {
    /// Go `Metadata(ctx)` (`tables.get`).
    async fn metadata(&self, ctx: &Context) -> Result<TableMetadata, Error>;
    /// Go `Create(ctx, meta)` (`tables.insert`).
    async fn create(&self, ctx: &Context, meta: &TableMetadata) -> Result<(), Error>;
    /// Go `Update(ctx, update, etag)` (`tables.patch` with `If-Match: etag` when non-empty).
    async fn update(
        &self,
        ctx: &Context,
        update: &TableMetadataUpdate,
        etag: &str,
    ) -> Result<TableMetadata, Error>;
}

/// Go `ensureTableAndSchema`: creates the table when it is missing (DAY partitioning on
/// `timestamp`, `config.clustering_fields`, the schema-version label; a create conflict re-reads
/// and upgrades unless `disable_schema_upgrade`), else upgrades the schema additively unless
/// `disable_schema_upgrade`. `denied` are the payload columns left out of the table.
pub async fn ensure_table_and_schema(
    ctx: &Context,
    table: &dyn TableMetadataOps,
    config: &Config,
    denied: &BTreeSet<String>,
) -> Result<(), Error> {
    todo!("Task 8")
}

/// Go `maybeUpgradeSchema`: adds the missing columns of `desired` with an etag-guarded update
/// (re-reading after a precondition failure up to [`MAX_PRECONDITION_RETRIES`] times), writing
/// nothing when every column already matches.
pub async fn maybe_upgrade_schema(
    ctx: &Context,
    table: &dyn TableMetadataOps,
    md: TableMetadata,
    config: &Config,
    desired: &[FieldSchema],
) -> Result<(), Error> {
    todo!("Task 8")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn set_label_records_labels() {
        let mut update = TableMetadataUpdate::default();
        update.set_label(SCHEMA_VERSION_KEY, SCHEMA_VERSION);
        assert_eq!(
            update
                .set_labels
                .get("adk_schema_version")
                .map(String::as_str),
            Some("2")
        );
        assert!(update.schema.is_none());
    }
}
