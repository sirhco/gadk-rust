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

//! The probe's *Config projection* (`scripts/gen-go-agentanalytics` `zzConfigProjection`, see
//! `SCENARIO.md` § config/) for the Rust [`Config`], and the reverse: a `Config` built from a
//! projection. Shared by `tests/golden_config.rs` (public API) and the crate-internal
//! `golden_apply_defaults` unit tests, which include this file with `#[path]`; the including
//! module brings `Config`, `ClientOptions`, `Context`, `Error`, `GcsContentOffloader`,
//! `GcsUploadRequest`, `Logger`, `RetryConfig` and `StdLogger` into scope.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{
    ClientOptions, Config, Context, Error, GcsContentOffloader, GcsUploadRequest, Logger,
    RetryConfig, StdLogger,
};

/// The projection keys holding a Go slice or map: Go tells nil (`null`) from empty (`[]` /
/// `{}`), a Rust `Vec` / `Map` cannot, so both sides write empty as `null`.
const NIL_OR_EMPTY_KEYS: [&str; 7] = [
    "ClusteringFields",
    "CustomTags",
    "EventAllowlist",
    "EventDenylist",
    "PayloadColumnDenylist",
    "CustomMetadataAllowlist",
    "FinalResponseToolNames",
];

/// The projection keys holding a `time.Duration` (ns).
const DURATION_KEYS: [&str; 3] = ["BatchFlushIntv", "ShutdownTimeout", "GCSUploadTimeout"];

fn nanos(d: Duration) -> Value {
    json!(u64::try_from(d.as_nanos()).unwrap())
}

/// A Go float64 as `encoding/json` writes it: whole values without a fraction.
fn go_float(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

fn nil_or(present: bool) -> Value {
    json!(if present { "non-nil" } else { "nil" })
}

fn list(items: &[String]) -> Value {
    if items.is_empty() {
        Value::Null
    } else {
        json!(items)
    }
}

fn logger_kind(logger: Option<&Arc<dyn Logger>>) -> Value {
    json!(match logger {
        None => "nil",
        Some(l) if l.is_std_default() => "log.Default()",
        Some(_) => "custom",
    })
}

/// The probe's Config projection of `c` (keys in Go's field order).
pub fn project(c: &Config) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    put("Enabled", json!(c.enabled));
    put("ProjectID", json!(c.project_id));
    put("DatasetID", json!(c.dataset_id));
    put("TableName", json!(c.table_name));
    put("MaxContentLen", json!(c.max_content_len));
    put("BatchSize", json!(c.batch_size));
    put("BatchFlushIntv", nanos(c.batch_flush_intv));
    put("QueueMaxSize", json!(c.queue_max_size));
    put("ClientOptions", nil_or(!c.client_options.is_empty()));
    put("ClusteringFields", list(&c.clustering_fields));
    put(
        "CustomTags",
        if c.custom_tags.is_empty() {
            Value::Null
        } else {
            Value::Object(c.custom_tags.clone())
        },
    );
    put("LogMultiModalContent", json!(c.log_multi_modal_content));
    put("ShutdownTimeout", nanos(c.shutdown_timeout));
    put("Logger", logger_kind(c.logger.as_ref()));
    put(
        "RetryConfig",
        json!({
            "MaxRetries": c.retry_config.max_retries,
            "InitialDelay": nanos(c.retry_config.initial_delay),
            "MaxDelay": nanos(c.retry_config.max_delay),
            "Multiplier": go_float(c.retry_config.multiplier),
        }),
    );
    put(
        "DisableMultiModalContent",
        json!(c.disable_multi_modal_content),
    );
    put("DisableSessionMetadata", json!(c.disable_session_metadata));
    put(
        "CaptureFullMessageContent",
        json!(c.capture_full_message_content),
    );
    put(
        "DisableSensitiveKeyRedaction",
        json!(c.disable_sensitive_key_redaction),
    );
    put("ContentFormatter", nil_or(c.content_formatter.is_some()));
    put(
        "DebugContentFormatterErrors",
        json!(c.debug_content_formatter_errors),
    );
    put("EventAllowlist", list(&c.event_allowlist));
    put("EventDenylist", list(&c.event_denylist));
    put("PayloadColumnDenylist", list(&c.payload_column_denylist));
    put(
        "CustomMetadataAllowlist",
        list(&c.custom_metadata_allowlist),
    );
    put("FinalResponseToolNames", list(&c.final_response_tool_names));
    put("DisableFlushOnRunEnd", json!(c.disable_flush_on_run_end));
    put("DisableSchemaUpgrade", json!(c.disable_schema_upgrade));
    put("EnableOTelCorrelation", json!(c.enable_otel_correlation));
    put("GCSBucketName", json!(c.gcs_bucket_name));
    put("GCSClientOptions", nil_or(!c.gcs_client_options.is_empty()));
    put("GCSPathPrefix", json!(c.gcs_path_prefix));
    put("ConnectionID", json!(c.connection_id));
    put("MaxInlineTextSize", json!(c.max_inline_text_size));
    put(
        "GCSMaxConcurrentUploads",
        json!(c.gcs_max_concurrent_uploads),
    );
    put("GCSUploadTimeout", nanos(c.gcs_upload_timeout));
    put(
        "GCSMaxPendingUploadBytes",
        json!(c.gcs_max_pending_upload_bytes),
    );
    put("GCSOffloader", nil_or(c.gcs_offloader.is_some()));
    put("DisableCreateViews", json!(c.disable_create_views));
    put("ViewPrefix", json!(c.view_prefix));
    put("ExactlyOnceDelivery", json!(c.exactly_once_delivery));
    Value::Object(m)
}

