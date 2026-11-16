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

//! gadk-agentanalytics (sub-projects 7b-1, 7b-2, 7b-3a and 7b-3b): a port of ADK-Go's
//! `plugin/agentanalytics`, the BigQuery Agent Analytics plugin.
//!
//! The plugin ([`plugin`], 7b-3a) is a [`gadk_agent::Plugin`]: add it to a runner's plugin list
//! and every user message, invocation, agent, model call, tool call and session event becomes a
//! row of the BigQuery events table, written through the BigQuery Storage Write API.
//! [`new_bigquery_agent_analytics_plugin`] (Go `NewBigQueryAgentAnalyticsPlugin`) takes a
//! project, dataset and table with the default [`Config`];
//! [`new_bigquery_agent_analytics_plugin_with_config`] a whole [`Config`] (a disabled config
//! gives Go's disabled plugin, which only closes); and
//! [`new_bigquery_agent_analytics_plugin_with_clients`] caller-made clients (a
//! [`BigQueryClient`] for table setup and views, a [`WriteClient`] for the rows). Construction
//! validates the config with Go's texts, creates or upgrades the table and starts the views in
//! the background. [`as_bigquery_agent_analytics_plugin`] recovers a constructed plugin's helpers
//! ([`BigQueryAgentAnalyticsPlugin::drop_stats`],
//! [`BigQueryAgentAnalyticsPlugin::sync_views`]); the plugin's `close` (run by the runner's
//! plugin manager) stops the views, drains the batch processor on a task of its own and closes
//! the clients, once. Callbacks never fail a run: a panic while building a row is recovered and
//! counted as a dropped row. Rows carry trace and span IDs from the crate's trace manager (Go's
//! `trace_manager.go`, with its goroutine attribution of workflow nodes, parallel branches and
//! worker items read from the callback's `gadk_agent::fiber` (7b-3b)) and the session-event
//! signal rows of Go's `events.go`.
//!
//! The foundations (7b-1): configuration ([`Config`]), the events table's BigQuery and Arrow
//! schemas, table creation and additive schema upgrade, the per-event-type analytics views, the
//! content formatter and redaction, and a BigQuery REST admin client. The transports (7b-2): rows
//! ([`row`]), the generated BigQuery Storage Write API ([`storagepb`]), the gapic write client,
//! Arrow record batches and the stream writer ([`write`](mod@crate::write)), the [`BatchProcessor`]
//! ([`batch_processor`]) and the GCS content offloader with its upload orchestration
//! ([`offload`]).
//!
//! Exported Go entry points: the plugin constructors above, `AsBigQueryAgentAnalyticsPlugin`,
//! `DefaultConfig` ([`Config::default`]), `EventsSchema` ([`events_schema`]), `ArrowSchema`
//! ([`arrow_schema`](fn@arrow_schema)), `SerializedArrowSchema` ([`serialized_arrow_schema`]),
//! `Config.ViewDefinitions` ([`Config::view_definitions`]), `FormatContentParts`
//! ([`format_content_parts`]), `SmartTruncate` ([`smart_truncate`]) and the `Event*` row types
//! ([`event_types`]). Table creation and schema upgrade are [`ensure_table_and_schema`] over
//! [`TableOps`]; view creation is [`sync_analytics_views`] over a [`BigQueryViewRunner`]. The
//! plugin's unexported transports are public here: [`WriteClient`] (Go's gapic
//! `BigQueryWriteClient`), [`BatchProcessor`] and [`GcsOffloader`].
//!
//! The `gadk` façade re-exports this crate as `gadk::agentanalytics` (feature `agentanalytics`).
//! Feature `testutil` adds the in-memory fakes of `bigquery::fake`, the in-process BigQuery
//! Write server `testutil::fake_write_server`, the Arrow row decoder `testutil::arrow_rows` and
//! the probe's row specs. Behavior matches ADK-Go except for the accepted divergences, each
//! annotated where it lives (`// Divergence`).

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

pub(crate) mod formatter;
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
