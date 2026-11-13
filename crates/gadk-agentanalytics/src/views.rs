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

//! Port of ADK-Go `plugin/agentanalytics/views.go` without the plugin state (7b-3): the
//! per-event-type view definitions ([`Config::view_definitions`]), the DDL runner
//! ([`ViewRunner`], [`BigQueryViewRunner`]) and [`sync_analytics_views`].

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::BTreeSet;
use std::sync::Weak;
use std::time::Duration;

use crate::bigquery::BigQueryClient;
use crate::config::{Config, DEFAULT_VIEW_PREFIX, Logger};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error, is_conflict_error, status_code};
use crate::redaction::parse_payload_column_denylist;

/// Go `viewDDLTimeout`: each view's DDL gets its own deadline under the pass's context.
pub(crate) const VIEW_DDL_TIMEOUT: Duration = Duration::from_secs(30);

/// Go `maxBigQueryIdentifierLen`: the longest dataset ID, and table or view name, in bytes.
pub(crate) const MAX_BIGQUERY_IDENTIFIER_LEN: usize = 1024;

/// A BigQuery view over the events table for one event type (Go `ViewDefinition`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDefinition {
    /// The unqualified view name (for example `v_llm_request`).
    pub name: String,
    /// The `event_type` value the view filters on.
    pub event_type: String,
    /// The complete `CREATE OR REPLACE VIEW` statement.
    pub query: String,
}

/// Go `viewCommonColumns`: selected by every view (adk-python `_VIEW_COMMON_COLUMNS`).
pub(crate) const VIEW_COMMON_COLUMNS: [&str; 13] = [
    "timestamp",
    "event_id",
    "event_type",
    "agent",
    "session_id",
    "invocation_id",
    "user_id",
    "trace_id",
    "span_id",
    "parent_span_id",
    "status",
    "error_message",
    "is_truncated",
];

/// Go `eventViewSpec`: one view's event type and its extra column expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventViewSpec {
    /// Go `eventType`.
    pub(crate) event_type: &'static str,
    /// Go `extraCols`: `<expr> AS <name>` SQL expressions.
    pub(crate) extra_cols: &'static [&'static str],
}

/// Go `eventViewSpecs`: the 25 views in Go's (= adk-python `_EVENT_VIEW_DEFS`) order.
// Task 6 fills in the 25 specs (verbatim from Go).
pub(crate) static EVENT_VIEW_SPECS: &[EventViewSpec] = &[];

impl Config {
    /// Go `Config.ViewDefinitions()`: the `CREATE OR REPLACE VIEW` statements for this config,
    /// one per view in [`EVENT_VIEW_SPECS`] order, honoring `view_prefix` (empty means `"v"`)
    /// and `payload_column_denylist`. Errors for an invalid identifier or a view name equal
    /// (case-insensitively) to the table name.
    pub fn view_definitions(&self) -> Result<Vec<ViewDefinition>, Error> {
        todo!("Task 6")
    }
}

/// Go `viewDefinitionsWithDenied(projectID, datasetID, tableName, viewPrefix, deniedColumns)`.
pub(crate) fn view_definitions_with_denied(
    project_id: &str,
    dataset_id: &str,
    table_name: &str,
    view_prefix: &str,
    denied_columns: &BTreeSet<String>,
) -> Result<Vec<ViewDefinition>, Error> {
    todo!("Task 6")
}

/// Go `projectViewColumns`: drops the extra expressions that reference a denied column.
pub(crate) fn project_view_columns(
    extra_cols: &[&'static str],
    denied_columns: &BTreeSet<String>,
) -> Vec<&'static str> {
    todo!("Task 6")
}

/// Go `referencesDeniedColumn`.
pub(crate) fn references_denied_column(expr: &str, denied_columns: &BTreeSet<String>) -> bool {
    todo!("Task 6")
}

