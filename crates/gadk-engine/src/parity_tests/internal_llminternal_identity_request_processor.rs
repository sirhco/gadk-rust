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

//! Parity port of Go internal/llminternal/identity_request_processor_test.go.

use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, InvocationContext, Mode, agent};
use gadk_genai::{Content, GenerateContentConfig};
use gadk_model::LlmRequest;

use crate::{
    llm::{
        config::LlmAgentConfig,
        flow::Flow,
        processors::{identity::identity_request_processor, test_llm_agent},
    },
    tool::utils::text_parts,
};

struct Case {
    name: &'static str,
    agent: Arc<dyn Agent>,
    existing_si: Option<Content>,
    want_contains: Vec<&'static str>,
    want_not_contain: Vec<&'static str>,
    want_no_si: bool,
}

fn llm(name: &str, description: &str, mode: Mode) -> Arc<dyn Agent> {
    test_llm_agent(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        mode,
        ..Default::default()
    })
}

#[tokio::test]
async fn test_identity_request_processor() {
    let cases = vec![
        Case {
            name: "InjectsNameOnly",
            agent: llm("test_agent", "", Mode::Unset),
            existing_si: None,
            want_contains: vec!["You are an agent.", "Your internal name is \"test_agent\"."],
            want_not_contain: vec!["The description about you is"],
            want_no_si: false,
        },
        Case {
            name: "InjectsNameAndDescription",
            agent: llm(
                "helper_agent",
                "A helpful assistant that answers questions",
                Mode::Unset,
            ),
            existing_si: None,
            want_contains: vec![
                "You are an agent.",
                "Your internal name is \"helper_agent\".",
                "The description about you is \"A helpful assistant that answers questions\".",
            ],
            want_not_contain: vec![],
            want_no_si: false,
        },
        Case {
            name: "NoOpForNonLLMAgent",
            agent: agent::new(AgentConfig {
                name: "plain_agent".into(),
                ..Default::default()
            })
            .unwrap(),
            existing_si: None,
            want_contains: vec![],
            want_not_contain: vec![],
            want_no_si: true,
        },
        Case {
            name: "NoOpForSingleTurnMode",
            agent: llm("single_turn", "", Mode::SingleTurn),
            existing_si: None,
            want_contains: vec![],
            want_not_contain: vec![],
            want_no_si: true,
        },
        Case {
            name: "EmptyDescription",
            agent: llm("empty_desc_agent", "", Mode::Unset),
            existing_si: None,
            want_contains: vec![
                "You are an agent.",
                "Your internal name is \"empty_desc_agent\".",
            ],
            want_not_contain: vec!["The description about you is"],
            want_no_si: false,
        },
        Case {
            name: "AppendsToExistingInstructions",
            agent: llm("append_agent", "", Mode::Unset),
            existing_si: Some(Content::from_text("Be concise.", "user")),
            want_contains: vec![
                "Be concise.",
                "You are an agent.",
                "Your internal name is \"append_agent\".",
            ],
            want_not_contain: vec![],
            want_no_si: false,
        },
        Case {
            name: "SkipsForSingleTurnMode",
            agent: llm(
                "single_turn_agent",
                "A single-shot worker.",
                Mode::SingleTurn,
            ),
            existing_si: None,
            want_contains: vec![],
            want_not_contain: vec![],
            want_no_si: true,
        },
        Case {
            name: "EmittedForChatMode",
            agent: llm("chat_agent", "", Mode::Chat),
            existing_si: None,
            want_contains: vec!["You are an agent.", "Your internal name is \"chat_agent\"."],
            want_not_contain: vec![],
            want_no_si: false,
        },
        Case {
            name: "EmittedForTaskMode",
            agent: llm("task_agent", "", Mode::Task),
            existing_si: None,
            want_contains: vec!["You are an agent.", "Your internal name is \"task_agent\"."],
            want_not_contain: vec![],
            want_no_si: false,
        },
    ];
    for tc in cases {
        let mut req = LlmRequest::default();
        if let Some(si) = tc.existing_si {
            req.config = Some(GenerateContentConfig {
                system_instruction: Some(si),
                ..Default::default()
            });
        }
        let ctx = InvocationContext::builder()
            .agent(tc.agent)
            .build_invocation();
        let events = identity_request_processor(&ctx, &mut req, &Flow::default())
            .await
            .unwrap();
        assert!(
            events.is_empty(),
            "{}: yielded {} events, want 0",
            tc.name,
            events.len()
        );
        let si = req
            .config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref());
        if tc.want_no_si {
            assert!(si.is_none(), "{}: set SystemInstruction: {si:?}", tc.name);
            continue;
        }
        let si = text_parts(si).join(" ");
        for w in tc.want_contains {
            assert!(si.contains(w), "{}: SI {si:?} lacks {w:?}", tc.name);
        }
        for w in tc.want_not_contain {
            assert!(!si.contains(w), "{}: SI {si:?} contains {w:?}", tc.name);
        }
    }
}
