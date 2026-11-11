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

//! R20: `gadk::agentengine` (feature `agentengine`, not on by default) re-exports the Agent
//! Engine launcher constructors and the server handler; a handler built through the façade
//! answers the reasoning-engine API.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use gadk::agent::{AgentConfig, SingleLoader};
use gadk::agentengine::{
    AgentEngineConfig, AgentEngineLauncher, AgentEngineLog, Error, list_class_methods, new_handler,
};
use gadk::launcher::Launcher;
use gadk::platform::GoDuration;
use gadk::session::InMemorySessionService;
use pretty_assertions::assert_eq;

/// Compiles only if each façade path names the `gadk_launcher` / `gadk_server` item.
#[test]
fn facade_agentengine_paths_are_the_crate_items() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<AgentEngineConfig>(None, None::<&gadk_server::agentengine::AgentEngineConfig>);
    same::<Error>(None, None::<&gadk_server::agentengine::Error>);
    same::<AgentEngineLog>(None, None::<&gadk_server::agentengine::AgentEngineLog>);
    same::<AgentEngineLauncher>(
        None,
        None::<&gadk_launcher::web::agentengine::AgentEngineLauncher>,
    );
    let _: fn(&str) -> gadk_launcher::universal::UniversalLauncher =
        gadk::agentengine::new_launcher;
    let _: fn(&str) -> AgentEngineLauncher = gadk::agentengine::new;
}

/// Go `ListClassMethods()`'s six methods, in Go's order.
#[test]
fn facade_agentengine_lists_gos_class_methods() {
    let names: Vec<String> = list_class_methods()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "async_create_session",
            "async_get_session",
            "async_list_sessions",
            "async_delete_session",
            "async_stream_query",
            "streaming_agent_run_with_events",
        ]
    );
}

/// `new_launcher` is Go's universal(web(agentengine)): `web` lists the `agentengine`
/// sub-launcher.
#[test]
fn facade_agentengine_launcher_serves_agentengine_under_web() {
    let syntax = gadk::agentengine::new_launcher("engine").command_line_syntax();
    assert!(
        syntax.starts_with("Arguments: Specify one of the following:\n  * web - "),
        "{syntax}"
    );
    assert!(
        syntax.contains("    * agentengine - starts AgentEngine server which serves reasoning engine API while deployed to Agent Engine\n"),
        "{syntax}"
    );
}

#[tokio::test]
async fn facade_agentengine_handler_creates_a_session() {
    let agent = gadk::agent::new_agent(AgentConfig {
        name: "app".into(),
        ..Default::default()
    })
    .unwrap();
    let cfg = AgentEngineConfig::new(
        Arc::new(InMemorySessionService::new()),
        Arc::new(SingleLoader::new(agent)),
    );
    let handler = new_handler(cfg, GoDuration(0), 1 << 20, "engine").unwrap();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/reasoning_engine")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"class_method":"async_create_session","input":{"user_id":"u"}}"#,
        ))
        .unwrap();
    let resp = handler(req).await;
    assert_eq!(resp.status().as_u16(), 200);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["output"]["user_id"], "u");
    assert_eq!(body["output"]["app_name"], "engine");
}
