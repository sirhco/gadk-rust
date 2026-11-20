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
//! The test works as a random user and deletes the RAG file it created, even when an
//! assertion fails. It does not delete the corpus or the project's RAG database; see the
//! README's "Cost and teardown".

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use chrono::Utc;
use futures::FutureExt as _;
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{MemoryService, SearchRequest};
use gadk_model::LlmResponse;
use gadk_platform::env::ProcessEnv;
use gadk_session::Event;
use http::Method;
use serde_json::{Map, Value};
use tokio::time::Instant;

use super::rag::{VertexAiRagMemoryConfig, VertexAiRagMemoryService};
use crate::client::VertexClient;
use crate::config::{host_url, resolve};
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
async fn files_named(client: &VertexClient, corpus: &str, display: &str) -> Vec<String> {
    client
        .list_all(
            &format!("/v1/{corpus}/ragFiles"),
            &[],
            ("parent", corpus),
            "ragFiles",
        )
        .await
        .unwrap()
        .iter()
        .filter(|f| f.get("displayName").and_then(Value::as_str) == Some(display))
        .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect()
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
    // Delete this test's file whatever happened.
    for name in files_named(&client, &corpus, &display).await {
        let op = client
            .call(
                Method::DELETE,
                &format!("/v1/{name}"),
                &[],
                ("name", &name),
                None,
            )
            .await
            .unwrap();
        client.wait_operation(op).await.unwrap();
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// Adds the session twice (the second add replaces the file), checks one file exists, then
/// searches every 5 s for at most 120 s until the code word comes back.
async fn round_trip(
    svc: &VertexAiRagMemoryService,
    client: &VertexClient,
    corpus: &str,
    user: &str,
    display: &str,
    code_word: &str,
) {
    let session = VertexSession::new(APP.into(), user.into(), "s1".into(), Map::new(), Utc::now());
    session.set_events(vec![Event {
        author: "user".into(),
        timestamp: Utc::now(),
        response: LlmResponse {
            content: Some(Content::from_text(
                format!("My secret code word is {code_word}."),
                ROLE_USER,
            )),
            ..Default::default()
        },
        ..Default::default()
    }]);
    svc.add_session_to_memory(&session).await.unwrap();
    svc.add_session_to_memory(&session).await.unwrap();
    assert_eq!(files_named(client, corpus, display).await.len(), 1);

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let found = svc
            .search_memory(SearchRequest {
                query: "What is my secret code word?".into(),
                user_id: user.into(),
                app_name: APP.into(),
            })
            .await
            .unwrap();
        let hit = found.memories.iter().any(|m| {
            m.content
                .as_ref()
                .is_some_and(|c| c.parts.iter().any(|p| p.text.contains(code_word)))
        });
        if hit {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the code word was not retrieved within 120 s"
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
