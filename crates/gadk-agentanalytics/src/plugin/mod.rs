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

//! The BigQuery Agent Analytics plugin, from ADK-Go
//! `plugin/agentanalytics/bigquery_agent_analytics_plugin.go` (constructors, the disabled
//! plugin, `pluginState`, `BigQueryAgentAnalyticsPlugin`, `AsBigQueryAgentAnalyticsPlugin`,
//! `DropStats`, `analyticsPlugin`, `eventData`, `closeWriteClient`, `validateEventFilterLists`,
//! the `CloseFunc`) and `views.go`'s plugin state (`SyncViews`, the view gate, async view
//! creation). The callbacks are [`callbacks`], rows [`rows`], filters and the log / write entry
//! points [`filter`], the ADK version [`version`]; the session-event signal rows are
//! [`crate::events`].
//!
//! Constructors return a [`gadk_agent::Plugin`] (Go `*baseplugin.Plugin`); its typed state slot
//! (`PluginConfig::state`, R1(e)) holds the [`PluginState`] that
//! [`as_bigquery_agent_analytics_plugin`] recovers (Go's weak-pointer registry).

#![allow(dead_code, unused_variables)] // Task 9 implements the bodies; consumers: Tasks 6, 7, 9.

pub(crate) mod callbacks;
pub(crate) mod filter;
pub(crate) mod rows;
pub(crate) mod version;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::Plugin;
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::Event;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::batch_processor::{BatchProcessor, DropCounters};
use crate::bigquery::BigQueryClient;
use crate::config::{Config, DropStats, Logger};
use crate::context::Context;
use crate::error::Error;
use crate::event_types::*;
use crate::redaction::CustomMetadataFilter;
use crate::trace::{RowSpan, TraceManager};
use crate::views::{ViewDefinition, ViewRunner};
use crate::write::gapic::WriteClient;

/// Go `pluginName`.
pub(crate) const PLUGIN_NAME: &str = "bigquery_agent_analytics";
/// Go `legacyEventType`: accepted in the event filters as an alias of every session-event row.
pub(crate) const LEGACY_EVENT_TYPE: &str = "EVENT";
/// Go `adkEnvelopeSchemaVersion`: the version of the `attributes.adk` object.
pub(crate) const ADK_ENVELOPE_SCHEMA_VERSION: &str = "1";
/// Go `userAuthor`.
pub(crate) const USER_AUTHOR: &str = "user";
/// Go `scopeKindNodeRun`.
pub(crate) const SCOPE_KIND_NODE_RUN: &str = "node_run";
/// Go `scopeKindFunctionCall`.
pub(crate) const SCOPE_KIND_FUNCTION_CALL: &str = "function_call";
/// Go `errPluginClosed`'s text (`views.go`).
pub(crate) const PLUGIN_CLOSED_TEXT: &str = "agentanalytics: plugin is closed";

/// Go `knownEventTypes`: the event types the filters accept without a warning (the `Event*`
/// constants and the legacy `EVENT`).
pub(crate) const KNOWN_EVENT_TYPES: [&str; 29] = [
    EVENT_USER_MESSAGE_RECEIVED,
    EVENT_INVOCATION_STARTING,
    EVENT_INVOCATION_COMPLETED,
    EVENT_INVOCATION_ERROR,
    EVENT_AGENT_STARTING,
    EVENT_AGENT_COMPLETED,
    EVENT_AGENT_ERROR,
    EVENT_LLM_REQUEST,
    EVENT_LLM_RESPONSE,
    EVENT_LLM_ERROR,
    EVENT_TOOL_STARTING,
    EVENT_TOOL_COMPLETED,
    EVENT_TOOL_ERROR,
    EVENT_STATE_DELTA,
    EVENT_AGENT_TRANSFER,
    EVENT_EVENT_COMPACTION,
    EVENT_AGENT_STATE_CHECKPOINT,
    EVENT_TOOL_PAUSED,
    EVENT_HITL_CREDENTIAL_REQUEST,
    EVENT_HITL_CONFIRMATION_REQUEST,
    EVENT_HITL_INPUT_REQUEST,
    EVENT_HITL_CREDENTIAL_COMPLETED,
    EVENT_HITL_CONFIRMATION_COMPLETED,
    EVENT_HITL_INPUT_COMPLETED,
    EVENT_A2A_INTERACTION,
    EVENT_AGENT_RESPONSE,
    EVENT_NODE_OUTPUT,
    EVENT_NODE_ERROR,
    LEGACY_EVENT_TYPE,
];

