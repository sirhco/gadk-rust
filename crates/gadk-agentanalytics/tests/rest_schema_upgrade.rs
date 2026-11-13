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

//! Runs ADK-Go `ensureTableAndSchema` / `maybeUpgradeSchema` (`schema_upgrade.go`) end to end
//! over the REST client (spec R4, R9): for every table scenario `scripts/gen-go-agentanalytics`
//! recorded (`tests/fixtures/wire/*.json` whose `call.fn` is not a view runner), a
//! `FakeServer` answers Go's canned responses in order and the real
//! [`ensure_table_and_schema`] (or `TableOps::metadata` + [`maybe_upgrade_schema`] for
//! `maybe_upgrade_direct`) runs over [`TableOps`] with the scenario's `Config`. Asserted against
//! the Go run: the exact request sequence (method, path, query, headers except
//! `Authorization`/`User-Agent`/`X-Goog-Api-Client` and transport-added ones, JSON body
//! semantically), the full error text, the error's wrap chain, and the `Config.Logger` lines.
//! (`tests/rest_wire.rs` replays the same captures as raw client calls; this file drives the
//! function under test instead.)

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gadk_agentanalytics::bigquery::fake::RecordingLogger;
use gadk_agentanalytics::{
    BigQueryClient, ClientOptions, Config, Context, Error, Logger, TableMetadataOps, TableOps,
    ensure_table_and_schema, maybe_upgrade_schema, project_events_schema,
};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use pretty_assertions::assert_eq;
use serde_json::Value;

const WIRE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wire");

/// Every table scenario of `tests/fixtures/wire` (the `view_*` ones are `tests/rest_views.rs`').
const SCENARIOS: [&str; 22] = [
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
];

/// Headers left out of the comparison: credentials and client identification, and those the
/// HTTP transports add themselves (Go `net/http`: `Accept-Encoding`, `Content-Length`;
/// reqwest/hyper: `Accept`, `Host`).
const IGNORED_HEADERS: [&str; 7] = [
    "authorization",
    "user-agent",
    "x-goog-api-client",
    "accept",
    "accept-encoding",
    "content-length",
    "host",
];

/// The base `Config` description every `ensureTableAndSchema` scenario starts from.
const BASE_CONFIG: &str = "DefaultConfig() with ProjectID \"test-project\" and a recording Logger";

fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{WIRE_DIR}/{name}.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The scenario's `Config` (Go `call.config`): `DefaultConfig()` with `ProjectID
/// "test-project"`, plus a recording logger and the overrides the description names.
fn config_for(description: &str, logger: &Arc<RecordingLogger>) -> Config {
    let mut cfg = Config {
        project_id: "test-project".to_owned(),
        ..Config::default()
    };
    if description == "DefaultConfig() with ProjectID \"test-project\"" {
        return cfg;
    }
    cfg.logger = Some(logger.clone() as Arc<dyn Logger>);
    let overrides = description
        .strip_prefix(BASE_CONFIG)
        .unwrap_or_else(|| panic!("unknown config {description:?}"));
    match overrides {
        "" => {}
        ", DisableSchemaUpgrade true" => cfg.disable_schema_upgrade = true,
        ", ClusteringFields nil" => cfg.clustering_fields = Vec::new(),
        ", ClusteringFields [\"session_id\"], DatasetID \"test_dataset\", TableName \"events\"" => {
            cfg.clustering_fields = vec!["session_id".to_owned()];
            cfg.dataset_id = "test_dataset".to_owned();
            cfg.table_name = "events".to_owned();
        }
        other => panic!("unknown config overrides {other:?}"),
    }
    cfg
}

/// A `FakeServer` answering a scenario's canned responses in order (each with Go's
/// `application/json` reply).
fn serve(fixture: &Value) -> FakeServer {
    let responses: Vec<(u16, String)> = fixture["exchanges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let status = u16::try_from(e["response"]["status"].as_u64().unwrap()).unwrap();
            (status, e["response"]["body"].to_string())
        })
        .collect();
    let next = AtomicUsize::new(0);
    FakeServer::start(move |_req: &RecordedRequest| {
        let i = next.fetch_add(1, Ordering::SeqCst);
        match responses.get(i) {
            Some((status, body)) => FakeResponse::json_status(*status, body.clone()),
            None => FakeResponse::json_status(500, r#"{"error":"unexpected request"}"#),
        }
    })
}

/// Go `bq.NewClient(ctx, project, option.WithEndpoint(server+"/bigquery/v2/"),
/// option.WithoutAuthentication())`.
fn client_for(fixture: &Value, server: &FakeServer) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        endpoint: Some(format!("{}/bigquery/v2/", server.url())),
        without_authentication: true,
        ..ClientOptions::default()
    };
    let project = fixture["client"]["project"].as_str().unwrap();
    Arc::new(BigQueryClient::new(project, &options).unwrap())
}

