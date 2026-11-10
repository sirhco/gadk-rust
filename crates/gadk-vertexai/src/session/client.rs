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

//! `SessionClient`: Go's `vertexAiClient` session methods (create, get, list, delete, append,
//! list events), engine-ID resolution, and the name parsers and filter quoting they use.

use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_platform::go_quote;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, SessionError, zero_time,
};
use http::Method;
use regex::Regex;
use serde_json::{Map, Value};

use super::convert::{
    event_id_from_name, event_to_session_event, filter_nil_values, session_event_to_event,
    struct_as_map, to_struct,
};
use super::local::VertexSession;
use crate::client::VertexClient;
use crate::config::AgentEngineData;
use crate::error::{BoxError, VertexError, find_vertex_error, session_error, wrap};
use crate::protojson::parse_timestamp;

/// Go `waitForOperation`'s attempts (`maxRetries`)…
const WAIT_ATTEMPTS: u32 = 10;
/// …its delay unit (`baseDelay`, multiplied by the attempt number squared)…
const WAIT_BASE_DELAY: Duration = Duration::from_secs(1);
/// …and its delay cap (`maxDelay`).
const WAIT_MAX_DELAY: Duration = Duration::from_secs(5);

/// Go `reasoningEnginePattern`. `[0-9]`, not `\d`: Go's `\d` is ASCII only.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static REASONING_ENGINE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^projects/(?:[a-zA-Z0-9_-]+)/locations/(?:[a-zA-Z0-9_-]+)/reasoningEngines/([0-9]+)$",
    )
    .expect("valid reasoning-engine regex")
});

/// Go `vertexAiClient`.
pub(crate) struct SessionClient {
    client: VertexClient,
    project_id: String,
    location: String,
    /// The configured engine ID; `""` resolves it from each request's app name.
    reasoning_engine: String,
}

impl SessionClient {
    pub(crate) fn new(
        client: VertexClient,
        project_id: String,
        location: String,
        reasoning_engine: String,
    ) -> Self {
        Self {
            client,
            project_id,
            location,
            reasoning_engine,
        }
    }

    /// Go `getReasoningEngineID`: the configured engine; else an app name Go's `strconv.Atoi`
    /// accepts (optional sign, ASCII digits, fits 64 bits), verbatim; else the ID a full
    /// `projects/…/locations/…/reasoningEngines/{id}` app name names.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidRequest`] with Go's text `app name "{app}" is not valid. It
    /// should be the full ReasoningEngine resource name or the reasoning engine numeric ID`.
    pub(crate) fn reasoning_engine_id(&self, app_name: &str) -> Result<String, SessionError> {
        if !self.reasoning_engine.is_empty() {
            return Ok(self.reasoning_engine.clone());
        }
        if app_name.parse::<i64>().is_ok() {
            return Ok(app_name.to_owned());
        }
        match REASONING_ENGINE_PATTERN.captures(app_name) {
            Some(caps) => Ok(caps[1].to_owned()),
            None => Err(SessionError::InvalidRequest(format!(
                "app name {} is not valid. It should be the full ReasoningEngine resource name or \
                 the reasoning engine numeric ID",
                go_quote(app_name)
            ))),
        }
    }

    /// The engine data of an engine ID (Go builds a `vertexaiutil.AgentEngineData` per call).
    fn engine(&self, reasoning_engine: String) -> AgentEngineData {
        AgentEngineData {
            project_id: self.project_id.clone(),
            location: self.location.clone(),
            reasoning_engine,
        }
    }

    /// Go `sessionNameByID`.
    fn session_name(&self, app_name: &str, session_id: &str) -> Result<String, SessionError> {
        let engine = self.engine(self.reasoning_engine_id(app_name)?);
        Ok(engine.session_resource(session_id))
    }