/// Go `eventData.content any`: the raw payload of the `content` / `content_parts` columns.
/// `add_content` formats the typed payloads as Go's type switch does; everything else is JSON.
pub(crate) enum Payload {
    /// Go `*genai.Content`.
    Content(Content),
    /// Go `*model.LLMRequest`.
    LlmRequest(Box<LlmRequest>),
    /// Go `*model.LLMResponse`.
    LlmResponse(Box<LlmResponse>),
    /// Any other Go value (maps, strings), as JSON.
    Json(Value),
}

/// Go `eventData`: the event-specific parts of a row.
#[derive(Default)]
pub(crate) struct EventData {
    /// Go `span`: the place of the row.
    pub(crate) span: RowSpan,
    /// Go `content` (nil = `None`).
    pub(crate) content: Option<Payload>,
    pub(crate) content_truncated: bool,
    /// Go `parts []map[string]any`: pre-formatted `content_parts` entries.
    pub(crate) parts: Vec<Map<String, Value>>,
    pub(crate) parts_truncated: bool,
    /// Go `attributes map[string]any` (nil = empty).
    pub(crate) attributes: Map<String, Value>,
    /// Go `adkExtras map[string]any` (nil = empty).
    pub(crate) adk_extras: Map<String, Value>,
    pub(crate) err_msg: String,
    pub(crate) failed: bool,
    pub(crate) function_call_id: String,
    /// Go `sourceEvent *session.Event`.
    pub(crate) source_event: Option<Arc<Event>>,
    pub(crate) author: String,
    /// Go `customMetadata map[string]any` (nil = `None`).
    pub(crate) custom_metadata: Option<Map<String, Value>>,
    pub(crate) custom_metadata_scrubbed: bool,
    pub(crate) custom_metadata_truncated: bool,
    /// Go `timestamp` (zero = `None`): overrides `span.timestamp`.
    pub(crate) timestamp: Option<DateTime<Utc>>,
}

/// Go `analyticsPlugin`: the state shared by the plugin callbacks. Go's nil `processor` /
/// `traces` (test literals) are `None`.
pub(crate) struct AnalyticsPlugin {
    pub(crate) config: Config,
    pub(crate) processor: Option<Arc<BatchProcessor>>,
    pub(crate) allowlist: BTreeSet<String>,
    pub(crate) denylist: BTreeSet<String>,
    pub(crate) denied_columns: BTreeSet<String>,
    pub(crate) final_response_tools: BTreeSet<String>,
    pub(crate) custom_metadata: CustomMetadataFilter,
    pub(crate) traces: Option<Arc<TraceManager>>,
}

impl Default for AnalyticsPlugin {
    /// Go's zero `analyticsPlugin{}` (zero `Config`, as Go's test literals).
    fn default() -> AnalyticsPlugin {
        AnalyticsPlugin {
            config: Config::zero(),
            processor: None,
            allowlist: BTreeSet::new(),
            denylist: BTreeSet::new(),
            denied_columns: BTreeSet::new(),
            final_response_tools: BTreeSet::new(),
            custom_metadata: CustomMetadataFilter::default(),
            traces: None,
        }
    }
}

/// What Go's `pluginState.viewsMu` guards.
#[derive(Default)]
pub(crate) struct ViewsState {
    pub(crate) closed: bool,
    pub(crate) runner: Option<Arc<dyn ViewRunner>>,
    /// Go `viewsCancel`: the running async pass's context (cancel with `cancel()`).
    pub(crate) views_cancel: Option<Context>,
    /// Go `asyncViewsDone`: cancelled when the async pass ends (Go closes the channel).
    pub(crate) async_views_done: Option<CancellationToken>,
}

