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

//! Port of ADK-Go `plugin/agentanalytics/views_test.go` (ledger `docs/parity/7b-1.md`).
//! Go's `fakeViewRunner` / `recordingLogger` are `bigquery::fake::{FakeViewRunner,
//! RecordingLogger}`; Go's `mockTransport` HTTP client is a `FakeServer` behind the client's
//! endpoint. The SQL-evaluation helpers of the Go file (`extractExtraProjections`,
//! `extractSQLJSONExprs`, `resolveJSONPath`, `splitJSONPathSegments`,
//! `parseBigQueryTimestampString`, `evalCompactionTimestampSQL`) are ported below for the
//! tests that use them.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use pretty_assertions::assert_eq;
use regex::Regex;
use serde_json::{Map, Value};

use crate::bigquery::BigQueryClient;
use crate::bigquery::fake::{FakeError, FakeViewRunner, OnRun, RecordingLogger};
use crate::config::{ClientOptions, Config};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error};
use crate::event_types::*;
use crate::redaction::{COLUMN_ATTRIBUTES, COLUMN_CONTENT, COLUMN_LATENCY_MS};
use crate::views::{
    BigQueryViewRunner, MAX_BIGQUERY_IDENTIFIER_LEN, VIEW_COMMON_COLUMNS, ViewDefinition,
    ViewRunner, contains_identifier_token, references_denied_column, sync_analytics_views,
    view_definitions_with_denied,
};
// The plugin's view lifecycle.
use std::sync::atomic::AtomicBool;

use gadk_model::rest::test_util::ScriptedClient;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::config::Logger;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::new_test_plugin_with_config;
use crate::plugin::{
    BigQueryAgentAnalyticsPlugin, PLUGIN_CLOSED_TEXT, as_bigquery_agent_analytics_plugin,
    new_bigquery_agent_analytics_plugin_with_clients,
};
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::views::{EVENT_VIEW_SPECS, VIEW_DDL_TIMEOUT};

// parity: adapted: Go json.Unmarshal of a Config → struct construction (Config is not Deserialize, spec R1)
#[test]
fn test_config_create_views() {
    // Go: json.Unmarshal(`{"Enabled": true, "DisableCreateViews": true}`, &decoded).
    let decoded = Config {
        enabled: true,
        disable_create_views: true,
        ..Config::zero()
    };
    let tests: Vec<(&str, Config, bool, &str)> = vec![
        (
            "DefaultConfig creates views with v prefix",
            Config::default(),
            true,
            "v",
        ),
        (
            "Config literal creates views by default with v prefix",
            Config {
                enabled: true,
                ..Config::zero()
            }
            .apply_defaults(),
            true,
            "v",
        ),
        (
            "Config literal with DisableCreateViews disables view creation",
            Config {
                enabled: true,
                disable_create_views: true,
                view_prefix: "custom_v".to_owned(),
                ..Config::zero()
            }
            .apply_defaults(),
            false,
            "custom_v",
        ),
        (
            "DefaultConfig with DisableCreateViews disables view creation",
            Config {
                disable_create_views: true,
                ..Config::default()
            },
            false,
            "v",
        ),
        (
            "Config decoded from JSON with DisableCreateViews disables view creation",
            decoded.apply_defaults(),
            false,
            "v",
        ),
    ];
    for (name, config, want_create, want_prefix) in tests {
        assert_eq!(config.create_views(), want_create, "{name}: createViews()");
        assert_eq!(config.view_prefix, want_prefix, "{name}: ViewPrefix");
    }
}

#[test]
fn test_view_definitions_order_and_sql_structure() {
    const PROJECT_ID: &str = "my-domain.com:analytics-proj-1";
    const DATASET_ID: &str = "agent_analytics";
    const TABLE_NAME: &str = "agent_events";

    let defs =
        view_definitions_with_denied(PROJECT_ID, DATASET_ID, TABLE_NAME, "", &Default::default())
            .unwrap();

    let want_event_types = [
        EVENT_USER_MESSAGE_RECEIVED,
        EVENT_LLM_REQUEST,
        EVENT_LLM_RESPONSE,
        EVENT_LLM_ERROR,
        EVENT_TOOL_STARTING,
        EVENT_TOOL_COMPLETED,
        EVENT_TOOL_ERROR,
        EVENT_AGENT_STARTING,
        EVENT_AGENT_COMPLETED,
        EVENT_AGENT_ERROR,
        EVENT_INVOCATION_STARTING,
        EVENT_INVOCATION_COMPLETED,
        EVENT_INVOCATION_ERROR,
        EVENT_STATE_DELTA,
        EVENT_HITL_CREDENTIAL_REQUEST,
        EVENT_HITL_CONFIRMATION_REQUEST,
        EVENT_HITL_INPUT_REQUEST,
        EVENT_A2A_INTERACTION,
        EVENT_AGENT_RESPONSE,
        EVENT_AGENT_TRANSFER,
        EVENT_EVENT_COMPACTION,
        EVENT_AGENT_STATE_CHECKPOINT,
        EVENT_TOOL_PAUSED,
        EVENT_NODE_OUTPUT,
        EVENT_NODE_ERROR,
    ];
    assert_eq!(defs.len(), want_event_types.len(), "len(ViewDefinitions)");

    // Deterministic ordering across repeated calls.
    let defs2 =
        view_definitions_with_denied(PROJECT_ID, DATASET_ID, TABLE_NAME, "", &Default::default())
            .unwrap();
    assert_eq!(defs, defs2, "ViewDefinitions() is not deterministic");

    for (i, def) in defs.iter().enumerate() {
        let want_type = want_event_types[i];
        let want_name = format!("v_{}", want_type.to_lowercase());
        assert_eq!(def.event_type, want_type, "defs[{i}].EventType");
        assert_eq!(def.name, want_name, "defs[{i}].Name");
        let want_header =
            format!("CREATE OR REPLACE VIEW `{PROJECT_ID}.{DATASET_ID}.{want_name}` AS\nSELECT\n");
        assert!(
            def.query.starts_with(&want_header),
            "defs[{i}] ({}) query does not start with {want_header:?}:\n{}",
            def.name,
            def.query
        );
        let want_footer = format!(
            "\nFROM\n  `{PROJECT_ID}.{DATASET_ID}.{TABLE_NAME}`\nWHERE\n  event_type = '{want_type}'\n"
        );
        assert!(
            def.query.ends_with(&want_footer),
            "defs[{i}] ({}) query does not end with {want_footer:?}:\n{}",
            def.name,
            def.query
        );
        for common_col in VIEW_COMMON_COLUMNS {
            assert!(
                contains_identifier_token(&def.query, common_col),
                "defs[{i}] ({}) query missing common column {common_col:?}:\n{}",
                def.name,
                def.query
            );
        }
    }

    // v_user_message_received matches adk-python (common columns only).
    let (_, aliases) = extract_extra_projections(&defs[0].query);
    assert!(
        aliases.is_empty(),
        "v_user_message_received extra projections = {aliases:?}, want empty (matching adk-python)"
    );
}

#[test]
fn test_view_definitions_exact_projection_parity_with_adk_python() {
    const PROJECT_ID: &str = "my-proj";
    const DATASET_ID: &str = "agent_analytics";
    const TABLE_NAME: &str = "agent_events";
    let defs = Config {
        project_id: PROJECT_ID.to_owned(),
        dataset_id: DATASET_ID.to_owned(),
        table_name: TABLE_NAME.to_owned(),
        ..Config::zero()
    }
    .view_definitions()
    .unwrap();

    let want_common = [
        "timestamp",
        "event_id",
        "event_type",
        "agent",
        "session_id",
        "invocation_id",
        "user_id",
        "trace_id",
        "span_id",
        "parent_span_id",
        "status",
        "error_message",
        "is_truncated",
    ];

    let want_extras_list: [(&str, &[&str]); 25] = [
        ("USER_MESSAGE_RECEIVED", &[]),
        (
            "LLM_REQUEST",
            &[
                "JSON_VALUE(attributes, '$.model') AS model",
                "content AS request_content",
                "JSON_QUERY(attributes, '$.llm_config') AS llm_config",
                "JSON_QUERY(attributes, '$.tools') AS tools",
            ],
        ),
        (
            "LLM_RESPONSE",
            &[
                "JSON_QUERY(content, '$.response') AS response",
                "CAST(JSON_VALUE(content, '$.usage.prompt') AS INT64) AS usage_prompt_tokens",
                "CAST(JSON_VALUE(content, '$.usage.completion') AS INT64) AS usage_completion_tokens",
                "CAST(JSON_VALUE(content, '$.usage.total') AS INT64) AS usage_total_tokens",
                "CAST(JSON_VALUE(attributes, '$.usage_metadata.cached_content_token_count') AS INT64) AS usage_cached_tokens",
                "CAST(JSON_VALUE(attributes, '$.usage_metadata.thoughts_token_count') AS INT64) AS usage_thinking_tokens",
                "CAST(JSON_VALUE(attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64) AS usage_tool_use_tokens",
                "SAFE_DIVIDE(CAST(JSON_VALUE(attributes, '$.usage_metadata.cached_content_token_count') AS INT64),CAST(JSON_VALUE(content, '$.usage.prompt') AS INT64)) AS context_cache_hit_rate",
                "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
                "CAST(JSON_VALUE(latency_ms, '$.time_to_first_token_ms') AS INT64) AS ttft_ms",
                "JSON_VALUE(attributes, '$.model_version') AS model_version",
                "JSON_QUERY(attributes, '$.usage_metadata') AS usage_metadata",
                "JSON_QUERY(attributes, '$.cache_metadata') AS cache_metadata",
                "JSON_VALUE(attributes, '$.cache_type') AS cache_type",
                "JSON_VALUE(attributes, '$.finish_reason') AS finish_reason",
            ],
        ),
        (
            "LLM_ERROR",
            &["CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms"],
        ),
        (
            "TOOL_STARTING",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.args') AS tool_args",
                "JSON_VALUE(content, '$.tool_origin') AS tool_origin",
            ],
        ),
        (
            "TOOL_COMPLETED",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.result') AS tool_result",
                "JSON_VALUE(content, '$.tool_origin') AS tool_origin",
                "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
                "JSON_VALUE(attributes, '$.adk.pause_kind') AS pause_kind",
                "JSON_VALUE(attributes, '$.adk.function_call_id') AS function_call_id",
            ],
        ),
        (
            "TOOL_ERROR",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.args') AS tool_args",
                "JSON_VALUE(content, '$.tool_origin') AS tool_origin",
                "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
            ],
        ),
        (
            "AGENT_STARTING",
            &[
                r#"COALESCE(JSON_VALUE(content, '$.text_summary'), IF(REGEXP_CONTAINS(JSON_VALUE(content, '$'), r'^(?:<(?:function|bound method) |<.+ at 0x[0-9a-fA-F]+>|functools\.partial\()'), NULL, JSON_VALUE(content, '$'))) AS agent_instruction"#,
                "JSON_VALUE(attributes, '$.instruction_source') AS instruction_source",
                "JSON_VALUE(attributes, '$.static_instruction') AS static_instruction",
                "JSON_VALUE(attributes, '$.agent_description') AS agent_description",
                "JSON_VALUE(attributes, '$.model') AS model",
                "JSON_QUERY(attributes, '$.sub_agents') AS sub_agents",
            ],
        ),
        (
            "AGENT_COMPLETED",
            &["CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms"],
        ),
        (
            "AGENT_ERROR",
            &[
                "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
                "JSON_VALUE(content, '$.error_traceback') AS error_traceback",
            ],
        ),
        ("INVOCATION_STARTING", &[]),
        ("INVOCATION_COMPLETED", &[]),
        (
            "INVOCATION_ERROR",
            &["JSON_VALUE(content, '$.error_traceback') AS error_traceback"],
        ),
        (
            "STATE_DELTA",
            &["JSON_QUERY(attributes, '$.state_delta') AS state_delta"],
        ),
        (
            "HITL_CREDENTIAL_REQUEST",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.args') AS tool_args",
            ],
        ),
        (
            "HITL_CONFIRMATION_REQUEST",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.args') AS tool_args",
            ],
        ),
        (
            "HITL_INPUT_REQUEST",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.args') AS tool_args",
            ],
        ),
        (
            "A2A_INTERACTION",
            &[
                "content AS response_content",
                r#"JSON_VALUE(attributes, '$.a2a_metadata."a2a:task_id"') AS a2a_task_id"#,
                r#"JSON_VALUE(attributes, '$.a2a_metadata."a2a:context_id"') AS a2a_context_id"#,
                r#"JSON_QUERY(attributes, '$.a2a_metadata."a2a:request"') AS a2a_request"#,
                r#"JSON_QUERY(attributes, '$.a2a_metadata."a2a:response"') AS a2a_response"#,
            ],
        ),
        (
            "AGENT_RESPONSE",
            &[
                "JSON_VALUE(content, '$.response') AS response_text",
                "JSON_VALUE(attributes, '$.source_event_id') AS source_event_id",
                "JSON_VALUE(attributes, '$.source_event_author') AS source_event_author",
                "JSON_VALUE(attributes, '$.source_event_branch') AS source_event_branch",
            ],
        ),
        (
            "AGENT_TRANSFER",
            &[
                "JSON_VALUE(content, '$.from_agent') AS from_agent",
                "JSON_VALUE(content, '$.to_agent') AS to_agent",
                "JSON_VALUE(attributes, '$.adk.source_event_id') AS source_event_id",
            ],
        ),
        (
            "EVENT_COMPACTION",
            &[
                r#"COALESCE(SAFE_CAST(JSON_VALUE(content, '$.start_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.start_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6) AS start_seconds"#,
                r#"COALESCE(SAFE_CAST(JSON_VALUE(content, '$.end_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.end_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6) AS end_seconds"#,
                r#"SAFE.TIMESTAMP_MICROS(SAFE_CAST(SAFE_MULTIPLY(COALESCE(SAFE_CAST(JSON_VALUE(content, '$.start_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.start_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6), 1000000) AS INT64)) AS window_start"#,
                r#"SAFE.TIMESTAMP_MICROS(SAFE_CAST(SAFE_MULTIPLY(COALESCE(SAFE_CAST(JSON_VALUE(content, '$.end_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.end_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6), 1000000) AS INT64)) AS window_end"#,
                "JSON_QUERY(content, '$.compacted_content') AS compacted_content",
            ],
        ),
        (
            "AGENT_STATE_CHECKPOINT",
            &[
                "JSON_QUERY(content, '$.agent_state') AS agent_state",
                "JSON_TYPE(JSON_QUERY(content, '$.agent_state')) AS agent_state_type",
                "SAFE_CAST(JSON_VALUE(content, '$.end_of_agent') AS BOOL) AS end_of_agent",
                "JSON_VALUE(attributes, '$.adk.source_event_id') AS source_event_id",
            ],
        ),
        (
            "TOOL_PAUSED",
            &[
                "JSON_VALUE(content, '$.tool') AS tool_name",
                "JSON_QUERY(content, '$.args') AS tool_args",
                "JSON_VALUE(attributes, '$.adk.pause_kind') AS pause_kind",
                "JSON_VALUE(attributes, '$.adk.function_call_id') AS function_call_id",
            ],
        ),
        (
            "NODE_OUTPUT",
            &[
                "JSON_VALUE(attributes, '$.adk.node.path') AS node_path",
                "JSON_VALUE(attributes, '$.adk.node.run_id') AS node_run_id",
                "JSON_VALUE(attributes, '$.adk.node.parent_run_id') AS node_parent_run_id",
                "content AS output",
            ],
        ),
        (
            "NODE_ERROR",
            &[
                "JSON_VALUE(attributes, '$.adk.node.path') AS node_path",
                "JSON_VALUE(attributes, '$.adk.node.run_id') AS node_run_id",
                "JSON_VALUE(attributes, '$.adk.node.parent_run_id') AS node_parent_run_id",
                "JSON_VALUE(content, '$.error_code') AS error_code",
            ],
        ),
    ];
    let want_extras: HashMap<&str, &[&str]> = want_extras_list.into_iter().collect();

    assert_eq!(defs.len(), want_extras.len(), "len(ViewDefinitions)");
    for def in &defs {
        let extras = want_extras
            .get(def.event_type.as_str())
            .unwrap_or_else(|| panic!("unexpected view EventType {:?}", def.event_type));
        let cols: Vec<&str> = want_common.iter().chain(extras.iter()).copied().collect();
        let want_view_name = format!("v_{}", def.event_type.to_lowercase());
        let want_query = format!(
            "CREATE OR REPLACE VIEW `{PROJECT_ID}.{DATASET_ID}.{want_view_name}` AS\nSELECT\n  {}\nFROM\n  `{PROJECT_ID}.{DATASET_ID}.{TABLE_NAME}`\nWHERE\n  event_type = '{}'\n",
            cols.join(",\n  "),
            def.event_type
        );
        assert_eq!(
            want_query, def.query,
            "{} ({}) query mismatch",
            def.name, def.event_type
        );
    }
}

#[test]
fn test_view_definitions_custom_view_prefix_and_multi_table_disjoint() {
    let none = Default::default();
    let defs_a =
        view_definitions_with_denied("proj", "shared_ds", "events_a", "v_agent_a", &none).unwrap();
    let defs_b =
        view_definitions_with_denied("proj", "shared_ds", "events_b", "v_agent_b", &none).unwrap();

    let mut seen_names = std::collections::HashSet::new();
    for d in &defs_a {
        assert!(
            d.name.starts_with("v_agent_a_"),
            "defsA view name = {:?}, want prefix v_agent_a_",
            d.name
        );
        assert!(
            d.query.contains("`proj.shared_ds.events_a`"),
            "defsA view {} query does not target events_a:\n{}",
            d.name,
            d.query
        );
        seen_names.insert(d.name.clone());
    }
    for d in &defs_b {
        assert!(
            d.name.starts_with("v_agent_b_"),
            "defsB view name = {:?}, want prefix v_agent_b_",
            d.name
        );
        assert!(
            d.query.contains("`proj.shared_ds.events_b`"),
            "defsB view {} query does not target events_b:\n{}",
            d.name,
            d.query
        );
        assert!(
            !seen_names.contains(&d.name),
            "view name {:?} collided between events_a and events_b",
            d.name
        );
    }
}

