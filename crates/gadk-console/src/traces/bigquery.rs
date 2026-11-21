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

//! The BigQuery trace source: traces read from the agent-analytics table (`agent_events`).
//!
//! Rows that share a `trace_id` are one trace; rows that share a `span_id` are one span; every
//! row is also one log entry of its span. Rows are never joined to Cloud Logging.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use gadk_bigquery::{BigQueryClient, BigQueryError, QueryOptions};
use serde_json::{Map, Value, json};

use super::{
    LogEntry, LogSet, RawSpan, SourceError, TraceDetail, TraceId, TracePage, TraceQuery,
    TraceSource, TraceSummary,
};

use crate::prices::{
    LABEL_CACHED, LABEL_INPUT, LABEL_MODEL, LABEL_OPERATION, LABEL_OUTPUT, LABEL_REASONING,
    OP_GENERATE_CONTENT,
};

/// The URL form `--traces` accepts.
const URL_FORM: &str = "bigquery://<project>.<dataset>.<table>";

/// A BigQuery table: `project.dataset.table`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BigQueryTable {
    /// The project id, optionally domain-scoped (`example.com:proj`).
    pub project: String,
    /// The dataset id.
    pub dataset: String,
    /// The table id.
    pub table: String,
}

impl BigQueryTable {
    /// A table reference (not validated here; queries validate it).
    pub fn new(
        project: impl Into<String>,
        dataset: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            dataset: dataset.into(),
            table: table.into(),
        }
    }

    /// Parses `bigquery://<project>.<dataset>.<table>`, splitting from the right so a
    /// domain-scoped project (`example.com:proj`) keeps its dots.
    ///
    /// # Errors
    ///
    /// A message naming the expected form when the scheme, the parts or their characters are
    /// wrong.
    pub fn parse_url(s: &str) -> Result<Self, String> {
        let bad = || format!("expected {URL_FORM}, got {s:?}");
        let rest = s.strip_prefix("bigquery://").ok_or_else(bad)?;
        let mut parts = rest.rsplitn(3, '.');
        let (Some(table), Some(dataset), Some(project)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let t = Self::new(project, dataset, table);
        if t.is_valid() { Ok(t) } else { Err(bad()) }
    }

    /// Whether every part matches the allowed characters (see `is_project`, `is_dataset_or_table`).
    fn is_valid(&self) -> bool {
        is_project(&self.project)
            && is_dataset_or_table(&self.dataset)
            && is_dataset_or_table(&self.table)
    }

    /// The backquoted SQL path `` `project.dataset.table` ``.
    pub(crate) fn sql_path(&self) -> Result<String, SourceError> {
        if self.is_valid() {
            Ok(format!(
                "`{}.{}.{}`",
                self.project, self.dataset, self.table
            ))
        } else {
            Err(SourceError::BigQuery {
                status: None,
                reason: None,
                detail: format!(
                    "invalid table {}.{}.{}",
                    self.project, self.dataset, self.table
                ),
            })
        }
    }
}

/// `[a-z][a-z0-9-]{4,28}[a-z0-9]`, optionally prefixed by a domain `[a-z0-9.-]+:`.
fn is_project(s: &str) -> bool {
    let id = match s.rsplit_once(':') {
        Some((domain, id)) => {
            if domain.is_empty()
                || !domain
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            {
                return false;
            }
            id
        }
        None => s,
    };
    let b = id.as_bytes();
    (6..=30).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && (b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `[A-Za-z0-9_]{1,1024}`.
fn is_dataset_or_table(s: &str) -> bool {
    (1..=1024).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// How an event type joins a span family.
enum Match {
    /// The event type equals the string.
    Exact(&'static str),
    /// The event type starts with the string.
    Prefix(&'static str),
}

impl Match {
    fn matches(&self, event_type: &str) -> bool {
        match self {
            Self::Exact(s) => event_type == *s,
            Self::Prefix(s) => event_type.starts_with(s),
        }
    }

    /// The condition over `root.event_type` in the list query.
    fn sql(&self) -> String {
        match self {
            Self::Exact(s) => format!("root.event_type = '{s}'"),
            Self::Prefix(s) => format!("STARTS_WITH(root.event_type, '{s}')"),
        }
    }

    /// The bare string: one alternative of the list query's root-order regex. The regex is not
    /// anchored at the end, so there an exact match is a prefix match (as it always was).
    fn text(&self) -> &'static str {
        match self {
            Self::Exact(s) | Self::Prefix(s) => s,
        }
    }
}

/// Which column supplies a span name's suffix.
#[derive(Clone, Copy)]
enum Suffix {
    None,
    Agent,
    Model,
    Tool,
}

impl Suffix {
    /// The list query's column for the suffix.
    fn column(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Agent => Some("root.agent"),
            Self::Model => Some("root.model"),
            Self::Tool => Some("root.tool"),
        }
    }

    /// The suffix value among a row's agent, model and tool.
    fn pick<'a>(
        self,
        agent: Option<&'a str>,
        model: Option<&'a str>,
        tool: Option<&'a str>,
    ) -> Option<&'a str> {
        match self {
            Self::None => None,
            Self::Agent => agent,
            Self::Model => model,
            Self::Tool => tool,
        }
    }
}

/// The span families in match order: the event types each covers, its span-name base, and the
/// column that supplies the name's suffix. The single source of the naming rules behind
/// [`is_named_family`], [`span_name`], [`root_name_sql`] and [`root_order_regex`].
const FAMILIES: [(&[Match], &str, Suffix); 4] = [
    (
        &[
            Match::Exact("USER_MESSAGE_RECEIVED"),
            Match::Prefix("INVOCATION_"),
        ],
        "invocation",
        Suffix::None,
    ),
    (&[Match::Prefix("AGENT_")], "invoke_agent", Suffix::Agent),
    (&[Match::Prefix("LLM_")], "call_llm", Suffix::Model),
    (&[Match::Prefix("TOOL_")], "execute_tool", Suffix::Tool),
];

/// Whether an event type belongs to one of the span families in [`FAMILIES`].
pub(crate) fn is_named_family(event_type: &str) -> bool {
    FAMILIES
        .iter()
        .any(|(rules, ..)| rules.iter().any(|m| m.matches(event_type)))
}

/// The span name for a row: `invocation`, `invoke_agent <agent>`, `call_llm <model>`,
/// `execute_tool <tool>`, else the event type lowercased. A missing or empty suffix is dropped.
pub(crate) fn span_name(
    event_type: &str,
    agent: Option<&str>,
    model: Option<&str>,
    tool: Option<&str>,
) -> String {
    for (rules, base, suffix) in &FAMILIES {
        if rules.iter().any(|m| m.matches(event_type)) {
            return match suffix.pick(agent, model, tool).filter(|s| !s.is_empty()) {
                Some(s) => format!("{base} {s}"),
                None => (*base).to_owned(),
            };
        }
    }
    event_type.to_ascii_lowercase()
}

/// The root-name expression of the list query: [`span_name`] as SQL over the root row, one line.
fn root_name_sql() -> String {
    let mut sql = String::from("CASE ");
    for (rules, base, suffix) in &FAMILIES {
        let conditions: Vec<String> = rules.iter().map(Match::sql).collect();
        let name = match suffix.column() {
            None => format!("'{base}'"),
            Some(col) => {
                format!("IF(IFNULL({col}, '') = '', '{base}', CONCAT('{base} ', {col}))")
            }
        };
        sql.push_str(&format!("WHEN {} THEN {name} ", conditions.join(" OR ")));
    }
    sql.push_str("ELSE LOWER(root.event_type) END");
    sql
}

/// The list query's root-order regex: rows of a span family sort first when picking a trace's
/// root row.
fn root_order_regex() -> String {
    let mut alternatives = Vec::new();
    for (rules, ..) in &FAMILIES {
        alternatives.extend(rules.iter().map(Match::text));
    }
    format!("^({})", alternatives.join("|"))
}

/// A `gadk-bigquery` error as a trace-source error.
pub(crate) fn from_bigquery(e: BigQueryError) -> SourceError {
    match e {
        BigQueryError::Api {
            status,
            message,
            reason,
        } => SourceError::BigQuery {
            status: Some(status),
            reason,
            detail: message,
        },
        BigQueryError::Credentials(text) => SourceError::Credentials(text),
        BigQueryError::Http(text) | BigQueryError::Job(text) | BigQueryError::Decode(text) => {
            SourceError::BigQuery {
                status: None,
                reason: None,
                detail: text,
            }
        }
        other => SourceError::BigQuery {
            status: None,
            reason: None,
            detail: other.to_string(),
        },
    }
}

/// Traces listed per page.
pub(crate) const PAGE_SIZE: usize = 50;

/// Traces from the agent-analytics BigQuery table. Every query runs (and is billed) in the
/// compute project and stops at the byte cap.
#[non_exhaustive]
pub struct BigQueryTraceSource {
    client: BigQueryClient,
    compute_project: String,
    table: BigQueryTable,
    max_bytes_billed: i64,
    pub(crate) now: fn() -> DateTime<Utc>,
}

impl std::fmt::Debug for BigQueryTraceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigQueryTraceSource")
            .field("compute_project", &self.compute_project)
            .field("table", &self.table)
            .field("max_bytes_billed", &self.max_bytes_billed)
            .finish_non_exhaustive()
    }
}

