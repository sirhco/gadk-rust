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

//! The `agentengine` web sub-launcher, serving
//! [`gadk_server::agentengine`] under `-path_prefix` for Vertex AI Agent Engine.
//!
//! The mount matches POST only (another method under the prefix gets a bare 405), and the prefix
//! match is not segment-aware; the handler sees the path with the prefix stripped.

use std::io::Write;
use std::sync::{Arc, Mutex};

use gadk_platform::GoDuration;
use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_server::agentengine::{AgentEngineConfig, new_handler};
use http::Method;

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::web::api::strip_prefix;
use crate::web::{WebRouter, WebSublauncher};

/// The flag-backed settings of the launcher.
struct AgentEngineFlags {
    /// `-path_prefix` (default `/api`).
    path_prefix: Flag<String>,
    /// `-max_payload_size` (default 10 MiB).
    max_payload_size: Flag<i64>,
    /// `-sse-write-timeout` (default 120 s).
    sse_write_timeout: Flag<GoDuration>,
}

/// The `agentengine` web sub-launcher.
pub struct AgentEngineLauncher {
    /// The `web` flag set (named `"web"`), behind a `Mutex` as `ApiLauncher`'s is.
    flags: Mutex<FlagSet>,
    config: AgentEngineFlags,
    /// The path prefix after `Parse` normalized it (the flag default until then).
    path_prefix: String,
    /// The Agent Engine ID.
    agent_engine_id: String,
}

/// The sub-launcher for `agent_engine_id`, with its flags, defaults and usage text.
pub fn new(agent_engine_id: &str) -> AgentEngineLauncher {
    let mut fs = FlagSet::new("web");
    let path_prefix = fs.string(
        "path_prefix",
        "/api",
        "ADK Agent Engine API path prefix. Default is '/api'.",
    );
    let max_payload_size = fs.int(
        "max_payload_size",
        10 * 1024 * 1024,
        "The payload will be truncated after this amount of bytes",
    );
    let sse_write_timeout = fs.duration(
        "sse-write-timeout",
        GoDuration::from_secs(120),
        "SSE server write timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for writing the SSE response after reading the headers & body",
    );
    let prefix = path_prefix.get();
    AgentEngineLauncher {
        flags: Mutex::new(fs),
        config: AgentEngineFlags {
            path_prefix,
            max_payload_size,
            sse_write_timeout,
        },
        path_prefix: prefix,
        agent_engine_id: agent_engine_id.to_owned(),
    }
}

impl AgentEngineLauncher {
    /// Replaces where the flag set writes its errors and usage (default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> AgentEngineLauncher {
        lock(&self.flags).set_output(out);
        self
    }
}

/// The handler config from the launcher's `Config` (the web launcher has already defaulted
/// the services; a missing session service is `Error::Config`), its `log` the launcher's
/// logger.
pub(crate) fn engine_config(config: &Config) -> Result<AgentEngineConfig, Error> {
    // The handler requires a session service. The web launcher's `Run` always supplies one
    // (see `apply_service_defaults`).
    let Some(session_service) = config.session_service.clone() else {
        return Err(Error::Config("no session service configured".to_owned()));
    };
    let mut cfg = AgentEngineConfig::new(session_service, Arc::clone(&config.agent_loader));
    cfg.memory_service = config.memory_service.clone();
    cfg.artifact_service = config.artifact_service.clone();
    cfg.plugin_config = config.plugin_config.clone();
    cfg.compaction = config.compaction.clone();
    let logger = config.log.clone();
    cfg.log = Some(Arc::new(move |m: &str| logger.print(m)));
    Ok(cfg)
}

impl WebSublauncher for AgentEngineLauncher {
    /// The keyword `agentengine`.
    fn keyword(&self) -> String {
        "agentengine".to_owned()
    }

