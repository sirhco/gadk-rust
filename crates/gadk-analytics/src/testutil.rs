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

//! Test doubles for code that runs the queries (feature `test-util`): [`FakeHttp`], a scripted
//! HTTP client to build a `gadk_bigquery::BigQueryClient` over, and the `jobs.query` reply
//! builders.
//!
//! ```
//! use std::sync::Arc;
//!
//! use chrono::{TimeZone, Utc};
//! use gadk_analytics::testutil::{FakeHttp, reply};
//! use gadk_analytics::{AnalyticsSource, BigQueryTable};
//! use gadk_bigquery::{BigQueryClient, NoAuth};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! // One scripted `jobs.query` reply: a single breakdown row.
//! let http = FakeHttp::new(vec![reply(
//!     &[
//!         ("model", "STRING"), ("agent", "STRING"), ("prompt", "STRING"), ("calls", "INT64"),
//!         ("input", "INT64"), ("cached", "INT64"), ("output", "INT64"), ("reasoning", "INT64"),
//!     ],
//!     vec![vec![
//!         "gemini-3.8-flash".into(), "triage".into(), "triage@2".into(), "3".into(),
//!         "3000".into(), "1000".into(), "600".into(), "0".into(),
//!     ]],
//!     false,
//! )]);
//! let source = AnalyticsSource::new(
//!     BigQueryClient::with_client(http.clone(), Arc::new(NoAuth)),
//!     "billing-proj",
//!     BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
//!     1_073_741_824,
//! );
//! let end = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
//! let breakdown = source.breakdown(end - chrono::TimeDelta::hours(24), end).await?;
//! assert_eq!(breakdown.cells.len(), 1);
//! // The fake records each request: here the one `jobs.query` call, run in the billing project.
//! assert_eq!(
//!     http.uris(),
//!     ["https://bigquery.googleapis.com/bigquery/v2/projects/billing-proj/queries"]
//! );
//! # Ok::<(), gadk_analytics::SourceError>(())
//! # }).unwrap();
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use gadk_model::{BodyStream, HttpClient, ModelError};
use http::{HeaderMap, Method, StatusCode};
use serde_json::{Value, json};

/// One request the fake client saw.
pub struct Seen {
    /// The request method.
    pub method: Method,
    /// The request URI.
    pub uri: String,
    /// The request headers.
    pub headers: HeaderMap,
    /// The request body.
    pub body: Bytes,
}

/// Records requests; answers each with the next scripted reply
/// (`Ok((status, body))`, or `Err(detail)` for a transport failure).
#[derive(Default)]
pub struct FakeHttp {
    replies: Mutex<VecDeque<Result<(u16, String), String>>>,
    /// Every request seen, in order.
    pub seen: Mutex<Vec<Seen>>,
}

impl FakeHttp {
    /// A client that answers with `replies`, in order; a request past the last reply panics.
    pub fn new(replies: Vec<Result<(u16, String), String>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::default(),
        })
    }

    /// The URIs of the requests seen, in order.
    pub fn uris(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.uri.clone())
            .collect()
    }

    /// The JSON body of the `i`th request seen.
    pub fn body(&self, i: usize) -> Value {
        serde_json::from_slice(&self.seen.lock().unwrap()[i].body).unwrap()
    }
}

// `HttpClient` is an `async_trait` trait; this is the method that attribute generates, written
// out so the crate does not depend on `async-trait`.
impl HttpClient for FakeHttp {
    fn send<'life0, 'async_trait>(
        &'life0 self,
        req: http::Request<Bytes>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<http::Response<BodyStream>, ModelError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            self.seen.lock().unwrap().push(Seen {
                method: parts.method,
                uri: parts.uri.to_string(),
                headers: parts.headers,
                body,
            });
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unscripted request");
            match reply {
                Ok((status, text)) => {
                    let stream: BodyStream =
                        futures::stream::once(async move { Ok(Bytes::from(text)) }).boxed();
                    let mut resp = http::Response::new(stream);
                    *resp.status_mut() = StatusCode::from_u16(status).unwrap();
                    Ok(resp)
                }
                Err(detail) => Err(ModelError::Transport(detail.into())),
            }
        })
    }
}

/// A completed `jobs.query` reply with `fields` (name, type) and `rows` (one JSON per cell);
/// `page_token` adds a `pageToken` (more rows exist).
pub fn reply(
    fields: &[(&str, &str)],
    rows: Vec<Vec<Value>>,
    page_token: bool,
) -> Result<(u16, String), String> {
    let schema: Vec<Value> = fields
        .iter()
        .map(|(n, t)| json!({ "name": n, "type": t, "mode": "NULLABLE" }))
        .collect();
    schema_reply(schema, rows, page_token)
}

