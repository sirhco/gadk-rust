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

//! The Go wire probe's scenario (`scripts/gen-go-adkrest/SCENARIO.md`): the fixed clock and
//! UUID providers, the scripted models, the sample agents and the three server configurations
//! whose answers `testdata/wire` records. `tests/wire.rs` replays the fixtures against
//! [`wire_server`]; the controller tests reuse the agents.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use futures::stream::{self, BoxStream};
use gadk_agent::{Agent, AgentConfig, Context, Loader, MultiLoader, Tool, ToolError};
use gadk_artifact::{ArtifactService, InMemoryArtifactService};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FinishReason, FunctionCall, GenerateContentResponseUsageMetadata, Part};
use gadk_memory::{InMemoryMemoryService, MemoryService};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{InMemorySessionService, SessionService};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::authn::{self, AuthnError, Caller};
use crate::authz;
use crate::rest::{DebugApiConfig, Server, ServerConfig, ServerError};

pub use crate::rest::routing::RouteInfo;

/// The fixed clock's first reading: `2026-01-02T03:04:05Z` as Unix seconds.
pub const START_UNIX: i64 = 1_767_323_045;

/// The probe's fixed providers: the n-th clock read (n = 0, 1, …) is [`START_UNIX`] + n seconds,
/// the n-th UUID (n = 1, 2, …) `00000000-0000-4000-8000-<n as 12 digits>`. Clones share the
/// counters, so one platform serves every service of a configuration.
pub fn fixed_platform() -> Platform {
    let reads = Arc::new(AtomicI64::new(0));
    let ids = Arc::new(AtomicU64::new(0));
    Platform::default()
        .with_time(move || {
            let n = reads.fetch_add(1, Ordering::SeqCst);
            DateTime::<Utc>::from_timestamp(START_UNIX + n, 0).unwrap_or_default()
        })
        .with_uuid(move || {
            let n = ids.fetch_add(1, Ordering::SeqCst) + 1;
            format!("00000000-0000-4000-8000-{n:012}")
        })
}

/// The probe's scripted model: one reply per call, computed from the last request content.
pub struct ScriptedModel {
    name: &'static str,
    reply: fn(Option<&Content>) -> Result<Content, ModelError>,
}

impl ScriptedModel {
    /// A model named `name` answering with `reply(last content)`.
    #[allow(clippy::new_ret_no_self)] // the model as Go's `model.LLM`, as `FunctionTool::new`
    pub fn new(
        name: &'static str,
        reply: fn(Option<&Content>) -> Result<Content, ModelError>,
    ) -> Arc<dyn Llm> {
        Arc::new(ScriptedModel { name, reply })
    }
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        self.name
    }

    /// One item whatever `stream` says: the reply with the probe's usage (10 prompt, 5
    /// candidates, 15 total tokens) and `STOP`, or the reply's error.
    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let item = (self.reply)(req.contents.last()).map(|content| LlmResponse {
            content: Some(content),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
                ..Default::default()
            }),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        });
        Box::pin(stream::iter([item]))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `scripted-assistant`'s replies (SCENARIO.md, first match wins): a function response →
/// `Saved your note.`; then on the first non-empty text: `hello` → a greeting, `fail` → the error
/// `scripted model failure`, `save: <text>` → a `save_note` call, anything else → `I can only
/// say hello.`
pub fn assistant_reply(last: Option<&Content>) -> Result<Content, ModelError> {
    let parts = last.map(|c| c.parts.as_slice()).unwrap_or_default();
    if parts.iter().any(|p| p.function_response.is_some()) {
        return Ok(Content::from_text("Saved your note.", "model"));
    }
    let text = parts
        .iter()
        .find(|p| !p.text.is_empty())
        .map(|p| p.text.as_str())
        .unwrap_or_default();
    match text {
        "hello" => Ok(Content::from_text("Hello! How can I help?", "model")),
        "fail" => Err(ModelError::Other("scripted model failure".into())),
        _ => match text.strip_prefix("save: ") {
            Some(note) => Ok(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        name: "save_note".into(),
                        args: Map::from_iter([("text".to_owned(), Value::from(note))]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            None => Ok(Content::from_text("I can only say hello.", "model")),
        },
    }
}

/// Go `saveNoteArgs`.
#[derive(Deserialize, schemars::JsonSchema)]
struct SaveNoteArgs {
    text: String,
}

/// Go `saveNoteResult`.
#[derive(Serialize, schemars::JsonSchema)]
struct SaveNoteResult {
    status: String,
    version: i64,
}

/// The probe's `save_note` tool: saves `args.text` as the text artifact `note.txt` and returns
/// `{"status":"saved","version":<saved version>}`.
pub fn save_note_tool() -> Arc<dyn Tool> {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("save_note", "Saves a note as the artifact note.txt."),
        |ctx: Context, args: SaveNoteArgs| async move {
            let artifacts = ctx
                .artifacts()
                .ok_or_else(|| ToolError::Execution("no artifact service".into()))?;
            let saved = artifacts
                .save("note.txt", Part::from_text(args.text))
                .await
                .map_err(|e| ToolError::Execution(Box::new(e)))?;
            Ok(SaveNoteResult {
                status: "saved".into(),
                version: saved.version,
            })
        },
    );
    // The arguments and result are plain structs; the tool always builds.
    tool.unwrap_or_else(|e| unreachable!("save_note: {e}"))
}

/// An LLM agent with the probe's options (no callbacks, output key, schemas or config).
fn llm_agent(
    name: &str,
    description: &str,
    instruction: &str,
    model: Arc<dyn Llm>,
    tools: Vec<Arc<dyn Tool>>,
) -> Arc<dyn Agent> {
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        instruction: instruction.into(),
        model: Some(model),
        tools,
        ..Default::default()
    })
    .unwrap_or_else(|e| unreachable!("{name}: {e}"))
}

