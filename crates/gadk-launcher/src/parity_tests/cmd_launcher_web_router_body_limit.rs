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

//! Port of ADK-Go `cmd/launcher/web/router_body_limit_test.go`.
//!
//! Go builds `webLauncher` literals with `webConfig{maxPayloadSize}` and the active
//! sub-launchers; here the same state comes from parsing `-max_request_body_size` and the
//! keyword. `WebRouter` has no `Subrouter()`, so the fake trigger registers its full path.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use gadk_server::mux::{Request, Response, handler};
use http::{Method, StatusCode};
use pretty_assertions::assert_eq;

use crate::config::Config;
use crate::error::Error;
use crate::golog;
use crate::launcher::SubLauncher;
use crate::web::{self, WebLauncher, WebRouter, WebSublauncher};

struct NoAgents;

impl gadk_agent::Loader for NoAgents {
    fn list_agents(&self) -> Vec<String> {
        Vec::new()
    }
    fn load_agent(
        &self,
        name: &str,
    ) -> Result<Arc<dyn gadk_agent::Agent>, gadk_agent::LoaderError> {
        Err(gadk_agent::LoaderError(format!("no agent {name}")))
    }
    fn root_agent(&self) -> Arc<dyn gadk_agent::Agent> {
        unimplemented!("the body-limit tests load no agent")
    }
}

/// Go `&launcher.Config{MaxPayloadSize: max}`.
fn config(max_payload_size: i64) -> Config {
    let (log, _) = golog::capture();
    let mut config = Config::new(Arc::new(NoAgents));
    config.log = log;
    config.max_payload_size = max_payload_size;
    config
}

/// Go `fakeTriggerSublauncher`: a trigger endpoint that decodes the request body as JSON, 400
/// when it cannot.
struct FakeTrigger;

impl WebSublauncher for FakeTrigger {
    fn keyword(&self) -> String {
        "faketrigger".to_owned()
    }
    fn parse(&mut self, _args: Vec<String>) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }
    fn command_line_syntax(&self) -> String {
        "faketrigger".to_owned()
    }
    fn simple_description(&self) -> String {
        "fake trigger sublauncher for body-limit test".to_owned()
    }
    fn user_message(&self, _web_url: &str, _printer: &mut dyn FnMut(&str)) {}
    fn setup_subrouters(&self, router: &mut WebRouter, _config: &mut Config) -> Result<(), Error> {
        router.handle(
            &[Method::POST],
            "/api/apps/{app_name}/trigger/pubsub",
            handler(|req: Request| async move {
                let decoded = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                    Ok(body) => serde_json::from_slice::<serde_json::Value>(&body)
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                match decoded {
                    Ok(()) => {
                        let mut resp = Response::new(Body::empty());
                        *resp.status_mut() = StatusCode::NO_CONTENT;
                        resp
                    }
                    Err(e) => {
                        let mut resp =
                            Response::new(Body::from(format!("failed to decode request: {e}\n")));
                        *resp.status_mut() = StatusCode::BAD_REQUEST;
                        resp
                    }
                }
            }),
        );
        Ok(())
    }
}

/// Go `recordingSublauncher`: records the `MaxPayloadSize` it was given.
struct Recording(Arc<Mutex<Option<i64>>>);

impl WebSublauncher for Recording {
    fn keyword(&self) -> String {
        "recording".to_owned()
    }
    fn parse(&mut self, _args: Vec<String>) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }
    fn command_line_syntax(&self) -> String {
        "recording".to_owned()
    }
    fn simple_description(&self) -> String {
        "recording sublauncher".to_owned()
    }
    fn user_message(&self, _web_url: &str, _printer: &mut dyn FnMut(&str)) {}
    fn setup_subrouters(&self, _router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        *self.0.lock().unwrap() = Some(config.max_payload_size);
        Ok(())
    }
}

/// Go's `webLauncher` literal: `sub` active, `-max_request_body_size` `web` (0 = not set).
fn launcher(sub: Box<dyn WebSublauncher>, web: i64) -> WebLauncher {
    let keyword = sub.keyword();
    let mut l = web::new(vec![sub]);
    let mut args = Vec::new();
    if web != 0 {
        args.extend(["-max_request_body_size".to_owned(), web.to_string()]);
    }
    args.push(keyword);
    l.parse(args).unwrap();
    l
}

async fn post(router: &WebRouter, body: String) -> (StatusCode, String) {
    let req = http::Request::builder()
        .method(Method::POST)
        .uri("http://localhost/api/apps/my-app/trigger/pubsub")
        .body(Body::from(body))
        .unwrap();
    let resp = router.serve(req).await;
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn test_build_router_applies_body_limit_middleware_to_sublauncher_trigger_routes() {
    let l = launcher(Box::new(FakeTrigger), 1024);
    let router = l.build_router(&mut config(0)).unwrap();

    // A request body well under the limit decodes successfully.
    let (status, body) = post(&router, r#"{"message":{"data":"small"}}"#.to_owned()).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "small request: {body}");

    // An oversized body trips the limit, so the handler observes a decode error.
    let oversized = format!(r#"{{"message":{{"data":"{}"}}}}"#, "a".repeat(8192));
    let (status, body) = post(&router, oversized).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "oversized request: {body}");
    assert!(
        body.contains("http: request body too large"),
        "oversized request: got body {body:?}"
    );
}

#[test]
fn test_build_router_propagates_max_payload_size_to_sublaunchers() {
    for (name, web, caller, want) in [
        ("raised", 20 << 20, 0, 20 << 20),
        ("default", 0, 0, 0),
        ("caller-set value is preserved", 0, 1 << 20, 1 << 20),
        ("flag wins over caller value", 20 << 20, 1 << 20, 20 << 20),
    ] {
        let seen = Arc::new(Mutex::new(None));
        let l = launcher(Box::new(Recording(Arc::clone(&seen))), web);
        l.build_router(&mut config(caller)).unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            Some(want),
            "{name}: config.MaxPayloadSize"
        );
    }
}

#[tokio::test]
async fn test_build_router_base_router_honors_embedder_payload_limit() {
    const EMBEDDER_LIMIT: i64 = 1 << 20;
    let l = launcher(Box::new(FakeTrigger), 0);
    let router = l.build_router(&mut config(EMBEDDER_LIMIT)).unwrap();

    let (status, body) = post(&router, r#"{"message":{"data":"small"}}"#.to_owned()).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "small request: {body}");

    // Over the embedder's limit but well under the 10 MiB default.
    let oversized = format!(
        r#"{{"message":{{"data":"{}"}}}}"#,
        "a".repeat(EMBEDDER_LIMIT as usize + 1024)
    );
    let (status, body) = post(&router, oversized).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "oversized request: {body}");
    assert!(
        body.contains("http: request body too large"),
        "oversized request: got body {body:?}"
    );
}