/// Asserts the Rust request equals Go's recorded one.
fn assert_request(at: &str, got: &RecordedRequest, want: &Value) {
    assert_eq!(got.method, want["method"].as_str().unwrap(), "{at}: method");
    assert_eq!(got.path, want["path"].as_str().unwrap(), "{at}: path");
    assert_eq!(
        got.query.clone().unwrap_or_default(),
        want["raw_query"].as_str().unwrap(),
        "{at}: query"
    );
    let mut got_headers: Vec<(String, Vec<String>)> = got
        .headers
        .keys()
        .map(|k| k.as_str().to_ascii_lowercase())
        .filter(|k| !IGNORED_HEADERS.contains(&k.as_str()))
        .map(|k| (k.clone(), got.header_values(&k)))
        .collect();
    got_headers.sort();
    got_headers.dedup();
    let mut want_headers: Vec<(String, Vec<String>)> = want["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            (
                k.to_ascii_lowercase(),
                serde_json::from_value(v.clone()).unwrap(),
            )
        })
        .filter(|(k, _)| !IGNORED_HEADERS.contains(&k.as_str()))
        .collect();
    want_headers.sort();
    assert_eq!(got_headers, want_headers, "{at}: headers");
    if want["body"].is_null() {
        assert_eq!(got.body_text(), "", "{at}: body");
    } else {
        let got_body: Value = serde_json::from_slice(&got.body).unwrap();
        assert_eq!(got_body, want["body"], "{at}: body");
    }
}

/// The wrap chain of `err` in Go `%T` names (Go's `result.error_type`), leaving out Go's
/// `*apierror.APIError` (the `googleapi.Error`'s own wrapped detail, which gadk's `ApiError`
/// folds in).
fn go_types(err: &Error) -> Vec<&'static str> {
    let mut chain = Vec::new();
    let mut cur = Some(err);
    while let Some(e) = cur {
        cur = None;
        chain.push(match e {
            Error::Wrapped { source, .. } => {
                cur = Some(source.as_ref());
                "*fmt.wrapError"
            }
            Error::Api(_) => "*googleapi.Error",
            Error::SchemaMismatch(_) => "*agentanalytics.schemaMismatchError",
            other => panic!("unexpected error variant {other:?}"),
        });
    }
    chain
}

/// Runs scenario `name` (see the module doc).
async fn run(name: &str) {
    let fixture = load(name);
    assert_eq!(fixture["name"], name);
    let call = &fixture["call"];
    let server = serve(&fixture);
    let client = client_for(&fixture, &server);
    let logger = Arc::new(RecordingLogger::default());
    let cfg = config_for(call["config"].as_str().unwrap(), &logger);
    let table = TableOps::new(
        client,
        cfg.project_id.clone(),
        cfg.dataset_id.clone(),
        cfg.table_name.clone(),
    );
    let ctx = Context::background();
    let fn_text = call["fn"].as_str().unwrap();
    let result = if fn_text.starts_with("ensureTableAndSchema(") {
        let denied: BTreeSet<String> = serde_json::from_value(call["denied"].clone()).unwrap();
        ensure_table_and_schema(&ctx, &table, &cfg, &denied).await
    } else {
        assert_eq!(
            fn_text,
            "md := table.Metadata(ctx); maybeUpgradeSchema(ctx, bqTableAdapter{table}, md, cfg, projectEventsSchema(nil))"
        );
        let md = table.metadata(&ctx).await.unwrap();
        let desired = project_events_schema(&BTreeSet::new());
        maybe_upgrade_schema(&ctx, &table, md, &cfg, &desired).await
    };

    let exchanges = fixture["exchanges"].as_array().unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), exchanges.len(), "{name}: request count");
    for (i, (got, want)) in requests.iter().zip(exchanges).enumerate() {
        assert_request(&format!("{name} request {i}"), got, &want["request"]);
    }

    match (&result, fixture["result"]["error"].as_str()) {
        (Ok(()), None) => {}
        (Err(err), Some(want)) => {
            assert_eq!(err.to_string(), want, "{name}: error text");
            let mut want_types: Vec<String> =
                serde_json::from_value(fixture["result"]["error_type"].clone()).unwrap();
            want_types.retain(|t| t != "*apierror.APIError");
            assert_eq!(go_types(err), want_types, "{name}: error chain");
        }
        (got, want) => panic!("{name}: result {got:?}, Go error {want:?}"),
    }

    let want_logs: Vec<String> = serde_json::from_value(fixture["logs"].clone()).unwrap();
    assert_eq!(logger.lines(), want_logs, "{name}: logs");
}

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                run(stringify!($name)).await;
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
);

/// Every non-view wire fixture is one of [`SCENARIOS`].
#[test]
fn every_table_scenario_is_run() {
    let mut names: Vec<String> = std::fs::read_dir(WIRE_DIR)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|f| f.strip_suffix(".json").map(str::to_owned))
        .filter(|n| !n.starts_with("view_"))
        .collect();
    names.sort();
    assert_eq!(names, SCENARIOS.map(String::from).to_vec());
}
