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

//! A reusable, backend-agnostic conformance suite for [`SessionService`] implementations.
//!
//! Enabled by the `testsuite` feature. Call [`run`](crate::testsuite::run) from a backend's tests with a factory
//! that returns a fresh service, or [`run_named`](crate::testsuite::run_named) when the factory needs the case name (to
//! pick a per-case recording, for example).

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Duration, SubsecRound, TimeZone, Utc};
use gadk_genai::{
    Citation, CitationMetadata, Content, GenerateContentResponseUsageMetadata, GroundingMetadata,
};
use gadk_model::LlmResponse;
use serde_json::{Map, Value, json};

use crate::error::SessionError;
use crate::event::{Event, EventActions, EventCompaction, EventRef};
use crate::service::{
    CreateRequest, DeleteRequest, GetRequest, KEY_PREFIX_TEMP, ListRequest, Session,
    SessionService, State,
};
use crate::toolauth::{
    Config, Credential, CredentialType, OAuth2Auth, OAuthFlow, OAuthFlows, Scheme, SchemeType,
};

/// A snapshot of a session's public state, for test comparisons.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExpectedSession {
    /// The app name.
    pub app_name: String,
    /// The user id.
    pub user_id: String,
    /// The session id.
    pub session_id: String,
    /// The session state.
    pub state: BTreeMap<String, Value>,
    /// The session events, in order.
    pub events: Vec<Event>,
}

/// Extracts an [`ExpectedSession`] from any [`Session`] implementation.
pub fn snapshot(s: &dyn Session) -> ExpectedSession {
    ExpectedSession {
        app_name: s.app_name().to_string(),
        user_id: s.user_id().to_string(),
        session_id: s.id().to_string(),
        state: s.state().all().into_iter().collect(),
        events: s.events(),
    }
}

/// Configuration for adaptive suite runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SuiteOptions {
    /// The backend accepts a caller-provided session ID on create.
    pub supports_user_provided_session_id: bool,
    /// The backend names events itself, so a caller's event ID is not what it stores.
    pub provides_server_assigned_event_id: bool,
    /// Runs the case that requires `append_event` to refuse an event whose requested auth
    /// config does not decode as a [`crate::toolauth::Config`]. [`SessionService`] does not
    /// require that check.
    pub checks_requested_auth_configs: bool,
    /// The app name to use; `"testApp"` when `None`.
    pub app_name: Option<String>,
}

/// Runs every conformance case against a fresh service from `factory`, sequentially, and
/// panics listing every failure as `sessiontestsuite/<Case/subcase>: <message>`.
pub async fn run<F, Fut>(opts: SuiteOptions, factory: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn SessionService>>,
{
    run_named(opts, |_| factory()).await;
}

/// Like [`run`], but passes each case's name to the factory before the case runs. The name is
/// spelled exactly as the failure lines spell it after `sessiontestsuite/` (`"Create/full_key"`,
/// `"Get/ok"`, `"Create/when_already_exists,_it_fails"`, `"List"`, …: the case's subtest name
/// with spaces as `_`), so a backend can, for example, pick the recording made for that case.
pub async fn run_named<F, Fut>(opts: SuiteOptions, factory: F)
where
    F: Fn(&'static str) -> Fut,
    Fut: Future<Output = Arc<dyn SessionService>>,
{
    macro_rules! cases {
        ($($name:literal => $f:ident),* $(,)?) => {{
            let mut failures: Vec<String> = Vec::new();
            $(
                let mut t = T::default();
                let svc = factory($name).await;
                if let Err(fatal) = $f(&mut t, svc, &opts).await {
                    t.errors.push(fatal);
                }
                for e in t.errors {
                    failures.push(format!("sessiontestsuite/{}: {}", $name, e));
                }
            )*
            failures
        }};
    }
    let failures = cases! {
        "Create/full_key" => create_full_key,
        "Create/generated_session_id" => create_generated_session_id,
        "Create/when_already_exists,_it_fails" => create_when_already_exists_it_fails,
        "Get/ok" => get_ok,
        "Get/error_when_not_found" => get_error_when_not_found,
        "Get/get_session_respects_user_id" => get_session_respects_user_id,
        "Get/with_config_filters" => get_with_config_filters,
        "List" => list,
        "Delete" => delete,
        "delete_session_respects_user_id" => delete_session_respects_user_id,
        "AppendEvent/when_session_not_found_should_fail" => append_event_when_session_not_found_should_fail,
        "AppendEvent/when_session_deleted_returns_ErrNotFound" => append_event_when_session_deleted_returns_err_not_found,
        "AppendEvent/ok" => append_event_ok,
        "AppendEvent/compaction_record_round_trips" => append_event_compaction_record_round_trips,
        "AppendEvent/requested_auth_configs_round_trip" => append_event_requested_auth_configs_round_trip,
        "AppendEvent/requested_auth_config_that_does_not_decode_is_refused" => append_event_requested_auth_config_that_does_not_decode_is_refused,
        "AppendEvent/a_hole_still_names_its_event_after_a_round_trip" => append_event_a_hole_still_names_its_event_after_a_round_trip,
        "AppendEvent/a_missing_event_id_is_assigned" => append_event_a_missing_event_id_is_assigned,
        "AppendEvent/partial_events_are_not_persisted" => append_event_partial_events_are_not_persisted,
        "AppendEvent/with_bytes_content" => append_event_with_bytes_content,
        "AppendEvent/with_existing_events" => append_event_with_existing_events,
        "AppendEvent/with_all_fields" => append_event_with_all_fields,
        "StateManagement/app_state_is_shared" => state_management_app_state_is_shared,
        "StateManagement/user_state_is_user_specific" => state_management_user_state_is_user_specific,
        "StateManagement/session_state_is_not_shared" => state_management_session_state_is_not_shared,
        "StateManagement/temp_state_is_not_persisted" => state_management_temp_state_is_not_persisted,
    };
    if !failures.is_empty() {
        panic!("{}", failures.join("\n"));
    }
}

/// Test context: `errorf` records a failure and continues; a fatal failure is the
/// subtest's `Err` return.
#[derive(Default)]
struct T {
    errors: Vec<String>,
}

impl T {
    fn errorf(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }
}

type Outcome = Result<(), String>;

fn app_name(opts: &SuiteOptions) -> String {
    opts.app_name
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "testApp".to_string())
}

fn state(pairs: &[(&str, Value)]) -> Option<Map<String, Value>> {
    Some(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    )
}

fn create_req(app: &str, user: &str) -> CreateRequest {
    CreateRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        ..Default::default()
    }
}