    /// Go `createSession`: `CreateSession`, the session ID from the returned operation's name,
    /// then [`wait_for_session`](Self::wait_for_session).
    pub(crate) async fn create_session(
        &self,
        req: &CreateRequest,
    ) -> Result<VertexSession, SessionError> {
        let mut session = Map::new();
        if !req.user_id.is_empty() {
            session.insert("userId".into(), Value::String(req.user_id.clone()));
        }
        if let Some(state) = req.state.as_ref().filter(|s| !s.is_empty()) {
            let state = to_struct(&Value::Object(state.clone()))
                .map_err(|e| session_error(wrap("failed to convert state to structpb", e)))?;
            session.insert("sessionState".into(), state);
        }

        let parent = self
            .engine(self.reasoning_engine_id(&req.app_name)?)
            .engine_resource();
        let mut query = Vec::new();
        if let Some(id) = req.session_id.as_deref().filter(|id| !id.is_empty()) {
            query.push(("sessionId", id.to_owned()));
        }
        let op = self
            .client
            .call(
                Method::POST,
                &format!("/v1beta1/{parent}/sessions"),
                &query,
                ("parent", &parent),
                Some(&Value::Object(session)),
            )
            .await
            .map_err(|e| session_error(wrap("error creating session", e)))?;

        let op_name = op.get("name").and_then(Value::as_str).unwrap_or_default();
        let session_id = session_id_by_operation_name(op_name)
            .map_err(|e| session_error(wrap("error creating session", e)))?;
        self.wait_for_session(&req.app_name, &req.user_id, &session_id)
            .await
            .map_err(|e| prefix_error("LRO for CreateSession failed", e))
    }