#[test]
fn test_view_definitions_identifier_validation() {
    let none = Default::default();
    if let Err(err) = view_definitions_with_denied("proj", "ds", "agent events", "v", &none) {
        panic!("ViewDefinitions() with space in tableName error = {err}, want nil");
    }
    // The longest names BigQuery accepts are valid.
    if let Err(err) = view_definitions_with_denied(
        "proj",
        &"d".repeat(MAX_BIGQUERY_IDENTIFIER_LEN),
        &"t".repeat(MAX_BIGQUERY_IDENTIFIER_LEN),
        "v",
        &none,
    ) {
        panic!(
            "viewDefinitionsWithDenied() with {MAX_BIGQUERY_IDENTIFIER_LEN}-byte dataset ID and table name error = {err}, want nil"
        );
    }
    // The public entry point applies the same checks after the defaults.
    match (Config {
        project_id: "proj".to_owned(),
        dataset_id: "ds".to_owned(),
        table_name: "v_llm_request".to_owned(),
        ..Config::zero()
    })
    .view_definitions()
    {
        Err(err) if err.to_string().contains("collides with the events table") => {}
        other => panic!(
            "Config.ViewDefinitions() with TableName v_llm_request = {other:?}, want collision error"
        ),
    }

    let long_table = "t".repeat(MAX_BIGQUERY_IDENTIFIER_LEN + 1);
    let long_dataset = "d".repeat(MAX_BIGQUERY_IDENTIFIER_LEN + 1);
    let long_prefix = "v".repeat(MAX_BIGQUERY_IDENTIFIER_LEN);
    // (name, projectID, datasetID, tableName, viewPrefix, wantErr)
    let tests: [(&str, &str, &str, &str, &str, &str); 14] = [
        (
            "empty projectID",
            "",
            "ds",
            "tbl",
            "",
            "invalid BigQuery project ID",
        ),
        (
            "backtick in projectID",
            "proj` --",
            "ds",
            "tbl",
            "",
            "invalid BigQuery project ID",
        ),
        (
            "empty datasetID",
            "proj",
            "",
            "tbl",
            "",
            "invalid BigQuery dataset ID",
        ),
        (
            "dot in datasetID",
            "proj",
            "ds.other",
            "tbl",
            "",
            "invalid BigQuery dataset ID",
        ),
        (
            "hyphen in datasetID",
            "proj",
            "test-dataset",
            "tbl",
            "",
            "invalid BigQuery dataset ID",
        ),
        (
            "empty tableName",
            "proj",
            "ds",
            "",
            "",
            "invalid BigQuery table name",
        ),
        (
            "semicolon in tableName",
            "proj",
            "ds",
            "tbl;DROP",
            "",
            "invalid BigQuery table name",
        ),
        (
            "space in viewPrefix",
            "proj",
            "ds",
            "tbl",
            "v prefix",
            "invalid BigQuery view prefix",
        ),
        (
            "whitespace-only tableName",
            "proj",
            "ds",
            "   ",
            "",
            "invalid BigQuery table name",
        ),
        (
            "tableName longer than 1024 bytes",
            "proj",
            "ds",
            &long_table,
            "",
            "invalid BigQuery table name",
        ),
        (
            "datasetID longer than 1024 bytes",
            "proj",
            &long_dataset,
            "tbl",
            "",
            "invalid BigQuery dataset ID",
        ),
        (
            "view names longer than 1024 bytes",
            "proj",
            "ds",
            "tbl",
            &long_prefix,
            "longer than 1024 bytes",
        ),
        (
            "view name equal to tableName",
            "proj",
            "ds",
            "v_llm_request",
            "",
            r#"BigQuery view name "v_llm_request" collides with the events table "v_llm_request""#,
        ),
        (
            "view name equal to tableName but for case",
            "proj",
            "ds",
            "V_Tool_Starting",
            "",
            r#"BigQuery view name "v_tool_starting" collides with the events table "V_Tool_Starting""#,
        ),
    ];
    for (name, project_id, dataset_id, table_name, view_prefix, want_err) in tests {
        match view_definitions_with_denied(project_id, dataset_id, table_name, view_prefix, &none) {
            Err(err) if err.to_string().contains(want_err) => {}
            other => panic!(
                "{name}: ViewDefinitions() = {:?}, want error containing {want_err:?}",
                other.map(|d| d.len())
            ),
        }
    }
}

#[test]
fn test_view_definitions_payload_column_denylist_projection() {
    fn by_event_type(defs: Vec<ViewDefinition>) -> HashMap<String, ViewDefinition> {
        defs.into_iter()
            .map(|d| (d.event_type.clone(), d))
            .collect()
    }

    // content denied drops content expressions
    {
        let cfg = Config {
            project_id: "proj".to_owned(),
            dataset_id: "ds".to_owned(),
            table_name: "tbl".to_owned(),
            payload_column_denylist: vec![COLUMN_CONTENT.to_owned()],
            ..Config::default()
        };
        let m = by_event_type(cfg.view_definitions().unwrap());
        let req = &m[EVENT_LLM_REQUEST].query;
        let resp = &m[EVENT_LLM_RESPONSE].query;
        assert!(
            !req.contains("request_content"),
            "v_llm_request retained request_content when content is denied:\n{req}"
        );
        assert!(
            req.contains("AS model"),
            "v_llm_request dropped attributes-backed model column when only content is denied:\n{req}"
        );
        assert!(
            !resp.contains("context_cache_hit_rate") && !resp.contains("usage_prompt_tokens"),
            "v_llm_response retained content-dependent columns when content is denied:\n{resp}"
        );
        assert!(
            resp.contains("AS usage_cached_tokens") && resp.contains("AS total_ms"),
            "v_llm_response dropped attributes/latency_ms columns when only content is denied:\n{resp}"
        );
    }

    // attributes denied drops attributes expressions
    {
        let defs =
            view_definitions_with_denied("proj", "ds", "tbl", "v", &new_set(&[COLUMN_ATTRIBUTES]))
                .unwrap();
        let m = by_event_type(defs);
        let req = &m[EVENT_LLM_REQUEST].query;
        assert!(
            !req.contains("AS model") && !req.contains("AS llm_config"),
            "v_llm_request retained attributes columns when attributes is denied:\n{req}"
        );
        assert!(
            req.contains("content AS request_content"),
            "v_llm_request dropped content column when only attributes is denied:\n{req}"
        );
        let delta = &m[EVENT_STATE_DELTA].query;
        assert!(
            !delta.contains("state_delta\nFROM"),
            "v_state_delta retained state_delta when attributes is denied:\n{delta}"
        );
        let output = &m[EVENT_NODE_OUTPUT].query;
        assert!(
            !output.contains("AS node_path"),
            "v_node_output retained node_path when attributes is denied:\n{output}"
        );
        assert!(
            output.contains("content AS output"),
            "v_node_output dropped output when only attributes is denied:\n{output}"
        );
    }

    // latency_ms denied drops latency expressions
    {
        let defs =
            view_definitions_with_denied("proj", "ds", "tbl", "v", &new_set(&[COLUMN_LATENCY_MS]))
                .unwrap();
        let m = by_event_type(defs);
        let resp = &m[EVENT_LLM_RESPONSE].query;
        assert!(
            !resp.contains("AS total_ms") && !resp.contains("AS ttft_ms"),
            "v_llm_response retained latency_ms columns when latency_ms is denied:\n{resp}"
        );
        assert!(
            resp.contains("AS usage_prompt_tokens"),
            "v_llm_response dropped content columns when only latency_ms is denied:\n{resp}"
        );
        let err = &m[EVENT_LLM_ERROR].query;
        assert!(
            !err.contains("AS total_ms"),
            "v_llm_error retained total_ms when latency_ms is denied:\n{err}"
        );
    }

    // containsIdentifierToken respects token boundaries, aliases, and quoted JSON paths
    {
        assert!(!contains_identifier_token("my_content", "content"));
        assert!(!contains_identifier_token("content_parts", "content"));
        assert!(!contains_identifier_token("something", ""));
        let content = new_set(&[COLUMN_CONTENT]);
        assert!(!references_denied_column(
            "JSON_VALUE(attributes, '$.model') AS content",
            &content
        ));
        assert!(!references_denied_column(
            "JSON_VALUE(attributes, '$.content')",
            &content
        ));
        assert!(!references_denied_column(
            "JSON_QUERY(attributes, '$.nested.content') AS x",
            &content
        ));
    }
}

