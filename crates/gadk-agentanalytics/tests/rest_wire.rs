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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Replays every REST wire scenario the Go probe recorded from Go's
//! `cloud.google.com/go/bigquery` v1.79.0 client (`tests/fixtures/wire/*.json`, spec R9): a
//! `FakeServer` answers the canned responses in order (with `{{JOB_ID}}` replaced by the job
//! ID the client sent), and the Rust client is driven through the same client calls Go made —
//! `TableOps` (Go `bqTableAdapter`) for the table scenarios, `BigQueryClient::get_table` plus
//! `insert_query_job` / `wait_job` for the view scenarios (Go `bqViewRunner.RunViewDDL`'s
//! calls). The call sequence is read off Go's recorded requests (`ensure_table_and_schema` and
//! `run_view_ddl` themselves run in `tests/rest_schema_upgrade.rs` and `tests/rest_views.rs`): each recorded request starts one client call,
//! and the requests the client sends for it (its retries and job polling included) must be
//! exactly Go's — method, path, query, headers (`Authorization`, `User-Agent`,
//! `X-Goog-Api-Client` and the transport's `Accept`, `Accept-Encoding`, `Content-Length`,
//! `Host` aside), JSON body semantically, job IDs by shape and count. A call that ended the
//! scenario with an error must produce Go's error text.

use std::sync::Arc;

use gadk_agentanalytics::schema_upgrade::{Clustering, TimePartitioning};
use gadk_agentanalytics::{
    BigQueryClient, Context, Error, TableMetadata, TableMetadataOps, TableMetadataUpdate, TableOps,
    TableSchema,
};
use pretty_assertions::assert_eq;
use serde_json::Value;

#[path = "support/wire.rs"]
mod wire;

use wire::{Replay, assert_request, client_for, load, scenario_names};

/// Scenarios whose Go error is raised by the caller (`ensureTableAndSchema`,
/// `bqViewRunner.RunViewDDL`), not by a client call: every call it made succeeded.
const CALLER_ERROR_SCENARIOS: [&str; 4] = [
    "incompatible_schema",
    "view_dropped_client",
    "view_non_view_materialized",
    "view_non_view_table",
];

/// Every scenario of `tests/fixtures/wire` (SCENARIO.md), one test each.
const SCENARIOS: [&str; 41] = [
    "additive_patch",
    "additive_patch_denylist",
    "columns_match_no_write",
    "create_conflict_disable_upgrade",
    "create_conflict_reread_fails",
    "create_conflict_reread_patch",
    "create_fails",
    "disable_schema_upgrade_existing",
    "get_permission_denied",
    "get_unavailable_retried",
    "incompatible_schema",
    "maybe_upgrade_direct",
    "patch_412_exhausted",
    "patch_412_reread_fails",
    "patch_412_retry",
    "patch_412_then_columns_match",
    "patch_fails",
    "table_missing_create",
    "table_missing_create_custom_clustering",
    "table_missing_create_denylist",
    "table_missing_create_nil_clustering",
    "table_without_etag_patch",
    "view_differs_replace",
    "view_dropped_client",
    "view_get_error_still_creates",
    "view_identical_no_job",
    "view_job_error",
    "view_job_insert_400",
    "view_job_insert_409",
    "view_job_insert_503_retried",
    "view_job_pending_no_config",
    "view_job_pending_no_config_then_error",
    "view_job_pending_query_path",
    "view_job_pending_then_error",
    "view_missing_create_done",
    "view_no_dataset_skips_get",
    "view_non_view_materialized",
    "view_non_view_table",
    "view_query_without_as_newline",
    "view_runner_empty_project_uses_client",
    "view_runner_project_differs_from_client",
];

/// A recorded request's REST method, from its method and path under `/bigquery/v2/`.
#[derive(Debug)]
enum Rpc {
    TablesGet(String, String, String),
    TablesInsert(String, String),
    TablesPatch(String, String, String),
    JobsInsert,
}

