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

//! R11: `gadk::agentanalytics` (feature `agentanalytics`, not on by default) is
//! `gadk_agentanalytics`.

use gadk::agentanalytics::{BigQueryClient, Config, Error, events_schema};
use pretty_assertions::assert_eq;

/// Compiles only if each façade path names the `gadk_agentanalytics` item.
#[test]
fn facade_agentanalytics_paths_are_gadk_agentanalytics() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Config>(None, None::<&gadk_agentanalytics::Config>);
    same::<Error>(None, None::<&gadk_agentanalytics::Error>);
    same::<BigQueryClient>(None, None::<&gadk_agentanalytics::BigQueryClient>);
}

/// `Config::default()` is Go's `DefaultConfig()` through the façade, and its view definitions
/// follow Go's project-ID validation.
#[test]
fn facade_agentanalytics_default_config() {
    let config = Config::default();
    assert_eq!(config.table_name, "agent_events");
    assert_eq!(
        config.view_definitions().unwrap_err().to_string(),
        r#"invalid BigQuery project ID """#
    );
    let config = Config {
        project_id: "my-project".to_owned(),
        dataset_id: "analytics".to_owned(),
        ..Config::default()
    };
    assert_eq!(config.view_definitions().unwrap().len(), 25);
    assert_eq!(events_schema()[0].name, "timestamp");
}
