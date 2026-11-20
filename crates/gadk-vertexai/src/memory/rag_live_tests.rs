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

//! Live round trip of [`VertexAiRagMemoryService`] against a real RAG corpus. Set
//! `GADK_TEST_VERTEX_RAG_CORPUS` to a corpus resource name
//! (`projects/{project}/locations/us-central1/ragCorpora/{id}`) that Application Default
//! Credentials can use, and run with `--ignored`:
//!
//! `GADK_TEST_VERTEX_RAG_CORPUS=projects/my-project/locations/us-central1/ragCorpora/123
//! cargo test -p gadk-vertexai memory::rag_live_tests -- --ignored --nocapture`
//!
//! The test works as a random user and deletes the RAG files it created, even when an
//! assertion fails. It does not delete the corpus or the project's RAG database; see the
//! README's "Cost and teardown".

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use chrono::Utc;
use futures::FutureExt as _;
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{AddEventsRequest, MemoryService, SearchRequest, add_events_to_memory};
use gadk_model::LlmResponse;
use gadk_platform::env::ProcessEnv;
use gadk_session::Event;
use http::Method;
use serde_json::{Map, Value};
use tokio::time::Instant;

use super::rag::{VertexAiRagMemoryConfig, VertexAiRagMemoryService};
use crate::client::VertexClient;
use crate::config::{host_url, resolve};
use crate::error::BoxError;
use crate::session::VertexSession;

const APP: &str = "gadk-live";

/// A client for the corpus's region with Application Default Credentials.
fn admin_client(corpus: &str) -> VertexClient {
    let location = corpus.split('/').nth(3).unwrap_or_default();
    let resolved = resolve(
        None,
        &format!("https://{}", host_url(location)),
        None,
        false,
        None,
        &ProcessEnv,
    )
    .unwrap();
    VertexClient::new(resolved)
}

/// The resource names of the corpus's files with display name `display`.
async fn files_named(
    client: &VertexClient,
    corpus: &str,
    display: &str,
) -> Result<Vec<String>, BoxError> {
    Ok(client
        .list_all(
            &format!("/v1/{corpus}/ragFiles"),
            &[],
            ("parent", corpus),
            "ragFiles",
        )
        .await?
        .iter()
        .filter(|f| f.get("displayName").and_then(Value::as_str) == Some(display))
        .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

/// Deletes this test's files with display name `display`, waiting for each delete. Returns
/// whether every step succeeded; each failure is logged with what was left behind.
async fn cleanup(client: &VertexClient, corpus: &str, display: &str) -> bool {
    let names = match files_named(client, corpus, display).await {
        Ok(names) => names,
        Err(e) => {
            eprintln!("cleanup: listing files failed ({e}); files named {display} may be left");
            return false;
        }
    };
    let mut ok = true;
    for name in names {
        let deleted = match client
            .call(
                Method::DELETE,
                &format!("/v1/{name}"),
                &[],
                ("name", &name),
                None,
            )
            .await
        {
            Ok(op) => client.wait_operation_at(op, "/v1").await.map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = deleted {
            eprintln!("cleanup: deleting {name} failed ({e}); the file may be left");
            ok = false;
        }
    }
    ok
}

#[tokio::test]
#[ignore = "needs GADK_TEST_VERTEX_RAG_CORPUS and Application Default Credentials"]
async fn rag_memory_round_trip() {
    let Some(corpus) = std::env::var("GADK_TEST_VERTEX_RAG_CORPUS")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        println!("skipping rag_memory_round_trip: GADK_TEST_VERTEX_RAG_CORPUS is unset");
        return;
    };
    let user = format!("u{:016x}", rand::random::<u64>());
    let display = format!("{APP}.{user}.s1");
    let code_word = format!("zephyr{:08x}", rand::random::<u32>());
    let svc = VertexAiRagMemoryService::new(VertexAiRagMemoryConfig::new(corpus.clone())).unwrap();
    let client = admin_client(&corpus);

    let outcome = AssertUnwindSafe(round_trip(
        &svc, &client, &corpus, &user, &display, &code_word,
    ))
    .catch_unwind()
    .await;
    // Delete this test's file whatever happened; the round trip's own panic wins.
    let cleaned = cleanup(&client, &corpus, &display).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(
        cleaned,
        "cleanup failed: see the logged files left in {corpus}"
    );
}

/// A user event with `text`, timestamped now.
fn text_event(text: &str) -> Event {
    Event {
        author: "user".into(),
        timestamp: Utc::now(),
        response: LlmResponse {
            content: Some(Content::from_text(text, ROLE_USER)),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Searches every 5 s for at most 120 s until a memory's text contains `needle`; a failed
/// search is logged and retried.
async fn search_until_found(svc: &VertexAiRagMemoryService, user: &str, query: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let found = svc
            .search_memory(SearchRequest {
                query: query.into(),
                user_id: user.into(),
                app_name: APP.into(),
            })
            .await;
        match found {
            Ok(found) => {
                let hit = found.memories.iter().any(|m| {
                    m.content
                        .as_ref()
                        .is_some_and(|c| c.parts.iter().any(|p| p.text.contains(needle)))
                });
                if hit {
                    return;
                }
            }
            Err(e) => eprintln!("search failed ({e}); retrying"),
        }
        assert!(
            Instant::now() < deadline,
            "{needle} was not retrieved within 120 s"
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Adds the session twice (the second add replaces the file) and finds the code word; adds a
/// batch with a second fact (one more file with the session's name) and finds it; then adds
/// the session again, which replaces the session file and the batch with one file.
async fn round_trip(
    svc: &VertexAiRagMemoryService,
    client: &VertexClient,
    corpus: &str,
    user: &str,
    display: &str,
    code_word: &str,
) {
    let session = VertexSession::new(APP.into(), user.into(), "s1".into(), Map::new(), Utc::now());
    session.set_events(vec![text_event(&format!(
        "My secret code word is {code_word}."
    ))]);
    svc.add_session_to_memory(&session).await.unwrap();
    svc.add_session_to_memory(&session).await.unwrap();
    assert_eq!(files_named(client, corpus, display).await.unwrap().len(), 1);
    search_until_found(svc, user, "What is my secret code word?", code_word).await;

    let color = format!("vermilion{:08x}", rand::random::<u32>());
    add_events_to_memory(
        svc,
        AddEventsRequest {
            app_name: APP.into(),
            user_id: user.into(),
            session_id: "s1".into(),
            events: vec![text_event(&format!("My favorite color is {color}."))],
            custom_metadata: Map::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(files_named(client, corpus, display).await.unwrap().len(), 2);
    search_until_found(svc, user, "What is my favorite color?", &color).await;

    svc.add_session_to_memory(&session).await.unwrap();
    assert_eq!(files_named(client, corpus, display).await.unwrap().len(), 1);
}