impl BigQueryTraceSource {
    /// A source reading `table`, running queries in `compute_project` with Application Default
    /// Credentials (built on the first call), each capped at `max_bytes_billed` bytes.
    pub fn new(
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self::with_client(
            BigQueryClient::new(),
            compute_project,
            table,
            max_bytes_billed,
        )
    }

    /// [`new`](Self::new) with an explicit BigQuery client.
    pub fn with_client(
        client: BigQueryClient,
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self {
            client,
            compute_project: compute_project.into(),
            table,
            max_bytes_billed,
            now: Utc::now,
        }
    }

    /// The options of every query: the byte cap and `params`.
    pub(crate) fn options(&self, params: Vec<Value>) -> QueryOptions {
        QueryOptions::new()
            .with_maximum_bytes_billed(self.max_bytes_billed)
            .with_query_parameters(params)
    }

    /// Runs `sql` in the compute project.
    pub(crate) async fn run(
        &self,
        sql: &str,
        opts: &QueryOptions,
    ) -> Result<gadk_bigquery::QueryResult, SourceError> {
        self.client
            .query(&self.compute_project, sql, opts)
            .await
            .map_err(from_bigquery)
    }
}

/// A named query parameter; `None` is SQL NULL (`parameterValue` without `value`).
pub(crate) fn param(name: &str, ty: &str, value: Option<String>) -> Value {
    let value = match value {
        Some(v) => json!({ "value": v }),
        None => json!({}),
    };
    json!({ "name": name, "parameterType": { "type": ty }, "parameterValue": value })
}

/// A TIMESTAMP parameter value: `%Y-%m-%d %H:%M:%S%.6f+00:00`.
pub(crate) fn ts_param(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string()
}

/// A non-empty STRING cell.
pub(crate) fn text(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A TIMESTAMP cell (RFC 3339 text from `gadk-bigquery`).
pub(crate) fn timestamp(row: &Map<String, Value>, key: &str) -> Result<DateTime<Utc>, SourceError> {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| SourceError::BigQuery {
            status: None,
            reason: None,
            detail: format!("row without a valid {key}"),
        })
}

fn list_sql(table: &str) -> String {
    let root_order = root_order_regex();
    let root_name = root_name_sql();
    format!(
        "WITH t AS (\n\
  SELECT trace_id,\n\
    ARRAY_AGG(STRUCT(timestamp, event_type, agent, session_id,\n\
                     JSON_VALUE(attributes, '$.model') AS model,\n\
                     JSON_VALUE(content, '$.tool') AS tool)\n\
              ORDER BY IF(REGEXP_CONTAINS(event_type,\n\
                            r'{root_order}'), 0, 1),\n\
                       timestamp, event_type LIMIT 1)[OFFSET(0)] AS root,\n\
    MIN(timestamp) AS first_ts,\n\
    MAX(timestamp) AS last_ts\n\
  FROM {table}\n\
  WHERE timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)\n\
    AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)\n\
    AND trace_id IS NOT NULL AND trace_id != ''\n\
    AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{{32}}$')\n\
  GROUP BY trace_id),\n\
r AS (\n\
  SELECT trace_id, first_ts, last_ts, root.event_type AS event_type, root.agent AS agent,\n\
    root.session_id AS session_id, root.model AS model, root.tool AS tool,\n\
    {root_name} AS root_name\n\
  FROM t)\n\
SELECT trace_id, first_ts AS start, last_ts, event_type, agent, session_id, model, tool\n\
FROM r\n\
WHERE first_ts >= @start AND first_ts < @end\n\
  AND (@agent IS NULL OR agent = @agent)\n\
  AND (@session IS NULL OR session_id = @session)\n\
  AND (@name IS NULL OR STARTS_WITH(root_name, @name))\n\
ORDER BY start DESC, trace_id\n\
LIMIT @limit OFFSET @offset"
    )
}

