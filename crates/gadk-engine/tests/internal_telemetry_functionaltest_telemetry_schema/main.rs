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

//! Port of ADK-Go `internal/telemetry/functionaltest/telemetry_schema_test.go`: each scenario case
//! runs end to end through a real runner, and its spans and log records, digested, must equal the
//! golden ADK-Go recorded (`testdata/<scenario>/<case>.json`, copied verbatim from ADK-Go). Spans
//! and log records are the `tracing` spans and D6 events the engine emits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod scenarios;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_engine::testutil::telemetry::{ENV_LOCK, build_digest, capture_logs};
use gadk_platform::env::MapEnv;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::Value;

use scenarios::{Case, cases};

/// Go `captureContentEnvVar`.
const CAPTURE_CONTENT_ENV_VAR: &str = "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT";
/// Go `adkTelemetrySchemaVersionOptIn`: every case is recorded under each value.
const ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN: &str = "ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN";

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/internal_telemetry_functionaltest_telemetry_schema/testdata")
}

/// Go `goldenPath`.
fn golden_path(c: &Case) -> PathBuf {
    testdata().join(format!("{}.json", c.id()))
}

/// Go `record`: runs `c` under its telemetry configuration and returns its digest as JSON (Go
/// round-trips the digest through JSON so it holds the types a golden read back does).
fn record(c: &Case) -> Value {
    let _lock = ENV_LOCK.lock();
    gadk_engine::telemetry::set_env_provider(Arc::new(MapEnv(
        [
            (CAPTURE_CONTENT_ENV_VAR.to_owned(), c.capture.to_owned()),
            (
                ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN.to_owned(),
                c.schema_version.to_owned(),
            ),
        ]
        .into(),
    )));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (res, spans, logs) = rt.block_on(capture_logs(scenarios::run(c.scenario)));
    gadk_engine::telemetry::set_env_provider(Arc::new(MapEnv::default()));
    assert_eq!(
        res.is_err(),
        c.scenario.want_err(),
        "run error = {res:?}, want error: {}",
        c.scenario.want_err()
    );
    let d = build_digest(&spans, &logs).unwrap_or_else(|e| panic!("build digest: {e}"));
    serde_json::to_value(&d).expect("marshal digest")
}

