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

//! Ports of ADK-Go `internal/telemetry/experimental_semconv_test.go`.

use gadk_genai::{Content, Part};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::telemetry::converters::from_log_value;
use crate::telemetry::env::{
    CAPTURE_MESSAGE_CONTENT_ENV_VAR, SCHEMA_VERSION_OPT_IN_ENV_VAR, test_env,
};
use crate::telemetry::semconv::OperationDetails;
use crate::telemetry::span_digest::capture;

// parity: adapted: Go compares the address of the first encoded attribute to see that an unchanged request is not encoded again; Rust compares the attribute vector's buffer address
#[tokio::test]
async fn test_operation_details_note_llm_request() {
    let _env = test_env(&[
        (SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44"),
        (CAPTURE_MESSAGE_CONTENT_ENV_VAR, "SPAN_AND_EVENT"),
    ]);
    let (event_request, spans, _) = capture(async {
        let span = tracing::info_span!(
            "generate_content",
            gen_ai.input.messages = tracing::field::Empty
        );
        let mut req = LlmRequest {
            contents: vec![Content {
                role: "user".into(),
                parts: vec![Part {
                    text: "original".into(),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        };
        let mut d = OperationDetails::new(span.clone());
        d.note_llm_request(&req);
        let first = d.event_request.as_ptr();

        d.note_llm_request(&req);
        assert!(
            std::ptr::eq(d.event_request.as_ptr(), first),
            "noting an unchanged request encoded its messages again"
        );

        req.contents[0].parts[0].text = "edited by the model".into();
        d.note_llm_request(&req);
        d.event_request
            .iter()
            .map(|(k, v)| ((*k).to_owned(), from_log_value(v)))
            .collect::<Vec<(String, Value)>>()
    })
    .await;
    let want = vec![(
        "gen_ai.input.messages".to_owned(),
        json!([{"role": "user", "parts": [{"type": "text", "content": "edited by the model"}]}]),
    )];
    assert_eq!(event_request, want, "event messages after an in-place edit");
    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].attributes, want,
        "span messages after an in-place edit"
    );
}
