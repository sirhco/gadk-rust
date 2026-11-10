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

//! Ports of ADK-Go `session/vertexai/notfound_test.go`.

use gadk_session::{Event, GetRequest, SessionError, SessionService};
use serde_json::Map;

use super::session_vertexai_delete_ownership::new_fake_service;
use crate::session::VertexSession;

fn get_request(user_id: &str, session_id: &str) -> GetRequest {
    GetRequest {
        app_name: "123".to_owned(),
        user_id: user_id.to_owned(),
        session_id: session_id.to_owned(),
        ..Default::default()
    }
}

fn local(session_id: &str) -> VertexSession {
    VertexSession::new(
        "123".to_owned(),
        "user1".to_owned(),
        session_id.to_owned(),
        Map::new(),
        gadk_session::zero_time(),
    )
}

fn event() -> Event {
    Event {
        id: "e1".to_owned(),
        author: "user".to_owned(),
        invocation_id: "inv1".to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_get_missing_session_wraps_err_not_found() {
    let (s, _fake) = new_fake_service();

    match s.get(get_request("user1", "other")).await {
        Err(SessionError::NotFound) => {}
        Err(err) => panic!("Get(missing session) error = {err}, want SessionError::NotFound"),
        Ok(_) => panic!("Get(missing session) = Ok, want SessionError::NotFound"),
    }
}

#[tokio::test]
async fn test_get_permission_denied_is_not_err_not_found() {
    let (s, _fake) = new_fake_service();

    match s.get(get_request("user1", "denied")).await {
        Err(SessionError::NotFound) => {
            panic!("Get(unreadable session) error = NotFound, want a permission error")
        }
        Err(err) => assert!(
            err.to_string().contains("code = PermissionDenied"),
            "Get(unreadable session) error = {err}, want a permission error"
        ),
        Ok(_) => panic!("Get(unreadable session) = Ok, want a permission error"),
    }
}

// A session that belongs to someone else must be reported as missing, as the in-memory and
// database services report it.
#[tokio::test]
async fn test_get_wrong_user_wraps_err_not_found() {
    let (s, _fake) = new_fake_service();

    match s.get(get_request("user2", "owned")).await {
        Err(SessionError::NotFound) => {}
        Err(err) => panic!("Get(other user's session) error = {err}, want SessionError::NotFound"),
        Ok(_) => panic!("Get(other user's session) = Ok, want SessionError::NotFound"),
    }
}

#[tokio::test]
async fn test_append_event_missing_session_wraps_err_not_found() {
    let (s, fake) = new_fake_service();

    match s.append_event(&local("other"), &mut event()).await {
        Err(SessionError::NotFound) => {}
        Err(err) => {
            panic!("AppendEvent(missing session) error = {err}, want SessionError::NotFound")
        }
        Ok(()) => panic!("AppendEvent(missing session) = Ok, want SessionError::NotFound"),
    }
    assert_eq!(
        fake.hits("AppendEvent"),
        1,
        "AppendEvent RPCs: the error must come from the backend, not from a local short-circuit"
    );
}

#[tokio::test]
async fn test_append_event_permission_denied_is_not_err_not_found() {
    let (s, _fake) = new_fake_service();

    match s.append_event(&local("denied"), &mut event()).await {
        Err(SessionError::NotFound) => {
            panic!("AppendEvent(unwritable session) error = NotFound, want a permission error")
        }
        Err(err) => assert!(
            err.to_string().contains("code = PermissionDenied"),
            "AppendEvent(unwritable session) error = {err}, want a permission error"
        ),
        Ok(()) => panic!("AppendEvent(unwritable session) = Ok, want a permission error"),
    }
}

// The negative control: an append to a session that is there still succeeds.
#[tokio::test]
async fn test_append_event_existing_session_succeeds() {
    let (s, fake) = new_fake_service();

    if let Err(err) = s.append_event(&local("owned"), &mut event()).await {
        panic!("AppendEvent(existing session) error = {err}, want Ok");
    }
    assert_eq!(fake.hits("AppendEvent"), 1, "AppendEvent RPCs");
}
