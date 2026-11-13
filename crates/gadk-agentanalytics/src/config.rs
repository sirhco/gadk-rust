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

//! Port of ADK-Go `plugin/agentanalytics/config.go`: [`Config`] (Go `DefaultConfig()` is
//! [`Config::default`], Go's zero value `Config{}` is [`Config::zero`]), `applyDefaults`,
//! [`Logger`], [`RetryConfig`], [`ContentFormatter`], [`DropStats`], [`GcsUploadRequest`] and the
//! [`GcsContentOffloader`] trait, plus `gcsUploadTimeout` and `defaultMaxPendingUploadBytes` from
//! `gcs_offloader.go`. Go's `[]option.ClientOption` is [`ClientOptions`].

use std::fmt;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gadk_model::HttpClient;
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::context::Context;
use crate::error::{BoxError, Error};

/// The version of the events table schema written by this plugin and stored under the
/// [`SCHEMA_VERSION_KEY`] table label (Go `SchemaVersion`). It matches adk-python's.
pub const SCHEMA_VERSION: &str = "2";
/// The BigQuery table label key that records the schema version (Go `schemaVersionKey`).
pub(crate) const SCHEMA_VERSION_KEY: &str = "adk_schema_version";

/// Go `defaultTableName`.
pub(crate) const DEFAULT_TABLE_NAME: &str = "agent_events";
/// Go `defaultMaxInlineTextSize` (32 KiB).
pub(crate) const DEFAULT_MAX_INLINE_TEXT_SIZE: i64 = 32 * 1024;
/// Go `defaultGCSMaxConcurrentUploads`.
pub(crate) const DEFAULT_GCS_MAX_CONCURRENT_UPLOADS: i64 = 8;
/// Go `defaultViewPrefix`.
pub(crate) const DEFAULT_VIEW_PREFIX: &str = "v";
/// Go `gcsUploadTimeout` (`gcs_offloader.go`): bounds one batch's uploads to Cloud Storage when
/// [`Config::gcs_upload_timeout`] is not set.
pub(crate) const GCS_UPLOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Go `defaultMaxPendingUploadBytes` (`gcs_offloader.go`): caps the bytes held in memory by
/// pending uploads across queued rows (256 MiB).
pub(crate) const DEFAULT_MAX_PENDING_UPLOAD_BYTES: i64 = 256 * 1024 * 1024;

/// Go's `Logger` interface (`Printf`, `Println`). The caller formats the arguments
/// (`format_args!`), so `printf(format_args!("x %d", n))` is Go's `Printf("x %d", n)`.
pub trait Logger: Send + Sync {
    /// Go `Printf(format, v...)`.
    fn printf(&self, args: fmt::Arguments<'_>);
    /// Go `Println(v...)`.
    fn println(&self, args: fmt::Arguments<'_>);
    /// Whether this is Go's `log.Default()` ([`StdLogger`]): background view creation never
    /// logs through it (Go `viewLogf`). Other loggers keep the default `false`.
    fn is_std_default(&self) -> bool {
        false
    }
}

/// Go `log.Default()`: writes each message to stderr after a local `2006/01/02 15:04:05 `
/// timestamp, adding a final newline when the message has none.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdLogger;

impl StdLogger {
    fn output(line: &str) {
        let mut out = format!(
            "{} {line}",
            chrono::Local::now().format("%Y/%m/%d %H:%M:%S")
        );
        if !out.ends_with('\n') {
            out.push('\n');
        }
        // Go's log package ignores write errors too.
        let _ = std::io::stderr().write_all(out.as_bytes());
    }
}

impl Logger for StdLogger {
    fn printf(&self, args: fmt::Arguments<'_>) {
        StdLogger::output(&args.to_string());
    }

    fn println(&self, args: fmt::Arguments<'_>) {
        StdLogger::output(&format!("{args}\n"));
    }

    fn is_std_default(&self) -> bool {
        true
    }
}

/// Retry parameters for BigQuery appends (Go `RetryConfig`). An all-zero `RetryConfig` is
/// filled from [`Config::default`] (3 retries, 100ms initial delay, 10s max delay, multiplier 2);
/// to disable retries set `max_retries: 0` with a non-zero delay or multiplier.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RetryConfig {
    /// Go `MaxRetries`.
    pub max_retries: i64,
    /// Go `InitialDelay`.
    pub initial_delay: Duration,
    /// Go `MaxDelay`.
    pub max_delay: Duration,
    /// Go `Multiplier`.
    pub multiplier: f64,
}

/// Transforms or redacts an event payload before it is truncated and written to the `content`
/// column (Go `ContentFormatter`). It must return a new value; an error (or a panic) replaces the
/// content with `"[FORMATTER_FAILED]"`. The second argument is the event type.
pub type ContentFormatter = Arc<dyn Fn(&Value, &str) -> Result<Value, BoxError> + Send + Sync>;

/// Cumulative event-drop and formatter-failure counts (Go `DropStats`; JSON field names as Go's
/// tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct DropStats {
    /// Events dropped because `queue_max_size` was reached.
    pub queue_full_drops: i64,
    /// Events dropped by failed or skipped appends (an upper bound).
    pub append_failure_drops: i64,
    /// Events whose payload was replaced with `"[FORMATTER_FAILED]"` (no row is dropped).
    pub formatter_failure_drops: i64,
    /// Events rejected after close or dropped when the shutdown timeout expired.
    pub shutdown_drops: i64,
    /// `queue_full_drops + append_failure_drops + shutdown_drops`.
    pub total_drops: i64,
}

