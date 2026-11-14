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

pub(crate) mod callbacks;
pub(crate) mod filter;
pub(crate) mod rows;
pub(crate) mod version;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use gadk_agent::{AgentError, CloseFn, Plugin, PluginConfig, go_quote};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::Event;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::batch_processor::{BatchProcessor, DropCounters};
use crate::bigquery::{BigQueryClient, TableOps};
use crate::config::{Config, DropStats, GcsContentOffloader, Logger};
use crate::context::Context;
use crate::error::Error;
use crate::event_types::*;
use crate::offload::gcs::GcsOffloader;
use crate::redaction::{COLUMN_CONTENT_PARTS, CustomMetadataFilter};
use crate::schema_upgrade::ensure_table_and_schema;
use crate::trace::{LogFn, RowSpan, TraceManager};
use crate::validate::validate_config;
use crate::views::{BigQueryViewRunner, ViewDefinition, ViewRunner, sync_analytics_views};
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
/// Go `bigquery.DetectProjectID`: a project ID that asks the BigQuery client for its project.
pub(crate) const DETECT_PROJECT_ID: &str = "*detect-project-id*";

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
        PluginState {
            counters: None,
            enabled: false,
            defs: Vec::new(),
            defs_err: None,
            view_gate: Arc::new(Semaphore::new(1)),
            views: parking_lot::Mutex::new(ViewsState {
                closed: true,
                ..ViewsState::default()
            }),
            life_ctx: None,
        }
    }

    /// Go `(*pluginState).SyncViews(ctx)`.
    ///
    /// # Errors
    ///
    /// Go's texts: `agentanalytics: plugin is disabled`, [`PLUGIN_CLOSED_TEXT`],
    /// `agentanalytics: BigQuery client is not configured for view creation`, the context's
    /// error, the definitions' error, the joined view errors.
    pub(crate) async fn sync_views(&self, ctx: &Context) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::msg("agentanalytics: plugin is disabled"));
        }
        let (closed, runner) = {
            let views = self.views.lock();
            (views.closed, views.runner.clone())
        };
        if closed {
            return Err(Error::msg(PLUGIN_CLOSED_TEXT));
        }
        let Some(runner) = runner else {
            return Err(Error::msg(
                "agentanalytics: BigQuery client is not configured for view creation",
            ));
        };
        if let Some(err) = ctx.err() {
            return Err(Error::Context(err));
        }
        if let Some(err) = &self.defs_err {
            return Err(Error::msg(err.clone()));
        }

        let ctx = ctx.with_cancel();
        let stop = self.life_ctx.as_ref().map(|life| {
            let pass = ctx.clone();
            life.after_func(move || pass.cancel())
        });
        let result = self.sync_views_pass(&ctx, runner.as_ref()).await;
        if let Some(stop) = stop {
            stop.stop();
        }
        ctx.cancel();
        result
    }

    /// The part of Go's `SyncViews` after its checks, under its derived context (Go's deferred
    /// `cancel` / `stop` run by the caller).
    async fn sync_views_pass(&self, ctx: &Context, runner: &dyn ViewRunner) -> Result<(), Error> {
        if let Err(err) = self.preempt_async_view_creation(ctx).await {
            return Err(self.closed_or(err));
        }
        let permit = match self.acquire_view_gate(ctx).await {
            Ok(permit) => permit,
            Err(err) => return Err(self.closed_or(err)),
        };
        let result = if self.is_closed() {
            Err(Error::msg(PLUGIN_CLOSED_TEXT))
        } else {
            sync_analytics_views(ctx, runner, &self.defs, None)
                .await
                .map_err(|err| self.closed_or(err))
        };
        drop(permit);
        result
    }

    /// Go `if s.isClosed() { return errPluginClosed }; return err`.
    fn closed_or(&self, err: Error) -> Error {
        if self.is_closed() {
            Error::msg(PLUGIN_CLOSED_TEXT)
        } else {
            err
        }
    }

    /// Go `acquireViewGate(ctx)`: the gate's one permit (dropping it is Go's
    /// `releaseViewGate`), or the context's error once it ends first.
    async fn acquire_view_gate(
        &self,
        ctx: &Context,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, Error> {
        let gate = Arc::clone(&self.view_gate);
        tokio::select! {
            biased;
            permit = gate.acquire_owned() => {
                permit.map_err(|_| Error::msg(PLUGIN_CLOSED_TEXT))
            }
            () = ctx.done() => Err(Error::Context(
                ctx.err().unwrap_or(crate::context::ContextError::Canceled),
            )),
        }
    }

    /// Go `isClosed`.
    pub(crate) fn is_closed(&self) -> bool {
        self.views.lock().closed
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
        let views_ctx = parent_ctx.without_cancel().with_cancel();
        let done = CancellationToken::new();
        let runner = {
            let mut views = self.views.lock();
            views.views_cancel = Some(views_ctx.clone());
            views.async_views_done = Some(done.clone());
            views.runner.clone()
        };
        let state = Arc::clone(self);
        tokio::spawn(async move {
            // The pass keeps the client alive (the runner refers to it weakly).
            let _client = client;
            if let Ok(permit) = state.acquire_view_gate(&views_ctx).await {
                if !state.is_closed()
                    && views_ctx.err().is_none()
                    && let Some(runner) = &runner
                {
                    let _ =
                        sync_analytics_views(&views_ctx, runner.as_ref(), &defs, logf.as_deref())
                            .await;
                }
                drop(permit);
            }
            views_ctx.cancel();
            // The pass forgets its context once it ends, before anyone waiting on `done` goes
            // on.
            {
                let mut views = state.views.lock();
                views.views_cancel = None;
                views.async_views_done = None;
            }
            done.cancel();
        });
    }

    /// Go `preemptAsyncViewCreation(ctx)`.
    pub(crate) async fn preempt_async_view_creation(&self, ctx: &Context) -> Result<(), Error> {
        let (cancel, done) = {
            let views = self.views.lock();
            (views.views_cancel.clone(), views.async_views_done.clone())
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        if let Some(done) = done {
            tokio::select! {
                biased;
                () = done.cancelled() => {}
                () = ctx.done() => {
                    return Err(Error::Context(
                        ctx.err().unwrap_or(crate::context::ContextError::Canceled),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Go `stopAsyncViewCreation`: marks the state closed, cancels and awaits the async pass,
    /// then waits for the view gate.
    pub(crate) async fn stop_async_view_creation(&self) {
        let (cancel, done) = {
            let mut views = self.views.lock();
            views.closed = true;
            // No new view DDL job starts after close; an in-flight pass uses its own runner.
            views.runner = None;
            (views.views_cancel.clone(), views.async_views_done.clone())
        };
        if let Some(life) = &self.life_ctx {
            life.cancel();
        }
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        if let Some(done) = done {
            done.cancelled().await;
        }
        // Wait for a SyncViews pass holding the gate to return.
        drop(Arc::clone(&self.view_gate).acquire_owned().await);
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
        self.state
            .counters
            .as_ref()
            .map(|c| c.drop_stats())
            .unwrap_or_default()
    }

    /// Go `SyncViews(ctx)`: creates or updates the analytics views now (see
    /// [`PluginState::sync_views`]).
    ///
    /// # Errors
    ///
    /// See Go `SyncViews`: disabled or closed plugin, no BigQuery client, the context's error,
    /// the view errors.
    pub async fn sync_views(&self, ctx: &Context) -> Result<(), Error> {
        // `self.plugin` keeps the BigQuery client alive; the runner refers to it weakly.
        self.state.sync_views(ctx).await
    }
}

/// Go `AsBigQueryAgentAnalyticsPlugin(p)`: the helpers of a plugin this crate constructed
/// (`plugin.state::<PluginState>()`), also after it closed.
pub fn as_bigquery_agent_analytics_plugin(p: &Plugin) -> Option<BigQueryAgentAnalyticsPlugin> {
    let state = p.state::<PluginState>()?;
    Some(BigQueryAgentAnalyticsPlugin {
        plugin: p.clone(),
        state,
    })
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
    let config = Config {
        project_id: project_id.to_owned(),
        dataset_id: dataset_id.to_owned(),
        table_name: table_id.to_owned(),
        ..Config::default()
    };
    new_bigquery_agent_analytics_plugin_with_config(ctx, config).await
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
    if !config.enabled {
        return Ok(new_disabled_plugin());
    }
    let config = config.apply_defaults();
    // Reject invalid settings before any client is created.
    validate_config(&config)?;
    let bq_client = BigQueryClient::new(config.project_id.clone(), &config.client_options)
        .map_err(|e| Error::wrap("failed to create bigquery client", e))?;
    // Go closes the BigQuery client when the write client fails; the REST client holds no
    // connection to close.
    let write_client = WriteClient::new(config.client_options.clone())
        .await
        .map_err(|e| Error::wrap("failed to create BigQuery write client", e))?;
    new_plugin(
        ctx,
        config,
        Some(Arc::new(bq_client)),
        Some(write_client),
        true,
    )
    .await
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
    new_plugin(ctx, config, bq_client, write_client, false).await
}

/// Go `newBigQueryAgentAnalyticsPlugin(ctx, config, bqClient, writeClient, ownBQClient)`.
pub(crate) async fn new_plugin(
    ctx: &Context,
    config: Config,
    bq_client: Option<Arc<BigQueryClient>>,
    write_client: Option<WriteClient>,
    own_bq_client: bool,
) -> Result<Plugin, Error> {
    // Go `closeClients`: the write client; an owned BigQuery client (REST, nothing to close)
    // is dropped with the plugin.
    let _ = own_bq_client;
    if !config.enabled {
        // The disabled plugin never writes, so it does not keep the clients.
        close_write_client(write_client.as_ref(), &config).await;
        return Ok(new_disabled_plugin());
    }
    let mut config = config.apply_defaults();
    let denied = match validate_config(&config) {
        Ok(denied) => denied,
        Err(err) => {
            close_write_client(write_client.as_ref(), &config).await;
            return Err(err);
        }
    };
    let Some(write_client) = write_client else {
        return Err(Error::msg(
            "agentanalytics: writeClient must not be nil when plugin is enabled",
        ));
    };
    validate_event_filter_lists(&config);

    // The plugin owns, and closes, an offloader that it builds itself; one the caller supplies
    // stays the caller's. When content_parts is disabled or denied nothing can be offloaded,
    // so no Cloud Storage client is created.
    let parts_denied = denied.contains(COLUMN_CONTENT_PARTS);
    let mut owned_offloader: Option<Arc<GcsOffloader>> = None;
    if config.gcs_offloader.is_none() && !config.gcs_bucket_name.is_empty() {
        if parts_denied || !config.log_multi_modal_content() {
            config.logf(format_args!(
                "agentanalytics: GCSBucketName {} is ignored because content_parts logging is disabled",
                go_quote(&config.gcs_bucket_name)
            ));
        } else {
            let options = if config.gcs_client_options.is_empty() {
                &config.client_options
            } else {
                &config.gcs_client_options
            };
            match GcsOffloader::new(config.gcs_bucket_name.clone(), options) {
                Ok(o) => {
                    let o = Arc::new(o);
                    config.gcs_offloader = Some(Arc::clone(&o) as Arc<dyn GcsContentOffloader>);
                    owned_offloader = Some(o);
                }
                Err(err) => {
                    close_write_client(Some(&write_client), &config).await;
                    return Err(Error::wrap("failed to create GCS offloader", err));
                }
            }
        }
    }
    let close_owned_resources = |config: &Config| {
        let write_client = write_client.clone();
        let owned_offloader = owned_offloader.clone();
        let config = config.clone();
        async move {
            close_write_client(Some(&write_client), &config).await;
            if let Some(o) = owned_offloader
                && let Err(err) = o.close().await
            {
                config.logf(format_args!("failed to close GCS offloader: {err}"));
            }
        }
    };

    // A client made for DetectProjectID, or a project ID left empty, knows the project that
    // the table, the views and the write stream belong to.
    if let Some(bq) = &bq_client
        && (config.project_id.is_empty() || config.project_id == DETECT_PROJECT_ID)
    {
        config.project_id = bq.project_id().to_owned();
    }

    // Ensure the table exists and optionally upgrade its schema (once per instance).
    if let Some(bq) = &bq_client {
        // Go `client.Dataset(d)` (the client's project) when the project is empty.
        let project = if config.project_id.is_empty() {
            bq.project_id().to_owned()
        } else {
            config.project_id.clone()
        };
        let table = TableOps::new(
            Arc::clone(bq),
            project,
            config.dataset_id.clone(),
            config.table_name.clone(),
        );
        if let Err(err) = ensure_table_and_schema(ctx, &table, &config, &denied).await {
            close_owned_resources(&config).await;
            return Err(err);
        }
    }

    // The default stream unless exactly-once delivery is on, in which case the processor
    // creates a COMMITTED stream.
    let stream_name = if config.exactly_once_delivery {
        String::new()
    } else {
        format!(
            "projects/{}/datasets/{}/tables/{}/streams/_default",
            config.project_id, config.dataset_id, config.table_name
        )
    };
    let processor = match BatchProcessor::new(
        ctx,
        Some(write_client.clone()),
        &stream_name,
        config.clone(),
    )
    .await
    {
        Ok(p) => p,
        Err(err) => {
            close_owned_resources(&config).await;
            return Err(Error::wrap("failed to initialize batch processor", err));
        }
    };
    processor.start();

    let mut traces = TraceManager::new();
    traces.logf = config.logger.clone().map(|logger| {
        let logf: LogFn = Arc::new(move |line: &str| logger.printf(format_args!("{line}")));
        logf
    });
    let plugin = Arc::new(AnalyticsPlugin {
        config: config.clone(),
        processor: Some(Arc::clone(&processor)),
        allowlist: config.event_allowlist.iter().cloned().collect(),
        denylist: config.event_denylist.iter().cloned().collect(),
        denied_columns: denied,
        final_response_tools: config.final_response_tool_names.iter().cloned().collect(),
        custom_metadata: CustomMetadataFilter::new(&config.custom_metadata_allowlist),
        traces: Some(Arc::new(traces)),
    });

    let (defs, defs_err) = match config.view_definitions() {
        Ok(defs) => (defs, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    };
    let runner: Option<Arc<dyn ViewRunner>> = bq_client.as_ref().map(|bq| {
        let runner: Arc<dyn ViewRunner> = Arc::new(BigQueryViewRunner::new(
            Arc::downgrade(bq),
            config.project_id.clone(),
            config.dataset_id.clone(),
        ));
        runner
    });
    let state = Arc::new(PluginState {
        counters: Some(processor.counters()),
        enabled: true,
        defs: defs.clone(),
        defs_err: defs_err.clone(),
        view_gate: Arc::new(Semaphore::new(1)),
        views: parking_lot::Mutex::new(ViewsState {
            runner: runner.clone(),
            ..ViewsState::default()
        }),
        life_ctx: Some(Context::background().with_cancel()),
    });

    let close = close_fn(CloseResources {
        state: Arc::clone(&state),
        processor,
        owned_offloader,
        write_client,
        bq_client: bq_client.clone(),
    });
    let adk_plugin = Plugin::new(callbacks::plugin_config(&plugin, Arc::clone(&state), close));

    if config.create_views() && runner.is_some() {
        let logf = config.view_logf();
        match defs_err {
            Some(err) => {
                if let Some(logf) = logf {
                    logf.printf(format_args!(
                        "agentanalytics: skipping view creation: {err}"
                    ));
                }
            }
            // Only the non-default Logger goes to the pass, so it does not retain Config.
            None => state.start_async_view_creation(ctx, defs, logf, bq_client),
        }
    }
    Ok(adk_plugin)
}

/// What the plugin's close function closes, in Go's `CloseFunc` order.
struct CloseResources {
    state: Arc<PluginState>,
    processor: Arc<BatchProcessor>,
    owned_offloader: Option<Arc<GcsOffloader>>,
    write_client: WriteClient,
    /// Kept alive until the plugin closes: the view runner refers to it only weakly (Go
    /// `runtime.KeepAlive(bqClient)`).
    bq_client: Option<Arc<BigQueryClient>>,
}

impl CloseResources {
    /// Go's `CloseFunc` body.
    async fn close(self) -> Result<(), Arc<Error>> {
        self.state.stop_async_view_creation().await;
        // The processor uploads while it flushes, so it goes first.
        self.processor.close().await;
        let mut errs = Vec::new();
        if let Some(o) = &self.owned_offloader
            && let Err(err) = o.close().await
        {
            errs.push(Error::wrap("failed to close GCS offloader", err));
        }
        if let Err(err) = self.write_client.close().await {
            errs.push(Error::wrap("failed to close BigQuery write client", err));
        }
        // An owned BigQuery client (REST) has no connection to close; it is dropped here.
        drop(self.bq_client);
        match Error::join(errs) {
            Some(err) => Err(Arc::new(err)),
            None => Ok(()),
        }
    }
}

/// The result of the close sequence, shared by every `close` call.
type CloseResult = Result<(), Arc<Error>>;

/// The plugin's close function: the first call spawns Go's close sequence on a task of its
/// own and every call awaits that task's result, so a caller that stops waiting (a
/// `PluginManager` close timeout) never stops the drain, and the rows still reach BigQuery.
//
// Divergence (ledger 7b-3a): Go's `CloseFunc` has no once-guard and closes the clients again on
// a second call; gadk runs the sequence once and a second `close` returns the first call's
// result (R5).
fn close_fn(resources: CloseResources) -> CloseFn {
    let resources = parking_lot::Mutex::new(Some(resources));
    let shared: parking_lot::Mutex<Option<Shared<BoxFuture<'static, CloseResult>>>> =
        parking_lot::Mutex::new(None);
    Arc::new(move || {
        let fut = {
            let mut shared = shared.lock();
            match &*shared {
                Some(fut) => fut.clone(),
                None => {
                    let task = resources.lock().take().map(|r| tokio::spawn(r.close()));
                    let fut = async move {
                        match task {
                            Some(task) => task.await.unwrap_or_else(|_| {
                                Err(Arc::new(Error::msg(
                                    "agentanalytics: plugin close panicked",
                                )))
                            }),
                            None => Ok(()),
                        }
                    }
                    .boxed()
                    .shared();
                    *shared = Some(fut.clone());
                    fut
                }
            }
        };
        async move {
            fut.await
                .map_err(|err| AgentError::Other(Box::new(SharedError(err))))
        }
        .boxed()
    })
}

/// An `Arc<Error>` as a boxed error of [`AgentError::Other`] (text and source of the error).
#[derive(Debug)]
struct SharedError(Arc<Error>);

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.0)
    }
}

/// Go `newDisabledPlugin()`: only a no-op close, and the disabled state.
pub(crate) fn new_disabled_plugin() -> Plugin {
    let close: CloseFn = Arc::new(|| async { Ok(()) }.boxed());
    Plugin::new(PluginConfig {
        name: PLUGIN_NAME.to_owned(),
        close: Some(close),
        state: Some(Arc::new(PluginState::disabled())),
        ..PluginConfig::default()
    })
}

/// Go `closeWriteClient(writeClient, config)`: closes an unused write client, logging any
/// error (`failed to close BigQuery write client: …`).
pub(crate) async fn close_write_client(write_client: Option<&WriteClient>, config: &Config) {
    let Some(write_client) = write_client else {
        return;
    };
    if let Err(err) = write_client.close().await {
        config.logf(format_args!("failed to close BigQuery write client: {err}"));
    }
}

/// Go `validateEventFilterLists(c)`: logs each unknown filter entry (`agentanalytics: unknown
/// event_type %q in filter configuration (possible typo)`).
pub(crate) fn validate_event_filter_lists(config: &Config) {
    for event_type in config.event_allowlist.iter().chain(&config.event_denylist) {
        if !KNOWN_EVENT_TYPES.contains(&event_type.as_str()) {
            config.logf(format_args!(
                "agentanalytics: unknown event_type {} in filter configuration (possible typo)",
                go_quote(event_type)
            ));
        }
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::any::Any;
    use std::time::Duration;

    use gadk_agent::{PluginManager, Tool};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::bigquery::fake::RecordingLogger;
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
        MockCtx, event_types, new_clocked_plugin, setup_test_plugin,
    };
    use crate::storagepb::AppendRowsResponse;
    use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer};

    fn err_text<T>(r: Result<T, Error>) -> String {
        match r {
            Ok(_) => "<ok>".into(),
            Err(e) => e.to_string(),
        }
    }

    #[tokio::test]
    async fn disabled_plugin_has_only_close_and_a_disabled_state() {
        let p = new_bigquery_agent_analytics_plugin_with_config(
            &Context::background(),
            Config {
                enabled: false,
                ..Config::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(p.name(), PLUGIN_NAME);
        assert!(p.before_run_callback().is_none());
        assert!(p.on_event_callback().is_none());
        (p.close().unwrap())().await.unwrap();
        let bq = as_bigquery_agent_analytics_plugin(&p).expect("constructed by this crate");
        assert_eq!(bq.drop_stats(), DropStats::default());
        assert_eq!(
            err_text(bq.sync_views(&Context::background()).await),
            "agentanalytics: plugin is disabled"
        );
        assert!(bq.state.is_closed());
        let other = Plugin::new(PluginConfig::default());
        assert!(as_bigquery_agent_analytics_plugin(&other).is_none());
    }

    #[tokio::test]
    async fn with_clients_needs_a_write_client_and_a_valid_config() {
        let ctx = Context::background();
        let config = Config {
            project_id: "p".into(),
            ..Config::default()
        };
        assert_eq!(
            err_text(
                new_bigquery_agent_analytics_plugin_with_clients(&ctx, config.clone(), None, None)
                    .await
            ),
            "agentanalytics: writeClient must not be nil when plugin is enabled"
        );
        let server = FakeWriteServer::new();
        let bad = Config {
            max_content_len: -2,
            ..config
        };
        assert_eq!(
            err_text(
                new_bigquery_agent_analytics_plugin_with_clients(
                    &ctx,
                    bad,
                    None,
                    Some(server.client().unwrap()),
                )
                .await
            ),
            "invalid config: MaxContentLen must be -1 or >= 1"
        );
    }

    #[test]
    fn unknown_filter_entries_are_logged() {
        let logger = Arc::new(RecordingLogger::default());
        validate_event_filter_lists(&Config {
            event_allowlist: vec![EVENT_LLM_REQUEST.into(), "LLM_REQEST".into()],
            event_denylist: vec![LEGACY_EVENT_TYPE.into(), "nope".into()],
            logger: Some(logger.clone()),
            ..Config::default()
        });
        assert_eq!(
            logger.lines(),
            vec![
                r#"agentanalytics: unknown event_type "LLM_REQEST" in filter configuration (possible typo)"#,
                r#"agentanalytics: unknown event_type "nope" in filter configuration (possible typo)"#,
            ]
        );
    }

    #[tokio::test]
    async fn enabled_plugin_registers_every_callback_and_closes_once() {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        assert_eq!(p.name(), PLUGIN_NAME);
        assert!(p.on_user_message_callback().is_some());
        assert!(p.on_event_callback().is_some());
        assert!(p.before_run_callback().is_some());
        assert!(p.after_run_callback().is_some());
        assert!(p.before_agent_callback().is_some());
        assert!(p.after_agent_callback().is_some());
        assert!(p.before_model_callback().is_some());
        assert!(p.after_model_callback().is_some());
        assert!(p.on_model_error_callback().is_some());
        assert!(p.before_tool_callback().is_some());
        assert!(p.after_tool_callback().is_some());
        assert!(p.on_tool_error_callback().is_some());

        let ictx = tp.ctx.invocation();
        (p.before_run_callback().unwrap())(ictx.clone())
            .await
            .unwrap();
        (p.after_run_callback().unwrap())(ictx).await;
        let rows = tp.collect_rows(|rows| rows.len() >= 2).await;
        assert_eq!(
            event_types(&rows),
            vec![EVENT_INVOCATION_STARTING, EVENT_INVOCATION_COMPLETED]
        );

        tp.close().await.unwrap();
        // A second close returns the first result and closes nothing again.
        tp.close().await.unwrap();
        let bq = as_bigquery_agent_analytics_plugin(p).unwrap();
        assert!(bq.state.is_closed());
        assert_eq!(
            err_text(bq.sync_views(&Context::background()).await),
            PLUGIN_CLOSED_TEXT
        );
        assert_eq!(bq.drop_stats(), DropStats::default());
    }

    // Spec R5: the drain runs on the close function's own task, so a PluginManager close
    // timeout shorter than the drain stops only the waiting, never the rows.
    #[tokio::test(start_paused = true)]
    async fn close_under_a_shorter_manager_timeout_still_delivers_every_row() {
        // The first append fails with a retryable status, so the drain backs off (Go's
        // RetryConfig: 100 ms first) for far longer than the close timeout before it writes
        // again; only acknowledged requests count as delivered.
        let server = FakeWriteServer::new();
        let acked = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let streams = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_acked = Arc::clone(&acked);
        server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
            let acked = Arc::clone(&hook_acked);
            let first = streams.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            Box::pin(async move {
                while let Some(req) = stream.recv().await? {
                    if first {
                        return Err(tonic::Status::unavailable("try again"));
                    }
                    acked.lock().push(req);
                    stream.send(AppendRowsResponse::default()).await?;
                }
                Ok(())
            })
        }));
        let config = Config {
            project_id: "test-project".into(),
            dataset_id: "test_dataset".into(),
            table_name: "test-table".into(),
            // The rows wait in the queue until close drains it.
            batch_flush_intv: Duration::from_secs(3600),
            disable_flush_on_run_end: true,
            ..Config::default()
        };
        let p = new_bigquery_agent_analytics_plugin_with_clients(
            &Context::background(),
            config,
            None,
            Some(server.client().unwrap()),
        )
        .await
        .unwrap();
        let mctx = MockCtx::new();
        let before_run = p.before_run_callback().unwrap();
        for _ in 0..5 {
            before_run(mctx.invocation()).await.unwrap();
        }
        assert!(server.append_requests().is_empty(), "rows wait for close");

        let manager = PluginManager::new(vec![p.clone()], Duration::from_millis(10)).unwrap();
        let err = manager.close().await.unwrap_err();
        assert!(
            err.to_string().contains("close timed out after 10ms"),
            "{err}"
        );

        // The waiter is gone; the close task drains on, with no one awaiting it.
        tokio::time::timeout(Duration::from_secs(60), async {
            while acked.lock().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the drain stopped with its waiter");
        let rows: Vec<_> = acked
            .lock()
            .iter()
            .flat_map(|r| crate::testutil::arrow_rows::decode_arrow_rows(r).unwrap())
            .collect();
        assert_eq!(event_types(&rows), vec![EVENT_INVOCATION_STARTING; 5]);
        // A later close awaits the same task and returns its result.
        (p.close().unwrap())().await.unwrap();
        let bq = as_bigquery_agent_analytics_plugin(&p).unwrap();
        assert_eq!(bq.drop_stats(), DropStats::default());
    }

    struct PanickingTool;

    #[async_trait::async_trait]
    impl Tool for PanickingTool {
        fn name(&self) -> &str {
            "panicky"
        }
        fn description(&self) -> &str {
            panic!("Description failed")
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    // Spec R7: a panic while a callback builds its row drops the row, counted once, and never
    // reaches the agent run.
    #[tokio::test]
    async fn a_panic_in_a_callback_row_drops_the_row() {
        let c = new_clocked_plugin().await;
        let mctx = MockCtx::named("agent", "inv-panic").with_function_call("call-1");
        let cfg = super::callbacks::callbacks_config("t", &c.plugin);
        let before_tool = cfg.before_tool_callback.unwrap();
        let tool: Arc<dyn Tool> = Arc::new(PanickingTool);
        let out = before_tool(mctx.context(), tool, Map::new()).await.unwrap();
        assert!(out.is_none());
        assert!(c.take_rows().is_empty());
        let stats = c.plugin.processor.as_ref().unwrap().drop_stats();
        assert_eq!(stats.append_failure_drops, 1);
        assert_eq!(stats.total_drops, 1);
    }
}