/// Go `pluginState`: the operational state of a constructed plugin, kept in the plugin's state
/// slot (Go's registry value).
pub(crate) struct PluginState {
    /// Go `counters` (nil for the disabled plugin).
    pub(crate) counters: Option<Arc<DropCounters>>,
    pub(crate) enabled: bool,
    /// Go `defs`, `defsErr` (the error's text; `SyncViews` returns it as `Error::msg`).
    pub(crate) defs: Vec<ViewDefinition>,
    pub(crate) defs_err: Option<String>,
    /// Go `viewGate chan struct{}` (capacity 1, one token): one permit.
    pub(crate) view_gate: Arc<Semaphore>,
    /// Go `viewsMu` and the fields it guards.
    pub(crate) views: parking_lot::Mutex<ViewsState>,
    /// Go `lifeCtx` / `lifeCancel` (`cancel()` on it); `None` for the disabled plugin.
    pub(crate) life_ctx: Option<Context>,
}

impl PluginState {
    /// Go `&pluginState{enabled: false, closed: true}` (the disabled plugin).
    pub(crate) fn disabled() -> PluginState {
        todo!("Task 9")
    }

    /// Go `(*pluginState).SyncViews(ctx)`.
    ///
    /// # Errors
    ///
    /// Go's texts: `agentanalytics: plugin is disabled`, [`PLUGIN_CLOSED_TEXT`],
    /// `agentanalytics: BigQuery client is not configured for view creation`, the context's
    /// error, the definitions' error, the joined view errors.
    pub(crate) async fn sync_views(&self, ctx: &Context) -> Result<(), Error> {
        todo!("Task 9")
    }

    /// Go `isClosed`.
    pub(crate) fn is_closed(&self) -> bool {
        todo!("Task 9")
    }

    /// Go `startAsyncViewCreation(parentCtx, defs, logf, client)`: a background view pass on
    /// a spawned task.
    pub(crate) fn start_async_view_creation(
        self: &Arc<Self>,
        parent_ctx: &Context,
        defs: Vec<ViewDefinition>,
        logf: Option<Arc<dyn Logger>>,
        client: Option<Arc<BigQueryClient>>,
    ) {
        todo!("Task 9")
    }

    /// Go `preemptAsyncViewCreation(ctx)`.
    pub(crate) async fn preempt_async_view_creation(&self, ctx: &Context) -> Result<(), Error> {
        todo!("Task 9")
    }

    /// Go `stopAsyncViewCreation`: marks the state closed, cancels and awaits the async pass,
    /// then waits for the view gate.
    pub(crate) async fn stop_async_view_creation(&self) {
        todo!("Task 9")
    }
}

/// Go `BigQueryAgentAnalyticsPlugin`: a constructed plugin with its operational helpers. Get
/// one with [`as_bigquery_agent_analytics_plugin`].
#[derive(Clone)]
pub struct BigQueryAgentAnalyticsPlugin {
    pub(crate) plugin: Plugin,
    pub(crate) state: Arc<PluginState>,
}

impl BigQueryAgentAnalyticsPlugin {
    /// The plugin (Go's embedded `*baseplugin.Plugin`).
    pub fn plugin(&self) -> &Plugin {
        &self.plugin
    }

    /// Go `DropStats()`: cumulative event-drop and formatter-failure counts (zero for the
    /// disabled plugin). Available after the plugin closed.
    pub fn drop_stats(&self) -> DropStats {
        todo!("Task 9")
    }

    /// Go `SyncViews(ctx)`: creates or updates the analytics views now (see
    /// [`PluginState::sync_views`]).
    ///
    /// # Errors
    ///
    /// See Go `SyncViews`: disabled or closed plugin, no BigQuery client, the context's error,
    /// the view errors.
    pub async fn sync_views(&self, ctx: &Context) -> Result<(), Error> {
        todo!("Task 9")
    }
}