// parity: adapted: Go's native compaction record (a struct with time.Time fields) is custom metadata, which is JSON in gadk: the record's JSON encoding; Go's t.Errorf checks are collected and asserted at the end of each part
#[tokio::test]
async fn test_views_json_paths_match_emitted_payloads() {
    use crate::parity_tests::plugin_agentanalytics_batch_processor::support::inject_stream;
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
        MockCtx, decode_arrow_rows, default_config, event_types, json_column,
        new_test_plugin_with_config, only_row_of_type, queue_processor, rows_of_type,
        until_event_type,
    };
    use crate::plugin::{AnalyticsPlugin, EventData, Payload, USER_AUTHOR};
    use crate::row::{QueuedRow, RowValue};
    use crate::trace::CallbackCtx;
    use crate::write::stream_writer::tests::{MockCounts, MockStream};
    use gadk_genai::{FunctionCall, Part};
    use serde_json::json;

    let none = Default::default();
    let defs =
        view_definitions_with_denied("test-project", "test_dataset", "test-table", "v", &none)
            .unwrap_or_else(|e| panic!("ViewDefinitions() error = {e}"));
    let defs_by_type: HashMap<String, &ViewDefinition> =
        defs.iter().map(|d| (d.event_type.clone(), d)).collect();

    // Part 1: Emit events through live plugin callbacks (including all session.Event signal
    // types via OnEventCallback), decode the Arrow IPC RecordBatch rows, and verify that the
    // view JSON paths resolve to valid scalars/objects and parse under BigQuery CAST rules.
    {
        let tp = new_test_plugin_with_config(|c| {
            c.custom_metadata_allowlist = vec![
                "adk_compaction".into(),
                "adk_agent_state".into(),
                "end_of_agent".into(),
            ];
        })
        .await;
        let p = &tp.plugin;
        let mut m_ctx: MockCtx = tp.ctx.with_function_call("call-42");
        // Go's one mCtx is one agent and one activation: every callback gets a clone of the
        // same ADK contexts.
        m_ctx.agent = Some(m_ctx.agent());
        let (m_ictx, m_cctx) = (m_ctx.invocation(), m_ctx.context());

        let lookup_tool: Arc<dyn gadk_agent::Tool> = Arc::new(DeclaredTool {
            name: "lookup",
            description: "Looks up facts",
            decl: gadk_genai::FunctionDeclaration {
                name: "lookup".into(),
                description: "Looks up facts".into(),
                parameters_json_schema: Some(json!({"type": "object"})),
                ..Default::default()
            },
        });

        let llm_req = gadk_model::LlmRequest {
            model: "gemini-2.5-pro".into(),
            contents: vec![text_content("user", "what is 6*7?")],
            config: Some(gadk_genai::GenerateContentConfig {
                system_instruction: Some(text_content("", "answer concisely")),
                temperature: Some(0.2),
                ..Default::default()
            }),
            tools: HashMap::from([(
                "lookup".to_owned(),
                Arc::new(Arc::clone(&lookup_tool)) as Arc<dyn std::any::Any + Send + Sync>,
            )]),
            ..Default::default()
        };
        let llm_res = gadk_model::LlmResponse {
            content: Some(text_content("model", "42")),
            usage_metadata: Some(gadk_genai::GenerateContentResponseUsageMetadata {
                prompt_token_count: 100,
                candidates_token_count: 10,
                total_token_count: 115,
                cached_content_token_count: 20,
                thoughts_token_count: 5,
                tool_use_prompt_token_count: 15,
                ..Default::default()
            }),
            model_version: "gemini-2.5-pro-001".into(),
            finish_reason: Some(gadk_genai::FinishReason::Stop),
            turn_complete: true,
            ..Default::default()
        };

        let compaction_start = DateTime::parse_from_rfc3339("2026-10-10T01:00:00.5Z")
            .unwrap()
            .with_timezone(&Utc);
        let compaction_end = DateTime::parse_from_rfc3339("2026-10-10T01:05:00.25Z")
            .unwrap()
            .with_timezone(&Utc);
        // Go's compactionRecord{StartTimestamp, EndTimestamp, CompactedContent} as JSON.
        let compaction_record_val = json!({
            "StartTimestamp": "2026-10-10T01:00:00.5Z",
            "EndTimestamp": "2026-10-10T01:05:00.25Z",
            "CompactedContent": text_content("model", "compacted summary"),
        });
        let event = |id: &str| gadk_session::Event {
            id: id.into(),
            author: "root-agent".into(),
            ..gadk_session::Event::default()
        };
        let mut compaction_event = event("ev-native-compaction");
        compaction_event
            .response
            .custom_metadata
            .insert("adk_compaction".into(), compaction_record_val.clone());
        compaction_event
            .response
            .custom_metadata
            .insert("compaction".into(), compaction_record_val);

        // Map-shaped CustomMetadata["adk_compaction"] with 9-digit nanosecond RFC3339
        // strings is converted to float64 epoch seconds so adk-python's v_event_compaction
        // view can also read the row.
        let nano_compaction_map = json!({
            "start_timestamp": "2026-10-10T01:00:00.500000123Z",
            "end_timestamp": "2026-10-10T01:05:00.250000987Z",
            "compacted_content": "compacted summary",
        });
        let mut nano_string_compaction_event = event("ev-nano-string-compaction");
        nano_string_compaction_event
            .response
            .custom_metadata
            .insert("adk_compaction".into(), nano_compaction_map.clone());
        nano_string_compaction_event
            .response
            .custom_metadata
            .insert("compaction".into(), nano_compaction_map);
        let mut unparseable_string_compaction_event = event("ev-unparseable-string-compaction");
        unparseable_string_compaction_event
            .response
            .custom_metadata
            .insert(
                "adk_compaction".into(),
                json!({
                    "start_timestamp": "not-a-timestamp",
                    "end_timestamp": "0001-01-01T00:00:00Z",
                    "compacted_content": "compacted summary",
                }),
            );

        let mut checkpoint_and_transfer_event = event("ev-state-transfer-ckpt");
        checkpoint_and_transfer_event.branch = "root.agent".into();
        checkpoint_and_transfer_event.actions.state_delta =
            Some(json!({"counter": 1}).as_object().cloned().unwrap());
        checkpoint_and_transfer_event.actions.transfer_to_agent = "specialist-agent".into();
        checkpoint_and_transfer_event
            .response
            .custom_metadata
            .insert("adk_agent_state".into(), json!({"step": 2}));
        checkpoint_and_transfer_event
            .response
            .custom_metadata
            .insert("end_of_agent".into(), json!(true));

        let call = |id: &str, name: &str, args: Value| Part {
            function_call: Some(FunctionCall {
                id: id.into(),
                name: name.into(),
                args: args.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            }),
            ..Part::default()
        };
        let mut hitl_and_pause_event = event("ev-hitl-pause");
        hitl_and_pause_event.long_running_tool_ids = vec![
            "call-cred".into(),
            "call-confirm".into(),
            "call-input".into(),
            "call-pause".into(),
        ];
        hitl_and_pause_event.response.content = Some(gadk_genai::Content {
            role: "model".into(),
            parts: vec![
                call(
                    "call-cred",
                    "adk_request_credential",
                    json!({"scope": "read"}),
                ),
                call(
                    "call-confirm",
                    "adk_request_confirmation",
                    json!({"action": "delete"}),
                ),
                call(
                    "call-input",
                    "adk_request_input",
                    json!({"prompt": "name?"}),
                ),
                call(
                    "call-pause",
                    "deploy_cluster",
                    json!({"region": "us-central1"}),
                ),
            ],
        });

        let mut a2a_node_out_and_final_event = event("ev-a2a-node-final");
        a2a_node_out_and_final_event.branch = "root.agent".into();
        a2a_node_out_and_final_event.node_info = Some(gadk_session::NodeInfo {
            path: "wf@0/step@1".into(),
            ..Default::default()
        });
        a2a_node_out_and_final_event.output = Some(json!({"result": "step done"}));
        a2a_node_out_and_final_event.response.content = Some(text_content("model", "42"));
        for (k, v) in [
            ("a2a:task_id", json!("task-1")),
            ("a2a:context_id", json!("ctx-1")),
            ("a2a:request", json!({"method": "message/send"})),
            ("a2a:response", json!({"status": "completed"})),
        ] {
            a2a_node_out_and_final_event
                .response
                .custom_metadata
                .insert(k.into(), v);
        }

        let mut node_error_event = event("ev-node-err");
        node_error_event.node_info = Some(gadk_session::NodeInfo {
            path: "wf@0/step@2".into(),
            ..Default::default()
        });
        node_error_event.response.error_code = "STEP_TIMEOUT".into();
        node_error_event.response.error_message = "step timed out".into();

        let q_args = || json!({"q": "6*7"}).as_object().cloned().unwrap();
        let mut errs: Vec<gadk_agent::AgentError> = Vec::new();
        let mut add = |r: Result<(), gadk_agent::AgentError>| {
            if let Err(e) = r {
                errs.push(e);
            }
        };
        add((p.on_user_message_callback().unwrap())(
            m_ictx.clone(),
            text_content("user", "what is 6*7?"),
        )
        .await
        .map(drop));
        add((p.before_run_callback().unwrap())(m_ictx.clone())
            .await
            .map(drop));
        add((p.before_agent_callback().unwrap())(m_cctx.clone())
            .await
            .map(drop));
        add(
            (p.before_model_callback().unwrap())(m_cctx.clone(), llm_req.clone())
                .await
                .1
                .map(drop),
        );
        add(
            (p.after_model_callback().unwrap())(m_cctx.clone(), llm_res, None)
                .await
                .1
                .map(drop),
        );
        add(
            (p.before_model_callback().unwrap())(m_cctx.clone(), llm_req.clone())
                .await
                .1
                .map(drop),
        );
        add((p.on_model_error_callback().unwrap())(
            m_cctx.clone(),
            llm_req,
            Arc::new(gadk_model::ModelError::Other("rate limited".into())),
        )
        .await
        .map(drop));
        add((p.before_tool_callback().unwrap())(
            m_cctx.clone(),
            Arc::clone(&lookup_tool),
            q_args(),
        )
        .await
        .1
        .map(drop));
        add((p.after_tool_callback().unwrap())(
            m_cctx.clone(),
            Arc::clone(&lookup_tool),
            q_args(),
            json!({"ok": true}).as_object().cloned().unwrap(),
            None,
        )
        .await
        .map(drop));
        add((p.before_tool_callback().unwrap())(
            m_cctx.clone(),
            Arc::clone(&lookup_tool),
            q_args(),
        )
        .await
        .1
        .map(drop));
        add((p.on_tool_error_callback().unwrap())(
            m_cctx.clone(),
            Arc::clone(&lookup_tool),
            q_args(),
            Arc::new(gadk_agent::ToolError::Execution("timeout".into())),
        )
        .await
        .map(drop));
        for ev in [
            compaction_event,
            nano_string_compaction_event,
            unparseable_string_compaction_event,
            checkpoint_and_transfer_event,
            hitl_and_pause_event,
            a2a_node_out_and_final_event,
            node_error_event,
        ] {
            add((p.on_event_callback().unwrap())(m_ictx.clone(), ev)
                .await
                .map(drop));
        }
        add((p.after_agent_callback().unwrap())(m_cctx.clone())
            .await
            .map(drop));
        assert!(errs.is_empty(), "callback errors: {errs:?}");
        (p.after_run_callback().unwrap())(m_ictx.clone()).await;

        let rows = tp
            .collect_rows(|rows| {
                until_event_type(EVENT_INVOCATION_COMPLETED)(rows)
                    || until_event_type(EVENT_INVOCATION_ERROR)(rows)
            })
            .await;

        // Verify callback-emitted JSON paths (content, attributes, and latency_ms) for each
        // event type populated by live callbacks.
        // (event type, column -> JSON paths expected on the callback-emitted row)
        type WantPaths<'a> = Vec<(&'a str, Vec<&'a str>)>;
        let callback_checks: Vec<(&str, WantPaths)> = vec![
            (
                EVENT_LLM_REQUEST,
                vec![(
                    COLUMN_ATTRIBUTES,
                    vec!["$.model", "$.llm_config", "$.tools"],
                )],
            ),
            (
                EVENT_LLM_RESPONSE,
                vec![
                    (
                        COLUMN_CONTENT,
                        vec![
                            "$.response",
                            "$.usage.prompt",
                            "$.usage.completion",
                            "$.usage.total",
                        ],
                    ),
                    (
                        COLUMN_ATTRIBUTES,
                        vec![
                            "$.usage_metadata.cached_content_token_count",
                            "$.usage_metadata.thoughts_token_count",
                            "$.usage_metadata.tool_use_prompt_token_count",
                            "$.model_version",
                            "$.usage_metadata",
                            "$.cache_type",
                            "$.finish_reason",
                        ],
                    ),
                    (COLUMN_LATENCY_MS, vec!["$.total_ms"]),
                ],
            ),
            (
                EVENT_LLM_ERROR,
                vec![(COLUMN_LATENCY_MS, vec!["$.total_ms"])],
            ),
            (
                EVENT_TOOL_STARTING,
                vec![(COLUMN_CONTENT, vec!["$.tool", "$.args", "$.tool_origin"])],
            ),
            (
                EVENT_TOOL_COMPLETED,
                vec![
                    (COLUMN_CONTENT, vec!["$.tool", "$.result", "$.tool_origin"]),
                    (COLUMN_ATTRIBUTES, vec!["$.adk.function_call_id"]),
                    (COLUMN_LATENCY_MS, vec!["$.total_ms"]),
                ],
            ),
            (
                EVENT_TOOL_ERROR,
                vec![
                    (COLUMN_CONTENT, vec!["$.tool", "$.args", "$.tool_origin"]),
                    (COLUMN_LATENCY_MS, vec!["$.total_ms"]),
                ],
            ),
            (
                EVENT_AGENT_COMPLETED,
                vec![(COLUMN_LATENCY_MS, vec!["$.total_ms"])],
            ),
            (
                EVENT_STATE_DELTA,
                vec![(COLUMN_ATTRIBUTES, vec!["$.state_delta"])],
            ),
            (
                EVENT_HITL_CREDENTIAL_REQUEST,
                vec![(COLUMN_CONTENT, vec!["$.tool", "$.args"])],
            ),
            (
                EVENT_HITL_CONFIRMATION_REQUEST,
                vec![(COLUMN_CONTENT, vec!["$.tool", "$.args"])],
            ),
            (
                EVENT_HITL_INPUT_REQUEST,
                vec![(COLUMN_CONTENT, vec!["$.tool", "$.args"])],
            ),
            (
                EVENT_A2A_INTERACTION,
                vec![(
                    COLUMN_ATTRIBUTES,
                    vec![
                        r#"$.a2a_metadata."a2a:task_id""#,
                        r#"$.a2a_metadata."a2a:context_id""#,
                        r#"$.a2a_metadata."a2a:request""#,
                        r#"$.a2a_metadata."a2a:response""#,
                    ],
                )],
            ),
            (
                EVENT_AGENT_RESPONSE,
                vec![
                    (COLUMN_CONTENT, vec!["$.response"]),
                    (
                        COLUMN_ATTRIBUTES,
                        vec![
                            "$.source_event_id",
                            "$.source_event_author",
                            "$.source_event_branch",
                        ],
                    ),
                ],
            ),
            (
                EVENT_AGENT_TRANSFER,
                vec![
                    (COLUMN_CONTENT, vec!["$.from_agent", "$.to_agent"]),
                    (COLUMN_ATTRIBUTES, vec!["$.adk.source_event_id"]),
                ],
            ),
            (
                EVENT_EVENT_COMPACTION,
                vec![(
                    COLUMN_CONTENT,
                    vec![
                        "$.start_timestamp",
                        "$.end_timestamp",
                        "$.compacted_content",
                    ],
                )],
            ),
            (
                EVENT_AGENT_STATE_CHECKPOINT,
                vec![
                    (COLUMN_CONTENT, vec!["$.agent_state", "$.end_of_agent"]),
                    (COLUMN_ATTRIBUTES, vec!["$.adk.source_event_id"]),
                ],
            ),
            (
                EVENT_TOOL_PAUSED,
                vec![
                    (COLUMN_CONTENT, vec!["$.tool", "$.args"]),
                    (
                        COLUMN_ATTRIBUTES,
                        vec!["$.adk.pause_kind", "$.adk.function_call_id"],
                    ),
                ],
            ),
            (
                EVENT_NODE_OUTPUT,
                vec![(
                    COLUMN_ATTRIBUTES,
                    vec![
                        "$.adk.node.path",
                        "$.adk.node.run_id",
                        "$.adk.node.parent_run_id",
                    ],
                )],
            ),
            (
                EVENT_NODE_ERROR,
                vec![
                    (COLUMN_CONTENT, vec!["$.error_code"]),
                    (
                        COLUMN_ATTRIBUTES,
                        vec![
                            "$.adk.node.path",
                            "$.adk.node.run_id",
                            "$.adk.node.parent_run_id",
                        ],
                    ),
                ],
            ),
        ];

        let mut errors: Vec<String> = Vec::new();
        for (event_type, want_paths) in &callback_checks {
            let matching_rows = rows_of_type(&rows, event_type);
            assert!(
                !matching_rows.is_empty(),
                "no {event_type} rows found; event types: {:?}",
                event_types(&rows)
            );
            let row = &matching_rows[0];
            let view_def = defs_by_type[*event_type];
            let view_exprs = extract_sql_json_exprs(&view_def.query);
            let cast_exprs = extract_sql_cast_exprs(&view_def.query);
            for (col, paths) in want_paths {
                let decoded = json_column(row, col)
                    .unwrap_or_else(|| panic!("{event_type} row missing {col} column"));
                for path in paths {
                    let matched_expr = view_exprs
                        .iter()
                        .find(|e| e.column == *col && e.path == *path)
                        .unwrap_or_else(|| {
                            panic!("{event_type} view query does not contain ({col}, {path})")
                        });
                    let Some(val) = resolve_json_value(&decoded, path) else {
                        errors.push(format!(
                            "{event_type} callback-emitted {col} did not contain path {path:?}; payload = {decoded}"
                        ));
                        continue;
                    };
                    if matched_expr.func == "JSON_VALUE" && *path != "$" && is_non_scalar(&val) {
                        errors.push(format!(
                            "{event_type} view {}: JSON_VALUE({col}, '{path}') targeted non-scalar ({})",
                            view_def.name,
                            val.sprint()
                        ));
                    }
                    for ce in &cast_exprs {
                        if ce.column == *col && ce.path == *path {
                            verify_cast_value(&mut errors, &view_def.name, event_type, ce, &val);
                        }
                    }
                }
            }
        }

        // Verify that v_event_compaction's 4 timestamp expressions (start_seconds,
        // end_seconds, window_start, window_end) evaluate cleanly under BigQuery conversion
        // rules for:
        // 1) the native callback payload (float64 epoch seconds matching adk-python),
        // 2) 9-digit nanosecond RFC3339 timestamp strings emitted via OnEventCallback and
        //    converted to float64 epoch seconds, and
        // 3) legacy microsecond and 9-digit nanosecond RFC3339 timestamp strings.
        let want_start_sec = compaction_start.timestamp_micros() as f64 / 1e6;
        let want_end_sec = compaction_end.timestamp_micros() as f64 / 1e6;

        let compaction_rows = rows_of_type(&rows, EVENT_EVENT_COMPACTION);
        assert_eq!(
            compaction_rows.len(),
            3,
            "got {} EVENT_COMPACTION rows, want 3",
            compaction_rows.len()
        );
        let number = |content: &Map<String, Value>, key: &str, what: &str| -> f64 {
            let v = content.get(key);
            match v {
                Some(Value::Number(n)) => n.as_f64().unwrap(),
                _ => panic!("{what} EVENT_COMPACTION {key} = {v:?}, want json.Number"),
            }
        };
        let compaction_content = object_of(
            json_column(&compaction_rows[0], COLUMN_CONTENT),
            "native EVENT_COMPACTION content",
        );
        let got = number(&compaction_content, "start_timestamp", "native");
        assert_eq!(
            got, want_start_sec,
            "native EVENT_COMPACTION start_timestamp = {got}, want {want_start_sec}"
        );
        let got = number(&compaction_content, "end_timestamp", "native");
        assert_eq!(
            got, want_end_sec,
            "native EVENT_COMPACTION end_timestamp = {got}, want {want_end_sec}"
        );

        let parsed_nano_start = DateTime::parse_from_rfc3339("2026-10-10T01:00:00.500000123Z")
            .unwrap()
            .with_timezone(&Utc);
        let parsed_nano_end = DateTime::parse_from_rfc3339("2026-10-10T01:05:00.250000987Z")
            .unwrap()
            .with_timezone(&Utc);
        let want_nano_start_sec = crate::events::epoch_seconds(Some(parsed_nano_start))
            .as_f64()
            .unwrap();
        let want_nano_end_sec = crate::events::epoch_seconds(Some(parsed_nano_end))
            .as_f64()
            .unwrap();

        let nano_compaction_content = object_of(
            json_column(&compaction_rows[1], COLUMN_CONTENT),
            "nano EVENT_COMPACTION content",
        );
        let got = number(&nano_compaction_content, "start_timestamp", "nano");
        assert_eq!(
            got, want_nano_start_sec,
            "nano EVENT_COMPACTION start_timestamp = {got}, want {want_nano_start_sec}"
        );
        let got = number(&nano_compaction_content, "end_timestamp", "nano");
        assert_eq!(
            got, want_nano_end_sec,
            "nano EVENT_COMPACTION end_timestamp = {got}, want {want_nano_end_sec}"
        );

        let unparseable_compaction_content = object_of(
            json_column(&compaction_rows[2], COLUMN_CONTENT),
            "unparseable EVENT_COMPACTION content",
        );
        if let Some(got) = unparseable_compaction_content
            .get("start_timestamp")
            .filter(|v| !v.is_null())
        {
            errors.push(format!(
                "unparseable EVENT_COMPACTION start_timestamp = {got}, want nil"
            ));
        }
        if let Some(got) = unparseable_compaction_content
            .get("end_timestamp")
            .filter(|v| !v.is_null())
        {
            errors.push(format!(
                "zero RFC3339 EVENT_COMPACTION end_timestamp = {got}, want nil"
            ));
        }

        let (compaction_exprs, compaction_aliases) =
            extract_extra_projections(&defs_by_type[EVENT_EVENT_COMPACTION].query);
        let expr_by_alias: HashMap<String, String> = compaction_aliases
            .into_iter()
            .zip(compaction_exprs)
            .collect();
        for alias in ["start_seconds", "end_seconds", "window_start", "window_end"] {
            let expr = expr_by_alias.get(alias).cloned().unwrap_or_default();
            if !expr.contains("REGEXP_REPLACE(") || !expr.contains(r"r'(\.\d{6})\d+'") {
                errors.push(format!(
                    "v_event_compaction {alias} expression missing sub-microsecond REGEXP_REPLACE normalization: {expr}"
                ));
            }
        }

        // Go compactionStart.UTC().Format(time.RFC3339Nano) / compactionEnd's.
        let rfc3339_content = json!({
            "start_timestamp": "2026-10-10T01:00:00.5Z",
            "end_timestamp": "2026-10-10T01:05:00.25Z",
            "compacted_content": "compacted summary",
        });
        let raw_nano_rfc3339_content = json!({
            "start_timestamp": "2026-10-10T01:00:00.500000123Z",
            "end_timestamp": "2026-10-10T01:05:00.250000987Z",
            "compacted_content": "compacted summary",
        });

        let want_nano_win_start =
            DateTime::from_timestamp_micros((want_nano_start_sec * 1e6).round() as i64).unwrap();
        let want_nano_win_end =
            DateTime::from_timestamp_micros((want_nano_end_sec * 1e6).round() as i64).unwrap();

        for (name, content, w_start_sec, w_end_sec, w_win_start, w_win_end) in [
            (
                "native Go callback float64 epoch seconds",
                compaction_content.clone(),
                want_start_sec,
                want_end_sec,
                compaction_start,
                compaction_end,
            ),
            (
                "Arrow-decoded RFC3339Nano string converted to float64 epoch seconds",
                nano_compaction_content.clone(),
                want_nano_start_sec,
                want_nano_end_sec,
                want_nano_win_start,
                want_nano_win_end,
            ),
            (
                "legacy RFC3339 microsecond timestamp strings",
                rfc3339_content.as_object().cloned().unwrap(),
                want_start_sec,
                want_end_sec,
                compaction_start,
                compaction_end,
            ),
            (
                "legacy 9-digit nanosecond RFC3339 timestamp strings",
                raw_nano_rfc3339_content.as_object().cloned().unwrap(),
                want_start_sec,
                want_end_sec,
                compaction_start,
                compaction_end,
            ),
        ] {
            let got = eval_compaction_timestamp_sql(&expr_by_alias["start_seconds"], &content);
            if got.err.is_some() || got.is_window || got.secs != w_start_sec {
                errors.push(format!(
                    "{name}: start_seconds eval = {got:?}, want ({w_start_sec}, false, nil)"
                ));
            }
            let got = eval_compaction_timestamp_sql(&expr_by_alias["end_seconds"], &content);
            if got.err.is_some() || got.is_window || got.secs != w_end_sec {
                errors.push(format!(
                    "{name}: end_seconds eval = {got:?}, want ({w_end_sec}, false, nil)"
                ));
            }
            let got = eval_compaction_timestamp_sql(&expr_by_alias["window_start"], &content);
            if got.err.is_some() || !got.is_window || got.time != Some(w_win_start) {
                errors.push(format!(
                    "{name}: window_start eval = {got:?}, want ({w_win_start}, true, nil)"
                ));
            }
            let got = eval_compaction_timestamp_sql(&expr_by_alias["window_end"], &content);
            if got.err.is_some() || !got.is_window || got.time != Some(w_win_end) {
                errors.push(format!(
                    "{name}: window_end eval = {got:?}, want ({w_win_end}, true, nil)"
                ));
            }
        }
        assert!(
            errors.is_empty(),
            "live plugin callbacks emit JSON keys matching view definitions:\n{}",
            errors.join("\n")
        );
    }

    // Part 2: Verify projected column aliases, Arrow IPC round-trip, JSON_VALUE scalar
    // requirements, and CAST(... AS INT64|FLOAT64|BOOL) parsing across all 25 views,
    // including the Python-only event types (EventAgentError, EventInvocationError) and
    // Python-only optional fields (static_instruction on EventAgentStarting, cache_metadata
    // and time_to_first_token_ms on EventLLMResponse, and error_traceback on EventAgentError /
    // EventInvocationError).
    {
        let s = |v: &[&str]| -> Vec<String> { v.iter().map(|x| (*x).to_owned()).collect() };
        let want_aliases: HashMap<&str, Vec<String>> = HashMap::from([
            (EVENT_USER_MESSAGE_RECEIVED, s(&[])),
            (
                EVENT_LLM_REQUEST,
                s(&["model", "request_content", "llm_config", "tools"]),
            ),
            (
                EVENT_LLM_RESPONSE,
                s(&[
                    "response",
                    "usage_prompt_tokens",
                    "usage_completion_tokens",
                    "usage_total_tokens",
                    "usage_cached_tokens",
                    "usage_thinking_tokens",
                    "usage_tool_use_tokens",
                    "context_cache_hit_rate",
                    "total_ms",
                    "ttft_ms",
                    "model_version",
                    "usage_metadata",
                    "cache_metadata",
                    "cache_type",
                    "finish_reason",
                ]),
            ),
            (EVENT_LLM_ERROR, s(&["total_ms"])),
            (
                EVENT_TOOL_STARTING,
                s(&["tool_name", "tool_args", "tool_origin"]),
            ),
            (
                EVENT_TOOL_COMPLETED,
                s(&[
                    "tool_name",
                    "tool_result",
                    "tool_origin",
                    "total_ms",
                    "pause_kind",
                    "function_call_id",
                ]),
            ),
            (
                EVENT_TOOL_ERROR,
                s(&["tool_name", "tool_args", "tool_origin", "total_ms"]),
            ),
            (
                EVENT_AGENT_STARTING,
                s(&[
                    "agent_instruction",
                    "instruction_source",
                    "static_instruction",
                    "agent_description",
                    "model",
                    "sub_agents",
                ]),
            ),
            (EVENT_AGENT_COMPLETED, s(&["total_ms"])),
            (EVENT_AGENT_ERROR, s(&["total_ms", "error_traceback"])),
            (EVENT_INVOCATION_STARTING, s(&[])),
            (EVENT_INVOCATION_COMPLETED, s(&[])),
            (EVENT_INVOCATION_ERROR, s(&["error_traceback"])),
            (EVENT_STATE_DELTA, s(&["state_delta"])),
            (
                EVENT_HITL_CREDENTIAL_REQUEST,
                s(&["tool_name", "tool_args"]),
            ),
            (
                EVENT_HITL_CONFIRMATION_REQUEST,
                s(&["tool_name", "tool_args"]),
            ),
            (EVENT_HITL_INPUT_REQUEST, s(&["tool_name", "tool_args"])),
            (
                EVENT_A2A_INTERACTION,
                s(&[
                    "response_content",
                    "a2a_task_id",
                    "a2a_context_id",
                    "a2a_request",
                    "a2a_response",
                ]),
            ),
            (
                EVENT_AGENT_RESPONSE,
                s(&[
                    "response_text",
                    "source_event_id",
                    "source_event_author",
                    "source_event_branch",
                ]),
            ),
            (
                EVENT_AGENT_TRANSFER,
                s(&["from_agent", "to_agent", "source_event_id"]),
            ),
            (
                EVENT_EVENT_COMPACTION,
                s(&[
                    "start_seconds",
                    "end_seconds",
                    "window_start",
                    "window_end",
                    "compacted_content",
                ]),
            ),
            (
                EVENT_AGENT_STATE_CHECKPOINT,
                s(&[
                    "agent_state",
                    "agent_state_type",
                    "end_of_agent",
                    "source_event_id",
                ]),
            ),
            (
                EVENT_TOOL_PAUSED,
                s(&["tool_name", "tool_args", "pause_kind", "function_call_id"]),
            ),
            (
                EVENT_NODE_OUTPUT,
                s(&["node_path", "node_run_id", "node_parent_run_id", "output"]),
            ),
            (
                EVENT_NODE_ERROR,
                s(&[
                    "node_path",
                    "node_run_id",
                    "node_parent_run_id",
                    "error_code",
                ]),
            ),
        ]);
        assert_eq!(
            want_aliases.len(),
            defs.len(),
            "len(wantAliases) = {}, want {}",
            want_aliases.len(),
            defs.len()
        );

        let ctx = Context::background();
        let mut cfg = default_config();
        cfg.retry_config.max_retries = 0;

        let bp = queue_processor(&cfg).await;
        let counts = Arc::new(MockCounts::default());
        inject_stream(
            &bp,
            MockStream {
                recv_res: Some(crate::storagepb::AppendRowsResponse::default()),
                counts: Arc::clone(&counts),
                ..MockStream::default()
            },
        )
        .await;

        let ap = AnalyticsPlugin {
            custom_metadata: crate::redaction::CustomMetadataFilter::new(
                &cfg.custom_metadata_allowlist,
            ),
            config: cfg,
            processor: Some(Arc::clone(&bp)),
            ..AnalyticsPlugin::default()
        };
        let m_ctx = MockCtx {
            agent_name: "root-agent".into(),
            session_id: "sess-1".into(),
            invocation_id: "inv-1".into(),
            user_id: "user-1".into(),
            app_name: "app-1".into(),
            branch: "root".into(),
            function_call_id: "call-1".into(),
            ..MockCtx::default()
        };
        let cctx = CallbackCtx::from_context(&m_ctx.context());
        let ident = crate::plugin::rows::identity_of(&cctx);

        let make_row = |event_type: &str, mut d: EventData, latency: Option<Value>| -> QueuedRow {
            let mut row = ap.build_row(&cctx, &ident, event_type, &mut d);
            if let Some(latency) = latency {
                row.row.insert(
                    COLUMN_LATENCY_MS.to_owned(),
                    RowValue::Str(serde_json::to_string(&latency).unwrap()),
                );
            }
            row
        };
        // Inject adk.pause_kind alongside adk.function_call_id as emitted on paused/resumed
        // tool completions.
        let with_pause_kind = |mut row: QueuedRow| -> QueuedRow {
            let mut attrs: Value = match row.row.get(COLUMN_ATTRIBUTES) {
                Some(RowValue::Str(text)) => serde_json::from_str(text).unwrap(),
                other => panic!("attributes = {other:?}"),
            };
            attrs["adk"]["pause_kind"] = json!("long_running");
            row.row.insert(
                COLUMN_ATTRIBUTES.to_owned(),
                RowValue::Str(serde_json::to_string(&attrs).unwrap()),
            );
            row
        };
        let obj = |v: Value| -> Map<String, Value> { v.as_object().cloned().unwrap() };
        let tool_content = |name: &str, key: &str, payload: Value| {
            Some(Payload::Json(crate::plugin::callbacks::tool_content(
                name,
                key,
                &obj(payload),
                crate::events::TOOL_ORIGIN_LOCAL,
            )))
        };

        let node_source_event = Arc::new(gadk_session::Event {
            id: "src-ev-1".into(),
            author: "root-agent".into(),
            branch: "root".into(),
            node_info: Some(gadk_session::NodeInfo {
                path: "wf@0/step@1".into(),
                ..Default::default()
            }),
            ..gadk_session::Event::default()
        });

        let sample_rows = vec![
            make_row(
                EVENT_USER_MESSAGE_RECEIVED,
                EventData {
                    content: Some(Payload::Content(text_content("user", "hello"))),
                    author: USER_AUTHOR.into(),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_LLM_REQUEST,
                EventData {
                    content: Some(Payload::LlmRequest(Box::new(gadk_model::LlmRequest {
                        model: "gemini-2.5-pro".into(),
                        contents: vec![text_content("user", "hello")],
                        ..Default::default()
                    }))),
                    attributes: obj(json!({
                        "model": "gemini-2.5-pro",
                        "llm_config": {"temperature": 0.5},
                        "tools": [{"name": "lookup"}],
                    })),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_LLM_RESPONSE,
                EventData {
                    content: Some(Payload::Json(json!({
                        "response": "text: 'hi'",
                        "usage": {"prompt": 10, "completion": 5, "total": 15},
                    }))),
                    attributes: obj(json!({
                        "model_version": "gemini-2.5-pro-001",
                        "finish_reason": "STOP",
                        "cache_type": "EXPLICIT",
                        "cache_metadata": {"cache_name": "caches/123"},
                        "usage_metadata": {
                            "prompt_token_count": 10,
                            "candidates_token_count": 5,
                            "total_token_count": 15,
                            "cached_content_token_count": 4,
                            "thoughts_token_count": 2,
                            "tool_use_prompt_token_count": 3,
                        },
                    })),
                    ..EventData::default()
                },
                Some(json!({"total_ms": 120, "time_to_first_token_ms": 35})),
            ),
            make_row(
                EVENT_LLM_ERROR,
                EventData {
                    failed: true,
                    err_msg: "quota exceeded".into(),
                    ..EventData::default()
                },
                Some(json!({"total_ms": 45})),
            ),
            make_row(
                EVENT_TOOL_STARTING,
                EventData {
                    content: tool_content("lookup", "args", json!({"q": "go"})),
                    function_call_id: "call-1".into(),
                    ..EventData::default()
                },
                None,
            ),
            with_pause_kind(make_row(
                EVENT_TOOL_COMPLETED,
                EventData {
                    content: tool_content("lookup", "result", json!({"ok": true})),
                    function_call_id: "call-1".into(),
                    ..EventData::default()
                },
                Some(json!({"total_ms": 25})),
            )),
            make_row(
                EVENT_TOOL_ERROR,
                EventData {
                    content: tool_content("lookup", "args", json!({"q": "go"})),
                    failed: true,
                    err_msg: "tool failed".into(),
                    function_call_id: "call-1".into(),
                    ..EventData::default()
                },
                Some(json!({"total_ms": 30})),
            ),
            make_row(
                EVENT_AGENT_STARTING,
                EventData {
                    content: Some(Payload::Json(
                        json!({"text_summary": "You are a helpful agent"}),
                    )),
                    attributes: obj(json!({
                        "instruction_source": "static",
                        "static_instruction": "You are a helpful agent",
                        "agent_description": "Root agent",
                        "model": "gemini-2.5-pro",
                        "sub_agents": ["helper"],
                    })),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_AGENT_COMPLETED,
                EventData::default(),
                Some(json!({"total_ms": 200})),
            ),
            make_row(
                EVENT_AGENT_ERROR,
                EventData {
                    content: Some(Payload::Json(json!({"error_traceback": "Traceback ..."}))),
                    failed: true,
                    err_msg: "agent crashed".into(),
                    ..EventData::default()
                },
                Some(json!({"total_ms": 210})),
            ),
            make_row(EVENT_INVOCATION_STARTING, EventData::default(), None),
            make_row(EVENT_INVOCATION_COMPLETED, EventData::default(), None),
            make_row(
                EVENT_INVOCATION_ERROR,
                EventData {
                    content: Some(Payload::Json(json!({"error_traceback": "Traceback ..."}))),
                    failed: true,
                    err_msg: "invocation crashed".into(),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_STATE_DELTA,
                EventData {
                    attributes: obj(json!({"state_delta": {"counter": 1}})),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_HITL_CREDENTIAL_REQUEST,
                EventData {
                    content: Some(Payload::Json(
                        json!({"tool": "adk_request_credential", "args": {"scope": "read"}}),
                    )),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_HITL_CONFIRMATION_REQUEST,
                EventData {
                    content: Some(Payload::Json(
                        json!({"tool": "adk_request_confirmation", "args": {"action": "delete"}}),
                    )),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_HITL_INPUT_REQUEST,
                EventData {
                    content: Some(Payload::Json(
                        json!({"tool": "adk_request_input", "args": {"prompt": "name?"}}),
                    )),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_A2A_INTERACTION,
                EventData {
                    content: Some(Payload::Json(json!({"text_summary": "remote answer"}))),
                    attributes: obj(json!({
                        "a2a_metadata": {
                            "a2a:task_id": "task-1",
                            "a2a:context_id": "ctx-1",
                            "a2a:request": {"method": "message/send"},
                            "a2a:response": {"status": "completed"},
                        },
                    })),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_AGENT_RESPONSE,
                EventData {
                    content: Some(Payload::Json(json!({"response": "final answer"}))),
                    attributes: obj(json!({
                        "source_event_id": "src-ev-1",
                        "source_event_author": "root-agent",
                        "source_event_branch": "root",
                    })),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_AGENT_TRANSFER,
                EventData {
                    content: Some(Payload::Json(
                        json!({"from_agent": "root-agent", "to_agent": "sub-agent"}),
                    )),
                    source_event: Some(Arc::clone(&node_source_event)),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_EVENT_COMPACTION,
                EventData {
                    content: Some(Payload::Json(json!({
                        "start_timestamp": 1_700_000_000.5,
                        "end_timestamp": 1_700_000_010.5,
                        "compacted_content": {"text_summary": "summary"},
                    }))),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_AGENT_STATE_CHECKPOINT,
                EventData {
                    content: Some(Payload::Json(
                        json!({"agent_state": {"step": 2}, "end_of_agent": true}),
                    )),
                    source_event: Some(Arc::clone(&node_source_event)),
                    ..EventData::default()
                },
                None,
            ),
            with_pause_kind(make_row(
                EVENT_TOOL_PAUSED,
                EventData {
                    content: Some(Payload::Json(json!({"tool": "long_op", "args": {"id": 1}}))),
                    function_call_id: "call-1".into(),
                    ..EventData::default()
                },
                None,
            )),
            make_row(
                EVENT_NODE_OUTPUT,
                EventData {
                    content: Some(Payload::Json(json!({"result": "step done"}))),
                    source_event: Some(Arc::clone(&node_source_event)),
                    ..EventData::default()
                },
                None,
            ),
            make_row(
                EVENT_NODE_ERROR,
                EventData {
                    content: Some(Payload::Json(json!({"error_code": "STEP_TIMEOUT"}))),
                    source_event: Some(Arc::clone(&node_source_event)),
                    failed: true,
                    err_msg: "step timed out".into(),
                    ..EventData::default()
                },
                None,
            ),
        ];

        bp.write_batch(&ctx, sample_rows)
            .await
            .unwrap_or_else(|e| panic!("writeBatch() error = {e}"));
        let sent = counts.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "Send count = {}, want 1", sent.len());
        let decoded_rows = decode_arrow_rows(&sent[0]);

        let mut errors: Vec<String> = Vec::new();
        let mut total_exprs_checked = 0;
        for def in &defs {
            let Some(want_extra_aliases) = want_aliases.get(def.event_type.as_str()) else {
                panic!(
                    "view {} ({}) missing from wantAliases",
                    def.name, def.event_type
                );
            };
            let (_, got_aliases) = extract_extra_projections(&def.query);
            if *want_extra_aliases != got_aliases {
                errors.push(format!(
                    "view {} ({}) projected aliases mismatch: want {want_extra_aliases:?}, got {got_aliases:?}",
                    def.name, def.event_type
                ));
            }

            let row = only_row_of_type(&decoded_rows, &def.event_type);
            let exprs = extract_sql_json_exprs(&def.query);
            let cast_exprs = extract_sql_cast_exprs(&def.query);
            for expr in &exprs {
                total_exprs_checked += 1;
                let Some(col_val) = json_column(&row, &expr.column) else {
                    errors.push(format!(
                        "view {} ({}): decoded row missing column {:?} for path {:?}",
                        def.name, def.event_type, expr.column, expr.path
                    ));
                    continue;
                };
                let Some(val) = resolve_json_value(&col_val, &expr.path) else {
                    errors.push(format!(
                        "view {} ({}): path {:?} in column {:?} resolved to NULL; column JSON = {col_val}",
                        def.name, def.event_type, expr.path, expr.column
                    ));
                    continue;
                };
                // JSON_VALUE in BigQuery extracts scalars (string, number, bool), not
                // objects/arrays.
                if expr.func == "JSON_VALUE" && is_non_scalar(&val) && expr.path != "$" {
                    errors.push(format!(
                        "view {} ({}): JSON_VALUE({}, '{}') targeted non-scalar ({}); use JSON_QUERY instead",
                        def.name,
                        def.event_type,
                        expr.column,
                        expr.path,
                        val.sprint()
                    ));
                }
                for ce in &cast_exprs {
                    if ce.column == expr.column && ce.path == expr.path {
                        verify_cast_value(&mut errors, &def.name, &def.event_type, ce, &val);
                    }
                }
            }
        }
        assert!(
            total_exprs_checked > 0,
            "extractSQLJSONExprs matched 0 expressions across all 25 views"
        );
        assert!(
            errors.is_empty(),
            "all 25 view SQL projections and CAST expressions resolve across Arrow-encoded rows:\n{}",
            errors.join("\n")
        );
    }
}

// parity: adapted: Go's mockTransport http.Client → a ScriptedClient behind the client's endpoint, Go's in-process gRPC write server → FakeWriteServer; channels → CancellationTokens; Go's `<-r.Context().Done()` in the transport → a drop guard on the handler future (the client drops the request when its context ends); log.Default() → a recording Logger reporting is_std_default (gadk's StdLogger writes to stderr); collectingRunner's runtime.GC → none (Arc drops are immediate); newJobCountingBQClient's POST /jobs answers a DONE job (gadk's REST client polls a job without a status until it is done); time is paused
#[tokio::test(start_paused = true)]
async fn test_views_async_startup_and_close_lifecycle() {
    let views = EVENT_VIEW_SPECS.len();

    // view creation runs asynchronously by default without blocking constructor and creates
    // all 25 views
    {
        let first_job_started = CancellationToken::new();
        let release_jobs = CancellationToken::new();
        let all_jobs_done = CancellationToken::new();
        let job_count = Arc::new(AtomicUsize::new(0));
        let job_queries = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let http = {
            let (first, release, all) = (
                first_job_started.clone(),
                release_jobs.clone(),
                all_jobs_done.clone(),
            );
            let (count, queries) = (job_count.clone(), job_queries.clone());
            ScriptedClient::new_async(move |r: RecordedRequest| {
                let (first, release, all) = (first.clone(), release.clone(), all.clone());
                let (count, queries) = (count.clone(), queries.clone());
                async move {
                    if r.method == "POST" && r.path.contains("/jobs") {
                        let payload: Value = serde_json::from_slice(&r.body).unwrap_or_default();
                        let query = payload["configuration"]["query"]["query"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        queries.lock().push(query);
                        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                        if n == 1 {
                            first.cancel();
                        }
                        release.cancelled().await;
                        if n == views {
                            all.cancel();
                        }
                        return FakeResponse::json(done_job_json("job-1"));
                    }
                    FakeResponse::json("{}")
                }
            })
        };
        let bq_client = bq_client_over(http);
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            ..Config::default()
        };
        let server = FakeWriteServer::new();
        // Use a short startup context that we cancel immediately after construction to verify
        // background view creation uses context.WithoutCancel(ctx).
        let startup_ctx = Context::background().with_cancel();
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &startup_ctx,
            cfg,
            Some(bq_client),
            Some(server.client().unwrap()),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
        startup_ctx.cancel();

        // Constructor returned while releaseJobs is still closed!
        within(
            Duration::from_secs(3),
            first_job_started.cancelled(),
            "timed out waiting for async view creation goroutine to start",
        )
        .await;
        release_jobs.cancel();
        let waited = tokio::time::timeout(Duration::from_secs(5), all_jobs_done.cancelled()).await;
        assert!(
            waited.is_ok(),
            "timed out waiting for all {views} view DDL jobs; got {}",
            job_count.load(Ordering::SeqCst)
        );
        close_plugin(&p).await.expect("Close() error");

        assert_eq!(
            job_count.load(Ordering::SeqCst),
            views,
            "view DDL job count"
        );
        let queries = job_queries.lock();
        assert!(
            queries[0].contains("v_user_message_received"),
            "first view query = {:?}, want v_user_message_received",
            queries[0]
        );
        assert!(
            queries[queries.len() - 1].contains("v_node_error"),
            "last view query = {:?}, want v_node_error",
            queries[queries.len() - 1]
        );
    }

    // Close cancels in-flight async view creation cleanly
    {
        let in_flight = CancellationToken::new();
        let canceled = CancellationToken::new();
        let http = {
            let (in_flight, canceled) = (in_flight.clone(), canceled.clone());
            ScriptedClient::new_async(move |r: RecordedRequest| {
                let (in_flight, canceled) = (in_flight.clone(), canceled.clone());
                async move {
                    if r.method == "POST" && r.path.contains("/jobs") {
                        let _canceled = CancelOnDrop(canceled);
                        in_flight.cancel();
                        std::future::pending::<()>().await;
                    }
                    FakeResponse::json("{}")
                }
            })
        };
        let bq_client = bq_client_over(http);
        let logger = Arc::new(RecordingLogger::default());
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            logger: Some(logger.clone()),
            ..Config::default()
        };
        let server = FakeWriteServer::new();
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            cfg,
            Some(bq_client),
            Some(server.client().unwrap()),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");

        within(
            Duration::from_secs(3),
            in_flight.cancelled(),
            "timed out waiting for view creation request to enter RoundTrip",
        )
        .await;

        let close_done = tokio::spawn((p.close().unwrap())());
        let closed = within(
            Duration::from_secs(3),
            close_done,
            "Close() hung waiting for async view creation",
        )
        .await;
        closed.unwrap().expect("Close() error");

        assert!(
            canceled.is_cancelled(),
            "expected in-flight view DDL request context to be canceled by Close()"
        );
        assert!(
            !(logger.contains("failed to create view")
                || logger.contains("view creation finished")
                || logger.contains("call SyncViews to retry")),
            "Close() cancellation should not log failure or retry summary, got:\n{logger}"
        );
    }

    // DisableCreateViews executes zero view queries
    for (name, config) in [
        (
            "DefaultConfig with DisableCreateViews",
            Config {
                disable_create_views: true,
                ..Config::default()
            },
        ),
        (
            "Config literal with DisableCreateViews",
            Config {
                enabled: true,
                disable_create_views: true,
                ..Config::zero()
            },
        ),
    ] {
        let jobs_called = Arc::new(AtomicUsize::new(0));
        let http = {
            let jobs_called = jobs_called.clone();
            ScriptedClient::new(move |r: &RecordedRequest| {
                if r.path.contains("/jobs") {
                    jobs_called.fetch_add(1, Ordering::SeqCst);
                }
                FakeResponse::json("{}")
            })
        };
        let bq_client = bq_client_over(http);
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            ..config
        };
        let server = FakeWriteServer::new();
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            cfg,
            Some(bq_client),
            Some(server.client().unwrap()),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("{name}: NewBigQueryAgentAnalyticsPluginWithClients() error = {e}")
        });
        close_plugin(&p)
            .await
            .unwrap_or_else(|e| panic!("{name}: Close() error = {e}"));
        assert_eq!(
            jobs_called.load(Ordering::SeqCst),
            0,
            "{name}: /jobs requests"
        );
    }

    // HTTP 403 logs warning without failing startup and HTTP 409 is tolerated
    {
        let job_idx = Arc::new(AtomicUsize::new(0));
        let http = {
            let job_idx = job_idx.clone();
            ScriptedClient::new(move |r: &RecordedRequest| {
                if r.method == "POST" && r.path.contains("/jobs") {
                    return match job_idx.fetch_add(1, Ordering::SeqCst) + 1 {
                        1 => FakeResponse::json_status(
                            409,
                            r#"{"error":{"code":409,"message":"Already Exists"}}"#,
                        ),
                        2 => FakeResponse::json_status(
                            403,
                            r#"{"error":{"code":403,"message":"Access Denied"}}"#,
                        ),
                        _ => FakeResponse::json(done_job_json("job-ok")),
                    };
                }
                FakeResponse::json("{}")
            })
        };
        let bq_client = bq_client_over(http);
        let logger = Arc::new(RecordingLogger::default());
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            logger: Some(logger.clone()),
            ..Config::default()
        };
        let server = FakeWriteServer::new();
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            cfg,
            Some(bq_client),
            Some(server.client().unwrap()),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
        // Wait for the pass to log its summary, the last thing it does, then close. Closing
        // once the last job is sent instead could cancel the pass before the summary, which a
        // canceled pass does not log.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !logger.contains("view creation finished") && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        close_plugin(&p).await.expect("Close() error");

        assert_eq!(
            job_idx.load(Ordering::SeqCst),
            views,
            "executed view DDL jobs (must continue after 409 and 403)"
        );
        assert!(
            !logger.contains("v_user_message_received"),
            "409 Conflict on v_user_message_received should be benign, but logged:\n{logger}"
        );
        assert!(
            logger.contains("failed to create view v_llm_request"),
            "expected warning for 403 on v_llm_request, got logs:\n{logger}"
        );
        assert!(
            logger.contains(
                "view creation finished: 24 created, 1 failed of 25; call SyncViews to retry"
            ),
            "expected summary log line for partial failure, got logs:\n{logger}"
        );
    }

    // DefaultConfig does not write background view failures or startup skip to log.Default
    {
        let std_log = Arc::new(StdDefaultRecorder::default());
        let job_idx = Arc::new(AtomicUsize::new(0));
        let all_failed = CancellationToken::new();
        let http = {
            let (job_idx, all_failed) = (job_idx.clone(), all_failed.clone());
            ScriptedClient::new(move |r: &RecordedRequest| {
                if r.method == "POST" && r.path.contains("/jobs") {
                    if job_idx.fetch_add(1, Ordering::SeqCst) + 1 == views {
                        all_failed.cancel();
                    }
                    return FakeResponse::json_status(
                        403,
                        r#"{"error":{"code":403,"message":"Access Denied"}}"#,
                    );
                }
                FakeResponse::json("{}")
            })
        };
        let bq_client = bq_client_over(http);

        // DefaultConfig() leaves Logger == log.Default(); neither per-view failures, the retry
        // summary, nor startup view-skip diagnostics may write to log.Default().
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            logger: Some(std_log.clone()),
            ..Config::default()
        };
        let server = FakeWriteServer::new();
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            cfg,
            Some(bq_client.clone()),
            Some(server.client().unwrap()),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
        let waited = tokio::time::timeout(Duration::from_secs(5), all_failed.cancelled()).await;
        assert!(
            waited.is_ok(),
            "timed out waiting for all {views} view DDL jobs; got {}",
            job_idx.load(Ordering::SeqCst)
        );
        // Give syncAnalyticsViews a moment after the 25th RunViewDDL returns to reach the
        // post-loop summary check before Close() cancels lifeCtx.
        tokio::time::sleep(Duration::from_millis(20)).await;
        close_plugin(&p).await.expect("Close() error");

        let skip_cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "v_llm_request".into(),
            logger: Some(std_log.clone()),
            ..Config::default()
        };
        let p_skip = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            skip_cfg,
            Some(bq_client),
            Some(server.client().unwrap()),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients(skipCfg) error");
        close_plugin(&p_skip).await.expect("pSkip.Close() error");

        let got = std_log.0.to_string();
        assert!(
            got.is_empty(),
            "DefaultConfig() wrote diagnostics to log.Default():\n{got}"
        );
    }

    // startup view creation keeps its BigQuery client alive until it ends
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
        let defs = view_definitions_with_denied(
            "test-project",
            "test_dataset",
            "test-table",
            "v",
            &Default::default(),
        )
        .expect("viewDefinitionsWithDenied() error");
        // Holding the view gate keeps the pass from starting before done is read.
        let gate = Arc::clone(&bqaap.state.view_gate)
            .acquire_owned()
            .await
            .expect("acquireViewGate() error");
        let jobs = Arc::new(AtomicUsize::new(0));
        let logger = Arc::new(RecordingLogger::default());
        {
            // Only the pass refers to this client, and its runner collects garbage before
            // every view.
            let client = new_job_counting_bq_client(jobs.clone());
            let runner: Arc<dyn ViewRunner> = Arc::new(CollectingRunner {
                inner: Arc::new(BigQueryViewRunner::new(
                    Arc::downgrade(&client),
                    "test-project",
                    "test_dataset",
                )),
            });
            set_view_runner(&bqaap, Some(runner));
            bqaap.state.start_async_view_creation(
                &Context::background(),
                defs,
                Some(logger.clone()),
                Some(client),
            );
        }
        let done = bqaap
            .state
            .views
            .lock()
            .async_views_done
            .clone()
            .expect("asyncViewsDone");
        drop(gate);
        within(
            Duration::from_secs(10),
            done.cancelled(),
            "timed out waiting for view creation to end",
        )
        .await;
        assert_eq!(
            jobs.load(Ordering::SeqCst),
            views,
            "view creation ran view jobs; log:\n{logger}"
        );
    }
}

// parity: adapted: Go's nil *BigQueryAgentAnalyticsPlugin receiver has no gadk value (the check is dropped); Go's direct writes to the plugin's runner / closed fields → the PluginState's views lock; acquireViewGate → the view gate's permit; errors.Is(err, errPluginClosed) → the error text; mockTransport → ScriptedClient; goroutines → spawned tasks; time is paused
#[tokio::test(start_paused = true)]
async fn test_big_query_agent_analytics_plugin_sync_views() {
    let ctx = Context::background();
    let views = EVENT_VIEW_SPECS.len();

    let disabled_p = new_bigquery_agent_analytics_plugin_with_clients(
        &ctx,
        Config {
            enabled: false,
            ..Config::zero()
        },
        None,
        None,
    )
    .await
    .expect("NewBigQueryAgentAnalyticsPluginWithClients(disabled) error");
    let disabled_bqaap = as_bigquery_agent_analytics_plugin(&disabled_p)
        .expect("AsBigQueryAgentAnalyticsPlugin(disabledP) = false, want true");
    let err = disabled_bqaap.sync_views(&ctx).await;
    assert!(
        matches!(&err, Err(e) if e.to_string().contains("disabled")),
        "disabledBQAAP.SyncViews() error = {err:?}, want disabled error"
    );

    // Plugin with nil bqClient -> nil runner.
    let no_bq = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
    let no_bq_bqaap = as_bigquery_agent_analytics_plugin(&no_bq.plugin)
        .expect("AsBigQueryAgentAnalyticsPlugin(noBQP) = false, want true");
    let saved_runner = set_view_runner(&no_bq_bqaap, None);
    let err = no_bq_bqaap.sync_views(&ctx).await;
    assert!(
        matches!(&err, Err(e) if e.to_string().contains("not configured")),
        "SyncViews() with nil runner error = {err:?}, want not configured error"
    );
    set_view_runner(&no_bq_bqaap, saved_runner);

    // executes all 25 views synchronously and tolerates 409 Conflict
    {
        let tp = new_test_plugin_with_config(|c| {
            c.disable_create_views = true;
            c.view_prefix = "v_sync".into();
        })
        .await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
        let fake_runner = Arc::new(FakeViewRunner::new(
            HashMap::from([
                (
                    "v_sync_user_message_received".to_owned(),
                    FakeError::Api {
                        code: 409,
                        message: "conflict".into(),
                    },
                ),
                (
                    "v_sync_llm_request".to_owned(),
                    FakeError::Status {
                        code: Code::AlreadyExists,
                        message: "already exists".into(),
                    },
                ),
            ]),
            None,
        ));
        set_view_runner(&bqaap, Some(fake_runner.clone()));

        bqaap
            .sync_views(&ctx)
            .await
            .expect("SyncViews() error, want nil");
        assert_eq!(
            fake_runner.executed_names().len(),
            views,
            "SyncViews() executed views"
        );
    }

    // applies per-statement viewDDLTimeout and preserves shorter caller deadline
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");

        // 1. Under context.Background(), every RunViewDDL call must receive a deadline <=
        // viewDDLTimeout.
        let saw_deadline_count = Arc::new(AtomicUsize::new(0));
        let saw = saw_deadline_count.clone();
        set_view_runner(
            &bqaap,
            Some(Arc::new(FakeViewRunner::new(
                HashMap::new(),
                Some(on_run(move |run_ctx, view_name| {
                    let saw = saw.clone();
                    async move {
                        let deadline = run_ctx.deadline().unwrap_or_else(|| {
                            panic!(
                                "RunViewDDL({view_name}): missing per-statement deadline under background context"
                            )
                        });
                        let rem = deadline.saturating_duration_since(Instant::now());
                        assert!(
                            !rem.is_zero() && rem <= VIEW_DDL_TIMEOUT,
                            "RunViewDDL({view_name}): time.Until(deadline) = {rem:?}, want in (0, {VIEW_DDL_TIMEOUT:?}]"
                        );
                        saw.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                })),
            ))),
        );
        bqaap
            .sync_views(&Context::background())
            .await
            .expect("SyncViews(Background) error");
        assert_eq!(
            saw_deadline_count.load(Ordering::SeqCst),
            views,
            "inspected deadlines"
        );

        // 2. Under a shorter caller deadline (50ms), the tighter caller deadline must win over
        // viewDDLTimeout.
        let short_timeout = Duration::from_millis(50);
        let short_ctx = Context::background().with_timeout(short_timeout);
        set_view_runner(
            &bqaap,
            Some(Arc::new(FakeViewRunner::new(
                HashMap::new(),
                Some(on_run(move |run_ctx, view_name| async move {
                    let deadline = run_ctx.deadline().unwrap_or_else(|| {
                        panic!(
                            "RunViewDDL({view_name}): missing deadline under short caller context"
                        )
                    });
                    let rem = deadline.saturating_duration_since(Instant::now());
                    assert!(
                        rem <= short_timeout,
                        "RunViewDDL({view_name}): time.Until(deadline) = {rem:?}, want <= {short_timeout:?} (caller deadline must win)"
                    );
                    Ok(())
                })),
            ))),
        );
        bqaap
            .sync_views(&short_ctx)
            .await
            .expect("SyncViews(shortCtx) error");
        short_ctx.cancel();
    }

    // aggregates errors across views and stops on context cancellation
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
        let fake_runner = Arc::new(FakeViewRunner::new(
            HashMap::from([(
                "v_llm_request".to_owned(),
                FakeError::Message("ddl syntax error".into()),
            )]),
            None,
        ));
        set_view_runner(&bqaap, Some(fake_runner.clone()));

        let err = bqaap.sync_views(&ctx).await;
        assert!(
            matches!(&err, Err(e) if e.to_string().contains("v_llm_request")),
            "SyncViews() error = {err:?}, want error naming v_llm_request"
        );
        assert_eq!(
            fake_runner.executed_names().len(),
            views,
            "SyncViews() executed views on single error"
        );

        let canceled_ctx = ctx.with_cancel();
        canceled_ctx.cancel();
        let fake_runner2 = Arc::new(FakeViewRunner::default());
        set_view_runner(&bqaap, Some(fake_runner2.clone()));
        let err = bqaap.sync_views(&canceled_ctx).await;
        assert!(
            matches!(&err, Err(e) if e.is_context(ContextError::Canceled)),
            "SyncViews(canceledCtx) error = {err:?}, want context.Canceled"
        );
        assert_eq!(
            fake_runner2.executed_names().len(),
            0,
            "SyncViews(canceledCtx) executed views"
        );
    }

    // bqViewRunner waits on running jobs and surfaces job status errors
    {
        let poll_calls = Arc::new(AtomicUsize::new(0));
        let http = {
            let poll_calls = poll_calls.clone();
            ScriptedClient::new(move |r: &RecordedRequest| {
                if r.method == "POST" && r.path.contains("/jobs") {
                    return FakeResponse::json(
                        r#"{"jobReference":{"projectId":"test-project","jobId":"job-running"},"status":{"state":"RUNNING"}}"#,
                    );
                }
                if r.method == "GET" && r.path.contains("/jobs/job-running") {
                    poll_calls.fetch_add(1, Ordering::SeqCst);
                    return FakeResponse::json(
                        r#"{"jobReference":{"projectId":"test-project","jobId":"job-running"},"status":{"state":"DONE","errorResult":{"reason":"invalidQuery","message":"bad view query"}}}"#,
                    );
                }
                FakeResponse::json("{}")
            })
        };
        let bq_client = bq_client_over(http);
        let runner = BigQueryViewRunner::new(Arc::downgrade(&bq_client), "", "");
        let err = runner
            .run_view_ddl(
                &ctx,
                "v_test",
                "CREATE OR REPLACE VIEW `p.d.v_test` AS SELECT 1",
            )
            .await;
        assert!(
            matches!(&err, Err(e) if e.to_string().contains("bad view query")),
            "RunViewDDL() error = {err:?}, want error containing 'bad view query'"
        );
        assert!(
            poll_calls.load(Ordering::SeqCst) > 0,
            "expected job.Wait to poll /jobs/job-running when initial state is RUNNING"
        );
    }

    // bqViewRunner skips CREATE OR REPLACE VIEW when existing view query matches
    {
        let jobs_posted = Arc::new(AtomicUsize::new(0));
        let http = {
            let jobs_posted = jobs_posted.clone();
            ScriptedClient::new(move |r: &RecordedRequest| {
                if r.method == "GET" && r.path.contains("/datasets/test_dataset/tables/v_test") {
                    return FakeResponse::json(r#"{"type":"VIEW","view":{"query":"SELECT 1"}}"#);
                }
                if r.method == "POST" && r.path.contains("/jobs") {
                    jobs_posted.fetch_add(1, Ordering::SeqCst);
                    return FakeResponse::json(done_job_json("job-1"));
                }
                FakeResponse::json("{}")
            })
        };
        let bq_client = bq_client_over(http);
        let runner = BigQueryViewRunner::new(Arc::downgrade(&bq_client), "", "test_dataset");
        runner
            .run_view_ddl(
                &ctx,
                "v_test",
                "CREATE OR REPLACE VIEW `test-project.test_dataset.v_test` AS\nSELECT 1",
            )
            .await
            .expect("RunViewDDL(unchanged) error");
        assert_eq!(
            jobs_posted.load(Ordering::SeqCst),
            0,
            "jobsPosted for unchanged view"
        );

        runner
            .run_view_ddl(
                &ctx,
                "v_test",
                "CREATE OR REPLACE VIEW `test-project.test_dataset.v_test` AS\nSELECT 2",
            )
            .await
            .expect("RunViewDDL(changed) error");
        assert_eq!(
            jobs_posted.load(Ordering::SeqCst),
            1,
            "jobsPosted for changed view"
        );
    }

    // bqViewRunner without a BigQuery client reports it
    {
        let err = BigQueryViewRunner::new(std::sync::Weak::new(), "", "test_dataset")
            .run_view_ddl(
                &ctx,
                "v_test",
                "CREATE OR REPLACE VIEW `test-project.test_dataset.v_test` AS\nSELECT 1",
            )
            .await;
        assert!(
            matches!(&err, Err(e) if e.to_string().contains("BigQuery client is not configured")),
            "RunViewDDL() error = {err:?}, want the missing client reported"
        );
    }

    // SyncViews keeps the plugin's BigQuery client alive while it runs
    {
        // Only the plugin refers to its client, as with NewBigQueryAgentAnalyticsPluginWithConfig,
        // and only the wrapper that SyncViews is called on refers to the plugin. The runner
        // collects garbage before every view.
        let jobs = Arc::new(AtomicUsize::new(0));
        let server = FakeWriteServer::new();
        let bqaap = {
            let cfg = Config {
                enabled: true,
                project_id: "test-project".into(),
                dataset_id: "test_dataset".into(),
                table_name: "test-table".into(),
                disable_create_views: true,
                ..Config::default()
            };
            let p = new_bigquery_agent_analytics_plugin_with_clients(
                &ctx,
                cfg,
                Some(new_job_counting_bq_client(jobs.clone())),
                Some(server.client().unwrap()),
            )
            .await
            .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
            let bqaap =
                as_bigquery_agent_analytics_plugin(&p).expect("AsBigQueryAgentAnalyticsPlugin");
            let inner = bqaap.state.views.lock().runner.clone().expect("runner");
            set_view_runner(&bqaap, Some(Arc::new(CollectingRunner { inner })));
            bqaap
        };
        let err = bqaap.sync_views(&ctx).await;
        let _ = close_plugin(bqaap.plugin()).await;
        err.expect("SyncViews() error");
        assert_eq!(
            jobs.load(Ordering::SeqCst),
            views,
            "SyncViews() ran view jobs"
        );
    }

    // Close preempts in-flight SyncViews call
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");

        let sync_in_flight = CancellationToken::new();
        let runner_canceled = CancellationToken::new();
        let release_runner = CancellationToken::new();
        let once = Arc::new(AtomicBool::new(false));
        {
            let (sync_in_flight, runner_canceled, release_runner) = (
                sync_in_flight.clone(),
                runner_canceled.clone(),
                release_runner.clone(),
            );
            set_view_runner(
                &bqaap,
                Some(Arc::new(FakeViewRunner::new(
                    HashMap::new(),
                    Some(on_run(move |run_ctx, _| {
                        let first = !once.swap(true, Ordering::SeqCst);
                        let (sync_in_flight, runner_canceled, release_runner) = (
                            sync_in_flight.clone(),
                            runner_canceled.clone(),
                            release_runner.clone(),
                        );
                        async move {
                            if first {
                                sync_in_flight.cancel();
                                run_ctx.done().await;
                                runner_canceled.cancel();
                                release_runner.cancelled().await;
                            }
                            ctx_result(&run_ctx)
                        }
                    })),
                ))),
            );
        }

        let sync_done = spawn_sync_views(&bqaap, Context::background());
        within(
            Duration::from_secs(3),
            sync_in_flight.cancelled(),
            "timed out waiting for SyncViews to enter RunViewDDL",
        )
        .await;

        // Start a second SyncViews call that must wait on viewGate behind the in-flight
        // SyncViews call.
        let contender_done = spawn_sync_views(&bqaap, Context::background());
        tokio::time::sleep(Duration::from_millis(15)).await;

        let mut close_done = tokio::spawn((tp.plugin.close().unwrap())());

        within(
            Duration::from_secs(3),
            runner_canceled.cancelled(),
            "timed out waiting for Close() to cancel in-flight SyncViews context",
        )
        .await;

        // The gate-waiting contender returns errPluginClosed promptly without waiting for the
        // gate holder's RunViewDDL step to finish.
        let err = within(
            Duration::from_secs(3),
            contender_done,
            "gate-waiting SyncViews() did not return after Close() canceled lifeCtx",
        )
        .await
        .unwrap();
        assert!(
            is_plugin_closed(&err),
            "gate-waiting SyncViews() error = {err:?}, want {PLUGIN_CLOSED_TEXT}"
        );

        // Close() must NOT return while the gate-holding SyncViews step is still running
        // (verifying the viewGate drain in stopAsyncViewCreation).
        if let Ok(res) = tokio::time::timeout(Duration::from_millis(20), &mut close_done).await {
            panic!("Close() returned ({res:?}) before in-flight gate-holding SyncViews finished");
        }

        release_runner.cancel();

        within(
            Duration::from_secs(3),
            &mut close_done,
            "Close() blocked after in-flight SyncViews released gate",
        )
        .await
        .unwrap()
        .expect("Close() error");

        let err = within(
            Duration::from_secs(3),
            sync_done,
            "SyncViews() did not return after Close()",
        )
        .await
        .unwrap();
        assert!(
            is_plugin_closed(&err),
            "SyncViews() preempted by Close() error = {err:?}, want {PLUGIN_CLOSED_TEXT}"
        );
    }

    // Close preempts SyncViews waiting on startup pass and post-gate close check returns
    // errPluginClosed
    {
        // Subcase A: SyncViews blocked inside preemptAsyncViewCreation when Close() runs.
        let tp_a = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap_a = as_bigquery_agent_analytics_plugin(&tp_a.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(pA) = false");
        let bg_entered = CancellationToken::new();
        let release_bg = CancellationToken::new();
        let once = Arc::new(AtomicBool::new(false));
        {
            let (bg_entered, release_bg) = (bg_entered.clone(), release_bg.clone());
            set_view_runner(
                &bqaap_a,
                Some(Arc::new(FakeViewRunner::new(
                    HashMap::new(),
                    Some(on_run(move |run_ctx, _| {
                        let first = !once.swap(true, Ordering::SeqCst);
                        let (bg_entered, release_bg) = (bg_entered.clone(), release_bg.clone());
                        async move {
                            if first {
                                bg_entered.cancel();
                                run_ctx.done().await;
                                release_bg.cancelled().await;
                            }
                            ctx_result(&run_ctx)
                        }
                    })),
                ))),
            );
        }
        let defs = view_definitions_with_denied(
            "test-project",
            "test_dataset",
            "test-table",
            "v",
            &Default::default(),
        )
        .expect("viewDefinitionsWithDenied");
        bqaap_a
            .state
            .start_async_view_creation(&ctx, defs, None, None);
        bg_entered.cancelled().await;

        let sync_err = spawn_sync_views(&bqaap_a, Context::background());
        tokio::time::sleep(Duration::from_millis(15)).await;

        let close = tokio::spawn((tp_a.plugin.close().unwrap())());

        let err = within(
            Duration::from_secs(3),
            sync_err,
            "SyncViews() waiting on startup pass did not return when Close() canceled lifeCtx",
        )
        .await
        .unwrap();
        assert!(
            is_plugin_closed(&err),
            "SyncViews() waiting on startup pass error = {err:?}, want {PLUGIN_CLOSED_TEXT}"
        );
        release_bg.cancel();
        close.await.unwrap().expect("pA.Close() error");

        // Subcase B: SyncViews acquires viewGate after s.closed becomes true before
        // context.AfterFunc cancels ctx; post-gate check returns errPluginClosed.
        let tp_b = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap_b = as_bigquery_agent_analytics_plugin(&tp_b.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(pB) = false");
        let post_gate_runner = Arc::new(FakeViewRunner::default());
        set_view_runner(&bqaap_b, Some(post_gate_runner.clone()));
        let gate = Arc::clone(&bqaap_b.state.view_gate)
            .acquire_owned()
            .await
            .expect("acquireViewGate");
        let post_gate_err = spawn_sync_views(&bqaap_b, Context::background());
        tokio::time::sleep(Duration::from_millis(15)).await;
        bqaap_b.state.views.lock().closed = true;
        drop(gate);

        let err = within(
            Duration::from_secs(3),
            post_gate_err,
            "post-gate SyncViews() did not return",
        )
        .await
        .unwrap();
        assert!(
            is_plugin_closed(&err),
            "post-gate SyncViews() error = {err:?}, want {PLUGIN_CLOSED_TEXT}"
        );
        assert_eq!(
            post_gate_runner.executed_names().len(),
            0,
            "post-gate closed SyncViews() executed views"
        );
    }

    // SyncViews after Close returns closed error without touching runner
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
        let fake_runner = Arc::new(FakeViewRunner::default());
        set_view_runner(&bqaap, Some(fake_runner.clone()));

        tp.close().await.expect("Close() error");
        let err = bqaap.sync_views(&ctx).await;
        assert!(
            is_plugin_closed(&err),
            "SyncViews() after Close() error = {err:?}, want {PLUGIN_CLOSED_TEXT}"
        );
        assert_eq!(
            fake_runner.executed_names().len(),
            0,
            "SyncViews() after Close() executed views"
        );
    }

    // SyncViews returns why the config has no views, and startup skips them
    {
        let logger = Arc::new(RecordingLogger::default());
        let tp = {
            let logger = logger.clone();
            new_test_plugin_with_config(move |c| {
                c.table_name = "v_llm_request".into();
                c.logger = Some(logger);
            })
            .await
        };
        assert!(
            logger.contains("agentanalytics: skipping view creation"),
            "startup log does not report skipped view creation; got:\n{logger}"
        );
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
        let fake_runner = Arc::new(FakeViewRunner::default());
        set_view_runner(&bqaap, Some(fake_runner.clone()));
        let err = bqaap.sync_views(&ctx).await;
        assert!(
            matches!(&err, Err(e) if e.to_string().contains("collides with the events table")),
            "SyncViews() error = {err:?}, want the view name collision"
        );
        assert_eq!(
            fake_runner.executed_names().len(),
            0,
            "SyncViews() executed views"
        );
    }

    // SyncViews while async startup is in-flight preempts background run and executes views
    // once
    {
        let server = FakeWriteServer::new();
        let bg_in_flight = CancellationToken::new();
        let bg_canceled = CancellationToken::new();
        let call_index = Arc::new(AtomicUsize::new(0));
        let sync_completed = Arc::new(AtomicUsize::new(0));
        let http = {
            let (bg_in_flight, bg_canceled) = (bg_in_flight.clone(), bg_canceled.clone());
            let (call_index, sync_completed) = (call_index.clone(), sync_completed.clone());
            ScriptedClient::new_async(move |r: RecordedRequest| {
                let (bg_in_flight, bg_canceled) = (bg_in_flight.clone(), bg_canceled.clone());
                let (call_index, sync_completed) = (call_index.clone(), sync_completed.clone());
                async move {
                    if r.method == "POST" && r.path.contains("/jobs") {
                        if call_index.fetch_add(1, Ordering::SeqCst) + 1 == 1 {
                            let _canceled = CancelOnDrop(bg_canceled);
                            bg_in_flight.cancel();
                            std::future::pending::<()>().await;
                        }
                        sync_completed.fetch_add(1, Ordering::SeqCst);
                        return FakeResponse::json(done_job_json("job-sync"));
                    }
                    FakeResponse::json("{}")
                }
            })
        };
        let bq_client = bq_client_over(http);
        let logger = Arc::new(RecordingLogger::default());
        let cfg = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            logger: Some(logger.clone()),
            ..Config::default()
        };
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &ctx,
            cfg,
            Some(bq_client),
            Some(server.client().unwrap()),
        )
        .await
        .expect("NewBigQueryAgentAnalyticsPluginWithClients() error");
        let bqaap = as_bigquery_agent_analytics_plugin(&p)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");

        within(
            Duration::from_secs(3),
            bg_in_flight.cancelled(),
            "timed out waiting for background startup view creation to enter RoundTrip",
        )
        .await;

        bqaap
            .sync_views(&ctx)
            .await
            .expect("SyncViews() error, want nil");

        assert!(
            bg_canceled.is_cancelled(),
            "expected SyncViews() to cancel in-flight background view creation"
        );
        assert_eq!(
            sync_completed.load(Ordering::SeqCst),
            views,
            "SyncViews() completed view DDLs after preemption"
        );
        assert!(
            !(logger.contains("failed to create view")
                || logger.contains("view creation finished")
                || logger.contains("call SyncViews to retry")),
            "preempted background view creation should not log context.Canceled warning or retry summary, got:\n{logger}"
        );
        let _ = close_plugin(&p).await;
    }

    // concurrent SyncViews honors caller context deadline while waiting for active refresh
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");

        let first_in_flight = CancellationToken::new();
        let release_first = CancellationToken::new();
        let blocked_once = Arc::new(AtomicBool::new(false));
        {
            let (first_in_flight, release_first) = (first_in_flight.clone(), release_first.clone());
            set_view_runner(
                &bqaap,
                Some(Arc::new(FakeViewRunner::new(
                    HashMap::new(),
                    Some(on_run(move |run_ctx, _| {
                        let first = !blocked_once.swap(true, Ordering::SeqCst);
                        let (first_in_flight, release_first) =
                            (first_in_flight.clone(), release_first.clone());
                        async move {
                            if first {
                                first_in_flight.cancel();
                                release_first.cancelled().await;
                            }
                            ctx_result(&run_ctx)
                        }
                    })),
                ))),
            );
        }

        let first_done = spawn_sync_views(&bqaap, ctx.clone());
        within(
            Duration::from_secs(3),
            first_in_flight.cancelled(),
            "timed out waiting for first SyncViews to enter RunViewDDL",
        )
        .await;

        let short_ctx = ctx.with_timeout(Duration::from_millis(30));
        let start = Instant::now();
        let err = bqaap.sync_views(&short_ctx).await;
        let elapsed = start.elapsed();
        assert!(
            matches!(&err, Err(e) if e.is_context(ContextError::DeadlineExceeded)),
            "second SyncViews(shortCtx) error = {err:?}, want context.DeadlineExceeded"
        );
        assert!(
            elapsed <= Duration::from_millis(500),
            "second SyncViews(shortCtx) took {elapsed:?}, expected prompt return on deadline"
        );
        short_ctx.cancel();

        release_first.cancel();
        first_done
            .await
            .unwrap()
            .expect("first SyncViews() error, want nil");

        let after_runner = Arc::new(FakeViewRunner::default());
        set_view_runner(&bqaap, Some(after_runner.clone()));
        bqaap
            .sync_views(&ctx)
            .await
            .expect("SyncViews() after releasing first refresh error, want nil");
        assert_eq!(
            after_runner.executed_names().len(),
            views,
            "subsequent SyncViews() executed views"
        );
    }

    // SyncViews honors caller context deadline while waiting for in-flight startup refresh
    {
        let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
        let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");

        let bg_entered = CancellationToken::new();
        let release_bg = CancellationToken::new();
        let once = Arc::new(AtomicBool::new(false));
        {
            let (bg_entered, release_bg) = (bg_entered.clone(), release_bg.clone());
            set_view_runner(
                &bqaap,
                Some(Arc::new(FakeViewRunner::new(
                    HashMap::new(),
                    Some(on_run(move |run_ctx, _| {
                        let first = !once.swap(true, Ordering::SeqCst);
                        let (bg_entered, release_bg) = (bg_entered.clone(), release_bg.clone());
                        async move {
                            if first {
                                bg_entered.cancel();
                                release_bg.cancelled().await;
                            }
                            ctx_result(&run_ctx)
                        }
                    })),
                ))),
            );
        }

        let defs = view_definitions_with_denied(
            "test-project",
            "test_dataset",
            "test-table",
            "v",
            &Default::default(),
        )
        .expect("ViewDefinitions");
        bqaap
            .state
            .start_async_view_creation(&ctx, defs, None, None);
        let bg_done = bqaap
            .state
            .views
            .lock()
            .async_views_done
            .clone()
            .expect("asyncViewsDone");
        within(
            Duration::from_secs(3),
            bg_entered.cancelled(),
            "timed out waiting for startup view creation to enter RunViewDDL",
        )
        .await;

        let short_ctx = ctx.with_timeout(Duration::from_millis(30));
        let start = Instant::now();
        let err = bqaap.sync_views(&short_ctx).await;
        let elapsed = start.elapsed();
        assert!(
            matches!(&err, Err(e) if e.is_context(ContextError::DeadlineExceeded)),
            "SyncViews(shortCtx) while startup refresh blocked error = {err:?}, want context.DeadlineExceeded"
        );
        assert!(
            elapsed <= Duration::from_millis(500),
            "SyncViews(shortCtx) took {elapsed:?}, expected prompt return on deadline"
        );
        short_ctx.cancel();

        release_bg.cancel();
        within(
            Duration::from_secs(3),
            bg_done.cancelled(),
            "timed out waiting for startup view creation to complete",
        )
        .await;
        let after_runner = Arc::new(FakeViewRunner::default());
        set_view_runner(&bqaap, Some(after_runner.clone()));
        bqaap
            .sync_views(&ctx)
            .await
            .expect("SyncViews() after startup refresh unblocked error, want nil");
        assert_eq!(
            after_runner.executed_names().len(),
            views,
            "SyncViews() executed views"
        );
    }
}

