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

//! Golden tests for `Config::view_definitions` (spec R5) against the Go probe's
//! `tests/fixtures/views/view_definitions.json` (written by `scripts/gen-go-agentanalytics`):
//! every view name, event type and `CREATE OR REPLACE VIEW` statement is byte-equal to Go's, and
//! every error text equals Go's `err.Error()`.
//!
//! The `Config.ViewDefinitions` cases build the receiver from its Config projection. The
//! `viewDefinitionsWithDenied` cases call a crate-internal function; here they run through the
//! public `Config::view_definitions` whenever that is the same call (Go's `applyDefaults` only
//! fills an empty dataset ID, table name or view prefix, and an empty view prefix means `"v"` in
//! both). The two cases with an empty dataset ID or table name are covered by the
//! crate-internal golden test in `src/views.rs` instead.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

use gadk_agentanalytics::{Config, ViewDefinition};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn fixture(rel: &str) -> Value {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", rel]
        .iter()
        .collect();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn cases(rel: &str) -> Vec<Value> {
    fixture(rel)["cases"]
        .as_array()
        .expect("cases array")
        .clone()
}

fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .map(|s| s.as_str().expect("string item").to_owned())
            .collect(),
        other => panic!("not a list: {other}"),
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} is not a string in {v}"))
}

/// The fields of a Config projection that `ViewDefinitions` reads, over Go's zero `Config{}`.
fn config_from_projection(p: &Value) -> Config {
    Config {
        project_id: str_field(p, "ProjectID").to_owned(),
        dataset_id: str_field(p, "DatasetID").to_owned(),
        table_name: str_field(p, "TableName").to_owned(),
        view_prefix: str_field(p, "ViewPrefix").to_owned(),
        payload_column_denylist: string_list(&p["PayloadColumnDenylist"]),
        ..Config::zero()
    }
}

/// Compares a `view_definitions` result with a fixture `result` (`{views}` or `{error}`).
fn assert_result(
    name: &str,
    got: Result<Vec<ViewDefinition>, gadk_agentanalytics::Error>,
    want: &Value,
) {
    if let Some(err) = want.get("error") {
        let got_err = got
            .err()
            .unwrap_or_else(|| panic!("{name}: want error {err}, got views"));
        assert_eq!(
            got_err.to_string(),
            err.as_str().unwrap(),
            "{name}: error text"
        );
        return;
    }
    let views = got.unwrap_or_else(|e| panic!("{name}: unexpected error {e}"));
    let want_views = want["views"].as_array().expect("views array");
    assert_eq!(views.len(), want_views.len(), "{name}: view count");
    for (got, want) in views.iter().zip(want_views) {
        assert_eq!(got.name, str_field(want, "name"), "{name}: view name");
        assert_eq!(
            got.event_type,
            str_field(want, "event_type"),
            "{name}: {} event type",
            got.name
        );
        assert_eq!(
            got.query,
            str_field(want, "query"),
            "{name}: {} query",
            got.name
        );
    }
}

#[test]
fn config_view_definitions_match_go() {
    let mut checked = 0;
    for case in cases("views/view_definitions.json") {
        if case["fn"] != "Config.ViewDefinitions" {
            continue;
        }
        let name = str_field(&case, "name");
        let config = config_from_projection(&case["config"]);
        assert_result(name, config.view_definitions(), &case["result"]);
        checked += 1;
    }
    assert_eq!(checked, 13, "Config.ViewDefinitions cases");
}

#[test]
fn view_definitions_with_denied_cases_match_go_through_config() {
    let mut checked = 0;
    let mut not_reproducible = Vec::new();
    for case in cases("views/view_definitions.json") {
        if case["fn"] != "viewDefinitionsWithDenied" {
            continue;
        }
        let name = str_field(&case, "name");
        let args = &case["args"];
        let config = Config {
            project_id: str_field(args, "projectID").to_owned(),
            dataset_id: str_field(args, "datasetID").to_owned(),
            table_name: str_field(args, "tableName").to_owned(),
            view_prefix: str_field(args, "viewPrefix").to_owned(),
            payload_column_denylist: string_list(&args["denied"]),
            ..Config::zero()
        };
        if config.dataset_id.is_empty() || config.table_name.is_empty() {
            // applyDefaults would fill these, so the Config path is a different call.
            not_reproducible.push(name.to_owned());
            continue;
        }
        assert_result(name, config.view_definitions(), &case["result"]);
        checked += 1;
    }
    assert_eq!(
        checked, 24,
        "viewDefinitionsWithDenied cases run through Config"
    );
    assert_eq!(not_reproducible, ["err_empty_dataset", "err_empty_table"]);
}

#[test]
fn view_event_types_are_gos_event_types_with_views() {
    let want = string_list(&fixture("config/constants.json")["eventTypesWithViews"]);
    assert_eq!(want.len(), 25);
    let views = Config {
        project_id: "p".to_owned(),
        ..Config::default()
    }
    .view_definitions()
    .unwrap();
    let got: Vec<String> = views.iter().map(|v| v.event_type.clone()).collect();
    assert_eq!(got, want);
    let names: Vec<String> = views.iter().map(|v| v.name.clone()).collect();
    let want_names: Vec<String> = want
        .iter()
        .map(|t| format!("v_{}", t.to_lowercase()))
        .collect();
    assert_eq!(names, want_names);
}