fn get_req(app: &str, user: &str, session_id: &str) -> GetRequest {
    GetRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        session_id: session_id.to_string(),
        ..Default::default()
    }
}

fn simple_event(id: &str, invocation_id: &str) -> Event {
    Event {
        id: id.to_string(),
        author: "user".to_string(),
        invocation_id: invocation_id.to_string(),
        ..Default::default()
    }
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, nanos: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap() + Duration::nanoseconds(nanos)
}

/// The concatenated text parts of `c`.
fn text_of(c: Option<&Content>) -> String {
    c.map(|c| c.parts.iter().map(|p| p.text.as_str()).collect())
        .unwrap_or_default()
}

fn value_of(snap: &ExpectedSession, key: &str) -> Option<Value> {
    snap.state.get(key).cloned()
}

// ---- Create ----

async fn create_full_key(t: &mut T, s: Arc<dyn SessionService>, opts: &SuiteOptions) -> Outcome {
    if !opts.supports_user_provided_session_id {
        // Skipped: the full-key case requires user provided session ID support.
        return Ok(());
    }
    let app = app_name(opts);
    let req = CreateRequest {
        app_name: app.clone(),
        user_id: "testUserID".into(),
        session_id: Some("test-session-id".into()),
        state: state(&[("k", json!(5.0))]),
    };
    let got = s.create(req.clone()).await;
    if opts.supports_user_provided_session_id {
        let got = got.map_err(|e| format!("Create() error = {e}, wantErr false"))?;
        if got.app_name() != req.app_name {
            t.errorf(format!(
                "AppName got: {}, want: {}",
                got.app_name(),
                req.app_name
            ));
        }
        if got.user_id() != req.user_id {
            t.errorf(format!(
                "UserID got: {}, want: {}",
                got.user_id(),
                req.user_id
            ));
        }
        if Some(got.id()) != req.session_id.as_deref() {
            t.errorf(format!(
                "SessionID got: {}, want: {:?}",
                got.id(),
                req.session_id
            ));
        }
    } else if got.is_ok() {
        return Err("Expected error for user-provided SessionID".into());
    }
    Ok(())
}

async fn create_generated_session_id(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let got = s
        .create(CreateRequest {
            state: state(&[("k", json!(5.0))]),
            ..create_req(&app, "testUserID")
        })
        .await
        .map_err(|e| format!("Create() error = {e}"))?;
    if got.id().is_empty() {
        t.errorf("Expected generated SessionID, got empty");
    }
    Ok(())
}

async fn create_when_already_exists_it_fails(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let got1 = s
        .create(create_req(&app, "testUserID"))
        .await
        .map_err(|e| format!("First Create() failed: {e}"))?;
    let req2 = CreateRequest {
        session_id: Some(got1.id().to_string()),
        ..create_req(&app, "testUserID")
    };
    let res = s.create(req2).await;
    if opts.supports_user_provided_session_id {
        if res.is_ok() {
            t.errorf("Expected failure when creating duplicate session");
        }
    } else if res.is_ok() {
        t.errorf("Expected failure (unsupported or duplicate)");
    }
    Ok(())
}

// ---- Get ----