/// The most rows (log entries) read for one trace.
pub(crate) const MAX_ROWS: usize = 1000;

fn get_sql(table: &str) -> String {
    format!(
        "SELECT timestamp, event_type, agent, session_id, invocation_id, span_id, parent_span_id,\n\
  content, attributes, latency_ms, status, error_message\n\
FROM {table} WHERE trace_id = @trace_id\n\
ORDER BY timestamp, event_type LIMIT @limit"
    )
}

/// One decoded `agent_events` row.
struct Row {
    ts: DateTime<Utc>,
    event_type: String,
    agent: Option<String>,
    session: Option<String>,
    invocation: Option<String>,
    span_id: Option<String>,
    parent: Option<String>,
    content: Option<Value>,
    attributes: Option<Value>,
    latency: Option<Value>,
    status: Option<String>,
    error: Option<String>,
}

/// A JSON cell: JSON text (or an already-decoded value); NULL and JSON `null` are absent.
fn json_cell(row: &Map<String, Value>, key: &str) -> Result<Option<Value>, SourceError> {
    let parsed = match row.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => {
            serde_json::from_str::<Value>(s).map_err(|e| SourceError::BigQuery {
                status: None,
                reason: None,
                detail: format!("{key} is not JSON: {e}"),
            })?
        }
        Some(other) => other.clone(),
    };
    Ok((!parsed.is_null()).then_some(parsed))
}

fn decode_row(row: &Map<String, Value>) -> Result<Row, SourceError> {
    Ok(Row {
        ts: timestamp(row, "timestamp")?,
        event_type: text(row, "event_type").unwrap_or_default(),
        agent: text(row, "agent"),
        session: text(row, "session_id"),
        invocation: text(row, "invocation_id"),
        span_id: text(row, "span_id").map(|s| s.to_ascii_lowercase()),
        parent: text(row, "parent_span_id").map(|s| s.to_ascii_lowercase()),
        content: json_cell(row, "content")?,
        attributes: json_cell(row, "attributes")?,
        latency: json_cell(row, "latency_ms")?,
        status: text(row, "status"),
        error: text(row, "error_message"),
    })
}

impl Row {
    fn model(&self) -> Option<&str> {
        self.attributes.as_ref()?.get("model")?.as_str()
    }

    fn model_version(&self) -> Option<&str> {
        self.attributes.as_ref()?.get("model_version")?.as_str()
    }

    fn tool(&self) -> Option<&str> {
        self.content.as_ref()?.get("tool")?.as_str()
    }

    fn severity(&self) -> &'static str {
        if self.status.as_deref() == Some("ERROR") || self.event_type.ends_with("_ERROR") {
            "ERROR"
        } else {
            "INFO"
        }
    }

    fn entry(&self) -> LogEntry {
        let mut payload = Map::new();
        payload.insert("event.name".into(), Value::from(self.event_type.clone()));
        if let Some(body) = &self.content {
            payload.insert("body".into(), body.clone());
        }
        let mut attributes = self.attributes.clone();
        if let Some(error) = &self.error {
            match attributes.get_or_insert_with(|| json!({})) {
                Value::Object(map) => {
                    map.insert("error_message".into(), Value::from(error.clone()));
                }
                other => *other = json!({ "value": other.clone(), "error_message": error }),
            }
        }
        if let Some(attributes) = attributes {
            payload.insert("attributes".into(), attributes);
        }
        LogEntry {
            timestamp: self.ts,
            span_id: self.span_id.clone(),
            severity: Some(self.severity().to_owned()),
            payload: Value::Object(payload),
        }
    }
}

/// Rows (oldest first) grouped into spans by `span_id`, in first-seen order.
fn spans(rows: &[Row]) -> Vec<RawSpan> {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: HashMap<&str, Vec<&Row>> = HashMap::new();
    for row in rows {
        if let Some(id) = row.span_id.as_deref() {
            groups
                .entry(id)
                .or_insert_with(|| {
                    order.push(id);
                    Vec::new()
                })
                .push(row);
        }
    }
    order
        .into_iter()
        .map(|id| {
            let group = &groups[id];
            let first_some =
                |f: fn(&Row) -> Option<&str>| group.iter().find_map(|r| f(r)).map(str::to_owned);
            let last_some = |f: fn(&Row) -> Option<&str>| {
                group.iter().rev().find_map(|r| f(r)).map(str::to_owned)
            };
            let namer = group
                .iter()
                .find(|r| is_named_family(&r.event_type))
                .unwrap_or(&group[0]);
            let agent = first_some(|r| r.agent.as_deref());
            let name = span_name(
                &namer.event_type,
                agent.as_deref(),
                first_some(Row::model).as_deref(),
                first_some(Row::tool).as_deref(),
            );
            let start = group.iter().map(|r| r.ts).min().unwrap_or(group[0].ts);
            let latest = group.iter().map(|r| r.ts).max().unwrap_or(start);
            let by_latency = group
                .iter()
                .filter_map(|r| r.latency.as_ref()?.get("total_ms")?.as_f64())
                .filter(|ms| ms.is_finite() && *ms >= 0.0)
                .filter_map(|ms| {
                    let micros = chrono::TimeDelta::microseconds((ms * 1000.0) as i64);
                    start.checked_add_signed(micros)
                })
                .max();
            let end = by_latency.map_or(latest, |t| t.max(latest));
            let mut labels = BTreeMap::new();
            for (key, value) in [
                ("gen_ai.agent.name", agent),
                (
                    "gen_ai.conversation.id",
                    first_some(|r| r.session.as_deref()),
                ),
                ("invocation_id", first_some(|r| r.invocation.as_deref())),
                ("status", last_some(|r| r.status.as_deref())),
                ("error_message", last_some(|r| r.error.as_deref())),
            ] {
                if let Some(value) = value {
                    labels.insert(key.to_owned(), value);
                }
            }
            if group.iter().any(|r| r.event_type.starts_with("LLM_")) {
                llm_labels(group, &mut labels);
            }
            RawSpan {
                span_id: id.to_owned(),
                parent_id: first_some(|r| r.parent.as_deref()),
                name,
                start,
                end,
                labels,
            }
        })
        .collect()
}