/// Go `readGolden`.
fn read_golden(path: &Path) -> Value {
    let b =
        std::fs::read(path).unwrap_or_else(|e| panic!("missing golden {}: {e}", path.display()));
    serde_json::from_slice(&b).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

// parity: adapted: Go's -update flag (re-recording the goldens) is not ported, the goldens are ADK-Go's copied verbatim; every Go subtest is a case named after its golden
#[rstest]
#[case::toolcall_no_content_otel_semconv_1_36("toolcall/no_content-otel_semconv_1_36")]
#[case::toolcall_no_content_otel_semconv_1_44("toolcall/no_content-otel_semconv_1_44")]
#[case::toolcall_span_only_otel_semconv_1_36("toolcall/span_only-otel_semconv_1_36")]
#[case::toolcall_span_only_otel_semconv_1_44("toolcall/span_only-otel_semconv_1_44")]
#[case::toolcall_event_only_otel_semconv_1_36("toolcall/event_only-otel_semconv_1_36")]
#[case::toolcall_event_only_otel_semconv_1_44("toolcall/event_only-otel_semconv_1_44")]
#[case::toolcall_span_and_event_otel_semconv_1_36("toolcall/span_and_event-otel_semconv_1_36")]
#[case::toolcall_span_and_event_otel_semconv_1_44("toolcall/span_and_event-otel_semconv_1_44")]
#[case::toolcall_inference_error_no_content_otel_semconv_1_36(
    "toolcall/inference-error-no_content-otel_semconv_1_36"
)]
#[case::toolcall_inference_error_no_content_otel_semconv_1_44(
    "toolcall/inference-error-no_content-otel_semconv_1_44"
)]
#[case::toolcall_inference_error_span_only_otel_semconv_1_36(
    "toolcall/inference-error-span_only-otel_semconv_1_36"
)]
#[case::toolcall_inference_error_span_only_otel_semconv_1_44(
    "toolcall/inference-error-span_only-otel_semconv_1_44"
)]
#[case::toolcall_inference_error_event_only_otel_semconv_1_36(
    "toolcall/inference-error-event_only-otel_semconv_1_36"
)]
#[case::toolcall_inference_error_event_only_otel_semconv_1_44(
    "toolcall/inference-error-event_only-otel_semconv_1_44"
)]
#[case::toolcall_inference_error_span_and_event_otel_semconv_1_36(
    "toolcall/inference-error-span_and_event-otel_semconv_1_36"
)]
#[case::toolcall_inference_error_span_and_event_otel_semconv_1_44(
    "toolcall/inference-error-span_and_event-otel_semconv_1_44"
)]
#[case::toolcall_tool_error_no_content_otel_semconv_1_36(
    "toolcall/tool-error-no_content-otel_semconv_1_36"
)]
#[case::toolcall_tool_error_no_content_otel_semconv_1_44(
    "toolcall/tool-error-no_content-otel_semconv_1_44"
)]
#[case::toolcall_tool_error_span_only_otel_semconv_1_36(
    "toolcall/tool-error-span_only-otel_semconv_1_36"
)]
#[case::toolcall_tool_error_span_only_otel_semconv_1_44(
    "toolcall/tool-error-span_only-otel_semconv_1_44"
)]
#[case::toolcall_tool_error_event_only_otel_semconv_1_36(
    "toolcall/tool-error-event_only-otel_semconv_1_36"
)]
#[case::toolcall_tool_error_event_only_otel_semconv_1_44(
    "toolcall/tool-error-event_only-otel_semconv_1_44"
)]
#[case::toolcall_tool_error_span_and_event_otel_semconv_1_36(
    "toolcall/tool-error-span_and_event-otel_semconv_1_36"
)]
#[case::toolcall_tool_error_span_and_event_otel_semconv_1_44(
    "toolcall/tool-error-span_and_event-otel_semconv_1_44"
)]
#[case::streaming_no_content_otel_semconv_1_36("streaming/no_content-otel_semconv_1_36")]
#[case::streaming_no_content_otel_semconv_1_44("streaming/no_content-otel_semconv_1_44")]
#[case::streaming_span_only_otel_semconv_1_36("streaming/span_only-otel_semconv_1_36")]
#[case::streaming_span_only_otel_semconv_1_44("streaming/span_only-otel_semconv_1_44")]
#[case::streaming_event_only_otel_semconv_1_36("streaming/event_only-otel_semconv_1_36")]
#[case::streaming_event_only_otel_semconv_1_44("streaming/event_only-otel_semconv_1_44")]
#[case::streaming_span_and_event_otel_semconv_1_36("streaming/span_and_event-otel_semconv_1_36")]
#[case::streaming_span_and_event_otel_semconv_1_44("streaming/span_and_event-otel_semconv_1_44")]
#[case::delegation_no_content_otel_semconv_1_36("delegation/no_content-otel_semconv_1_36")]
#[case::delegation_no_content_otel_semconv_1_44("delegation/no_content-otel_semconv_1_44")]
#[case::delegation_span_only_otel_semconv_1_36("delegation/span_only-otel_semconv_1_36")]
#[case::delegation_span_only_otel_semconv_1_44("delegation/span_only-otel_semconv_1_44")]
#[case::delegation_event_only_otel_semconv_1_36("delegation/event_only-otel_semconv_1_36")]
#[case::delegation_event_only_otel_semconv_1_44("delegation/event_only-otel_semconv_1_44")]
#[case::delegation_span_and_event_otel_semconv_1_36("delegation/span_and_event-otel_semconv_1_36")]
#[case::delegation_span_and_event_otel_semconv_1_44("delegation/span_and_event-otel_semconv_1_44")]
#[case::dynamicworkflow_no_content_otel_semconv_1_36(
    "dynamicworkflow/no_content-otel_semconv_1_36"
)]
#[case::dynamicworkflow_no_content_otel_semconv_1_44(
    "dynamicworkflow/no_content-otel_semconv_1_44"
)]
#[case::dynamicworkflow_span_only_otel_semconv_1_36("dynamicworkflow/span_only-otel_semconv_1_36")]
#[case::dynamicworkflow_span_only_otel_semconv_1_44("dynamicworkflow/span_only-otel_semconv_1_44")]
#[case::dynamicworkflow_event_only_otel_semconv_1_36(
    "dynamicworkflow/event_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_event_only_otel_semconv_1_44(
    "dynamicworkflow/event_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_span_and_event_otel_semconv_1_36(
    "dynamicworkflow/span_and_event-otel_semconv_1_36"
)]
#[case::dynamicworkflow_span_and_event_otel_semconv_1_44(
    "dynamicworkflow/span_and_event-otel_semconv_1_44"
)]
#[case::dynamicworkflow_static_node_error_no_content_otel_semconv_1_36(
    "dynamicworkflow/static-node-error-no_content-otel_semconv_1_36"
)]
#[case::dynamicworkflow_static_node_error_no_content_otel_semconv_1_44(
    "dynamicworkflow/static-node-error-no_content-otel_semconv_1_44"
)]
#[case::dynamicworkflow_static_node_error_span_only_otel_semconv_1_36(
    "dynamicworkflow/static-node-error-span_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_static_node_error_span_only_otel_semconv_1_44(
    "dynamicworkflow/static-node-error-span_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_static_node_error_event_only_otel_semconv_1_36(
    "dynamicworkflow/static-node-error-event_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_static_node_error_event_only_otel_semconv_1_44(
    "dynamicworkflow/static-node-error-event_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_static_node_error_span_and_event_otel_semconv_1_36(
    "dynamicworkflow/static-node-error-span_and_event-otel_semconv_1_36"
)]
#[case::dynamicworkflow_static_node_error_span_and_event_otel_semconv_1_44(
    "dynamicworkflow/static-node-error-span_and_event-otel_semconv_1_44"
)]
#[case::dynamicworkflow_dynamic_node_error_no_content_otel_semconv_1_36(
    "dynamicworkflow/dynamic-node-error-no_content-otel_semconv_1_36"
)]
#[case::dynamicworkflow_dynamic_node_error_no_content_otel_semconv_1_44(
    "dynamicworkflow/dynamic-node-error-no_content-otel_semconv_1_44"
)]
#[case::dynamicworkflow_dynamic_node_error_span_only_otel_semconv_1_36(
    "dynamicworkflow/dynamic-node-error-span_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_dynamic_node_error_span_only_otel_semconv_1_44(
    "dynamicworkflow/dynamic-node-error-span_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_dynamic_node_error_event_only_otel_semconv_1_36(
    "dynamicworkflow/dynamic-node-error-event_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_dynamic_node_error_event_only_otel_semconv_1_44(
    "dynamicworkflow/dynamic-node-error-event_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_dynamic_node_error_span_and_event_otel_semconv_1_36(
    "dynamicworkflow/dynamic-node-error-span_and_event-otel_semconv_1_36"
)]
#[case::dynamicworkflow_dynamic_node_error_span_and_event_otel_semconv_1_44(
    "dynamicworkflow/dynamic-node-error-span_and_event-otel_semconv_1_44"
)]
#[case::dynamicworkflow_first_agent_error_no_content_otel_semconv_1_36(
    "dynamicworkflow/first-agent-error-no_content-otel_semconv_1_36"
)]
#[case::dynamicworkflow_first_agent_error_no_content_otel_semconv_1_44(
    "dynamicworkflow/first-agent-error-no_content-otel_semconv_1_44"
)]
#[case::dynamicworkflow_first_agent_error_span_only_otel_semconv_1_36(
    "dynamicworkflow/first-agent-error-span_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_first_agent_error_span_only_otel_semconv_1_44(
    "dynamicworkflow/first-agent-error-span_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_first_agent_error_event_only_otel_semconv_1_36(
    "dynamicworkflow/first-agent-error-event_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_first_agent_error_event_only_otel_semconv_1_44(
    "dynamicworkflow/first-agent-error-event_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_first_agent_error_span_and_event_otel_semconv_1_36(
    "dynamicworkflow/first-agent-error-span_and_event-otel_semconv_1_36"
)]
#[case::dynamicworkflow_first_agent_error_span_and_event_otel_semconv_1_44(
    "dynamicworkflow/first-agent-error-span_and_event-otel_semconv_1_44"
)]
#[case::dynamicworkflow_second_agent_error_no_content_otel_semconv_1_36(
    "dynamicworkflow/second-agent-error-no_content-otel_semconv_1_36"
)]
#[case::dynamicworkflow_second_agent_error_no_content_otel_semconv_1_44(
    "dynamicworkflow/second-agent-error-no_content-otel_semconv_1_44"
)]
#[case::dynamicworkflow_second_agent_error_span_only_otel_semconv_1_36(
    "dynamicworkflow/second-agent-error-span_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_second_agent_error_span_only_otel_semconv_1_44(
    "dynamicworkflow/second-agent-error-span_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_second_agent_error_event_only_otel_semconv_1_36(
    "dynamicworkflow/second-agent-error-event_only-otel_semconv_1_36"
)]
#[case::dynamicworkflow_second_agent_error_event_only_otel_semconv_1_44(
    "dynamicworkflow/second-agent-error-event_only-otel_semconv_1_44"
)]
#[case::dynamicworkflow_second_agent_error_span_and_event_otel_semconv_1_36(
    "dynamicworkflow/second-agent-error-span_and_event-otel_semconv_1_36"
)]
#[case::dynamicworkflow_second_agent_error_span_and_event_otel_semconv_1_44(
    "dynamicworkflow/second-agent-error-span_and_event-otel_semconv_1_44"
)]
fn test_telemetry_schema(#[case] id: &str) {
    let c = cases()
        .into_iter()
        .find(|c| c.id() == id)
        .unwrap_or_else(|| panic!("no case {id}"));
    let got = record(&c);
    let want = read_golden(&golden_path(&c));
    assert_eq!(
        serde_json::to_string_pretty(&want).unwrap(),
        serde_json::to_string_pretty(&got).unwrap(),
        "telemetry differs from {}",
        golden_path(&c).display()
    );
}

/// Fails on a golden no case records any more, so a renamed or dropped case cannot leave a stale
/// file behind.
#[test]
fn test_goldens_have_cases() {
    let want: BTreeSet<PathBuf> = cases().iter().map(golden_path).collect();
    for dir in std::fs::read_dir(testdata()).unwrap() {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            if f.extension().is_some_and(|e| e == "json") {
                assert!(
                    want.contains(&f),
                    "{} is recorded by no case; delete it",
                    f.display()
                );
            }
        }
    }
}