fn classify(req: &Value) -> Rpc {
    let path = req["path"].as_str().unwrap();
    let rel = path.strip_prefix("/bigquery/v2/").unwrap();
    let seg: Vec<&str> = rel.split('/').collect();
    let method = req["method"].as_str().unwrap();
    match (method, seg.as_slice()) {
        ("GET", ["projects", p, "datasets", d, "tables", t]) => {
            Rpc::TablesGet((*p).into(), (*d).into(), (*t).into())
        }
        ("PATCH", ["projects", p, "datasets", d, "tables", t]) => {
            Rpc::TablesPatch((*p).into(), (*d).into(), (*t).into())
        }
        ("POST", ["projects", p, "datasets", d, "tables"]) => {
            Rpc::TablesInsert((*p).into(), (*d).into())
        }
        ("POST", ["projects", _, "jobs"]) => Rpc::JobsInsert,
        _ => panic!("request {method} {path} does not start a client call"),
    }
}

/// The `TableMetadata` whose `tables.insert` body is Go's recorded `body` (the inverse of Go's
/// `TableMetadata.toBQ()` for the fields the plugin sets).
fn metadata_from_insert_body(body: &Value) -> TableMetadata {
    let schema: TableSchema = serde_json::from_value(body["schema"].clone()).unwrap();
    let time_partitioning = body.get("timePartitioning").map(|p| TimePartitioning {
        partition_type: p["type"].as_str().unwrap_or_default().into(),
        field: p["field"].as_str().unwrap_or_default().into(),
        ..TimePartitioning::default()
    });
    let clustering = body.get("clustering").map(|c| Clustering {
        fields: c
            .get("fields")
            .map(|f| serde_json::from_value(f.clone()).unwrap())
            .unwrap_or_default(),
    });
    TableMetadata {
        schema: schema.fields,
        time_partitioning,
        clustering,
        labels: serde_json::from_value(body["labels"].clone()).unwrap_or_default(),
        ..TableMetadata::default()
    }
}

/// The `TableMetadataUpdate` whose `tables.patch` body is Go's recorded `body`.
fn update_from_patch_body(body: &Value) -> TableMetadataUpdate {
    TableMetadataUpdate {
        schema: body.get("schema").map(|s| {
            serde_json::from_value::<TableSchema>(s.clone())
                .unwrap()
                .fields
        }),
        set_labels: serde_json::from_value(body["labels"].clone()).unwrap_or_default(),
    }
}

/// What one client call ended with: `Ok` or the error it returned (for a job, Go
/// `RunViewDDL`'s `js.Err()` included).
type Outcome = Result<(), Error>;

/// Drives the client call that `exchanges[i]`'s request starts.
async fn run_call(
    ctx: &Context,
    client: &Arc<BigQueryClient>,
    view_scenario: bool,
    exchange: &Value,
) -> Outcome {
    let req = &exchange["request"];
    let table = |p: &str, d: &str, t: &str| TableOps::new(client.clone(), p, d, t);
    match classify(req) {
        Rpc::TablesGet(p, d, t) if view_scenario => {
            client.get_table(ctx, &p, &d, &t).await.map(drop)
        }
        Rpc::TablesGet(p, d, t) => table(&p, &d, &t).metadata(ctx).await.map(drop),
        Rpc::TablesInsert(p, d) => {
            let body = &req["body"];
            let t = body["tableReference"]["tableId"].as_str().unwrap();
            table(&p, &d, t)
                .create(ctx, &metadata_from_insert_body(body))
                .await
        }
        Rpc::TablesPatch(p, d, t) => {
            let etag = req["headers"]["If-Match"][0].as_str().unwrap_or_default();
            table(&p, &d, &t)
                .update(ctx, &update_from_patch_body(&req["body"]), etag)
                .await
                .map(drop)
        }
        Rpc::JobsInsert => {
            let query = req["body"]["configuration"]["query"]["query"]
                .as_str()
                .unwrap();
            // Go `bqViewRunner.RunViewDDL` after its `tables.get` check.
            let job = client.insert_query_job(ctx, query).await?;
            let status = match job.last_status.clone() {
                Some(status) if status.done() => status,
                _ => client.wait_job(ctx, &job).await?,
            };
            match status.err() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }
    }
}

