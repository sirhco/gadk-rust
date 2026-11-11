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

//! Port of ADK-Go `cmd/launcher/web/a2a`: the sub-launcher that serves the root agent over A2A
//! JSON-RPC v1 at `/a2a/v1/invoke`, with its agent card at `/.well-known/agent-card.json`.
//!
//! Go also serves A2A 0.3 at `/a2a/invoke` and publishes a card that is the union of the v1 and
//! 0.3 shapes; a2a-rs implements v1 only, so neither is served here (parity ledger 6).

use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use gadk_a2a::a2a::{
    self, A2AError, AgentCapabilities, AgentCard, AgentExtension, AgentInterface,
    CancelTaskRequest, DeleteTaskPushNotificationConfigRequest, GetExtendedAgentCardRequest,
    GetTaskPushNotificationConfigRequest, GetTaskRequest, ListTaskPushNotificationConfigsRequest,
    ListTaskPushNotificationConfigsResponse, ListTasksRequest, ListTasksResponse,
    SendMessageRequest, SendMessageResponse, StreamResponse, SubscribeToTaskRequest, Task,
    TaskPushNotificationConfig,
};
use gadk_a2a::a2a_server::agent_card::agent_card_router;
use gadk_a2a::a2a_server::jsonrpc::jsonrpc_router;
use gadk_a2a::a2a_server::{
    DefaultRequestHandler, InMemoryTaskStore, RequestHandler, ServiceParams, StaticAgentCard,
    WELL_KNOWN_AGENT_CARD_PATH,
};
use gadk_a2a::convert::ADK_EXTENSION_URI;
use gadk_a2a::server::{Executor, ExecutorConfig, build_agent_skills};
use gadk_agent::Agent;
use gadk_engine::runner::RunnerConfig;
use gadk_platform::Platform;
use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_platform::go_url;
use gadk_session::{InMemorySessionService, SessionService};

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::web::{WebRouter, WebSublauncher, axum_handler};

/// Go `apiPath`: the suffix of the A2A v1 invocation URL.
const API_PATH: &str = "/a2a/v1/invoke";

/// Go `a2aLauncher`.
pub struct A2ALauncher {
    /// Go `flags` (behind a mutex: `WebSublauncher` is `Sync`).
    flags: Mutex<FlagSet>,
    /// Go `config.agentURL`: the A2A host URL the agent card advertises.
    agent_url: Flag<String>,
}

/// Go `a2a.NewLauncher()`.
pub fn new() -> A2ALauncher {
    let mut fs = FlagSet::new("a2a");
    let agent_url = fs.string(
        "a2a_agent_url",
        "http://localhost:8080",
        "A2A host URL as advertised in the public agent card. It is used by A2A clients as a connection endpoint.",
    );
    A2ALauncher {
        flags: Mutex::new(fs),
        agent_url,
    }
}

impl A2ALauncher {
    /// Replaces where the `a2a` flag set writes its errors and usage (Go `fs.SetOutput`; default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> A2ALauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// The JSON-RPC v1 router over the request handler `config` makes of the default one (Go
    /// `a2asrv.NewHandler(executor, config.A2AOptions...)`): every option in order, then every
    /// wrapper in order; without wrappers the configured default handler is served directly.
    fn jsonrpc_routes(config: &Config, default: DefaultRequestHandler) -> axum::Router {
        let configured = config
            .a2a_options
            .iter()
            .fold(default, |handler, option| option(handler));
        let routes = if config.a2a_wrappers.is_empty() {
            jsonrpc_router(Arc::new(configured))
        } else {
            let wrapped = config.a2a_wrappers.iter().fold(
                Arc::new(configured) as Arc<dyn RequestHandler>,
                |handler, wrapper| wrapper(handler),
            );
            jsonrpc_router(Arc::new(DynHandler(wrapped)))
        };
        axum::Router::new().nest(API_PATH, routes)
    }
}

