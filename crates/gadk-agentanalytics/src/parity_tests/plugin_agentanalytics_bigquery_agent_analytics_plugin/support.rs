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

//! The Go test helpers shared by the 7b-3a ports: ADK contexts for direct callback
//! tests (Go `mockInvocationContext`), the plugin over the in-process `FakeWriteServer` with its
//! decoded rows (Go `fakeBigQueryWriteServer`), the row-capturing plugins, and scripted models /
//! agents for runner-level tests.
//!
//! Go → gadk: `mockInvocationContext` = [`MockCtx`] (its ADK contexts carry a [`MockSession`]
//! with Go's identity fields, a custom agent named `agent_name`, and the path / function call
//! of a tool context); `setupTestPlugin` / `newTestPluginWithConfig` = [`setup_test_plugin`] /
//! [`new_test_plugin_with_config`] (the plugin over [`FakeWriteServer`], whose requests are Go's
//! `requests` channel: [`TestPlugin::rows`] / [`TestPlugin::collect_rows`]);
//! `newClockedPlugin` = [`new_clocked_plugin`]; `newTraceOnlyPlugin` / `newQueuedRowsPlugin` =
//! [`new_trace_only_plugin`] / [`new_queued_rows_plugin`]; `newTestRunner` /
//! `newTestAgent` = [`new_test_runner`] / [`new_test_agent`]; the row readers of
//! `trace_manager_test.go` (`placeOf`, `rowLatency`, `functionCallIDOf`) and
//! `arrow_test_helper_test.go` (`jsonColumnValue`, `jsonObjectColumn`, `untilEventType`,
//! `untilInvocationEnd`). Go's `lookup` / `checkJSON` are `crate::bigquery::fake::{lookup,
//! check_json}`; `eventTypes` / `rowsOfType` / `onlyRowOfType` are re-exported from the
//! batch-processor support.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentError, ContextDelta, InvocationContext, Plugin, RunConfig, Tool, ToolOriginKind,
};
use gadk_engine::telemetry::d6::SpanIds;
use gadk_engine::{RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, FunctionDeclaration};
use gadk_model::rest::test_util::{FakeResponse, ScriptedClient};
use gadk_session::{Event, InMemorySessionService, Session, SessionError, State};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::batch_processor::BatchProcessor;
use crate::bigquery::BigQueryClient;
use crate::config::{ClientOptions, Config};
use crate::context::Context;
use crate::plugin::callbacks::callbacks_config;
use crate::plugin::{
    AnalyticsPlugin, PLUGIN_NAME, new_bigquery_agent_analytics_plugin_with_clients,
};
use crate::redaction::CustomMetadataFilter;
use crate::row::{Row, RowValue};
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::trace::testkit::ManualClock;
use crate::trace::{CallbackCtx, TraceManager};
use crate::validate::validate_config;

pub(crate) use crate::bigquery::fake::check_json;
pub(crate) use crate::bigquery::fake::lookup;
pub(crate) use crate::parity_tests::plugin_agentanalytics_batch_processor::support::{
    decode_arrow_rows, event_types, only_row_of_type, rows_of_type, until_event_type,
};

// ---------------------------------------------------------------------------------------------
// ADK contexts (Go mockInvocationContext, mapState)
// ---------------------------------------------------------------------------------------------

/// Go `mapState`: a session state backed by a map.
#[derive(Default)]
pub(crate) struct MapState(parking_lot::Mutex<Map<String, Value>>);

