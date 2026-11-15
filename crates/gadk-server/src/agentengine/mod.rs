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
//! The Agent Engine server (port of ADK-Go `server/agentengine`): the handler Vertex AI Agent
//! Engine calls, `POST /reasoning_engine` (session methods) and `POST /stream_reasoning_engine`
//! (streaming methods), byte-compatible with ADK-Go's.
//!
//! [`new_handler`] validates the config and builds the [`Handler`]; mount it under a prefix
//! (the launcher's `agentengine` sub-launcher mounts it at `-path_prefix`, default `/api`).
//! [`list_class_methods`] is the method list a deployment declares.

pub(crate) mod controller;
pub(crate) mod encode;
// Generated (`scripts/gen-go-agentengine --shapes`); its own fixed layout, diffed by check-fixtures.sh.
#[rustfmt::skip]
pub(crate) mod encode_shapes;
pub(crate) mod metadata;
pub(crate) mod method;
pub(crate) mod models;
pub(crate) mod routes;
pub(crate) mod unmarshal;

use std::sync::Arc;

use gadk_agent::{AgentError, Loader};
use gadk_artifact::ArtifactService;
use gadk_engine::compactionvalidate::against_root_agent;
use gadk_engine::runner::RunnerConfig;
use gadk_memory::MemoryService;
use gadk_platform::{GoDuration, Platform};
use gadk_session::SessionService;
use serde_json::{Map, Value};

use self::controller::Controller;
use self::method::MethodHandler;
use self::method::create_session::{self, CreateSessionHandler};
use self::method::delete_session::{self, DeleteSessionHandler};
use self::method::get_session::{self, GetSessionHandler};
use self::method::list_sessions::{self, ListSessionsHandler};
use self::method::stream_query::{self, StreamQueryHandler};
use self::method::streaming_agent_run_with_events::{self, StreamingAgentRunWithEventsHandler};
use crate::mux::Handler;

/// Go `log.Printf`/`log.Println` of the agentengine code: called with the text Go's logger
/// receives (Go `Sprintf` / `Sprintln` output, without the timestamp prefix; a trailing newline
/// is part of the text when Go's is). The launcher passes its Go logger.
pub type AgentEngineLog = Arc<dyn Fn(&str) + Send + Sync>;

/// What the Agent Engine handler serves (the fields of Go's `launcher.Config` it reads; the
/// launcher fills it as it fills `rest::ServerConfig`).
#[derive(Clone)]
pub struct AgentEngineConfig {
    /// Go `SessionService`. Required.
    pub session_service: Arc<dyn SessionService>,
    /// Go `MemoryService`.
    pub memory_service: Option<Arc<dyn MemoryService>>,
    /// Go `ArtifactService`.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Go `AgentLoader`: the streaming methods run its root agent.
    pub agent_loader: Arc<dyn Loader>,
    /// Go `PluginConfig`.
    pub plugin_config: gadk_engine::runner::PluginConfig,
    /// Go `Compaction`; `None` disables it.
    pub compaction: Option<gadk_session::compaction::Config>,
    /// Clock and ID source of every runner the handler builds.
    pub platform: Platform,
    /// Go's `log` output; `None` writes the lines as `tracing` info events.
    pub log: Option<AgentEngineLog>,
}

impl AgentEngineConfig {
    /// A config with the two required services and every other field at Go's zero value.
    pub fn new(session_service: Arc<dyn SessionService>, agent_loader: Arc<dyn Loader>) -> Self {
        AgentEngineConfig {
            session_service,
            memory_service: None,
            artifact_service: None,
            agent_loader,
            plugin_config: gadk_engine::runner::PluginConfig::default(),
            compaction: None,
            platform: Platform::default(),
            log: None,
        }
    }
}

/// A construction error of [`new_handler`], `Display`ing Go's text.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Go `compactionvalidate.AgainstRootAgent`'s error, unwrapped as Go returns it.
    #[error(transparent)]
    Compaction(AgentError),
    /// Go `NewAgentEngineAPIController`'s duplicate check, wrapped as `NewHandler` wraps it
    /// (`kind` is `non-streaming` or `streaming`). Unreachable with the six built-in methods.
    #[error(
        "controllers.NewAgentEngineAPIController failed (for {kind}): duplicate method name: {name}"
    )]
    DuplicateMethod {
        /// `non-streaming` or `streaming`.
        kind: &'static str,
        /// The duplicated method name.
        name: String,
    },
    /// Go `json.NewEncoder failed: %w` while logging the method metadata.
    #[error("json.NewEncoder failed: {0}")]
    Encode(String),
}

