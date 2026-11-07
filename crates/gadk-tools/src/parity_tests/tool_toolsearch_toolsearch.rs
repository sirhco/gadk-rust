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

//! Parity port of Go tool/toolsearch/toolsearch_test.go (white-box, package toolsearch).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::any::Any;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use gadk_agent::{
    AgentConfig, Context, ReadonlyContext, RunConfig, Tool, ToolError, Toolset, new_agent,
};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{
    Content, FunctionCall, FunctionDeclaration, Part, ROLE_MODEL, ROLE_USER, Schema, Type,
};
use gadk_model::{Llm, LlmRequest};
use gadk_platform::Platform;
use gadk_session::{
    Event, GetRequest, InMemorySessionService, Session, SessionError, SessionService, State,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::gemini;
use crate::ranksearch;
use crate::toolsearch::{
    Config, GatingToolset, STATE_KEY_PREFIX, SearchArgs, SearchMatch, SearchOutput, TOOL_NAME,
    build_items, discovered_names, execute_search, new, new_gating, reveal_tools,
};

/// Go `testAgent`: the agent name the stub contexts report.
const TEST_AGENT: &str = "test_agent";

// parity: skipped TestGatingToolset_SkipsDiscoveredToolsTheFlowCannotPack: every Rust Tool has a (defaulted) process_request, so the non-packable stub this test reveals cannot exist and the filter it pins has nothing to skip (spec R8)

// ---------------------------------------------------------------------------------------------
// Errors (Go `errors.New` sentinels and `errors.Is`).

/// A sentinel error, like Go's `errors.New(msg)` in the tests.
#[derive(Debug)]
struct TestError(&'static str);

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TestError {}

fn test_err(msg: &'static str) -> ToolError {
    ToolError::Execution(Box::new(TestError(msg)))
}

/// Go `errors.Is(err, errors.New(want))`: whether a [`TestError`] with text `want` is in the
/// chain. The transparent `ToolError::Execution` and `SessionError::Backend` forward `source()`
/// past their boxed error, so the walk steps into those boxes itself.
fn wraps(err: &ToolError, want: &str) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if e.downcast_ref::<TestError>().is_some_and(|t| t.0 == want) {
            return true;
        }
        cur = match (
            e.downcast_ref::<ToolError>(),
            e.downcast_ref::<SessionError>(),
        ) {
            (Some(ToolError::Execution(inner)), _) => Some(&**inner),
            (_, Some(SessionError::Backend(inner))) => Some(&**inner),
            _ => e.source(),
        };
    }
    false
}

// ---------------------------------------------------------------------------------------------
// Fake state and contexts (Go fakeState, stubReadonlyContext, fakeToolContext).

/// Go `fakeState`: when `set_err` is set, `set` fails with it.
struct FakeState {
    data: Mutex<Map<String, Value>>,
    set_err: Option<&'static str>,
}

impl State for FakeState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.data
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        if let Some(err) = self.set_err {
            return Err(SessionError::Backend(Box::new(TestError(err))));
        }
        self.data.lock().unwrap().insert(key.to_string(), value);
        Ok(())
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.data
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// A session over a [`FakeState`]: Go's stub contexts return the fake state directly, Rust
/// contexts read and write state through their session.
struct FakeSession {
    state: FakeState,
}

impl Session for FakeSession {
    fn id(&self) -> &str {
        "session"
    }

    fn app_name(&self) -> &str {
        "app"
    }