// parity: adapted: Go's direct reads of viewsCancel / asyncViewsDone / runner → the PluginState's views lock; the runner's release on Close is also observed through a Weak of it (Go needs the GC for that); time is paused
#[tokio::test(start_paused = true)]
async fn test_plugin_state_releases_view_pass_and_runner() {
    let tp = new_test_plugin_with_config(|c| c.disable_create_views = true).await;
    let bqaap = as_bigquery_agent_analytics_plugin(&tp.plugin)
        .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
    let release = CancellationToken::new();
    let runner: Arc<dyn ViewRunner> = {
        let release = release.clone();
        Arc::new(FakeViewRunner::new(
            HashMap::new(),
            Some(on_run(move |_, _| {
                let release = release.clone();
                async move {
                    release.cancelled().await;
                    Ok(())
                }
            })),
        ))
    };
    let weak_runner = Arc::downgrade(&runner);
    set_view_runner(&bqaap, Some(runner));
    let defs = view_definitions_with_denied(
        "test-project",
        "test_dataset",
        "test-table",
        "v",
        &Default::default(),
    )
    .expect("viewDefinitionsWithDenied() error");

    bqaap
        .state
        .start_async_view_creation(&Context::background(), defs, None, None);
    let done = bqaap
        .state
        .views
        .lock()
        .async_views_done
        .clone()
        .expect("asyncViewsDone");
    release.cancel();
    within(
        Duration::from_secs(3),
        done.cancelled(),
        "timed out waiting for the view pass to end",
    )
    .await;
    let (cancel_kept, done_kept) = {
        let views = bqaap.state.views.lock();
        (
            views.views_cancel.is_some(),
            views.async_views_done.is_some(),
        )
    };
    assert!(
        !cancel_kept && !done_kept,
        "after the view pass ended: viewsCancel kept = {cancel_kept}, asyncViewsDone kept = {done_kept}; want neither kept"
    );

    tp.close().await.expect("Close() error");
    let runner_kept = bqaap.state.views.lock().runner.is_some();
    assert!(
        !runner_kept,
        "after Close: view runner kept, want it dropped"
    );
    assert!(
        weak_runner.upgrade().is_none(),
        "after Close: view runner still alive, want it released"
    );
}

