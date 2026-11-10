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

//! [`FakeAgentEngine`]: a stateful Agent Engine fake (sessions, events, operations) with Go
//! `fakeSessions`'s seeded behaviour.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::{error_response, grpc_request};
use crate::protojson::{format_timestamp, int64_value, parse_timestamp};

/// The text the real Agent Engine answers a malformed request with.
const INVALID_ARGUMENT: &str = "Request contains an invalid argument.";

/// A stateful Agent Engine fake on 127.0.0.1 (needs a running tokio runtime). It answers the
/// session service's REST calls as the real engine does, as far as Go's tests and recordings
/// show it:
///
/// - **Seeded sessions** (Go `fakeSessions`): a session whose ID is `owned`, under any
///   project, location and engine, exists from the start and belongs to `user1` (until it is
///   deleted); every call on a session whose ID is `denied` fails 403 `PERMISSION_DENIED`
///   (`caller cannot read {name}` / `caller cannot write {name}`); any other unknown session
///   is 404 `NOT_FOUND` `Session {name} not found.`.
/// - **CreateSession**: the given `sessionId`, else a server-assigned numeric ID; a duplicate
///   ID is 400 `INVALID_ARGUMENT` `Session with user-provided ID '{name}' already exists.`
///   (the real engine's answer); an empty `userId` is 400. The answer is an operation
///   `{session}/operations/{n}` whose `response` is the Session (`@type`
///   `…v1beta1.Session`, the state as requested).
/// - **State**, as the recordings show the real engine scoping it: `app:` keys are shared by
///   every session of the engine, `user:` keys by the sessions of one user in the engine, other
///   keys (including `temp:`, so a client that leaks them is caught) belong to the session.
///   GetSession and ListSessions return the union; AppendEvent merges `actions.stateDelta`.
/// - **ListSessions**: newest first, filter `userId="…"` (Go `quoteFilterLiteral` quoting);
///   **ListEvents**: append order, filter `timestamp>="…"`; both page with `pageSize` /
///   `pageToken` (an offset). An empty page is `{}`. Another filter is 400.
/// - **DeleteSession**: removes the session and its events (404 afterwards); the answer is an
///   operation `{session}/operations/{n}` with response `google.protobuf.Empty`.
/// - **AppendEvent**: 400 `INVALID_ARGUMENT` when `author` or `invocationId` is empty; names
///   the event `{session}/events/event-N` (N = 1, 2, … per session) and stores `actions: {}`
///   when the event has none (Go `fakeVertexAiSessionService`); answers `{}`.
/// - **Operations**: with [`set_operations_pending(true)`](Self::set_operations_pending),
///   create and delete still take effect at once but answer `done: false`; `GET /ui/{op}`
///   answers every operation done (404 for an unknown one).
pub struct FakeAgentEngine {
    server: FakeServer,
    state: Arc<Mutex<Engine>>,
}

#[derive(Clone)]
struct StoredSession {
    user_id: String,
    /// Session-scoped state (no `app:` / `user:` keys).
    state: Map<String, Value>,
    create_time: String,
    update_time: String,
    /// Creation order, for ListSessions' newest-first order.
    seq: u64,
}

#[derive(Default)]
struct Engine {
    sessions: HashMap<String, StoredSession>,
    events: HashMap<String, Vec<Value>>,
    /// Seeded `owned` sessions that were deleted.
    deleted: HashSet<String>,
    /// `app:` state per engine resource.
    app_state: HashMap<String, Map<String, Value>>,
    /// `user:` state per (engine resource, user).
    user_state: HashMap<(String, String), Map<String, Value>>,
    /// Every operation answered, done, by name.
    operations: HashMap<String, Value>,
    hits: HashMap<&'static str, usize>,
    next_session_id: u64,
    next_op: u64,
    next_seq: u64,
    pending: bool,
}

