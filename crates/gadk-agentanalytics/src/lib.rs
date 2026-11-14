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

//! gadk-agentanalytics (sub-projects 7b-1 and 7b-2): a port of ADK-Go's `plugin/agentanalytics`
//! (the BigQuery Agent Analytics plugin) without the plugin itself. 7b-1: configuration
//! ([`Config`]), the events table's BigQuery and Arrow schemas, table creation and additive
//! schema upgrade, the per-event-type analytics views, the content formatter and redaction, and
//! a BigQuery REST admin client. 7b-2: rows ([`row`]), the generated BigQuery Storage Write API
//! ([`storagepb`]), the gapic write client, Arrow record batches and the stream writer
//! ([`write`]), the [`BatchProcessor`] ([`batch_processor`]) and the GCS content offloader with
//! its upload orchestration ([`offload`]). The plugin, events and trace manager (7b-3) build on
//! it.
//!
//! Exported Go entry points: `DefaultConfig` ([`Config::default`]), `EventsSchema`
//! ([`events_schema`]), `ArrowSchema` ([`arrow_schema`]), `SerializedArrowSchema`
//! ([`serialized_arrow_schema`]), `Config.ViewDefinitions` ([`Config::view_definitions`]),
//! `FormatContentParts` ([`format_content_parts`]) and `SmartTruncate` ([`smart_truncate`]).
//! Table creation and schema upgrade are [`ensure_table_and_schema`] over [`TableOps`]; view
//! creation is [`sync_analytics_views`] over a [`BigQueryViewRunner`]. The plugin's unexported
//! transports are public here for 7b-3's constructors: [`WriteClient`] (Go's gapic
//! `BigQueryWriteClient`), [`BatchProcessor`] and [`GcsOffloader`].
//!
//! The `gadk` façade re-exports this crate as `gadk::agentanalytics` (feature `agentanalytics`).
//! Feature `testutil` adds the in-memory fakes of `bigquery::fake`, the in-process BigQuery
//! Write server `testutil::fake_write_server` and the probe's row specs. Behavior matches
//! ADK-Go except for the divergences listed in `docs/parity/7b-1.md` and `docs/parity/7b-2.md`,
//! each annotated where it lives.

#![forbid(unsafe_code)]

pub mod batch_processor;
pub mod bigquery;
pub mod config;
pub mod context;
pub mod error;
pub mod event_types;
pub mod offload;
pub mod plugin;
pub mod row;
pub mod schema;
pub mod schema_upgrade;
pub mod storagepb;
pub mod views;
pub mod write;

pub(crate) mod crc32c;
pub(crate) mod events;
pub(crate) mod gax;
pub(crate) mod trace;
pub(crate) mod validate;

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

// The plugin-side entry points of `formatter` and `redaction` (content summaries, value
// sanitizing, `apply_content_formatter`, the custom-metadata filter) are consumed by 7b-3
// (events, plugin); until then only tests reach them.
#[allow(dead_code)]
pub(crate) mod formatter;
#[allow(dead_code)]
pub(crate) mod redaction;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use batch_processor::BatchProcessor;
pub use bigquery::{BigQueryClient, TableOps};
pub use config::{
    ClientOptions, Config, ContentFormatter, DropStats, GcsContentOffloader, GcsUploadRequest,
    Logger, RetryConfig, SCHEMA_VERSION, StdLogger,
};
pub use context::{AfterFunc, Context, ContextError, Metadata};
pub use error::{ApiError, BigQueryError, BoxError, Code, Error, ErrorItem};
pub use event_types::*;
pub use formatter::{format_content_parts, smart_truncate};
pub use offload::gcs::GcsOffloader;
pub use plugin::{
    BigQueryAgentAnalyticsPlugin, as_bigquery_agent_analytics_plugin,
    new_bigquery_agent_analytics_plugin, new_bigquery_agent_analytics_plugin_with_clients,
    new_bigquery_agent_analytics_plugin_with_config,
};
pub use row::{QueuedRow, Row, RowExt, RowValue};
pub use schema::{
    FieldSchema, Schema, SchemaMismatchError, TableSchema, arrow_schema, events_schema,
    project_arrow_schema, project_events_schema, serialized_arrow_schema, upgrade_table_schema,
};
pub use schema_upgrade::{
    TableMetadata, TableMetadataOps, TableMetadataUpdate, ensure_table_and_schema,
    maybe_upgrade_schema,
};
pub use views::{BigQueryViewRunner, ViewDefinition, ViewRunner, sync_analytics_views};
pub use write::gapic::WriteClient;