async fn get_ok(t: &mut T, s: Arc<dyn SessionService>, opts: &SuiteOptions) -> Outcome {
    let app = app_name(opts);
    let req = CreateRequest {
        state: state(&[("k1", json!("v1"))]),
        ..create_req(&app, "testUserID")
    };
    let created = s
        .create(req.clone())
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let got = s
        .get(get_req(&req.app_name, &req.user_id, created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.app_name != req.app_name {
        t.errorf(format!(
            "Get AppName = {}, want {}",
            snap.app_name, req.app_name
        ));
    }
    if snap.user_id != req.user_id {
        t.errorf(format!(
            "Get UserID = {}, want {}",
            snap.user_id, req.user_id
        ));
    }
    if value_of(&snap, "k1") != Some(json!("v1")) {
        t.errorf(format!(
            "Get State[k1] = {:?}, want v1",
            value_of(&snap, "k1")
        ));
    }
    Ok(())
}

async fn get_error_when_not_found(
    t: &mut T,
    s: Arc<dyn SessionService>,
    _opts: &SuiteOptions,
) -> Outcome {
    if s.get(get_req("nonExistent", "user", "s1")).await.is_ok() {
        t.errorf("Expected error for non-existent session");
    }
    Ok(())
}

async fn get_session_respects_user_id(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let c1 = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Create user1 failed: {e}"))?;
    s.append_event(c1.as_ref(), &mut simple_event("event1", "inv1"))
        .await
        .map_err(|e| format!("AppendEvent failed: {e}"))?;
    if s.get(get_req(&app, "user2", c1.id())).await.is_ok() {
        t.errorf("Expected error or not found when getting session with wrong UserID");
    }
    Ok(())
}

async fn get_with_config_filters(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let mut req = create_req(&app, "user1");
    if opts.supports_user_provided_session_id {
        req.session_id = Some("s1".into());
    }
    let created = s
        .create(req)
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;

    let num_test_events = 5;
    let base_time = utc(2025, 1, 1, 0, 0, 0, 0);
    let mut timestamps = Vec::new();
    for i in 1..=num_test_events {
        let ts = base_time + Duration::seconds(i);
        timestamps.push(ts);
        let mut event = Event {
            timestamp: ts,
            ..simple_event(&i.to_string(), "inv1")
        };
        s.append_event(created.as_ref(), &mut event)
            .await
            .map_err(|e| format!("AppendEvent failed: {e}"))?;
    }

    let id = created.id().to_string();
    let get = |num_recent_events: usize, after: Option<DateTime<Utc>>| {
        let s = s.clone();
        let req = GetRequest {
            num_recent_events,
            after,
            ..get_req(&app, "user1", &id)
        };
        async move { s.get(req).await.map_err(|e| format!("Get error: {e}")) }
    };

    let got_normal = get(0, None).await?;
    if got_normal.events().len() != 5 {
        t.errorf(format!(
            "Get no filter events len = {}, want 5",
            got_normal.events().len()
        ));
    }
    let got_limit = get(3, None).await?;
    if got_limit.events().len() != 3 {
        t.errorf(format!(
            "Get NumRecentEvents len = {}, want 3",
            got_limit.events().len()
        ));
    }
    let got_after = get(0, Some(timestamps[1])).await?;
    if got_after.events().len() != 4 {
        t.errorf(format!(
            "Get After filter events len = {}, want 4",
            got_after.events().len()
        ));
    }
    let got_combined = get(2, Some(timestamps[0])).await?;
    if got_combined.events().len() != 2 {
        t.errorf(format!(
            "Get Combined filters len = {}, want 2",
            got_combined.events().len()
        ));
    }
    Ok(())
}

// ---- List / Delete ----

async fn list(t: &mut T, s: Arc<dyn SessionService>, opts: &SuiteOptions) -> Outcome {
    let app = app_name(opts);
    s.create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Create user1 session1 failed: {e}"))?;
    s.create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Create user1 session2 failed: {e}"))?;
    s.create(create_req(&app, "user2"))
        .await
        .map_err(|e| format!("Create user2 failed: {e}"))?;

    let list_req = |user: &str| ListRequest {
        app_name: app.clone(),
        user_id: user.to_string(),
    };
    for (user, want, what) in [
        ("user1", 2, "user1"),
        ("user2", 1, "user2"),
        ("nonExistent", 0, "nonExistent"),
        ("", 3, "all users"),
    ] {
        let got = s
            .list(list_req(user))
            .await
            .map_err(|e| format!("List {what} failed: {e}"))?;
        if got.len() != want {
            t.errorf(format!("List {what} len = {}, want {want}", got.len()));
        }
    }
    Ok(())
}

async fn delete(t: &mut T, s: Arc<dyn SessionService>, opts: &SuiteOptions) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    s.delete(DeleteRequest {
        app_name: app.clone(),
        user_id: "user1".into(),
        session_id: created.id().to_string(),
    })
    .await
    .map_err(|e| format!("Delete() error = {e}"))?;

    match s.get(get_req(&app, "user1", created.id())).await {
        Ok(_) => t.errorf("Expected error when getting deleted session"),
        // A Service that reports a missing session as a plain error leaves callers unable
        // to tell it from a storage failure.
        Err(SessionError::NotFound) => {}
        Err(e) => t.errorf(format!(
            "Get(deleted session) error = {e}, want SessionError::NotFound"
        )),
    }

    if opts.supports_user_provided_session_id {
        let res = s
            .delete(DeleteRequest {
                app_name: app.clone(),
                user_id: "user1".into(),
                session_id: "nonExistent".into(),
            })
            .await;
        // Accepts `Ok` or `InvalidRequest` (a Vertex AI backend fails on delete when the
        // session is not found); `InvalidRequest` stands in for an invalid-argument status.
        match res {
            Ok(()) | Err(SessionError::InvalidRequest(_)) => {}
            Err(e) => t.errorf(format!(
                "Delete() non-existent error = {e}, want Ok or InvalidRequest"
            )),
        }
    }
    Ok(())
}

async fn delete_session_respects_user_id(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let c1 = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Create user1 failed: {e}"))?;
    // A delete by a different user must not remove user1's session. Backends may either
    // error or no-op; the invariant is that the session survives.
    let _ = s
        .delete(DeleteRequest {
            app_name: app.clone(),
            user_id: "user2".into(),
            session_id: c1.id().to_string(),
        })
        .await;
    if let Err(e) = s.get(get_req(&app, "user1", c1.id())).await {
        t.errorf(format!(
            "session was removed by a different user's delete: {e}"
        ));
    }
    Ok(())
}

// ---- AppendEvent ----

/// A session of no backend's type.
struct MockSession {
    app_name: String,
    user_id: String,
    id: String,
    state: MockState,
}

/// A `Session` must hand out a state, so the mock's is empty.
struct MockState;

impl State for MockState {
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

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app_name
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> Vec<Event> {
        Vec::new()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn append_event_when_session_not_found_should_fail(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let m = MockSession {
        app_name: app_name(opts),
        user_id: "user1".into(),
        id: "nonExistent".into(),
        state: MockState,
    };
    let mut event = simple_event("event1", "inv1");
    if s.append_event(&m, &mut event).await.is_ok() {
        t.errorf("AppendEvent() expected error for non-existent session, got nil");
    }
    Ok(())
}

async fn append_event_when_session_deleted_returns_err_not_found(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    s.delete(DeleteRequest {
        app_name: app.clone(),
        user_id: "user1".into(),
        session_id: created.id().to_string(),
    })
    .await
    .map_err(|e| format!("Setup: Delete failed: {e}"))?;

    // Unlike the case above, this passes the service a session of its own type, so it
    // reaches the storage lookup instead of stopping at a type check.
    match s
        .append_event(created.as_ref(), &mut simple_event("event1", "inv1"))
        .await
    {
        Ok(()) => {
            return Err(
                "AppendEvent(deleted session) error = nil, want SessionError::NotFound".into(),
            );
        }
        Err(SessionError::NotFound) => {}
        Err(e) => t.errorf(format!(
            "AppendEvent(deleted session) error = {e}, want SessionError::NotFound"
        )),
    }
    Ok(())
}

async fn append_event_ok(t: &mut T, s: Arc<dyn SessionService>, opts: &SuiteOptions) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let mut event = simple_event("new_event1", "inv1");
    s.append_event(created.as_ref(), &mut event)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;
    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 1 {
        t.errorf(format!("Expected 1 event, got {}", snap.events.len()));
    } else if !opts.provides_server_assigned_event_id && snap.events[0].id != event.id {
        t.errorf(format!(
            "Event ID mismatch: got {}, want {}",
            snap.events[0].id, event.id
        ));
    }
    Ok(())
}

async fn append_event_compaction_record_round_trips(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    // A context-compaction summary carries its content only on actions.compaction. A
    // backend that persists events by looking only at content or deltas drops it silently.
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;

    // Nanosecond precision on purpose: a millisecond-truncated fixture cannot tell a
    // faithful backend from one that rounds.
    let start = utc(2026, 3, 4, 5, 6, 7, 123_456_789);
    let end = start + Duration::seconds(5) + Duration::nanoseconds(987);
    let excluded = vec![
        EventRef {
            invocation_id: "inv-1".into(),
            timestamp: start,
        },
        EventRef {
            invocation_id: "inv-2".into(),
            timestamp: end,
        },
    ];
    let mut event = Event {
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: start,
                end_timestamp: end,
                compacted_content: Some(Content::from_text("summary of earlier turns", "model")),
                excluded_events: excluded.clone(),
            }),
            ..Default::default()
        },
        ..simple_event("compaction_event", "inv-compaction")
    };
    s.append_event(created.as_ref(), &mut event)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;

    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 1 {
        return Err(format!("Expected 1 event, got {}", snap.events.len()));
    }
    let Some(c) = snap.events[0].actions.compaction.clone() else {
        return Err("Actions.Compaction was not persisted, so the summary is unrecoverable".into());
    };
    if c.start_timestamp != start || c.end_timestamp != end {
        t.errorf(format!(
            "compaction range = [{}, {}], want [{start}, {end}]",
            c.start_timestamp, c.end_timestamp
        ));
    }
    let text = text_of(c.compacted_content.as_ref());
    if text != "summary of earlier turns" {
        t.errorf(format!(
            "compacted content = {text:?}, want \"summary of earlier turns\""
        ));
    }
    // The covered set is what prompt assembly deletes on.
    if c.excluded_events != excluded {
        t.errorf(format!(
            "excluded events mismatch: want {excluded:?}, got {:?}",
            c.excluded_events
        ));
    }
    Ok(())
}