/// One payload to upload to Cloud Storage (Go `GCSUploadRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcsUploadRequest {
    /// The object name within the bucket; it already includes [`Config::gcs_path_prefix`].
    pub path: String,
    /// The raw payload.
    pub data: Bytes,
    /// The MIME type to set on the stored object.
    pub content_type: String,
}

/// Uploads multimodal or oversized text payloads to Cloud Storage and returns the `gs://` URI of
/// the stored object (Go `GCSContentOffloader`). Called from several tasks at once; must return
/// soon after `ctx` ends.
#[async_trait::async_trait]
pub trait GcsContentOffloader: Send + Sync {
    /// Go `UploadContent(ctx, req)`.
    async fn upload_content(&self, ctx: &Context, req: GcsUploadRequest) -> Result<String, Error>;

    /// Releases the offloader's resources (the plugin closes only an offloader it built
    /// itself). The default does nothing.
    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// How a Google client reaches its service (Go `[]option.ClientOption`; the options the plugin
/// uses). Empty ([`ClientOptions::is_empty`]) means Go's empty option list.
#[derive(Clone, Default)]
pub struct ClientOptions {
    /// Go `option.WithCredentials` / `WithAuthCredentials`; `None` resolves Application Default
    /// Credentials.
    pub credentials: Option<Credentials>,
    /// Go `option.WithoutAuthentication`.
    pub without_authentication: bool,
    /// Go `option.WithHTTPClient`; `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Go `option.WithEndpoint`.
    pub endpoint: Option<String>,
    /// Extra headers on every request (Go `option.WithRequestReason`-style header options).
    pub headers: HeaderMap,
}

impl ClientOptions {
    /// Whether no option is set (Go `len(opts) == 0`).
    pub fn is_empty(&self) -> bool {
        self.credentials.is_none()
            && !self.without_authentication
            && self.http_client.is_none()
            && self.endpoint.is_none()
            && self.headers.is_empty()
    }
}

impl fmt::Debug for ClientOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientOptions")
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .field("endpoint", &self.endpoint)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The plugin's settings (Go `Config`, mapping to adk-python's `BigQueryLoggerConfig`). See the
/// Go documentation of each field; Go `int` fields are `i64`, `time.Duration` fields
/// [`Duration`], `map[string]any` a JSON object, and func / interface fields `Option`s.
///
/// [`Config::default`] is Go's `DefaultConfig()`; Go's zero value `Config{}` is
/// [`Config::zero`]. Zero numeric and duration fields are filled from the defaults when a plugin
/// or batch processor is built (Go `applyDefaults`).
#[derive(Clone)]
pub struct Config {
    /// Go `Enabled`.
    pub enabled: bool,
    /// Go `ProjectID`.
    pub project_id: String,
    /// Go `DatasetID`.
    pub dataset_id: String,
    /// Go `TableName`.
    pub table_name: String,
    /// Go `MaxContentLen`.
    pub max_content_len: i64,
    /// Go `BatchSize`.
    pub batch_size: i64,
    /// Go `BatchFlushIntv`.
    pub batch_flush_intv: Duration,
    /// Go `QueueMaxSize`.
    pub queue_max_size: i64,
    /// Go `ClientOptions`.
    pub client_options: ClientOptions,
    /// Fields to cluster the table by (Go `ClusteringFields`).
    pub clustering_fields: Vec<String>,
    /// Written to `attributes.custom_tags` on every event (Go `CustomTags`).
    pub custom_tags: Map<String, Value>,
    /// Whether to log multi-modal content in `content_parts` (Go `LogMultiModalContent`).
    pub log_multi_modal_content: bool,
    /// Go `ShutdownTimeout`.
    pub shutdown_timeout: Duration,
    /// Go `Logger`; `None` is Go's nil logger.
    pub logger: Option<Arc<dyn Logger>>,
    /// Go `RetryConfig`.
    pub retry_config: RetryConfig,
    /// Go `DisableMultiModalContent`.
    pub disable_multi_modal_content: bool,
    /// Go `DisableSessionMetadata`.
    pub disable_session_metadata: bool,
    /// Go `CaptureFullMessageContent`.
    pub capture_full_message_content: bool,
    /// Go `DisableSensitiveKeyRedaction`.
    pub disable_sensitive_key_redaction: bool,
    /// Go `ContentFormatter`.
    pub content_formatter: Option<ContentFormatter>,
    /// Go `DebugContentFormatterErrors`.
    pub debug_content_formatter_errors: bool,
    /// Go `EventAllowlist`.
    pub event_allowlist: Vec<String>,
    /// Go `EventDenylist`.
    pub event_denylist: Vec<String>,
    /// Go `PayloadColumnDenylist`: `content`, `content_parts`, `attributes`, `latency_ms`.
    pub payload_column_denylist: Vec<String>,
    /// Go `CustomMetadataAllowlist`.
    pub custom_metadata_allowlist: Vec<String>,
    /// Go `FinalResponseToolNames`.
    pub final_response_tool_names: Vec<String>,
    /// Go `DisableFlushOnRunEnd`.
    pub disable_flush_on_run_end: bool,
    /// Go `DisableSchemaUpgrade`.
    pub disable_schema_upgrade: bool,
    /// Go `EnableOTelCorrelation`.
    pub enable_otel_correlation: bool,
    /// Go `GCSBucketName`.
    pub gcs_bucket_name: String,
    /// Go `GCSClientOptions`; empty means [`client_options`](Self::client_options).
    pub gcs_client_options: ClientOptions,
    /// Go `GCSPathPrefix`.
    pub gcs_path_prefix: String,
    /// Go `ConnectionID` (`location.connection_id`).
    pub connection_id: String,
    /// Go `MaxInlineTextSize`; values ≤ 0 mean 32 KiB.
    pub max_inline_text_size: i64,
    /// Go `GCSMaxConcurrentUploads`; values ≤ 0 mean 8.
    pub gcs_max_concurrent_uploads: i64,
    /// Go `GCSUploadTimeout`; zero means 10s.
    pub gcs_upload_timeout: Duration,
    /// Go `GCSMaxPendingUploadBytes`; values ≤ 0 mean 256 MiB.
    pub gcs_max_pending_upload_bytes: i64,
    /// Go `GCSOffloader`.
    pub gcs_offloader: Option<Arc<dyn GcsContentOffloader>>,
    /// Go `DisableCreateViews`.
    pub disable_create_views: bool,
    /// Go `ViewPrefix`; empty means `"v"`.
    pub view_prefix: String,
    /// Go `ExactlyOnceDelivery`.
    pub exactly_once_delivery: bool,
}

