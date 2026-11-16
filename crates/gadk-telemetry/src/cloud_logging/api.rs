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

//! The Cloud Logging `entries:write` log exporter.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use opentelemetry_http::{Bytes, HttpClient, Request};
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use serde_json::Value;

use super::entry::api_entry;
use super::{CloudLogTarget, REQUEST_LIMIT, WARN_TARGET};

/// At most this many response-body bytes appear in an export error.
pub(crate) const BODY_EXCERPT: usize = 1024;

/// Writes log records to Cloud Logging with `entries:write`.
pub(crate) struct CloudLoggingExporter {
    client: Arc<dyn HttpClient>,
    target: CloudLogTarget,
    endpoint: String,
    /// Set by a failed export, cleared by a successful one: only the first of a run of failures
    /// warns.
    failing: AtomicBool,
}

impl fmt::Debug for CloudLoggingExporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudLoggingExporter")
            .field("target", &self.target)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl CloudLoggingExporter {
    pub(crate) fn new(
        client: Arc<dyn HttpClient>,
        target: CloudLogTarget,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            client,
            target,
            endpoint: endpoint.into(),
            failing: AtomicBool::new(false),
        }
    }

    /// The request bodies for `batch`: each `{"logName", "resource", "partialSuccess": true,
    /// "entries": [...]}` and at most [`REQUEST_LIMIT`] bytes (a single entry is at most
    /// `ENTRY_LIMIT`, so every entry fits a request).
    pub(crate) fn request_bodies(&self, batch: &LogBatch<'_>, now: SystemTime) -> Vec<Vec<u8>> {
        let head = format!(
            r#"{{"logName":{},"resource":{},"partialSuccess":true,"entries":["#,
            Value::from(self.target.log_name()),
            self.target.resource.to_json(),
        );
        const TAIL: &[u8] = b"]}";
        let mut bodies = Vec::new();
        let mut cur = head.clone().into_bytes();
        let mut count = 0usize;
        for (record, _) in batch.iter() {
            // Serializing a `Value` cannot fail (its map keys are always strings).
            let Ok(entry) = serde_json::to_vec(&api_entry(record, &self.target, now)) else {
                continue;
            };
            if count > 0 && cur.len() + 1 + entry.len() + TAIL.len() > REQUEST_LIMIT {
                cur.extend_from_slice(TAIL);
                bodies.push(std::mem::replace(&mut cur, head.clone().into_bytes()));
                count = 0;
            }
            if count > 0 {
                cur.push(b',');
            }
            cur.extend_from_slice(&entry);
            count += 1;
        }
        if count > 0 {
            cur.extend_from_slice(TAIL);
            bodies.push(cur);
        }
        bodies
    }

    async fn send(&self, body: Vec<u8>) -> Result<(), OTelSdkError> {
        let request = Request::post(self.endpoint.as_str())
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Bytes::from(body))
            .map_err(|e| OTelSdkError::InternalFailure(format!("cloud logging: request: {e}")))?;
        let response = self.client.send_bytes(request).await.map_err(|e| {
            OTelSdkError::InternalFailure(format!("cloud logging: entries:write: {e}"))
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let excerpt = body_excerpt(response.body());
        Err(OTelSdkError::InternalFailure(format!(
            "cloud logging: entries:write: HTTP {}: {excerpt}",
            status.as_u16()
        )))
    }
}

impl LogExporter for CloudLoggingExporter {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        let bodies = self.request_bodies(&batch, SystemTime::now());
        let mut first_err = None;
        for body in bodies {
            if let Err(e) = self.send(body).await {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            None => {
                self.failing.store(false, Ordering::SeqCst);
                Ok(())
            }
            Some(err) => {
                // The SDK drops export results off the `force_flush` path, so the first of a run
                // of failures is also a `tracing` warning. The error holds only the status and
                // body excerpt or the transport error, never record content.
                if !self.failing.swap(true, Ordering::SeqCst) {
                    tracing::warn!(target: WARN_TARGET, "{err}");
                }
                Err(err)
            }
        }
    }
}