#[tokio::test]
async fn test_views_event_compaction_window_out_of_range_is_null() {
    let defs = Config {
        project_id: "test-project".to_owned(),
        dataset_id: "test_dataset".to_owned(),
        table_name: "test-table".to_owned(),
        ..Config::zero()
    }
    .view_definitions()
    .unwrap();
    let query = defs
        .iter()
        .find(|d| d.event_type == EVENT_EVENT_COMPACTION)
        .map(|d| d.query.clone())
        .unwrap_or_default();
    let (exprs, aliases) = extract_extra_projections(&query);
    let expr_by_alias: HashMap<String, String> = aliases.into_iter().zip(exprs).collect();

    // Go json.Number values: the content map holds them as JSON numbers.
    let numeric = |start: &str, end: &str| -> Map<String, Value> {
        serde_json::from_str(&format!(
            r#"{{"start_timestamp": {start}, "end_timestamp": {end}}}"#
        ))
        .unwrap()
    };

    for (name, value) in [
        ("epoch milliseconds beyond year 9999", "1729634400000"),
        ("seconds beyond INT64 microseconds", "1e300"),
        ("seconds before year 1", "-1e12"),
        ("seconds whose microseconds exceed FLOAT64", "1e308"),
        (
            "negative seconds whose microseconds exceed FLOAT64",
            "-1e308",
        ),
    ] {
        let content = numeric(value, value);
        for alias in ["window_start", "window_end"] {
            let got = eval_compaction_timestamp_sql(&expr_by_alias[alias], &content);
            assert!(
                got.err.is_none() && got.is_window && got.time.is_none(),
                "{name}: {alias} = {got:?}, want NULL without failing the query"
            );
        }
    }

    // A bare multiplication would overflow FLOAT64 before the safe cast runs, and fail the
    // query.
    for value in ["1e308", "-1e308"] {
        let content = numeric(value, value);
        for alias in ["window_start", "window_end"] {
            let expr = &expr_by_alias[alias];
            let bare = expr
                .replacen("SAFE_CAST(SAFE_MULTIPLY(", "SAFE_CAST(", 1)
                .replacen(", 1000000) AS INT64))", " * 1000000 AS INT64))", 1);
            assert_ne!(
                &bare, expr,
                "{alias} does not multiply by 1000000 with SAFE_MULTIPLY: {expr}"
            );
            let got = eval_compaction_timestamp_sql(&bare, &content);
            assert!(
                got.err.is_some(),
                "{alias} with a bare multiplication of {value}: error = nil, want a FLOAT64 overflow"
            );
        }
    }

    // A value in range still yields its timestamp.
    let content = numeric("1760000000.5", "1760000001");
    let got = eval_compaction_timestamp_sql(&expr_by_alias["window_start"], &content);
    assert!(got.err.is_none(), "window_start: {got:?}");
    assert_eq!(
        got.time,
        DateTime::from_timestamp(1_760_000_000, 500_000_000)
    );
    let got = eval_compaction_timestamp_sql(&expr_by_alias["window_end"], &content);
    assert!(got.err.is_none(), "window_end: {got:?}");
    assert_eq!(got.time, DateTime::from_timestamp(1_760_000_001, 0));

    // The plugin writes such numbers unchanged, so the view meets them.
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
        json_column, new_test_plugin_with_config, rows_of_type, until_event_type,
    };
    let tp = new_test_plugin_with_config(|c| {
        c.custom_metadata_allowlist = vec!["adk_compaction".into()];
        c.disable_create_views = true;
    })
    .await;
    let mut ev = gadk_session::Event {
        id: "ev-extreme-compaction".into(),
        author: "test-agent".into(),
        ..gadk_session::Event::default()
    };
    ev.response.custom_metadata.insert(
        "adk_compaction".into(),
        serde_json::json!({
            "start_timestamp": 1e308,
            "end_timestamp": -1e308,
            "compacted_content": "compacted summary",
        }),
    );
    (tp.plugin.on_event_callback().unwrap())(tp.ctx.invocation(), ev)
        .await
        .unwrap();
    (tp.plugin.after_run_callback().unwrap())(tp.ctx.invocation()).await;
    let rows = rows_of_type(
        &tp.collect_rows(until_event_type(EVENT_EVENT_COMPACTION))
            .await,
        EVENT_EVENT_COMPACTION,
    );
    let written = match json_column(&rows[0], COLUMN_CONTENT) {
        Some(Value::Object(m)) => m,
        other => panic!("content = {other:?}, want a JSON object"),
    };
    for (key, want) in [("start_timestamp", 1e308), ("end_timestamp", -1e308)] {
        let got = written.get(key);
        assert!(
            got.is_some_and(Value::is_number),
            "written {key} = {got:?}, want the number {want}"
        );
        assert_eq!(got.and_then(Value::as_f64), Some(want), "written {key}");
    }
    for alias in ["window_start", "window_end"] {
        let got = eval_compaction_timestamp_sql(&expr_by_alias[alias], &written);
        assert!(
            got.err.is_none() && got.is_window && got.time.is_none(),
            "written row: {alias} = {got:?}, want NULL without failing the query"
        );
    }
}