/// Replays scenario `name` (see the module doc).
async fn replay(name: &str) {
    let fixture = load(name);
    assert_eq!(fixture["name"], name);
    let replay = Replay::start(&fixture);
    let client = client_for(&fixture, &replay.server);
    let ctx = Context::background();
    let view_scenario = fixture["call"]["fn"]
        .as_str()
        .unwrap()
        .starts_with("bqViewRunner");
    let exchanges = fixture["exchanges"].as_array().unwrap();
    let mut last: Option<Outcome> = None;
    let mut i = 0;
    while i < exchanges.len() {
        let before = replay.server.hits();
        let outcome = run_call(&ctx, &client, view_scenario, &exchanges[i]).await;
        let sent = replay.server.hits() - before;
        assert!(sent >= 1, "{name}: call at request {i} sent nothing");
        // The call's last response decides its outcome: a non-2xx status is that error.
        let status = exchanges[i + sent - 1]["response"]["status"]
            .as_u64()
            .unwrap();
        match &outcome {
            Err(Error::Api(api)) => assert_eq!(u64::from(api.code), status, "{name}: call {i}"),
            Err(Error::BigQuery(_)) => assert_eq!(status, 200, "{name}: call {i}"),
            Err(other) => panic!("{name}: call at request {i}: unexpected error {other:?}"),
            Ok(()) => assert!((200..300).contains(&status), "{name}: call {i} succeeded"),
        }
        i += sent;
        last = Some(outcome);
    }
    let requests = replay.server.requests();
    assert_eq!(requests.len(), exchanges.len(), "{name}: request count");
    let job_id = replay.job_id();
    for (i, (got, want)) in requests.iter().zip(exchanges).enumerate() {
        assert_request(name, i, got, &want["request"], &job_id);
    }

    // Job IDs: Go's shape, and as many distinct IDs as Go sent (a retried jobs.insert keeps
    // its ID).
    let sent_ids: std::collections::BTreeSet<String> = requests
        .iter()
        .filter(|r| r.method == "POST" && r.path.ends_with("/jobs"))
        .map(|r| {
            r.json()["jobReference"]["jobId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    match fixture["job_id"].as_object() {
        None => assert!(sent_ids.is_empty(), "{name}: unexpected job"),
        Some(spec) => {
            assert_eq!(spec["pattern"], "^[0-9a-f]{32}$");
            assert_eq!(
                sent_ids.len() as u64,
                spec["distinct_ids"].as_u64().unwrap(),
                "{name}: distinct job IDs"
            );
            for id in &sent_ids {
                assert!(
                    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                    "{name}: job ID {id:?}"
                );
            }
        }
    }

    // A scenario Go ended with an error the last call returned carries that call's error text
    // (wrapped by ensureTableAndSchema's prefixes).
    match (fixture["result"]["error"].as_str(), &last) {
        (Some(want), Some(Err(err))) => {
            let want = want.replace("{{JOB_ID}}", &job_id);
            let got = err.to_string();
            assert!(
                want.ends_with(&got),
                "{name}: Go error {want:?} does not end with {got:?}"
            );
        }
        // Go's error came from the caller's logic after its last call succeeded (a schema
        // mismatch, a non-view table) or before any call (a dropped client);
        // `tests/rest_schema_upgrade.rs` and `tests/rest_views.rs` run that logic over these
        // scenarios and check the text.
        (Some(_), Some(Ok(())) | None) if CALLER_ERROR_SCENARIOS.contains(&name) => {}
        (Some(want), Some(Ok(()))) => {
            panic!("{name}: Go ended with error {want:?}, the last Rust call succeeded")
        }
        (Some(want), None) => panic!("{name}: Go ended with error {want:?}, no Rust call ran"),
        (None, _) => {}
    }
}

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                replay(stringify!($name)).await;
            }
        )*
    };
}