/// The probe's `assistant`: `scripted-assistant` with the `save_note` tool.
pub fn assistant_agent() -> Arc<dyn Agent> {
    llm_agent(
        "assistant",
        "Answers greetings and saves notes.",
        "Answer briefly. Use save_note to save notes.",
        ScriptedModel::new("scripted-assistant", assistant_reply),
        vec![save_note_tool()],
    )
}

/// The probe's `pipeline`: a sequential agent running `drafter` (always `Draft ready.`), then
/// `reviewer` (always `Review done.`).
pub fn pipeline_agent() -> Arc<dyn Agent> {
    let drafter = llm_agent(
        "drafter",
        "Writes a draft.",
        "Write a draft.",
        ScriptedModel::new("scripted-drafter", |_| {
            Ok(Content::from_text("Draft ready.", "model"))
        }),
        vec![],
    );
    let reviewer = llm_agent(
        "reviewer",
        "Reviews the draft.",
        "Review the draft.",
        ScriptedModel::new("scripted-reviewer", |_| {
            Ok(Content::from_text("Review done.", "model"))
        }),
        vec![],
    );
    sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "pipeline".into(),
            description: "Drafts, then reviews.".into(),
            sub_agents: vec![drafter, reviewer],
            ..Default::default()
        },
    })
    .unwrap_or_else(|e| unreachable!("pipeline: {e}"))
}

/// The probe's loader: root `assistant`, then `pipeline`.
pub fn scenario_loader() -> Arc<dyn Loader> {
    let loader = MultiLoader::new(assistant_agent(), vec![pipeline_agent()]);
    Arc::new(loader.unwrap_or_else(|e| unreachable!("loader: {e}")))
}

/// A server configuration of the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireConfig {
    /// Debug API on, 1024-byte body limit, no authenticator or authorizer.
    Main,
    /// `authn::header("X-Test-User")` and `authz::strict()`.
    Strict,
    /// A custom authenticator reading `X-Test-User` (`forbid`, `boom`, absent, or a user).
    Custom,
}

impl WireConfig {
    /// The fixture-name prefix (`main`, `strict`, `custom`).
    pub fn name(self) -> &'static str {
        match self {
            WireConfig::Main => "main",
            WireConfig::Strict => "strict",
            WireConfig::Custom => "custom",
        }
    }
}

/// The `custom` configuration's authenticator: `X-Test-User` `forbid` → forbidden, `boom` → the
/// error `boom`, empty or absent → no caller, anything else → that user.
fn custom_authenticator() -> Arc<dyn authn::Authenticator> {
    authn::custom(|parts: &http::request::Parts| {
        let user = parts
            .headers
            .get("X-Test-User")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        async move {
            match user.as_str() {
                "forbid" => Err(AuthnError::Forbidden(None)),
                "boom" => Err(AuthnError::Other("boom".into())),
                "" => Ok(None),
                _ => Ok(Some(Caller {
                    user_id: user,
                    claims: None,
                })),
            }
        }
    })
}

/// `config`'s [`ServerConfig`]: fresh in-memory session, memory and artifact services and a
/// fresh [`scenario_loader`], everything on `platform`, `BindHost: 127.0.0.1` and
/// `AllowedOrigins: [http://localhost:4200]`.
pub fn server_config(config: WireConfig, platform: Platform) -> ServerConfig {
    let sessions: Arc<dyn SessionService> =
        Arc::new(InMemorySessionService::with_platform(platform.clone()));
    let memory: Arc<dyn MemoryService> = Arc::new(InMemoryMemoryService::new());
    let artifacts: Arc<dyn ArtifactService> =
        Arc::new(InMemoryArtifactService::with_platform(platform.clone()));
    let mut cfg = ServerConfig::new(sessions);
    cfg.memory_service = Some(memory);
    cfg.artifact_service = Some(artifacts);
    cfg.agent_loader = Some(scenario_loader());
    cfg.bind_host = "127.0.0.1".into();
    cfg.allowed_origins = vec!["http://localhost:4200".into()];
    cfg.platform = platform;
    match config {
        WireConfig::Main => {
            cfg.debug_api_config = DebugApiConfig {
                include_debug_api: true,
            };
            cfg.max_payload_size = 1024;
        }
        WireConfig::Strict => {
            cfg.authenticator = Some(authn::header("X-Test-User"));
            cfg.authorizer = Some(authz::strict());
        }
        WireConfig::Custom => cfg.authenticator = Some(custom_authenticator()),
    }
    cfg
}

/// The server of `config` on a fresh [`fixed_platform`].
pub fn wire_server(config: WireConfig) -> Result<Server, ServerError> {
    Server::new(server_config(config, fixed_platform()))
}

/// Every route `server` registered, in registration order (Go `mux.Router.Walk`).
pub fn server_routes(server: &Server) -> Vec<RouteInfo> {
    server.routes()
}