// parity: adapted: time is paused
#[tokio::test(start_paused = true)]
async fn test_sync_analytics_views_context_ends_pass() {
    let ctx = Context::background();
    let none = Default::default();

    // SyncViews caller deadline is reported once as canceled
    {
        let tp = crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::new_test_plugin_with_config(
            |c| c.disable_create_views = true,
        )
        .await;
        let bqaap = crate::plugin::as_bigquery_agent_analytics_plugin(&tp.plugin)
            .expect("AsBigQueryAgentAnalyticsPlugin(p) = false");
        let on_run: OnRun = Arc::new(|run_ctx: Context, _name: String| {
            Box::pin(async move {
                run_ctx.done().await;
                Err(Error::Context(
                    run_ctx.err().unwrap_or(ContextError::Canceled),
                ))
            })
        });
        let runner = Arc::new(FakeViewRunner::new(HashMap::new(), Some(on_run)));
        bqaap.state.views.lock().runner = Some(Arc::clone(&runner) as Arc<dyn ViewRunner>);

        let deadline_ctx = ctx.with_timeout(Duration::from_millis(100));
        let err = bqaap.sync_views(&deadline_ctx).await.unwrap_err();
        deadline_ctx.cancel();
        assert!(
            err.is_context(ContextError::DeadlineExceeded),
            "SyncViews() error = {err}, want context.DeadlineExceeded"
        );
        assert_eq!(
            err.to_string(),
            "view creation canceled during v_user_message_received: context deadline exceeded"
        );
        assert_eq!(runner.executed_names().len(), 1, "views run");
    }

    // cancellation reported by a transport error that does not wrap it is not a failure
    {
        let defs =
            view_definitions_with_denied("test-project", "test_dataset", "test-table", "v", &none)
                .unwrap();
        let pass_ctx = ctx.with_cancel();
        let cancel_ctx = pass_ctx.clone();
        let on_run: OnRun = Arc::new(move |_ctx: Context, _name: String| {
            cancel_ctx.cancel(); // As Close or a preempting SyncViews does.
            Box::pin(async {
                Err(Error::Status {
                    code: Code::Canceled,
                    message: "transport: stream canceled".to_owned(),
                })
            })
        });
        let runner = FakeViewRunner::new(HashMap::new(), Some(on_run));
        let logger = RecordingLogger::default();
        let err = sync_analytics_views(&pass_ctx, &runner, &defs, Some(&logger))
            .await
            .unwrap_err();
        pass_ctx.cancel();
        assert!(
            err.is_context(ContextError::Canceled),
            "syncAnalyticsViews() error = {err}, want context.Canceled"
        );
        let text = err.to_string();
        assert!(
            text.contains("view creation canceled during v_user_message_received")
                && !text.contains("failed to create view"),
            "syncAnalyticsViews() error = {text:?}, want one cancellation and no failure"
        );
        assert_eq!(
            text,
            "view creation canceled during v_user_message_received: context canceled (rpc error: code = Canceled desc = transport: stream canceled)"
        );
        assert_eq!(logger.to_string(), "", "canceled pass logged");
        assert_eq!(runner.executed_names().len(), 1, "views run");
    }

    // per-view timeout under a live context is a failure and the pass continues
    {
        let defs =
            view_definitions_with_denied("test-project", "test_dataset", "test-table", "v", &none)
                .unwrap();
        let runner = FakeViewRunner::new(
            HashMap::from([(
                "v_user_message_received".to_owned(),
                FakeError::Context(ContextError::DeadlineExceeded),
            )]),
            None,
        );
        let logger = RecordingLogger::default();
        let err = sync_analytics_views(&ctx, &runner, &defs, Some(&logger))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to create view v_user_message_received"),
            "syncAnalyticsViews() error = {err}, want failure of v_user_message_received"
        );
        assert_eq!(runner.executed_names().len(), defs.len(), "views run");
        let want = "view creation finished: 24 created, 1 failed of 25; call SyncViews to retry";
        assert!(
            logger.contains(want),
            "log does not contain {want:?}; got:\n{logger}"
        );
        assert_eq!(
            logger.lines(),
            [
                "agentanalytics: failed to create view v_user_message_received: context deadline exceeded",
                "agentanalytics: view creation finished: 24 created, 1 failed of 25; call SyncViews to retry",
            ]
        );
    }
}