/// Go `AsBigQueryAgentAnalyticsPlugin(p)`: the helpers of a plugin this crate constructed
/// (`plugin.state::<PluginState>()`), also after it closed.
pub fn as_bigquery_agent_analytics_plugin(p: &Plugin) -> Option<BigQueryAgentAnalyticsPlugin> {
    todo!("Task 9")
}

/// Go `NewBigQueryAgentAnalyticsPlugin(ctx, projectID, datasetID, tableID)`: the default config
/// for the table.
///
/// Register the plugin before any plugin whose callbacks return replacement events, model
/// responses, tool results or agent content (see Go's doc comment).
///
/// # Errors
///
/// As [`new_bigquery_agent_analytics_plugin_with_config`].
pub async fn new_bigquery_agent_analytics_plugin(
    ctx: &Context,
    project_id: &str,
    dataset_id: &str,
    table_id: &str,
) -> Result<Plugin, Error> {
    todo!("Task 9")
}

/// Go `NewBigQueryAgentAnalyticsPluginWithConfig(ctx, config)`: a disabled config gives the
/// disabled plugin; otherwise the config is defaulted and validated, then the BigQuery and
/// write clients are created and owned by the plugin.
///
/// # Errors
///
/// The config's validation error (Go's texts), `failed to create bigquery client: …`,
/// `failed to create BigQuery write client: …`, and the errors of
/// [`new_bigquery_agent_analytics_plugin_with_clients`].
pub async fn new_bigquery_agent_analytics_plugin_with_config(
    ctx: &Context,
    config: Config,
) -> Result<Plugin, Error> {
    todo!("Task 9")
}

/// Go `NewBigQueryAgentAnalyticsPluginWithClients(ctx, config, bqClient, writeClient)`: rows go
/// through `write_client` (owned and closed by the plugin; closed right away when the config is
/// disabled or construction fails); `bq_client` (caller-owned, `None` skips table setup and
/// views) creates or upgrades the table and runs the views.
///
/// # Errors
///
/// The config's validation error; `agentanalytics: writeClient must not be nil when plugin is
/// enabled`; `failed to create GCS offloader: …`; the table setup error; `failed to initialize
/// batch processor: …`.
pub async fn new_bigquery_agent_analytics_plugin_with_clients(
    ctx: &Context,
    config: Config,
    bq_client: Option<Arc<BigQueryClient>>,
    write_client: Option<WriteClient>,
) -> Result<Plugin, Error> {
    todo!("Task 9")
}

/// Go `newBigQueryAgentAnalyticsPlugin(ctx, config, bqClient, writeClient, ownBQClient)`.
pub(crate) async fn new_plugin(
    ctx: &Context,
    config: Config,
    bq_client: Option<Arc<BigQueryClient>>,
    write_client: Option<WriteClient>,
    own_bq_client: bool,
) -> Result<Plugin, Error> {
    todo!("Task 9")
}

/// Go `newDisabledPlugin()`: only a no-op close, and the disabled state.
pub(crate) fn new_disabled_plugin() -> Plugin {
    todo!("Task 9")
}

/// Go `closeWriteClient(writeClient, config)`: closes an unused write client, logging any
/// error (`failed to close BigQuery write client: …`).
pub(crate) async fn close_write_client(write_client: Option<&WriteClient>, config: &Config) {
    todo!("Task 9")
}

/// Go `validateEventFilterLists(c)`: logs each unknown filter entry (`agentanalytics: unknown
/// event_type %q in filter configuration (possible typo)`).
pub(crate) fn validate_event_filter_lists(config: &Config) {
    todo!("Task 9")
}

// The callbacks share the plugin across tasks, the state slot holds `PluginState` as
// `Arc<dyn Any + Send + Sync>`, and span records keep ADK contexts: all must be `Send + Sync`.
const _: fn() = || {
    fn send_sync<T: Send + Sync + 'static>() {}
    send_sync::<AnalyticsPlugin>();
    send_sync::<PluginState>();
    send_sync::<BigQueryAgentAnalyticsPlugin>();
    send_sync::<TraceManager>();
    send_sync::<crate::trace::CallbackCtx>();
    send_sync::<crate::trace::SpanRef>();
};