/// [`reply`] with whole `jobs.query` schema fields, so a field can be `REPEATED` or a `RECORD`
/// with its own `fields` (see [`record`] and [`repeated`] for such cells).
pub fn schema_reply(
    schema: Vec<Value>,
    rows: Vec<Vec<Value>>,
    page_token: bool,
) -> Result<(u16, String), String> {
    let rows: Vec<Value> = rows.into_iter().map(record).collect();
    let mut body = json!({
        "jobComplete": true,
        "jobReference": { "projectId": "billing-proj", "jobId": "job_1" },
        "schema": { "fields": schema },
        "rows": rows,
    });
    if page_token {
        body["pageToken"] = json!("next");
    }
    Ok((200, body.to_string()))
}

/// A `RECORD` value (or a whole row) as `jobs.query` sends it: `{"f": [{"v": cell}, …]}`.
pub fn record(cells: Vec<Value>) -> Value {
    json!({ "f": cells.into_iter().map(|v| json!({ "v": v })).collect::<Vec<_>>() })
}

/// A `REPEATED` cell as `jobs.query` sends it: `[{"v": item}, …]`.
pub fn repeated(items: Vec<Value>) -> Value {
    Value::Array(items.into_iter().map(|v| json!({ "v": v })).collect())
}

/// The query parameter `name` of a `jobs.query` request body.
pub fn param_value(body: &Value, name: &str) -> Value {
    body["queryParameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == json!(name))
        .unwrap_or_else(|| panic!("no parameter {name}: {body}"))
        .clone()
}

/// The columns of the trend query ([`AnalyticsSource::trend`](crate::AnalyticsSource::trend)),
/// for [`reply`].
pub const TREND_FIELDS: [(&str, &str); 9] = [
    ("bucket", "INT64"),
    ("model", "STRING"),
    ("agent", "STRING"),
    ("prompt", "STRING"),
    ("calls", "INT64"),
    ("input", "INT64"),
    ("cached", "INT64"),
    ("output", "INT64"),
    ("reasoning", "INT64"),
];

/// The columns of the sessions query ([`AnalyticsSource::sessions`](crate::AnalyticsSource::sessions)).
pub const SESSION_FIELDS: [(&str, &str); 14] = [
    ("session_id", "STRING"),
    ("user_id", "STRING"),
    ("agent", "STRING"),
    ("first", "TIMESTAMP"),
    ("last", "TIMESTAMP"),
    ("turns", "INT64"),
    ("errors", "INT64"),
    ("ups", "INT64"),
    ("downs", "INT64"),
    ("model", "STRING"),
    ("input", "INT64"),
    ("cached", "INT64"),
    ("output", "INT64"),
    ("reasoning", "INT64"),
];

/// The columns of the users query ([`AnalyticsSource::users`](crate::AnalyticsSource::users)).
pub const USER_FIELDS: [(&str, &str); 12] = [
    ("user_id", "STRING"),
    ("sessions", "INT64"),
    ("turns", "INT64"),
    ("errors", "INT64"),
    ("ups", "INT64"),
    ("downs", "INT64"),
    ("last", "TIMESTAMP"),
    ("model", "STRING"),
    ("input", "INT64"),
    ("cached", "INT64"),
    ("output", "INT64"),
    ("reasoning", "INT64"),
];
/// The model cells of a test row: `(model, [input, cached, output, reasoning])`, or `None` for
/// a row without model calls (NULL cells).
pub type ModelCells<'a> = Option<(&'a str, [u64; 4])>;

/// A TIMESTAMP cell as `jobs.query` sends it (microseconds since the epoch).
fn ts_cell(t: DateTime<Utc>) -> Value {
    Value::from(t.timestamp_micros().to_string())
}

/// The model, input, cached, output and reasoning cells of `model`.
fn model_cells(model: ModelCells<'_>) -> Vec<Value> {
    match model {
        Some((name, counts)) => std::iter::once(Value::from(name))
            .chain(counts.iter().map(|n| Value::from(n.to_string())))
            .collect(),
        None => vec![Value::Null; 5],
    }
}

/// The cells of one row of the sessions query
/// ([`AnalyticsSource::sessions`](crate::AnalyticsSource::sessions)); `counts` is
/// `[turns, errors, ups, downs]`.
pub fn session_cells(
    id: &str,
    user: &str,
    agent: &str,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
    counts: [u64; 4],
    model: ModelCells<'_>,
) -> Vec<Value> {
    let mut cells = vec![
        Value::from(id),
        Value::from(user),
        Value::from(agent),
        ts_cell(first),
        ts_cell(last),
    ];
    cells.extend(counts.iter().map(|n| Value::from(n.to_string())));
    cells.extend(model_cells(model));
    cells
}

/// The cells of one row of the users query
/// ([`AnalyticsSource::users`](crate::AnalyticsSource::users)), `None` being a NULL user;
/// `counts` is `[sessions, turns, errors, ups, downs]`.
pub fn user_cells(
    user: Option<&str>,
    counts: [u64; 5],
    last: DateTime<Utc>,
    model: ModelCells<'_>,
) -> Vec<Value> {
    let mut cells = vec![user.map_or(Value::Null, Value::from)];
    cells.extend(counts.iter().map(|n| Value::from(n.to_string())));
    cells.push(ts_cell(last));
    cells.extend(model_cells(model));
    cells
}

