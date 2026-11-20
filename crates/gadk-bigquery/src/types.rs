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

//! The values the client's query calls take and return.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Options of one dry run or query.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct QueryOptions {
    /// The most rows the query returns (`maxResults`); `None` for BigQuery's default.
    pub max_rows: Option<u32>,
    /// The job location, such as `US`.
    pub location: Option<String>,
    /// The cost cap: the job fails rather than bill more bytes than this.
    pub maximum_bytes_billed: Option<i64>,
    /// Job labels.
    pub labels: BTreeMap<String, String>,
    /// Runs the job inside this BigQuery session.
    pub session_id: Option<String>,
    /// Starts a new BigQuery session with this job (see [`QueryResult::session`]).
    pub create_session: bool,
    /// Named query parameters: BigQuery `QueryParameter` objects such as
    /// `{"name": "id", "parameterType": {"type": "STRING"}, "parameterValue": {"value": "x"}}`,
    /// sent with `parameterMode: "NAMED"` when not empty.
    pub query_parameters: Vec<Value>,
}

impl QueryOptions {
    /// Every option unset.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets [`max_rows`](Self::max_rows).
    pub fn with_max_rows(mut self, max_rows: u32) -> Self {
        self.max_rows = Some(max_rows);
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

    /// Sets [`labels`](Self::labels).
    pub fn with_labels(mut self, labels: BTreeMap<String, String>) -> Self {
        self.labels = labels;
        self
    }

    /// Sets (or clears) [`session_id`](Self::session_id).
    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    /// Sets [`create_session`](Self::create_session).
    pub fn with_create_session(mut self, create: bool) -> Self {
        self.create_session = create;
        self
    }

    /// Sets [`query_parameters`](Self::query_parameters).
    pub fn with_query_parameters(mut self, params: Vec<Value>) -> Self {
        self.query_parameters = params;
        self
    }
}

/// A table reference.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TableRef {
    /// The project id.
    pub project: String,
    /// The dataset id.
    pub dataset: String,
    /// The table id.
    pub table: String,
}

impl TableRef {
    /// A reference to `project.dataset.table`.
    pub fn new(
        project: impl Into<String>,
        dataset: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            dataset: dataset.into(),
            table: table.into(),
        }
    }
}

/// What a dry run says about a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DryRunInfo {
    /// `statistics.query.statementType`, such as `SELECT`, `INSERT` or `CREATE_TABLE`.
    pub statement_type: String,
    /// The table the statement writes: `configuration.query.destinationTable`, else
    /// `statistics.query.ddlTargetTable`; `None` when the reply has neither.
    pub destination: Option<TableRef>,
}

impl DryRunInfo {
    /// A dry-run result.
    pub fn new(statement_type: impl Into<String>, destination: Option<TableRef>) -> Self {
        Self {
            statement_type: statement_type.into(),
            destination,
        }
    }
}

/// A BigQuery session: where it runs and its anonymous dataset (which holds its temp tables).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SessionInfo {
    /// The project the session's jobs run in.
    pub project: String,
    /// The session id.
    pub session_id: String,
    /// The session's anonymous dataset.
    pub dataset: String,
}

impl SessionInfo {
    /// A session.
    pub fn new(
        project: impl Into<String>,
        session_id: impl Into<String>,
        dataset: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            session_id: session_id.into(),
            dataset: dataset.into(),
        }
    }
}

/// The result of a query.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct QueryResult {
    /// The rows returned, at most [`QueryOptions::max_rows`].
    pub rows: Vec<Map<String, Value>>,
    /// `totalRows` of the reply.
    pub total_rows: Option<u64>,
    /// Whether the result has more rows than were returned.
    pub has_more: bool,
    /// The session the query started, when [`QueryOptions::create_session`] was set.
    pub session: Option<SessionInfo>,
}

impl QueryResult {
    /// A complete result of `rows`.
    pub fn new(rows: Vec<Map<String, Value>>) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }
}