    /// Parses the flags (`failed to parse agent engine flags: <err>`), then the prefix
    /// with a leading `/` added and one trailing `/` trimmed.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let mut fs = lock(&self.flags);
        if let Err(e) = fs.parse(&args) {
            return Err(Error::Parse(format!(
                "failed to parse agent engine flags: {e}"
            )));
        }
        let mut p = self.config.path_prefix.get();
        if !p.starts_with('/') {
            p.insert(0, '/');
        }
        self.path_prefix = p.strip_suffix('/').unwrap_or(&p).to_owned();
        Ok(fs.args())
    }

    /// The flag usage text (`format_flag_usage` of the flag set).
    fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    /// A one-line description.
    fn simple_description(&self) -> String {
        "starts AgentEngine server which serves reasoning engine API while deployed to Agent Engine"
            .to_owned()
    }

    /// Checks `Config::validate`, then builds the handler with
    /// `gadk_server::agentengine::new_handler` (either error is answered as
    /// `Error::Config` carrying the handler-construction error text), then mounts the handler with
    /// `router.handle_prefix(&[Method::POST], prefix, strip_prefix(prefix, handler))`.
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        let wrap = |e: &dyn std::fmt::Display| {
            Error::Config(format!("agentengine.NewHandler failed: {e}"))
        };
        config.validate().map_err(|e| wrap(&e))?;
        let cfg = engine_config(config).map_err(|e| wrap(&e))?;
        let handler = new_handler(
            cfg,
            self.config.sse_write_timeout.get(),
            self.config.max_payload_size.get(),
            &self.agent_engine_id,
        )
        .map_err(|e| wrap(&e))?;
        router.handle_prefix(
            &[Method::POST],
            &self.path_prefix,
            strip_prefix(&self.path_prefix, handler),
        );
        Ok(())
    }

    /// The five startup lines, as ADK launchers print them (the `/api` in the first two is literal,
    /// not the prefix).
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        printer(&format!(
            "       agentEngine:  you can access this server locally by {web_url}/api/reasoning_engine"
        ));
        printer(&format!(
            "                                                           {web_url}/api/stream_reasoning_engine"
        ));
        printer("                     to access it while deployed to Agent Engine, you should use");
        printer(
            "                               https://${LOCATION_ID}-aiplatform.googleapis.com/v1/projects/${PROJECT_ID}/locations/${LOCATION_ID}/reasoningEngines/${RESOURCE_ID}:query",
        );
        printer(
            "                            or https://${LOCATION_ID}-aiplatform.googleapis.com/v1/projects/${PROJECT_ID}/locations/${LOCATION_ID}/reasoningEngines/${RESOURCE_ID}:streamQuery",
        );
    }
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::LazyLock;

    use gadk_agent::SingleLoader;
    use pretty_assertions::assert_eq;
    use regex::Regex;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::launcher::{Launcher, SubLauncher};
    use crate::test_support::{self as ts, strings};
    use crate::universal::UniversalLauncher;

    /// The probe's agent engine ID (C.11).
    const ENGINE_ID: &str = "test-engine";
    /// `testdata/agentengine`.
    const DIR: &str = "agentengine";
    /// Go's `Config.Validate` text for `&compaction.Config{}` ("no strategy at all").
    const NO_STRATEGY: &str = "invalid Compaction: no compaction strategy is enabled, set CompactionInterval or TokenThreshold (or leave the whole config nil to disable compaction)";

    /// A launcher config over the agentengine probe's `assistant` agent (the 5a scenario's,
    /// C.11; not the 6 launcher probe's streaming one).
    fn config() -> Config {
        Config::new(Arc::new(SingleLoader::new(
            gadk_server::rest::testutil::wire_scenario::assistant_agent(),
        )))
    }

    #[test]
    fn flags_have_go_defaults() {
        let l = new(ENGINE_ID);
        assert_eq!(l.keyword(), "agentengine");
        assert_eq!(l.agent_engine_id, ENGINE_ID);
        assert_eq!(l.path_prefix, "/api");
        assert_eq!(l.config.path_prefix.get(), "/api");
        assert_eq!(l.config.max_payload_size.get(), 10 * 1024 * 1024);
        assert_eq!(l.config.sse_write_timeout.get(), GoDuration::from_secs(120));
    }

    #[test]
    fn parse_normalizes_the_prefix() {
        for (arg, want) in [
            ("/api", "/api"),
            ("api", "/api"),
            ("/api/", "/api"),
            ("custom/", "/custom"),
            ("adk/v1/", "/adk/v1"),
            ("", ""),
            ("/", ""),
            // Go trims one trailing slash only.
            ("x//", "/x/"),
        ] {
            let mut l = new(ENGINE_ID);
            let rest = WebSublauncher::parse(&mut l, strings(&["-path_prefix", arg, "rest"]));
            assert_eq!(rest.unwrap(), strings(&["rest"]), "{arg:?}");
            assert_eq!(l.path_prefix, want, "{arg:?}");
        }
        let mut l = new(ENGINE_ID);
        WebSublauncher::parse(
            &mut l,
            strings(&["-max_payload_size", "7", "-sse-write-timeout", "-1s"]),
        )
        .unwrap();
        assert_eq!(l.config.max_payload_size.get(), 7);
        assert_eq!(l.config.sse_write_timeout.get(), GoDuration::from_secs(-1));
    }

    #[test]
    fn parse_errors_are_go_text() {
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut l =
            new(ENGINE_ID).with_flag_output(Arc::clone(&out) as Arc<Mutex<dyn Write + Send>>);
        assert_eq!(
            WebSublauncher::parse(&mut l, strings(&["-max_payload_size", "x"]))
                .unwrap_err()
                .to_string(),
            "failed to parse agent engine flags: invalid value \"x\" for flag -max_payload_size: parse error"
        );
        let usage = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(usage.contains("Usage of web:\n"), "{usage}");
    }

    #[test]
    fn descriptions_and_user_message_are_go_text() {
        let mut l = new(ENGINE_ID);
        assert_eq!(
            l.simple_description(),
            "starts AgentEngine server which serves reasoning engine API while deployed to Agent Engine"
        );
        // The prefix does not change the message: Go hard-codes `/api`.
        WebSublauncher::parse(&mut l, strings(&["-path_prefix", "custom/"])).unwrap();
        let mut lines = Vec::new();
        l.user_message("http://localhost:8080", &mut |line: &str| {
            lines.push(line.to_owned())
        });
        assert_eq!(
            lines,
            strings(&[
                "       agentEngine:  you can access this server locally by http://localhost:8080/api/reasoning_engine",
                "                                                           http://localhost:8080/api/stream_reasoning_engine",
                "                     to access it while deployed to Agent Engine, you should use",
                "                               https://${LOCATION_ID}-aiplatform.googleapis.com/v1/projects/${PROJECT_ID}/locations/${LOCATION_ID}/reasoningEngines/${RESOURCE_ID}:query",
                "                            or https://${LOCATION_ID}-aiplatform.googleapis.com/v1/projects/${PROJECT_ID}/locations/${LOCATION_ID}/reasoningEngines/${RESOURCE_ID}:streamQuery",
            ])
        );
    }

    #[test]
    fn engine_config_comes_from_config() {
        let (log, out) = crate::golog::capture();
        let mut config = config();
        config.log = log.with_clock(|| ts::TS.to_owned());
        assert_eq!(
            engine_config(&config).err().unwrap().to_string(),
            "no session service configured"
        );
        crate::web::apply_service_defaults(&mut config);
        config.compaction = Some(gadk_session::compaction::Config::default());
        let cfg = engine_config(&config).unwrap();
        assert!(cfg.memory_service.is_some());
        assert!(cfg.artifact_service.is_some());
        assert!(cfg.compaction.is_some());
        assert_eq!(
            cfg.agent_loader.root_agent().name(),
            config.agent_loader.root_agent().name()
        );
        let logged = out.contents().len();
        (cfg.log.unwrap())("Supported methods:\n");
        assert_eq!(
            &out.contents()[logged..],
            "2009/11/10 23:00:00 Supported methods:\n"
        );
    }

    #[test]
    fn setup_errors_are_go_text() {
        let l = new(ENGINE_ID);
        let mut config = config();
        config.log = crate::golog::capture().0;
        assert_eq!(
            l.setup_subrouters(&mut WebRouter::new(true), &mut config)
                .unwrap_err()
                .to_string(),
            "agentengine.NewHandler failed: no session service configured"
        );
        crate::web::apply_service_defaults(&mut config);
        config.compaction = Some(gadk_session::compaction::Config::default());
        assert_eq!(
            l.setup_subrouters(&mut WebRouter::new(true), &mut config)
                .unwrap_err()
                .to_string(),
            format!("agentengine.NewHandler failed: {NO_STRATEGY}")
        );

        // The web launcher adds Go's `<keyword> subrouter setup failed: ` (web.go buildRouter).
        let mut web = crate::web::new(vec![Box::new(new(ENGINE_ID)) as Box<dyn WebSublauncher>]);
        SubLauncher::parse(&mut web, strings(&["agentengine"])).unwrap();
        assert_eq!(
            web.build_router(&mut config).err().unwrap().to_string(),
            format!(
                "agentengine subrouter setup failed: agentengine.NewHandler failed: {NO_STRATEGY}"
            )
        );
    }

    /// The `cli_*` fixtures: Go's `agentengine.NewLauncher("test-engine")` run with the
    /// fixture's arguments; its stderr is the flag sets' output, the log, then the
    /// `Run failed: <err>\n\n<syntax>` record Go's `main` writes.
    #[tokio::test]
    async fn cli_fixtures_match_go() {
        let names: Vec<String> = ts::fixture_names(DIR)
            .into_iter()
            .filter(|n| n.starts_with("cli_"))
            .collect();
        assert_eq!(names.len(), 7);
        for name in names {
            let f = ts::fixture(DIR, &name);
            let out = Arc::new(Mutex::new(Vec::<u8>::new()));
            let sink = || Arc::clone(&out) as Arc<Mutex<dyn Write + Send>>;
            let mut l = UniversalLauncher::new(vec![Box::new(
                crate::web::new(vec![Box::new(new(ENGINE_ID).with_flag_output(sink()))])
                    .with_flag_output(sink()),
            )]);
            let (log, captured) = crate::golog::capture();
            let mut config = config();
            config.log = log.with_clock(|| ts::TS.to_owned());
            match f["launcher"].as_str().unwrap() {
                "agentengine" => {}
                "agentengine_bad_compaction" => {
                    config.compaction = Some(gadk_session::compaction::Config::default());
                }
                other => panic!("{name}: unknown launcher {other}"),
            }
            let args: Vec<String> = f["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect();
            let err = Launcher::execute(&mut l, CancellationToken::new(), config, args)
                .await
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                f["error"].as_str().unwrap(),
                "{name}: error"
            );
            let stderr = format!(
                "{}{}<ts> Run failed: {err}\n\n{}",
                String::from_utf8(out.lock().unwrap().clone()).unwrap(),
                ts::n1(&captured.contents()),
                Launcher::command_line_syntax(&l)
            );
            assert_eq!(stderr, f["stderr"].as_str().unwrap(), "{name}: stderr");
            assert_eq!(f["stdout"], "", "{name}: stdout");
            assert_eq!(f["exit_status"], 1, "{name}: exit status");
        }
    }

    static LAST_UPDATE_TIME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""last_update_time":(-?[0-9][0-9.eE+-]*|"[^"]*")"#).unwrap());

    /// `ts::normalize_log` plus the agentengine probe's rule: `<ts> Processing event: ` lines
    /// (Go's `%+v` dump, ledgered) dropped.
    fn normalize_log(text: &str, port: u16) -> Vec<String> {
        ts::normalize_log(text, port)
            .into_iter()
            .filter(|l| !l.starts_with("<ts> Processing event: "))
            .collect()
    }

    /// `ts::parse_response` plus the agentengine probe's N5: `last_update_time` too (a changed
    /// body's `Content-Length` becomes `<n>`; a wall-clock value drops `body_length`).
    fn response_of(raw: &[u8], head_request: bool, port: u16) -> Value {
        let mut resp = ts::parse_response(raw, head_request, port);
        let Some(body) = resp["body"].as_str().map(str::to_owned) else {
            return resp;
        };
        let normalized = LAST_UPDATE_TIME.replace_all(&body, r#""last_update_time":"<t>""#);
        if normalized == body {
            return resp;
        }
        if LAST_UPDATE_TIME
            .captures_iter(&body)
            .any(|c| c[1].contains('.'))
        {
            resp["body_length"] = Value::Null;
        }
        for h in resp["headers"].as_array_mut().unwrap() {
            if h[0] == "Content-Length" {
                h[1] = json!("<n>");
            }
        }
        resp["body"] = json!(normalized);
        resp
    }

    #[test]
    fn agentengine_normalizations_extend_the_launcher_ones() {
        let body = "{\"output\":{\"last_update_time\":1767323045.5}}\n";
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            response_of(raw.as_bytes(), false, 1),
            json!({
                "status": 200,
                "status_text": "OK",
                "headers": [["Content-Length", "<n>"]],
                "body": "{\"output\":{\"last_update_time\":\"<t>\"}}\n",
                "body_length": null,
                "body_sha256": null,
                "base_href": null,
            })
        );
        assert_eq!(
            normalize_log(
                "2026/01/02 03:04:05 Processing event: {ID:x}\n2026/01/02 03:04:05 POST /api/reasoning_engine 1.5ms\n",
                1
            ),
            strings(&["<ts> POST /api/reasoning_engine <elapsed>"])
        );
    }

    /// Sends the request of the `testdata/agentengine` fixture `name` and asserts the response
    /// and the log lines it added equal the fixture's.
    async fn assert_exchange(port: u16, name: &str, log: impl Fn() -> String) {
        let f = ts::fixture(DIR, name);
        let logged = log().len();
        let raw = ts::exchange(port, &f["request"]).await;
        let head = f["request"]["method"] == "HEAD";
        assert_eq!(
            response_of(&raw, head, port),
            f["response"],
            "{name}: response"
        );
        assert_eq!(
            normalize_log(&log()[logged..], port),
            ts::log_lines(&f),
            "{name}: log"
        );
    }

    /// Starts the web launcher with this sub-launcher on the `wire_<server>_00_startup`
    /// fixture's arguments, asserts the startup log (the service defaults, the handler's
    /// `Supported methods:` lines, the banner and Go's user message), then replays `exchanges`.
    async fn replay(server: &str, exchanges: &[&str]) {
        let startup = ts::fixture(DIR, &format!("wire_{server}_00_startup"));
        assert_eq!(startup["kind"], "startup");
        let port = ts::free_port();
        let mut l = crate::web::new(vec![Box::new(new(ENGINE_ID)) as Box<dyn WebSublauncher>]);
        assert_eq!(
            SubLauncher::parse(&mut l, ts::args(&startup, port)[1..].to_vec()).unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        let (log, out) = crate::golog::capture();
        let mut config = config();
        config.log = log.with_clock(|| ts::TS.to_owned());
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        ts::wait_listening(port).await;

        assert_eq!(
            normalize_log(&out.contents(), port),
            ts::log_lines(&startup),
            "{server}: startup log"
        );
        for name in exchanges {
            assert_exchange(port, name, || out.contents()).await;
        }
        cancel.cancel();
        running.await.unwrap().unwrap();
    }

    /// `wire_agentengine_*`: the default `/api` mount — a session created, a stream query
    /// answered, and the outer router's 405 for `GET`.
    #[tokio::test]
    async fn the_api_mount_matches_go_wire_fixtures() {
        replay(
            "agentengine",
            &[
                "wire_agentengine_01_create_session",
                "wire_agentengine_02_stream_query",
                "wire_agentengine_03_get_not_allowed",
            ],
        )
        .await;
    }

    /// `wire_prefix_*`: `-path_prefix custom/` mounts under `/custom`, while the user message
    /// still names `/api`.
    #[tokio::test]
    async fn the_custom_prefix_mount_matches_go_wire_fixtures() {
        replay("prefix", &["wire_prefix_01_create_session"]).await;
    }

    /// Every `wire_*` fixture is replayed by one of the two tests above.
    #[test]
    fn every_wire_fixture_is_replayed() {
        let names: Vec<String> = ts::fixture_names(DIR)
            .into_iter()
            .filter(|n| n.starts_with("wire_"))
            .collect();
        assert_eq!(
            names,
            strings(&[
                "wire_agentengine_00_startup",
                "wire_agentengine_01_create_session",
                "wire_agentengine_02_stream_query",
                "wire_agentengine_03_get_not_allowed",
                "wire_prefix_00_startup",
                "wire_prefix_01_create_session",
            ])
        );
    }
}
