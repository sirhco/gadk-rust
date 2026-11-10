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

//! Ports of ADK-Go `session/vertexai/delete_ownership_test.go`.

use gadk_session::{DeleteRequest, SessionService};

use crate::session::{VertexAiSessionService, VertexAiSessionServiceConfig};
use crate::testutil::{FakeAgentEngine, session_config};

/// Go `newFakeService`: a service on a [`FakeAgentEngine`] (Go's in-process `fakeSessions`:
/// session `owned` of `user1`, `denied` refused, others missing), project `p`, location
/// `us-central1`, reasoning engine `123`.
pub(super) fn new_fake_service() -> (VertexAiSessionService, FakeAgentEngine) {
    let fake = FakeAgentEngine::start();
    let config = VertexAiSessionServiceConfig {
        project_id: "p".to_owned(),
        location: "us-central1".to_owned(),
        reasoning_engine: Some("123".to_owned()),
        ..session_config(&fake.url())
    };
    (VertexAiSessionService::new(config).unwrap(), fake)
}

fn delete_request(user_id: &str, session_id: &str) -> DeleteRequest {
    DeleteRequest {
        app_name: "123".to_owned(),
        user_id: user_id.to_owned(),
        session_id: session_id.to_owned(),
    }
}

// A delete requested by a non-owner must not reach the backend's DeleteSession. It is a
// no-op, the same as deleting a missing session.
#[tokio::test]
async fn test_delete_wrong_user_is_no_op() {
    let (s, fake) = new_fake_service();

    if let Err(err) = s.delete(delete_request("user2", "owned")).await {
        panic!("cross-user Delete: got {err}, want Ok");
    }
    assert_eq!(fake.hits("DeleteSession"), 0, "DeleteSession RPCs");
}

// The owner can still delete, so a guard that rejects everything can't pass.
#[tokio::test]
async fn test_delete_owner_deletes() {
    let (s, fake) = new_fake_service();

    if let Err(err) = s.delete(delete_request("user1", "owned")).await {
        panic!("owner Delete: got {err}, want Ok");
    }
    assert_eq!(fake.hits("DeleteSession"), 1, "DeleteSession RPCs");
}

// Deleting a session that does not exist is a no-op (no error, no delete RPC).
#[tokio::test]
async fn test_delete_missing_session_is_no_op() {
    let (s, fake) = new_fake_service();

    if let Err(err) = s.delete(delete_request("user1", "other")).await {
        panic!("missing-session Delete: got {err}, want Ok");
    }
    assert_eq!(fake.hits("DeleteSession"), 0, "DeleteSession RPCs");
}