#[tokio::test]
async fn test_sync_analytics_views_nil_logf() {
    let defs = view_definitions_with_denied(
        "test-project",
        "test_dataset",
        "test-table",
        "v",
        &Default::default(),
    )
    .unwrap();
    let runner = FakeViewRunner::new(
        HashMap::from([(
            "v_llm_request".to_owned(),
            FakeError::Message("access denied".to_owned()),
        )]),
        None,
    );
    let err = sync_analytics_views(&Context::background(), &runner, &defs, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("failed to create view v_llm_request"),
        "syncAnalyticsViews() error = {err}, want failure of v_llm_request"
    );
    assert_eq!(runner.executed_names().len(), defs.len(), "views run");
}

// parity: adapted: Go's mockTransport http.Client → a FakeServer behind the client's endpoint (unauthenticated)
#[tokio::test]
async fn test_bq_view_runner_does_not_replace_non_view() {
    for table_type in ["TABLE", "MATERIALIZED_VIEW"] {
        let jobs_posted = Arc::new(AtomicUsize::new(0));
        let posted = jobs_posted.clone();
        let server = FakeServer::start(move |r: &RecordedRequest| {
            if r.method == "GET" && r.path.contains("/datasets/test_dataset/tables/v_test") {
                return FakeResponse::json(format!(r#"{{"type":"{table_type}"}}"#));
            }
            if r.method == "POST" && r.path.contains("/jobs") {
                posted.fetch_add(1, Ordering::SeqCst);
                return FakeResponse::json(
                    r#"{"jobReference":{"projectId":"test-project","jobId":"job-1"},"status":{"state":"DONE"}}"#,
                );
            }
            FakeResponse::json("{}")
        });
        let options = ClientOptions {
            endpoint: Some(format!("{}/bigquery/v2/", server.url())),
            without_authentication: true,
            ..ClientOptions::default()
        };
        let bq_client = Arc::new(BigQueryClient::new("test-project", &options).unwrap());
        let runner = BigQueryViewRunner::new(Arc::downgrade(&bq_client), "", "test_dataset");
        let err = runner
            .run_view_ddl(
                &Context::background(),
                "v_test",
                "CREATE OR REPLACE VIEW `test-project.test_dataset.v_test` AS\nSELECT 1",
            )
            .await;
        let want = format!(r#"cannot replace existing BigQuery {table_type} "v_test" with a view"#);
        assert_eq!(
            err.map_err(|e| e.to_string()),
            Err(want),
            "{table_type}: RunViewDDL()"
        );
        assert_eq!(
            jobs_posted.load(Ordering::SeqCst),
            0,
            "{table_type}: DDL jobs posted"
        );
        // The runner reads the table under the client's project (empty runner project).
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{table_type}: requests");
        assert_eq!(
            reqs[0].path,
            "/bigquery/v2/projects/test-project/datasets/test_dataset/tables/v_test"
        );
    }
}

// parity: adapted: Go's mockTransport http.Client → a ScriptedClient; Go reads bqViewRunner.projectID, gadk's runner is a `dyn ViewRunner`, so its project is read off the table GET of one DDL run (a view name answered as a TABLE stops it before any job)
#[tokio::test]
async fn test_new_big_query_agent_analytics_plugin_resolves_project_for_table_and_views() {
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::MockCtx;
    use crate::testutil::fake_write_server::FakeWriteServer;
    use gadk_model::rest::test_util::ScriptedClient;

    let ctx = Context::background();
    for (name, config_project, want_project) in [
        (
            "empty ProjectID takes the client's project",
            "",
            "client-project",
        ),
        (
            "DetectProjectID takes the client's project",
            crate::plugin::DETECT_PROJECT_ID,
            "client-project",
        ),
        (
            "explicit ProjectID is used for the table check",
            "config-project",
            "config-project",
        ),
    ] {
        let table_gets = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let gets = Arc::clone(&table_gets);
        let http = ScriptedClient::new(move |r: &RecordedRequest| {
            if r.method == "GET" && r.path.contains("/tables/test-table") {
                gets.lock().push(r.path.clone());
            }
            // Fork D: the runner probe below (not in Go).
            if r.method == "GET" && r.path.ends_with("/tables/v_runner_probe") {
                return FakeResponse::json(r#"{"type":"TABLE"}"#);
            }
            FakeResponse::json("{}")
        });
        let options = ClientOptions {
            http_client: Some(http.clone()),
            without_authentication: true,
            endpoint: Some("http://bq.test/bigquery/v2/".into()),
            ..ClientOptions::default()
        };
        let bq_client = Arc::new(BigQueryClient::new("client-project", &options).unwrap());

        let server = FakeWriteServer::new();
        let cfg = Config {
            project_id: config_project.into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            disable_create_views: true,
            ..Config::default()
        };
        let p = crate::plugin::new_bigquery_agent_analytics_plugin_with_clients(
            &ctx,
            cfg,
            Some(Arc::clone(&bq_client)),
            Some(server.client().unwrap()),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("{name}: NewBigQueryAgentAnalyticsPluginWithClients() error = {e}")
        });
        let bqaap = crate::plugin::as_bigquery_agent_analytics_plugin(&p)
            .unwrap_or_else(|| panic!("{name}: AsBigQueryAgentAnalyticsPlugin(p) = false"));

        let runner = bqaap
            .state
            .views
            .lock()
            .runner
            .clone()
            .unwrap_or_else(|| panic!("{name}: no view runner"));
        let probe = runner
            .run_view_ddl(
                &ctx,
                "v_runner_probe",
                "CREATE OR REPLACE VIEW `x.test_dataset.v_runner_probe` AS\nSELECT 1",
            )
            .await;
        assert!(probe.is_err(), "{name}: runner probe ran a job");
        let probe_path = http
            .requests()
            .into_iter()
            .map(|r| r.path)
            .find(|path| path.ends_with("/tables/v_runner_probe"))
            .unwrap_or_default();
        assert!(
            probe_path.ends_with(&format!(
                "/projects/{want_project}/datasets/test_dataset/tables/v_runner_probe"
            )),
            "{name}: view runner project: table read {probe_path:?}, want project {want_project:?}"
        );

        let gets = table_gets.lock().clone();
        let want_path = format!("/projects/{want_project}/datasets/test_dataset/tables/test-table");
        assert!(
            gets.first().is_some_and(|g| g.ends_with(&want_path)),
            "{name}: table metadata GETs = {gets:?}, want one ending in {want_path:?}"
        );

        let m_ctx = MockCtx {
            agent_name: "test-agent".into(),
            session_id: "sess-1".into(),
            invocation_id: "inv-1".into(),
            user_id: "user-1".into(),
            ..MockCtx::default()
        };
        (p.before_run_callback().unwrap())(m_ctx.invocation())
            .await
            .unwrap_or_else(|e| panic!("{name}: BeforeRunCallback() error = {e}"));
        (p.after_run_callback().unwrap())(m_ctx.invocation()).await;
        let want_stream = format!(
            "projects/{want_project}/datasets/test_dataset/tables/test-table/streams/_default"
        );
        tokio::time::timeout(Duration::from_secs(3), server.wait_for_append_requests(1))
            .await
            .unwrap_or_else(|_| panic!("{name}: timed out waiting for AppendRowsRequest"));
        let req = server.append_requests().remove(0).request;
        assert_eq!(
            req.write_stream, want_stream,
            "{name}: AppendRowsRequest.WriteStream"
        );

        let runner = Arc::new(FakeViewRunner::default());
        bqaap.state.views.lock().runner = Some(Arc::clone(&runner) as Arc<dyn ViewRunner>);
        bqaap
            .sync_views(&ctx)
            .await
            .unwrap_or_else(|e| panic!("{name}: SyncViews() error = {e}"));
        let first_query = runner.queries()[0].clone();
        let want = format!("`{want_project}.test_dataset.v_user_message_received`");
        assert!(
            first_query.contains(&want),
            "{name}: first view DDL = {first_query:?}, want it to name {want}"
        );
        (p.close().unwrap())().await.unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers of views_test.go.

// --- Helpers ---

/// Go `declaredTool` (a `fakeTool` with a function declaration, arrow_test_helper_test.go).
struct DeclaredTool {
    name: &'static str,
    description: &'static str,
    decl: gadk_genai::FunctionDeclaration,
}

impl gadk_agent::Tool for DeclaredTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn declaration(&self) -> Option<gadk_genai::FunctionDeclaration> {
        Some(self.decl.clone())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `textContent(role, text)`.
fn text_content(role: &str, text: &str) -> gadk_genai::Content {
    gadk_genai::Content::from_text(text, role)
}

/// Go `sqlCastExpr`.
#[derive(Debug)]
struct SqlCastExpr {
    column: String,
    path: String,
    cast_type: String,
}

/// Go `sqlCastJSONValueRe` (`\s` ASCII as in RE2).
static SQL_CAST_JSON_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:SAFE_)?CAST\((?-u:\s)*JSON_VALUE\((?-u:\s)*(content|attributes|latency_ms)(?-u:\s)*,(?-u:\s)*'(\$[^']*)'(?-u:\s)*\)(?-u:\s)+AS(?-u:\s)+(INT64|FLOAT64|BOOL)(?-u:\s)*\)",
    )
    .unwrap()
});

/// Go `extractSQLCastExprs`.
fn extract_sql_cast_exprs(query: &str) -> Vec<SqlCastExpr> {
    SQL_CAST_JSON_VALUE_RE
        .captures_iter(query)
        .map(|m| SqlCastExpr {
            column: m[1].to_owned(),
            path: m[2].to_owned(),
            cast_type: m[3].to_owned(),
        })
        .collect()
}

/// Go `strconv.ParseBool`.
fn go_parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err(format!("strconv.ParseBool: parsing {s:?}: invalid syntax")),
    }
}

/// Go `verifyCastValue` (`t.Errorf` → a message pushed onto `errs`).
fn verify_cast_value(
    errs: &mut Vec<String>,
    view_name: &str,
    event_type: &str,
    cast: &SqlCastExpr,
    raw_val: &ScalarRef<'_>,
) {
    let s = raw_val.sprint();
    let (column, path) = (&cast.column, &cast.path);
    let failure = match cast.cast_type.as_str() {
        "INT64" => s.parse::<i64>().err().map(|e| e.to_string()),
        "FLOAT64" => go_parse_float(&s)
            .is_none()
            .then(|| format!("strconv.ParseFloat: parsing {s:?}: invalid syntax")),
        "BOOL" => go_parse_bool(&s).err(),
        _ => None,
    };
    if let Some(err) = failure {
        errs.push(format!(
            "view {view_name} ({event_type}): CAST(JSON_VALUE({column}, '{path}') AS {}) value {s:?} does not parse as {}: {err}",
            cast.cast_type, cast.cast_type
        ));
    }
}

/// Go `resolveJSONPath(doc, path)` over any decoded JSON column value.
fn resolve_json_value<'a>(doc: &'a Value, path: &str) -> Option<ScalarRef<'a>> {
    match doc {
        Value::Object(m) => resolve_json_path(m, path),
        Value::Null => None,
        v if path == "$" => Some(ScalarRef::Value(v)),
        _ => None,
    }
}

/// Whether a resolved value is an object or an array (Go `map[string]any, []any`).
fn is_non_scalar(v: &ScalarRef<'_>) -> bool {
    match v {
        ScalarRef::Object(_) => true,
        ScalarRef::Value(v) => v.is_object() || v.is_array(),
    }
}

/// `v` as an object (Go `jsonObjectColumn`'s map).
fn object_of(v: Option<Value>, what: &str) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m,
        other => panic!("{what} = {other:?}, want a JSON object"),
    }
}