impl WebSublauncher for A2ALauncher {
    /// Go `Keyword()`.
    fn keyword(&self) -> String {
        "a2a".to_owned()
    }

    /// Go `Parse`.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let fs = self.flags.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = fs.parse(&args) {
            return Err(Error::Parse(format!("failed to parse a2a flags: {e}")));
        }
        Ok(fs.args())
    }

    /// Go `CommandLineSyntax()`.
    fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    /// Go `SimpleDescription()`.
    fn simple_description(&self) -> String {
        format!("starts A2A server which handles jsonrpc requests on {API_PATH} path")
    }

    /// Go `SetupSubrouters`: refuses a compaction config this surface cannot serve, then mounts
    /// the agent card and the JSON-RPC v1 endpoint (no authentication).
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        let root_agent = config.agent_loader.root_agent();
        // The web launcher fills in the services before it builds its router; called on its own,
        // an unset session service gets an in-memory one (Go passes nil and fails each request).
        let session_service: Arc<dyn SessionService> = match &config.session_service {
            Some(s) => Arc::clone(s),
            None => Arc::new(InMemorySessionService::new()),
        };
        let runner_config = RunnerConfig {
            app_name: root_agent.name().to_owned(),
            agent: Arc::clone(&root_agent),
            session_service,
            artifact_service: config.artifact_service.clone(),
            memory_service: config.memory_service.clone(),
            plugin_config: config.plugin_config.clone(),
            auto_create_session: false,
            platform: Platform::default(),
            compaction: config.compaction.clone(),
        };

        // Go's dry run: a compaction config can pass its own shape check and still be
        // unservable, and without this the process starts healthy and then fails every request.
        gadk_engine::compactionvalidate::against_root_agent(
            config.compaction.as_ref(),
            Some(Arc::clone(&root_agent)),
            &runner_config,
        )
        .map_err(|e| Error::Config(e.to_string()))?;

        // Go also joins `/a2a/invoke` (0.3) here; a2a-rs serves v1 only (parity ledger 6).
        let public_url = join_url(&self.agent_url.get(), API_PATH)?;
        let card = agent_card(root_agent.as_ref(), public_url);
        router.handle(
            &[],
            WELL_KNOWN_AGENT_CARD_PATH,
            axum_handler(agent_card_router(Arc::new(StaticAgentCard::new(card)))),
        );

        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(runner_config),
            ..Default::default()
        });
        let default = DefaultRequestHandler::new(executor, InMemoryTaskStore::new());
        router.handle(
            &[],
            API_PATH,
            axum_handler(Self::jsonrpc_routes(config, default)),
        );
        Ok(())
    }

    /// Go `UserMessage`.
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        printer(&format!(
            "       a2a:  you can access A2A using jsonrpc protocol: {web_url}"
        ));
    }
}

/// Go `url.JoinPath(base, elem)`.
fn join_url(base: &str, elem: &str) -> Result<String, Error> {
    let base = go_url::parse(base).map_err(|e| Error::Config(e.to_string()))?;
    Ok(base
        .join_path(&[elem])
        .map_err(Error::Config)?
        .to_go_string())
}

/// The card Go's `SetupSubrouters` builds, without its 0.3 interface (parity ledger 6).
fn agent_card(root_agent: &dyn Agent, public_url: String) -> AgentCard {
    AgentCard {
        name: root_agent.name().to_owned(),
        description: root_agent.description().to_owned(),
        version: "2.0.0".to_owned(),
        supported_interfaces: vec![AgentInterface::new(
            public_url,
            a2a::TRANSPORT_PROTOCOL_JSONRPC,
        )],
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: None,
            extensions: Some(vec![AgentExtension {
                uri: ADK_EXTENSION_URI.to_owned(),
                description: Some(
                    "Content is spread across task artifacts and the status message, which carries long-running function calls."
                        .to_owned(),
                ),
                required: None,
                params: None,
            }]),
            extended_agent_card: None,
        },
        default_input_modes: vec!["text/plain".to_owned()],
        default_output_modes: vec!["text/plain".to_owned()],
        skills: build_agent_skills(root_agent),
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    }
}

