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

use gadk_agentanalytics::bigquery::fake::RecordingLogger;
use gadk_agentanalytics::{
    Config, Context, Error, Logger, TableMetadataOps, TableOps, ensure_table_and_schema,
    maybe_upgrade_schema, project_events_schema,
};
use pretty_assertions::assert_eq;

#[path = "support/wire.rs"]
mod wire;

use wire::{Replay, assert_request, client_for, load, scenario_names};

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

/// The base `Config` description every `ensureTableAndSchema` scenario starts from.
const BASE_CONFIG: &str = "DefaultConfig() with ProjectID \"test-project\" and a recording Logger";

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
    let replay = Replay::start(&fixture);
    let client = client_for(&fixture, &replay.server);
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
    let requests = replay.server.requests();
    assert_eq!(requests.len(), exchanges.len(), "{name}: request count");
    for (i, (got, want)) in requests.iter().zip(exchanges).enumerate() {
        assert_request(name, i, got, &want["request"], &replay.job_id());
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
    let names: Vec<String> = scenario_names()
        .into_iter()
        .filter(|n| !n.starts_with("view_"))
        .collect();
    assert_eq!(names, SCENARIOS.map(String::from).to_vec());
}