/// Go's `log` package as the agentengine code uses it: through [`AgentEngineConfig::log`], or
/// `tracing::info!` without one.
#[derive(Clone, Default)]
pub(crate) struct Log(pub(crate) Option<AgentEngineLog>);

impl Log {
    /// Go `log.Print(text)` / `log.Printf` with the message already formatted.
    pub(crate) fn print(&self, text: &str) {
        match &self.0 {
            Some(log) => log(text),
            None => tracing::info!("{}", text.trim_end_matches('\n')),
        }
    }

    /// Go `log.Println(text)`: [`Log::print`] of `text` plus `\n` (Go `Sprintln`).
    pub(crate) fn println(&self, text: &str) {
        self.print(&format!("{text}\n"));
    }
}

/// The method names, in Go's `listNonStreamHandlers` + `listStreamHandlers` order, with their
/// Go api modes.
pub(crate) const NON_STREAM_METHODS: [(&str, &str); 4] = [
    ("async_create_session", "async"),
    ("async_get_session", "async"),
    ("async_list_sessions", "async"),
    ("async_delete_session", "async"),
];
/// See [`NON_STREAM_METHODS`].
pub(crate) const STREAM_METHODS: [(&str, &str); 2] = [
    ("async_stream_query", "async_stream"),
    ("streaming_agent_run_with_events", "async_stream"),
];

/// Go `NewHandler`: validates `cfg.compaction` against the loader's root agent
/// (`compactionvalidate::against_root_agent`, error returned unwrapped), builds the two
/// controllers (non-streaming: the four session methods; streaming: `async_stream_query` and
/// `streaming_agent_run_with_events`) on a strict-slash `WebRouter` (`routes::router`), and
/// logs `Supported methods:` then one line per [`list_class_methods`] entry, taken from the
/// handlers' own `MethodHandler::metadata` (Go
/// `log.Println(<json.Encoder output>)`, see `metadata::structpb_json`).
///
/// `sse_write_timeout`: zero means 120 s, negative means no write deadline. Go's
/// `config.Validate()` is the caller's (the launcher runs `Config::validate` first).
pub fn new_handler(
    cfg: AgentEngineConfig,
    sse_write_timeout: GoDuration,
    max_payload_size: i64,
    agent_engine_id: &str,
) -> Result<Handler, Error> {
    let root = cfg.agent_loader.root_agent();
    let base = RunnerConfig {
        app_name: agent_engine_id.to_owned(),
        agent: Arc::clone(&root),
        session_service: Arc::clone(&cfg.session_service),
        artifact_service: cfg.artifact_service.clone(),
        memory_service: cfg.memory_service.clone(),
        plugin_config: cfg.plugin_config.clone(),
        auto_create_session: false,
        platform: cfg.platform.clone(),
        compaction: None,
    };
    against_root_agent(cfg.compaction.as_ref(), Some(root), &base).map_err(Error::Compaction)?;

    let log = Log(cfg.log.clone());
    let [
        (create, create_mode),
        (get, get_mode),
        (list, list_mode),
        (delete, delete_mode),
    ] = NON_STREAM_METHODS;
    let ss = &cfg.session_service;
    let non_stream: Vec<Arc<dyn MethodHandler>> = vec![
        Arc::new(CreateSessionHandler::new(
            Arc::clone(ss),
            agent_engine_id,
            create,
            create_mode,
        )),
        Arc::new(GetSessionHandler::new(
            Arc::clone(ss),
            agent_engine_id,
            get,
            get_mode,
        )),
        Arc::new(ListSessionsHandler::new(
            Arc::clone(ss),
            agent_engine_id,
            list,
            list_mode,
        )),
        Arc::new(DeleteSessionHandler::new(
            Arc::clone(ss),
            agent_engine_id,
            delete,
            delete_mode,
        )),
    ];
    let [(query, query_mode), (run, run_mode)] = STREAM_METHODS;
    let stream: Vec<Arc<dyn MethodHandler>> = vec![
        Arc::new(StreamQueryHandler::new(
            cfg.clone(),
            agent_engine_id,
            query,
            query_mode,
        )),
        Arc::new(StreamingAgentRunWithEventsHandler::new(
            cfg.clone(),
            agent_engine_id,
            run,
            run_mode,
        )),
    ];
    // Go logs `ListClassMethods()`, the same handlers' `Metadata()` (the agent engine ID does
    // not enter it).
    let methods: Vec<Map<String, Value>> = non_stream
        .iter()
        .chain(&stream)
        .map(|h| h.metadata())
        .collect();
    let non_stream = Controller::new(sse_write_timeout, max_payload_size, non_stream, log.clone())
        .map_err(|name| Error::DuplicateMethod {
            kind: "non-streaming",
            name,
        })?;
    let stream = Controller::new(sse_write_timeout, max_payload_size, stream, log.clone())
        .map_err(|name| Error::DuplicateMethod {
            kind: "streaming",
            name,
        })?;
    let router = routes::router(Arc::new(non_stream), Arc::new(stream));

    log.println("Supported methods:");
    for m in methods {
        log.println(&metadata::structpb_json(&m).map_err(Error::Encode)?);
    }

    Ok(Arc::new(move |req| {
        let router = router.clone();
        Box::pin(async move { router.serve(req).await })
    }))
}

