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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Live test against a project with the `gadk_examples` dataset.
//!
//! ```sh
//! GADK_BIGQUERY_LIVE_PROJECT=<project> cargo test -p gadk-bigquery --test live -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials (BigQuery Data Viewer and Job User). It creates no
//! datasets or tables; the Protected-mode check writes one temporary table in a BigQuery
//! session's anonymous dataset, which BigQuery drops with the session.

use std::sync::Arc;

use gadk_agent::{Context, Tool};
use gadk_bigquery::{BigQueryClient, BigQueryToolConfig, BigQueryToolset, WriteMode};
use serde_json::{Map, Value, json};

fn project() -> String {
    std::env::var("GADK_BIGQUERY_LIVE_PROJECT").expect("set GADK_BIGQUERY_LIVE_PROJECT")
}

async fn call(
    tools: &[Arc<dyn Tool>],
    ctx: &Context,
    name: &str,
    args: Value,
) -> Map<String, Value> {
    let tool = tools.iter().find(|t| t.name() == name).unwrap();
    let out = tool
        .as_runnable()
        .unwrap()
        .run(ctx.clone(), args)
        .await
        .unwrap();
    println!("{name}: {}", Value::Object(out.clone()));
    out
}

async fn tools_of(config: BigQueryToolConfig) -> Vec<Arc<dyn Tool>> {
    use gadk_agent::Toolset;
    let ts = BigQueryToolset::with_client(config, Arc::new(BigQueryClient::new())).unwrap();
    let ctx = Context::builder().build();
    ts.tools(ctx.as_readonly()).await.unwrap()
}

#[tokio::test]
#[ignore = "needs GADK_BIGQUERY_LIVE_PROJECT and ADC"]
async fn explores_and_queries_gadk_examples() {
    let p = project();
    let ctx = Context::builder().build();
    let tools = tools_of(BigQueryToolConfig::new().with_max_query_result_rows(3)).await;

    let ds = call(&tools, &ctx, "list_dataset_ids", json!({ "project_id": p })).await;
    assert!(
        ds["dataset_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("gadk_examples"))
    );

    let ts = call(
        &tools,
        &ctx,
        "list_table_ids",
        json!({ "project_id": p, "dataset_id": "gadk_examples" }),
    )
    .await;
    assert!(
        ts["table_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("orders"))
    );

    let info = call(
        &tools,
        &ctx,
        "get_table_info",
        json!({ "project_id": p, "dataset_id": "gadk_examples", "table_id": "orders" }),
    )
    .await;
    let fields = info["info"]["schema"]["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f["name"] == json!("ordered_at")));

    let q = call(
        &tools,
        &ctx,
        "execute_sql",
        json!({ "project_id": p, "query": "SELECT * FROM gadk_examples.monthly_revenue" }),
    )
    .await;
    assert_eq!(q["status"], json!("SUCCESS"));
    let rows = q["rows"].as_array().unwrap();
    assert!(rows.len() <= 3);
    if rows.len() == 3 {
        // monthly_revenue has more than 3 months in the sample data, so the cap is hit.
        assert_eq!(q.get("result_is_likely_truncated"), Some(&json!(true)));
    }

    let w = call(
        &tools,
        &ctx,
        "execute_sql",
        json!({ "project_id": p,
        "query": "INSERT INTO gadk_examples.support_tickets (ticket_id) VALUES ('live-test')" }),
    )
    .await;
    assert_eq!(w["status"], json!("ERROR"));
    assert_eq!(
        w["error_details"],
        json!("Read-only mode only supports SELECT statements.")
    );
}

#[tokio::test]
#[ignore = "needs GADK_BIGQUERY_LIVE_PROJECT and ADC"]
async fn protected_mode_writes_a_session_temp_table() {
    let p = project();
    let ctx = Context::builder().build();
    let tools = tools_of(
        BigQueryToolConfig::new()
            .with_write_mode(WriteMode::Protected)
            .with_location("US"),
    )
    .await;
    let create = call(
        &tools,
        &ctx,
        "execute_sql",
        json!({ "project_id": p,
        "query": "CREATE TEMP TABLE live_tmp AS SELECT 1 AS n UNION ALL SELECT 2" }),
    )
    .await;
    assert_eq!(create["status"], json!("SUCCESS"), "{create:?}");
    let read = call(
        &tools,
        &ctx,
        "execute_sql",
        json!({ "project_id": p, "query": "SELECT SUM(n) AS total FROM live_tmp" }),
    )
    .await;
    assert_eq!(read["rows"], json!([{ "total": 3 }]));
}