/// The model-call labels of an LLM span, named and counted as the model-call spans in Cloud
/// Trace carry them: the operation, the model (the first `attributes.model`, from the
/// `LLM_REQUEST` row, else the response's `attributes.model_version`) and, from the last
/// `LLM_RESPONSE` row, the token usage (`input_tokens` = prompt + tool-use prompt,
/// `output_tokens` = completion + thoughts, missing values 0).
fn llm_labels(group: &[&Row], labels: &mut BTreeMap<String, String>) {
    labels.insert(LABEL_OPERATION.to_owned(), OP_GENERATE_CONTENT.to_owned());
    let model = group
        .iter()
        .find_map(|r| r.model().filter(|m| !m.is_empty()))
        .or_else(|| {
            group
                .iter()
                .rev()
                .find_map(|r| r.model_version().filter(|m| !m.is_empty()))
        });
    if let Some(model) = model {
        labels.insert(LABEL_MODEL.to_owned(), model.to_owned());
    }
    let Some(res) = group.iter().rev().find(|r| r.event_type == "LLM_RESPONSE") else {
        return;
    };
    let content = res.content.as_ref();
    let attributes = res.attributes.as_ref();
    let thoughts = count(attributes, "/usage_metadata/thoughts_token_count");
    let input = count(content, "/usage/prompt")
        + count(attributes, "/usage_metadata/tool_use_prompt_token_count");
    let output = count(content, "/usage/completion") + thoughts;
    let cached = count(attributes, "/usage_metadata/cached_content_token_count");
    for (key, value) in [
        (LABEL_INPUT, input),
        (LABEL_OUTPUT, output),
        (LABEL_CACHED, cached),
        (LABEL_REASONING, thoughts),
    ] {
        labels.insert(key.to_owned(), value.to_string());
    }
}

