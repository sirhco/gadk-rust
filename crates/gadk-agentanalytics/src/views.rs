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

//! The per-event-type view definitions ([`Config::view_definitions`]), the DDL runner
//! ([`ViewRunner`], [`BigQueryViewRunner`]) and [`sync_analytics_views`].

use std::collections::BTreeSet;
use std::sync::{LazyLock, Weak};
use std::time::Duration;

use gadk_platform::go_quote;

use crate::bigquery::BigQueryClient;
use crate::config::{Config, DEFAULT_VIEW_PREFIX, Logger};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error, is_conflict_error, status_code};
use crate::event_types::{
    EVENT_A2A_INTERACTION, EVENT_AGENT_COMPLETED, EVENT_AGENT_ERROR, EVENT_AGENT_RESPONSE,
    EVENT_AGENT_STARTING, EVENT_AGENT_STATE_CHECKPOINT, EVENT_AGENT_TRANSFER,
    EVENT_EVENT_COMPACTION, EVENT_HITL_CONFIRMATION_REQUEST, EVENT_HITL_CREDENTIAL_REQUEST,
    EVENT_HITL_INPUT_REQUEST, EVENT_INVOCATION_COMPLETED, EVENT_INVOCATION_ERROR,
    EVENT_INVOCATION_STARTING, EVENT_LLM_ERROR, EVENT_LLM_REQUEST, EVENT_LLM_RESPONSE,
    EVENT_NODE_ERROR, EVENT_NODE_OUTPUT, EVENT_STATE_DELTA, EVENT_TOOL_COMPLETED, EVENT_TOOL_ERROR,
    EVENT_TOOL_PAUSED, EVENT_TOOL_STARTING, EVENT_USER_MESSAGE_RECEIVED,
};
use crate::redaction::parse_payload_column_denylist;
use crate::schema_upgrade::VIEW_TABLE_TYPE;

/// Each view's DDL gets its own deadline under the pass's context.
pub(crate) const VIEW_DDL_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest dataset ID, and table or view name, in bytes.
pub(crate) const MAX_BIGQUERY_IDENTIFIER_LEN: usize = 1024;

/// A BigQuery view over the events table for one event type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDefinition {
    /// The unqualified view name (for example `v_llm_request`).
    pub name: String,
    /// The `event_type` value the view filters on.
    pub event_type: String,
    /// The complete `CREATE OR REPLACE VIEW` statement.
    pub query: String,
}

/// The columns selected by every view (adk-python `_VIEW_COMMON_COLUMNS`).
pub(crate) const VIEW_COMMON_COLUMNS: [&str; 13] = [
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

/// One view's event type and its extra column expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventViewSpec {
    /// The `event_type` the view filters on.
    pub(crate) event_type: &'static str,
    /// The extra columns: `<expr> AS <name>` SQL expressions.
    pub(crate) extra_cols: &'static [&'static str],
}

