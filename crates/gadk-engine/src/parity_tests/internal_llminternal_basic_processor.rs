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

//! Parity port of Go internal/llminternal/basic_processor_test.go.

use std::collections::BTreeMap;

use gadk_agent::{InvocationContext, Mode};
use gadk_genai::{Schema, Type};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use rstest::rstest;

use crate::llm::{
    config::LlmAgentConfig,
    flow::Flow,
    processors::{TestLlm, basic::basic_request_processor, test_llm_agent},
};

#[rstest]
#[case::task_mode_skips_output_schema(Mode::Task, false)]
#[case::single_turn_mode_sets_output_schema(Mode::SingleTurn, true)]
#[case::chat_mode_sets_output_schema(Mode::Chat, true)]
#[tokio::test]
async fn test_basic_request_processor_output_schema_per_mode(
    #[case] mode: Mode,
    #[case] want_schema_set: bool,
) {
    let schema = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([(
            "answer".to_owned(),
            Schema {
                r#type: Some(Type::String),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let agent = test_llm_agent(LlmAgentConfig {
        name: format!("agent_{}", mode.as_str()),
        mode,
        model: Some(TestLlm::named("m")),
        output_schema: Some(schema.clone()),
        ..Default::default()
    });
    let ctx = InvocationContext::builder().agent(agent).build_invocation();
    let mut req = LlmRequest::default();
    let events = basic_request_processor(&ctx, &mut req, &Flow::default())
        .await
        .unwrap();
    assert!(
        events.is_empty(),
        "basic_request_processor unexpectedly yielded events: {events:?}"
    );
    let cfg = req.config.expect("req.config is None; want Some");
    let (want_schema, want_mime) = if want_schema_set {
        (Some(schema), "application/json")
    } else {
        (None, "")
    };
    assert_eq!(cfg.response_schema, want_schema);
    assert_eq!(cfg.response_mime_type, want_mime);
}