/// At most [`BODY_EXCERPT`] bytes of `body`, decoded lossily and cut at a character boundary.
fn body_excerpt(body: &[u8]) -> String {
    // Lossy decoding never shrinks the input, so the first `BODY_EXCERPT + 4` bytes cover
    // every character that can end within `BODY_EXCERPT` decoded bytes.
    let mut text = String::from_utf8_lossy(&body[..body.len().min(BODY_EXCERPT + 4)]).into_owned();
    text.truncate(text.floor_char_boundary(BODY_EXCERPT));
    text
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU16, Ordering};

    use async_trait::async_trait;
    use opentelemetry::logs::{AnyValue, LogRecord as _};
    use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
    use opentelemetry_sdk::logs::LogBatch;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::cloud_logging::ENTRY_LIMIT;
    use crate::cloud_logging::test_records::{Warnings, record, refs};

    #[derive(Debug)]
    struct FakeClient {
        status: AtomicU16,
        body: Vec<u8>,
        requests: Mutex<Vec<Request<Bytes>>>,
    }

    impl FakeClient {
        fn new(status: u16, body: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                status: AtomicU16::new(status),
                body: body.to_vec(),
                requests: Mutex::default(),
            })
        }
        fn bodies(&self) -> Vec<Value> {
            self.requests
                .lock()
                .iter()
                .map(|r| serde_json::from_slice(r.body()).unwrap())
                .collect()
        }
    }

    #[async_trait]
    impl HttpClient for FakeClient {
        async fn send_bytes(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
            self.requests.lock().push(request);
            Ok(Response::builder()
                .status(self.status.load(Ordering::SeqCst))
                .body(Bytes::from(self.body.clone()))?)
        }
    }

    fn exporter(client: &Arc<FakeClient>, log_id: &str) -> CloudLoggingExporter {
        let client: Arc<dyn HttpClient> = client.clone();
        CloudLoggingExporter::new(
            client,
            CloudLogTarget::new("p", log_id, None),
            "http://fake/v2/entries:write",
        )
    }

    #[tokio::test]
    async fn one_request_with_log_name_resource_and_partial_success() {
        let client = FakeClient::new(200, b"{}");
        let records = vec![record(|r| r.set_event_name("e"))];
        exporter(&client, "a/b")
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap();
        let reqs = client.requests.lock();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].uri(), "http://fake/v2/entries:write");
        assert_eq!(reqs[0].method(), "POST");
        assert_eq!(reqs[0].headers()["content-type"], "application/json");
        drop(reqs);
        let body = &client.bodies()[0];
        assert_eq!(body["logName"], json!("projects/p/logs/a%2Fb"));
        assert_eq!(
            body["resource"],
            json!({"type": "global", "labels": {"project_id": "p"}})
        );
        assert_eq!(body["partialSuccess"], json!(true));
        assert_eq!(body["entries"][0]["jsonPayload"]["event.name"], json!("e"));
    }

    #[tokio::test]
    async fn default_log_name() {
        let client = FakeClient::new(200, b"{}");
        let records = vec![record(|_| {})];
        exporter(&client, "")
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap();
        assert_eq!(client.bodies()[0]["logName"], json!("projects/p/logs/gadk"));
    }

    #[tokio::test]
    async fn large_batches_split_under_the_request_limit() {
        let client = FakeClient::new(200, b"{}");
        let records: Vec<_> = (0..40)
            .map(|i| record(|r| r.add_attribute("a", format!("{i}{}", "q".repeat(250_000)))))
            .collect();
        exporter(&client, "")
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap();
        let reqs = client.requests.lock();
        assert!(reqs.len() >= 2, "{} requests", reqs.len());
        let mut entries = 0;
        for r in reqs.iter() {
            assert!(r.body().len() <= REQUEST_LIMIT);
            let v: Value = serde_json::from_slice(r.body()).unwrap();
            assert_eq!(v["partialSuccess"], json!(true));
            entries += v["entries"].as_array().unwrap().len();
        }
        assert_eq!(entries, 40);
    }

    #[tokio::test]
    async fn an_oversized_record_is_truncated() {
        let client = FakeClient::new(200, b"{}");
        let records = vec![record(|r| {
            r.set_body(AnyValue::String("w".repeat(1_000_000).into()))
        })];
        exporter(&client, "")
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap();
        let entry = &client.bodies()[0]["entries"][0];
        assert!(serde_json::to_vec(entry).unwrap().len() <= ENTRY_LIMIT);
        assert_eq!(entry["jsonPayload"]["gadk.truncated"], json!(true));
    }

    #[tokio::test]
    async fn non_2xx_reports_status_and_a_bounded_body() {
        let body = "e".repeat(5_000);
        let client = FakeClient::new(403, body.as_bytes());
        let records = vec![record(|_| {})];
        let err = exporter(&client, "")
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("HTTP 403"), "{text}");
        assert!(text.contains(&"e".repeat(BODY_EXCERPT)), "{text}");
        assert!(!text.contains(&"e".repeat(BODY_EXCERPT + 1)), "{text}");
    }

    #[tokio::test]
    async fn the_body_excerpt_ends_on_a_whole_character() {
        // "a" then 2-byte characters: byte 1024 falls inside the character at 1023..1025.
        let body = format!("a{}", "é".repeat(3_000));
        assert!(!body.is_char_boundary(BODY_EXCERPT));
        let client = FakeClient::new(500, body.as_bytes());
        let records = vec![record(|_| {})];
        let err = exporter(&client, "")
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap_err();
        let text = err.to_string();
        let excerpt = text.split_once("HTTP 500: ").unwrap().1;
        assert!(excerpt.len() <= BODY_EXCERPT, "{}", excerpt.len());
        assert_eq!(excerpt, &body[..BODY_EXCERPT - 1]);
        assert!(!excerpt.contains('\u{FFFD}'), "{excerpt}");
    }

    #[tokio::test]
    async fn an_empty_batch_sends_nothing() {
        let client = FakeClient::new(200, b"{}");
        exporter(&client, "")
            .export(LogBatch::new(&[]))
            .await
            .unwrap();
        assert!(client.requests.lock().is_empty());
    }

    #[tokio::test]
    async fn a_failing_write_warns_once_until_a_write_succeeds() {
        let warnings = Warnings::default();
        let _guard = warnings.capture();
        let client = FakeClient::new(403, b"permission denied");
        let exp = exporter(&client, "");
        let records = vec![record(|r| r.set_body(AnyValue::String("secret".into())))];
        assert!(exp.export(LogBatch::new(&refs(&records))).await.is_err());
        let first = warnings.take();
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(
            first[0].contains("HTTP 403: permission denied"),
            "{first:?}"
        );
        assert!(!first[0].contains("secret"), "{first:?}");
        assert!(exp.export(LogBatch::new(&refs(&records))).await.is_err());
        assert!(warnings.take().is_empty());
        client.status.store(200, Ordering::SeqCst);
        exp.export(LogBatch::new(&refs(&records))).await.unwrap();
        assert!(warnings.take().is_empty());
        client.status.store(500, Ordering::SeqCst);
        assert!(exp.export(LogBatch::new(&refs(&records))).await.is_err());
        assert_eq!(warnings.take().len(), 1);
    }
}