/// The 25 views, in adk-python's `_EVENT_VIEW_DEFS` order.
pub(crate) static EVENT_VIEW_SPECS: &[EventViewSpec] = &[
    EventViewSpec {
        event_type: EVENT_USER_MESSAGE_RECEIVED,
        extra_cols: &[],
    },
    EventViewSpec {
        event_type: EVENT_LLM_REQUEST,
        extra_cols: &[
            "JSON_VALUE(attributes, '$.model') AS model",
            "content AS request_content",
            "JSON_QUERY(attributes, '$.llm_config') AS llm_config",
            "JSON_QUERY(attributes, '$.tools') AS tools",
        ],
    },
    EventViewSpec {
        event_type: EVENT_LLM_RESPONSE,
        extra_cols: &[
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
    },
    EventViewSpec {
        event_type: EVENT_LLM_ERROR,
        extra_cols: &["CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms"],
    },
    EventViewSpec {
        event_type: EVENT_TOOL_STARTING,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.args') AS tool_args",
            "JSON_VALUE(content, '$.tool_origin') AS tool_origin",
        ],
    },
    EventViewSpec {
        event_type: EVENT_TOOL_COMPLETED,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.result') AS tool_result",
            "JSON_VALUE(content, '$.tool_origin') AS tool_origin",
            "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
            "JSON_VALUE(attributes, '$.adk.pause_kind') AS pause_kind",
            "JSON_VALUE(attributes, '$.adk.function_call_id') AS function_call_id",
        ],
    },
    EventViewSpec {
        event_type: EVENT_TOOL_ERROR,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.args') AS tool_args",
            "JSON_VALUE(content, '$.tool_origin') AS tool_origin",
            "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
        ],
    },
    EventViewSpec {
        event_type: EVENT_AGENT_STARTING,
        extra_cols: &[
            r"COALESCE(JSON_VALUE(content, '$.text_summary'), IF(REGEXP_CONTAINS(JSON_VALUE(content, '$'), r'^(?:<(?:function|bound method) |<.+ at 0x[0-9a-fA-F]+>|functools\.partial\()'), NULL, JSON_VALUE(content, '$'))) AS agent_instruction",
            "JSON_VALUE(attributes, '$.instruction_source') AS instruction_source",
            "JSON_VALUE(attributes, '$.static_instruction') AS static_instruction",
            "JSON_VALUE(attributes, '$.agent_description') AS agent_description",
            "JSON_VALUE(attributes, '$.model') AS model",
            "JSON_QUERY(attributes, '$.sub_agents') AS sub_agents",
        ],
    },
    EventViewSpec {
        event_type: EVENT_AGENT_COMPLETED,
        extra_cols: &["CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms"],
    },
    EventViewSpec {
        event_type: EVENT_AGENT_ERROR,
        extra_cols: &[
            "CAST(JSON_VALUE(latency_ms, '$.total_ms') AS INT64) AS total_ms",
            "JSON_VALUE(content, '$.error_traceback') AS error_traceback",
        ],
    },
    EventViewSpec {
        event_type: EVENT_INVOCATION_STARTING,
        extra_cols: &[],
    },
    EventViewSpec {
        event_type: EVENT_INVOCATION_COMPLETED,
        extra_cols: &[],
    },
    EventViewSpec {
        event_type: EVENT_INVOCATION_ERROR,
        extra_cols: &["JSON_VALUE(content, '$.error_traceback') AS error_traceback"],
    },
    EventViewSpec {
        event_type: EVENT_STATE_DELTA,
        extra_cols: &["JSON_QUERY(attributes, '$.state_delta') AS state_delta"],
    },
    EventViewSpec {
        event_type: EVENT_HITL_CREDENTIAL_REQUEST,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.args') AS tool_args",
        ],
    },
    EventViewSpec {
        event_type: EVENT_HITL_CONFIRMATION_REQUEST,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.args') AS tool_args",
        ],
    },
    EventViewSpec {
        event_type: EVENT_HITL_INPUT_REQUEST,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.args') AS tool_args",
        ],
    },
    EventViewSpec {
        event_type: EVENT_A2A_INTERACTION,
        extra_cols: &[
            "content AS response_content",
            r#"JSON_VALUE(attributes, '$.a2a_metadata."a2a:task_id"') AS a2a_task_id"#,
            r#"JSON_VALUE(attributes, '$.a2a_metadata."a2a:context_id"') AS a2a_context_id"#,
            r#"JSON_QUERY(attributes, '$.a2a_metadata."a2a:request"') AS a2a_request"#,
            r#"JSON_QUERY(attributes, '$.a2a_metadata."a2a:response"') AS a2a_response"#,
        ],
    },
    EventViewSpec {
        event_type: EVENT_AGENT_RESPONSE,
        extra_cols: &[
            "JSON_VALUE(content, '$.response') AS response_text",
            "JSON_VALUE(attributes, '$.source_event_id') AS source_event_id",
            "JSON_VALUE(attributes, '$.source_event_author') AS source_event_author",
            "JSON_VALUE(attributes, '$.source_event_branch') AS source_event_branch",
        ],
    },
    EventViewSpec {
        event_type: EVENT_AGENT_TRANSFER,
        extra_cols: &[
            "JSON_VALUE(content, '$.from_agent') AS from_agent",
            "JSON_VALUE(content, '$.to_agent') AS to_agent",
            "JSON_VALUE(attributes, '$.adk.source_event_id') AS source_event_id",
        ],
    },
    // Deliberately differs from adk-python's EVENT_COMPACTION projection (which uses bare CAST(...
    // AS FLOAT64), * 1000000, and TIMESTAMP_MICROS): SAFE_CAST, SAFE_MULTIPLY, and
    // SAFE.TIMESTAMP_MICROS yield NULL instead of failing the query on out-of-range numbers, and
    // the COALESCE fallback parses legacy RFC 3339 strings (including 9-digit nanoseconds). Both
    // projections expose the same column names and BigQuery types.
    EventViewSpec {
        event_type: EVENT_EVENT_COMPACTION,
        extra_cols: &[
            r"COALESCE(SAFE_CAST(JSON_VALUE(content, '$.start_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.start_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6) AS start_seconds",
            r"COALESCE(SAFE_CAST(JSON_VALUE(content, '$.end_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.end_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6) AS end_seconds",
            r"SAFE.TIMESTAMP_MICROS(SAFE_CAST(SAFE_MULTIPLY(COALESCE(SAFE_CAST(JSON_VALUE(content, '$.start_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.start_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6), 1000000) AS INT64)) AS window_start",
            r"SAFE.TIMESTAMP_MICROS(SAFE_CAST(SAFE_MULTIPLY(COALESCE(SAFE_CAST(JSON_VALUE(content, '$.end_timestamp') AS FLOAT64), UNIX_MICROS(SAFE_CAST(REGEXP_REPLACE(JSON_VALUE(content, '$.end_timestamp'), r'(\.\d{6})\d+', r'\1') AS TIMESTAMP)) / 1e6), 1000000) AS INT64)) AS window_end",
            "JSON_QUERY(content, '$.compacted_content') AS compacted_content",
        ],
    },
    EventViewSpec {
        event_type: EVENT_AGENT_STATE_CHECKPOINT,
        extra_cols: &[
            "JSON_QUERY(content, '$.agent_state') AS agent_state",
            "JSON_TYPE(JSON_QUERY(content, '$.agent_state')) AS agent_state_type",
            "SAFE_CAST(JSON_VALUE(content, '$.end_of_agent') AS BOOL) AS end_of_agent",
            "JSON_VALUE(attributes, '$.adk.source_event_id') AS source_event_id",
        ],
    },
    EventViewSpec {
        event_type: EVENT_TOOL_PAUSED,
        extra_cols: &[
            "JSON_VALUE(content, '$.tool') AS tool_name",
            "JSON_QUERY(content, '$.args') AS tool_args",
            "JSON_VALUE(attributes, '$.adk.pause_kind') AS pause_kind",
            "JSON_VALUE(attributes, '$.adk.function_call_id') AS function_call_id",
        ],
    },
    EventViewSpec {
        event_type: EVENT_NODE_OUTPUT,
        extra_cols: &[
            "JSON_VALUE(attributes, '$.adk.node.path') AS node_path",
            "JSON_VALUE(attributes, '$.adk.node.run_id') AS node_run_id",
            "JSON_VALUE(attributes, '$.adk.node.parent_run_id') AS node_parent_run_id",
            "content AS output",
        ],
    },
    EventViewSpec {
        event_type: EVENT_NODE_ERROR,
        extra_cols: &[
            "JSON_VALUE(attributes, '$.adk.node.path') AS node_path",
            "JSON_VALUE(attributes, '$.adk.node.run_id') AS node_run_id",
            "JSON_VALUE(attributes, '$.adk.node.parent_run_id') AS node_parent_run_id",
            "JSON_VALUE(content, '$.error_code') AS error_code",
        ],
    },
];

