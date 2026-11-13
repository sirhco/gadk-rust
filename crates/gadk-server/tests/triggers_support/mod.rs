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

//! Fixtures shared by the trigger test targets (Go's `triggers_test` package helpers:
//! `defaultTriggerConfig`, `createMockAgent`, `buildPubSubRequest`, the summarizer stubs).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures::StreamExt;
use gadk_agent::{Agent, AgentConfig, AgentError, Loader, LoaderError, SingleLoader, new_agent};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_server::rest::PathVars;
use gadk_server::rest::testutil::FakeSessionService;
use gadk_server::rest::triggers::{ControllerConfig, TriggerConfig};
use gadk_session::compaction::{SummarizeResult, Summarizer};
use gadk_session::{Event, SessionService};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Go `defaultTriggerConfig`.
pub const DEFAULT_TRIGGER_CONFIG: TriggerConfig = TriggerConfig {
    max_concurrent_runs: 10,
    max_retries: 3,
    base_delay: Duration::from_millis(1),
    max_delay: Duration::from_millis(5),
};

/// What a mock agent saw: how often it ran and the text of the last user message.
#[derive(Default)]
pub struct Runs {
    /// Go `mockAgentRunCount`.
    pub count: AtomicUsize,
    /// Go `userContent.Parts[0].Text` of the last run.
    pub received: Mutex<Option<String>>,
}

impl Runs {
    /// How many times the agent ran.
    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// The last user message text, parsed as JSON.
    pub fn received_json(&self) -> Value {
        let text = self
            .received
            .lock()
            .clone()
            .expect("the agent received no user content");
        serde_json::from_str(&text).expect("the user content is not JSON")
    }
}

/// Go `createMockAgent`: an agent named `test-agent` that records each run and, on its n-th run,
/// fails with `results[n-1]` when that is `Some`, else yields one event `success-event`.
pub fn mock_agent(results: Vec<Option<&'static str>>, runs: Arc<Runs>) -> Arc<dyn Agent> {
    new_agent(AgentConfig {
        name: "test-agent".into(),
        run: Some(Arc::new(move |ctx| {
            let n = runs.count.fetch_add(1, Ordering::SeqCst) + 1;
            *runs.received.lock() = ctx
                .user_content()
                .and_then(|c| c.parts.first())
                .map(|p| p.text.clone());
            let item = match results.get(n - 1).copied().flatten() {
                Some(err) => Err(AgentError::other(err)),
                None => Ok(Event {
                    id: "success-event".into(),
                    ..Default::default()
                }),
            };
            futures::stream::iter([item]).boxed()
        })),
        ..Default::default()
    })
    .unwrap()
}

/// A controller config over `service` and a single-agent loader, with Go's
/// `defaultTriggerConfig` and no compaction.
pub fn controller_config(
    service: Arc<dyn SessionService>,
    loader: Arc<dyn Loader>,
) -> ControllerConfig {
    ControllerConfig {
        session_service: service,
        agent_loader: loader,
        memory_service: None,
        artifact_service: None,
        plugin_config: Default::default(),
        trigger_config: DEFAULT_TRIGGER_CONFIG,
        compaction: None,
        platform: Default::default(),
    }
}

/// [`controller_config`] over a fresh [`FakeSessionService`] serving `agent`.
pub fn fake_config(agent: Arc<dyn Agent>) -> ControllerConfig {
    controller_config(
        Arc::new(FakeSessionService::default()),
        Arc::new(SingleLoader::new(agent)),
    )
}

/// Go's nil `agent.Loader`: lists no agents; its root is a placeholder never run.
pub struct EmptyLoader;

impl Loader for EmptyLoader {
    fn list_agents(&self) -> Vec<String> {
        Vec::new()
    }

    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        Err(LoaderError(format!("agent {name} not found")))
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        new_agent(AgentConfig {
            name: "placeholder".into(),
            ..Default::default()
        })
        .unwrap()
    }
}

/// A loader with no agents whose root panics, like gadk-configurable's conformance loader when
/// every `root_agent.yaml` fails to load (Go returns a nil root there).
pub struct NoRootLoader;

impl Loader for NoRootLoader {
    fn list_agents(&self) -> Vec<String> {
        Vec::new()
    }

    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        Err(LoaderError(format!("agent {name} not found")))
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        panic!("NoRootLoader has no root agent")
    }
}

/// A POST to `target` with `body`, the `app_name` path variable `test-agent` (Go
/// `mux.SetURLVars`) and the given headers.
pub fn post(target: &str, headers: &[(&str, &str)], body: Vec<u8>) -> http::Request<Body> {
    let mut builder = http::Request::post(target);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let mut req = builder.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(PathVars(BTreeMap::from([(
        "app_name".to_owned(),
        "test-agent".to_owned(),
    )])));
    req
}

/// Go's test envelope: `Message.Data` is the base64 text of `data` (so the wire carries it
/// base64-encoded twice), subscription `test-sub`.
pub fn pubsub_body(data: &str, attributes: Option<&[(&str, &str)]>) -> Vec<u8> {
    let mut message = json!({
        "data": STANDARD.encode(STANDARD.encode(data)),
        "messageId": "",
        "publishTime": "",
    });
    if let Some(attrs) = attributes {
        let attrs: serde_json::Map<String, Value> = attrs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
            .collect();
        message["attributes"] = Value::Object(attrs);
    }
    serde_json::to_vec(&json!({"message": message, "subscription": "test-sub"})).unwrap()
}

/// Go `buildPubSubRequest`.
pub fn build_pub_sub_request(data: &str) -> http::Request<Body> {
    post(
        "/apps/test-agent/triggers/pubsub",
        &[],
        pubsub_body(data, None),
    )
}

/// Status and body text of `resp`.
pub async fn read(resp: axum::response::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// Go `stubSummarizer`: summarizes to nothing.
pub struct StubSummarizer;

#[async_trait::async_trait]
impl Summarizer for StubSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(SummarizeResult::default())
    }
}

/// Go `failingSummarizer`: a summarizer outage.
pub struct FailingSummarizer;

#[async_trait::async_trait]
impl Summarizer for FailingSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        Err("summarizer unavailable".into())
    }
}

/// Go `countingSummarizer`: counts calls and answers `a summary`.
#[derive(Default)]
pub struct CountingSummarizer {
    calls: AtomicUsize,
}

impl CountingSummarizer {
    /// Go `count()`.
    pub fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Summarizer for CountingSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SummarizeResult {
            content: Some(Content::from_text("a summary", ROLE_MODEL)),
            ..Default::default()
        })
    }
}
