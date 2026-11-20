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

//! Live read of an agent-analytics table. Run with
//! `GADK_CONSOLE_PROJECT=<billing project> GADK_CONSOLE_BQ_TABLE=<project>.<dataset>.<table>
//! cargo test -p gadk-console --test bigquery_live -- --ignored`
//! (Application Default Credentials with roles/bigquery.dataViewer and roles/bigquery.jobUser).

use gadk_console::traces::{
    BigQueryTable, BigQueryTraceSource, TimeRange, TraceQuery, TraceSource,
};

#[tokio::test]
#[ignore = "needs GCP credentials and GADK_CONSOLE_PROJECT / GADK_CONSOLE_BQ_TABLE"]
async fn lists_and_opens_the_newest_trace() {
    let project = std::env::var("GADK_CONSOLE_PROJECT").expect("GADK_CONSOLE_PROJECT");
    let table = std::env::var("GADK_CONSOLE_BQ_TABLE").expect("GADK_CONSOLE_BQ_TABLE");
    let table = BigQueryTable::parse_url(&format!("bigquery://{table}")).expect("table");
    let source = BigQueryTraceSource::new(project, table, 1_073_741_824);
    let query = TraceQuery {
        range: TimeRange::Days7,
        ..TraceQuery::default()
    };
    let page = source.list(&query).await.expect("list");
    let newest = page.traces.first().expect(
        "no agent_events rows in the last 7 days; run an agent with the analytics plugin first",
    );
    let detail = source.get(&newest.trace_id).await.expect("get");
    assert!(
        detail.spans.iter().any(|s| !s.name.is_empty()),
        "no named span in {detail:?}"
    );
}
