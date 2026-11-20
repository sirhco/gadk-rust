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

//! gadk-bigquery: a BigQuery toolset for gadk agents.
//!
//! [`BigQueryToolset`] gives an agent five tools: `list_dataset_ids`, `get_dataset_info`,
//! `list_table_ids`, `get_table_info` and `execute_sql`. `execute_sql` is read-only by default
//! ([`WriteMode::Blocked`]); [`WriteMode::Protected`] also allows writes to the temporary tables
//! of a BigQuery session that belongs to the agent session, and [`WriteMode::Allowed`] runs any
//! statement. [`BigQueryClient`] is the small BigQuery v2 REST client the tools use.
//!
//! Query results are JSON objects per row: INT64 values are numbers up to 2^53 and strings
//! beyond, FLOAT64 values are numbers (NaN and infinities are strings), NUMERIC and BIGNUMERIC
//! values are strings, BOOL values are booleans, TIMESTAMP values are RFC 3339 strings, RECORD
//! values are objects, REPEATED values are arrays, NULL is `null`, and every other type is the
//! string BigQuery returns.

mod auth;
mod client;
mod config;
mod error;
mod ident;
mod rows;
mod toolset;
mod types;

pub use auth::{Adc, AuthHeaders, NoAuth};
pub use client::{BigQueryClient, ENDPOINT};
pub use config::{BigQueryToolConfig, WriteMode};
pub use error::BigQueryError;
pub use toolset::{BigQueryToolset, TOOLSET_NAME};
pub use types::{DryRunInfo, QueryOptions, QueryResult, SessionInfo, TableRef};