impl Config {
    /// The `CREATE OR REPLACE VIEW` statements for this config, one per view in `EVENT_VIEW_SPECS`
    /// order, honoring `view_prefix` (empty means `"v"`) and `payload_column_denylist`. Errors for
    /// an invalid identifier or a view name equal (case-insensitively) to the table name.
    pub fn view_definitions(&self) -> Result<Vec<ViewDefinition>, Error> {
        let c = self.clone().apply_defaults();
        let denied = parse_payload_column_denylist(&c.payload_column_denylist)?;
        view_definitions_with_denied(
            &c.project_id,
            &c.dataset_id,
            &c.table_name,
            &c.view_prefix,
            &denied,
        )
    }
}

/// The view definitions for the given names and denied payload columns (see
/// [`Config::view_definitions`]).
pub(crate) fn view_definitions_with_denied(
    project_id: &str,
    dataset_id: &str,
    table_name: &str,
    view_prefix: &str,
    denied_columns: &BTreeSet<String>,
) -> Result<Vec<ViewDefinition>, Error> {
    let view_prefix = if view_prefix.is_empty() {
        DEFAULT_VIEW_PREFIX
    } else {
        view_prefix
    };
    validate_view_identifiers(project_id, dataset_id, table_name, view_prefix)?;

    let mut defs = Vec::with_capacity(EVENT_VIEW_SPECS.len());
    for spec in EVENT_VIEW_SPECS {
        // Event types are ASCII, so ASCII lower-casing is enough.
        let view_name = format!("{view_prefix}_{}", spec.event_type.to_ascii_lowercase());
        if view_name.len() > MAX_BIGQUERY_IDENTIFIER_LEN {
            return Err(Error::msg(format!(
                "BigQuery view name {} is longer than {MAX_BIGQUERY_IDENTIFIER_LEN} bytes; use a shorter view prefix",
                go_quote(&view_name)
            )));
        }
        // Datasets can be case-insensitive, so a view must not differ from the events table
        // only in case either.
        if ascii_equal_fold(&view_name, table_name) {
            return Err(Error::msg(format!(
                "BigQuery view name {} collides with the events table {}; use another view prefix",
                go_quote(&view_name),
                go_quote(table_name)
            )));
        }
        let extra = project_view_columns(spec.extra_cols, denied_columns);
        let mut cols: Vec<&str> = Vec::with_capacity(VIEW_COMMON_COLUMNS.len() + extra.len());
        cols.extend(VIEW_COMMON_COLUMNS);
        cols.extend(extra);

        let query = format!(
            "CREATE OR REPLACE VIEW `{project_id}.{dataset_id}.{view_name}` AS\nSELECT\n  {}\nFROM\n  `{project_id}.{dataset_id}.{table_name}`\nWHERE\n  event_type = '{}'\n",
            cols.join(",\n  "),
            spec.event_type,
        );
        defs.push(ViewDefinition {
            name: view_name,
            event_type: spec.event_type.to_owned(),
            query,
        });
    }
    Ok(defs)
}

/// Whether `s` equals the ASCII string `ascii` under Unicode simple case folding (`ascii` is a view
/// name: a validated prefix, `_` and a lowercased event type). Characters are compared one by one;
/// the only non-ASCII characters whose fold orbit holds an ASCII character are U+212A KELVIN SIGN
/// (`k`, `K`) and U+017F LATIN SMALL LETTER LONG S (`s`, `S`).
pub(crate) fn ascii_equal_fold(ascii: &str, s: &str) -> bool {
    let mut runes = s.chars();
    for a in ascii.bytes() {
        let Some(r) = runes.next() else {
            return false;
        };
        let matches = match r {
            '\u{212A}' => a.eq_ignore_ascii_case(&b'k'),
            '\u{017F}' => a.eq_ignore_ascii_case(&b's'),
            r if r.is_ascii() => a.eq_ignore_ascii_case(&(r as u8)),
            _ => false,
        };
        if !matches {
            return false;
        }
    }
    runes.next().is_none()
}

