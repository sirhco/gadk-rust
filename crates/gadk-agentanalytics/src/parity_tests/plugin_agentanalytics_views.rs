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

// parity: skipped TestViews_JSONPathsMatchEmittedPayloads: deferred to 7b-3 (checks the views' JSON paths against rows the plugin emits through the batch processor and event callbacks)

// parity: skipped TestViews_AsyncStartupAndCloseLifecycle: deferred to 7b-3 (plugin state: startAsyncViewCreation, Close and the view gate)

// parity: skipped TestBigQueryAgentAnalyticsPlugin_SyncViews: deferred to 7b-3 (the plugin's SyncViews method and plugin state)

// parity: skipped TestPluginState_ReleasesViewPassAndRunner: deferred to 7b-3 (plugin state and Go weak-pointer/GC release of the runner)

// parity: adapted: the plugin half (an EVENT_COMPACTION row written through the plugin with 1e308 timestamps, then evaluated) is deferred to 7b-3; the SQL-evaluation half is ported
#[test]
fn test_views_event_compaction_window_out_of_range_is_null() {
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

    // Deferred to 7b-3: "The plugin writes such numbers unchanged, so the view meets them"
    // (setupTestPluginWithConfig, OnEventCallback, collectRows).
}

// parity: adapted: the first Go subtest drives the plugin's SyncViews (deferred to 7b-3); its syncAnalyticsViews pass (SyncViews passes a nil logf) runs here directly; time is paused
#[tokio::test(start_paused = true)]
async fn test_sync_analytics_views_context_ends_pass() {
    let ctx = Context::background();
    let none = Default::default();

    // SyncViews caller deadline is reported once as canceled (the plugin's SyncViews
    // wrapper is 7b-3; this is the syncAnalyticsViews call it makes).
    {
        let defs =
            view_definitions_with_denied("test-project", "test_dataset", "test-table", "v", &none)
                .unwrap();
        let on_run: OnRun = Arc::new(|run_ctx: Context, _name: String| {
            Box::pin(async move {
                run_ctx.done().await;
                Err(Error::Context(
                    run_ctx.err().unwrap_or(ContextError::Canceled),
                ))
            })
        });
        let runner = FakeViewRunner::new(HashMap::new(), Some(on_run));
        let deadline_ctx = ctx.with_timeout(Duration::from_millis(100));
        let err = sync_analytics_views(&deadline_ctx, &runner, &defs, None)
            .await
            .unwrap_err();
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

// parity: skipped TestNewBigQueryAgentAnalyticsPlugin_ResolvesProjectForTableAndViews: deferred to 7b-3 (NewBigQueryAgentAnalyticsPlugin project resolution for the table, write stream, runner and views)

// ---------------------------------------------------------------------------------------------
// Helpers of views_test.go.

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
    #[allow(dead_code)] // Go's field, unused by the ported tests
    func: String,
    #[allow(dead_code)] // Go's field, unused by the ported tests
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
    #[allow(dead_code)] // Go's result, unused by the ported tests
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