/// The `call-1` request of the `requested_auth_configs_round_trip` case.
fn requested_auth_configs() -> BTreeMap<String, Config> {
    let flow = OAuthFlow {
        authorization_url: "https://example.com/authorize".into(),
        token_url: "https://example.com/token".into(),
        scopes: BTreeMap::from([("read".into(), "Read".into())]),
        ..Default::default()
    };
    let config = Config {
        auth_scheme: Scheme {
            r#type: SchemeType::OAUTH2,
            flows: Some(OAuthFlows {
                authorization_code: Some(flow),
                ..Default::default()
            }),
            ..Default::default()
        },
        raw_auth_credential: Some(Credential {
            auth_type: CredentialType::OAUTH2,
            oauth2: Some(OAuth2Auth {
                client_id: "cid".into(),
                token_endpoint_auth_method: "client_secret_basic".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        credential_key: "my_tool_tokens".into(),
        ..Default::default()
    };
    BTreeMap::from([("call-1".into(), config)])
}

async fn append_event_requested_auth_configs_round_trip(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    // A pending credential request is recorded only on the event that made it. The user may
    // answer after a restart or on another server, and a backend that drops the config leaves
    // nothing to match the answer against.
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let mut event = Event {
        actions: EventActions {
            requested_auth_configs: requested_auth_configs(),
            ..Default::default()
        },
        author: "agent".into(),
        ..simple_event("auth_request_event", "inv-auth")
    };
    s.append_event(created.as_ref(), &mut event)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;

    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 1 {
        return Err(format!("Expected 1 event, got {}", snap.events.len()));
    }
    let want = requested_auth_configs();
    if snap.events[0].actions.requested_auth_configs != want {
        t.errorf(format!(
            "RequestedAuthConfigs mismatch after a round trip: want {want:?}, got {:?}",
            snap.events[0].actions.requested_auth_configs
        ));
    }
    Ok(())
}

async fn append_event_requested_auth_config_that_does_not_decode_is_refused(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    // A toolauth Config struct literal is not checked when it is built, and its encoder
    // accepts configs its decoder refuses. A backend that stored one could not read the
    // session back. A backend that opts in refuses the append, even if it never decodes.
    if !opts.checks_requested_auth_configs {
        // Skipped: the backend does not check requested auth configs on append.
        return Ok(());
    }
    let app = app_name(opts);
    let cases = [
        (
            "zero_scheme",
            Config {
                credential_key: "k".into(),
                ..Default::default()
            },
        ),
        (
            "oauth2_flow_without_urls",
            Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::OAUTH2,
                    flows: Some(OAuthFlows {
                        authorization_code: Some(OAuthFlow::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        (
            "credential_without_auth_type",
            Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::API_KEY,
                    r#in: "header".into(),
                    name: "X-Key".into(),
                    ..Default::default()
                },
                raw_auth_credential: Some(Credential {
                    api_key: "k".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ];
    for (name, config) in cases {
        // A fatal failure ends this subcase only.
        let mut sub = T::default();
        let outcome: Outcome = async {
            let created = s
                .create(create_req(&app, "user1"))
                .await
                .map_err(|e| format!("Setup: Create failed: {e}"))?;
            let mut event = Event {
                actions: EventActions {
                    requested_auth_configs: BTreeMap::from([("call-1".into(), config)]),
                    ..Default::default()
                },
                author: "agent".into(),
                ..simple_event("bad_auth_request_event", "inv-auth")
            };
            if s.append_event(created.as_ref(), &mut event).await.is_ok() {
                sub.errorf(
                    "AppendEvent() succeeded, want an error for a request that does not decode",
                );
            }
            let got = s
                .get(get_req(&app, "user1", created.id()))
                .await
                .map_err(|e| format!("Get() after the refused append failed: {e}"))?;
            let n = got.events().len();
            if n != 0 {
                sub.errorf(format!("Get() returned {n} events, want 0"));
            }
            let n = created.events().len();
            if n != 0 {
                sub.errorf(format!(
                    "the caller's session holds {n} events after the refused append, want 0"
                ));
            }
            Ok(())
        }
        .await;
        if let Err(fatal) = outcome {
            sub.errors.push(fatal);
        }
        for e in sub.errors {
            t.errorf(format!("{name}: {e}"));
        }
    }
    Ok(())
}

async fn append_event_a_hole_still_names_its_event_after_a_round_trip(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    // The previous case checks the record survives. This checks the record and the events
    // still agree about which event is which: a hole names an event by invocation and
    // timestamp, and a backend that keeps event timestamps and record payloads in
    // different precision domains breaks the match. Compaction compares at microsecond
    // granularity, which is what the assertion below mirrors.
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;

    let mut turn = Event {
        author: "user".into(),
        invocation_id: "inv-hole".into(),
        timestamp: utc(2026, 3, 4, 5, 6, 7, 123_456_789),
        ..Default::default()
    };
    s.append_event(created.as_ref(), &mut turn)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;

    let read_back = || {
        let s = s.clone();
        let req = get_req(&app, "user1", created.id());
        async move {
            s.get(req)
                .await
                .map(|got| snapshot(got.as_ref()).events)
                .map_err(|e| format!("Get() error = {e}"))
        }
    };

    let stored = read_back().await?;
    if stored.len() != 1 {
        return Err(format!("stored {} events, want 1", stored.len()));
    }
    // The reference is built from the caller's own event, not from the event read back.
    let reference = EventRef {
        invocation_id: turn.invocation_id.clone(),
        timestamp: turn.timestamp,
    };
    let mut record = Event {
        author: "user".into(),
        invocation_id: "inv-summary".into(),
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: reference.timestamp - Duration::hours(1),
                end_timestamp: reference.timestamp + Duration::hours(1),
                compacted_content: Some(Content::from_text("summary", "model")),
                excluded_events: vec![reference],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    s.append_event(created.as_ref(), &mut record)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;

    let after = read_back().await?;
    let mut event = None;
    let mut rng = None;
    for ev in &after {
        if let Some(c) = &ev.actions.compaction {
            rng = Some(c.clone());
            continue;
        }
        if ev.invocation_id == "inv-hole" {
            event = Some(ev.clone());
        }
    }
    let (Some(event), Some(rng)) = (event, rng.filter(|r| r.excluded_events.len() == 1)) else {
        return Err(format!(
            "expected the turn and a record naming one hole, got events={after:?}"
        ));
    };
    let got = &rng.excluded_events[0];
    if got.invocation_id != event.invocation_id
        || got.timestamp.trunc_subsecs(6) != event.timestamp.trunc_subsecs(6)
    {
        t.errorf(format!(
            "the hole no longer names its event after a round trip:\n  hole  = {} @ {}\n  event = {} @ {}",
            got.invocation_id, got.timestamp, event.invocation_id, event.timestamp
        ));
    }
    Ok(())
}

async fn append_event_a_missing_event_id_is_assigned(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    // An event built as a struct literal never passes through Event::new, so it arrives
    // with no ID; two such events are indistinguishable to anything that identifies events
    // by ID.
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    for _ in 0..2 {
        let mut event = simple_event("", "inv1");
        s.append_event(created.as_ref(), &mut event)
            .await
            .map_err(|e| format!("AppendEvent() error = {e}"))?;
        // Where the server names the event, the caller's struct is not where the name
        // appears; the stored event must have one everywhere, which the read below checks.
        if !opts.provides_server_assigned_event_id && event.id.is_empty() {
            t.errorf("AppendEvent left the event without an ID");
        }
    }
    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 2 {
        return Err(format!("stored {} events, want 2", snap.events.len()));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, ev) in snap.events.iter().enumerate() {
        if ev.id.is_empty() {
            t.errorf(format!("stored event {i} has no ID"));
            continue;
        }
        if !seen.insert(ev.id.clone()) {
            t.errorf(format!("stored event {i} reuses ID {:?}", ev.id));
        }
    }
    Ok(())
}

async fn append_event_partial_events_are_not_persisted(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let mut event = Event {
        response: LlmResponse {
            partial: true,
            ..Default::default()
        },
        ..simple_event("partial_event", "inv1")
    };
    s.append_event(created.as_ref(), &mut event)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;
    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if !snap.events.is_empty() {
        t.errorf(format!(
            "Expected 0 events (Partial=true should be skipped), got {}",
            snap.events.len()
        ));
    }
    Ok(())
}

async fn append_event_with_bytes_content(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let mut event = Event {
        response: LlmResponse {
            content: Some(Content::from_bytes(
                b"test_image_data".to_vec().into(),
                "image/png",
                "user",
            )),
            ..Default::default()
        },
        ..simple_event("event_with_bytes", "inv1")
    };
    s.append_event(created.as_ref(), &mut event)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;
    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 1 {
        return Err(format!("Expected 1 event, got {}", snap.events.len()));
    }
    let got_event = &snap.events[0];
    if event.response.content != got_event.response.content {
        t.errorf(format!(
            "Content mismatch: want {:?}, got {:?}",
            event.response.content, got_event.response.content
        ));
    }
    Ok(())
}

async fn append_event_with_existing_events(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let base_time = utc(2025, 1, 1, 0, 0, 0, 0);
    let mut event1 = Event {
        timestamp: base_time + Duration::seconds(1),
        ..simple_event("event1", "inv1")
    };
    s.append_event(created.as_ref(), &mut event1)
        .await
        .map_err(|e| format!("AppendEvent(1) failed: {e}"))?;
    let mut event2 = Event {
        timestamp: base_time + Duration::seconds(2),
        ..simple_event("event2", "inv2")
    };
    s.append_event(created.as_ref(), &mut event2)
        .await
        .map_err(|e| format!("AppendEvent(2) failed: {e}"))?;

    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() failed: {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 2 {
        return Err(format!("Expected 2 events, got {}", snap.events.len()));
    }
    let expected_order = ["inv1", "inv2"];
    if snap
        .events
        .iter()
        .zip(expected_order)
        .any(|(e, want)| e.invocation_id != want)
    {
        t.errorf(format!(
            "Expected event order for {expected_order:?}, got events with InvocationIDs"
        ));
    }
    Ok(())
}

async fn append_event_with_all_fields(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let created = s
        .create(create_req(&app, "user1"))
        .await
        .map_err(|e| format!("Setup: Create failed: {e}"))?;
    let mut event = Event {
        long_running_tool_ids: vec!["tool123".into()],
        actions: EventActions {
            state_delta: state(&[("user:k2", json!("v2"))]),
            ..Default::default()
        },
        response: LlmResponse {
            content: Some(Content::from_text("test_text", "user")),
            turn_complete: true,
            partial: false,
            error_code: "error_code".into(),
            error_message: "error_message".into(),
            interrupted: true,
            grounding_metadata: Some(GroundingMetadata {
                web_search_queries: vec!["query1".into()],
                ..Default::default()
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 1,
                candidates_token_count: 1,
                total_token_count: 2,
                ..Default::default()
            }),
            citation_metadata: Some(CitationMetadata {
                citations: vec![Citation {
                    title: "test".into(),
                    uri: "google.com".into(),
                    ..Default::default()
                }],
            }),
            custom_metadata: [("custom_key".to_string(), json!("custom_value"))]
                .into_iter()
                .collect(),
            ..Default::default()
        },
        ..simple_event("event_complete", "inv1")
    };
    s.append_event(created.as_ref(), &mut event)
        .await
        .map_err(|e| format!("AppendEvent() error = {e}"))?;

    let got = s
        .get(get_req(&app, "user1", created.id()))
        .await
        .map_err(|e| format!("Get() error = {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.events.len() != 1 {
        return Err(format!("Expected 1 event, got {}", snap.events.len()));
    }
    let mut want = event.clone();
    let mut got_event = snap.events[0].clone();
    if opts.provides_server_assigned_event_id {
        // Ignore the event ID, citation metadata and usage metadata when comparing.
        for e in [&mut want, &mut got_event] {
            e.id.clear();
            e.response.citation_metadata = None;
            e.response.usage_metadata = None;
        }
    }
    if want != got_event {
        t.errorf(format!(
            "Event mismatch:\n  want {want:?}\n  got  {got_event:?}"
        ));
    }
    Ok(())
}

// ---- StateManagement ----

fn delta_event(pairs: &[(&str, Value)]) -> Event {
    Event {
        actions: EventActions {
            state_delta: state(pairs),
            ..Default::default()
        },
        ..simple_event("event1", "inv1")
    }
}

async fn state_management_app_state_is_shared(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    // A failed Create ends the case.
    let s1 = s
        .create(CreateRequest {
            state: state(&[("app:k1", json!("v1"))]),
            ..create_req(&app, "u1")
        })
        .await
        .map_err(|e| format!("Create user1 failed: {e}"))?;
    s.append_event(s1.as_ref(), &mut delta_event(&[("app:k2", json!("v2"))]))
        .await
        .map_err(|e| format!("AppendEvent failed: {e}"))?;
    let s2 = s
        .create(create_req(&app, "u2"))
        .await
        .map_err(|e| format!("Create user2 failed: {e}"))?;
    let snap = snapshot(s2.as_ref());
    if value_of(&snap, "app:k1") != Some(json!("v1"))
        || value_of(&snap, "app:k2") != Some(json!("v2"))
    {
        t.errorf(format!("App state not shared, got: {:?}", snap.state));
    }
    Ok(())
}

async fn state_management_user_state_is_user_specific(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let s1 = s
        .create(CreateRequest {
            state: state(&[("user:k1", json!("v1"))]),
            ..create_req(&app, "u1")
        })
        .await
        .map_err(|e| format!("Create u1 failed: {e}"))?;
    s.append_event(s1.as_ref(), &mut delta_event(&[("user:k2", json!("v2"))]))
        .await
        .map_err(|e| format!("AppendEvent failed: {e}"))?;

    let s1b = s
        .create(create_req(&app, "u1"))
        .await
        .map_err(|e| format!("Create u1 (second) failed: {e}"))?;
    let snap1b = snapshot(s1b.as_ref());
    if value_of(&snap1b, "user:k1") != Some(json!("v1"))
        || value_of(&snap1b, "user:k2") != Some(json!("v2"))
    {
        t.errorf(format!(
            "User state missing for same user, got: {:?}",
            snap1b.state
        ));
    }

    let s2 = s
        .create(create_req(&app, "u2"))
        .await
        .map_err(|e| format!("Create u2 failed: {e}"))?;
    let snap2 = snapshot(s2.as_ref());
    if snap2.state.contains_key("user:k1") {
        t.errorf(format!(
            "User state leaked to user2, got: {:?}",
            snap2.state
        ));
    }
    Ok(())
}

async fn state_management_session_state_is_not_shared(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let s1 = s
        .create(CreateRequest {
            state: state(&[("sk1", json!("v1"))]),
            ..create_req(&app, "u1")
        })
        .await
        .map_err(|e| format!("Create u1 failed: {e}"))?;
    s.append_event(s1.as_ref(), &mut delta_event(&[("sk2", json!("v2"))]))
        .await
        .map_err(|e| format!("AppendEvent failed: {e}"))?;
    let s1b = s
        .create(create_req(&app, "u1"))
        .await
        .map_err(|e| format!("Create u1 (second) failed: {e}"))?;
    let snap = snapshot(s1b.as_ref());
    if snap.state.contains_key("sk1") {
        t.errorf(format!(
            "Session state leaked between sessions, got: {:?}",
            snap.state
        ));
    }
    Ok(())
}

async fn state_management_temp_state_is_not_persisted(
    t: &mut T,
    s: Arc<dyn SessionService>,
    opts: &SuiteOptions,
) -> Outcome {
    let app = app_name(opts);
    let s1 = s
        .create(create_req(&app, "u1"))
        .await
        .map_err(|e| format!("Create u1 failed: {e}"))?;
    s.append_event(
        s1.as_ref(),
        &mut delta_event(&[("temp:k1", json!("v1")), ("sk", json!("v2"))]),
    )
    .await
    .map_err(|e| format!("AppendEvent failed: {e}"))?;

    let got = s
        .get(get_req(&app, "u1", s1.id()))
        .await
        .map_err(|e| format!("Get failed: {e}"))?;
    let snap = snapshot(got.as_ref());
    if snap.state.contains_key("temp:k1") {
        t.errorf(format!(
            "Temp state leaked to persist step, got: {:?}",
            snap.state
        ));
    }
    if value_of(&snap, "sk") != Some(json!("v2")) {
        t.errorf(format!(
            "Standard state update missing: got {:?}",
            snap.state
        ));
    }

    // Session state can never hold a temp: key, so the assertions above cannot fail on
    // their own. The leak lives in the persisted event's actions.state_delta.
    if snap.events.len() != 1 {
        return Err(format!(
            "read back {} events, want the 1 appended; the temp: assertions below need it",
            snap.events.len()
        ));
    }
    let stored = snap.events[0]
        .actions
        .state_delta
        .clone()
        .unwrap_or_default();
    for k in stored.keys() {
        if k.starts_with(KEY_PREFIX_TEMP) {
            t.errorf(format!(
                "temp key leaked into the stored event delta: key {k:?}, delta {stored:?}"
            ));
        }
    }
    if stored.get("sk") != Some(&json!("v2")) {
        t.errorf(format!(
            "non-temp key dropped from the stored event delta: got {stored:?}, want \"v2\""
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;

    use super::{SuiteOptions, run_named};
    use crate::{InMemorySessionService, SessionService};

    /// `run_named` hands the factory every case name, in suite order, spelled as the failure
    /// lines spell it; the in-memory backend passes every case.
    #[tokio::test]
    async fn run_named_passes_each_case_name_in_order() {
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::default();
        let record = Arc::clone(&seen);
        run_named(
            SuiteOptions {
                supports_user_provided_session_id: true,
                checks_requested_auth_configs: true,
                ..Default::default()
            },
            move |name| {
                record.lock().push(name);
                async { Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService> }
            },
        )
        .await;
        assert_eq!(
            *seen.lock(),
            vec![
                "Create/full_key",
                "Create/generated_session_id",
                "Create/when_already_exists,_it_fails",
                "Get/ok",
                "Get/error_when_not_found",
                "Get/get_session_respects_user_id",
                "Get/with_config_filters",
                "List",
                "Delete",
                "delete_session_respects_user_id",
                "AppendEvent/when_session_not_found_should_fail",
                "AppendEvent/when_session_deleted_returns_ErrNotFound",
                "AppendEvent/ok",
                "AppendEvent/compaction_record_round_trips",
                "AppendEvent/requested_auth_configs_round_trip",
                "AppendEvent/requested_auth_config_that_does_not_decode_is_refused",
                "AppendEvent/a_hole_still_names_its_event_after_a_round_trip",
                "AppendEvent/a_missing_event_id_is_assigned",
                "AppendEvent/partial_events_are_not_persisted",
                "AppendEvent/with_bytes_content",
                "AppendEvent/with_existing_events",
                "AppendEvent/with_all_fields",
                "StateManagement/app_state_is_shared",
                "StateManagement/user_state_is_user_specific",
                "StateManagement/session_state_is_not_shared",
                "StateManagement/temp_state_is_not_persisted",
            ]
        );
    }
}