    fn user_id(&self) -> &str {
        "user"
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn events(&self) -> Vec<Event> {
        Vec::new()
    }

    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::<chrono::Utc>::default()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `newFakeState(data)`.
fn new_fake_state(data: Map<String, Value>) -> Arc<FakeSession> {
    Arc::new(FakeSession {
        state: FakeState {
            data: Mutex::new(data),
            set_err: None,
        },
    })
}

/// Go `newFakeState(nil)`.
fn empty_state() -> Arc<FakeSession> {
    new_fake_state(Map::new())
}

/// Go `newAgentToolCtx(state, agentName)`. The same context also stands in for Go's
/// `newCtx(state)` read-only context, through `as_readonly()`.
fn new_agent_tool_ctx(state: &Arc<FakeSession>, agent_name: &str) -> Context {
    let agent = new_agent(AgentConfig {
        name: agent_name.into(),
        ..Default::default()
    })
    .unwrap();
    Context::builder()
        .session(state.clone())
        .agent(agent)
        .build()
}

/// Go `newToolCtx(state)` / `newCtx(state)`.
fn new_tool_ctx(state: &Arc<FakeSession>) -> Context {
    new_agent_tool_ctx(state, TEST_AGENT)
}

/// Go `discoveredNames(state, agentName)` over a fake state.
fn discovered(state: &FakeSession, agent_name: &str) -> Vec<String> {
    discovered_names(state.state.all(), agent_name)
}

/// Go `discoveredState(t, names...)`: state in which test_agent discovered `names` in order.
fn discovered_state(names: &[&str]) -> Arc<FakeSession> {
    let state = empty_state();
    reveal_tools(&new_tool_ctx(&state), names)
        .unwrap_or_else(|e| panic!("RevealTools() error = {e}"));
    state
}

// ---------------------------------------------------------------------------------------------
// Stub tools (Go stubTool, packableStubTool, typedParamsTool, typedNilSchemaTool,
// nilDeclarationTool, nonPackableStubTool).

/// One stub covers Go's stub tool types: `decl` is what `Declaration()` returns, and
/// `pack_count` counts `process_request` calls (Go `packableStubTool.packCount`).
struct StubTool {
    name: String,
    desc: String,
    decl: Option<FunctionDeclaration>,
    pack_count: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl Tool for StubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.desc
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        self.decl.clone()
    }

    async fn process_request(
        &self,
        _ctx: &Context,
        _req: &mut LlmRequest,
    ) -> Result<(), ToolError> {
        if let Some(count) = &self.pack_count {
            count.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn tool_with(name: &str, desc: &str, decl: Option<FunctionDeclaration>) -> Arc<dyn Tool> {
    Arc::new(StubTool {
        name: name.into(),
        desc: desc.into(),
        decl,
        pack_count: None,
    })
}

/// Go `&stubTool{name, desc}`: declares `{Name, Description}`.
fn stub_tool(name: &str, desc: &str) -> Arc<dyn Tool> {
    tool_with(
        name,
        desc,
        Some(FunctionDeclaration {
            name: name.into(),
            description: desc.into(),
            ..Default::default()
        }),
    )
}

/// Go `nilDeclarationTool` (and the Rust stand-in for `nonPackableStubTool`): declares nothing.
fn nil_declaration_tool(name: &str, desc: &str) -> Arc<dyn Tool> {
    tool_with(name, desc, None)
}

/// Go `packableStubTool`: a stub tool that counts its `process_request` calls.
fn packable_stub_tool(name: &str, desc: &str, count: Option<Arc<AtomicUsize>>) -> Arc<dyn Tool> {
    Arc::new(StubTool {
        name: name.into(),
        desc: desc.into(),
        decl: Some(FunctionDeclaration {
            name: name.into(),
            description: desc.into(),
            ..Default::default()
        }),
        pack_count: count,
    })
}

/// Go `makeTools(defs...)`.
fn make_tools(defs: &[(&str, &str)]) -> Vec<Arc<dyn Tool>> {
    defs.iter().map(|(n, d)| stub_tool(n, d)).collect()
}

// ---------------------------------------------------------------------------------------------
// Toolsets (Go staticToolset, packableBaseToolset, failingToolset, failingRequestToolset).

/// Go `staticToolset`: always returns the same tools.
struct StaticToolset {
    tools: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Toolset for StaticToolset {
    fn name(&self) -> &str {
        "static"
    }

    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.tools.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn static_toolset(tools: Vec<Arc<dyn Tool>>) -> Arc<dyn Toolset> {
    Arc::new(StaticToolset { tools })
}

/// Go `packableBaseToolset`: a static toolset whose `process_request` injects its own entry.
struct PackableBaseToolset {
    tools: Vec<Arc<dyn Tool>>,
    pack_count: Arc<AtomicUsize>,
}

#[async_trait]
impl Toolset for PackableBaseToolset {
    fn name(&self) -> &str {
        "static"
    }

    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.tools.clone())
    }

    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        req.tools.insert(
            "base_injected".to_string(),
            Arc::new(()) as Arc<dyn Any + Send + Sync>,
        );
        self.pack_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `failingToolset`: `tools` fails with `err`.
struct FailingToolset {
    err: &'static str,
}

#[async_trait]
impl Toolset for FailingToolset {
    fn name(&self) -> &str {
        "failing"
    }

    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Err(test_err(self.err))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `failingRequestToolset`: `process_request` fails with `err`.
struct FailingRequestToolset {
    tools: Vec<Arc<dyn Tool>>,
    err: &'static str,
}

#[async_trait]
impl Toolset for FailingRequestToolset {
    fn name(&self) -> &str {
        "static"
    }

    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.tools.clone())
    }

    async fn process_request(
        &self,
        _ctx: &Context,
        _req: &mut LlmRequest,
    ) -> Result<(), ToolError> {
        Err(test_err(self.err))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers (Go checkNames, mustNew, mustToolNames, mustSearch, toolNames, itemNames, matchNames).

/// Go `checkNames`: names in `want` must be in `got`, names in `not_want` must not.
fn check_names(got: &[String], want: &[&str], not_want: &[&str]) {
    for n in want {
        assert!(
            got.iter().any(|g| g == n),
            "names = {got:?}, want {n:?} included"
        );
    }
    for n in not_want {
        assert!(
            !got.iter().any(|g| g == n),
            "names = {got:?}, want {n:?} excluded"
        );
    }
}

fn must_new(base: Arc<dyn Toolset>, cfg: Config) -> Arc<GatingToolset> {
    new_gating(base, cfg).unwrap_or_else(|e| panic!("New() error = {e}"))
}

fn tool_names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
    tools.iter().map(|t| t.name().to_string()).collect()
}

async fn must_tool_names(ts: &dyn Toolset, ctx: &Context) -> Vec<String> {
    let tools = ts
        .tools(ctx.as_readonly())
        .await
        .unwrap_or_else(|e| panic!("Tools() error = {e}"));
    tool_names(&tools)
}

async fn must_search(
    ctx: &Context,
    query: &str,
    base: &dyn Toolset,
    gts: &GatingToolset,
    max_results: i32,
) -> SearchOutput {
    execute_search(
        ctx,
        SearchArgs {
            query: query.into(),
        },
        base,
        &gts.core_names,
        &gts.skill_annotations,
        max_results,
    )
    .await
    .unwrap_or_else(|e| panic!("executeSearch({query:?}) error = {e}"))
}

fn item_names(items: &[ranksearch::Item]) -> Vec<String> {
    items.iter().map(|it| it.name.clone()).collect()
}

fn match_names(matches: &[SearchMatch]) -> Vec<String> {
    matches.iter().map(|m| m.name.clone()).collect()
}

// ---------------------------------------------------------------------------------------------
// Runner helpers (Go parallelCallModel + llmagent.New + runner.New).

#[derive(Deserialize, schemars::JsonSchema)]
struct NoArgs {}

#[derive(Serialize, schemars::JsonSchema)]
struct NoResult {}

/// Go `newTool(name)`: a function tool named and described `name` taking and returning `struct{}`.
fn new_tool(name: &str) -> Arc<dyn Tool> {
    FunctionTool::new(
        FunctionToolConfig::new(name, name),
        |_ctx: Context, _args: NoArgs| async { Ok::<_, ToolError>(NoResult {}) },
    )
    .unwrap_or_else(|e| panic!("functiontool.New({name:?}) error = {e}"))
}

/// Go `parallelCallModel`: the first response holds one `search_tools` call per `(id, query)`.
/// Each of the next `later_steps` responses is the text "done".
fn parallel_call_model(calls: &[(&str, &str)], later_steps: usize) -> Arc<MockModel> {
    let parts = calls
        .iter()
        .map(|(id, query)| Part {
            function_call: Some(FunctionCall {
                id: (*id).to_string(),
                name: TOOL_NAME.into(),
                args: Map::from_iter([("query".to_string(), json!(query))]),
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect();
    let mut contents = vec![Content::from_parts(parts, ROLE_MODEL)];
    contents.extend((0..later_steps).map(|_| Content::from_text("done", ROLE_MODEL)));
    MockModel::from_contents(contents)
}

/// Go `parallelCallModel.declared`: the function declarations of the latest request.
fn declared(llm: &MockModel) -> Vec<String> {
    let requests = llm.requests();
    let Some(req) = requests.last() else {
        return Vec::new();
    };
    req.config
        .iter()
        .flat_map(|c| c.tools.iter())
        .flat_map(|t| t.function_declarations.iter())
        .map(|d| d.name.clone())
        .collect()
}

fn new_runner(
    agent_name: &str,
    llm: Arc<MockModel>,
    gated: Arc<dyn Toolset>,
    svc: Arc<dyn SessionService>,
) -> Runner {
    let agent = llm_agent::new(LlmAgentConfig {
        name: agent_name.into(),
        model: Some(llm as Arc<dyn Llm>),
        toolsets: vec![gated],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New() error = {e}"));
    Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap_or_else(|e| panic!("runner.New() error = {e}"))
}

/// Go `r.Run(ctx, "user", "session", "go", RunConfig{})`, failing on any error.
async fn run_turn(r: &Runner, label: &str, state_delta: Option<Map<String, Value>>) -> Vec<Event> {
    let items: Vec<_> = r
        .run(
            "user",
            "session",
            Some(Content::from_text("go", ROLE_USER)),
            RunConfig::default(),
            RunOptions {
                state_delta,
                ..Default::default()
            },
        )
        .collect()
        .await;
    items
        .into_iter()
        .map(|i| i.unwrap_or_else(|e| panic!("{label}: Run() error = {e}")))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Tests, in Go file order.

#[tokio::test]
async fn test_gating_toolset_empty_state() {
    let base = static_toolset(make_tools(&[
        ("get_current_time", "get time"),
        ("list_books", "list all books"),
        ("get_author", "get an author"),
    ]));
    let ts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into()],
            ..Default::default()
        },
    );

    let names = must_tool_names(ts.as_ref(), &new_tool_ctx(&empty_state())).await;
    check_names(
        &names,
        &[TOOL_NAME, "get_current_time"],
        &["list_books", "get_author"],
    );
}

#[tokio::test]
async fn test_gating_toolset_with_discovered_state() {
    let base = static_toolset(make_tools(&[
        ("get_current_time", "get time"),
        ("list_books", "list all books"),
        ("get_author", "get an author"),
    ]));
    let ts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into()],
            ..Default::default()
        },
    );

    let state = discovered_state(&["list_books"]);

    let names = must_tool_names(ts.as_ref(), &new_tool_ctx(&state)).await;
    check_names(
        &names,
        &[TOOL_NAME, "get_current_time", "list_books"],
        &["get_author"],
    );
}

#[tokio::test]
async fn test_reveal_tools_appends_unique_names_in_order() {
    // Discovery order deliberately differs from name order.
    let state = discovered_state(&["zeta_tool", "shared_tool"]);

    reveal_tools(
        &new_tool_ctx(&state),
        &[
            "shared_tool",
            "skill_tool_b",
            "skill_tool_a",
            "skill_tool_b",
        ],
    )
    .unwrap_or_else(|e| panic!("RevealTools() error = {e}"));

    assert_eq!(
        discovered(&state, TEST_AGENT),
        ["zeta_tool", "shared_tool", "skill_tool_b", "skill_tool_a"],
        "discoveredNames() mismatch"
    );
}

#[tokio::test]
async fn test_search_excludes_previously_revealed_tools() {
    let base = static_toolset(make_tools(&[
        ("list_notes", "list notes"),
        ("list_books", "list books"),
    ]));
    let state = empty_state();
    reveal_tools(&new_tool_ctx(&state), &["list_notes"])
        .unwrap_or_else(|e| panic!("RevealTools() error = {e}"));

    let out = execute_search(
        &new_tool_ctx(&state),
        SearchArgs {
            query: "select:list_notes,list_books".into(),
        },
        base.as_ref(),
        &HashSet::new(),
        &BTreeMap::new(),
        8,
    )
    .await
    .unwrap_or_else(|e| panic!("executeSearch() error = {e}"));
    assert_eq!(
        match_names(&out.matches),
        ["list_books"],
        "executeSearch() matches mismatch"
    );
}

/// Discovered tools come after the core tools, in discovery order rather than catalog order.
/// That keeps the serialized tool list append-only across turns, which preserves the
/// provider's prompt-cache prefix.
#[tokio::test]
async fn test_gating_toolset_discovered_tools_in_discovery_order() {
    let base = static_toolset(make_tools(&[
        ("a_tool", "catalog-first tool"),
        ("get_current_time", "get time"),
        ("z_tool", "catalog-last tool"),
    ]));
    let ts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into()],
            ..Default::default()
        },
    );

    // z_tool was discovered before a_tool.
    let state = discovered_state(&["z_tool", "a_tool"]);

    assert_eq!(
        must_tool_names(ts.as_ref(), &new_tool_ctx(&state)).await,
        [TOOL_NAME, "get_current_time", "z_tool", "a_tool"],
        "Tools() must follow discovery order, not catalog order"
    );
}

#[tokio::test]
async fn test_gating_toolset_degenerate_guard() {
    let base = static_toolset(make_tools(&[
        ("get_current_time", "get time"),
        ("show_help", "show help"),
    ]));
    let ts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into(), "show_help".into()],
            ..Default::default()
        },
    );