/// A non-negative integer at `pointer` of a JSON cell; 0 when absent.
fn count(cell: Option<&Value>, pointer: &str) -> u64 {
    cell.and_then(|v| v.pointer(pointer))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[async_trait::async_trait]
impl TraceSource for BigQueryTraceSource {
    async fn list(&self, query: &TraceQuery) -> Result<TracePage, SourceError> {
        let offset = match query.page_token.as_deref() {
            None => 0,
            Some(t) => t.parse::<u64>().map_err(|_| SourceError::BigQuery {
                status: None,
                reason: None,
                detail: "bad page token".into(),
            })?,
        };
        let sql = list_sql(&self.table.sql_path()?);
        let end = (self.now)();
        let start = end - query.range.duration();
        let params = vec![
            param("start", "TIMESTAMP", Some(ts_param(start))),
            param("end", "TIMESTAMP", Some(ts_param(end))),
            param("name", "STRING", query.name.clone()),
            param("agent", "STRING", query.agent.clone()),
            param("session", "STRING", query.session.clone()),
            param("limit", "INT64", Some((PAGE_SIZE + 1).to_string())),
            param("offset", "INT64", Some(offset.to_string())),
        ];
        let result = self.run(&sql, &self.options(params)).await?;
        let more = result.rows.len() > PAGE_SIZE;
        let mut traces = Vec::new();
        for row in result.rows.iter().take(PAGE_SIZE) {
            let Some(trace_id) = row
                .get("trace_id")
                .and_then(Value::as_str)
                .and_then(TraceId::parse)
            else {
                continue;
            };
            let start = timestamp(row, "start")?;
            let last = timestamp(row, "last_ts")?;
            let event_type = text(row, "event_type").unwrap_or_default();
            let agent = text(row, "agent");
            traces.push(TraceSummary {
                trace_id,
                root_name: span_name(
                    &event_type,
                    agent.as_deref(),
                    text(row, "model").as_deref(),
                    text(row, "tool").as_deref(),
                ),
                start,
                duration_ms: (last - start).num_microseconds().unwrap_or(0) as f64 / 1000.0,
                agent,
                session: text(row, "session_id"),
            });
        }
        Ok(TracePage {
            traces,
            next: more.then(|| (offset + PAGE_SIZE as u64).to_string()),
        })
    }

    async fn get(&self, id: &TraceId) -> Result<TraceDetail, SourceError> {
        let sql = get_sql(&self.table.sql_path()?);
        let params = vec![
            param("trace_id", "STRING", Some(id.as_str().to_owned())),
            param("limit", "INT64", Some((MAX_ROWS + 1).to_string())),
        ];
        let opts = self.options(params).with_max_rows((MAX_ROWS + 1) as u32);
        let result = self.run(&sql, &opts).await?;
        if result.rows.is_empty() {
            return Err(SourceError::NotFound);
        }
        let truncated = result.rows.len() > MAX_ROWS || result.has_more;
        let rows = result
            .rows
            .iter()
            .take(MAX_ROWS)
            .map(decode_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TraceDetail {
            trace_id: id.clone(),
            spans: spans(&rows),
            logs: Ok(LogSet {
                entries: rows.iter().map(Row::entry).collect(),
                truncated,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, BigQueryError, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::prices::Usage;
    use crate::traces::test_http::{FakeHttp, param_value, reply};
    use crate::traces::{LogEntry, RawSpan, SourceError, TimeRange, TraceQuery, TraceSource};

    /// `list_sql("`p.d.t`")` as the list query was written before the span-family table.
    const LIST_SQL_BEFORE: &str = r#"WITH t AS (
SELECT trace_id,
ARRAY_AGG(STRUCT(timestamp, event_type, agent, session_id,
JSON_VALUE(attributes, '$.model') AS model,
JSON_VALUE(content, '$.tool') AS tool)
ORDER BY IF(REGEXP_CONTAINS(event_type,
r'^(USER_MESSAGE_RECEIVED|INVOCATION_|AGENT_|LLM_|TOOL_)'), 0, 1),
timestamp, event_type LIMIT 1)[OFFSET(0)] AS root,
MIN(timestamp) AS first_ts,
MAX(timestamp) AS last_ts
FROM `p.d.t`
WHERE timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)
AND trace_id IS NOT NULL AND trace_id != ''
AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{32}$')
GROUP BY trace_id),
r AS (
SELECT trace_id, first_ts, last_ts, root.event_type AS event_type, root.agent AS agent,
root.session_id AS session_id, root.model AS model, root.tool AS tool,
CASE WHEN root.event_type = 'USER_MESSAGE_RECEIVED' OR STARTS_WITH(root.event_type, 'INVOCATION_') THEN 'invocation' WHEN STARTS_WITH(root.event_type, 'AGENT_') THEN IF(IFNULL(root.agent, '') = '', 'invoke_agent', CONCAT('invoke_agent ', root.agent)) WHEN STARTS_WITH(root.event_type, 'LLM_') THEN IF(IFNULL(root.model, '') = '', 'call_llm', CONCAT('call_llm ', root.model)) WHEN STARTS_WITH(root.event_type, 'TOOL_') THEN IF(IFNULL(root.tool, '') = '', 'execute_tool', CONCAT('execute_tool ', root.tool)) ELSE LOWER(root.event_type) END AS root_name
FROM t)
SELECT trace_id, first_ts AS start, last_ts, event_type, agent, session_id, model, tool
FROM r
WHERE first_ts >= @start AND first_ts < @end
AND (@agent IS NULL OR agent = @agent)
AND (@session IS NULL OR session_id = @session)
AND (@name IS NULL OR STARTS_WITH(root_name, @name))
ORDER BY start DESC, trace_id
LIMIT @limit OFFSET @offset"#;

    /// The list query is byte-for-byte the one before the span-family table.
    #[test]
    fn the_list_sql_is_unchanged() {
        assert_eq!(list_sql("`p.d.t`"), LIST_SQL_BEFORE);
    }

    /// The SQL the span-family table generates is the hand-written `ROOT_NAME_SQL` and root-order
    /// regex the list query used before the table existed.
    #[test]
    fn the_generated_sql_matches_the_span_name_rules() {
        let want = "CASE \
  WHEN root.event_type = 'USER_MESSAGE_RECEIVED' OR STARTS_WITH(root.event_type, 'INVOCATION_') THEN 'invocation' \
  WHEN STARTS_WITH(root.event_type, 'AGENT_') THEN IF(IFNULL(root.agent, '') = '', 'invoke_agent', CONCAT('invoke_agent ', root.agent)) \
  WHEN STARTS_WITH(root.event_type, 'LLM_') THEN IF(IFNULL(root.model, '') = '', 'call_llm', CONCAT('call_llm ', root.model)) \
  WHEN STARTS_WITH(root.event_type, 'TOOL_') THEN IF(IFNULL(root.tool, '') = '', 'execute_tool', CONCAT('execute_tool ', root.tool)) \
  ELSE LOWER(root.event_type) END";
        assert_eq!(root_name_sql(), want);
        assert_eq!(
            root_order_regex(),
            "^(USER_MESSAGE_RECEIVED|INVOCATION_|AGENT_|LLM_|TOOL_)"
        );
    }

    const QUERIES: &str =
        "https://bigquery.googleapis.com/bigquery/v2/projects/billing-proj/queries";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap()
    }

    /// Microseconds since the epoch, as `useInt64Timestamp` sends TIMESTAMP cells.
    fn micros(t: DateTime<Utc>) -> String {
        t.timestamp_micros().to_string()
    }

    fn source(fake: &Arc<FakeHttp>) -> BigQueryTraceSource {
        let client = BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth));
        let mut s = BigQueryTraceSource::with_client(
            client,
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        );
        s.now = now;
        s
    }

    const LIST_FIELDS: [(&str, &str); 8] = [
        ("trace_id", "STRING"),
        ("start", "TIMESTAMP"),
        ("last_ts", "TIMESTAMP"),
        ("event_type", "STRING"),
        ("agent", "STRING"),
        ("session_id", "STRING"),
        ("model", "STRING"),
        ("tool", "STRING"),
    ];

    fn trace_id(i: usize) -> String {
        format!("{i:032x}")
    }

    fn list_row(i: usize, event: &str) -> Vec<Value> {
        let start = now() - chrono::TimeDelta::minutes(i as i64);
        vec![
            json!(trace_id(i)),
            json!(micros(start)),
            json!(micros(start + chrono::TimeDelta::milliseconds(1500))),
            json!(event),
            json!("weather_bot"),
            json!("s-1"),
            Value::Null,
            Value::Null,
        ]
    }

    #[tokio::test]
    async fn list_maps_rows_and_pages_by_offset() {
        let rows: Vec<Vec<Value>> = (1..=51)
            .map(|i| list_row(i, "USER_MESSAGE_RECEIVED"))
            .collect();
        let fake = FakeHttp::new(vec![reply(&LIST_FIELDS, rows, false)]);
        let page = source(&fake).list(&TraceQuery::default()).await.unwrap();
        assert_eq!(page.traces.len(), 50);
        assert_eq!(page.next.as_deref(), Some("50"));
        let first = &page.traces[0];
        assert_eq!(first.trace_id.as_str(), trace_id(1));
        assert_eq!(first.root_name, "invocation");
        assert_eq!(first.start, now() - chrono::TimeDelta::minutes(1));
        assert_eq!(first.duration_ms, 1500.0);
        assert_eq!(first.agent.as_deref(), Some("weather_bot"));
        assert_eq!(first.session.as_deref(), Some("s-1"));

        assert_eq!(fake.uris(), [QUERIES]);
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["parameterMode"], json!("NAMED"));
        let sql = body["query"].as_str().unwrap();
        assert!(
            sql.contains("FROM `data-proj.agent_analytics.agent_events`"),
            "{sql}"
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-20 11:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "end")["parameterValue"]["value"],
            json!("2026-11-20 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("51")
        );
        assert_eq!(
            param_value(&body, "offset")["parameterValue"]["value"],
            json!("0")
        );
        assert_eq!(param_value(&body, "agent")["parameterValue"], json!({}));
    }

    #[tokio::test]
    async fn list_last_page_has_no_next_and_skips_bad_ids() {
        let mut rows: Vec<Vec<Value>> = (1..=3).map(|i| list_row(i, "AGENT_STARTING")).collect();
        rows[1][0] = json!("not-a-trace-id");
        let fake = FakeHttp::new(vec![reply(&LIST_FIELDS, rows, false)]);
        let q = TraceQuery {
            page_token: Some("50".into()),
            ..TraceQuery::default()
        };
        let page = source(&fake).list(&q).await.unwrap();
        assert_eq!(page.next, None);
        assert_eq!(page.traces.len(), 2);
        assert_eq!(page.traces[0].root_name, "invoke_agent weather_bot");
        assert_eq!(
            param_value(&fake.body(0), "offset")["parameterValue"]["value"],
            json!("50")
        );
    }

    #[tokio::test]
    async fn list_filters_are_parameters_not_sql() {
        let fake = FakeHttp::new(vec![reply(&LIST_FIELDS, vec![], false)]);
        let q = TraceQuery {
            range: TimeRange::Days7,
            name: Some("invoke_agent x'; --".into()),
            agent: Some("bot\"name".into()),
            session: Some("sess`1".into()),
            page_token: None,
        };
        source(&fake).list(&q).await.unwrap();
        let body = fake.body(0);
        let sql = body["query"].as_str().unwrap();
        for value in ["invoke_agent x'; --", "bot\"name", "sess`1"] {
            assert!(!sql.contains(value), "{value} leaked into SQL: {sql}");
        }
        assert_eq!(
            param_value(&body, "name")["parameterValue"]["value"],
            json!("invoke_agent x'; --")
        );
        assert_eq!(
            param_value(&body, "agent")["parameterValue"]["value"],
            json!("bot\"name")
        );
        assert_eq!(
            param_value(&body, "session")["parameterValue"]["value"],
            json!("sess`1")
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-13 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn list_rejects_a_bad_page_token_without_querying() {
        let fake = FakeHttp::new(vec![]);
        for token in ["x", "-1", "1.5"] {
            let q = TraceQuery {
                page_token: Some(token.into()),
                ..TraceQuery::default()
            };
            let err = source(&fake).list(&q).await.unwrap_err();
            assert_eq!(
                err,
                SourceError::BigQuery {
                    status: None,
                    reason: None,
                    detail: "bad page token".into()
                }
            );
        }
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn list_maps_bigquery_failures() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let err = source(&fake)
            .list(&TraceQuery::default())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SourceError::BigQuery {
                status: Some(403),
                reason: Some("accessDenied".into()),
                detail: "Access Denied".into()
            }
        );
    }

    const GET_FIELDS: [(&str, &str); 12] = [
        ("timestamp", "TIMESTAMP"),
        ("event_type", "STRING"),
        ("agent", "STRING"),
        ("session_id", "STRING"),
        ("invocation_id", "STRING"),
        ("span_id", "STRING"),
        ("parent_span_id", "STRING"),
        ("content", "JSON"),
        ("attributes", "JSON"),
        ("latency_ms", "JSON"),
        ("status", "STRING"),
        ("error_message", "STRING"),
    ];

    const ID: &str = "0af7651916cd43dd8448eb211c80319c";
    const ROOT: &str = "aaaaaaaaaaaaaaaa";
    const AGENT: &str = "bbbbbbbbbbbbbbbb";
    const LLM: &str = "cccccccccccccccc";

    /// One `agent_events` row at `now + ms`.
    #[allow(clippy::too_many_arguments)]
    fn ev(
        ms: i64,
        event: &str,
        span: Option<&str>,
        parent: Option<&str>,
        content: Option<Value>,
        attributes: Option<Value>,
        latency: Option<Value>,
        status: &str,
        error: Option<&str>,
    ) -> Vec<Value> {
        let json_text = |v: Option<Value>| v.map_or(Value::Null, |v| json!(v.to_string()));
        vec![
            json!(micros(now() + chrono::TimeDelta::milliseconds(ms))),
            json!(event),
            json!("weather_bot"),
            json!("s-1"),
            json!("inv-1"),
            span.map_or(Value::Null, |s| json!(s)),
            parent.map_or(Value::Null, |s| json!(s)),
            json_text(content),
            json_text(attributes),
            json_text(latency),
            json!(status),
            error.map_or(Value::Null, |s| json!(s)),
        ]
    }

    fn trace_rows() -> Vec<Vec<Value>> {
        vec![
            ev(
                0,
                "USER_MESSAGE_RECEIVED",
                Some(ROOT),
                None,
                Some(json!({"text": "hi"})),
                None,
                None,
                "OK",
                None,
            ),
            ev(
                1,
                "INVOCATION_STARTING",
                Some(ROOT),
                None,
                None,
                None,
                None,
                "OK",
                None,
            ),
            ev(
                2,
                "AGENT_STARTING",
                Some(AGENT),
                Some(ROOT),
                None,
                None,
                None,
                "OK",
                None,
            ),
            ev(
                3,
                "LLM_REQUEST",
                Some(LLM),
                Some(AGENT),
                None,
                Some(json!({"model": "gemini-3.8-flash"})),
                None,
                "OK",
                None,
            ),
            ev(
                10,
                "LLM_RESPONSE",
                Some(LLM),
                Some(AGENT),
                Some(json!({"text": "hello"})),
                None,
                Some(json!({"total_ms": 900})),
                "OK",
                None,
            ),
            ev(
                20,
                "STATE_DELTA",
                None,
                None,
                Some(json!({"k": 1})),
                None,
                None,
                "OK",
                None,
            ),
            ev(
                30,
                "AGENT_ERROR",
                Some(AGENT),
                Some(ROOT),
                None,
                Some(Value::Null),
                None,
                "ERROR",
                Some("boom"),
            ),
            ev(
                40,
                "INVOCATION_COMPLETED",
                Some(ROOT),
                None,
                None,
                None,
                None,
                "OK",
                None,
            ),
        ]
    }

    fn at(ms: i64) -> DateTime<Utc> {
        now() + chrono::TimeDelta::milliseconds(ms)
    }

    #[tokio::test]
    async fn get_builds_spans_and_entries() {
        let fake = FakeHttp::new(vec![reply(&GET_FIELDS, trace_rows(), false)]);
        let id = TraceId::parse(ID).unwrap();
        let detail = source(&fake).get(&id).await.unwrap();
        assert_eq!(detail.trace_id, id);

        let names: Vec<(&str, &str, Option<&str>)> = detail
            .spans
            .iter()
            .map(|s| (s.span_id.as_str(), s.name.as_str(), s.parent_id.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                (ROOT, "invocation", None),
                (AGENT, "invoke_agent weather_bot", Some(ROOT)),
                (LLM, "call_llm gemini-3.8-flash", Some(AGENT)),
            ]
        );
        let root: &RawSpan = &detail.spans[0];
        assert_eq!((root.start, root.end), (at(0), at(40)));
        let llm = &detail.spans[2];
        assert_eq!(
            (llm.start, llm.end),
            (at(3), at(903)),
            "end from latency_ms.total_ms"
        );
        let agent = &detail.spans[1];
        assert_eq!(agent.labels["gen_ai.agent.name"], "weather_bot");
        assert_eq!(agent.labels["gen_ai.conversation.id"], "s-1");
        assert_eq!(agent.labels["invocation_id"], "inv-1");
        assert_eq!(agent.labels["status"], "ERROR");
        assert_eq!(agent.labels["error_message"], "boom");
        assert!(!root.labels.contains_key("error_message"));

        let logs = detail.logs.unwrap();
        assert!(!logs.truncated);
        assert_eq!(logs.entries.len(), 8);
        let first: &LogEntry = &logs.entries[0];
        assert_eq!(first.span_id.as_deref(), Some(ROOT));
        assert_eq!(first.severity.as_deref(), Some("INFO"));
        assert_eq!(
            first.payload,
            json!({ "event.name": "USER_MESSAGE_RECEIVED", "body": { "text": "hi" } })
        );
        let stray = &logs.entries[5];
        assert_eq!(stray.span_id, None, "rows without span_id are unattached");
        let error = &logs.entries[6];
        assert_eq!(error.severity.as_deref(), Some("ERROR"));
        assert_eq!(
            error.payload,
            json!({ "event.name": "AGENT_ERROR", "attributes": { "error_message": "boom" } })
        );

        let body = fake.body(0);
        assert_eq!(fake.uris(), [QUERIES]);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["maxResults"], json!(1001));
        assert_eq!(
            param_value(&body, "trace_id")["parameterValue"]["value"],
            json!(ID)
        );
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("1001")
        );
        assert!(!body["query"].as_str().unwrap().contains(ID));
    }

    #[tokio::test]
    async fn get_names_spans_from_the_first_named_row() {
        let rows = vec![
            ev(
                0,
                "STATE_DELTA",
                Some(ROOT),
                None,
                None,
                None,
                None,
                "OK",
                None,
            ),
            ev(
                1,
                "TOOL_STARTING",
                Some(ROOT),
                None,
                Some(json!({"tool": "get_weather"})),
                None,
                None,
                "OK",
                None,
            ),
        ];
        let fake = FakeHttp::new(vec![reply(&GET_FIELDS, rows, false)]);
        let detail = source(&fake)
            .get(&TraceId::parse(ID).unwrap())
            .await
            .unwrap();
        assert_eq!(detail.spans[0].name, "execute_tool get_weather");
    }

    #[tokio::test]
    async fn get_truncates_by_row_count_and_by_page_token() {
        let many: Vec<Vec<Value>> = (0..1001)
            .map(|i| {
                ev(
                    i,
                    "LLM_REQUEST",
                    Some(LLM),
                    None,
                    None,
                    None,
                    None,
                    "OK",
                    None,
                )
            })
            .collect();
        let fake = FakeHttp::new(vec![reply(&GET_FIELDS, many, false)]);
        let logs = source(&fake)
            .get(&TraceId::parse(ID).unwrap())
            .await
            .unwrap()
            .logs
            .unwrap();
        assert!(logs.truncated);
        assert_eq!(logs.entries.len(), 1000);

        let some: Vec<Vec<Value>> = (0..300)
            .map(|i| {
                ev(
                    i,
                    "LLM_REQUEST",
                    Some(LLM),
                    None,
                    None,
                    None,
                    None,
                    "OK",
                    None,
                )
            })
            .collect();
        let fake = FakeHttp::new(vec![reply(&GET_FIELDS, some, true)]);
        let logs = source(&fake)
            .get(&TraceId::parse(ID).unwrap())
            .await
            .unwrap()
            .logs
            .unwrap();
        assert!(logs.truncated, "a pageToken means more rows exist");
        assert_eq!(logs.entries.len(), 300);
    }

    #[tokio::test]
    async fn get_without_rows_is_not_found() {
        let fake = FakeHttp::new(vec![reply(&GET_FIELDS, vec![], false)]);
        let err = source(&fake)
            .get(&TraceId::parse(ID).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err, SourceError::NotFound);
    }

    #[tokio::test]
    async fn get_rejects_rows_without_timestamps_or_with_bad_json() {
        let mut no_ts = trace_rows();
        no_ts[0][0] = Value::Null;
        let mut bad_json = trace_rows();
        bad_json[0][7] = json!("{not json");
        for rows in [no_ts, bad_json] {
            let fake = FakeHttp::new(vec![reply(&GET_FIELDS, rows, false)]);
            let err = source(&fake)
                .get(&TraceId::parse(ID).unwrap())
                .await
                .unwrap_err();
            assert!(
                matches!(err, SourceError::BigQuery { status: None, .. }),
                "{err:?}"
            );
        }
    }

    #[tokio::test]
    async fn get_ignores_an_overflowing_latency() {
        for total_ms in [1e300, 1e18] {
            let rows = vec![ev(
                7,
                "LLM_RESPONSE",
                Some(LLM),
                None,
                None,
                None,
                Some(json!({ "total_ms": total_ms })),
                "OK",
                None,
            )];
            let fake = FakeHttp::new(vec![reply(&GET_FIELDS, rows, false)]);
            let detail = source(&fake)
                .get(&TraceId::parse(ID).unwrap())
                .await
                .unwrap();
            assert_eq!(detail.spans.len(), 1);
            let llm = &detail.spans[0];
            assert_eq!((llm.start, llm.end), (at(7), at(7)), "total_ms {total_ms}");
        }
    }

    #[test]
    fn parse_url_accepts_plain_and_domain_scoped_projects() {
        assert_eq!(
            BigQueryTable::parse_url("bigquery://sirhco-apps.agent_analytics.agent_events"),
            Ok(BigQueryTable::new(
                "sirhco-apps",
                "agent_analytics",
                "agent_events"
            ))
        );
        assert_eq!(
            BigQueryTable::parse_url("bigquery://example.com:proj-1.ds.tbl"),
            Ok(BigQueryTable::new("example.com:proj-1", "ds", "tbl"))
        );
    }

    #[test]
    fn parse_url_rejects_malformed_values() {
        for bad in [
            "",
            "sirhco-apps.ds.tbl",
            "bq://sirhco-apps.ds.tbl",
            "bigquery://sirhco-apps.ds",
            "bigquery://.ds.tbl",
            "bigquery://Sirhco-apps.ds.tbl",
            "bigquery://abc.ds.tbl",
            "bigquery://sirhco-apps.d-s.tbl",
            "bigquery://sirhco-apps.ds.t`bl",
            "bigquery://sirhco-apps.ds.",
            "bigquery://EXAMPLE.com:proj-1.ds.tbl",
        ] {
            let err = BigQueryTable::parse_url(bad).unwrap_err();
            assert!(
                err.contains("bigquery://<project>.<dataset>.<table>"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn sql_path_backquotes_valid_tables_and_rejects_others() {
        assert_eq!(
            BigQueryTable::new("example.com:proj-1", "ds", "tbl").sql_path(),
            Ok("`example.com:proj-1.ds.tbl`".to_owned())
        );
        let err = BigQueryTable::new("p", "ds", "t`; DROP")
            .sql_path()
            .unwrap_err();
        assert_eq!(
            err,
            SourceError::BigQuery {
                status: None,
                reason: None,
                detail: "invalid table p.ds.t`; DROP".into()
            }
        );
    }

    /// `USER_MESSAGE_RECEIVED` matches exactly; `INVOCATION_` matches as a prefix.
    #[test]
    fn exact_and_prefix_matches_differ() {
        assert_eq!(
            span_name("USER_MESSAGE_RECEIVED_X", None, None, None),
            "user_message_received_x"
        );
        assert!(!is_named_family("USER_MESSAGE_RECEIVED_X"));
        assert_eq!(span_name("INVOCATION_X", None, None, None), "invocation");
        assert!(is_named_family("INVOCATION_X"));
    }

    #[test]
    fn span_names_follow_the_event_family() {
        let cases = [
            ("USER_MESSAGE_RECEIVED", "invocation"),
            ("INVOCATION_STARTING", "invocation"),
            ("INVOCATION_COMPLETED", "invocation"),
            ("AGENT_STARTING", "invoke_agent weather_bot"),
            ("LLM_REQUEST", "call_llm gemini-3.8-flash"),
            ("TOOL_STARTING", "execute_tool get_weather"),
            ("STATE_DELTA", "state_delta"),
        ];
        for (event, want) in cases {
            assert_eq!(
                span_name(
                    event,
                    Some("weather_bot"),
                    Some("gemini-3.8-flash"),
                    Some("get_weather")
                ),
                want,
                "{event}"
            );
        }
        assert_eq!(
            span_name("AGENT_COMPLETED", None, None, None),
            "invoke_agent"
        );
        assert_eq!(span_name("LLM_RESPONSE", None, Some(""), None), "call_llm");
        assert_eq!(span_name("TOOL_ERROR", None, None, None), "execute_tool");
        assert!(is_named_family("USER_MESSAGE_RECEIVED"));
        assert!(is_named_family("TOOL_COMPLETED"));
        assert!(!is_named_family("STATE_DELTA"));
    }

    #[test]
    fn bigquery_errors_map_to_source_errors() {
        assert_eq!(
            from_bigquery(BigQueryError::Api {
                status: 403,
                message: "Access Denied".into(),
                reason: Some("accessDenied".into()),
            }),
            SourceError::BigQuery {
                status: Some(403),
                reason: Some("accessDenied".into()),
                detail: "Access Denied".into(),
            }
        );
        assert_eq!(
            from_bigquery(BigQueryError::Credentials("no ADC".into())),
            SourceError::Credentials("no ADC".into())
        );
        for e in [
            BigQueryError::Http("reset".into()),
            BigQueryError::Job(
                "Query exceeded limit for bytes billed: 10. 20 or higher required.".into(),
            ),
            BigQueryError::Decode("no rows".into()),
        ] {
            let text = match &e {
                BigQueryError::Http(t) | BigQueryError::Job(t) | BigQueryError::Decode(t) => {
                    t.clone()
                }
                _ => unreachable!(),
            };
            assert_eq!(
                from_bigquery(e),
                SourceError::BigQuery {
                    status: None,
                    reason: None,
                    detail: text
                }
            );
        }
    }

    /// R10: LLM spans carry the Cloud Trace model-call labels, computed from the rows `get`
    /// already reads; other spans do not.
    #[tokio::test]
    async fn get_puts_model_call_labels_on_llm_spans() {
        const TOOL: &str = "dddddddddddddddd";
        const BARE: &str = "eeeeeeeeeeeeeeee";
        let response_attributes = json!({
            "model_version": "gemini-3.8-flash-001",
            "usage_metadata": {
                "prompt_token_count": 1000,
                "candidates_token_count": 300,
                "total_token_count": 1550,
                "thoughts_token_count": 50,
                "cached_content_token_count": 200,
                "tool_use_prompt_token_count": 200
            }
        });
        let rows = vec![
            ev(
                0,
                "LLM_REQUEST",
                Some(LLM),
                Some(AGENT),
                None,
                Some(json!({"model": "gemini-3.8-flash"})),
                None,
                "OK",
                None,
            ),
            ev(
                5,
                "LLM_RESPONSE",
                Some(LLM),
                Some(AGENT),
                Some(
                    json!({"response": "hi", "usage": {"prompt": 1000, "completion": 300, "total": 1550}}),
                ),
                Some(response_attributes),
                Some(json!({"total_ms": 900})),
                "OK",
                None,
            ),
            ev(
                6,
                "LLM_RESPONSE",
                Some(BARE),
                Some(AGENT),
                None,
                Some(json!({"model_version": "gemini-3.8-pro-002"})),
                None,
                "OK",
                None,
            ),
            ev(
                7,
                "TOOL_STARTING",
                Some(TOOL),
                Some(AGENT),
                Some(json!({"tool": "get_weather"})),
                None,
                None,
                "OK",
                None,
            ),
        ];
        let fake = FakeHttp::new(vec![reply(&GET_FIELDS, rows, false)]);
        let detail = source(&fake)
            .get(&TraceId::parse(ID).unwrap())
            .await
            .unwrap();
        let labels = |id: &str| {
            detail
                .spans
                .iter()
                .find(|s| s.span_id == id)
                .unwrap_or_else(|| panic!("span {id}"))
                .labels
                .clone()
        };
        let llm = labels(LLM);
        for (key, want) in [
            ("gen_ai.operation.name", "generate_content"),
            ("gen_ai.request.model", "gemini-3.8-flash"),
            ("gen_ai.usage.input_tokens", "1200"),
            ("gen_ai.usage.output_tokens", "350"),
            ("gen_ai.usage.cache_read.input_tokens", "200"),
            ("gen_ai.usage.reasoning.output_tokens", "50"),
        ] {
            assert_eq!(llm.get(key).map(String::as_str), Some(want), "{key}");
        }
        assert_eq!(
            Usage::from_labels(&llm),
            Usage {
                input: 1200,
                cached: 200,
                output: 300,
                reasoning: 50
            },
            "the BigQuery row mapping of R6"
        );
        let bare = labels(BARE);
        assert_eq!(bare["gen_ai.request.model"], "gemini-3.8-pro-002");
        assert_eq!(bare["gen_ai.usage.input_tokens"], "0");
        assert_eq!(bare["gen_ai.operation.name"], "generate_content");
        let tool = labels(TOOL);
        assert!(!tool.contains_key("gen_ai.operation.name"), "{tool:?}");
        assert!(!tool.contains_key("gen_ai.usage.input_tokens"), "{tool:?}");
    }
}
