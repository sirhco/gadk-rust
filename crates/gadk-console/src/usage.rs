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

//! [`UsageSource`]: the console's [`AnalyticsSource`] with the queries that are not in
//! gadk-analytics yet.

use std::ops::Deref;

use gadk_analytics::AnalyticsSource;
use gadk_bigquery::BigQueryClient;

use crate::traces::BigQueryTable;

/// The console's query source: an [`AnalyticsSource`] (which it derefs to) plus the queries
/// that are still the console's.
pub(crate) struct UsageSource {
    inner: AnalyticsSource,
}

impl UsageSource {
    /// A source reading `table` with Application Default Credentials (built on the first call),
    /// each query capped at `max_bytes_billed` bytes.
    pub(crate) fn new(
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self::with_client(
            BigQueryClient::new(),
            compute_project,
            table,
            max_bytes_billed,
        )
    }

    /// [`new`](Self::new) with an explicit BigQuery client.
    pub(crate) fn with_client(
        client: BigQueryClient,
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self {
            inner: AnalyticsSource::new(client, compute_project, table, max_bytes_billed),
        }
    }
}

impl Deref for UsageSource {
    type Target = AnalyticsSource;

    fn deref(&self) -> &AnalyticsSource {
        &self.inner
    }
}