/// Drops the extra expressions that reference a denied column (adk-python `_project_view_columns`).
pub(crate) fn project_view_columns(
    extra_cols: &[&'static str],
    denied_columns: &BTreeSet<String>,
) -> Vec<&'static str> {
    if denied_columns.is_empty() || extra_cols.is_empty() {
        return extra_cols.to_vec();
    }
    extra_cols
        .iter()
        .copied()
        .filter(|expr| !references_denied_column(expr, denied_columns))
        .collect()
}

/// Whether the SQL before the last ` AS `, with its string literals emptied, names a denied column
/// as an identifier token.
pub(crate) fn references_denied_column(expr: &str, denied_columns: &BTreeSet<String>) -> bool {
    let sql_part = match expr.rfind(" AS ") {
        Some(idx) => &expr[..idx],
        None => expr,
    };
    let sql_part = strip_sql_string_literals(sql_part);
    denied_columns
        .iter()
        .any(|col| contains_identifier_token(&sql_part, col))
}

/// Empties every single-quoted SQL string literal (a backslash inside one escapes the next byte),
/// so identifier scans skip JSON paths such as `'$.content'`.
pub(crate) fn strip_sql_string_literals(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if !in_quote {
            out.push(ch);
            if ch == b'\'' {
                in_quote = true;
            }
        } else if ch == b'\\' && i + 1 < bytes.len() {
            i += 1;
        } else if ch == b'\'' {
            out.push(b'\'');
            in_quote = false;
        }
        i += 1;
    }
    // Every kept run starts and ends at an ASCII byte (a quote or the string's ends), so the
    // output is valid UTF-8 and the lossy conversion never replaces anything.
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `s` contains `ident` bounded on both sides by a non-`[A-Za-z0-9_]` byte or the string's
/// ends. Byte-wise, so a match at any byte offset is found without slicing inside a UTF-8 sequence.
pub(crate) fn contains_identifier_token(s: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    let (s, ident) = (s.as_bytes(), ident.as_bytes());
    let mut start = 0;
    while start < s.len() {
        let Some(idx) = s[start..].windows(ident.len()).position(|w| w == ident) else {
            return false;
        };
        let pos = start + idx;
        let end = pos + ident.len();
        let left_ok = pos == 0 || !is_ident_char(s[pos - 1]);
        let right_ok = end == s.len() || !is_ident_char(s[end]);
        if left_ok && right_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Whether `b` is in `[A-Za-z0-9_]`.
pub(crate) fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `s` is a valid project ID: non-empty `[A-Za-z0-9_.:-]`.
pub(crate) fn is_valid_project_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| is_ident_char(b) || b == b'-' || b == b'.' || b == b':')
}

/// Whether `s` is a valid identifier: non-empty `[A-Za-z0-9_-]`.
pub(crate) fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| is_ident_char(b) || b == b'-')
}

/// Whether `s` is a valid dataset ID: 1 to [`MAX_BIGQUERY_IDENTIFIER_LEN`] bytes of `[A-Za-z0-9_]`.
pub(crate) fn is_valid_dataset_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_BIGQUERY_IDENTIFIER_LEN && s.bytes().all(is_ident_char)
}

/// A Unicode letter (category L) or digit (category Nd). The categories come from the `regex`
/// crate's Unicode tables, whose Unicode version may differ slightly from that of an ADK server for
/// newly assigned code points.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static GO_LETTER_OR_DIGIT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[\p{L}\p{Nd}]$").expect("valid letter/digit regex"));

/// Whether `s` is a valid table name (Unicode letters and digits, `_`, `-`, space): not blank
/// (trimming Unicode White_Space) and at most [`MAX_BIGQUERY_IDENTIFIER_LEN`] bytes.
pub(crate) fn is_valid_table_name(s: &str) -> bool {
    // `str::trim` trims Unicode White_Space.
    if s.trim().is_empty() || s.len() > MAX_BIGQUERY_IDENTIFIER_LEN {
        return false;
    }
    s.chars().all(|r| {
        if r.is_ascii() {
            r.is_ascii_alphanumeric() || r == '_' || r == '-' || r == ' '
        } else {
            let mut buf = [0u8; 4];
            GO_LETTER_OR_DIGIT.is_match(r.encode_utf8(&mut buf))
        }
    })
}