impl Default for Config {
    /// Go `DefaultConfig()`.
    fn default() -> Self {
        Config {
            enabled: true,
            max_content_len: 500 * 1024,
            dataset_id: "agent_analytics".to_owned(),
            table_name: DEFAULT_TABLE_NAME.to_owned(),
            clustering_fields: vec![
                "event_type".to_owned(),
                "agent".to_owned(),
                "user_id".to_owned(),
            ],
            log_multi_modal_content: true,
            batch_size: 100,
            batch_flush_intv: Duration::from_secs(1),
            shutdown_timeout: Duration::from_secs(10),
            queue_max_size: 10000,
            custom_tags: Map::new(),
            logger: Some(Arc::new(StdLogger)),
            max_inline_text_size: DEFAULT_MAX_INLINE_TEXT_SIZE,
            gcs_max_concurrent_uploads: DEFAULT_GCS_MAX_CONCURRENT_UPLOADS,
            gcs_upload_timeout: GCS_UPLOAD_TIMEOUT,
            gcs_max_pending_upload_bytes: DEFAULT_MAX_PENDING_UPLOAD_BYTES,
            view_prefix: DEFAULT_VIEW_PREFIX.to_owned(),
            retry_config: RetryConfig {
                max_retries: 3,
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(10),
                multiplier: 2.0,
            },
            ..Config::zero()
        }
    }
}

