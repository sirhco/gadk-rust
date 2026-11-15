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

//! Ports of ADK-Go `server/adkrest/internal/models/session_test.go`.

use chrono::DateTime;
use gadk_platform::Platform;
use gadk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};

use crate::rest::models;

#[tokio::test]
async fn test_from_session_accepts_last_update_in_first_second_of1970() {
    let service = InMemorySessionService::new();
    let created = service
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: Some("s".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut event = gadk_session::Event::new(&Platform::default(), "inv");
    event.author = "user".into();
    event.timestamp = DateTime::from_timestamp(0, 500_000_000).unwrap();
    let want = event.timestamp;
    service
        .append_event(created.as_ref(), &mut event)
        .await
        .unwrap();
    let got = service
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        got.last_update_time(),
        want,
        "LastUpdateTime(); the test no longer sets up the case it pins"
    );
    let mapped = models::Session::from_session(got.as_ref())
        .unwrap_or_else(|e| panic!("FromSession() failed: {e}"));
    assert_eq!(mapped.updated_at, 0, "FromSession().UpdatedAt");
}

// parity: skipped TestSessionStateCredentialsGoOutWithoutSecrets: 8c — sign-in (Go 1523862f #2231: credentials kept in session state reach REST clients without secrets)
