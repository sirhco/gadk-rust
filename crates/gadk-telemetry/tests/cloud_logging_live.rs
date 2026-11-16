// Copyright 2026 Chris Olson
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

//! Opt-in round trip against a real Cloud Logging project: writes one record through
//! `with_otel_to_cloud(true)` and reads it back with `entries:list`. The record is emitted
//! directly on the logger provider with an explicit span context (the bridge's own mapping to
//! log records is covered by `bridge.rs`'s tests).
//!
//! Run: `GADK_CLOUD_LOGGING_PROJECT=<project> cargo test -p gadk-telemetry --test
//! cloud_logging_live -- --ignored` with Application Default Credentials that may write and
//! read logs in that project.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gadk_model::gcp_auth::{adc_credentials, credential_headers};
use gadk_telemetry::{
    new, with_cloud_log_name, with_gcp_quota_project, with_gcp_resource_project,
    with_meter_provider, with_otel_to_cloud, with_tracer_provider,
};
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::trace::{SpanId, TraceFlags, TraceId};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Value, json};

const LOG_ID: &str = "gadk-live-test";
const TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
const SPAN: &str = "b7ad6b7169203331";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "writes to a real Cloud Logging project; set GADK_CLOUD_LOGGING_PROJECT"]
async fn live_entries_write_round_trip() {
    let project = std::env::var("GADK_CLOUD_LOGGING_PROJECT")
        .expect("set GADK_CLOUD_LOGGING_PROJECT to run this test");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let marker = format!("gadk-live-{}-{nanos}", std::process::id());

    let providers = new(&[
        with_otel_to_cloud(true),
        with_gcp_resource_project(project.clone()),
        with_gcp_quota_project(project.clone()),
        with_cloud_log_name(LOG_ID),
        // Supplied providers: only the log exporter talks to GCP.
        with_tracer_provider(SdkTracerProvider::builder().build()),
        with_meter_provider(SdkMeterProvider::builder().build()),
    ])
    .await
    .unwrap();
    let lp = providers.logger_provider.clone().unwrap();
    let logger = lp.logger("gadk-live");
    let mut record = logger.create_log_record();
    record.set_event_name("gadk.live_test");
    record.set_severity_number(Severity::Info);
    record.set_body(AnyValue::from(marker.clone()));
    record.set_trace_context(
        TraceId::from_hex(TRACE).unwrap(),
        SpanId::from_hex(SPAN).unwrap(),
        Some(TraceFlags::SAMPLED),
    );
    logger.emit(record);
    // The batch processor exports on its own thread; its HTTP request runs on this runtime.
    tokio::task::spawn_blocking(move || lp.force_flush())
        .await
        .unwrap()
        .unwrap();

    let creds = adc_credentials(None, &["https://www.googleapis.com/auth/cloud-platform"]).unwrap();
    let client = reqwest::Client::new();
    let filter =
        format!(r#"logName="projects/{project}/logs/{LOG_ID}" AND jsonPayload.body="{marker}""#);
    let deadline = Instant::now() + Duration::from_secs(60);
    let entry = loop {
        let mut headers = credential_headers(&creds).await.unwrap();
        // User ADC may already carry its own quota project: replace it, never send two.
        headers.insert(
            HeaderName::from_static("x-goog-user-project"),
            HeaderValue::from_str(&project).unwrap(),
        );
        let resp = client
            .post("https://logging.googleapis.com/v2/entries:list")
            .headers(headers)
            .json(&json!({
                "resourceNames": [format!("projects/{project}")],
                "filter": filter,
                "pageSize": 5
            }))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        assert!(status.is_success(), "entries:list: {status}: {text}");
        let resp: Value = serde_json::from_str(&text).unwrap();
        if let Some(e) = resp["entries"].as_array().and_then(|a| a.first()) {
            break e.clone();
        }
        assert!(
            Instant::now() < deadline,
            "entry {marker} not found within 60 s"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    };
    assert_eq!(
        entry["trace"],
        json!(format!("projects/{project}/traces/{TRACE}"))
    );
    assert_eq!(entry["spanId"], json!(SPAN));
    assert_eq!(entry["traceSampled"], json!(true));
    assert_eq!(entry["severity"], json!("INFO"));
    assert_eq!(entry["jsonPayload"]["event.name"], json!("gadk.live_test"));
    providers.shutdown().await.unwrap();
}
