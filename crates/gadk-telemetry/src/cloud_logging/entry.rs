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

//! Maps an OpenTelemetry log record to a Cloud Logging entry (API) or a structured stdout line.

use std::collections::BTreeMap;
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, SecondsFormat, Utc};
use opentelemetry::Key;
use opentelemetry::logs::AnyValue;
use opentelemetry::trace::{SpanId, TraceId};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLogRecord;
use serde_json::{Map, Number, Value};

use super::truncate::fit;
use super::{CloudLogTarget, ENTRY_LIMIT, MonitoredResource};

/// An `AnyValue` as JSON.
pub(crate) fn any_value_to_json(v: &AnyValue) -> Value {
    match v {
        AnyValue::Int(i) => Value::from(*i),
        AnyValue::Double(d) if d.is_nan() => Value::from("NaN"),
        AnyValue::Double(d) if d.is_infinite() => {
            Value::from(if *d > 0.0 { "Infinity" } else { "-Infinity" })
        }
        AnyValue::Double(d) => Number::from_f64(*d).map_or(Value::Null, Value::Number),
        AnyValue::String(s) => Value::from(s.as_str()),
        AnyValue::Boolean(b) => Value::Bool(*b),
        AnyValue::Bytes(b) => Value::from(STANDARD.encode(b.as_slice())),
        AnyValue::ListAny(l) => Value::Array(l.iter().map(any_value_to_json).collect()),
        AnyValue::Map(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.as_str().to_owned(), any_value_to_json(v)))
                .collect(),
        ),
        other => Value::from(format!("{other:?}")),
    }
}

/// `event.name`, `body` and `attributes`, each only when present.
pub(crate) fn payload(record: &SdkLogRecord) -> Map<String, Value> {
    let mut p = Map::new();
    if let Some(name) = record.event_name() {
        p.insert("event.name".to_owned(), Value::from(name));
    }
    if let Some(body) = record.body() {
        p.insert("body".to_owned(), any_value_to_json(body));
    }
    let attrs: Map<String, Value> = record
        .attributes_iter()
        .map(|(k, v)| (k.as_str().to_owned(), any_value_to_json(v)))
        .collect();
    if !attrs.is_empty() {
        p.insert("attributes".to_owned(), Value::Object(attrs));
    }
    p
}

/// The Cloud Logging severity of the record's OTel severity number.
pub(crate) fn severity(record: &SdkLogRecord) -> &'static str {
    match record.severity_number().map(|s| s as i32) {
        None => "DEFAULT",
        Some(..=8) => "DEBUG",
        Some(9..=12) => "INFO",
        Some(13..=16) => "WARNING",
        Some(17..=20) => "ERROR",
        Some(_) => "CRITICAL",
    }
}

/// The record time, else the observed time, else `now`: UTC, nine fractional digits, `Z`.
pub(crate) fn timestamp(record: &SdkLogRecord, now: SystemTime) -> String {
    let t = record
        .timestamp()
        .or_else(|| record.observed_timestamp())
        .unwrap_or(now);
    DateTime::<Utc>::from(t).to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// A record's Cloud Trace correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceFields {
    pub(crate) trace: String,
    pub(crate) span_id: String,
    pub(crate) sampled: bool,
}

/// The correlation fields, only for a non-zero trace ID and span ID.
pub(crate) fn trace_fields(record: &SdkLogRecord, project: &str) -> Option<TraceFields> {
    let tc = record.trace_context()?;
    if tc.trace_id == TraceId::INVALID || tc.span_id == SpanId::INVALID {
        return None;
    }
    Some(TraceFields {
        trace: format!("projects/{project}/traces/{}", tc.trace_id),
        span_id: tc.span_id.to_string(),
        sampled: tc.trace_flags.is_some_and(|f| f.is_sampled()),
    })
}

/// `cloud_run_revision` when the resource is a Cloud Run service with every label; else `global`.
pub(crate) fn monitored_resource(resource: Option<&Resource>, project: &str) -> MonitoredResource {
    let get = |k: &'static str| {
        resource
            .and_then(|r| r.get(&Key::from_static_str(k)))
            .map(|v| v.to_string())
            .filter(|s| !s.is_empty())
    };
    if get("cloud.platform").as_deref() == Some("gcp_cloud_run")
        && let (Some(service), Some(revision), Some(location)) =
            (get("faas.name"), get("faas.version"), get("cloud.region"))
    {
        return MonitoredResource {
            kind: "cloud_run_revision",
            labels: BTreeMap::from([
                ("project_id", project.to_owned()),
                ("service_name", service),
                ("revision_name", revision),
                ("location", location),
            ]),
        };
    }
    MonitoredResource {
        kind: "global",
        labels: BTreeMap::from([("project_id", project.to_owned())]),
    }
}