/// A reply with the schema of the turns query
/// ([`AnalyticsSource::session_turns`](crate::AnalyticsSource::session_turns)) and `rows`: `tools`
/// is `REPEATED`, and `tokens` and `scores` are `REPEATED RECORD`s.
pub fn turn_reply(rows: Vec<Vec<Value>>) -> Result<(u16, String), String> {
    use serde_json::json;
    let field = |name: &str, ty: &str| json!({ "name": name, "type": ty, "mode": "NULLABLE" });
    let schema = vec![
        field("invocation_id", "STRING"),
        field("trace_id", "STRING"),
        field("start", "TIMESTAMP"),
        field("completed", "TIMESTAMP"),
        field("errors", "INT64"),
        json!({ "name": "tools", "type": "STRING", "mode": "REPEATED" }),
        json!({ "name": "tokens", "type": "RECORD", "mode": "REPEATED", "fields": [
            field("model", "STRING"),
            field("input", "INT64"),
            field("cached", "INT64"),
            field("output", "INT64"),
            field("reasoning", "INT64"),
        ] }),
        json!({ "name": "scores", "type": "RECORD", "mode": "REPEATED", "fields": [
            field("name", "STRING"),
            field("source", "STRING"),
            field("value", "STRING"),
        ] }),
    ];
    schema_reply(schema, rows, false)
}

/// The cells of one row of the turns query
/// ([`AnalyticsSource::session_turns`](crate::AnalyticsSource::session_turns)).
#[derive(Debug, Clone)]
pub struct TurnCells<'a> {
    /// The invocation id.
    pub invocation_id: &'a str,
    /// The trace id (`None` is NULL).
    pub trace_id: Option<&'a str>,
    /// The turn's first activity row.
    pub start: DateTime<Utc>,
    /// The `INVOCATION_COMPLETED` time (`None` is NULL).
    pub end: Option<DateTime<Utc>>,
    /// The error count.
    pub errors: u64,
    /// The `TOOL_STARTING` tool names, in call order.
    pub tools: &'a [&'a str],
    /// `(model, [input, cached, output, reasoning])` per model.
    pub tokens: &'a [(&'a str, [u64; 4])],
    /// `(name, source, value)` per latest score.
    pub scores: &'a [(&'a str, &'a str, &'a str)],
}

impl<'a> TurnCells<'a> {
    /// Invocation `invocation_id` starting at `start`, with no trace, no completion, no errors,
    /// no tools, no tokens and no scores.
    pub fn new(invocation_id: &'a str, start: DateTime<Utc>) -> Self {
        Self {
            invocation_id,
            trace_id: None,
            start,
            end: None,
            errors: 0,
            tools: &[],
            tokens: &[],
            scores: &[],
        }
    }

    /// The row's cells.
    pub fn cells(&self) -> Vec<Value> {
        vec![
            Value::from(self.invocation_id),
            self.trace_id.map_or(Value::Null, Value::from),
            ts_cell(self.start),
            self.end.map_or(Value::Null, ts_cell),
            Value::from(self.errors.to_string()),
            repeated(self.tools.iter().map(|t| Value::from(*t)).collect()),
            repeated(
                self.tokens
                    .iter()
                    .map(|(model, counts)| record(model_cells(Some((model, *counts)))))
                    .collect(),
            ),
            repeated(
                self.scores
                    .iter()
                    .map(|(name, source, value)| {
                        record(vec![
                            Value::from(*name),
                            Value::from(*source),
                            Value::from(*value),
                        ])
                    })
                    .collect(),
            ),
        ]
    }
}

/// The columns of the pending query (`prompts` is `ARRAY<STRING>`).
const PENDING_FIELDS: [(&str, &str); 4] = [
    ("trace_id", "STRING"),
    ("first", "TIMESTAMP"),
    ("prompts", "STRING"),
    ("thumbs_down", "INT64"),
];

/// A `jobs.query` reply of pending-query rows
/// ([`AnalyticsSource::pending`](crate::AnalyticsSource::pending); see [`pending_cells`]), with
/// `prompts` `REPEATED`.
pub fn pending_reply(rows: Vec<Vec<Value>>) -> Result<(u16, String), String> {
    let (status, body) = reply(&PENDING_FIELDS, rows, false)?;
    let mut body: Value = serde_json::from_str(&body).unwrap();
    body["schema"]["fields"][2]["mode"] = Value::from("REPEATED");
    Ok((status, body.to_string()))
}

/// The cells of one pending row; no `prompts` is a NULL array.
pub fn pending_cells(
    trace_id: &str,
    first: DateTime<Utc>,
    prompts: &[&str],
    thumbs_down: u64,
) -> Vec<Value> {
    let prompts = if prompts.is_empty() {
        Value::Null
    } else {
        Value::Array(prompts.iter().map(|p| json!({ "v": p })).collect())
    };
    vec![
        Value::from(trace_id),
        Value::from(first.timestamp_micros().to_string()),
        prompts,
        Value::from(thumbs_down.to_string()),
    ]
}
