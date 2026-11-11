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

//! Port of ADK-Go `cmd/launcher/web/a2a/a2a_test.go`.
//!
//! Go's `l.Run(t.Context(), config)` is `run_with(…, |_| true)` here: the web launcher's
//! telemetry is installed on no process-global dispatcher, which the crate's other tests share.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use futures::StreamExt as _;
use gadk_a2a::a2a::{self, Message, Part, Role, SendMessageRequest, SendMessageResponse};
use gadk_a2a::a2a_client::A2AClientFactory;
use gadk_a2a::a2a_client::agent_card::AgentCardResolver;
use gadk_a2a::a2a_client::jsonrpc::JsonRpcTransportFactory;
use gadk_agent::{AgentConfig, SingleLoader, new_agent};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_session::{Event, InMemorySessionService};
use http::{Method, StatusCode};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::launcher::SubLauncher;
use crate::web::{self, WebRouter, WebSublauncher, a2a as a2a_launcher, api};

/// Go `getFreePort`.
fn get_free_port() -> u16 {
    std::net::TcpListener::bind("localhost:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// The web launcher serves A2A v1 to an A2A client resolving the card from the launcher's
/// address and sending a message.
// parity: adapted: Go's "A2A v0 client" subtest is not run: the A2A 0.3 endpoint is not served (plan D13, ledger 6)
#[tokio::test(flavor = "multi_thread")]
async fn test_web_launcher_serves_a2_a() {
    let port = get_free_port();

    let mut l = web::new(vec![Box::new(a2a_launcher::new())]);
    SubLauncher::parse(
        &mut l,
        strings(&[
            "--port",
            &port.to_string(),
            "a2a",
            "--a2a_agent_url",
            &format!("http://localhost:{port}"),
        ]),
    )
    .unwrap();

    let want_message = "Hello, world!";
    let agnt = new_agent(AgentConfig {
        name: "HelloWorldAgent".into(),
        run: Some(Arc::new(move |ic| {
            let mut event = Event::new(ic.platform(), ic.invocation_id());
            event.response.content = Some(Content::from_text(want_message, ROLE_MODEL));
            futures::stream::iter([Ok(event)]).boxed()
        })),
        ..Default::default()
    })
    .unwrap();
    let mut config = Config::new(Arc::new(SingleLoader::new(agnt)));
    config.session_service = Some(Arc::new(InMemorySessionService::new()));
    // Go's log goes to the test output; here it is captured to keep the output clean.
    config.log = crate::golog::capture().0;

    let cancel = CancellationToken::new();
    let running = {
        let cancel = cancel.clone();
        tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
    };

    // "A2A v2 client".
    let resolver = AgentCardResolver::new(None);
    let mut card = None;
    for retry in 0..3 {
        // Give the server time to start (Go sleeps 10ms; the build here is slower).
        tokio::time::sleep(Duration::from_millis(100)).await;
        match resolver.resolve(&format!("http://localhost:{port}")).await {
            Ok(c) => {
                card = Some(c);
                break;
            }
            Err(e) if retry == 2 => panic!("cardResolver.Resolve() error = {e}"),
            Err(_) => {}
        }
    }
    let card = card.unwrap();

    let client = A2AClientFactory::builder()
        .register(Arc::new(JsonRpcTransportFactory::new(None)))
        .build()
        .create_from_card(&card)
        .await
        .unwrap();
    let got = client
        .send_message(&SendMessageRequest {
            message: Message::new(Role::User, vec![Part::text("Hi!")]),
            configuration: None,
            metadata: None,
            tenant: None,
        })
        .await
        .unwrap();
    let SendMessageResponse::Task(task) = got else {
        panic!("client.SendMessage() result type = {got:?}, want a2a.Task");
    };
    let artifacts: Vec<a2a::Artifact> = task.artifacts.unwrap_or_default();
    assert_eq!(artifacts.len(), 1, "len(task.Artifacts)");
    let parts = &artifacts[0].parts;
    assert_eq!(parts.len(), 1, "len(task.Artifacts[0].Parts)");
    assert_eq!(
        parts[0].as_text(),
        Some(want_message),
        "task.Artifacts[0].Parts[0]"
    );

    cancel.cancel();
    running.await.unwrap().unwrap();
}

/// Go `routerCaptureSublauncher`: records the root router the web launcher built so a test can
/// send requests through the assembled middleware and sub-launchers.
#[derive(Clone, Default)]
struct RouterCaptureSublauncher {
    router: Arc<Mutex<Option<WebRouter>>>,
}

impl WebSublauncher for RouterCaptureSublauncher {
    fn keyword(&self) -> String {
        "capture".to_owned()
    }
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        Ok(args)
    }
    fn command_line_syntax(&self) -> String {
        String::new()
    }
    fn simple_description(&self) -> String {
        String::new()
    }
    fn user_message(&self, _web_url: &str, _printer: &mut dyn FnMut(&str)) {}
    fn setup_subrouters(&self, router: &mut WebRouter, _config: &mut Config) -> Result<(), Error> {
        *self.router.lock().unwrap() = Some(router.clone());
        Ok(())
    }
}

/// `-a2a_agent_url` (left at its `http://localhost:8080` default or set explicitly) does not
/// widen which origins `/api` admits, and what `/api` admits does not depend on whether a2a is
/// set up before api (as in `full::new()`) or after it (as in `prod::new()`).
#[tokio::test]
async fn test_a2_a_agent_url_does_not_widen_api_origins() {
    let agnt = new_agent(AgentConfig {
        name: "HelloWorldAgent".into(),
        ..Default::default()
    })
    .unwrap();

    type Build = fn(
        Box<dyn WebSublauncher>,
        Box<dyn WebSublauncher>,
        Box<dyn WebSublauncher>,
    ) -> web::WebLauncher;
    type Args = fn(&[&str]) -> Vec<String>;
    let orders: [(&str, Build, Args); 2] = [
        (
            "a2a before api",
            |a2a_sub, api_sub, cap_sub| web::new(vec![a2a_sub, api_sub, cap_sub]),
            |a2a_args| {
                let mut out = strings(&["-allow_origins", "https://allowed.example.com", "a2a"]);
                out.extend(strings(a2a_args));
                out.extend(strings(&[
                    "api",
                    "-webui_address",
                    "localhost:9000",
                    "capture",
                ]));
                out
            },
        ),
        (
            "api before a2a",
            |a2a_sub, api_sub, cap_sub| web::new(vec![api_sub, a2a_sub, cap_sub]),
            |a2a_args| {
                let mut out = strings(&[
                    "-allow_origins",
                    "https://allowed.example.com",
                    "api",
                    "-webui_address",
                    "localhost:9000",
                    "a2a",
                ]);
                out.extend(strings(a2a_args));
                out.push("capture".to_owned());
                out
            },
        ),
    ];
    struct Case {
        name: &'static str,
        a2a_args: &'static [&'static str],
        origin: &'static str,
        want_status: StatusCode,
        want_cors: &'static str,
    }
    let cases = [
        Case {
            name: "default a2a_agent_url is refused on /api",
            a2a_args: &[],
            origin: "http://localhost:8080",
            want_status: StatusCode::FORBIDDEN,
            want_cors: "",
        },
        Case {
            name: "explicit a2a_agent_url is refused on /api",
            a2a_args: &["-a2a_agent_url", "https://agent.example.com"],
            origin: "https://agent.example.com",
            want_status: StatusCode::FORBIDDEN,
            want_cors: "",
        },
        Case {
            name: "configured webui_address is served on /api",
            a2a_args: &[],
            origin: "http://localhost:9000",
            want_status: StatusCode::OK,
            want_cors: "http://localhost:9000",
        },
        Case {
            name: "configured allow_origins is served on /api",
            a2a_args: &[],
            origin: "https://allowed.example.com",
            want_status: StatusCode::OK,
            want_cors: "https://allowed.example.com",
        },
    ];

    for (order, build, order_args) in orders {
        for tc in &cases {
            // The port stays held, so `Run` fails to bind after the routes are set up.
            let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = ln.local_addr().unwrap().port();

            let cap_sub = RouterCaptureSublauncher::default();
            let mut l = build(
                Box::new(a2a_launcher::new()),
                Box::new(api::new()),
                Box::new(cap_sub.clone()),
            );
            let mut args = strings(&["-port", &port.to_string()]);
            args.extend(order_args(tc.a2a_args));
            SubLauncher::parse(&mut l, args.clone())
                .unwrap_or_else(|e| panic!("{order}/{}: Parse({args:?}) error = {e}", tc.name));
            let mut cfg = Config::new(Arc::new(SingleLoader::new(Arc::clone(&agnt))));
            cfg.session_service = Some(Arc::new(InMemorySessionService::new()));
            cfg.log = crate::golog::capture().0;
            assert!(
                l.run_with(CancellationToken::new(), cfg, |_| true)
                    .await
                    .is_err(),
                "{order}/{}: Run() succeeded, want server bind failure",
                tc.name
            );
            let router = cap_sub.router.lock().unwrap().clone().unwrap_or_else(|| {
                panic!(
                    "{order}/{}: Run() returned before SetupSubrouters captured the router",
                    tc.name
                )
            });

            let req = http::Request::builder()
                .method(Method::GET)
                .uri("/api/list-apps")
                .header("Host", "127.0.0.1:9000")
                .header("Origin", tc.origin)
                .body(Body::empty())
                .unwrap();
            let resp = router.serve(req).await;
            assert_eq!(
                resp.status(),
                tc.want_status,
                "{order}/{}: GET /api/list-apps with Origin {:?}",
                tc.name,
                tc.origin
            );
            let cors = resp
                .headers()
                .get("Access-Control-Allow-Origin")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            assert_eq!(
                cors, tc.want_cors,
                "{order}/{}: GET /api/list-apps with Origin {:?}: Access-Control-Allow-Origin",
                tc.name, tc.origin
            );
            drop(ln);
        }
    }
}