/// A wrapped handler as a sized `RequestHandler` (a2a-rs's `jsonrpc_router` takes `Arc<H>`).
struct DynHandler(Arc<dyn RequestHandler>);

#[async_trait]
impl RequestHandler for DynHandler {
    async fn send_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2AError> {
        self.0.send_message(params, req).await
    }

    async fn send_streaming_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.0.send_streaming_message(params, req).await
    }

    async fn get_task(
        &self,
        params: &ServiceParams,
        req: GetTaskRequest,
    ) -> Result<Task, A2AError> {
        self.0.get_task(params, req).await
    }

    async fn list_tasks(
        &self,
        params: &ServiceParams,
        req: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2AError> {
        self.0.list_tasks(params, req).await
    }

    async fn cancel_task(
        &self,
        params: &ServiceParams,
        req: CancelTaskRequest,
    ) -> Result<Task, A2AError> {
        self.0.cancel_task(params, req).await
    }

    async fn subscribe_to_task(
        &self,
        params: &ServiceParams,
        req: SubscribeToTaskRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.0.subscribe_to_task(params, req).await
    }

    async fn create_push_config(
        &self,
        params: &ServiceParams,
        req: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.0.create_push_config(params, req).await
    }

    async fn get_push_config(
        &self,
        params: &ServiceParams,
        req: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.0.get_push_config(params, req).await
    }

    async fn list_push_configs(
        &self,
        params: &ServiceParams,
        req: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        self.0.list_push_configs(params, req).await
    }

    async fn delete_push_config(
        &self,
        params: &ServiceParams,
        req: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2AError> {
        self.0.delete_push_config(params, req).await
    }

    async fn get_extended_agent_card(
        &self,
        params: &ServiceParams,
        req: GetExtendedAgentCardRequest,
    ) -> Result<AgentCard, A2AError> {
        self.0.get_extended_agent_card(params, req).await
    }
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use gadk_agent::SingleLoader;
    use gadk_engine::llm::LlmAgentConfig;
    use gadk_engine::testutil::MockModel;
    use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
    use gadk_genai::{Content, ROLE_MODEL};
    use gadk_session::compaction::Config as CompactionConfig;
    use http::{Method, StatusCode};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{A2AHandlerOption, A2AHandlerWrapper};
    use crate::golog::{self, Captured};
    use crate::launcher::SubLauncher;
    use crate::test_support::{self as ts, strings};

    /// The fixed `golog` clock of these tests.
    const TS: &str = "2009/11/10 23:00:00 ";

    /// The Go probe's `assistant` (Task 2, `scripts/gen-go-launcher`): its names, description,
    /// instruction and tool reach the card; its model answers `Hello! How can I help?` once.
    fn assistant() -> Arc<dyn Agent> {
        let model = MockModel::from_contents(vec![Content::from_text(
            "Hello! How can I help?",
            ROLE_MODEL,
        )])
        .name_as("scripted-assistant");
        let transfer_funds = FunctionTool::new(
            FunctionToolConfig::new("transfer_funds", "Transfers funds."),
            |_ctx, _args: serde_json::Map<String, Value>| async move {
                Ok::<Value, gadk_agent::ToolError>(json!({}))
            },
        )
        .unwrap();
        gadk_engine::llm_agent::new(LlmAgentConfig {
            name: "assistant".into(),
            description: "Answers greetings and transfers funds.".into(),
            instruction: "Answer briefly. Use transfer_funds to transfer funds.".into(),
            model: Some(model),
            tools: vec![transfer_funds],
            ..Default::default()
        })
        .unwrap()
    }

    fn test_config(agent: Arc<dyn Agent>) -> (Config, Captured) {
        let (log, out) = golog::capture();
        let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
        config.log = log.with_clock(|| TS.to_owned());
        (config, out)
    }

    /// The a2a routes alone, as `setup_subrouters` mounts them.
    fn routes(l: &A2ALauncher, config: &mut Config) -> WebRouter {
        let mut router = WebRouter::new(true);
        l.setup_subrouters(&mut router, config).unwrap();
        router
    }

    async fn call(
        router: &WebRouter,
        req: http::Request<Body>,
    ) -> (StatusCode, http::HeaderMap, String) {
        let resp = router.serve(req).await;
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        (
            parts.status,
            parts.headers,
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    fn send_message_request(version: Option<&str>) -> http::Request<Body> {
        let body = ts::wire("full_29_a2a_v1_send")["request"]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut b = http::Request::builder()
            .method(Method::POST)
            .uri(API_PATH)
            .header("Content-Type", "application/json");
        if let Some(v) = version {
            b = b.header("A2A-Version", v);
        }
        b.body(Body::from(body)).unwrap()
    }

    /// Whether `s` is a UUID (SCENARIO.md N6's pattern).
    fn is_uuid(s: &str) -> bool {
        s.len() == 36
            && s.bytes().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == b'-',
                _ => c.is_ascii_hexdigit(),
            })
    }

    /// Replaces the value at `path` in `got` with the one in `want` after `check` accepted it
    /// (SCENARIO.md N5 and N6, by position rather than by order of appearance).
    fn normalize_at(got: &mut Value, want: &Value, path: &str, check: fn(&str) -> bool) {
        let value = got.pointer_mut(path).unwrap();
        assert!(check(value.as_str().unwrap()), "{path}: {value}");
        *value = want.pointer(path).unwrap().clone();
    }

    #[test]
    fn keyword_syntax_description_and_user_message_are_go() {
        let l = new();
        assert_eq!(l.keyword(), "a2a");
        assert_eq!(
            l.simple_description(),
            "starts A2A server which handles jsonrpc requests on /a2a/v1/invoke path"
        );
        assert_eq!(
            l.command_line_syntax(),
            "  -a2a_agent_url string\n    \tA2A host URL as advertised in the public agent card. It is used by A2A clients as a connection endpoint. (default \"http://localhost:8080\")\n"
        );
        let mut lines = Vec::new();
        l.user_message("http://localhost:8080", &mut |s: &str| {
            lines.push(s.to_owned())
        });
        assert_eq!(
            lines,
            ["       a2a:  you can access A2A using jsonrpc protocol: http://localhost:8080"]
        );
    }

    #[test]
    fn parse_returns_the_rest_and_go_errors() {
        let mut l = new();
        assert_eq!(
            l.parse(strings(&["-a2a_agent_url", "http://h:1", "api", "-x"]))
                .unwrap(),
            strings(&["api", "-x"])
        );
        assert_eq!(l.agent_url.get(), "http://h:1");

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut l = new().with_flag_output(out.clone());
        assert_eq!(
            l.parse(strings(&["-nope"])).unwrap_err().to_string(),
            "failed to parse a2a flags: flag provided but not defined: -nope"
        );
        out.lock().unwrap().clear();
        assert_eq!(
            l.parse(strings(&["-h"])).unwrap_err().to_string(),
            "failed to parse a2a flags: flag: help requested"
        );
        // The CLI fixture `help_web_a2a`: the usage the flag set prints before the error.
        let help = ts::cli("help_web_a2a");
        let usage = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(
            help["stderr"].as_str().unwrap().starts_with(&usage),
            "{usage}"
        );
        assert!(usage.starts_with("Usage of a2a:\n"), "{usage}");
    }

    /// The card is Go's (Task 2's `full_28_a2a_card`) without the 0.3 union: no top-level `url`,
    /// `protocolVersion` or `preferredTransport`, no 0.3 interface, no `pushNotifications`.
    #[tokio::test]
    async fn agent_card_is_gos_without_the_0_3_fields() {
        let (mut config, _) = test_config(assistant());
        let router = routes(&new(), &mut config);
        let (status, headers, body) = call(
            &router,
            http::Request::get(WELL_KNOWN_AGENT_CARD_PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["access-control-allow-origin"], "*");
        assert_eq!(headers["content-type"], "application/json");

        let mut want: Value = serde_json::from_str(
            ts::wire("full_28_a2a_card")["response"]["body"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let card = want.as_object_mut().unwrap();
        for key in ["url", "protocolVersion", "preferredTransport"] {
            card.remove(key).unwrap();
        }
        card["supportedInterfaces"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        card["capabilities"]
            .as_object_mut()
            .unwrap()
            .remove("pushNotifications")
            .unwrap();
        let got: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn agent_url_flag_sets_the_v1_interface() {
        let (mut config, _) = test_config(assistant());
        let mut l = new();
        l.parse(strings(&[
            "-a2a_agent_url",
            "https://agent.example.com/base/",
        ]))
        .unwrap();
        let router = routes(&l, &mut config);
        let (_, _, body) = call(
            &router,
            http::Request::get(WELL_KNOWN_AGENT_CARD_PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let card: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            card["supportedInterfaces"],
            json!([{
                "url": "https://agent.example.com/base/a2a/v1/invoke",
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0",
            }])
        );
        // The agent URL is not an allowed origin (Go `TestA2AAgentURLDoesNotWidenAPIOrigins`).
        assert!(config.allowed_origins.is_empty());
    }

    /// The v1 `SendMessage` of Task 2's `full_29_a2a_v1_send` through a launched web launcher
    /// with only `a2a` active; `/a2a/invoke` (0.3) is gorilla's 404 (D13); without
    /// `A2A-Version` a2a-rs refuses the request (Go answers it: parity ledger 6).
    #[tokio::test]
    async fn send_message_round_trip_through_the_web_launcher() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut l = crate::web::new(vec![Box::new(new())]);
        let agent_url = format!("http://localhost:{port}");
        assert_eq!(
            SubLauncher::parse(
                &mut l,
                strings(&[
                    "-port",
                    &port.to_string(),
                    "a2a",
                    "-a2a_agent_url",
                    &agent_url
                ])
            )
            .unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        let (config, out) = test_config(assistant());
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            assert!(Instant::now() < deadline, "the server did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            out.contents().contains(&format!(
                "{TS}       a2a:  you can access A2A using jsonrpc protocol: http://localhost:{port}\n"
            )),
            "{}",
            out.contents()
        );

        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");
        let card: Value = client
            .get(format!("{base}{WELL_KNOWN_AGENT_CARD_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            card["supportedInterfaces"][0]["url"],
            format!("{agent_url}/a2a/v1/invoke")
        );

        let send = ts::wire("full_29_a2a_v1_send");
        let body = send["request"]["body"].as_str().unwrap().to_owned();
        let resp = client
            .post(format!("{base}{API_PATH}"))
            .header("A2A-Version", "1.0")
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "application/json");
        let got = resp.text().await.unwrap();
        let mut got: Value = serde_json::from_str(&got).unwrap();
        // Go's task after N5/N6, minus the merged status-update metadata a2a-rs's task store
        // drops (parity ledger 5b); a2a-rs orders the task's keys its own way.
        let mut want: Value =
            serde_json::from_str(send["response"]["body"].as_str().unwrap()).unwrap();
        want["result"]["task"]
            .as_object_mut()
            .unwrap()
            .remove("metadata")
            .unwrap();
        for path in [
            "/result/task/id",
            "/result/task/artifacts/0/artifactId",
            "/result/task/contextId",
        ] {
            normalize_at(&mut got, &want, path, is_uuid);
        }
        normalize_at(&mut got, &want, "/result/task/status/timestamp", |t| {
            chrono::DateTime::parse_from_rfc3339(t).is_ok()
        });
        assert_eq!(got, want);
        assert!(
            out.contents()
                .contains(&format!("{TS}POST /a2a/v1/invoke ")),
            "{}",
            out.contents()
        );

        let resp = client
            .post(format!("{base}{API_PATH}"))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let got: Value = resp.json().await.unwrap();
        assert_eq!(got["error"]["code"], json!(-32009), "{got}");

        let resp = client
            .post(format!("{base}/a2a/invoke"))
            .header("Content-Type", "application/json")
            .body(
                ts::wire("full_31_a2a_v03_send")["request"]["body"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        assert_eq!(resp.text().await.unwrap(), "404 page not found\n");

        cancel.cancel();
        running.await.unwrap().unwrap();
    }

    /// A wrapper's handler: records `<name> <method>` for every call, then delegates.
    struct Recording {
        name: &'static str,
        inner: Arc<dyn RequestHandler>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Recording {
        fn record(&self, method: &str) {
            self.log
                .lock()
                .unwrap()
                .push(format!("{} {method}", self.name));
        }
    }

    #[async_trait]
    impl RequestHandler for Recording {
        async fn send_message(
            &self,
            params: &ServiceParams,
            req: SendMessageRequest,
        ) -> Result<SendMessageResponse, A2AError> {
            self.record("send_message");
            self.inner.send_message(params, req).await
        }
        async fn send_streaming_message(
            &self,
            params: &ServiceParams,
            req: SendMessageRequest,
        ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
            self.record("send_streaming_message");
            self.inner.send_streaming_message(params, req).await
        }
        async fn get_task(
            &self,
            params: &ServiceParams,
            req: GetTaskRequest,
        ) -> Result<Task, A2AError> {
            self.record("get_task");
            self.inner.get_task(params, req).await
        }
        async fn list_tasks(
            &self,
            params: &ServiceParams,
            req: ListTasksRequest,
        ) -> Result<ListTasksResponse, A2AError> {
            self.record("list_tasks");
            self.inner.list_tasks(params, req).await
        }
        async fn cancel_task(
            &self,
            params: &ServiceParams,
            req: CancelTaskRequest,
        ) -> Result<Task, A2AError> {
            self.record("cancel_task");
            self.inner.cancel_task(params, req).await
        }
        async fn subscribe_to_task(
            &self,
            params: &ServiceParams,
            req: SubscribeToTaskRequest,
        ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
            self.record("subscribe_to_task");
            self.inner.subscribe_to_task(params, req).await
        }
        async fn create_push_config(
            &self,
            params: &ServiceParams,
            req: TaskPushNotificationConfig,
        ) -> Result<TaskPushNotificationConfig, A2AError> {
            self.record("create_push_config");
            self.inner.create_push_config(params, req).await
        }
        async fn get_push_config(
            &self,
            params: &ServiceParams,
            req: GetTaskPushNotificationConfigRequest,
        ) -> Result<TaskPushNotificationConfig, A2AError> {
            self.record("get_push_config");
            self.inner.get_push_config(params, req).await
        }
        async fn list_push_configs(
            &self,
            params: &ServiceParams,
            req: ListTaskPushNotificationConfigsRequest,
        ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
            self.record("list_push_configs");
            self.inner.list_push_configs(params, req).await
        }
        async fn delete_push_config(
            &self,
            params: &ServiceParams,
            req: DeleteTaskPushNotificationConfigRequest,
        ) -> Result<(), A2AError> {
            self.record("delete_push_config");
            self.inner.delete_push_config(params, req).await
        }
        async fn get_extended_agent_card(
            &self,
            params: &ServiceParams,
            req: GetExtendedAgentCardRequest,
        ) -> Result<AgentCard, A2AError> {
            self.record("get_extended_agent_card");
            self.inner.get_extended_agent_card(params, req).await
        }
    }

    fn recording_wrapper(name: &'static str, log: &Arc<Mutex<Vec<String>>>) -> A2AHandlerWrapper {
        let log = Arc::clone(log);
        Arc::new(move |inner| {
            log.lock().unwrap().push(format!("wrap {name}"));
            Arc::new(Recording {
                name,
                inner,
                log: Arc::clone(&log),
            }) as Arc<dyn RequestHandler>
        })
    }

    /// An option that records its name and serves an extended card named `name`.
    fn card_option(name: &'static str, log: &Arc<Mutex<Vec<String>>>) -> A2AHandlerOption {
        let log = Arc::clone(log);
        Arc::new(move |handler: DefaultRequestHandler| {
            log.lock().unwrap().push(format!("option {name}"));
            let mut card = agent_card(assistant().as_ref(), "http://h/a2a/v1/invoke".to_owned());
            card.name = name.to_owned();
            handler.with_extended_agent_card(card)
        })
    }

    /// Go applies every `A2AOptions` entry: here every option in order (the last extended card
    /// wins), then every wrapper in order (the last one is outermost).
    #[tokio::test]
    async fn a2a_options_then_wrappers_all_apply_in_order() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let (mut config, _) = test_config(assistant());
        config.a2a_options = vec![card_option("first", &log), card_option("second", &log)];
        config.a2a_wrappers = vec![recording_wrapper("w1", &log), recording_wrapper("w2", &log)];
        let router = routes(&new(), &mut config);

        let (status, _, body) = call(&router, send_message_request(Some("1.0"))).await;
        assert_eq!(status, StatusCode::OK);
        let got: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            got["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
            "{body}"
        );

        let (status, _, body) = call(
            &router,
            http::Request::post(API_PATH)
                .header("A2A-Version", "1.0")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"GetExtendedAgentCard","params":{}}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let got: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(got["result"]["name"], "second", "{body}");

        assert_eq!(
            *log.lock().unwrap(),
            [
                "option first",
                "option second",
                "wrap w1",
                "wrap w2",
                "w2 send_message",
                "w1 send_message",
                "w2 get_extended_agent_card",
                "w1 get_extended_agent_card",
            ]
        );
    }

    /// Without options or wrappers the default handler is served directly.
    #[tokio::test]
    async fn without_options_the_default_handler_is_served() {
        let (mut config, _) = test_config(assistant());
        let router = routes(&new(), &mut config);
        let (status, _, body) = call(&router, send_message_request(Some("1.0"))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("TASK_STATE_COMPLETED"), "{body}");
    }

    /// Go's compaction dry run and agent-URL errors surface through the web launcher as
    /// `a2a subrouter setup failed: …`.
    #[tokio::test]
    async fn setup_errors_are_go_text() {
        // No summarizer over a root agent with no model to default to.
        let echo = gadk_engine::llm_agent::new(LlmAgentConfig {
            name: "echo".into(),
            ..Default::default()
        })
        .unwrap();
        let (mut config, _) = test_config(echo);
        config.compaction = Some(CompactionConfig {
            compaction_interval: 1,
            ..Default::default()
        });
        let mut l = crate::web::new(vec![Box::new(new())]);
        SubLauncher::parse(&mut l, strings(&["a2a"])).unwrap();
        let err = l
            .run_with(CancellationToken::new(), config, |_| true)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "a2a subrouter setup failed: the Compaction config cannot serve app \"echo\": the Compaction config needs a Summarizer: root agent \"echo\" has no model"
        );

        let (mut config, _) = test_config(assistant());
        config.compaction = Some(CompactionConfig {
            compaction_interval: -1,
            ..Default::default()
        });
        let err = l
            .run_with(CancellationToken::new(), config, |_| true)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "a2a subrouter setup failed: invalid Compaction: CompactionInterval must not be negative, got -1"
        );

        let (config, _) = test_config(assistant());
        let mut l = crate::web::new(vec![Box::new(new())]);
        SubLauncher::parse(&mut l, strings(&["a2a", "-a2a_agent_url", ":foo"])).unwrap();
        let err = l
            .run_with(CancellationToken::new(), config, |_| true)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "a2a subrouter setup failed: parse \":foo\": missing protocol scheme"
        );
    }
}