    let names = must_tool_names(ts.as_ref(), &new_tool_ctx(&empty_state())).await;
    check_names(&names, &["get_current_time", "show_help"], &[TOOL_NAME]);
}

/// A core tool whose name is also in the discovery state (after an app moves a discovered tool
/// into CoreToolNames) comes back from Tools once; a second copy would fail every later model
/// step with a duplicate-tool error.
#[tokio::test]
async fn test_gating_toolset_discovered_core_tool_is_listed_once() {
    let base = static_toolset(make_tools(&[
        ("get_current_time", "get time"),
        ("list_books", "list books"),
    ]));
    let ts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into()],
            ..Default::default()
        },
    );

    let state = discovered_state(&["get_current_time", "list_books"]);

    assert_eq!(
        must_tool_names(ts.as_ref(), &new_tool_ctx(&state)).await,
        [TOOL_NAME, "get_current_time", "list_books"],
        "Tools() mismatch"
    );
}

/// ProcessRequest does not pack discovered tools itself: Tools already returns them and the
/// flow packs everything Tools returns.
#[tokio::test]
async fn test_process_request_leaves_tool_packing_to_the_flow() {
    let packs = Arc::new(AtomicUsize::new(0));
    let base = static_toolset(vec![
        packable_stub_tool("list_books", "list", Some(packs.clone())),
        packable_stub_tool("get_author", "get", None),
    ]);
    let gts = must_new(base, Config::default());

    let mut req = LlmRequest::default();
    req.tools.insert(
        TOOL_NAME.to_string(),
        Arc::new(()) as Arc<dyn Any + Send + Sync>,
    );
    gts.process_request(&new_tool_ctx(&discovered_state(&["list_books"])), &mut req)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest() error = {e}"));

    assert_eq!(
        packs.load(Ordering::SeqCst),
        0,
        "discovered tool packed by ProcessRequest, want 0"
    );
}