impl Config {
    /// Go's zero value `Config{}`: every field empty, zero, `false` or `None`.
    pub fn zero() -> Config {
        Config {
            enabled: false,
            project_id: String::new(),
            dataset_id: String::new(),
            table_name: String::new(),
            max_content_len: 0,
            batch_size: 0,
            batch_flush_intv: Duration::ZERO,
            queue_max_size: 0,
            client_options: ClientOptions::default(),
            clustering_fields: Vec::new(),
            custom_tags: Map::new(),
            log_multi_modal_content: false,
            shutdown_timeout: Duration::ZERO,
            logger: None,
            retry_config: RetryConfig::default(),
            disable_multi_modal_content: false,
            disable_session_metadata: false,
            capture_full_message_content: false,
            disable_sensitive_key_redaction: false,
            content_formatter: None,
            debug_content_formatter_errors: false,
            event_allowlist: Vec::new(),
            event_denylist: Vec::new(),
            payload_column_denylist: Vec::new(),
            custom_metadata_allowlist: Vec::new(),
            final_response_tool_names: Vec::new(),
            disable_flush_on_run_end: false,
            disable_schema_upgrade: false,
            enable_otel_correlation: false,
            gcs_bucket_name: String::new(),
            gcs_client_options: ClientOptions::default(),
            gcs_path_prefix: String::new(),
            connection_id: String::new(),
            max_inline_text_size: 0,
            gcs_max_concurrent_uploads: 0,
            gcs_upload_timeout: Duration::ZERO,
            gcs_max_pending_upload_bytes: 0,
            gcs_offloader: None,
            disable_create_views: false,
            view_prefix: String::new(),
            exactly_once_delivery: false,
        }
    }

    /// Go `applyDefaults(c)`: fills the zero values that have a non-zero default.
    pub(crate) fn apply_defaults(mut self) -> Config {
        let defaults = Config::default();
        if self.dataset_id.is_empty() {
            self.dataset_id = defaults.dataset_id;
        }
        if self.table_name.is_empty() {
            self.table_name = defaults.table_name;
        }
        if self.max_content_len == 0 {
            self.max_content_len = defaults.max_content_len;
        }
        if self.batch_size == 0 {
            self.batch_size = defaults.batch_size;
        }
        if self.batch_flush_intv.is_zero() {
            self.batch_flush_intv = defaults.batch_flush_intv;
        }
        if self.shutdown_timeout.is_zero() {
            self.shutdown_timeout = defaults.shutdown_timeout;
        }
        if self.queue_max_size == 0 {
            self.queue_max_size = defaults.queue_max_size;
        }
        if self.retry_config == RetryConfig::default() {
            self.retry_config = defaults.retry_config;
        } else {
            if self.retry_config.initial_delay.is_zero() {
                self.retry_config.initial_delay = defaults.retry_config.initial_delay;
            }
            if self.retry_config.max_delay.is_zero() {
                self.retry_config.max_delay = defaults.retry_config.max_delay;
            }
            if self.retry_config.multiplier == 0.0 {
                self.retry_config.multiplier = defaults.retry_config.multiplier;
            }
        }
        if self.max_inline_text_size <= 0 {
            self.max_inline_text_size = DEFAULT_MAX_INLINE_TEXT_SIZE;
        }
        if self.gcs_max_concurrent_uploads <= 0 {
            self.gcs_max_concurrent_uploads = DEFAULT_GCS_MAX_CONCURRENT_UPLOADS;
        }
        // Divergence (ledger 7b-1): Go also replaces a negative GCSUploadTimeout; a Rust
        // `Duration` cannot be negative, so only zero is replaced.
        if self.gcs_upload_timeout.is_zero() {
            self.gcs_upload_timeout = GCS_UPLOAD_TIMEOUT;
        }
        if self.gcs_max_pending_upload_bytes <= 0 {
            self.gcs_max_pending_upload_bytes = DEFAULT_MAX_PENDING_UPLOAD_BYTES;
        }
        if self.view_prefix.is_empty() {
            self.view_prefix = DEFAULT_VIEW_PREFIX.to_owned();
        }
        self
    }