/// A Go projection with nil-or-empty slices and maps written as `null` (see
/// [`NIL_OR_EMPTY_KEYS`]) and `drop` keys removed.
pub fn normalize(go: &Value, drop: &[&str]) -> Value {
    let mut m = go.as_object().unwrap().clone();
    for k in NIL_OR_EMPTY_KEYS {
        let empty = match &m[k] {
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            _ => false,
        };
        if empty {
            m.insert(k.to_owned(), Value::Null);
        }
    }
    for k in drop {
        m.remove(*k);
    }
    Value::Object(m)
}

/// A caller-supplied logger (projection `"custom"`).
struct CustomLogger;

impl Logger for CustomLogger {
    fn printf(&self, _args: fmt::Arguments<'_>) {}
    fn println(&self, _args: fmt::Arguments<'_>) {}
}

/// A caller-supplied offloader (projection `"non-nil"`).
struct StubOffloader;

#[async_trait::async_trait]
impl GcsContentOffloader for StubOffloader {
    async fn upload_content(
        &self,
        _ctx: &Context,
        _req: GcsUploadRequest,
    ) -> Result<String, Error> {
        Ok(String::new())
    }
}

fn options(v: &Value) -> ClientOptions {
    match v.as_str().unwrap() {
        "nil" => ClientOptions::default(),
        "non-nil" => ClientOptions {
            without_authentication: true,
            ..ClientOptions::default()
        },
        other => panic!("unexpected client options {other}"),
    }
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::Null => Vec::new(),
        Value::Array(a) => a.iter().map(|s| s.as_str().unwrap().to_owned()).collect(),
        other => panic!("unexpected list {other}"),
    }
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap()
}

/// A Go duration in ns; `None` when negative (a Rust `Duration` cannot be).
fn duration(v: &Value) -> Option<Duration> {
    u64::try_from(int(v)).ok().map(Duration::from_nanos)
}

/// The `Config` a projection describes, and the projection keys it could not represent
/// (negative durations, set to zero instead).
pub fn build(p: &Value) -> (Config, Vec<&'static str>) {
    let mut unrepresentable = Vec::new();
    let mut dur = |k: &'static str| match duration(&p[k]) {
        Some(d) => d,
        None => {
            unrepresentable.push(k);
            Duration::ZERO
        }
    };
    let batch_flush_intv = dur(DURATION_KEYS[0]);
    let shutdown_timeout = dur(DURATION_KEYS[1]);
    let gcs_upload_timeout = dur(DURATION_KEYS[2]);
    let retry = &p["RetryConfig"];
    let config = Config {
        enabled: p["Enabled"].as_bool().unwrap(),
        project_id: p["ProjectID"].as_str().unwrap().to_owned(),
        dataset_id: p["DatasetID"].as_str().unwrap().to_owned(),
        table_name: p["TableName"].as_str().unwrap().to_owned(),
        max_content_len: int(&p["MaxContentLen"]),
        batch_size: int(&p["BatchSize"]),
        batch_flush_intv,
        queue_max_size: int(&p["QueueMaxSize"]),
        client_options: options(&p["ClientOptions"]),
        clustering_fields: strings(&p["ClusteringFields"]),
        custom_tags: p["CustomTags"].as_object().cloned().unwrap_or_default(),
        log_multi_modal_content: p["LogMultiModalContent"].as_bool().unwrap(),
        shutdown_timeout,
        logger: match p["Logger"].as_str().unwrap() {
            "nil" => None,
            "log.Default()" => Some(Arc::new(StdLogger) as Arc<dyn Logger>),
            "custom" => Some(Arc::new(CustomLogger) as Arc<dyn Logger>),
            other => panic!("unexpected logger {other}"),
        },
        retry_config: RetryConfig {
            max_retries: int(&retry["MaxRetries"]),
            initial_delay: duration(&retry["InitialDelay"]).unwrap(),
            max_delay: duration(&retry["MaxDelay"]).unwrap(),
            multiplier: retry["Multiplier"].as_f64().unwrap(),
        },
        disable_multi_modal_content: p["DisableMultiModalContent"].as_bool().unwrap(),
        disable_session_metadata: p["DisableSessionMetadata"].as_bool().unwrap(),
        capture_full_message_content: p["CaptureFullMessageContent"].as_bool().unwrap(),
        disable_sensitive_key_redaction: p["DisableSensitiveKeyRedaction"].as_bool().unwrap(),
        content_formatter: match p["ContentFormatter"].as_str().unwrap() {
            "nil" => None,
            _ => Some(Arc::new(|v: &Value, _: &str| Ok(v.clone()))),
        },
        debug_content_formatter_errors: p["DebugContentFormatterErrors"].as_bool().unwrap(),
        event_allowlist: strings(&p["EventAllowlist"]),
        event_denylist: strings(&p["EventDenylist"]),
        payload_column_denylist: strings(&p["PayloadColumnDenylist"]),
        custom_metadata_allowlist: strings(&p["CustomMetadataAllowlist"]),
        final_response_tool_names: strings(&p["FinalResponseToolNames"]),
        disable_flush_on_run_end: p["DisableFlushOnRunEnd"].as_bool().unwrap(),
        disable_schema_upgrade: p["DisableSchemaUpgrade"].as_bool().unwrap(),
        enable_otel_correlation: p["EnableOTelCorrelation"].as_bool().unwrap(),
        gcs_bucket_name: p["GCSBucketName"].as_str().unwrap().to_owned(),
        gcs_client_options: options(&p["GCSClientOptions"]),
        gcs_path_prefix: p["GCSPathPrefix"].as_str().unwrap().to_owned(),
        connection_id: p["ConnectionID"].as_str().unwrap().to_owned(),
        max_inline_text_size: int(&p["MaxInlineTextSize"]),
        gcs_max_concurrent_uploads: int(&p["GCSMaxConcurrentUploads"]),
        gcs_upload_timeout,
        gcs_max_pending_upload_bytes: int(&p["GCSMaxPendingUploadBytes"]),
        gcs_offloader: match p["GCSOffloader"].as_str().unwrap() {
            "nil" => None,
            _ => Some(Arc::new(StubOffloader) as Arc<dyn GcsContentOffloader>),
        },
        disable_create_views: p["DisableCreateViews"].as_bool().unwrap(),
        view_prefix: p["ViewPrefix"].as_str().unwrap().to_owned(),
        exactly_once_delivery: p["ExactlyOnceDelivery"].as_bool().unwrap(),
    };
    (config, unrepresentable)
}