/// Go `newSet` over the column constants.
fn new_set(items: &[&str]) -> std::collections::BTreeSet<String> {
    crate::event_types::new_set(items)
}

/// Go `extractExtraProjections`: parses the SELECT clause of a `CREATE OR REPLACE VIEW` query,
/// checks the leading common columns and returns the extra projection expressions and their
/// aliases (`t.Fatalf` → panic).
fn extract_extra_projections(query: &str) -> (Vec<String>, Vec<String>) {
    const SELECT_MARKER: &str = "SELECT\n  ";
    const FROM_MARKER: &str = "\nFROM\n";
    let (Some(start), Some(end)) = (query.find(SELECT_MARKER), query.find(FROM_MARKER)) else {
        panic!("failed to parse SELECT clause from query:\n{query}");
    };
    assert!(
        end > start + SELECT_MARKER.len(),
        "failed to parse SELECT clause from query:\n{query}"
    );
    let select_body = &query[start + SELECT_MARKER.len()..end];
    let cols: Vec<&str> = select_body.split(",\n  ").collect();
    assert!(
        cols.len() >= VIEW_COMMON_COLUMNS.len(),
        "SELECT has {} columns, want at least {}:\n{query}",
        cols.len(),
        VIEW_COMMON_COLUMNS.len()
    );
    assert_eq!(
        VIEW_COMMON_COLUMNS.as_slice(),
        &cols[..VIEW_COMMON_COLUMNS.len()],
        "common columns prefix mismatch"
    );
    let extra = &cols[VIEW_COMMON_COLUMNS.len()..];
    let aliases = extra
        .iter()
        .map(|col| match col.rfind(" AS ") {
            Some(idx) => col[idx + 4..].trim().to_owned(),
            None => col.trim().to_owned(),
        })
        .collect();
    (extra.iter().map(|c| (*c).to_owned()).collect(), aliases)
}

/// Go `sqlJSONExpr`.
#[derive(Debug)]
struct SqlJsonExpr {
    func: String,
    column: String,
    path: String,
}

/// Go `sqlJSONExprRe` (`\s` is ASCII in RE2; the `regex` crate's `\s` is Unicode, so the
/// classes are spelled out).
static SQL_JSON_EXPR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(JSON_(?:VALUE|QUERY))\((?-u:\s)*(content|attributes|latency_ms)(?-u:\s)*,(?-u:\s)*'(\$[^']*)'(?-u:\s)*\)",
    )
    .unwrap()
});

/// Go `subMicroTimestampRe` (`\d` ASCII as in RE2).
static SUB_MICRO_TIMESTAMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\.[0-9]{6})[0-9]+").unwrap());

/// Go `extractSQLJSONExprs`.
fn extract_sql_json_exprs(query: &str) -> Vec<SqlJsonExpr> {
    SQL_JSON_EXPR_RE
        .captures_iter(query)
        .map(|m| SqlJsonExpr {
            func: m[1].to_owned(),
            column: m[2].to_owned(),
            path: m[3].to_owned(),
        })
        .collect()
}

/// Go `resolveJSONPath` over a decoded JSON object.
fn resolve_json_path<'a>(doc: &'a Map<String, Value>, path: &str) -> Option<ScalarRef<'a>> {
    if path == "$" {
        return Some(ScalarRef::Object(doc));
    }
    let rest = path.strip_prefix("$.")?;
    let mut cur: &Value = doc.get(split_json_path_segments(rest).first()?.as_str())?;
    for seg in split_json_path_segments(rest).iter().skip(1) {
        cur = cur.as_object()?.get(seg.as_str())?;
    }
    if cur.is_null() {
        return None;
    }
    Some(ScalarRef::Value(cur))
}

/// A resolved JSON path: the whole document (`$`) or a value inside it.
enum ScalarRef<'a> {
    Object(&'a Map<String, Value>),
    Value(&'a Value),
}

impl ScalarRef<'_> {
    /// Go `fmt.Sprint(rawVal)` for the scalars the tests use: a string's text, a number's
    /// JSON text (Go `json.Number`).
    fn sprint(&self) -> String {
        match self {
            ScalarRef::Value(Value::String(s)) => s.clone(),
            ScalarRef::Value(v) => v.to_string(),
            ScalarRef::Object(m) => Value::Object((*m).clone()).to_string(),
        }
    }
}

/// Go `splitJSONPathSegments`.
fn split_json_path_segments(s: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = Vec::new();
    let mut in_quotes = false;
    for &ch in s.as_bytes() {
        match ch {
            b'"' => in_quotes = !in_quotes,
            b'.' if !in_quotes => {
                segs.push(String::from_utf8_lossy(&std::mem::take(&mut cur)).into_owned())
            }
            _ => cur.push(ch),
        }
    }
    segs.push(String::from_utf8_lossy(&cur).into_owned());
    segs
}

/// Go `parseBigQueryTimestampString`: BigQuery's `SAFE_CAST(string AS TIMESTAMP)` accepts
/// RFC 3339 with at most 6 fractional digits.
fn parse_bigquery_timestamp_string(s: &str) -> Result<DateTime<Utc>, String> {
    if SUB_MICRO_TIMESTAMP_RE.is_match(s) {
        return Err(format!(
            "BigQuery TIMESTAMP cast rejects >6 fractional digits in {s:?}"
        ));
    }
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| e.to_string())
}

/// Go `minBigQueryTimestampMicros` / `maxBigQueryTimestampMicros`.
const MIN_BIGQUERY_TIMESTAMP_MICROS: i64 = -62_135_596_800_000_000;
const MAX_BIGQUERY_TIMESTAMP_MICROS: i64 = 253_402_300_799_999_999;

/// Go `evalCompactionTimestampSQL`'s results `(secs, time, isWindow, err)`; Go's zero
/// `time.Time` (SQL NULL) is `None`.
#[derive(Debug)]
struct CompactionEval {
    secs: f64,
    time: Option<DateTime<Utc>>,
    is_window: bool,
    err: Option<String>,
}

impl CompactionEval {
    fn ok(secs: f64, time: Option<DateTime<Utc>>, is_window: bool) -> CompactionEval {
        CompactionEval {
            secs,
            time,
            is_window,
            err: None,
        }
    }

    fn fail(is_window: bool, err: String) -> CompactionEval {
        CompactionEval {
            secs: 0.0,
            time: None,
            is_window,
            err: Some(err),
        }
    }
}

/// Go `evalCompactionTimestampSQL`: simulates BigQuery's evaluation of a compaction seconds or
/// `window_start` / `window_end` projection against a decoded content object (see the Go doc:
/// `CAST(… AS FLOAT64)` rejects RFC 3339 strings, `SAFE_CAST(… AS TIMESTAMP)` rejects > 6
/// fractional digits unless normalized; a bare multiplication overflowing FLOAT64 fails the
/// query, `SAFE_MULTIPLY` yields NULL; out-of-range INT64 / TIMESTAMP fail under `CAST` /
/// `TIMESTAMP_MICROS` and yield NULL under `SAFE_CAST` / `SAFE.TIMESTAMP_MICROS`).
fn eval_compaction_timestamp_sql(expr: &str, content: &Map<String, Value>) -> CompactionEval {
    let sql_part = match expr.rfind(" AS ") {
        Some(idx) => expr[..idx].trim(),
        None => expr,
    };
    const CAST_SUFFIX: &str = " AS INT64))";
    let (mut is_window, mut safe_window, mut safe_multiply) = (false, false, false);
    let mut inner = sql_part;
    for (prefix, safe) in [
        ("SAFE.TIMESTAMP_MICROS(SAFE_CAST(", true),
        ("TIMESTAMP_MICROS(CAST(", false),
    ] {
        let Some(micros) = sql_part.strip_prefix(prefix) else {
            continue;
        };
        let Some(micros) = micros.strip_suffix(CAST_SUFFIX) else {
            continue;
        };
        is_window = true;
        safe_window = safe;
        if let Some(m) = micros
            .strip_prefix("SAFE_MULTIPLY(")
            .and_then(|m| m.strip_suffix(", 1000000)"))
        {
            inner = m;
            safe_multiply = true;
        } else if let Some(m) = micros.strip_suffix(" * 1000000") {
            inner = m;
        } else {
            return CompactionEval::fail(
                true,
                format!("expression {expr:?}: unrecognized conversion of seconds to microseconds"),
            );
        }
        break;
    }

    let exprs = extract_sql_json_exprs(inner);
    let Some(first) = exprs.first() else {
        return CompactionEval::fail(false, format!("no JSON_VALUE expression found in {expr:?}"));
    };
    let path = first.path.as_str();
    let Some(raw_val) = resolve_json_path(content, path) else {
        return CompactionEval::fail(false, format!("path {path:?} missing in content"));
    };
    // JSON_VALUE extracts a scalar as its SQL STRING representation.
    let scalar_str = raw_val.sprint();

    let want_coalesce = format!(
        r"COALESCE(SAFE_CAST(JSON_VALUE(content, '{path}') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '{path}'), r'(\.\d{{6}})\d+', r'\1') AS TIMESTAMP)) / 1e6)"
    );
    let secs: f64 = if inner == want_coalesce {
        match go_parse_float(&scalar_str) {
            Some(f) => f,
            None => {
                let normalized = SUB_MICRO_TIMESTAMP_RE.replace_all(&scalar_str, "${1}");
                match parse_bigquery_timestamp_string(&normalized) {
                    Ok(ts) => ts.timestamp_micros() as f64 / 1e6,
                    Err(err) => {
                        return CompactionEval::fail(
                            is_window,
                            format!(
                                "COALESCE failed to parse {scalar_str:?} as FLOAT64 or BigQuery TIMESTAMP: {err}"
                            ),
                        );
                    }
                }
            }
        }
    } else if let Some(f) = go_parse_float(&scalar_str) {
        // Without the REGEXP_REPLACE + COALESCE expression, BigQuery either fails
        // CAST(... AS FLOAT64) on strings or rejects >6 fractional digits in TIMESTAMP.
        f
    } else if let (Ok(ts), true) = (
        parse_bigquery_timestamp_string(&scalar_str),
        inner.contains("AS TIMESTAMP"),
    ) {
        ts.timestamp_micros() as f64 / 1e6
    } else {
        return CompactionEval::fail(
            is_window,
            format!(
                "expression {expr:?} failed to evaluate {scalar_str:?} under BigQuery conversion rules"
            ),
        );
    };
    if !is_window {
        return CompactionEval::ok(secs, None, false);
    }
    let micros = secs * 1e6;
    if micros.is_infinite() && !secs.is_infinite() {
        if safe_multiply {
            return CompactionEval::ok(secs, None, true);
        }
        return CompactionEval::fail(
            true,
            format!("expression {expr:?}: {secs} * 1000000 overflows FLOAT64"),
        );
    }
    if micros.is_nan() || micros < i64::MIN as f64 || micros >= i64::MAX as f64 {
        if safe_window {
            return CompactionEval::ok(secs, None, true);
        }
        return CompactionEval::fail(
            true,
            format!("expression {expr:?}: CAST({micros} AS INT64) overflows INT64"),
        );
    }
    let m = micros as i64;
    if !(MIN_BIGQUERY_TIMESTAMP_MICROS..=MAX_BIGQUERY_TIMESTAMP_MICROS).contains(&m) {
        if safe_window {
            return CompactionEval::ok(secs, None, true);
        }
        return CompactionEval::fail(
            true,
            format!("expression {expr:?}: TIMESTAMP_MICROS({m}) is out of the TIMESTAMP range"),
        );
    }
    CompactionEval::ok(secs, DateTime::from_timestamp_micros(m), true)
}

/// Go `strconv.ParseFloat(s, 64)` (decimal forms; Go's hex floats are not needed): an
/// out-of-range value is an error in Go (`ErrRange`) where Rust parses it to an infinity, while
/// the `inf` / `infinity` / `nan` spellings parse in both.
fn go_parse_float(s: &str) -> Option<f64> {
    let f = s.parse::<f64>().ok()?;
    let spelled_inf = s
        .trim_start_matches(['+', '-'])
        .to_ascii_lowercase()
        .starts_with("inf");
    (!f.is_infinite() || spelled_inf).then_some(f)
}

#[test]
fn eval_helpers_follow_go() {
    // splitJSONPathSegments keeps quoted dots.
    assert_eq!(
        split_json_path_segments(r#"a2a_metadata."a2a:task_id".x"#),
        ["a2a_metadata", "a2a:task_id", "x"]
    );
    // resolveJSONPath: nested objects, `$`, missing and null.
    let doc: Map<String, Value> =
        serde_json::from_str(r#"{"usage": {"prompt": 3}, "n": null}"#).unwrap();
    assert_eq!(
        resolve_json_path(&doc, "$.usage.prompt").map(|v| v.sprint()),
        Some("3".to_owned())
    );
    assert!(resolve_json_path(&doc, "$").is_some());
    assert!(resolve_json_path(&doc, "$.n").is_none());
    assert!(resolve_json_path(&doc, "$.usage.missing").is_none());
    assert!(resolve_json_path(&doc, "usage").is_none());
    // An RFC 3339 string with nanoseconds goes through the COALESCE's REGEXP_REPLACE.
    let defs = Config {
        project_id: "p".to_owned(),
        ..Config::zero()
    }
    .view_definitions()
    .unwrap();
    let query = &defs
        .iter()
        .find(|d| d.event_type == EVENT_EVENT_COMPACTION)
        .unwrap()
        .query;
    let (exprs, aliases) = extract_extra_projections(query);
    let by_alias: BTreeMap<String, String> = aliases.into_iter().zip(exprs).collect();
    let content: Map<String, Value> = serde_json::from_str(
        r#"{"start_timestamp": "2025-10-09T08:53:20.123456789Z", "end_timestamp": "x"}"#,
    )
    .unwrap();
    let got = eval_compaction_timestamp_sql(&by_alias["window_start"], &content);
    // 2025-10-09T08:53:20Z = 1760000000; the seconds pass through FLOAT64 as in Go, so the
    // microseconds may round by one.
    let want = DateTime::from_timestamp(1_760_000_000, 123_456_000).unwrap();
    let got_time = got.time.unwrap();
    assert!(
        (got_time - want).num_microseconds().unwrap().abs() <= 1,
        "{got_time} vs {want}"
    );
    let got = eval_compaction_timestamp_sql(&by_alias["start_seconds"], &content);
    assert!(!got.is_window && got.err.is_none(), "{got:?}");
    let got = eval_compaction_timestamp_sql(&by_alias["window_end"], &content);
    assert!(got.err.is_some() && got.is_window, "{got:?}");
    assert!(parse_bigquery_timestamp_string("2025-10-09T08:53:20.1234567Z").is_err());
}

// --- Helpers of the plugin's view lifecycle ---

/// A BigQuery client whose requests go to `http` (Go `bq.NewClient(ctx, "test-project",
/// option.WithHTTPClient(&http.Client{Transport: &mockTransport{…}}))`).
fn bq_client_over(http: Arc<ScriptedClient>) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        http_client: Some(http),
        without_authentication: true,
        endpoint: Some("http://bq.test/bigquery/v2/".into()),
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new("test-project", &options).expect("bq.NewClient"))
}

/// The body of a finished job `job_id`.
fn done_job_json(job_id: &str) -> String {
    format!(
        r#"{{"jobReference":{{"projectId":"test-project","jobId":"{job_id}"}},"status":{{"state":"DONE"}}}}"#
    )
}

/// Go `newJobCountingBQClient(t, jobs)`: a BigQuery client whose view DDL jobs succeed and are
/// counted in `jobs`.
fn new_job_counting_bq_client(jobs: Arc<AtomicUsize>) -> Arc<BigQueryClient> {
    bq_client_over(ScriptedClient::new(move |r: &RecordedRequest| {
        if r.method == "POST" && r.path.contains("/jobs") {
            jobs.fetch_add(1, Ordering::SeqCst);
            return FakeResponse::json(done_job_json("job-1"));
        }
        FakeResponse::json("{}")
    }))
}

/// Go `collectingRunner`: hands each view to `inner` (Go runs the garbage collector first; an
/// `Arc` that nothing holds is already gone).
struct CollectingRunner {
    inner: Arc<dyn ViewRunner>,
}

#[async_trait::async_trait]
impl ViewRunner for CollectingRunner {
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error> {
        self.inner.run_view_ddl(ctx, view_name, query).await
    }
}

/// Cancels its token when dropped: a transport handler whose request the client abandons
/// (Go `<-r.Context().Done()`).
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Go `log.Default()` with its output captured: a recording logger that is the standard one.
#[derive(Default)]
struct StdDefaultRecorder(RecordingLogger);

impl Logger for StdDefaultRecorder {
    fn printf(&self, args: std::fmt::Arguments<'_>) {
        self.0.printf(args);
    }
    fn println(&self, args: std::fmt::Arguments<'_>) {
        self.0.println(args);
    }
    fn is_std_default(&self) -> bool {
        true
    }
}

/// Go `bqaap.runner = runner`: returns the runner it replaces.
fn set_view_runner(
    bqaap: &BigQueryAgentAnalyticsPlugin,
    runner: Option<Arc<dyn ViewRunner>>,
) -> Option<Arc<dyn ViewRunner>> {
    std::mem::replace(&mut bqaap.state.views.lock().runner, runner)
}

/// A Go `onRun` from a closure.
fn on_run<F, Fut>(f: F) -> OnRun
where
    F: Fn(Context, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), Error>> + Send + 'static,
{
    Arc::new(move |ctx, name| Box::pin(f(ctx, name)))
}

/// Go `return ctx.Err()`.
fn ctx_result(ctx: &Context) -> Result<(), Error> {
    match ctx.err() {
        Some(err) => Err(Error::Context(err)),
        None => Ok(()),
    }
}

/// Go `errors.Is(err, errPluginClosed)`.
fn is_plugin_closed<T>(r: &Result<T, Error>) -> bool {
    matches!(r, Err(e) if e.to_string() == PLUGIN_CLOSED_TEXT)
}

/// Go `go func() { done <- bqaap.SyncViews(ctx) }()`.
fn spawn_sync_views(
    bqaap: &BigQueryAgentAnalyticsPlugin,
    ctx: Context,
) -> tokio::task::JoinHandle<Result<(), Error>> {
    let bqaap = bqaap.clone();
    tokio::spawn(async move { bqaap.sync_views(&ctx).await })
}

/// Go `p.Close()`.
async fn close_plugin(p: &gadk_agent::Plugin) -> Result<(), gadk_agent::AgentError> {
    (p.close().expect("close function"))().await
}

/// Go `select { case <-ch: case <-time.After(d): t.Fatal(msg) }`.
async fn within<T>(d: Duration, fut: impl std::future::Future<Output = T>, msg: &str) -> T {
    tokio::time::timeout(d, fut)
        .await
        .unwrap_or_else(|_| panic!("{msg}"))
}