/// One `entries:write` entry (log name and resource are set on the request).
pub(crate) fn api_entry(record: &SdkLogRecord, target: &CloudLogTarget, now: SystemTime) -> Value {
    let mut e = Map::new();
    e.insert("severity".to_owned(), Value::from(severity(record)));
    e.insert("timestamp".to_owned(), Value::from(timestamp(record, now)));
    if let Some(t) = trace_fields(record, &target.project) {
        e.insert("trace".to_owned(), Value::from(t.trace));
        e.insert("spanId".to_owned(), Value::from(t.span_id));
        e.insert("traceSampled".to_owned(), Value::Bool(t.sampled));
    }
    let mut p = payload(record);
    // The envelope plus `,"jsonPayload":`.
    let overhead = serde_json::to_vec(&e).map_or(0, |b| b.len()) + 15;
    fit(&mut p, overhead, ENTRY_LIMIT);
    e.insert("jsonPayload".to_owned(), Value::Object(p));
    Value::Object(e)
}

/// One stdout line: Cloud Run's special keys plus the payload fields at top level.
pub(crate) fn stdout_line(
    record: &SdkLogRecord,
    target: &CloudLogTarget,
    now: SystemTime,
) -> Value {
    let mut special = Map::new();
    special.insert("severity".to_owned(), Value::from(severity(record)));
    special.insert("timestamp".to_owned(), Value::from(timestamp(record, now)));
    if let Some(t) = trace_fields(record, &target.project) {
        special.insert(
            "logging.googleapis.com/trace".to_owned(),
            Value::from(t.trace),
        );
        special.insert(
            "logging.googleapis.com/spanId".to_owned(),
            Value::from(t.span_id),
        );
        special.insert(
            "logging.googleapis.com/trace_sampled".to_owned(),
            Value::Bool(t.sampled),
        );
    }
    let mut line = payload(record);
    let overhead = serde_json::to_vec(&special).map_or(0, |b| b.len()) + 1;
    fit(&mut line, overhead, ENTRY_LIMIT);
    line.extend(special);
    Value::Object(line)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use opentelemetry::logs::{AnyValue, LogRecord as _, Severity};
    use opentelemetry::trace::{SpanId, TraceFlags, TraceId};
    use opentelemetry::{Key, KeyValue};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::cloud_logging::test_records::record;

    fn at(secs: u64, nanos: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(secs, nanos)
    }

    #[test]
    fn any_values_convert() {
        let map: HashMap<Key, AnyValue> =
            [(Key::from("k"), AnyValue::Int(1))].into_iter().collect();
        let cases = [
            (AnyValue::String("s".into()), json!("s")),
            (AnyValue::Boolean(true), json!(true)),
            (AnyValue::Int(-7), json!(-7)),
            (AnyValue::Double(0.5), json!(0.5)),
            (AnyValue::Double(f64::NAN), json!("NaN")),
            (AnyValue::Double(f64::INFINITY), json!("Infinity")),
            (AnyValue::Double(f64::NEG_INFINITY), json!("-Infinity")),
            (AnyValue::Bytes(Box::new(vec![0, 1, 255])), json!("AAH/")),
            (
                AnyValue::ListAny(Box::new(vec![
                    AnyValue::Int(1),
                    AnyValue::String("x".into()),
                ])),
                json!([1, "x"]),
            ),
            (AnyValue::Map(Box::new(map)), json!({"k": 1})),
        ];
        for (v, want) in cases {
            assert_eq!(any_value_to_json(&v), want, "{v:?}");
        }
    }

    #[test]
    fn payload_has_event_name_body_and_attributes_only_when_set() {
        let (r, _) = record(|r| {
            r.set_event_name("gen_ai.choice");
            r.set_body(AnyValue::String("hi".into()));
            r.add_attribute("n", 1_i64);
        });
        assert_eq!(
            Value::Object(payload(&r)),
            json!({"event.name": "gen_ai.choice", "body": "hi", "attributes": {"n": 1}})
        );
        let (empty, _) = record(|_| {});
        assert_eq!(Value::Object(payload(&empty)), json!({}));
    }

    #[test]
    fn severities_map() {
        let cases = [
            (None, "DEFAULT"),
            (Some(Severity::Trace), "DEBUG"),
            (Some(Severity::Debug4), "DEBUG"),
            (Some(Severity::Info), "INFO"),
            (Some(Severity::Info4), "INFO"),
            (Some(Severity::Warn), "WARNING"),
            (Some(Severity::Error3), "ERROR"),
            (Some(Severity::Fatal), "CRITICAL"),
            (Some(Severity::Fatal4), "CRITICAL"),
        ];
        for (sev, want) in cases {
            let (r, _) = record(|r| {
                if let Some(s) = sev {
                    r.set_severity_number(s);
                }
            });
            assert_eq!(severity(&r), want, "{sev:?}");
        }
    }

    #[test]
    fn timestamp_prefers_record_then_observed_then_now() {
        let (r, _) = record(|r| {
            r.set_timestamp(at(1_700_000_000, 5));
            r.set_observed_timestamp(at(1_700_000_001, 0));
        });
        assert_eq!(timestamp(&r, at(0, 0)), "2023-11-14T22:13:20.000000005Z");
        let (r, _) = record(|r| r.set_observed_timestamp(at(1_700_000_001, 0)));
        assert_eq!(timestamp(&r, at(0, 0)), "2023-11-14T22:13:21.000000000Z");
    }

    #[test]
    fn trace_fields_need_non_zero_ids() {
        let trace = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span = SpanId::from_hex("b7ad6b7169203331").unwrap();
        let (r, _) = record(|r| r.set_trace_context(trace, span, Some(TraceFlags::SAMPLED)));
        assert_eq!(
            trace_fields(&r, "p"),
            Some(TraceFields {
                trace: "projects/p/traces/0af7651916cd43dd8448eb211c80319c".into(),
                span_id: "b7ad6b7169203331".into(),
                sampled: true,
            })
        );
        let (r, _) = record(|r| r.set_trace_context(trace, span, None));
        assert_eq!(trace_fields(&r, "p").map(|t| t.sampled), Some(false));
        let (r, _) = record(|r| r.set_trace_context(TraceId::INVALID, span, None));
        assert_eq!(trace_fields(&r, "p"), None);
        let (r, _) = record(|r| r.set_trace_context(trace, SpanId::INVALID, None));
        assert_eq!(trace_fields(&r, "p"), None);
        let (r, _) = record(|_| {});
        assert_eq!(trace_fields(&r, "p"), None);
    }

    fn resource(attrs: &[(&'static str, &'static str)]) -> Resource {
        Resource::builder_empty()
            .with_attributes(attrs.iter().map(|(k, v)| KeyValue::new(*k, *v)))
            .build()
    }

    #[test]
    fn cloud_run_resource_needs_every_label() {
        let full = resource(&[
            ("cloud.platform", "gcp_cloud_run"),
            ("faas.name", "svc"),
            ("faas.version", "svc-00001"),
            ("cloud.region", "us-central1"),
        ]);
        assert_eq!(
            monitored_resource(Some(&full), "p").to_json(),
            json!({"type": "cloud_run_revision", "labels": {
                "project_id": "p", "service_name": "svc",
                "revision_name": "svc-00001", "location": "us-central1"}})
        );
        // A Cloud Run Job has no faas.version.
        let job = resource(&[
            ("cloud.platform", "gcp_cloud_run"),
            ("faas.name", "job"),
            ("cloud.region", "us-central1"),
        ]);
        let global = json!({"type": "global", "labels": {"project_id": "p"}});
        assert_eq!(monitored_resource(Some(&job), "p").to_json(), global);
        assert_eq!(
            monitored_resource(Some(&resource(&[])), "p").to_json(),
            global
        );
        assert_eq!(monitored_resource(None, "p").to_json(), global);
    }

    #[test]
    fn api_entry_shape() {
        let trace = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span = SpanId::from_hex("b7ad6b7169203331").unwrap();
        let (r, _) = record(|r| {
            r.set_timestamp(at(1_700_000_000, 0));
            r.set_severity_number(Severity::Debug);
            r.set_event_name("e");
            r.set_trace_context(trace, span, Some(TraceFlags::SAMPLED));
        });
        let target = CloudLogTarget::new("p", "", None);
        assert_eq!(
            api_entry(&r, &target, at(0, 0)),
            json!({
                "severity": "DEBUG",
                "timestamp": "2023-11-14T22:13:20.000000000Z",
                "trace": "projects/p/traces/0af7651916cd43dd8448eb211c80319c",
                "spanId": "b7ad6b7169203331",
                "traceSampled": true,
                "jsonPayload": {"event.name": "e"}
            })
        );
    }

    #[test]
    fn stdout_line_shape() {
        let trace = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span = SpanId::from_hex("b7ad6b7169203331").unwrap();
        let (r, _) = record(|r| {
            r.set_timestamp(at(1_700_000_000, 0));
            r.set_body(AnyValue::String("b".into()));
            r.set_trace_context(trace, span, None);
        });
        let target = CloudLogTarget::new("p", "", None);
        assert_eq!(
            stdout_line(&r, &target, at(0, 0)),
            json!({
                "severity": "DEFAULT",
                "timestamp": "2023-11-14T22:13:20.000000000Z",
                "logging.googleapis.com/trace": "projects/p/traces/0af7651916cd43dd8448eb211c80319c",
                "logging.googleapis.com/spanId": "b7ad6b7169203331",
                "logging.googleapis.com/trace_sampled": false,
                "body": "b"
            })
        );
        let (plain, _) = record(|r| r.set_timestamp(at(1_700_000_000, 0)));
        let line = stdout_line(&plain, &target, at(0, 0));
        assert!(line.get("logging.googleapis.com/trace").is_none());
    }

    #[test]
    fn oversized_entries_fit() {
        let big = "x".repeat(1_000_000);
        let (r, _) = record(|r| r.add_attribute("big", big.clone()));
        let target = CloudLogTarget::new("p", "", None);
        let e = api_entry(&r, &target, at(0, 0));
        assert!(serde_json::to_vec(&e).unwrap().len() <= ENTRY_LIMIT);
        assert_eq!(e["jsonPayload"]["gadk.truncated"], json!(true));
        let line = stdout_line(&r, &target, at(0, 0));
        assert!(serde_json::to_vec(&line).unwrap().len() <= ENTRY_LIMIT);
        assert_eq!(line["gadk.truncated"], json!(true));
    }
}
