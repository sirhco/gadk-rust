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

//! Configuration of the BigQuery toolset.

use std::collections::BTreeMap;

/// What `execute_sql` may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WriteMode {
    /// Only `SELECT` statements, checked with a dry run before running (the default).
    #[default]
    Blocked,
    /// `SELECT` statements, and writes to the temporary tables of a BigQuery session that
    /// belongs to the agent session. A BigQuery session is bound to one location, so set
    /// [`BigQueryToolConfig::location`] when the data is outside the `US` multi-region.
    Protected,
    /// Any statement the credentials allow.
    Allowed,
}

/// Configuration of a [`BigQueryToolset`](crate::BigQueryToolset).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BigQueryToolConfig {
    /// What `execute_sql` may run (default [`WriteMode::Blocked`]).
    pub write_mode: WriteMode,
    /// The most rows `execute_sql` returns to the model (default 50).
    pub max_query_result_rows: usize,
    /// The project that runs (and is billed for) query jobs; `None` uses the `project_id` the
    /// model passes to `execute_sql`.
    pub compute_project: Option<String>,
    /// The job location, such as `US`; `None` lets BigQuery choose.
    pub location: Option<String>,
    /// The cost cap per query in bytes; `None` for no cap.
    pub maximum_bytes_billed: Option<i64>,
    /// Labels added to every job (the toolset's `gadk-bigquery-tool` label overrides a label
    /// with that key).
    pub job_labels: BTreeMap<String, String>,
}

impl Default for BigQueryToolConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl BigQueryToolConfig {
    /// The defaults: read-only, 50 rows, no compute project, location, cost cap or labels.
    pub fn new() -> Self {
        Self {
            write_mode: WriteMode::Blocked,
            max_query_result_rows: 50,
            compute_project: None,
            location: None,
            maximum_bytes_billed: None,
            job_labels: BTreeMap::new(),
        }
    }

    /// Sets [`write_mode`](Self::write_mode).
    pub fn with_write_mode(mut self, mode: WriteMode) -> Self {
        self.write_mode = mode;
        self
    }

    /// Sets [`max_query_result_rows`](Self::max_query_result_rows).
    pub fn with_max_query_result_rows(mut self, rows: usize) -> Self {
        self.max_query_result_rows = rows;
        self
    }

    /// Sets [`compute_project`](Self::compute_project).
    pub fn with_compute_project(mut self, project: impl Into<String>) -> Self {
        self.compute_project = Some(project.into());
        self
    }

    /// Sets [`location`](Self::location).
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Sets [`maximum_bytes_billed`](Self::maximum_bytes_billed).
    pub fn with_maximum_bytes_billed(mut self, bytes: i64) -> Self {
        self.maximum_bytes_billed = Some(bytes);
        self
    }

    /// Sets [`job_labels`](Self::job_labels).
    pub fn with_job_labels(mut self, labels: BTreeMap<String, String>) -> Self {
        self.job_labels = labels;
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_setters() {
        let d = BigQueryToolConfig::new();
        assert_eq!(d.write_mode, WriteMode::Blocked);
        assert_eq!(d.max_query_result_rows, 50);
        assert_eq!(d.compute_project, None);
        assert_eq!(d.location, None);
        assert_eq!(d.maximum_bytes_billed, None);
        assert!(d.job_labels.is_empty());
        assert_eq!(BigQueryToolConfig::default(), d);

        let labels = BTreeMap::from([("a".to_string(), "b".to_string())]);
        let c = BigQueryToolConfig::new()
            .with_write_mode(WriteMode::Protected)
            .with_max_query_result_rows(3)
            .with_compute_project("cp")
            .with_location("US")
            .with_maximum_bytes_billed(10)
            .with_job_labels(labels.clone());
        assert_eq!(c.write_mode, WriteMode::Protected);
        assert_eq!(c.max_query_result_rows, 3);
        assert_eq!(c.compute_project.as_deref(), Some("cp"));
        assert_eq!(c.location.as_deref(), Some("US"));
        assert_eq!(c.maximum_bytes_billed, Some(10));
        assert_eq!(c.job_labels, labels);
    }
}
