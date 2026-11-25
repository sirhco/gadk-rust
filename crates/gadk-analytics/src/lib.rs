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

//! gadk-analytics: typed, parameterized BigQuery reads over the agent-analytics table
//! (`agent_events`) that `gadk-agentanalytics` writes.
//!
//! [`AnalyticsSource`] runs the queries: build it over a `gadk_bigquery::BigQueryClient`, the
//! project that runs and pays for the queries, the table and a byte cap per query. The queries
//! are grouped by module: [`usage`] (token usage per model, agent and prompt version) and
//! [`sessions`] (activity per session, per user and per turn).
//!
//! The shared types are at the root: [`BigQueryTable`] names the table, [`TimeRange`] and
//! [`TraceId`] are the trace-list window and id, and every query fails with a [`SourceError`].
//! [`row`] holds the cell readers and query-parameter builders the queries share, and [`cost`]
//! the token counts of model calls and their cost.

#![deny(missing_docs)]

pub mod cost;
mod error;
pub mod row;
pub mod sessions;
mod source;
mod table;
#[cfg(any(test, feature = "test-util"))]
pub mod testutil;
mod time;
pub mod usage;

pub use error::{Service, SourceError};
pub use source::AnalyticsSource;
pub use table::BigQueryTable;
pub use time::{TimeRange, TraceId};

/// Query plumbing that gadk-console's queries use until they move into this crate.
#[doc(hidden)]
pub mod plumbing {
    pub use crate::source::{count, string_array_param, version_of, window_params};
    pub use crate::usage::{CALL_COLUMNS, calls_cte, model_calls, responses_cte};
}