    /// Go `waitForOperation`: up to 10 `getSession` attempts; a not-found on attempts 0–8
    /// sleeps `min(i² × 1 s, 5 s)` and retries; any other error, or a not-found on the last
    /// attempt, is `error getting operation '{id}': …`. (Go's `LRO '%s' timed out after %d
    /// retries` cannot be reached and is not ported.)
    async fn wait_for_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
    ) -> Result<VertexSession, SessionError> {
        let req = GetRequest {
            app_name: app_name.to_owned(),
            user_id: user_id.to_owned(),
            session_id: session_id.to_owned(),
            ..Default::default()
        };
        let mut attempt = 0;
        loop {
            match self.get_session(&req).await {
                Ok(session) => return Ok(session),
                Err(SessionError::NotFound) if attempt < WAIT_ATTEMPTS - 1 => {
                    let delay = WAIT_BASE_DELAY.saturating_mul(attempt * attempt);
                    tokio::time::sleep(delay.min(WAIT_MAX_DELAY)).await;
                    attempt += 1;
                }
                Err(e) => {
                    return Err(prefix_error(
                        &format!("error getting operation '{session_id}'"),
                        e,
                    ));
                }
            }
        }
    }

    /// Go `getSession`: `GetSession`; a 404, or a session of another user, is
    /// [`SessionError::NotFound`]; the state drops null values.
    pub(crate) async fn get_session(
        &self,
        req: &GetRequest,
    ) -> Result<VertexSession, SessionError> {
        let name = self.session_name(&req.app_name, &req.session_id)?;
        let resp = self
            .client
            .call(
                Method::GET,
                &format!("/v1beta1/{name}"),
                &[],
                ("name", &name),
                None,
            )
            .await
            .map_err(|e| not_found_or("error fetching session", e))?;
        // Another user's session is reported as missing, as the in-memory and database
        // services report it.
        if str_field(&resp, "userId") != req.user_id {
            return Err(SessionError::NotFound);
        }
        let updated_at =
            update_time(&resp).map_err(|e| session_error(wrap("error fetching session", e)))?;
        Ok(VertexSession::new(
            req.app_name.clone(),
            req.user_id.clone(),
            req.session_id.clone(),
            filter_nil_values(&struct_as_map(resp.get("sessionState"))),
            updated_at,
        ))
    }

    /// Go `listSessionEvents`: `ListEvents` (with `filter=timestamp>="…"` when `after` is set
    /// and not Go's zero time),
    /// every page; `num_recent_events > 0` keeps the last that many. A 404 is
    /// [`SessionError::NotFound`].
    pub(crate) async fn list_session_events(
        &self,
        app_name: &str,
        session_id: &str,
        after: Option<DateTime<Utc>>,
        num_recent_events: usize,
    ) -> Result<Vec<Event>, SessionError> {
        let parent = self.session_name(app_name, session_id)?;
        let mut query = Vec::new();
        // Go adds the filter only `if !after.IsZero()`.
        if let Some(after) = after.filter(|t| *t != zero_time()) {
            // Go `after.Format("2006-01-02T15:04:05-07:00")` of a UTC time, `%q`-quoted.
            let formatted = after.format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
            query.push(("filter", format!("timestamp>={}", go_quote(&formatted))));
        }
        let items = self
            .client
            .list_all(
                &format!("/v1beta1/{parent}/events"),
                &query,
                ("", ""),
                "sessionEvents",
            )
            .await
            .map_err(|e| not_found_or("error fetching session events", e))?;
        let mut events = items
            .iter()
            .map(session_event_to_event)
            .collect::<Result<Vec<_>, _>>()
            .map_err(session_error)?;
        if num_recent_events > 0 && num_recent_events <= events.len() {
            events.drain(..events.len() - num_recent_events);
        }
        Ok(events)
    }

    /// Go `listSessions`: `ListSessions` (filtered to the user when one is given), every page;
    /// each session takes its user from the response, its ID from the last name segment, its
    /// state without null values and `updateTime` as its last update.
    pub(crate) async fn list_sessions(
        &self,
        req: &ListRequest,
    ) -> Result<Vec<VertexSession>, SessionError> {
        let parent = self
            .engine(self.reasoning_engine_id(&req.app_name)?)
            .engine_resource();
        let mut query = Vec::new();
        if !req.user_id.is_empty() {
            query.push((
                "filter",
                format!("userId={}", quote_filter_literal(&req.user_id)),
            ));
        }
        let items = self
            .client
            .list_all(
                &format!("/v1beta1/{parent}/sessions"),
                &query,
                ("", ""),
                "sessions",
            )
            .await
            .map_err(|e| session_error(wrap("error creating session list", e)))?;
        items
            .iter()
            .map(|s| {
                let id = session_id_by_session_name(str_field(s, "name"))?;
                let updated_at = update_time(s)?;
                Ok(VertexSession::new(
                    req.app_name.clone(),
                    str_field(s, "userId").to_owned(),
                    id,
                    filter_nil_values(&struct_as_map(s.get("sessionState"))),
                    updated_at,
                ))
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(|e| session_error(wrap("error creating session list", e)))
    }

    /// Go `deleteSession`: the `getSession` ownership check (a missing session, or another
    /// user's, deletes nothing and succeeds), then `DeleteSession` and a wait on its operation
    /// (Go `lro.Wait`).
    pub(crate) async fn delete_session(&self, req: &DeleteRequest) -> Result<(), SessionError> {
        let name = self.session_name(&req.app_name, &req.session_id)?;
        let check = GetRequest {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            session_id: req.session_id.clone(),
            ..Default::default()
        };
        match self.get_session(&check).await {
            Ok(_) => {}
            Err(SessionError::NotFound) => return Ok(()),
            Err(e) => return Err(e),
        }
        let op = self
            .client
            .call(
                Method::DELETE,
                &format!("/v1beta1/{name}"),
                &[],
                ("name", &name),
                None,
            )
            .await
            .map_err(|e| session_error(wrap("error deleting session", e)))?;
        let op = self
            .client
            .wait_operation(op)
            .await
            .map_err(session_error)?;
        // longrunning `Poll`: a done operation holding neither `error` nor `response`.
        if op.get("response").is_none_or(Value::is_null) {
            return Err(session_error("unsupported result type <nil>: <nil>".into()));
        }
        Ok(())
    }

    /// Go `appendEvent`: a partial event sends nothing; otherwise `AppendEvent` with the
    /// event's `SessionEvent`. A 404 is [`SessionError::NotFound`].
    pub(crate) async fn append_event(
        &self,
        app_name: &str,
        session_id: &str,
        event: &Event,
    ) -> Result<(), SessionError> {
        if event.response.partial {
            return Ok(());
        }
        let name = self.session_name(app_name, session_id)?;
        let body = event_to_session_event(event).map_err(session_error)?;
        self.client
            .call(
                Method::POST,
                &format!("/v1beta1/{name}:appendEvent"),
                &[],
                ("name", &name),
                Some(&body),
            )
            .await
            .map_err(|e| not_found_or("error appending event", e))?;
        Ok(())
    }
}

/// Go `fmt.Errorf("{prefix}: %w", err)` on a session error: a not-found stays
/// [`SessionError::NotFound`] (Go's `errors.Is(err, session.ErrNotFound)` still holds), an
/// invalid request keeps its variant with the prefixed text, anything else is wrapped.
pub(crate) fn prefix_error(prefix: &str, err: SessionError) -> SessionError {
    match err {
        SessionError::InvalidRequest(text) => {
            SessionError::InvalidRequest(format!("{prefix}: {text}"))
        }
        SessionError::Backend(e) => SessionError::Backend(wrap(prefix, e)),
        other => other,
    }
}

/// A call's error: [`SessionError::NotFound`] for a 404 (Go's `status.Code(err) ==
/// codes.NotFound` arm), else `{prefix}: {err}` mapped by [`session_error`].
fn not_found_or(prefix: &str, err: BoxError) -> SessionError {
    if find_vertex_error(err.as_ref()).is_some_and(VertexError::is_not_found) {
        SessionError::NotFound
    } else {
        session_error(wrap(prefix, err))
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A session's `updateTime`; an absent one is the Unix epoch (Go
/// `(*Timestamp)(nil).AsTime()`).
fn update_time(v: &Value) -> Result<DateTime<Utc>, String> {
    match v.get("updateTime").filter(|t| !t.is_null()) {
        None => Ok(DateTime::UNIX_EPOCH),
        Some(Value::String(s)) => parse_timestamp(s),
        Some(other) => Err(format!("invalid google.protobuf.Timestamp value {other}")),
    }
}

/// Go `quoteFilterLiteral`: `value` as a Google AIP-160 filter string literal, backslashes
/// escaped first, then double quotes, so caller input cannot add filter predicates.
pub(crate) fn quote_filter_literal(value: &str) -> String {
    let escaped = value.replace('\\', r"\\").replace('"', r#"\""#);
    format!("\"{escaped}\"")
}

/// Go `sessionIdBySessionName`: the segment after the last `/`.
///
/// # Errors
///
/// Go's texts: `invalid session name format "{name}": missing separator '/'` and
/// `invalid session name "{name}": empty session ID`.
pub(crate) fn session_id_by_session_name(name: &str) -> Result<String, String> {
    event_id_from_name(name)
}

/// Go `sessionIDByOperationName`: the ID between the last `/sessions/` and the last
/// `/operations/`.
///
/// # Errors
///
/// Go's texts: `invalid operation name "{name}": missing "/sessions/"`, `… missing
/// "/operations/"`, `… structure malformed or segments out of order`, `… empty session ID`.
pub(crate) fn session_id_by_operation_name(name: &str) -> Result<String, String> {
    const SESSION_PREFIX: &str = "/sessions/";
    const OPS_SUFFIX: &str = "/operations/";
    let quoted = go_quote(name);
    let Some(idx_session) = name.rfind(SESSION_PREFIX) else {
        return Err(format!(
            "invalid operation name {quoted}: missing {}",
            go_quote(SESSION_PREFIX)
        ));
    };
    let id_start = idx_session + SESSION_PREFIX.len();
    let Some(idx_ops) = name.rfind(OPS_SUFFIX) else {
        return Err(format!(
            "invalid operation name {quoted}: missing {}",
            go_quote(OPS_SUFFIX)
        ));
    };
    if id_start > idx_ops {
        return Err(format!(
            "invalid operation name {quoted}: structure malformed or segments out of order"
        ));
    }
    let id = &name[id_start..idx_ops];
    if id.is_empty() {
        return Err(format!("invalid operation name {quoted}: empty session ID"));
    }
    Ok(id.to_owned())
}