/// The project ID, dataset ID, table name and view prefix are checked in that order; the first
/// invalid one is reported with its quoted text (`go_quote`).
pub(crate) fn validate_view_identifiers(
    project_id: &str,
    dataset_id: &str,
    table_name: &str,
    view_prefix: &str,
) -> Result<(), Error> {
    if !is_valid_project_id(project_id) {
        return Err(Error::msg(format!(
            "invalid BigQuery project ID {}",
            go_quote(project_id)
        )));
    }
    if !is_valid_dataset_id(dataset_id) {
        return Err(Error::msg(format!(
            "invalid BigQuery dataset ID {}",
            go_quote(dataset_id)
        )));
    }
    if !is_valid_table_name(table_name) {
        return Err(Error::msg(format!(
            "invalid BigQuery table name {}",
            go_quote(table_name)
        )));
    }
    if !is_valid_identifier(view_prefix) {
        return Err(Error::msg(format!(
            "invalid BigQuery view prefix {}",
            go_quote(view_prefix)
        )));
    }
    Ok(())
}

/// Runs one view's DDL so view creation can be tested without BigQuery.
#[async_trait::async_trait]
pub trait ViewRunner: Send + Sync {
    /// Runs the DDL `query` that creates the view `view_name`.
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error>;
}

/// The BigQuery DDL runner The client is weak: the plugin keeps it alive; once it is dropped every
/// run fails with the "client is not configured" error.
#[derive(Debug, Clone)]
pub struct BigQueryViewRunner {
    pub(crate) client: Weak<BigQueryClient>,
    pub(crate) project_id: String,
    pub(crate) dataset_id: String,
}