#[tokio::test]
async fn test_search_catalog_error_is_returned() {
    let base: Arc<dyn Toolset> = Arc::new(FailingToolset {
        err: "catalog down",
    });
    let gts = must_new(base.clone(), Config::default());

    let err = execute_search(
        &new_tool_ctx(&empty_state()),
        SearchArgs {
            query: "books".into(),
        },
        base.as_ref(),
        &gts.core_names,
        &BTreeMap::new(),
        8,
    )
    .await
    .unwrap_err();
    assert!(
        wraps(&err, "catalog down"),
        "executeSearch() error = {err}, want it to wrap catalog down"
    );
    assert_eq!(err.to_string(), "toolsearch: list base tools: catalog down");
}

#[tokio::test]
async fn test_process_request_forwards_to_base() {
    let base_packs = Arc::new(AtomicUsize::new(0));
    let base: Arc<dyn Toolset> = Arc::new(PackableBaseToolset {
        tools: make_tools(&[("get_current_time", "ask"), ("list_books", "list")]),
        pack_count: base_packs.clone(),
    });
    let gts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into()],
            ..Default::default()
        },
    );

    let mut req = LlmRequest::default();
    req.tools.insert(
        TOOL_NAME.to_string(),
        Arc::new(()) as Arc<dyn Any + Send + Sync>,
    );
    gts.process_request(&new_tool_ctx(&empty_state()), &mut req)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest() error = {e}"));

    assert!(
        req.tools.contains_key("base_injected"),
        "base toolset ProcessRequest was not forwarded"
    );
    assert_eq!(
        base_packs.load(Ordering::SeqCst),
        1,
        "base ProcessRequest calls"
    );
}

#[tokio::test]
async fn test_process_request_returns_base_error() {
    let base: Arc<dyn Toolset> = Arc::new(FailingRequestToolset {
        tools: make_tools(&[("list_books", "list")]),
        err: "base request hook failed",
    });
    let gts = must_new(base, Config::default());
    let err = gts
        .process_request(&new_tool_ctx(&empty_state()), &mut LlmRequest::default())
        .await
        .unwrap_err();
    assert!(
        wraps(&err, "base request hook failed"),
        "ProcessRequest() error = {err}, want it to wrap base request hook failed"
    );
    assert_eq!(
        err.to_string(),
        "toolsearch: base toolset ProcessRequest: base request hook failed"
    );
}

/// The gated tool names in Config are listed in the search_tools description; core names and
/// the reserved ToolName are left out of that list.
#[tokio::test]
async fn test_search_tool_advertises_gated_tools_in_description() {
    let base = static_toolset(make_tools(&[
        ("get_current_time", "ask"),
        ("list_books", "list books"),
        ("list_publishers", "list publishers"),
    ]));
    let ts = must_new(
        base,
        Config {
            core_tool_names: vec!["get_current_time".into()],
            gated_tool_names: vec![
                "list_publishers".into(),
                "list_books".into(),
                "get_current_time".into(),
                TOOL_NAME.into(),
            ],
            ..Default::default()
        },
    );

    let ctx = new_tool_ctx(&empty_state());
    let tools = ts
        .tools(ctx.as_readonly())
        .await
        .unwrap_or_else(|e| panic!("Tools() error = {e}"));

    let desc = tools
        .iter()
        .filter(|t| t.name() == TOOL_NAME)
        .map(|t| t.description().to_string())
        .next_back()
        .unwrap_or_default();
    assert!(
        !desc.is_empty(),
        "search_tools is missing or has an empty description"
    );
    for want in ["list_publishers", "list_books"] {
        assert!(
            desc.contains(want),
            "search_tools description does not advertise gated tool {want:?}"
        );
    }
    assert!(
        !desc.contains("get_current_time"),
        "search_tools description lists core tool get_current_time"
    );
    // The base description mentions search_tools itself, so check only the list.
    let Some((_, advertised)) = desc.split_once("Available to load") else {
        panic!("search_tools description = {desc:?}, want an Available to load list");
    };
    assert!(
        !advertised.contains(TOOL_NAME),
        "search_tools description advertises {TOOL_NAME:?} as loadable: {advertised:?}"
    );
}

