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

//! [`AnalyticsSource`]: the queries' entry point, and the plumbing they share.
//!
//! Every query runs (and is billed) in the billing project, carries its values as query
//! parameters and stops at the byte cap.

use chrono::{DateTime, Utc};
use gadk_bigquery::{BigQueryClient, QueryOptions, QueryResult};
use serde_json::{Map, Value, json};

use crate::row::{from_bigquery, param, ts_param};
use crate::scores::DML_MIN_BYTES;
use crate::{BigQueryTable, SourceError};

/// The queries over one agent-analytics table, run (and billed) in the billing project, each
/// stopped at the byte cap.
#[non_exhaustive]
pub struct AnalyticsSource {
    client: BigQueryClient,
    compute_project: String,
    pub(crate) table: BigQueryTable,
    max_bytes_billed: i64,
}

impl std::fmt::Debug for AnalyticsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalyticsSource")
            .field("billing_project", &self.compute_project)
            .field("table", &self.table)
            .field("max_bytes_billed", &self.max_bytes_billed)
            .finish_non_exhaustive()
    }
}

impl AnalyticsSource {
    /// A source reading `table` through `client`, running every query in `billing_project`
    /// (which BigQuery bills) and capping each at `max_bytes_billed` bytes (a DML statement at
    /// no less than 10 MiB, BigQuery's minimum charge for one).
    pub fn new(
        client: BigQueryClient,
        billing_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self {
            client,
            compute_project: billing_project.into(),
            table,
            max_bytes_billed,
        }
    }

    /// The table the queries read.
    pub fn table(&self) -> &BigQueryTable {
        &self.table
    }

    /// The table as a backquoted SQL path.
    #[doc(hidden)]
    pub fn table_path(&self) -> Result<String, SourceError> {
        self.table.sql_path()
    }

    /// Runs `sql` with `params`, the byte cap and at most `max_rows` rows.
    #[doc(hidden)]
    pub async fn run(
        &self,
        sql: &str,
        params: Vec<Value>,
        max_rows: u32,
    ) -> Result<QueryResult, SourceError> {
        let opts = QueryOptions::new()
            .with_maximum_bytes_billed(self.max_bytes_billed)
            .with_query_parameters(params)
            .with_max_rows(max_rows);
        self.client
            .query(&self.compute_project, sql, &opts)
            .await
            .map_err(from_bigquery)
    }

    /// Runs the DML statement `sql` with `params` under the byte cap, raised to
    /// [`DML_MIN_BYTES`] (BigQuery bills every DML statement at least 10 MiB).
    #[doc(hidden)]
    pub async fn run_dml(&self, sql: &str, params: Vec<Value>) -> Result<(), SourceError> {
        let opts = QueryOptions::new()
            .with_maximum_bytes_billed(self.max_bytes_billed.max(DML_MIN_BYTES))
            .with_query_parameters(params);
        self.client
            .query(&self.compute_project, sql, &opts)
            .await
            .map(|_| ())
            .map_err(from_bigquery)
    }

    /// Runs `sql` with `params` plus `@limit` = `cap + 1`: the first `cap` rows and whether more
    /// existed.
    #[doc(hidden)]
    pub async fn run_capped(
        &self,
        sql: &str,
        mut params: Vec<Value>,
        cap: usize,
    ) -> Result<(Vec<Map<String, Value>>, bool), SourceError> {
        params.push(param("limit", "INT64", Some((cap + 1).to_string())));
        let mut result = self.run(sql, params, (cap + 1) as u32).await?;
        let truncated = result.rows.len() > cap || result.has_more;
        result.rows.truncate(cap);
        Ok((result.rows, truncated))
    }
}

/// The `@start` and `@end` TIMESTAMP parameters.
#[doc(hidden)]
pub fn window_params(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<Value> {
    vec![
        param("start", "TIMESTAMP", Some(ts_param(start))),
        param("end", "TIMESTAMP", Some(ts_param(end))),
    ]
}

/// An INT64 cell as a count; 0 when NULL or not a non-negative integer.
#[doc(hidden)]
pub fn count(row: &Map<String, Value>, key: &str) -> u64 {
    row.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The `version` cell as a prompt version.
pub(crate) fn version_of(row: &Map<String, Value>) -> Option<u32> {
    row.get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
}

/// An `ARRAY<STRING>` query parameter.
#[doc(hidden)]
pub fn string_array_param(name: &str, values: &[String]) -> Value {
    let values: Vec<Value> = values.iter().map(|v| json!({ "value": v })).collect();
    json!({
        "name": name,
        "parameterType": { "type": "ARRAY", "arrayType": { "type": "STRING" } },
        "parameterValue": { "arrayValues": values },
    })
}