impl BigQueryViewRunner {
    /// A runner over `client` for views in `project_id.dataset_id` (an empty project means the
    /// client's project).
    pub fn new(
        client: Weak<BigQueryClient>,
        project_id: impl Into<String>,
        dataset_id: impl Into<String>,
    ) -> BigQueryViewRunner {
        BigQueryViewRunner {
            client,
            project_id: project_id.into(),
            dataset_id: dataset_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl ViewRunner for BigQueryViewRunner {
    /// An existing view with the same query (trimmed) is left alone; an existing table of another
    /// type is refused (`cannot replace existing BigQuery <TYPE> "<view>" with a view`); otherwise
    /// a `CREATE OR REPLACE VIEW` query job runs and is awaited, and its error returned.
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error> {
        let Some(client) = self.client.upgrade() else {
            return Err(Error::msg(
                "agentanalytics: BigQuery client is not configured for view creation",
            ));
        };
        if !self.dataset_id.is_empty()
            && !view_name.is_empty()
            && let Some((_, select_body)) = query.split_once(" AS\n")
        {
            // An empty project is the client's project.
            let project_id = if self.project_id.is_empty() {
                client.project_id()
            } else {
                self.project_id.as_str()
            };
            // A failed read is left for the job to decide.
            if let Ok(md) = client
                .get_table(ctx, project_id, &self.dataset_id, view_name)
                .await
            {
                // CREATE OR REPLACE VIEW cannot replace a table that is not a view, so
                // say so instead of running a job that must fail. An empty type is left
                // for the job to decide.
                if !md.table_type.is_empty() && md.table_type != VIEW_TABLE_TYPE {
                    return Err(Error::msg(format!(
                        "cannot replace existing BigQuery {} {} with a view",
                        md.table_type,
                        go_quote(view_name)
                    )));
                }
                // Both queries are trimmed of Unicode White_Space.
                if md.table_type == VIEW_TABLE_TYPE && md.view_query.trim() == select_body.trim() {
                    return Ok(());
                }
            }
        }
        // Ending ctx stops the request or the wait; a job BigQuery already accepted is not
        // canceled on the server.
        let job = client.insert_query_job(ctx, query).await?;
        let status = match &job.last_status {
            Some(status) if status.done() => status.clone(),
            _ => client.wait_job(ctx, &job).await?,
        };
        match status.err() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// Runs every definition (each under its own `VIEW_DDL_TIMEOUT`); a conflict counts as created;
/// stops when `ctx` ends; joins the errors. `logf` (a non-default logger) gets each failure that is
/// not a cancellation and the pass summary.
pub async fn sync_analytics_views(
    ctx: &Context,
    runner: &dyn ViewRunner,
    defs: &[ViewDefinition],
    logf: Option<&dyn Logger>,
) -> Result<(), Error> {
    let mut errs = Vec::new();
    let mut created = 0usize;
    let mut failed = 0usize;
    for (i, def) in defs.iter().enumerate() {
        if let Some(err) = ctx.err() {
            let skipped: Vec<&str> = defs[i..].iter().map(|d| d.name.as_str()).collect();
            errs.push(Error::wrap_text(
                format!(
                    "view creation canceled before {} (skipped: {}): {err}",
                    def.name,
                    skipped.join(", ")
                ),
                Error::Context(err),
            ));
            break;
        }
        let ddl_ctx = ctx.with_timeout(VIEW_DDL_TIMEOUT);
        let result = runner.run_view_ddl(&ddl_ctx, &def.name, &def.query).await;
        ddl_ctx.cancel();
        let err = match result {
            Ok(()) => {
                created += 1;
                continue;
            }
            Err(err) if is_conflict_error(&err) => {
                created += 1;
                continue;
            }
            Err(err) => err,
        };
        // Once ctx is done, the error ends the pass whatever it says: a deadline is not a
        // Canceled error, and a transport need not wrap ctx's error. A per-view timeout
        // leaves ctx alive and is a failure.
        if let Some(parent_err) = ctx.err() {
            if err.is_context(parent_err) {
                errs.push(Error::wrap(
                    format_args!("view creation canceled during {}", def.name),
                    err,
                ));
            } else {
                errs.push(Error::wrap_text(
                    format!(
                        "view creation canceled during {}: {parent_err} ({err})",
                        def.name
                    ),
                    Error::Context(parent_err),
                ));
            }
            break;
        }
        failed += 1;
        if let Some(logf) = logf
            && !err.is_context(ContextError::Canceled)
            && status_code(&err) != Code::Canceled
        {
            logf.printf(format_args!(
                "agentanalytics: failed to create view {}: {err}",
                def.name
            ));
        }
        errs.push(Error::wrap(
            format_args!("failed to create view {}", def.name),
            err,
        ));
    }
    if let Some(logf) = logf
        && ctx.err() != Some(ContextError::Canceled)
        && failed > 0
    {
        logf.printf(format_args!(
            "agentanalytics: view creation finished: {created} created, {failed} failed of {}; call sync_views to retry",
            defs.len()
        ));
    }
    match Error::join(errs) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::Value;

    use super::*;

    fn fixture_cases(rel: &str) -> Vec<Value> {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", rel]
            .iter()
            .collect();
        let text = fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        doc["cases"].as_array().unwrap().clone()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn json_set(v: &Value) -> BTreeSet<String> {
        v.as_array()
            .map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default()
    }

    /// A caller's runner returning `Error::other(ContextError::Canceled)` (Go: a runner whose
    /// error wraps `context.Canceled`) takes Go's canceled paths: under a canceled pass the
    /// error is wrapped as is, and under a live pass it fails the view without a log line.
    #[tokio::test(start_paused = true)]
    async fn runner_error_other_context_canceled_takes_go_canceled_paths() {
        use std::collections::HashMap;

        use crate::bigquery::fake::{FakeViewRunner, OnRun, RecordingLogger};

        let ctx = Context::background();
        let none = Default::default();
        let defs =
            view_definitions_with_denied("test-project", "test_dataset", "test-table", "v", &none)
                .unwrap();

        // Canceled pass: Go `errors.Is(err, ctx.Err())` holds, so the text is not doubled.
        let pass_ctx = ctx.with_cancel();
        let cancel_ctx = pass_ctx.clone();
        let on_run: OnRun = Arc::new(move |_ctx: Context, _name: String| {
            cancel_ctx.cancel();
            Box::pin(async { Err(Error::other(ContextError::Canceled)) })
        });
        let runner = FakeViewRunner::new(HashMap::new(), Some(on_run));
        let logger = RecordingLogger::default();
        let err = sync_analytics_views(&pass_ctx, &runner, &defs, Some(&logger))
            .await
            .unwrap_err();
        pass_ctx.cancel();
        assert!(err.is_context(ContextError::Canceled), "{err}");
        assert_eq!(
            err.to_string(),
            "view creation canceled during v_user_message_received: context canceled"
        );
        assert_eq!(logger.to_string(), "", "canceled pass logged");
        assert_eq!(runner.executed_names().len(), 1, "views run");

        // Live pass: a failure per view, but Canceled errors are not logged per view.
        let on_run: OnRun = Arc::new(|_ctx: Context, _name: String| {
            Box::pin(async { Err(Error::other(ContextError::Canceled)) })
        });
        let runner = FakeViewRunner::new(HashMap::new(), Some(on_run));
        let logger = RecordingLogger::default();
        let err = sync_analytics_views(&ctx, &runner, &defs, Some(&logger))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("failed to create view v_user_message_received: context canceled"),
            "{err}"
        );
        assert!(
            !logger.contains("failed to create view"),
            "Canceled error logged: {logger}"
        );
        assert_eq!(runner.executed_names().len(), defs.len(), "views run");
    }

    /// A caller's runner error whose `source()` is the bare `ContextError` (Go
    /// `fmt.Errorf("run view: %w", ctx.Err())`) takes Go's canceled paths too.
    #[tokio::test(start_paused = true)]
    async fn runner_error_wrapping_bare_context_error_takes_go_canceled_paths() {
        use std::collections::HashMap;

        use crate::bigquery::fake::{FakeViewRunner, OnRun, RecordingLogger};

        #[derive(Debug)]
        struct RunnerErr(ContextError);

        impl std::fmt::Display for RunnerErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "run view: {}", self.0)
            }
        }

        impl std::error::Error for RunnerErr {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let ctx = Context::background();
        let none = Default::default();
        let defs =
            view_definitions_with_denied("test-project", "test_dataset", "test-table", "v", &none)
                .unwrap();

        // Canceled pass: Go `errors.Is(err, ctx.Err())` holds, so the text is not doubled.
        let pass_ctx = ctx.with_cancel();
        let cancel_ctx = pass_ctx.clone();
        let on_run: OnRun = Arc::new(move |run_ctx: Context, _name: String| {
            cancel_ctx.cancel();
            Box::pin(async move {
                let cause = run_ctx.err().unwrap_or(ContextError::DeadlineExceeded);
                Err(Error::other(RunnerErr(cause)))
            })
        });
        let runner = FakeViewRunner::new(HashMap::new(), Some(on_run));
        let logger = RecordingLogger::default();
        let err = sync_analytics_views(&pass_ctx, &runner, &defs, Some(&logger))
            .await
            .unwrap_err();
        pass_ctx.cancel();
        assert!(err.is_context(ContextError::Canceled), "{err}");
        assert_eq!(
            err.to_string(),
            "view creation canceled during v_user_message_received: run view: context canceled"
        );
        assert_eq!(logger.to_string(), "", "canceled pass logged");
        assert_eq!(runner.executed_names().len(), 1, "views run");

        // Live pass: a failure per view, but Canceled errors are not logged per view.
        let on_run: OnRun = Arc::new(|_ctx: Context, _name: String| {
            Box::pin(async { Err(Error::other(RunnerErr(ContextError::Canceled))) })
        });
        let runner = FakeViewRunner::new(HashMap::new(), Some(on_run));
        let logger = RecordingLogger::default();
        let err = sync_analytics_views(&ctx, &runner, &defs, Some(&logger))
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with(
                "failed to create view v_user_message_received: run view: context canceled"
            ),
            "{err}"
        );
        assert!(
            !logger.contains("failed to create view"),
            "Canceled error logged: {logger}"
        );
        assert_eq!(runner.executed_names().len(), defs.len(), "views run");
    }

    #[test]
    fn runner_new_keeps_a_weak_client() {
        let runner = BigQueryViewRunner::new(Weak::new(), "p", "d");
        assert!(runner.client.upgrade().is_none());
        assert_eq!(runner.project_id, "p");
        assert_eq!(runner.dataset_id, "d");
        let _: Arc<dyn ViewRunner> = Arc::new(runner);
        assert_eq!(VIEW_COMMON_COLUMNS.len(), 13);
    }

    /// Every `viewDefinitionsWithDenied` case of the probe, through the crate function itself
    /// (`tests/golden_views.rs` reaches all but two of them through `Config`).
    #[test]
    fn golden_view_definitions_with_denied() {
        let mut checked = 0;
        for case in fixture_cases("views/view_definitions.json") {
            if case["fn"] != "viewDefinitionsWithDenied" {
                continue;
            }
            let name = case["name"].as_str().unwrap();
            let a = &case["args"];
            let got = view_definitions_with_denied(
                a["projectID"].as_str().unwrap(),
                a["datasetID"].as_str().unwrap(),
                a["tableName"].as_str().unwrap(),
                a["viewPrefix"].as_str().unwrap(),
                &json_set(&a["denied"]),
            );
            let want = &case["result"];
            match got {
                Err(e) => assert_eq!(
                    Some(e.to_string().as_str()),
                    want["error"].as_str(),
                    "{name}"
                ),
                Ok(views) => {
                    let want_views = want["views"]
                        .as_array()
                        .unwrap_or_else(|| panic!("{name}: want {want}"));
                    assert_eq!(views.len(), want_views.len(), "{name}");
                    for (g, w) in views.iter().zip(want_views) {
                        assert_eq!(g.name, w["name"].as_str().unwrap(), "{name}");
                        assert_eq!(g.event_type, w["event_type"].as_str().unwrap(), "{name}");
                        assert_eq!(g.query, w["query"].as_str().unwrap(), "{name}: {}", g.name);
                    }
                }
            }
            checked += 1;
        }
        assert_eq!(checked, 26);
    }

    #[test]
    fn golden_referenced_columns() {
        let cases = fixture_cases("views/referenced_columns.json");
        assert_eq!(cases.len(), 10);
        for case in cases {
            let expr = case["expr"].as_str().unwrap();
            let denied = json_set(&case["denied"]);
            assert_eq!(
                references_denied_column(expr, &denied),
                case["referencesDeniedColumn"].as_bool().unwrap(),
                "referencesDeniedColumn({expr:?})"
            );
            assert_eq!(
                strip_sql_string_literals(expr),
                case["stripSQLStringLiterals"].as_str().unwrap(),
                "stripSQLStringLiterals({expr:?})"
            );
        }
    }

    #[test]
    fn event_view_specs_cover_25_event_types_without_hitl_completions() {
        assert_eq!(EVENT_VIEW_SPECS.len(), 25);
        let types: BTreeSet<&str> = EVENT_VIEW_SPECS.iter().map(|s| s.event_type).collect();
        assert_eq!(types.len(), 25);
        for t in [
            crate::event_types::EVENT_HITL_CREDENTIAL_COMPLETED,
            crate::event_types::EVENT_HITL_CONFIRMATION_COMPLETED,
            crate::event_types::EVENT_HITL_INPUT_COMPLETED,
        ] {
            assert!(!types.contains(t), "{t}");
        }
    }

    #[test]
    fn project_view_columns_keeps_all_without_denylist() {
        let cols = EVENT_VIEW_SPECS[1].extra_cols;
        assert_eq!(project_view_columns(cols, &BTreeSet::new()), cols);
        assert!(project_view_columns(&[], &set(&["content"])).is_empty());
        assert_eq!(
            project_view_columns(cols, &set(&["content"])),
            [
                "JSON_VALUE(attributes, '$.model') AS model",
                "JSON_QUERY(attributes, '$.llm_config') AS llm_config",
                "JSON_QUERY(attributes, '$.tools') AS tools",
            ]
        );
    }

    #[test]
    fn strip_sql_string_literals_edge_cases() {
        // Unterminated literal: everything after the opening quote is dropped.
        assert_eq!(strip_sql_string_literals("a 'bc"), "a '");
        // A trailing backslash in a literal is not an escape (no next byte).
        assert_eq!(strip_sql_string_literals(r"'x\"), "'");
        // Non-ASCII inside and outside literals.
        assert_eq!(strip_sql_string_literals("é 'ü\\é' ö"), "é '' ö");
        assert_eq!(strip_sql_string_literals(""), "");
    }

    #[test]
    fn contains_identifier_token_boundaries() {
        assert!(contains_identifier_token("content", "content"));
        assert!(contains_identifier_token("a,content)", "content"));
        assert!(!contains_identifier_token("content_parts", "content"));
        assert!(!contains_identifier_token("xcontent", "content"));
        // A later match is found after a rejected one.
        assert!(contains_identifier_token(
            "content_parts, content",
            "content"
        ));
        // Non-ASCII neighbours are not identifier bytes; no slicing inside a UTF-8 sequence.
        assert!(contains_identifier_token("écontenté", "content"));
        assert!(!contains_identifier_token("ééé", "é\u{301}"));
        assert!(contains_identifier_token("aéb é", "é"));
        assert!(!contains_identifier_token("abc", ""));
        assert!(!contains_identifier_token("", "a"));
    }

    #[test]
    fn identifier_validators() {
        assert!(is_valid_project_id("my-proj.example:p_1"));
        assert!(!is_valid_project_id(""));
        assert!(!is_valid_project_id("p`"));
        assert!(is_valid_identifier("v-1_x"));
        assert!(!is_valid_identifier("v.x"));
        assert!(!is_valid_identifier(""));
        assert!(is_valid_dataset_id(
            &"d".repeat(MAX_BIGQUERY_IDENTIFIER_LEN)
        ));
        assert!(!is_valid_dataset_id(
            &"d".repeat(MAX_BIGQUERY_IDENTIFIER_LEN + 1)
        ));
        assert!(!is_valid_dataset_id("a-b"));
        assert!(is_valid_table_name("tablé_ü 1-x"));
        assert!(is_valid_table_name("表格٣")); // Lo letters, Nd Arabic-Indic digit
        assert!(!is_valid_table_name("t²")); // No (superscript two) is not Nd
        assert!(!is_valid_table_name("t\u{2163}")); // Nl (ROMAN NUMERAL FOUR) is not L
        assert!(!is_valid_table_name("t\u{301}")); // Mn combining mark
        assert!(!is_valid_table_name("\u{3000}")); // blank (ideographic space)
        assert!(!is_valid_table_name("a\tb"));
        assert!(!is_valid_table_name(
            &"t".repeat(MAX_BIGQUERY_IDENTIFIER_LEN + 1)
        ));
        // 342 three-byte runes = 1026 bytes > 1024 although only 342 runes.
        assert!(!is_valid_table_name(&"表".repeat(342)));
    }

    #[test]
    fn view_name_collision_uses_go_simple_case_folding() {
        assert!(ascii_equal_fold("v_tool_starting", "V_Tool_Starting"));
        // U+017F LONG S folds to s; U+212A KELVIN SIGN folds to k (Go strings.EqualFold).
        assert!(ascii_equal_fold("v_tool_starting", "v_tool_\u{17F}tarting"));
        assert!(ascii_equal_fold(
            "v_agent_state_checkpoint",
            "v_agent_state_chec\u{212A}point"
        ));
        assert!(!ascii_equal_fold("v_tool_starting", "v_tool_startin"));
        assert!(!ascii_equal_fold("v_tool_startin", "v_tool_starting"));
        assert!(!ascii_equal_fold("v_tool_starting", "v_tool_ştarting"));

        let err = view_definitions_with_denied(
            "p",
            "d",
            "v_agent_state_chec\u{212A}point",
            "",
            &BTreeSet::new(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "BigQuery view name \"v_agent_state_checkpoint\" collides with the events table \"v_agent_state_chec\u{212A}point\"; use another view prefix"
        );
    }

    #[test]
    fn view_definitions_applies_defaults_and_denylist() {
        let config = Config {
            project_id: "p".to_owned(),
            payload_column_denylist: vec!["latency_ms".to_owned()],
            ..Config::zero()
        };
        let defs = config.view_definitions().unwrap();
        assert_eq!(defs.len(), 25);
        assert_eq!(defs[0].name, "v_user_message_received");
        assert!(defs[3].query.ends_with(
            "  is_truncated\nFROM\n  `p.agent_analytics.agent_events`\nWHERE\n  event_type = 'LLM_ERROR'\n"
        ));
        assert!(defs.iter().all(|d| !d.query.contains("latency_ms")));
    }
}