scenarios!(
    additive_patch,
    additive_patch_denylist,
    columns_match_no_write,
    create_conflict_disable_upgrade,
    create_conflict_reread_fails,
    create_conflict_reread_patch,
    create_fails,
    disable_schema_upgrade_existing,
    get_permission_denied,
    get_unavailable_retried,
    incompatible_schema,
    maybe_upgrade_direct,
    patch_412_exhausted,
    patch_412_reread_fails,
    patch_412_retry,
    patch_412_then_columns_match,
    patch_fails,
    table_missing_create,
    table_missing_create_custom_clustering,
    table_missing_create_denylist,
    table_missing_create_nil_clustering,
    table_without_etag_patch,
    view_differs_replace,
    view_dropped_client,
    view_get_error_still_creates,
    view_identical_no_job,
    view_job_error,
    view_job_insert_400,
    view_job_insert_409,
    view_job_insert_503_retried,
    view_job_pending_no_config,
    view_job_pending_no_config_then_error,
    view_job_pending_query_path,
    view_job_pending_then_error,
    view_missing_create_done,
    view_no_dataset_skips_get,
    view_non_view_materialized,
    view_non_view_table,
    view_query_without_as_newline,
    view_runner_empty_project_uses_client,
    view_runner_project_differs_from_client,
);

#[test]
fn every_wire_fixture_is_replayed() {
    assert_eq!(scenario_names(), SCENARIOS.map(String::from).to_vec());
}

/// `view_dropped_client`: Go sent nothing (the weak client pointer was empty;
/// `BigQueryViewRunner` returns the error before any request).
#[test]
fn dropped_client_sends_nothing() {
    let fixture = load("view_dropped_client");
    assert_eq!(fixture["exchanges"], Value::Array(Vec::new()));
    assert_eq!(
        fixture["result"]["error"],
        "agentanalytics: BigQuery client is not configured for view creation"
    );
}

/// The metadata a `tables.get` reply decodes to (Go `bqToTableMetadata`): the view the
/// identical-view check reads, the etag the upgrade sends back, the schema and labels.
#[tokio::test]
async fn get_table_decodes_go_metadata() {
    let fixture = load("view_identical_no_job");
    let replay = Replay::start(&fixture);
    let client = client_for(&fixture, &replay.server);
    let md = client
        .get_table(
            &Context::background(),
            "test-project",
            "agent_analytics",
            "v_user_message_received",
        )
        .await
        .unwrap();
    let reply = &fixture["exchanges"][0]["response"]["body"];
    assert_eq!(md.table_type, "VIEW");
    assert_eq!(md.view_query, reply["view"]["query"].as_str().unwrap());
    assert_eq!(md.etag, "view-etag");
    let query = fixture["call"]["query"].as_str().unwrap();
    let (_, select_body) = query.split_once(" AS\n").unwrap();
    assert_eq!(md.view_query.trim(), select_body.trim());

    let fixture = load("additive_patch");
    let replay = Replay::start(&fixture);
    let client = client_for(&fixture, &replay.server);
    let md = TableOps::new(client, "test-project", "agent_analytics", "agent_events")
        .metadata(&Context::background())
        .await
        .unwrap();
    let reply = &fixture["exchanges"][0]["response"]["body"];
    assert_eq!(md.etag, "etag-1");
    assert_eq!(md.labels.get("adk_schema_version").unwrap(), "1");
    let schema: TableSchema = serde_json::from_value(reply["schema"].clone()).unwrap();
    assert_eq!(md.schema, schema.fields);
}

/// Go `googleapi.Error` texts and codes for the recorded error replies.
#[tokio::test]
async fn api_errors_keep_go_texts() {
    let fixture = load("get_permission_denied");
    let replay = Replay::start(&fixture);
    let client = client_for(&fixture, &replay.server);
    let err = client
        .get_table(
            &Context::background(),
            "test-project",
            "agent_analytics",
            "agent_events",
        )
        .await
        .unwrap_err();
    let Error::Api(api) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(api.code, 403);
    assert_eq!(api.errors.len(), 1);
    assert_eq!(api.errors[0].reason, "accessDenied");
    assert_eq!(
        format!("failed to get BigQuery table agent_events metadata: {err}"),
        fixture["result"]["error"].as_str().unwrap()
    );
}