/// A failure: Google status name and message.
type Failure = (&'static str, String);

impl FakeAgentEngine {
    /// Starts the fake.
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(Engine::default()));
        let handler_state = state.clone();
        let server = FakeServer::start(move |req| handler_state.lock().serve(req));
        Self { server, state }
    }

    /// The server's base URL (`http://127.0.0.1:<port>`).
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Requests per gRPC method short name (`"CreateSession"`, `"GetSession"`,
    /// `"ListSessions"`, `"DeleteSession"`, `"AppendEvent"`, `"ListEvents"`, `"GetOperation"`),
    /// failed ones included.
    pub fn hits(&self, method: &str) -> usize {
        self.state.lock().hits.get(method).copied().unwrap_or(0)
    }

    /// Every request the client sent, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.server.requests()
    }

    /// When true, create and delete answer `done: false` (their effect is immediate) and the
    /// first `/ui/` poll answers done.
    pub fn set_operations_pending(&self, pending: bool) {
        self.state.lock().pending = pending;
    }

    /// The session `name` as GetSession would answer it (`name`, `userId`, `sessionState`
    /// with app and user state merged, `createTime`, `updateTime`), if it exists; a seeded
    /// `owned` session exists until deleted.
    pub fn session(&self, name: &str) -> Option<Value> {
        let engine = self.state.lock();
        let stored = engine
            .sessions
            .get(name)
            .cloned()
            .or_else(|| engine.seed(name))?;
        Some(engine.render(name, &stored))
    }

    /// The stored events of session `name` (as appended, after name assignment).
    pub fn events(&self, name: &str) -> Vec<Value> {
        self.state
            .lock()
            .events
            .get(name)
            .cloned()
            .unwrap_or_default()
    }
}

/// The session ID of a session resource name (`{engine}/sessions/{id}`), with its engine.
fn split_session(name: &str) -> Option<(&str, &str)> {
    let (engine, id) = name.rsplit_once("/sessions/")?;
    (!engine.is_empty() && !id.is_empty() && !id.contains('/')).then_some((engine, id))
}

