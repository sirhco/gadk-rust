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

//! Live round trip against a real Agent Engine: set `GADK_TEST_VERTEX_PROJECT`,
//! `GADK_TEST_VERTEX_LOCATION` and `GADK_TEST_VERTEX_ENGINE` (the reasoning engine's numeric
//! ID) to an engine the Application Default Credentials can use
//! (`gcloud auth application-default login`). The test works as a random user and deletes the
//! session it created, even when an assertion fails; the memories it generates stay in the
//! Memory Bank under that user's scope (the memory service has no delete). With any variable
//! unset it is skipped.
//!
//! `GADK_TEST_VERTEX_PROJECT=my-project GADK_TEST_VERTEX_LOCATION=us-central1
//! GADK_TEST_VERTEX_ENGINE=1234567890 cargo test -p gadk-vertexai --test live_vertex -- --nocapture`

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt as _;
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{MemoryService, SearchRequest};
use gadk_platform::Platform;
use gadk_platform::env::MapEnv;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, SessionError, SessionService,
};
use gadk_vertexai::AgentEngineData;
use gadk_vertexai::memory::{VertexAiMemoryService, VertexAiMemoryServiceConfig};
use gadk_vertexai::session::{VertexAiSessionService, VertexAiSessionServiceConfig};
use serde_json::json;

const APP: &str = "gadk-live";

/// The engine named by the three variables, or `None` (after a skip line) when any is unset.
fn engine(test: &str) -> Option<AgentEngineData> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let (Some(project_id), Some(location), Some(reasoning_engine)) = (
        var("GADK_TEST_VERTEX_PROJECT"),
        var("GADK_TEST_VERTEX_LOCATION"),
        var("GADK_TEST_VERTEX_ENGINE"),
    ) else {
        println!(
            "skipping {test}: GADK_TEST_VERTEX_PROJECT, GADK_TEST_VERTEX_LOCATION and \
             GADK_TEST_VERTEX_ENGINE must all be set"
        );
        return None;
    };
    Some(AgentEngineData {
        project_id,
        location,
        reasoning_engine,
    })
}

#[tokio::test]
async fn live_vertex_session_and_memory_round_trip() {
    let Some(engine) = engine("live_vertex_session_and_memory_round_trip") else {
        return;
    };
    // An empty injected environment: credentials from google-cloud-auth's own Application
    // Default Credentials lookup.
    let sessions = VertexAiSessionService::new(VertexAiSessionServiceConfig {
        project_id: engine.project_id.clone(),
        location: engine.location.clone(),
        reasoning_engine: Some(engine.reasoning_engine.clone()),
        env: Arc::new(MapEnv::default()),
        ..Default::default()
    })
    .unwrap();
    let memory = VertexAiMemoryService::new(VertexAiMemoryServiceConfig {
        engine,
        wait_for_completion: true,
        env: Arc::new(MapEnv::default()),
        ..Default::default()
    })
    .unwrap();
    let user = format!("gadk-live-{:016x}", rand::random::<u64>());

    let created = sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: user.clone(),
            session_id: None,
            state: json!({"sk": "v0"}).as_object().cloned(),
        })
        .await
        .unwrap();
    let session_id = created.id().to_owned();
    assert!(!session_id.is_empty(), "the engine assigns the session ID");
    assert_eq!(created.state().get("sk").unwrap(), json!("v0"));

    let outcome = AssertUnwindSafe(round_trip(&sessions, &memory, &user, &session_id))
        .catch_unwind()
        .await;
    // Delete the session whatever happened (a missing session is a no-op).
    let cleanup = sessions
        .delete(DeleteRequest {
            app_name: APP.into(),
            user_id: user.clone(),
            session_id: session_id.clone(),
        })
        .await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    cleanup.unwrap();
}

/// Append → get → list → generate and retrieve memories → delete → get NotFound.
async fn round_trip(
    sessions: &VertexAiSessionService,
    memory: &VertexAiMemoryService,
    user: &str,
    session_id: &str,
) {
    let get = || GetRequest {
        app_name: APP.into(),
        user_id: user.into(),
        session_id: session_id.into(),
        ..Default::default()
    };
    let session = sessions.get(get()).await.unwrap();

    let platform = Platform::default();
    let mut event = Event::new(&platform, "inv-live");
    event.author = "user".into();
    event.response.content = Some(Content::from_text(
        "My favorite color is teal and I live in Lisbon.",
        ROLE_USER,
    ));
    event.actions.state_delta = json!({
        "sk": "v1",
        "app:ak": "app value",
        "user:uk": 7,
        "temp:tk": "dropped before persisting",
    })
    .as_object()
    .cloned();
    sessions
        .append_event(session.as_ref(), &mut event)
        .await
        .unwrap();

    let got = sessions.get(get()).await.unwrap();
    assert_eq!(got.id(), session_id);
    assert_eq!(got.user_id(), user);
    let events = got.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].id, event.id);
    assert_eq!(events[0].author, "user");
    assert_eq!(
        events[0].response.content.as_ref().unwrap().parts[0].text,
        "My favorite color is teal and I live in Lisbon."
    );
    let state = got.state();
    assert_eq!(state.get("sk").unwrap(), json!("v1"));
    assert_eq!(state.get("app:ak").unwrap(), json!("app value"));
    assert_eq!(state.get("user:uk").unwrap(), json!(7));
    assert!(
        state.get("temp:tk").is_err(),
        "temp: keys must not be persisted"
    );

    let listed = sessions
        .list(ListRequest {
            app_name: APP.into(),
            user_id: user.into(),
        })
        .await
        .unwrap();
    let ids: Vec<&str> = listed.iter().map(|s| s.id()).collect();
    assert_eq!(
        ids,
        [session_id],
        "the user filter lists only this user's session"
    );

    memory.add_session_to_memory(got.as_ref()).await.unwrap();
    let found = memory
        .search_memory(SearchRequest {
            query: "What is my favorite color?".into(),
            user_id: user.into(),
            app_name: APP.into(),
        })
        .await
        .unwrap();
    // Memory generation is model-driven; an empty result is not a failure.
    println!("retrieved {} memories", found.memories.len());

    sessions
        .delete(DeleteRequest {
            app_name: APP.into(),
            user_id: user.into(),
            session_id: session_id.into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        sessions.get(get()).await,
        Err(SessionError::NotFound)
    ));
}