    /// Go `logMultiModalContent()`.
    pub(crate) fn log_multi_modal_content(&self) -> bool {
        self.log_multi_modal_content && !self.disable_multi_modal_content
    }

    /// Go `logSessionMetadata()`.
    pub(crate) fn log_session_metadata(&self) -> bool {
        !self.disable_session_metadata
    }

    /// Go `redactSensitiveKeys()`.
    pub(crate) fn redact_sensitive_keys(&self) -> bool {
        !self.disable_sensitive_key_redaction
    }

    /// Go `flushOnRunEnd()`.
    pub(crate) fn flush_on_run_end(&self) -> bool {
        !self.disable_flush_on_run_end
    }

    /// Go `createViews()`.
    pub(crate) fn create_views(&self) -> bool {
        !self.disable_create_views
    }

    /// Go `maxContentLength()`: the text truncation limit; -1 means no limit.
    pub(crate) fn max_content_length(&self) -> i64 {
        if self.capture_full_message_content {
            -1
        } else {
            self.max_content_len
        }
    }

    /// Go `logf(format, args...)`: logs through [`logger`](Self::logger) when one is set.
    pub(crate) fn logf(&self, args: fmt::Arguments<'_>) {
        if let Some(logger) = &self.logger {
            logger.printf(args);
        }
    }