fn now() -> String {
    format_timestamp(&Utc::now())
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn not_found(name: &str) -> Failure {
    ("NOT_FOUND", format!("Session {name} not found."))
}

fn invalid(message: impl Into<String>) -> Failure {
    ("INVALID_ARGUMENT", message.into())
}

/// The literal of a Go `quoteFilterLiteral` (`"…"` with `\` escapes), unquoted.
fn unquote_filter_literal(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// One page of `items` under `key` (`{}` when empty), per the request's `pageSize` /
/// `pageToken` (a decimal offset).
fn page(items: Vec<Value>, req: &Value, key: &str) -> Result<Value, Failure> {
    let offset = match str_field(req, "pageToken") {
        "" => 0,
        token => token
            .parse::<usize>()
            .map_err(|_| invalid(format!("Invalid page token: {token}")))?,
    };
    let size = req
        .get("pageSize")
        .and_then(int64_value)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(usize::MAX);
    let total = items.len();
    let end = offset.saturating_add(size).min(total);
    let chunk: Vec<Value> = items
        .into_iter()
        .skip(offset)
        .take(end.saturating_sub(offset))
        .collect();
    let mut out = Map::new();
    if !chunk.is_empty() {
        out.insert(key.to_owned(), Value::Array(chunk));
    }
    if end < total {
        out.insert("nextPageToken".to_owned(), json!(end.to_string()));
    }
    Ok(Value::Object(out))
}

impl Engine {
    fn serve(&mut self, req: &RecordedRequest) -> FakeResponse {
        let Some((method, request)) = grpc_request(req) else {
            return error_response(
                "UNIMPLEMENTED",
                &format!("FakeAgentEngine: no route for {} {}", req.method, req.path),
            );
        };
        *self.hits.entry(method).or_default() += 1;
        let result = match method {
            "CreateSession" => self.create(&request),
            "GetSession" => self.get(&request),
            "ListSessions" => self.list_sessions(&request),
            "DeleteSession" => self.delete(&request),
            "AppendEvent" => self.append(&request),
            "ListEvents" => self.list_events(&request),
            _ => self.get_operation(&request),
        };
        match result {
            Ok(body) => FakeResponse::json(body.to_string()),
            Err((status, message)) => error_response(status, &message),
        }
    }

    /// The seeded session `name` stands for, if it is an undeleted `owned` session.
    fn seed(&self, name: &str) -> Option<StoredSession> {
        let (_, id) = split_session(name)?;
        (id == "owned" && !self.deleted.contains(name)).then(|| {
            let t = now();
            StoredSession {
                user_id: "user1".to_owned(),
                state: Map::new(),
                create_time: t.clone(),
                update_time: t,
                seq: 0,
            }
        })
    }

    /// The session `name`, materializing a seeded one; `denied` → 403 with `verb`.
    fn lookup(&mut self, name: &str, verb: &str) -> Result<&mut StoredSession, Failure> {
        if split_session(name).is_some_and(|(_, id)| id == "denied") {
            return Err(("PERMISSION_DENIED", format!("caller cannot {verb} {name}")));
        }
        if !self.sessions.contains_key(name)
            && let Some(seeded) = self.seed(name)
        {
            self.sessions.insert(name.to_owned(), seeded);
        }
        self.sessions.get_mut(name).ok_or_else(|| not_found(name))
    }

    /// Stores `state`'s keys in their scopes: `app:` in the engine's, `user:` in the user's,
    /// the rest in `session`.
    fn merge_state(
        &mut self,
        engine: &str,
        user: &str,
        session: &mut Map<String, Value>,
        state: &Map<String, Value>,
    ) {
        for (k, v) in state {
            let scope = if k.starts_with("app:") {
                self.app_state.entry(engine.to_owned()).or_default()
            } else if k.starts_with("user:") {
                self.user_state
                    .entry((engine.to_owned(), user.to_owned()))
                    .or_default()
            } else {
                &mut *session
            };
            scope.insert(k.clone(), v.clone());
        }
    }

    /// A session as GetSession answers it.
    fn render(&self, name: &str, s: &StoredSession) -> Value {
        let engine = split_session(name).map_or("", |(e, _)| e);
        let mut state = Map::new();
        if let Some(app) = self.app_state.get(engine) {
            state.extend(app.clone());
        }
        if let Some(user) = self.user_state.get(&(engine.to_owned(), s.user_id.clone())) {
            state.extend(user.clone());
        }
        state.extend(s.state.clone());
        let mut out = json!({
            "name": name,
            "userId": s.user_id,
            "createTime": s.create_time,
            "updateTime": s.update_time,
        });
        if !state.is_empty() {
            out["sessionState"] = Value::Object(state);
        }
        out
    }

    /// Records a done operation `{target}/operations/{n}` and answers it (not done when
    /// operations are pending).
    fn operation(&mut self, target: &str, metadata_type: &str, response: Value) -> Value {
        self.next_op += 1;
        let name = format!("{target}/operations/{}", self.next_op);
        let t = now();
        let metadata = json!({
            "@type": format!("type.googleapis.com/google.cloud.aiplatform.v1beta1.{metadata_type}"),
            "genericMetadata": {"createTime": t, "updateTime": t},
        });
        let done = json!({"name": name, "done": true, "metadata": metadata, "response": response});
        self.operations.insert(name.clone(), done.clone());
        if self.pending {
            json!({"name": name, "metadata": metadata})
        } else {
            done
        }
    }

    fn create(&mut self, req: &Value) -> Result<Value, Failure> {
        let parent = str_field(req, "parent").to_owned();
        let session = req.get("session").cloned().unwrap_or_else(|| json!({}));
        let user = str_field(&session, "userId").to_owned();
        if parent.is_empty() || user.is_empty() {
            return Err(invalid(INVALID_ARGUMENT));
        }
        let id = match str_field(req, "sessionId") {
            "" => loop {
                self.next_session_id += 1;
                let id = self.next_session_id.to_string();
                if !self
                    .sessions
                    .contains_key(&format!("{parent}/sessions/{id}"))
                {
                    break id;
                }
            },
            id => id.to_owned(),
        };
        let name = format!("{parent}/sessions/{id}");
        if self.sessions.contains_key(&name) || self.seed(&name).is_some() {
            return Err(invalid(format!(
                "Session with user-provided ID '{name}' already exists."
            )));
        }
        let requested = session
            .get("sessionState")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut own = Map::new();
        self.merge_state(&parent, &user, &mut own, &requested);
        let t = now();
        self.next_seq += 1;
        self.sessions.insert(
            name.clone(),
            StoredSession {
                user_id: user.clone(),
                state: own,
                create_time: t.clone(),
                update_time: t.clone(),
                seq: self.next_seq,
            },
        );
        self.deleted.remove(&name);
        let mut response = json!({
            "@type": "type.googleapis.com/google.cloud.aiplatform.v1beta1.Session",
            "name": name,
            "userId": user,
            "createTime": t,
            "updateTime": t,
        });
        if !requested.is_empty() {
            response["sessionState"] = Value::Object(requested);
        }
        Ok(self.operation(&name, "CreateSessionOperationMetadata", response))
    }

    fn get(&mut self, req: &Value) -> Result<Value, Failure> {
        let name = str_field(req, "name");
        let stored = self.lookup(name, "read")?.clone();
        Ok(self.render(name, &stored))
    }

    fn list_sessions(&mut self, req: &Value) -> Result<Value, Failure> {
        let parent = str_field(req, "parent");
        let user = match str_field(req, "filter") {
            "" => None,
            filter => Some(
                filter
                    .strip_prefix("userId=")
                    .and_then(unquote_filter_literal)
                    .ok_or_else(|| invalid(format!("Unsupported filter: {filter}")))?,
            ),
        };
        let mut found: Vec<(&String, &StoredSession)> = self
            .sessions
            .iter()
            .filter(|(name, s)| {
                split_session(name).is_some_and(|(e, _)| e == parent)
                    && user.as_ref().is_none_or(|u| *u == s.user_id)
            })
            .collect();
        found.sort_by(|a, b| b.1.seq.cmp(&a.1.seq).then_with(|| a.0.cmp(b.0)));
        let items = found.iter().map(|(n, s)| self.render(n, s)).collect();
        page(items, req, "sessions")
    }

    fn delete(&mut self, req: &Value) -> Result<Value, Failure> {
        let name = str_field(req, "name").to_owned();
        self.lookup(&name, "write")?;
        self.sessions.remove(&name);
        self.events.remove(&name);
        self.deleted.insert(name.clone());
        let empty = json!({"@type": "type.googleapis.com/google.protobuf.Empty"});
        Ok(self.operation(&name, "DeleteOperationMetadata", empty))
    }

    fn append(&mut self, req: &Value) -> Result<Value, Failure> {
        let name = str_field(req, "name").to_owned();
        let mut event = req.get("event").cloned().unwrap_or_else(|| json!({}));
        if str_field(&event, "author").is_empty() || str_field(&event, "invocationId").is_empty() {
            return Err(invalid(INVALID_ARGUMENT));
        }
        let session = self.lookup(&name, "write")?;
        session.update_time = now();
        let user = session.user_id.clone();
        let mut own = std::mem::take(&mut session.state);
        let delta = event
            .pointer("/actions/stateDelta")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let engine = split_session(&name).map_or("", |(e, _)| e).to_owned();
        self.merge_state(&engine, &user, &mut own, &delta);
        if let Some(s) = self.sessions.get_mut(&name) {
            s.state = own;
        }
        let events = self.events.entry(name.clone()).or_default();
        if let Some(obj) = event.as_object_mut() {
            obj.insert(
                "name".to_owned(),
                json!(format!("{name}/events/event-{}", events.len() + 1)),
            );
            obj.entry("actions").or_insert_with(|| json!({}));
        }
        events.push(event);
        Ok(json!({}))
    }

    fn list_events(&mut self, req: &Value) -> Result<Value, Failure> {
        let parent = str_field(req, "parent").to_owned();
        self.lookup(&parent, "read")?;
        let after: Option<DateTime<Utc>> = match str_field(req, "filter") {
            "" => None,
            filter => Some(
                filter
                    .strip_prefix("timestamp>=")
                    .and_then(unquote_filter_literal)
                    .and_then(|t| parse_timestamp(&t).ok())
                    .ok_or_else(|| invalid(format!("Unsupported filter: {filter}")))?,
            ),
        };
        let items = self
            .events
            .get(&parent)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                after.is_none_or(|after| {
                    parse_timestamp(str_field(e, "timestamp")).is_ok_and(|t| t >= after)
                })
            })
            .collect();
        page(items, req, "sessionEvents")
    }

    fn get_operation(&mut self, req: &Value) -> Result<Value, Failure> {
        let name = str_field(req, "name");
        self.operations
            .get(name)
            .cloned()
            .ok_or_else(|| ("NOT_FOUND", format!("Operation {name} not found.")))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testutil::send;

    const ENGINE: &str = "projects/p/locations/us-central1/reasoningEngines/123";
    const ENGINE2: &str = "projects/p/locations/us-central1/reasoningEngines/456";

    fn v1(name: &str) -> String {
        format!("/v1beta1/{name}")
    }

    async fn create(fake: &FakeAgentEngine, id: Option<&str>, session: Value) -> (u16, Value) {
        let query: Vec<(&str, &str)> = id.map(|id| ("sessionId", id)).into_iter().collect();
        send(
            &fake.url(),
            "POST",
            &v1(&format!("{ENGINE}/sessions")),
            &query,
            Some(&session),
        )
        .await
    }

    fn event(author: &str, invocation: &str) -> Value {
        json!({"author": author, "invocationId": invocation, "timestamp": "2025-01-01T00:00:01Z"})
    }

    #[tokio::test]
    async fn seeded_sessions_follow_go_fake_sessions() {
        let fake = FakeAgentEngine::start();
        let url = fake.url();
        let owned = format!("{ENGINE}/sessions/owned");
        let denied = format!("{ENGINE2}/sessions/denied");
        let other = format!("{ENGINE}/sessions/other");

        let (status, got) = send(&url, "GET", &v1(&owned), &[], None).await;
        assert_eq!(status, 200);
        assert_eq!(
            (&got["name"], &got["userId"]),
            (&json!(owned), &json!("user1"))
        );
        assert_eq!(
            send(&url, "GET", &v1(&format!("{owned}/events")), &[], None).await,
            (200, json!({}))
        );

        for (method, path, body, verb) in [
            ("GET", v1(&denied), None, "read"),
            ("GET", v1(&format!("{denied}/events")), None, "read"),
            (
                "POST",
                v1(&format!("{denied}:appendEvent")),
                Some(event("user", "inv1")),
                "write",
            ),
            ("DELETE", v1(&denied), None, "write"),
        ] {
            let (status, err) = send(&url, method, &path, &[], body.as_ref()).await;
            assert_eq!(status, 403, "{method} {path}");
            assert_eq!(
                err["error"],
                json!({"code": 403, "message": format!("caller cannot {verb} {denied}"), "status": "PERMISSION_DENIED"})
            );
        }
        for (method, path, body) in [
            ("GET", v1(&other), None),
            ("GET", v1(&format!("{other}/events")), None),
            (
                "POST",
                v1(&format!("{other}:appendEvent")),
                Some(event("user", "inv1")),
            ),
            ("DELETE", v1(&other), None),
        ] {
            let (status, err) = send(&url, method, &path, &[], body.as_ref()).await;
            assert_eq!(status, 404, "{method} {path}");
            assert_eq!(
                err["error"],
                json!({"code": 404, "message": format!("Session {other} not found."), "status": "NOT_FOUND"})
            );
        }
        assert_eq!(fake.hits("AppendEvent"), 2);
        assert_eq!(fake.hits("DeleteSession"), 2);
        assert_eq!(fake.hits("GetSession"), 3);
        assert_eq!(fake.hits("ListEvents"), 3);
        assert_eq!(fake.hits("CreateSession"), 0);
        assert_eq!(fake.session(&owned).unwrap()["userId"], json!("user1"));
        assert_eq!(fake.session(&other), None);
    }

    #[tokio::test]
    async fn create_assigns_ids_and_rejects_duplicates() {
        let fake = FakeAgentEngine::start();
        let (status, op) = create(
            &fake,
            None,
            json!({"userId": "u1", "sessionState": {"k": 5}}),
        )
        .await;
        assert_eq!(status, 200);
        let name = op["response"]["name"].as_str().unwrap().to_owned();
        let id = name.strip_prefix(&format!("{ENGINE}/sessions/")).unwrap();
        assert!(id.parse::<u64>().is_ok(), "{name}");
        assert_eq!(op["name"], json!(format!("{name}/operations/1")));
        assert_eq!(op["done"], json!(true));
        assert_eq!(
            op["metadata"]["@type"],
            json!(
                "type.googleapis.com/google.cloud.aiplatform.v1beta1.CreateSessionOperationMetadata"
            )
        );
        assert_eq!(
            op["response"]["@type"],
            json!("type.googleapis.com/google.cloud.aiplatform.v1beta1.Session")
        );
        assert_eq!(
            (&op["response"]["userId"], &op["response"]["sessionState"]),
            (&json!("u1"), &json!({"k": 5}))
        );

        let (status, op) = create(&fake, Some("s1"), json!({"userId": "u1"})).await;
        assert_eq!(status, 200);
        assert_eq!(
            op["response"]["name"],
            json!(format!("{ENGINE}/sessions/s1"))
        );
        assert!(op["response"].get("sessionState").is_none());
        for id in ["s1", "owned"] {
            let (status, err) = create(&fake, Some(id), json!({"userId": "u2"})).await;
            assert_eq!(status, 400);
            assert_eq!(
                err["error"],
                json!({
                    "code": 400,
                    "message": format!("Session with user-provided ID '{ENGINE}/sessions/{id}' already exists."),
                    "status": "INVALID_ARGUMENT",
                })
            );
        }
        let (status, _) = create(&fake, None, json!({})).await;
        assert_eq!(status, 400);

        let (status, got) = send(&fake.url(), "GET", &v1(&name), &[], None).await;
        assert_eq!(status, 200);
        assert_eq!(got["sessionState"], json!({"k": 5}));
        assert_eq!(fake.session(&name).unwrap()["userId"], json!("u1"));
    }

    #[tokio::test]
    async fn list_filters_by_quoted_user_and_pages() {
        let fake = FakeAgentEngine::start();
        let tricky = r#"a"b\c"#;
        for (id, user) in [("s1", "u1"), ("s2", tricky), ("s3", "u1")] {
            assert_eq!(
                create(&fake, Some(id), json!({"userId": user})).await.0,
                200
            );
        }
        let url = fake.url();
        let list = format!("{ENGINE}/sessions");
        let names = |v: &Value| -> Vec<String> {
            v["sessions"].as_array().map_or(vec![], |a| {
                a.iter()
                    .map(|s| {
                        s["name"]
                            .as_str()
                            .unwrap()
                            .rsplit('/')
                            .next()
                            .unwrap()
                            .to_owned()
                    })
                    .collect()
            })
        };
        let (_, all) = send(&url, "GET", &v1(&list), &[], None).await;
        assert_eq!(names(&all), ["s3", "s2", "s1"]);
        let (_, u1) = send(
            &url,
            "GET",
            &v1(&list),
            &[("filter", r#"userId="u1""#)],
            None,
        )
        .await;
        assert_eq!(names(&u1), ["s3", "s1"]);
        let (_, t) = send(
            &url,
            "GET",
            &v1(&list),
            &[("filter", r#"userId="a\"b\\c""#)],
            None,
        )
        .await;
        assert_eq!(names(&t), ["s2"]);
        let (_, none) = send(
            &url,
            "GET",
            &v1(&list),
            &[("filter", r#"userId="nobody""#)],
            None,
        )
        .await;
        assert_eq!(none, json!({}));
        let (_, other) = send(&url, "GET", &v1(&format!("{ENGINE2}/sessions")), &[], None).await;
        assert_eq!(other, json!({}));
        let (status, _) = send(&url, "GET", &v1(&list), &[("filter", "appName=x")], None).await;
        assert_eq!(status, 400);

        let (_, p1) = send(&url, "GET", &v1(&list), &[("pageSize", "2")], None).await;
        assert_eq!(
            (names(&p1), &p1["nextPageToken"]),
            (vec!["s3".to_owned(), "s2".to_owned()], &json!("2"))
        );
        let (_, p2) = send(
            &url,
            "GET",
            &v1(&list),
            &[("pageSize", "2"), ("pageToken", "2")],
            None,
        )
        .await;
        assert_eq!(names(&p2), ["s1"]);
        assert!(p2.get("nextPageToken").is_none());
    }

    #[tokio::test]
    async fn delete_removes_the_session_and_operations_can_pend() {
        let fake = FakeAgentEngine::start();
        let url = fake.url();
        let name = format!("{ENGINE}/sessions/s1");
        create(&fake, Some("s1"), json!({"userId": "u1"})).await;
        let (_, op) = send(&url, "DELETE", &v1(&name), &[], None).await;
        assert_eq!(op["done"], json!(true));
        assert_eq!(
            op["response"],
            json!({"@type": "type.googleapis.com/google.protobuf.Empty"})
        );
        assert_eq!(
            op["metadata"]["@type"],
            json!("type.googleapis.com/google.cloud.aiplatform.v1beta1.DeleteOperationMetadata")
        );
        assert_eq!(send(&url, "GET", &v1(&name), &[], None).await.0, 404);
        assert_eq!(send(&url, "DELETE", &v1(&name), &[], None).await.0, 404);

        let owned = format!("{ENGINE}/sessions/owned");
        fake.set_operations_pending(true);
        let (_, op) = send(&url, "DELETE", &v1(&owned), &[], None).await;
        assert!(op.get("done").is_none(), "{op}");
        assert!(op.get("response").is_none());
        assert_eq!(send(&url, "GET", &v1(&owned), &[], None).await.0, 404);
        assert_eq!(fake.session(&owned), None);
        let op_name = op["name"].as_str().unwrap();
        let (status, polled) = send(&url, "GET", &format!("/ui/{op_name}"), &[], None).await;
        assert_eq!(status, 200);
        assert_eq!(
            (&polled["done"], &polled["name"]),
            (&json!(true), &op["name"])
        );

        let (_, op) = create(&fake, Some("s2"), json!({"userId": "u1"})).await;
        assert!(op.get("done").is_none());
        assert_eq!(
            send(
                &url,
                "GET",
                &v1(&format!("{ENGINE}/sessions/s2")),
                &[],
                None
            )
            .await
            .0,
            200
        );
        let (_, polled) = send(
            &url,
            "GET",
            &format!("/ui/{}", op["name"].as_str().unwrap()),
            &[],
            None,
        )
        .await;
        assert_eq!(
            polled["response"]["name"],
            json!(format!("{ENGINE}/sessions/s2"))
        );
        let (status, _) = send(
            &url,
            "GET",
            &format!("/ui/{ENGINE}/operations/99"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(fake.hits("GetOperation"), 3);
    }

    #[tokio::test]
    async fn append_validates_names_events_and_merges_state_by_scope() {
        let fake = FakeAgentEngine::start();
        let url = fake.url();
        create(
            &fake,
            Some("a"),
            json!({"userId": "u1", "sessionState": {"app:k1": "v1", "user:k1": "v1", "k": 1}}),
        )
        .await;
        create(&fake, Some("b"), json!({"userId": "u1"})).await;
        create(&fake, Some("c"), json!({"userId": "u2"})).await;
        let a = format!("{ENGINE}/sessions/a");
        let append = |body: Value| {
            let url = url.clone();
            let a = a.clone();
            async move {
                send(
                    &url,
                    "POST",
                    &v1(&format!("{a}:appendEvent")),
                    &[],
                    Some(&body),
                )
                .await
            }
        };
        for bad in [event("", "inv1"), event("user", ""), json!({})] {
            let (status, err) = append(bad).await;
            assert_eq!(status, 400);
            assert_eq!(
                err["error"],
                json!({"code": 400, "message": "Request contains an invalid argument.", "status": "INVALID_ARGUMENT"})
            );
        }
        assert_eq!(append(event("user", "inv1")).await, (200, json!({})));
        let mut second = json!({
            "author": "user", "invocationId": "inv2", "timestamp": "2025-01-01T00:00:02Z",
            "actions": {"stateDelta": {"app:k2": "v2", "user:k2": "v2", "sk": "v2", "temp:t": "x"}},
        });
        assert_eq!(append(second.clone()).await.0, 200);

        let events = fake.events(&a);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["name"], json!(format!("{a}/events/event-1")));
        assert_eq!(events[0]["actions"], json!({}));
        second["name"] = json!(format!("{a}/events/event-2"));
        assert_eq!(events[1], second);

        let state = |name: &str| {
            fake.session(&format!("{ENGINE}/sessions/{name}")).unwrap()["sessionState"].clone()
        };
        assert_eq!(
            state("a"),
            json!({"app:k1": "v1", "app:k2": "v2", "user:k1": "v1", "user:k2": "v2", "k": 1, "sk": "v2", "temp:t": "x"})
        );
        assert_eq!(
            state("b"),
            json!({"app:k1": "v1", "app:k2": "v2", "user:k1": "v1", "user:k2": "v2"})
        );
        assert_eq!(state("c"), json!({"app:k1": "v1", "app:k2": "v2"}));

        let (_, all) = send(&url, "GET", &v1(&format!("{a}/events")), &[], None).await;
        assert_eq!(all["sessionEvents"], json!(events));
        let (_, after) = send(
            &url,
            "GET",
            &v1(&format!("{a}/events")),
            &[("filter", r#"timestamp>="2025-01-01T00:00:02+00:00""#)],
            None,
        )
        .await;
        assert_eq!(after["sessionEvents"], json!([events[1]]));
        let (_, page) = send(
            &url,
            "GET",
            &v1(&format!("{a}/events")),
            &[("pageSize", "1")],
            None,
        )
        .await;
        assert_eq!(
            (&page["sessionEvents"], &page["nextPageToken"]),
            (&json!([events[0]]), &json!("1"))
        );
        assert_eq!(fake.hits("AppendEvent"), 5);
        assert_eq!(fake.requests().len(), 3 + 5 + 3);
    }

    #[tokio::test]
    async fn unknown_routes_are_unimplemented() {
        let fake = FakeAgentEngine::start();
        let (status, err) = send(&fake.url(), "GET", "/v1/other", &[], None).await;
        assert_eq!(status, 501);
        assert_eq!(err["error"]["status"], json!("UNIMPLEMENTED"));
    }

    #[test]
    fn filter_literals_unquote_go_quote_filter_literal() {
        assert_eq!(unquote_filter_literal(r#""u1""#).as_deref(), Some("u1"));
        assert_eq!(
            unquote_filter_literal(r#""a\"b\\c""#).as_deref(),
            Some(r#"a"b\c"#)
        );
        assert_eq!(unquote_filter_literal(r#""a"b""#), None);
        assert_eq!(unquote_filter_literal("u1"), None);
    }
}