/// Go `stripSQLStringLiterals`.
pub(crate) fn strip_sql_string_literals(s: &str) -> String {
    todo!("Task 6")
}

/// Go `containsIdentifierToken`.
pub(crate) fn contains_identifier_token(s: &str, ident: &str) -> bool {
    todo!("Task 6")
}

/// Go `isIdentChar`.
pub(crate) fn is_ident_char(b: u8) -> bool {
    todo!("Task 6")
}

/// Go `isValidProjectID`.
pub(crate) fn is_valid_project_id(s: &str) -> bool {
    todo!("Task 6")
}

/// Go `isValidIdentifier`.
pub(crate) fn is_valid_identifier(s: &str) -> bool {
    todo!("Task 6")
}

/// Go `isValidDatasetID`.
pub(crate) fn is_valid_dataset_id(s: &str) -> bool {
    todo!("Task 6")
}

/// Go `isValidTableName` (Unicode letters and digits, `_`, `-`, space).
pub(crate) fn is_valid_table_name(s: &str) -> bool {
    todo!("Task 6")
}

/// Go `validateViewIdentifiers`.
pub(crate) fn validate_view_identifiers(
    project_id: &str,
    dataset_id: &str,
    table_name: &str,
    view_prefix: &str,
) -> Result<(), Error> {
    todo!("Task 6")
}

/// Runs one view's DDL (Go `viewRunner`), so view creation can be tested without BigQuery.
#[async_trait::async_trait]
pub trait ViewRunner: Send + Sync {
    /// Go `RunViewDDL(ctx, viewName, query)`.
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error>;
}

/// The BigQuery DDL runner (Go `bqViewRunner`). The client is weak (Go `weak.Pointer`): the
/// plugin keeps it alive; once it is dropped every run fails with Go's "client is not
/// configured" error.
#[derive(Debug, Clone)]
pub struct BigQueryViewRunner {
    pub(crate) client: Weak<BigQueryClient>,
    pub(crate) project_id: String,
    pub(crate) dataset_id: String,
}

impl BigQueryViewRunner {
    /// A runner over `client` for views in `project_id.dataset_id` (an empty project means the
    /// client's project, Go `client.Dataset`).
    pub fn new(
        client: Weak<BigQueryClient>,
        project_id: impl Into<String>,
        dataset_id: impl Into<String>,
    ) -> BigQueryViewRunner {
        BigQueryViewRunner {
            client,
            project_id: project_id.into(),
            dataset_id: dataset_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl ViewRunner for BigQueryViewRunner {
    /// Go `bqViewRunner.RunViewDDL`: an existing view with the same query (trimmed) is left
    /// alone; an existing table of another type is refused (`cannot replace existing BigQuery
    /// <TYPE> "<view>" with a view`); otherwise a `CREATE OR REPLACE VIEW` query job runs and is
    /// awaited, and its error returned.
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error> {
        todo!("Task 9")
    }
}

/// Go `syncAnalyticsViews(ctx, runner, defs, logf)`: runs every definition (each under its own
/// [`VIEW_DDL_TIMEOUT`]); a conflict counts as created; stops when `ctx` ends; joins the errors.
/// `logf` (Go's non-nil `logf`, a non-default logger) gets each failure that is not a
/// cancellation and the pass summary.
pub async fn sync_analytics_views(
    ctx: &Context,
    runner: &dyn ViewRunner,
    defs: &[ViewDefinition],
    logf: Option<&dyn Logger>,
) -> Result<(), Error> {
    todo!("Task 9")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn runner_new_keeps_a_weak_client() {
        let runner = BigQueryViewRunner::new(Weak::new(), "p", "d");
        assert!(runner.client.upgrade().is_none());
        assert_eq!(runner.project_id, "p");
        assert_eq!(runner.dataset_id, "d");
        let _: Arc<dyn ViewRunner> = Arc::new(runner);
        assert_eq!(VIEW_COMMON_COLUMNS.len(), 13);
    }
}
