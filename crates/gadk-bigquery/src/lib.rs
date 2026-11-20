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
//! `BigQueryToolset` gives an agent five tools: `list_dataset_ids`, `get_dataset_info`,
//! `list_table_ids`, `get_table_info` and `execute_sql`. `execute_sql` is read-only by default
//! (`WriteMode::Blocked`); `WriteMode::Protected` also allows writes to the temporary tables
//! of a BigQuery session that belongs to the agent session, and `WriteMode::Allowed` runs any
//! statement. `BigQueryClient` is the small BigQuery v2 REST client the tools use.

mod auth;
mod error;
#[allow(dead_code)]
mod ident;

pub use auth::{Adc, AuthHeaders, NoAuth};
pub use error::BigQueryError;