#[tokio::test]
async fn test_reveal_tools_returns_state_error() {
    let state = Arc::new(FakeSession {
        state: FakeState {
            data: Mutex::new(Map::new()),
            set_err: Some("state unavailable"),
        },
    });
    let err = reveal_tools(&new_tool_ctx(&state), &["list_books"]).unwrap_err();
    assert!(
        wraps(&err, "state unavailable"),
        "RevealTools() error = {err}, want it to wrap state unavailable"
    );
    assert_eq!(err.to_string(), "state unavailable");

    let base = static_toolset(make_tools(&[("list_books", "list books")]));
    let err = execute_search(
        &new_tool_ctx(&state),
        SearchArgs {
            query: "select:list_books".into(),
        },
        base.as_ref(),
        &HashSet::new(),
        &BTreeMap::new(),
        8,
    )
    .await
    .unwrap_err();
    assert!(
        wraps(&err, "state unavailable"),
        "executeSearch() error = {err}, want it to wrap state unavailable"
    );
    assert_eq!(
        err.to_string(),
        "toolsearch: persist discovered tools: state unavailable"
    );
}

#[tokio::test]
async fn test_search_reports_optional_connected_skill_and_reveals_tool() {
    let mut tools = make_tools(&[("get_current_time", "ask")]);
    tools.push(stub_tool(
        "list_recent_files",
        "list recently modified files",
    ));
    let base = static_toolset(tools);
    let gts = must_new(
        base.clone(),
        Config {
            core_tool_names: vec!["get_current_time".into()],
            skill_annotations: BTreeMap::from([(
                "list_recent_files".to_string(),
                "file-browser".to_string(),
            )]),
            ..Default::default()
        },
    );

    check_names(
        &must_tool_names(gts.as_ref(), &new_tool_ctx(&empty_state())).await,
        &[TOOL_NAME],
        &[],
    );

    let state = empty_state();
    let out = must_search(
        &new_tool_ctx(&state),
        "recent files",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    assert_eq!(
        out.matches.len(),
        1,
        "executeSearch() returned {:?}, want 1 match",
        out.matches
    );
    let got = &out.matches[0];
    assert!(
        got.name == "list_recent_files" && got.connected_skill == "file-browser",
        "match = {got:?}, want list_recent_files with connected_skill file-browser"
    );
    assert_eq!(
        discovered(&state, TEST_AGENT),
        ["list_recent_files"],
        "discoveredNames() mismatch"
    );
    for want in [
        "could be relevant to your task",
        "load only the most relevant one",
    ] {
        assert!(
            out.next_step.contains(want),
            "next_step = {:?}, want it to contain {want:?}",
            out.next_step
        );
    }

    check_names(
        &must_tool_names(gts.as_ref(), &new_tool_ctx(&state)).await,
        &["list_recent_files"],
        &[],
    );
}

/// executeSearch plumbs the MaxResults cap through to the ranker and surfaces the truncation
/// note.
#[tokio::test]
async fn test_search_capped_results_include_note() {
    let base = static_toolset(make_tools(&[
        ("tool_a", "some tool"),
        ("tool_b", "some tool"),
        ("tool_c", "some tool"),
    ]));
    let gts = must_new(
        base.clone(),
        Config {
            max_results: 2,
            ..Default::default()
        },
    );

    let out = must_search(
        &new_tool_ctx(&empty_state()),
        "tool",
        base.as_ref(),
        &gts,
        2,
    )
    .await;
    assert_eq!(out.matches.len(), 2, "executeSearch() matches");
    assert!(
        !out.note.is_empty(),
        "executeSearch() note is empty, want a truncation note"
    );
}

/// Config.MaxResults reaches the search tool New builds; zero or a negative value means the
/// documented default of 8.
#[tokio::test]
async fn test_new_max_results() {
    let tools: Vec<Arc<dyn Tool>> = (0..10)
        .map(|i| stub_tool(&format!("tool_{i:02}"), "lists books"))
        .collect();
    let base = static_toolset(tools);
    for (max_results, want) in [(2, 2), (0, 8), (-1, 8)] {
        let gts = must_new(
            base.clone(),
            Config {
                max_results,
                ..Default::default()
            },
        );
        let run = gts
            .search_tool
            .as_runnable()
            .unwrap_or_else(|| panic!("search tool has no Run method"));
        let got = run
            .run(new_tool_ctx(&empty_state()), json!({"query": "books"}))
            .await
            .unwrap_or_else(|e| panic!("MaxResults {max_results}: search Run() error = {e}"));
        let n = got
            .get("matches")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        assert_eq!(
            n, want,
            "MaxResults {max_results}: search returned {n} matches, want {want}"
        );
    }
}

/// executeSearch handles the "select:a,b" form and persists the selected tools to the
/// discovered state.
#[tokio::test]
async fn test_search_select_by_name() {
    let base = static_toolset(make_tools(&[
        ("patch_book", "patch a book"),
        ("update_book", "update a book"),
        ("list_books", "list books"),
    ]));
    let gts = must_new(base.clone(), Config::default());

    let state = empty_state();
    let out = must_search(
        &new_tool_ctx(&state),
        "select:patch_book,update_book,unknown_tool",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    check_names(
        &match_names(&out.matches),
        &["patch_book", "update_book"],
        &["list_books"],
    );
    assert!(
        out.note.contains("unknown_tool"),
        "note = {:?}, want the missing tool name \"unknown_tool\" in it",
        out.note
    );
    assert_eq!(
        discovered(&state, TEST_AGENT),
        ["patch_book", "update_book"],
        "selected tools must be persisted to discovered state"
    );
}

// parity: adapted: every Rust Tool has process_request, so Go's non-packable stub is a tool declaring nothing, and the declaring-but-unpackable case cannot exist and is dropped
#[tokio::test]
async fn test_build_items_skips_non_packable_tools() {
    let catalog = vec![
        stub_tool("packable_tool", "can be called"),
        nil_declaration_tool("non_packable_tool", "cannot be called"),
    ];
    check_names(
        &item_names(&build_items(&catalog)),
        &["packable_tool"],
        &["non_packable_tool"],
    );
}

/// Go `updateNoteArgs`: the description becomes the property description the search indexes.
#[allow(dead_code)] // the field is only decoded, never read
#[derive(Deserialize, schemars::JsonSchema)]
struct UpdateNoteArgs {
    #[schemars(description = "the comment text to attach")]
    body: String,
}

/// A tool is discoverable when the query matches only an argument's description. It uses a real
/// function tool so the argument schema is exposed as `parameters_json_schema`.
#[tokio::test]
async fn test_search_indexes_arguments() {
    let update_note = FunctionTool::new(
        FunctionToolConfig::new("update_note", "modify a note"),
        |_ctx: Context, _args: UpdateNoteArgs| async { Ok::<_, ToolError>(NoResult {}) },
    )
    .unwrap_or_else(|e| panic!("functiontool.New() error = {e}"));

    let base = static_toolset(vec![
        update_note,
        stub_tool("list_images", "list all images"),
    ]);
    let gts = must_new(base.clone(), Config::default());

    // "comment" appears only in the body argument's description.
    let out = must_search(
        &new_tool_ctx(&empty_state()),
        "comment",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    check_names(
        &match_names(&out.matches),
        &["update_note"],
        &["list_images"],
    );
}

/// Go `typedParamsTool`: declares its arguments with a typed `Schema`.
fn typed_params_tool(name: &str, desc: &str) -> Arc<dyn Tool> {
    tool_with(
        name,
        desc,
        Some(FunctionDeclaration {
            name: name.into(),
            description: desc.into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: [
                    (
                        "zipcode".to_string(),
                        Schema {
                            r#type: Some(Type::String),
                            description: "the comment text to attach".into(),
                            ..Default::default()
                        },
                    ),
                    ("unused".to_string(), Schema::default()),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
}

// parity: adapted: a BTreeMap<String, Schema> cannot hold Go's nil property schema, so "unused" is an empty Schema (it contributes its name and an empty description)
#[tokio::test]
async fn test_search_indexes_typed_parameters() {
    let base = static_toolset(vec![
        typed_params_tool("update_note", "modify a note"),
        stub_tool("list_images", "list all images"),
    ]);
    let gts = must_new(base.clone(), Config::default());
    for query in ["comment", "zipcode"] {
        let out = must_search(&new_tool_ctx(&empty_state()), query, base.as_ref(), &gts, 8).await;
        check_names(
            &match_names(&out.matches),
            &["update_note"],
            &["list_images"],
        );
    }
}

#[tokio::test]
async fn test_search_annotates_base_tool_with_skill_annotation() {
    let base = static_toolset(make_tools(&[
        ("list_folders", "list folders in the workspace"),
        ("create_folder", "create a new folder"),
        ("list_notes", "list notes"),
    ]));
    let gts = must_new(
        base.clone(),
        Config {
            skill_annotations: BTreeMap::from([
                ("list_folders".to_string(), "folder-browser".to_string()),
                ("create_folder".to_string(), "folder-browser".to_string()),
            ]),
            ..Default::default()
        },
    );

    let out = must_search(
        &new_tool_ctx(&empty_state()),
        "folders",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    assert!(
        !out.matches.is_empty(),
        "executeSearch() returned no matches"
    );
    for m in &out.matches {
        match m.name.as_str() {
            "list_folders" | "create_folder" => assert_eq!(
                m.connected_skill, "folder-browser",
                "{} connected_skill = {:?}, want folder-browser",
                m.name, m.connected_skill
            ),
            "list_notes" => assert!(
                m.connected_skill.is_empty(),
                "unannotated tool list_notes has connected_skill {:?}",
                m.connected_skill
            ),
            _ => {}
        }
    }
}

/// Two search_tools calls from one model response through the real runner: each call gets its
/// own state delta, so a discovery written by one must survive the other.
#[tokio::test]
async fn test_search_parallel_calls_keep_all_discoveries() {
    let gated = must_new(
        static_toolset(vec![new_tool("add_numbers"), new_tool("multiply_numbers")]),
        Config::default(),
    );
    let llm = parallel_call_model(
        &[
            ("1", "select:add_numbers"),
            ("2", "select:multiply_numbers"),
        ],
        1,
    );
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = new_runner("calculator", llm.clone(), gated, svc);
    run_turn(&r, "run", None).await;
    check_names(&declared(&llm), &["add_numbers", "multiply_numbers"], &[]);
}

// parity: adapted: JSON state has no int/int64 distinction, so Go's int and int64 positions are JSON integers and its float64 is a JSON float
#[tokio::test]
async fn test_discovered_names_order_and_filtering() {
    let p = STATE_KEY_PREFIX;
    let state = new_fake_state(Map::from_iter([
        (format!("{p}test_agent:z_tool"), json!(0.0)),
        (format!("{p}test_agent:e_tool"), json!(1)),
        (format!("{p}test_agent:c_tool"), json!(1)),
        (format!("{p}test_agent:b_tool"), json!(1)),
        (format!("{p}test_agent:d_tool"), json!(1)),
        (format!("{p}test_agent:a_tool"), json!(1)),
        (format!("{p}test_agent:y_tool"), json!(2_i64)),
        (format!("{p}test_agent:bad_value"), json!("1")),
        (format!("{p}test_agent:"), json!(0)),
        (format!("{p}test_agent_2:other_tool"), json!(0)),
        ("unrelated".to_string(), json!(0)),
    ]));
    assert_eq!(
        discovered(&state, TEST_AGENT),
        [
            "z_tool",
            "a_tool",
            "b_tool",
            "c_tool",
            "d_tool",
            "e_tool",
            "y_tool",
            "bad_value"
        ],
        "discoveredNames() mismatch"
    );
}

/// Agents sharing a session see only their own discoveries. Agent and tool names may both
/// contain a colon, so agent "a" revealing "b:t" must not hand agent "a:b" the tool "t".
#[tokio::test]
async fn test_reveal_tools_keeps_discoveries_per_agent() {
    let agents = ["a", "a:b", "other"];
    for (agent, tool) in [("a", "b:t"), ("a:b", "t"), ("other", "github:create_issue")] {
        let state = empty_state();
        reveal_tools(&new_agent_tool_ctx(&state, agent), &[tool])
            .unwrap_or_else(|e| panic!("RevealTools({agent:?}) error = {e}"));
        for agent_name in agents {
            let want: Vec<String> = if agent_name == agent {
                vec![tool.to_string()]
            } else {
                Vec::new()
            };
            assert_eq!(
                discovered(&state, agent_name),
                want,
                "after {agent:?} revealed {tool:?}, discoveredNames({agent_name:?}) mismatch"
            );
        }
    }
}

// parity: adapted: Arc<dyn Toolset> cannot be null, so the nil-base half has no Rust equivalent (spec R8); the reserved core name half is ported with Go's exact text
#[tokio::test]
async fn test_new_rejects_invalid_config() {
    match new(
        static_toolset(Vec::new()),
        Config {
            core_tool_names: vec![TOOL_NAME.into()],
            ..Default::default()
        },
    ) {
        Ok(_) => panic!("New() with core tool {TOOL_NAME:?} error = nil, want an error"),
        Err(e) => assert_eq!(
            e.to_string(),
            "toolsearch: core tool name \"search_tools\" is reserved for the search tool"
        ),
    }
}

/// A base catalog containing a tool named search_tools runs through the real runner: selecting
/// it must not reveal it, and a recorded name must not make Tools return a second search_tools.
#[tokio::test]
async fn test_search_base_tool_named_search_tools_is_never_exposed() {
    let base = static_toolset(vec![new_tool(TOOL_NAME), new_tool("other_tool")]);
    let gated = must_new(base.clone(), Config::default());
    let select = format!("select:{TOOL_NAME}");
    let llm = parallel_call_model(&[("1", select.as_str())], 2);
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = new_runner("agent", llm, gated.clone(), svc);
    for turn in 1..=2 {
        run_turn(&r, &format!("turn {turn}"), None).await;
    }

    let out = must_search(
        &new_tool_ctx(&empty_state()),
        &select,
        base.as_ref(),
        &gated,
        8,
    )
    .await;
    check_names(&match_names(&out.matches), &[], &[TOOL_NAME]);

    // A name recorded before, e.g. by RevealTools, must not duplicate it either.
    let names = must_tool_names(
        gated.as_ref(),
        &new_tool_ctx(&discovered_state(&[TOOL_NAME, "other_tool"])),
    )
    .await;
    assert_eq!(names, [TOOL_NAME, "other_tool"], "Tools() mismatch");
}

#[tokio::test]
async fn test_gating_toolset_tools_returns_base_error() {
    let gts = must_new(
        Arc::new(FailingToolset {
            err: "catalog down",
        }),
        Config::default(),
    );
    let ctx = new_tool_ctx(&empty_state());
    let Err(err) = gts.tools(ctx.as_readonly()).await else {
        panic!("Tools() error = nil, want catalog down");
    };
    assert!(
        wraps(&err, "catalog down"),
        "Tools() error = {err}, want it to wrap catalog down"
    );
    assert_eq!(err.to_string(), "catalog down");
}

/// A model-side built-in declares no function, so search does not offer it.
#[tokio::test]
async fn test_search_skips_tools_without_function_declaration() {
    let base = static_toolset(vec![
        Arc::new(gemini::GoogleSearch) as Arc<dyn Tool>,
        stub_tool("web_lookup", "search the web"),
    ]);
    let gts = must_new(base.clone(), Config::default());
    for query in ["select:google_search", "google search web"] {
        let out = must_search(&new_tool_ctx(&empty_state()), query, base.as_ref(), &gts, 8).await;
        check_names(&match_names(&out.matches), &[], &["google_search"]);
    }
}

// parity: adapted: Go's typed-nil *jsonschema.Schema becomes a JSON null parameters_json_schema, the Rust value that holds no schema
#[tokio::test]
async fn test_search_typed_nil_schema_does_not_panic() {
    let base = static_toolset(vec![tool_with(
        "typed_nil",
        "typed nil schema",
        Some(FunctionDeclaration {
            name: "typed_nil".into(),
            parameters_json_schema: Some(Value::Null),
            ..Default::default()
        }),
    )]);
    let gts = must_new(base.clone(), Config::default());
    let out = must_search(
        &new_tool_ctx(&empty_state()),
        "typed",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    check_names(&match_names(&out.matches), &["typed_nil"], &[]);
}

// parity: adapted: Config is moved into new, so the caller can only mutate its own clone afterwards; the test pins that the search result carries the annotation given to new
#[tokio::test]
async fn test_new_copies_skill_annotations() {
    let base = static_toolset(make_tools(&[("list_folders", "list folders")]));
    let mut annotations =
        BTreeMap::from([("list_folders".to_string(), "folder-browser".to_string())]);
    let gts = must_new(
        base,
        Config {
            skill_annotations: annotations.clone(),
            ..Default::default()
        },
    );
    annotations.insert("list_folders".to_string(), "changed-after-new".to_string());

    let ctx = new_tool_ctx(&empty_state());
    let tools = gts
        .tools(ctx.as_readonly())
        .await
        .unwrap_or_else(|e| panic!("Tools() error = {e}"));
    let search = tools
        .iter()
        .find(|t| t.name() == TOOL_NAME)
        .unwrap_or_else(|| panic!("search tool missing from {:?}", tool_names(&tools)));
    let runner = search
        .as_runnable()
        .unwrap_or_else(|| panic!("search tool has no Run method"));
    let got = runner
        .run(
            new_tool_ctx(&empty_state()),
            json!({"query": "select:list_folders"}),
        )
        .await
        .unwrap_or_else(|e| panic!("search Run() error = {e}"));
    let text = serde_json::to_string(&got).unwrap();
    assert!(
        text.contains("folder-browser") && !text.contains("changed-after-new"),
        "search result = {text}, want the connected skill given to New"
    );
}

/// Two overlapping select: calls in one model response through the real runner. Each call must
/// either return zebra_tool or say it is already available.
#[tokio::test]
async fn test_search_overlapping_parallel_calls_report_already_available() {
    for i in 0..20 {
        let gated = must_new(
            static_toolset(vec![new_tool("zebra_tool"), new_tool("apple_tool")]),
            Config::default(),
        );
        let llm = parallel_call_model(
            &[
                ("1", "select:zebra_tool"),
                ("2", "select:zebra_tool,apple_tool"),
            ],
            1,
        );
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let r = new_runner("agent", llm.clone(), gated, svc);
        let mut responses = 0;
        for ev in run_turn(&r, &format!("run {i}"), None).await {
            let Some(content) = &ev.response.content else {
                continue;
            };
            for p in &content.parts {
                let Some(fr) = &p.function_response else {
                    continue;
                };
                responses += 1;
                // Go decodeSearchOutput: the function response decoded back to searchOutput.
                let out: SearchOutput =
                    serde_json::from_value(Value::Object(fr.response.clone())).unwrap();
                assert!(
                    match_names(&out.matches).iter().any(|n| n == "zebra_tool")
                        || out.note.contains("already available: zebra_tool"),
                    "run {i}: search_tools neither returned zebra_tool nor said it is already available: {out:?}"
                );
            }
        }
        assert_eq!(responses, 2, "run {i}: search_tools responses");
        check_names(&declared(&llm), &["zebra_tool", "apple_tool"], &[]);
    }
}

// parity: adapted: no MCP toolset until 2b; the stub declares the raw map schema mcptoolset stores, as jsonschema-go infers it for Go's `in` struct
#[tokio::test]
async fn test_search_indexes_mcp_arguments() {
    let mcp_schema = json!({
        "type": "object",
        "properties": {
            "zipcode": {"type": "string", "description": "postal code of the location"}
        },
        "required": ["zipcode"],
        "additionalProperties": false
    });
    let base = static_toolset(vec![tool_with(
        "get_weather",
        "returns weather",
        Some(FunctionDeclaration {
            name: "get_weather".into(),
            description: "returns weather".into(),
            parameters_json_schema: Some(mcp_schema),
            ..Default::default()
        }),
    )]);
    let gts = must_new(base.clone(), Config::default());
    let out = must_search(
        &new_tool_ctx(&empty_state()),
        "postal code",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    check_names(&match_names(&out.matches), &["get_weather"], &[]);
}

/// select: calls a name already available only when Tools returns it. A discovered tool the
/// base no longer returns, and a core name the base never had, are not found.
#[tokio::test]
async fn test_search_already_available_only_when_tools_returns_it() {
    let base = static_toolset(make_tools(&[
        ("list_books", "list books"),
        ("core_tool", "core"),
        ("other_tool", "other"),
    ]));
    let gts = must_new(
        base.clone(),
        Config {
            core_tool_names: vec!["core_tool".into(), "ghost_core".into()],
            ..Default::default()
        },
    );
    let state = discovered_state(&["list_books", "removed_tool"]);
    let out = must_search(
        &new_tool_ctx(&state),
        "select:list_books,core_tool,removed_tool,ghost_core",
        base.as_ref(),
        &gts,
        8,
    )
    .await;
    assert_eq!(
        out.note,
        "tools not found: removed_tool, ghost_core. already available: list_books, core_tool"
    );
}

#[tokio::test]
async fn test_build_items_skips_tools_with_nil_declaration() {
    let catalog = vec![
        stub_tool("declared_tool", "declares a function"),
        nil_declaration_tool("undeclared_tool", "declares nothing"),
    ];
    check_names(
        &item_names(&build_items(&catalog)),
        &["declared_tool"],
        &["undeclared_tool"],
    );
}

// parity: adapted: no database session service until sub-project 4; both cases use the in-memory service, and "float_order" writes the first discovery back as the float Go's sqlite JSON decode returns (through the second turn's user-message state delta)
#[tokio::test]
async fn test_search_discoveries_survive_reload() {
    let zebra_key = format!("{STATE_KEY_PREFIX}agent:zebra_tool");
    for (case, float_order) in [("inmemory", false), ("float_order", true)] {
        let gated = must_new(
            static_toolset(vec![new_tool("apple_tool"), new_tool("zebra_tool")]),
            Config::default(),
        );
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let mut last_declared = Vec::new();
        // Discovery order deliberately differs from name order.
        for (turn, query) in ["select:zebra_tool", "select:apple_tool"]
            .into_iter()
            .enumerate()
        {
            let llm = parallel_call_model(&[("1", query)], 1);
            let r = new_runner("agent", llm.clone(), gated.clone(), svc.clone());
            let state_delta = (float_order && turn == 1)
                .then(|| Map::from_iter([(zebra_key.clone(), json!(0.0))]));
            run_turn(&r, &format!("{case}: Run({query:?})"), state_delta).await;
            last_declared = declared(&llm);
        }
        assert_eq!(
            last_declared,
            [TOOL_NAME, "zebra_tool", "apple_tool"],
            "{case}: second turn declared tools mismatch"
        );
        let session = svc
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "session".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let stored = session.state().get(&zebra_key).unwrap();
        assert_eq!(
            stored.is_f64(),
            float_order,
            "{case}: stored zebra position {stored}"
        );
    }
}

/// Config.Description replaces the default description of the search tool, and the gated tool
/// names are still appended.
#[tokio::test]
async fn test_new_description() {
    let base = static_toolset(make_tools(&[("list_controls", "list controls")]));
    let gts = must_new(
        base,
        Config {
            description: "Search compliance tools by topic: controls, obligations, frameworks."
                .into(),
            gated_tool_names: vec!["list_controls".into()],
            skill_annotations: BTreeMap::from([(
                "list_controls".to_string(),
                "compliance".to_string(),
            )]),
            ..Default::default()
        },
    );
    let want = ranksearch::describe_search(
        "Search compliance tools by topic: controls, obligations, frameworks.",
        &["list_controls".to_string()],
    );
    assert_eq!(
        gts.search_tool.description(),
        want,
        "search tool description"
    );
}

/// The default description mentions connected_skill only when SkillAnnotations is set.
#[tokio::test]
async fn test_new_skill_hint_only_with_annotations() {
    let base = static_toolset(make_tools(&[("list_folders", "list folders")]));
    for (annotations, want) in [
        (BTreeMap::new(), false),
        (
            BTreeMap::from([("list_folders".to_string(), "folder-browser".to_string())]),
            true,
        ),
    ] {
        let gts = must_new(
            base.clone(),
            Config {
                skill_annotations: annotations.clone(),
                ..Default::default()
            },
        );
        let got = gts.search_tool.description().contains("connected_skill");
        assert_eq!(
            got, want,
            "SkillAnnotations {annotations:?}: description mentions connected_skill"
        );
    }
}