    /// Go `viewLogf()`: the logger when the caller set a non-default one, else `None` (no logger,
    /// or Go's `log.Default()`), so background view creation never writes to the process-wide
    /// standard logger.
    pub(crate) fn view_logf(&self) -> Option<Arc<dyn Logger>> {
        self.logger
            .as_ref()
            .filter(|logger| !logger.is_std_default())
            .cloned()
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("enabled", &self.enabled)
            .field("project_id", &self.project_id)
            .field("dataset_id", &self.dataset_id)
            .field("table_name", &self.table_name)
            .field("max_content_len", &self.max_content_len)
            .field("batch_size", &self.batch_size)
            .field("batch_flush_intv", &self.batch_flush_intv)
            .field("queue_max_size", &self.queue_max_size)
            .field("client_options", &self.client_options)
            .field("clustering_fields", &self.clustering_fields)
            .field("custom_tags", &self.custom_tags)
            .field("log_multi_modal_content", &self.log_multi_modal_content)
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field("logger", &self.logger.is_some())
            .field("retry_config", &self.retry_config)
            .field(
                "disable_multi_modal_content",
                &self.disable_multi_modal_content,
            )
            .field("disable_session_metadata", &self.disable_session_metadata)
            .field(
                "capture_full_message_content",
                &self.capture_full_message_content,
            )
            .field(
                "disable_sensitive_key_redaction",
                &self.disable_sensitive_key_redaction,
            )
            .field("content_formatter", &self.content_formatter.is_some())
            .field(
                "debug_content_formatter_errors",
                &self.debug_content_formatter_errors,
            )
            .field("event_allowlist", &self.event_allowlist)
            .field("event_denylist", &self.event_denylist)
            .field("payload_column_denylist", &self.payload_column_denylist)
            .field("custom_metadata_allowlist", &self.custom_metadata_allowlist)
            .field("final_response_tool_names", &self.final_response_tool_names)
            .field("disable_flush_on_run_end", &self.disable_flush_on_run_end)
            .field("disable_schema_upgrade", &self.disable_schema_upgrade)
            .field("enable_otel_correlation", &self.enable_otel_correlation)
            .field("gcs_bucket_name", &self.gcs_bucket_name)
            .field("gcs_client_options", &self.gcs_client_options)
            .field("gcs_path_prefix", &self.gcs_path_prefix)
            .field("connection_id", &self.connection_id)
            .field("max_inline_text_size", &self.max_inline_text_size)
            .field(
                "gcs_max_concurrent_uploads",
                &self.gcs_max_concurrent_uploads,
            )
            .field("gcs_upload_timeout", &self.gcs_upload_timeout)
            .field(
                "gcs_max_pending_upload_bytes",
                &self.gcs_max_pending_upload_bytes,
            )
            .field("gcs_offloader", &self.gcs_offloader.is_some())
            .field("disable_create_views", &self.disable_create_views)
            .field("view_prefix", &self.view_prefix)
            .field("exactly_once_delivery", &self.exactly_once_delivery)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Mutex;

    use pretty_assertions::assert_eq;

    use super::*;

    /// Keeps what is logged (Go `recordingLogger`, local to these tests).
    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl Logger for Lines {
        fn printf(&self, args: fmt::Arguments<'_>) {
            self.0.lock().unwrap().push(args.to_string());
        }
        fn println(&self, args: fmt::Arguments<'_>) {
            self.0.lock().unwrap().push(format!("{args}\n"));
        }
    }

    #[test]
    fn default_config_matches_go_default_config() {
        let c = Config::default();
        assert!(c.enabled);
        assert_eq!(c.project_id, "");
        assert_eq!(c.dataset_id, "agent_analytics");
        assert_eq!(c.table_name, "agent_events");
        assert_eq!(c.max_content_len, 512_000);
        assert_eq!(c.batch_size, 100);
        assert_eq!(c.batch_flush_intv, Duration::from_secs(1));
        assert_eq!(c.queue_max_size, 10000);
        assert!(c.client_options.is_empty());
        assert_eq!(c.clustering_fields, ["event_type", "agent", "user_id"]);
        assert!(c.custom_tags.is_empty());
        assert!(c.log_multi_modal_content);
        assert_eq!(c.shutdown_timeout, Duration::from_secs(10));
        assert!(c.logger.as_ref().unwrap().is_std_default());
        assert_eq!(
            c.retry_config,
            RetryConfig {
                max_retries: 3,
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(10),
                multiplier: 2.0,
            }
        );
        assert_eq!(c.max_inline_text_size, 32 * 1024);
        assert_eq!(c.gcs_max_concurrent_uploads, 8);
        assert_eq!(c.gcs_upload_timeout, Duration::from_secs(10));
        assert_eq!(c.gcs_max_pending_upload_bytes, 256 << 20);
        assert_eq!(c.view_prefix, "v");
        assert!(c.content_formatter.is_none() && c.gcs_offloader.is_none());
        assert!(!c.disable_create_views && !c.disable_schema_upgrade);
        assert!(!c.exactly_once_delivery && !c.capture_full_message_content);
        assert_eq!(c.max_content_length(), 512_000);
    }

    #[test]
    fn zero_config_is_gos_zero_value() {
        let c = Config::zero();
        assert!(!c.enabled);
        assert!(c.logger.is_none());
        assert_eq!(c.retry_config, RetryConfig::default());
        assert_eq!(c.view_prefix, "");
        assert_eq!(c.max_content_len, 0);
        assert!(c.clustering_fields.is_empty());
    }

    /// Adapted from Go `TestConfig_TableNameDefault` (`bigquery_agent_analytics_plugin_test.go`,
    /// 7b-3's ledger).
    #[test]
    fn table_name_default() {
        assert_eq!(Config::default().table_name, "agent_events");
        assert_eq!(Config::zero().apply_defaults().table_name, "agent_events");
        let explicit = Config {
            table_name: "custom".into(),
            ..Config::zero()
        };
        assert_eq!(explicit.apply_defaults().table_name, "custom");
    }

    #[test]
    fn apply_defaults_fills_zero_fields_only() {
        let c = Config::zero().apply_defaults();
        let d = Config::default();
        assert_eq!(c.dataset_id, d.dataset_id);
        assert_eq!(c.max_content_len, d.max_content_len);
        assert_eq!(c.batch_size, d.batch_size);
        assert_eq!(c.batch_flush_intv, d.batch_flush_intv);
        assert_eq!(c.shutdown_timeout, d.shutdown_timeout);
        assert_eq!(c.queue_max_size, d.queue_max_size);
        assert_eq!(c.retry_config, d.retry_config);
        assert_eq!(c.view_prefix, "v");
        // Fields without a zero-value default stay as they are.
        assert!(!c.enabled);
        assert!(c.logger.is_none());
        assert!(c.clustering_fields.is_empty());
        let set = Config {
            dataset_id: "ds".into(),
            max_content_len: -1,
            batch_size: 7,
            batch_flush_intv: Duration::from_millis(5),
            shutdown_timeout: Duration::from_millis(6),
            queue_max_size: 8,
            view_prefix: "custom_v".into(),
            ..Config::zero()
        }
        .apply_defaults();
        assert_eq!(set.dataset_id, "ds");
        assert_eq!(set.max_content_len, -1);
        assert_eq!(set.batch_size, 7);
        assert_eq!(set.batch_flush_intv, Duration::from_millis(5));
        assert_eq!(set.shutdown_timeout, Duration::from_millis(6));
        assert_eq!(set.queue_max_size, 8);
        assert_eq!(set.view_prefix, "custom_v");
    }

    #[test]
    fn apply_defaults_retry_config() {
        let only_retries = Config {
            retry_config: RetryConfig {
                max_retries: 5,
                ..RetryConfig::default()
            },
            ..Config::zero()
        }
        .apply_defaults();
        assert_eq!(
            only_retries.retry_config,
            RetryConfig {
                max_retries: 5,
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(10),
                multiplier: 2.0,
            }
        );
        // MaxRetries 0 with a non-zero field disables retries.
        let disabled = Config {
            retry_config: RetryConfig {
                multiplier: 1.5,
                ..RetryConfig::default()
            },
            ..Config::zero()
        }
        .apply_defaults();
        assert_eq!(disabled.retry_config.max_retries, 0);
        assert_eq!(disabled.retry_config.multiplier, 1.5);
        assert_eq!(
            disabled.retry_config.initial_delay,
            Duration::from_millis(100)
        );
    }

    /// Adapted from Go `TestApplyDefaults_GCSLimits` (`gcs_offloader_test.go`, 7b-2's ledger).
    #[test]
    fn apply_defaults_gcs_limits() {
        let cases: [(i64, i64, i64, i64, i64, i64); 4] = [
            (0, 0, 0, 32 * 1024, 8, 256 << 20),
            (-1, -5, -10, 32 * 1024, 8, 256 << 20),
            (1, 1, 1, 1, 1, 1),
            (1 << 20, 64, 512 << 20, 1 << 20, 64, 512 << 20),
        ];
        for (inline, uploads, pending, want_inline, want_uploads, want_pending) in cases {
            let got = Config {
                max_inline_text_size: inline,
                gcs_max_concurrent_uploads: uploads,
                gcs_max_pending_upload_bytes: pending,
                ..Config::zero()
            }
            .apply_defaults();
            assert_eq!(got.max_inline_text_size, want_inline, "inline {inline}");
            assert_eq!(got.gcs_max_concurrent_uploads, want_uploads);
            assert_eq!(got.gcs_max_pending_upload_bytes, want_pending);
            assert_eq!(got.gcs_upload_timeout, GCS_UPLOAD_TIMEOUT);
        }
    }

    /// Adapted from Go `TestConfig_DefaultOnFeatures` (`bigquery_agent_analytics_plugin_test.go`,
    /// 7b-3's ledger).
    #[test]
    fn default_on_features() {
        let cleared = Config {
            log_multi_modal_content: false,
            disable_session_metadata: true,
            disable_sensitive_key_redaction: true,
            disable_flush_on_run_end: true,
            disable_create_views: true,
            ..Config::default()
        };
        let disabled = Config {
            disable_multi_modal_content: true,
            disable_session_metadata: true,
            disable_sensitive_key_redaction: true,
            disable_flush_on_run_end: true,
            disable_create_views: true,
            ..Config::zero()
        };
        let default_disabled = Config {
            disable_multi_modal_content: true,
            disable_session_metadata: true,
            disable_sensitive_key_redaction: true,
            disable_flush_on_run_end: true,
            disable_create_views: true,
            ..Config::default()
        };
        let literal = Config {
            log_multi_modal_content: true,
            ..Config::zero()
        };
        for (name, c, want) in [
            ("DefaultConfig", Config::default(), true),
            ("Config literal with LogMultiModalContent", literal, true),
            ("DefaultConfig with the features cleared", cleared, false),
            ("Config literal with Disable fields", disabled, false),
            ("DefaultConfig with Disable fields", default_disabled, false),
        ] {
            assert_eq!(c.log_multi_modal_content(), want, "{name}");
            assert_eq!(c.log_session_metadata(), want, "{name}");
            assert_eq!(c.redact_sensitive_keys(), want, "{name}");
            assert_eq!(c.flush_on_run_end(), want, "{name}");
            assert_eq!(c.create_views(), want, "{name}");
        }
    }

    #[test]
    fn max_content_length_honors_capture_full_message_content() {
        let c = Config {
            capture_full_message_content: true,
            ..Config::default()
        };
        assert_eq!(c.max_content_length(), -1);
    }

    #[test]
    fn logf_and_view_logf() {
        let lines = Arc::new(Lines::default());
        let c = Config {
            logger: Some(lines.clone()),
            ..Config::zero()
        };
        c.logf(format_args!("Table {} not found. Creating it...", "t"));
        assert_eq!(
            *lines.0.lock().unwrap(),
            ["Table t not found. Creating it..."]
        );
        let view = c.view_logf().unwrap();
        view.printf(format_args!("x {}", 1));
        view.println(format_args!("y"));
        assert_eq!(lines.0.lock().unwrap()[1..], ["x 1", "y\n"]);
        // Nil logger and Go's log.Default() give no view logger; logf on nil is a no-op.
        Config::zero().logf(format_args!("dropped"));
        assert!(Config::zero().view_logf().is_none());
        assert!(Config::default().view_logf().is_none());
    }

    #[test]
    fn client_options_is_empty() {
        assert!(ClientOptions::default().is_empty());
        let with_endpoint = ClientOptions {
            endpoint: Some("http://127.0.0.1:1".into()),
            ..ClientOptions::default()
        };
        assert!(!with_endpoint.is_empty());
        let no_auth = ClientOptions {
            without_authentication: true,
            ..ClientOptions::default()
        };
        assert!(!no_auth.is_empty());
        assert!(format!("{with_endpoint:?}").contains("127.0.0.1"));
    }

    #[test]
    fn drop_stats_json_uses_go_tags() {
        let stats = DropStats {
            queue_full_drops: 1,
            append_failure_drops: 2,
            formatter_failure_drops: 3,
            shutdown_drops: 4,
            total_drops: 7,
        };
        assert_eq!(
            serde_json::to_string(&stats).unwrap(),
            r#"{"queue_full_drops":1,"append_failure_drops":2,"formatter_failure_drops":3,"shutdown_drops":4,"total_drops":7}"#
        );
    }

    #[tokio::test]
    async fn offloader_close_defaults_to_ok() {
        struct Echo;
        #[async_trait::async_trait]
        impl GcsContentOffloader for Echo {
            async fn upload_content(
                &self,
                _ctx: &Context,
                req: GcsUploadRequest,
            ) -> Result<String, Error> {
                Ok(format!("gs://bucket/{}", req.path))
            }
        }
        let off: Arc<dyn GcsContentOffloader> = Arc::new(Echo);
        let uri = off
            .upload_content(
                &Context::background(),
                GcsUploadRequest {
                    path: "p/o.txt".into(),
                    data: Bytes::from_static(b"x"),
                    content_type: "text/plain".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(uri, "gs://bucket/p/o.txt");
        off.close().await.unwrap();
    }

    #[test]
    fn config_debug_hides_callbacks() {
        let formatter: ContentFormatter = Arc::new(|v, _| Ok(v.clone()));
        let c = Config {
            content_formatter: Some(formatter),
            ..Config::default()
        };
        let text = format!("{c:?}");
        assert!(text.contains("content_formatter: true"), "{text}");
        assert!(text.contains("logger: true"), "{text}");
    }
}