impl State for MapState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.0
            .lock()
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }
    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.0.lock().insert(key.to_owned(), value);
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        self.0
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// The session behind a [`MockCtx`]: Go's `SessionID` / `AppName` / `UserID` and `state`.
pub(crate) struct MockSession {
    pub(crate) id: String,
    pub(crate) app_name: String,
    pub(crate) user_id: String,
    pub(crate) state: MapState,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app_name
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> Vec<Event> {
        Vec::new()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `mockInvocationContext`: the identity a direct callback test hands the plugin. Build the
/// ADK contexts with [`invocation`](Self::invocation) (invocation callbacks) and
/// [`context`](Self::context) (agent / model / tool callbacks). One `MockCtx` is one Go `mCtx`,
/// one activation: the first call builds the invocation context (agent, session, activation)
/// and later calls, on it or on its clones (e.g. [`with_function_call`](Self::with_function_call)),
/// reuse it; a struct-update copy that changes an identity field gets (and keeps) a context of
/// its own. A `MockCtx` built from `MockCtx::default()` / `new()` / `named()` starts a new
/// activation, as each Go `&mockInvocationContext{…}` does. Go's `state` nil = an empty state (a gadk session always has one).
#[derive(Clone)]
pub(crate) struct MockCtx {
    pub(crate) agent_name: String,
    pub(crate) session_id: String,
    pub(crate) invocation_id: String,
    pub(crate) user_id: String,
    pub(crate) app_name: String,
    pub(crate) branch: String,
    pub(crate) path: String,
    pub(crate) function_call_id: String,
    pub(crate) state: Map<String, Value>,
    /// Go `ctx`'s cancellation (Go `context.WithCancel(t.Context())`).
    pub(crate) cancel: CancellationToken,
    /// The agent of the contexts (a custom agent named `agent_name` unless set), e.g. a
    /// runner-tree agent for tests that look agents up by name.
    pub(crate) agent: Option<Arc<dyn Agent>>,
    /// The invocation context built for each set of identity fields (see the type docs).
    pub(crate) activation: Arc<parking_lot::Mutex<Vec<(MockKey, InvocationContext)>>>,
}

/// The [`MockCtx`] fields its invocation context is built from (all but `function_call_id`).
#[derive(Clone, PartialEq)]
pub(crate) struct MockKey {
    fields: [String; 7],
    state: Map<String, Value>,
    /// The address of `MockCtx::agent` (compared, never dereferenced).
    agent: Option<usize>,
}

impl Default for MockCtx {
    fn default() -> MockCtx {
        MockCtx {
            agent_name: String::new(),
            session_id: String::new(),
            invocation_id: String::new(),
            user_id: String::new(),
            app_name: String::new(),
            branch: String::new(),
            path: String::new(),
            function_call_id: String::new(),
            state: Map::new(),
            cancel: CancellationToken::new(),
            agent: None,
            activation: Arc::default(),
        }
    }
}

impl MockCtx {
    /// Go `setupTestPlugin`'s `mCtx`: agent `test-agent`, session `sess-id`, invocation
    /// `inv-id`, user `user-id`.
    pub(crate) fn new() -> MockCtx {
        MockCtx {
            agent_name: "test-agent".into(),
            session_id: "sess-id".into(),
            invocation_id: "inv-id".into(),
            user_id: "user-id".into(),
            ..MockCtx::default()
        }
    }

    /// Go `&mockInvocationContext{ctx: t.Context(), agentName: agent, invocationID: inv}`.
    pub(crate) fn named(agent_name: &str, invocation_id: &str) -> MockCtx {
        MockCtx {
            agent_name: agent_name.into(),
            invocation_id: invocation_id.into(),
            ..MockCtx::default()
        }
    }

    /// Go `withFunctionCall(ctx, id)`: a copy for the tool call `id`.
    pub(crate) fn with_function_call(&self, id: &str) -> MockCtx {
        MockCtx {
            function_call_id: id.into(),
            ..self.clone()
        }
    }

    /// The agent of the contexts.
    pub(crate) fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone().unwrap_or_else(|| {
            gadk_agent::new_agent(gadk_agent::AgentConfig {
                name: self.agent_name.clone(),
                ..Default::default()
            })
            .expect("custom agent")
        })
    }

    fn key(&self) -> MockKey {
        MockKey {
            fields: [
                self.agent_name.clone(),
                self.session_id.clone(),
                self.invocation_id.clone(),
                self.user_id.clone(),
                self.app_name.clone(),
                self.branch.clone(),
                self.path.clone(),
            ],
            state: self.state.clone(),
            agent: self
                .agent
                .as_ref()
                .map(|a| Arc::as_ptr(a).cast::<()>() as usize),
        }
    }

    /// The invocation context (Go's `mCtx` passed to an invocation callback): built once per
    /// activation (see the type docs).
    pub(crate) fn invocation(&self) -> InvocationContext {
        let key = self.key();
        let mut built = self.activation.lock();
        if let Some((_, ictx)) = built.iter().find(|(k, _)| *k == key) {
            return ictx.clone();
        }
        let ictx = self.build_invocation();
        built.push((key, ictx.clone()));
        ictx
    }

    fn build_invocation(&self) -> InvocationContext {
        let session = MockSession {
            id: self.session_id.clone(),
            app_name: self.app_name.clone(),
            user_id: self.user_id.clone(),
            state: MapState(parking_lot::Mutex::new(self.state.clone())),
        };
        let ictx = gadk_agent::Context::builder()
            .agent(self.agent())
            .session(Arc::new(session))
            .invocation_id(self.invocation_id.clone())
            .branch(self.branch.clone())
            .cancellation_token(self.cancel.clone())
            .build_invocation();
        if self.path.is_empty() {
            ictx
        } else {
            ictx.with_delta(ContextDelta {
                path: Some(self.path.clone()),
                ..ContextDelta::default()
            })
        }
    }

    /// The callback context (Go's `mCtx` passed to an agent / model / tool callback); a
    /// function call ID makes it a tool context.
    pub(crate) fn context(&self) -> gadk_agent::Context {
        let ctx = self.invocation().into_context();
        if self.function_call_id.is_empty() {
            ctx
        } else {
            ctx.with_delta(ContextDelta {
                function_call_id: Some(self.function_call_id.clone()),
                ..ContextDelta::default()
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The plugin over the fake write server (Go newTestPluginWithConfig, fakeBigQueryWriteServer)
// ---------------------------------------------------------------------------------------------

/// Go `newTestPluginWithConfig`'s BigQuery client (its `mockTransport`): a job request answers
/// a done job, every other request `200 {}`.
pub(crate) fn test_bq_client() -> Arc<BigQueryClient> {
    let http = ScriptedClient::new(|r| {
        if r.path.contains("/jobs") {
            FakeResponse::json(
                r#"{"jobReference":{"projectId":"test-project","jobId":"test-job"},"status":{"state":"DONE"}}"#,
            )
        } else {
            FakeResponse::json("{}")
        }
    });
    let options = ClientOptions {
        http_client: Some(http),
        without_authentication: true,
        endpoint: Some("http://bq.test/bigquery/v2/".into()),
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new("test-project", &options).expect("bigquery client"))
}

/// Go's test config: `DefaultConfig()` enabled for `test-project.test_dataset.test-table`.
pub(crate) fn test_config() -> Config {
    Config {
        enabled: true,
        project_id: "test-project".into(),
        dataset_id: "test_dataset".into(),
        table_name: "test-table".into(),
        ..Config::default()
    }
}

/// What Go's `setupTestPlugin` returns: the plugin, its write server (Go `requests`), and
/// `mCtx`.
pub(crate) struct TestPlugin {
    pub(crate) plugin: Plugin,
    pub(crate) server: FakeWriteServer,
    pub(crate) ctx: MockCtx,
}

/// Go `newTestPluginWithConfig(t, mutate)`: the plugin of [`test_config`] adjusted by
/// `mutate`, with [`test_bq_client`] and a write client of a new [`FakeWriteServer`].
pub(crate) async fn new_test_plugin_with_config(mutate: impl FnOnce(&mut Config)) -> TestPlugin {
    let mut config = test_config();
    mutate(&mut config);
    let server = FakeWriteServer::new();
    let plugin = new_bigquery_agent_analytics_plugin_with_clients(
        &Context::background(),
        config,
        Some(test_bq_client()),
        Some(server.client().expect("write client")),
    )
    .await
    .expect("plugin");
    TestPlugin {
        plugin,
        server,
        ctx: MockCtx::new(),
    }
}

/// Go `setupTestPlugin(t)` (the test closes the plugin itself, or leaves it to the runtime).
pub(crate) async fn setup_test_plugin() -> TestPlugin {
    new_test_plugin_with_config(|_| {}).await
}

impl TestPlugin {
    /// Every row the server received so far, in order.
    pub(crate) fn rows(&self) -> Vec<Row> {
        self.server
            .append_requests()
            .iter()
            .flat_map(|r| decode_arrow_rows(&r.request))
            .collect()
    }

    /// Go `collectRows(t, requests, done)`: waits (up to 5 s, as Go's tests) for requests
    /// until `done` holds for the rows received so far.
    pub(crate) async fn collect_rows(&self, done: impl Fn(&[Row]) -> bool) -> Vec<Row> {
        let wait = async {
            loop {
                let n = self.server.append_requests().len();
                let rows = self.rows();
                if done(&rows) {
                    return rows;
                }
                self.server.wait_for_append_requests(n + 1).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out collecting rows; got {:?}",
                    event_types(&self.rows())
                )
            })
    }

    /// Go `p.Close()`.
    pub(crate) async fn close(&self) -> Result<(), AgentError> {
        let close = self.plugin.close().expect("close function");
        close().await
    }
}

/// Go `untilInvocationEnd`.
pub(crate) fn until_invocation_end(rows: &[Row]) -> bool {
    !rows_of_type(rows, crate::event_types::EVENT_INVOCATION_COMPLETED).is_empty()
        || !rows_of_type(rows, crate::event_types::EVENT_INVOCATION_ERROR).is_empty()
}

// ---------------------------------------------------------------------------------------------
// Row-capturing plugins (Go newClockedPlugin, newTraceOnlyPlugin, newQueuedRowsPlugin)
// ---------------------------------------------------------------------------------------------

/// Go `applyDefaults(DefaultConfig())`.
pub(crate) fn default_config() -> Config {
    Config::default().apply_defaults()
}

/// A batch processor that never starts (Go `NewBatchProcessor(t.Context(), nil, "test_stream",
/// config)`): its rows wait in the queue until taken.
pub(crate) async fn queue_processor(config: &Config) -> Arc<BatchProcessor> {
    BatchProcessor::new(&Context::background(), None, "test_stream", config.clone())
        .await
        .expect("batch processor")
}

/// Go `clockedPlugin`: an analytics plugin whose trace manager reads [`ManualClock`] and whose
/// rows wait in a processor that never starts.
pub(crate) struct ClockedPlugin {
    pub(crate) plugin: Arc<AnalyticsPlugin>,
    pub(crate) clock: ManualClock,
}

/// Go `newClockedPlugin(t)` (Go's clock starts at `time.Unix(1000, 0)`; [`ManualClock`] at
/// 2026-01-01, and only differences are observable).
pub(crate) async fn new_clocked_plugin() -> ClockedPlugin {
    let config = default_config();
    let processor = queue_processor(&config).await;
    let clock = ManualClock::new();
    let mut traces = TraceManager::new();
    traces.now = clock.clock();
    let plugin = Arc::new(AnalyticsPlugin {
        config,
        processor: Some(processor),
        traces: Some(Arc::new(traces)),
        ..AnalyticsPlugin::default()
    });
    ClockedPlugin { plugin, clock }
}

impl ClockedPlugin {
    /// Go `takeRows()`: the rows queued since the last call.
    pub(crate) fn take_rows(&self) -> Vec<Row> {
        take_rows(self.plugin.processor.as_ref())
    }
}

/// The rows waiting in `processor`'s queue (Go `processor.drainQueue()`).
pub(crate) fn take_rows(processor: Option<&Arc<BatchProcessor>>) -> Vec<Row> {
    processor
        .map(|p| p.drain_queue().into_iter().map(|q| q.row).collect())
        .unwrap_or_default()
}

/// Go `newTraceOnlyPlugin(t, tm)`: the production callbacks over `tm`, writing no rows
/// (every event type is filtered out).
pub(crate) fn new_trace_only_plugin(tm: Arc<TraceManager>) -> Plugin {
    let mut config = default_config();
    config.disable_flush_on_run_end = true;
    let ap = Arc::new(AnalyticsPlugin {
        config,
        allowlist: ["NO_SUCH_EVENT".to_owned()].into(),
        traces: Some(tm),
        ..AnalyticsPlugin::default()
    });
    Plugin::new(callbacks_config("trace-only", &ap))
}

/// Go `newQueuedRowsPlugin(t, tm)`: the production callbacks over `tm`, whose rows wait in a
/// processor that never starts; take them with [`take_rows`] on the returned processor.
pub(crate) async fn new_queued_rows_plugin(tm: Arc<TraceManager>) -> (Plugin, Arc<BatchProcessor>) {
    let mut config = default_config();
    config.disable_flush_on_run_end = true;
    let processor = queue_processor(&config).await;
    let ap = Arc::new(AnalyticsPlugin {
        config,
        processor: Some(Arc::clone(&processor)),
        traces: Some(tm),
        ..AnalyticsPlugin::default()
    });
    (Plugin::new(callbacks_config("queued-rows", &ap)), processor)
}

/// A plugin with the production callbacks over `ap` (Go `baseplugin.New` of a test literal
/// registering `ap`'s callbacks).
pub(crate) fn plugin_of(name: &str, ap: &Arc<AnalyticsPlugin>) -> Plugin {
    Plugin::new(callbacks_config(name, ap))
}

// ---------------------------------------------------------------------------------------------
// Runners and agents (Go newTestRunner, newTestAgent)
// ---------------------------------------------------------------------------------------------

/// Go `newTestRunner(t, root, plugins...)`: app `test-app`, an in-memory session service with
/// auto-created sessions, the plugins in order.
pub(crate) fn new_test_runner(root: Arc<dyn Agent>, plugins: Vec<Plugin>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: "test-app".into(),
        agent: root,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: gadk_engine::runner::PluginConfig {
            plugins,
            ..Default::default()
        },
        auto_create_session: true,
        platform: gadk_platform::Platform::default(),
        compaction: None,
    })
    .expect("runner")
}

/// Go `r.Run(ctx, user, session, textContent("user", text), cfg)` iterated to the end: the
/// events and the errors it yielded, in order.
pub(crate) async fn run_turn(
    runner: &Runner,
    user_id: &str,
    session_id: &str,
    text: &str,
    cfg: RunConfig,
) -> (Vec<Event>, Vec<AgentError>) {
    let mut events = Vec::new();
    let mut errs = Vec::new();
    let mut stream = runner.run(
        user_id,
        session_id,
        Some(Content::from_text(text, "user")),
        cfg,
        RunOptions::default(),
    );
    while let Some(item) = stream.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(err) => errs.push(err),
        }
    }
    (events, errs)
}

/// Go `newTestAgent(t, name, subAgents...)`: a custom agent that runs nothing.
pub(crate) fn new_test_agent(name: &str, sub_agents: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    crate::trace::testkit::custom(name, sub_agents)
}

// ---------------------------------------------------------------------------------------------
// Row readers (trace_manager_test.go, arrow_test_helper_test.go)
// ---------------------------------------------------------------------------------------------

/// A string column of `row`, or "" (Go `row[name].(string)`).
pub(crate) fn str_col(row: &Row, name: &str) -> String {
    match row.get(name) {
        Some(RowValue::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Go `tracePlace`: the `trace_id`, `span_id` and `parent_span_id` columns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TracePlace {
    pub(crate) trace: String,
    pub(crate) span: String,
    pub(crate) parent: String,
}

/// Go `placeOf(row)`.
pub(crate) fn place_of(row: &Row) -> TracePlace {
    TracePlace {
        trace: str_col(row, "trace_id"),
        span: str_col(row, "span_id"),
        parent: str_col(row, "parent_span_id"),
    }
}

/// Go `jsonColumnValue(t, row, column)`: the parsed JSON of a JSON column, `None` when absent.
pub(crate) fn json_column(row: &Row, column: &str) -> Option<Value> {
    match row.get(column)? {
        RowValue::Str(text) => Some(
            serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("{column} {text:?} is not JSON: {e}")),
        ),
        RowValue::Null => None,
        other => panic!("{column} is {other:?}, want JSON text"),
    }
}

/// Go `jsonObjectColumn(t, row, column)`: the parsed JSON object of a JSON column.
pub(crate) fn json_object_column(row: &Row, column: &str) -> Value {
    let v = json_column(row, column).unwrap_or_else(|| panic!("row has no {column}"));
    assert!(v.is_object(), "{column} = {v}, want a JSON object");
    v
}

/// Go `rowLatency(t, row)`: the `latency_ms` column as integers, `None` without one.
pub(crate) fn row_latency(row: &Row) -> Option<BTreeMap<String, i64>> {
    let v = json_column(row, "latency_ms")?;
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("latency_ms is {v}, want a JSON object"));
    Some(
        obj.iter()
            .map(|(k, n)| {
                let ms = n
                    .as_i64()
                    .unwrap_or_else(|| panic!("latency_ms.{k} = {n}, want an integer"));
                (k.clone(), ms)
            })
            .collect(),
    )
}

/// Go `mustLatency(t, row)`.
pub(crate) fn must_latency(row: &Row) -> BTreeMap<String, i64> {
    row_latency(row)
        .unwrap_or_else(|| panic!("{} row has no latency_ms", str_col(row, "event_type")))
}

/// Go `functionCallIDOf(t, row)`: `attributes.adk.function_call_id`.
pub(crate) fn function_call_id_of(row: &Row) -> String {
    let attrs = json_object_column(row, "attributes");
    lookup(&attrs, &["adk", "function_call_id"])
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Go `agentErrorCounts(rows)`: the AGENT_ERROR rows per agent.
pub(crate) fn agent_error_counts(rows: &[Row]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for row in rows_of_type(rows, crate::event_types::EVENT_AGENT_ERROR) {
        *counts.entry(str_col(&row, "agent")).or_insert(0) += 1;
    }
    counts
}

// ---------------------------------------------------------------------------------------------
// Helpers shared by the four 7b-3a port directories and the 7b-2 plugin-level ports
// ---------------------------------------------------------------------------------------------

/// Go `textContent(role, text)`.
pub(crate) fn text_content(role: &str, text: &str) -> Content {
    Content::from_text(text, role)
}

/// Go `checkJSONAt(t, obj, want, path...)`.
#[track_caller]
pub(crate) fn check_json_at(obj: &Value, want: &str, path: &[&str]) {
    let name = path.join(".");
    let got = lookup(obj, path).unwrap_or_else(|| panic!("{name} is missing; got {obj}"));
    check_json(&name, got, want).unwrap_or_else(|e| panic!("{e}"));
}

/// Go `requireInvocationCompletedOK(t, rows)`.
#[track_caller]
pub(crate) fn require_invocation_completed_ok(rows: &[Row]) {
    let errs = rows_of_type(rows, crate::event_types::EVENT_INVOCATION_ERROR);
    assert!(
        errs.is_empty(),
        "got {} INVOCATION_ERROR rows, want 0; event types: {:?}",
        errs.len(),
        event_types(rows)
    );
    let completed = rows_of_type(rows, crate::event_types::EVENT_INVOCATION_COMPLETED);
    assert_eq!(
        completed.len(),
        1,
        "got {} INVOCATION_COMPLETED rows, want 1; event types: {:?}",
        completed.len(),
        event_types(rows)
    );
    assert_eq!(
        str_col(&completed[0], "status"),
        "OK",
        "INVOCATION_COMPLETED status"
    );
    assert!(
        agent_error_counts(rows).is_empty(),
        "AGENT_ERROR rows per agent = {:?}, want none",
        agent_error_counts(rows)
    );
}

/// Go `fakeTool` / `declaredTool` (`arrow_test_helper_test.go`): a tool for callback tests.
/// `origin` is its declared kind (Go's `fakeTool` has none: `Unknown`).
#[derive(Default)]
pub(crate) struct FakeTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) long_running: bool,
    /// The description panics (a tool whose metadata is unavailable).
    pub(crate) panic_on_description: bool,
    /// Go `declaredTool.Declaration()` (a `fakeTool` has none).
    pub(crate) decl: Option<FunctionDeclaration>,
    pub(crate) origin: ToolOriginKind,
}

impl FakeTool {
    /// Go `&fakeTool{name: name}`.
    pub(crate) fn named(name: &str) -> Arc<dyn Tool> {
        fake_tool(name)
    }
}

impl Tool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        assert!(!self.panic_on_description, "tool description unavailable");
        &self.description
    }
    fn is_long_running(&self) -> bool {
        self.long_running
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        self.decl.clone()
    }
    fn origin_kind(&self) -> ToolOriginKind {
        self.origin
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `&fakeTool{name: name}`.
pub(crate) fn fake_tool(name: &str) -> Arc<dyn Tool> {
    Arc::new(FakeTool {
        name: name.into(),
        ..FakeTool::default()
    })
}

/// Go `trackedInvocations(tm)`: the sorted IDs of the tracked invocations.
pub(crate) fn tracked_invocations(tm: &TraceManager) -> Vec<String> {
    let mut ids: Vec<String> = tm.mu.lock().invocations.keys().cloned().collect();
    ids.sort();
    ids
}

/// Go's one `mCtx` handed to every callback of a test: the invocation context of a
/// [`MockCtx`] and the callback context made from it, built once, so every callback sees one
/// activation (the trace manager keys calls by activation).
pub(crate) struct Mc {
    pub(crate) inv: InvocationContext,
    pub(crate) ctx: gadk_agent::Context,
}

impl Mc {
    /// The contexts of `m` (a function call ID makes the callback context a tool context).
    pub(crate) fn of(m: &MockCtx) -> Mc {
        Mc {
            inv: m.invocation(),
            ctx: m.context(),
        }
    }

    /// Go's `mCtx` passed to an invocation callback.
    pub(crate) fn invocation(&self) -> InvocationContext {
        self.inv.clone()
    }

    /// Go's `mCtx` passed to an agent / model / tool callback.
    pub(crate) fn context(&self) -> gadk_agent::Context {
        self.ctx.clone()
    }
}

/// The plugin of Go's tests that put a span context in `mCtx.ctx`: the production callback
/// bodies over a processor that writes to a [`FakeWriteServer`], assembled as `new_plugin`
/// builds it (defaults, `validateConfig`'s denied columns, a started processor, a trace manager,
/// the custom-metadata filter), and called with a [`CallbackCtx`] that carries the span (gadk
/// reads the ambient span from the tracing subscriber, which these tests do not install).
pub(crate) struct SpanPlugin {
    pub(crate) ap: Arc<AnalyticsPlugin>,
    pub(crate) tp: TestPlugin,
}

impl SpanPlugin {
    pub(crate) async fn new(mutate: impl FnOnce(&mut Config)) -> SpanPlugin {
        let mut config = test_config();
        mutate(&mut config);
        let config = config.apply_defaults();
        let denied = validate_config(&config).expect("validateConfig");
        let server = FakeWriteServer::new();
        let stream = format!(
            "projects/{}/datasets/{}/tables/{}/streams/_default",
            config.project_id, config.dataset_id, config.table_name
        );
        let processor = BatchProcessor::new(
            &Context::background(),
            Some(server.client().expect("write client")),
            &stream,
            config.clone(),
        )
        .await
        .expect("batch processor");
        processor.start();
        let ap = Arc::new(AnalyticsPlugin {
            custom_metadata: CustomMetadataFilter::new(&config.custom_metadata_allowlist),
            config,
            denied_columns: denied,
            processor: Some(processor),
            traces: Some(Arc::new(TraceManager::new())),
            ..AnalyticsPlugin::default()
        });
        let tp = TestPlugin {
            plugin: plugin_of(PLUGIN_NAME, &ap),
            server,
            ctx: MockCtx::new(),
        };
        SpanPlugin { ap, tp }
    }

    /// Go's `mCtx` (`m`) with `trace.ContextWithSpanContext(ctx, sc)`.
    pub(crate) fn ctx(m: &MockCtx, sc: SpanIds) -> CallbackCtx {
        CallbackCtx::from_invocation(&m.invocation()).with_span(sc)
    }

    /// Go's `AfterRunCallback` (with its run-end flush).
    pub(crate) async fn after_run(&self, ctx: &CallbackCtx) {
        self.ap.after_run(ctx);
        self.ap.processor.as_ref().unwrap().flush_run_end().await;
    }

    /// Go's `OnUserMessageCallback` then `AfterRunCallback` (with its run-end flush), with the
    /// span `sc` in the test's `mCtx` when set.
    pub(crate) async fn user_message_then_after_run(&self, sc: Option<SpanIds>, msg: &Content) {
        let ctx = match sc {
            Some(sc) => SpanPlugin::ctx(&self.tp.ctx, sc),
            None => CallbackCtx::from_invocation(&self.tp.ctx.invocation()),
        };
        self.ap.on_user_message(&ctx, msg);
        self.after_run(&ctx).await;
    }
}

/// Go `trace.TraceIDFromHex` / `SpanIDFromHex` of a span context (Go `withSpan`).
pub(crate) fn span_ids(trace_hex: &str, span_hex: &str) -> SpanIds {
    fn bytes<const N: usize>(hex: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex");
        }
        out
    }
    SpanIds {
        trace_id: bytes(trace_hex),
        span_id: bytes(span_hex),
    }
}

#[test]
fn mock_ctx_is_one_activation() {
    let m = MockCtx::new();
    let id = m.context().activation_id();
    assert_eq!(m.invocation().activation_id(), id);
    assert_eq!(m.with_function_call("c1").context().activation_id(), id);
    assert_eq!(
        m.with_function_call("c1").context().function_call_id(),
        "c1"
    );
    let other = MockCtx {
        invocation_id: "inv-2".into(),
        ..m.clone()
    };
    let other_id = other.context().activation_id();
    assert_ne!(other_id, id);
    assert_eq!(m.context().activation_id(), id);
    assert_eq!(other.context().activation_id(), other_id);
    assert_ne!(MockCtx::new().context().activation_id(), id);
}
