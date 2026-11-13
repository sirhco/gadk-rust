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

//! gadk-agentanalytics (sub-project 7b-1): the foundations of a port of ADK-Go's
//! `plugin/agentanalytics` (the BigQuery Agent Analytics plugin) — configuration ([`Config`]),
//! the events table's BigQuery and Arrow schemas, table creation and additive schema upgrade,
//! the per-event-type analytics views, the content formatter and redaction, and a BigQuery REST
//! admin client. The Storage Write stream, batch processor and GCS offloader (7b-2) and the
//! plugin itself (7b-3) build on it.
//!
//! Exported Go entry points: `DefaultConfig` ([`Config::default`]), `EventsSchema`
//! ([`events_schema`]), `ArrowSchema` ([`arrow_schema`]), `SerializedArrowSchema`
//! ([`serialized_arrow_schema`]), `Config.ViewDefinitions` ([`Config::view_definitions`]),
//! `FormatContentParts` ([`format_content_parts`]) and `SmartTruncate` ([`smart_truncate`]).

#![forbid(unsafe_code)]
// Task 10 removes this once every crate-internal item has its consumer.
#![allow(dead_code)]

pub mod bigquery;
pub mod config;
pub mod context;
pub mod error;
pub mod event_types;
pub mod schema;
pub mod schema_upgrade;
pub mod views;

pub(crate) mod formatter;
pub(crate) mod redaction;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use bigquery::{BigQueryClient, TableOps};
pub use config::{
    ClientOptions, Config, ContentFormatter, DropStats, GcsContentOffloader, GcsUploadRequest,
    Logger, RetryConfig, SCHEMA_VERSION, StdLogger,
};
pub use context::{Context, ContextError};
pub use error::{ApiError, BigQueryError, BoxError, Code, Error, ErrorItem};
pub use event_types::*;
pub use formatter::{format_content_parts, smart_truncate};
pub use schema::{
    FieldSchema, Schema, SchemaMismatchError, TableSchema, arrow_schema, events_schema,
    project_arrow_schema, project_events_schema, serialized_arrow_schema, upgrade_table_schema,
};
pub use schema_upgrade::{
    TableMetadata, TableMetadataOps, TableMetadataUpdate, ensure_table_and_schema,
    maybe_upgrade_schema,
};
pub use views::{BigQueryViewRunner, ViewDefinition, ViewRunner, sync_analytics_views};
