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

//! Test helpers for the compaction ports: Go internal/compactioninternal/helpers_test.go and
//! compactor_test.go's staticSession.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::compaction::{Config, SummarizeResult, Summarizer};
use gadk_session::{
    Event, EventActions, EventCompaction, EventRef, Session, SessionError, State, ToolConfirmation,
    zero_time,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::compactor::sliding_window;
use super::tail_retention::{TokenCounter, TurnScope, tail_retention};
use super::{BoxError, ProgressGate};

/// Go `epoch.Add(n * time.Second)`, epoch 2026-01-01T00:00:00Z.
pub(crate) fn at(n: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + chrono::Duration::seconds(n)
}

/// Go ids (an empty vector for Go's nil).
pub(crate) fn ids(events: &[Event]) -> Vec<String> {
    events.iter().map(|e| e.id.clone()).collect()
}

/// Expected-ID lists.
pub(crate) fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

/// Go newEvent: content (role = author) only when parts are given.
pub(crate) fn new_event(
    id: &str,
    invocation_id: &str,
    ts: i64,
    author: &str,
    parts: Vec<Part>,
) -> Event {
    Event {
        id: id.into(),
        invocation_id: invocation_id.into(),
        timestamp: at(ts),
        author: author.into(),
        response: LlmResponse {
            content: (!parts.is_empty()).then(|| Content {
                role: author.into(),
                parts,
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Go textEvent (author "user").
pub(crate) fn text_event(id: &str, invocation_id: &str, ts: i64, text: &str) -> Event {
    new_event(id, invocation_id, ts, "user", vec![Part::from_text(text)])
}

/// Go modelTextEvent (author "model").
pub(crate) fn model_text_event(id: &str, invocation_id: &str, ts: i64, text: &str) -> Event {
    new_event(id, invocation_id, ts, "model", vec![Part::from_text(text)])
}

fn call_part(call_id: &str) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: call_id.into(),
            name: format!("tool_{call_id}"),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go callEvent.
pub(crate) fn call_event(id: &str, invocation_id: &str, ts: i64, call_id: &str) -> Event {
    new_event(id, invocation_id, ts, "model", vec![call_part(call_id)])
}

/// Go multiCallEvent.
pub(crate) fn multi_call_event(id: &str, invocation_id: &str, ts: i64, call_ids: &[&str]) -> Event {
    new_event(
        id,
        invocation_id,
        ts,
        "model",
        call_ids.iter().map(|c| call_part(c)).collect(),
    )
}

/// Go responseEvent (response `{"result": "ok"}`).
pub(crate) fn response_event(id: &str, invocation_id: &str, ts: i64, call_id: &str) -> Event {
    let Value::Object(response) = json!({"result": "ok"}) else {
        unreachable!()
    };
    new_event(
        id,
        invocation_id,
        ts,
        "user",
        vec![Part {
            function_response: Some(FunctionResponse {
                id: call_id.into(),
                name: format!("tool_{call_id}"),
                response,
                ..Default::default()
            }),
            ..Default::default()
        }],
    )
}

/// Go callAndResponseEvent: the response part first, then the call.
pub(crate) fn call_and_response_event(
    id: &str,
    invocation_id: &str,
    ts: i64,
    call_id: &str,
) -> Event {
    new_event(
        id,
        invocation_id,
        ts,
        "model",
        vec![
            Part {
                function_response: Some(FunctionResponse {
                    id: call_id.into(),
                    name: format!("tool_{call_id}"),
                    response: Map::new(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            call_part(call_id),
        ],
    )
}

/// Go confirmationEvent: no content, one requested confirmation with hint "approve?".
pub(crate) fn confirmation_event(id: &str, invocation_id: &str, ts: i64, call_id: &str) -> Event {
    let mut ev = new_event(id, invocation_id, ts, "model", vec![]);
    ev.actions.requested_tool_confirmations.insert(
        call_id.into(),
        ToolConfirmation {
            hint: "approve?".into(),
            ..Default::default()
        },
    );
    ev
}

/// Go withAuthRequest: attaches a credential request for `call_id` to `ev` (a tool puts the
/// request on its own response for that call).
pub(crate) fn with_auth_request(mut ev: Event, call_id: &str) -> Event {
    ev.actions.requested_auth_configs = [(
        call_id.to_owned(),
        gadk_session::toolauth::Config::default(),
    )]
    .into();
    ev
}

/// Go compactionEvent: a stored record at `ts` covering `[start, end]` except `excluded`;
/// author "user", invocation "compaction-<id>", no content of its own.
pub(crate) fn compaction_event(
    id: &str,
    ts: i64,
    start: i64,
    end: i64,
    summary: &str,
    excluded: Vec<EventRef>,
) -> Event {
    Event {
        id: id.into(),
        invocation_id: format!("compaction-{id}"),
        timestamp: at(ts),
        author: "user".into(),
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: at(start),
                end_timestamp: at(end),
                compacted_content: Some(Content {
                    role: "model".into(),
                    parts: vec![Part::from_text(summary)],
                }),
                excluded_events: excluded,
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Go excl: the reference a test fixture excludes.
pub(crate) fn excl(invocation_id: &str, ts: i64) -> EventRef {
    EventRef {
        invocation_id: invocation_id.into(),
        timestamp: at(ts),
    }
}

/// Port of Go helpers_test.go slidingWindowStored: runs SlidingWindow and reports the summary as
/// stored, closing its span.
pub(crate) async fn sliding_window_stored(
    cfg: &Config,
    sess: Option<&dyn Session>,
) -> Result<Option<Event>, BoxError> {
    let (ev, finish) = sliding_window(&Platform::default(), cfg, sess, "").await?;
    finish.finish(None, "");
    Ok(ev)
}

/// Port of Go helpers_test.go tailRetentionStored: runs TailRetention and reports a produced
/// summary as stored, closing its span. (Go also calls `finish(err, "")` on the error path;
/// there Rust returns no `Finish` and the span is already closed.)
pub(crate) async fn tail_retention_stored(
    cfg: &Config,
    sess: Option<&dyn Session>,
    scope: &TurnScope,
    estimate: Option<&TokenCounter>,
    progress: Option<Arc<dyn ProgressGate>>,
) -> Result<Option<Event>, BoxError> {
    let (ev, finish) =
        tail_retention(&Platform::default(), cfg, sess, scope, estimate, progress).await?;
    finish.finish(None, "");
    Ok(ev)
}

/// A state with no keys (Go staticSession.State returns nil).
struct EmptyState;

impl State for EmptyState {
    fn get(&self, _key: &str) -> Result<Value, SessionError> {
        Err(SessionError::StateKeyNotExist)
    }
    fn set(&self, _key: &str, _value: Value) -> Result<(), SessionError> {
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
}

/// Go staticSession (compactor_test.go:38): id "sess", app "app", user "user", a fixed event list
/// that [`StaticSession::push`] can grow (Go mutableSession).
pub(crate) struct StaticSession {
    events: Mutex<Vec<Event>>,
    state: EmptyState,
}

impl StaticSession {
    pub(crate) fn new(events: Vec<Event>) -> Arc<StaticSession> {
        Arc::new(StaticSession {
            events: Mutex::new(events),
            state: EmptyState,
        })
    }

    /// Go mutableSession.append.
    pub(crate) fn push(&self, ev: Event) {
        self.events.lock().push(ev);
    }
}

impl Session for StaticSession {
    fn id(&self) -> &str {
        "sess"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> Vec<Event> {
        self.events.lock().clone()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        zero_time()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go fakeSummarizer: records each window's IDs; fails with `err`, declines on an empty summary
/// or window, else returns the summary as a role-less content.
#[allow(non_camel_case_types)]
pub(crate) struct fakeSummarizer {
    pub(crate) summary: String,
    pub(crate) err: Option<String>,
    windows: Mutex<Vec<Vec<String>>>,
}

impl fakeSummarizer {
    pub(crate) fn new(summary: &str) -> Arc<fakeSummarizer> {
        Arc::new(fakeSummarizer {
            summary: summary.into(),
            err: None,
            windows: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn failing(err: &str) -> Arc<fakeSummarizer> {
        Arc::new(fakeSummarizer {
            summary: String::new(),
            err: Some(err.into()),
            windows: Mutex::new(Vec::new()),
        })
    }

    /// The event IDs of every window passed in, in call order.
    pub(crate) fn windows(&self) -> Vec<Vec<String>> {
        self.windows.lock().clone()
    }
}

#[async_trait]
impl Summarizer for fakeSummarizer {
    async fn summarize_events(
        &self,
        events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        self.windows.lock().push(ids(events));
        if let Some(err) = &self.err {
            return Err(err.clone().into());
        }
        if self.summary.is_empty() || events.is_empty() {
            return Ok(SummarizeResult::default());
        }
        Ok(SummarizeResult {
            content: Some(Content {
                role: String::new(),
                parts: vec![Part::from_text(self.summary.clone())],
            }),
            usage: None,
        })
    }
}