/// Go `ListClassMethods`: the six methods' metadata (the maps Go passes to
/// `structpb.NewStruct`), non-streaming then streaming, with an empty agent engine ID.
pub fn list_class_methods() -> Vec<Map<String, Value>> {
    let [
        (create, create_mode),
        (get, get_mode),
        (list, list_mode),
        (delete, delete_mode),
    ] = NON_STREAM_METHODS;
    let [(query, query_mode), (run, run_mode)] = STREAM_METHODS;
    vec![
        create_session::metadata(create, create_mode),
        get_session::metadata(get, get_mode),
        list_sessions::metadata(list, list_mode),
        delete_session::metadata(delete, delete_mode),
        stream_query::metadata(query, query_mode),
        streaming_agent_run_with_events::metadata(run, run_mode),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::rest::testutil::single_agent_loader;

    /// Go `ListClassMethods()` as `[m.AsMap() …]` (ADK-Go `bb1fd51a`).
    const GO_CLASS_METHODS: &str = r#"[{"api_mode":"async","description":"Creates a new session.\n\t\t\nArgs:\n    user_id (str):\n\t        Required. The ID of the user.\n    state (dict[str, Any]):\n        Optional. The initial state of the session.\n\nReturns:\n    Session: The newly created session instance.\n","name":"async_create_session","parameters":{"properties":{"state":{"nullable":true,"type":"object"},"user_id":{"type":"string"}},"required":["user_id"],"type":"object"}},{"api_mode":"async","description":"Get a session for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Required. The ID of the session.\n\nReturns:\n    Session: The session instance (if any). Returns an error on failure\n\n","name":"async_get_session","parameters":{"properties":{"session_id":{"type":"string"},"user_id":{"type":"string"}},"required":["user_id","session_id"],"type":"object"}},{"api_mode":"async","description":"List sessions for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n\nReturns:\n    ListSessionsResponse: The list of sessions with data.\n","name":"async_list_sessions","parameters":{"properties":{"user_id":{"type":"string"}},"required":["user_id"],"type":"object"}},{"api_mode":"async","description":"Deletes a session for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Required. The ID of the session.\n\nReturns:\n\ton success returns an empty string. On error returns an error message.\n\n","name":"async_delete_session","parameters":{"properties":{"session_id":{"type":"string"},"user_id":{"type":"string"}},"required":["user_id","session_id"],"type":"object"}},{"api_mode":"async_stream","description":"Streams responses asynchronously from the ADK application.\nArgs:\n    message (genai.Content):\n        Required. The message to stream responses for.\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Optional. The ID of the session. If not provided, a new session will be created for the user.\n\nYields:\n    Single lines with JSON encoded event each. Errors are also emitted as JSON.\n\n","name":"async_stream_query","parameters":{"properties":{"message":{"anyOf":[{"type":"string"},{"additionalProperties":true,"type":"object"}]},"session_id":{"nullable":true,"type":"string"},"user_id":{"type":"string"}},"required":["message","user_id"],"type":"object"}},{"api_mode":"async_stream","description":"Streams responses asynchronously from the ADK application.\n\nThis method is primarily meant for invocation from Gemini Enterprise.\n\nArgs:\n    request_json (str):\n        Required. A JSON-encoded request object with:\n        - user_id (str): Required. The user ID to run the agent for.\n        - session_id (str): Optional. The session ID. Gemini Enterprise may\n          send its Discovery Engine session resource on the first turn; later\n          turns should use the backend ADK session ID returned in the response.\n        - message (Content): Required. The user message, using the genai\n          Content JSON shape, for example:\n          {\"role\":\"user\",\"parts\":[{\"text\":\"Hi\"}]}\n\n","name":"streaming_agent_run_with_events","parameters":{"properties":{"request_json":{"type":"string"}},"required":["request_json"],"type":"object"}}]"#;

    /// What Go's `log` wrote during `NewHandler` (flags 0).
    const GO_STARTUP_LOG: &str = r#"Supported methods:
{"api_mode":"async","description":"Creates a new session.\n\t\t\nArgs:\n    user_id (str):\n\t        Required. The ID of the user.\n    state (dict[str, Any]):\n        Optional. The initial state of the session.\n\nReturns:\n    Session: The newly created session instance.\n","name":"async_create_session","parameters":{"properties":{"state":{"nullable":true,"type":"object"},"user_id":{"type":"string"}},"required":["user_id"],"type":"object"}}

{"api_mode":"async","description":"Get a session for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Required. The ID of the session.\n\nReturns:\n    Session: The session instance (if any). Returns an error on failure\n\n","name":"async_get_session","parameters":{"properties":{"session_id":{"type":"string"},"user_id":{"type":"string"}},"required":["user_id","session_id"],"type":"object"}}

{"api_mode":"async","description":"List sessions for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n\nReturns:\n    ListSessionsResponse: The list of sessions with data.\n","name":"async_list_sessions","parameters":{"properties":{"user_id":{"type":"string"}},"required":["user_id"],"type":"object"}}

{"api_mode":"async","description":"Deletes a session for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Required. The ID of the session.\n\nReturns:\n\ton success returns an empty string. On error returns an error message.\n\n","name":"async_delete_session","parameters":{"properties":{"session_id":{"type":"string"},"user_id":{"type":"string"}},"required":["user_id","session_id"],"type":"object"}}

{"api_mode":"async_stream","description":"Streams responses asynchronously from the ADK application.\nArgs:\n    message (genai.Content):\n        Required. The message to stream responses for.\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Optional. The ID of the session. If not provided, a new session will be created for the user.\n\nYields:\n    Single lines with JSON encoded event each. Errors are also emitted as JSON.\n\n","name":"async_stream_query","parameters":{"properties":{"message":{"anyOf":[{"type":"string"},{"additionalProperties":true,"type":"object"}]},"session_id":{"nullable":true,"type":"string"},"user_id":{"type":"string"}},"required":["message","user_id"],"type":"object"}}

{"api_mode":"async_stream","description":"Streams responses asynchronously from the ADK application.\n\nThis method is primarily meant for invocation from Gemini Enterprise.\n\nArgs:\n    request_json (str):\n        Required. A JSON-encoded request object with:\n        - user_id (str): Required. The user ID to run the agent for.\n        - session_id (str): Optional. The session ID. Gemini Enterprise may\n          send its Discovery Engine session resource on the first turn; later\n          turns should use the backend ADK session ID returned in the response.\n        - message (Content): Required. The user message, using the genai\n          Content JSON shape, for example:\n          {\"role\":\"user\",\"parts\":[{\"text\":\"Hi\"}]}\n\n","name":"streaming_agent_run_with_events","parameters":{"properties":{"request_json":{"type":"string"}},"required":["request_json"],"type":"object"}}

"#;

    fn config() -> AgentEngineConfig {
        AgentEngineConfig::new(
            Arc::new(gadk_session::InMemorySessionService::new()),
            single_agent_loader("engine"),
        )
    }

    #[test]
    fn list_class_methods_matches_go() {
        let got = Value::Array(
            list_class_methods()
                .into_iter()
                .map(Value::Object)
                .collect(),
        );
        let want: Value = serde_json::from_str(GO_CLASS_METHODS).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn new_handler_logs_the_supported_methods_as_go() {
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&lines);
        let mut cfg = config();
        cfg.log = Some(Arc::new(move |m: &str| {
            sink.lock().unwrap().push(m.to_owned())
        }));
        new_handler(cfg, GoDuration::SECOND, 1 << 20, "engine").unwrap();
        // Go's logger adds a newline to a text that lacks one.
        let got: String = lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| {
                if l.ends_with('\n') {
                    l.clone()
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        assert_eq!(got, GO_STARTUP_LOG);
    }

    #[test]
    fn config_new_has_go_zero_values() {
        let cfg = config();
        assert!(cfg.memory_service.is_none());
        assert!(cfg.artifact_service.is_none());
        assert!(cfg.compaction.is_none());
        assert!(cfg.log.is_none());
        assert!(cfg.plugin_config.plugins.is_empty());
    }

    #[test]
    fn error_texts() {
        let e = Error::DuplicateMethod {
            kind: "streaming",
            name: "async_stream_query".into(),
        };
        assert_eq!(
            e.to_string(),
            "controllers.NewAgentEngineAPIController failed (for streaming): duplicate method name: async_stream_query"
        );
        assert_eq!(
            Error::Encode("x".into()).to_string(),
            "json.NewEncoder failed: x"
        );
    }
}
