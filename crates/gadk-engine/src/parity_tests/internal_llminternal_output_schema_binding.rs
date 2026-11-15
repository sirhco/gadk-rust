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

//! Port of Go internal/llminternal/output_schema_binding_test.go (be529e7b, #2194).

use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, InvocationContext, Mode, ParentMap};
use gadk_genai::{GenerateContentConfig, Schema, Type};
use serde_json::json;

use crate::llm::mode::with_bound_mode;
use crate::llm::output_schema_binding::{output_schema_for, with_bound_output_schema};
use crate::llm::state::reveal;
use crate::llm::{LlmAgentConfig, agent as llm_agent};

fn object(prop: &str) -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: [(
            prop.to_owned(),
            Schema {
                r#type: Some(Type::String),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

fn llm(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).expect("llmagent.New")
}

/// Go `icontext.NewInvocationContext(WithBoundOutputSchema(WithBoundMode(…), …), {Agent: a})`,
/// with an optional parent map.
fn bound_ctx(
    a: &Arc<dyn Agent>,
    mode: Mode,
    bind_to: &Arc<dyn Agent>,
    bind_name: &str,
    schema: Option<Schema>,
    parents: Option<Arc<ParentMap>>,
) -> InvocationContext {
    let mut b = InvocationContext::builder().agent(Arc::clone(a));
    if let Some(p) = parents {
        b = b.parent_map(p);
    }
    let ctx = with_bound_mode(&b.build_invocation(), a.name(), a.agent_state(), mode);
    with_bound_output_schema(&ctx, bind_name, bind_to.agent_state(), schema)
}

/// Which agent a case's binding names.
enum BindTo {
    /// The agent itself.
    Itself,
    /// Another agent with the same name (another state).
    SameNamedAgent,
    /// The agent's state under another name.
    OtherName(&'static str),
}

struct Case {
    name: &'static str,
    cfg: LlmAgentConfig,
    with_sub_agent: bool,
    under_parent: bool,
    bind_to: BindTo,
    mode: Mode,
    want: Option<Schema>,
}

fn case(name: &'static str, cfg: LlmAgentConfig, want: Option<Schema>) -> Case {
    Case {
        name,
        cfg,
        with_sub_agent: false,
        under_parent: false,
        bind_to: BindTo::Itself,
        mode: Mode::SingleTurn,
        want,
    }
}

#[tokio::test]
async fn test_output_schema_for() {
    let bound = object("a");
    let declared = object("b");
    let cases = vec![
        case(
            "bound schema",
            LlmAgentConfig::default(),
            Some(bound.clone()),
        ),
        Case {
            mode: Mode::Chat,
            ..case(
                "chat mode bound for this agent",
                LlmAgentConfig::default(),
                None,
            )
        },
        Case {
            mode: Mode::Task,
            ..case(
                "task mode bound for this agent",
                LlmAgentConfig::default(),
                None,
            )
        },
        case(
            "declared OutputSchema wins",
            LlmAgentConfig {
                output_schema: Some(declared.clone()),
                ..Default::default()
            },
            Some(declared.clone()),
        ),
        case(
            "GenerateContentConfig.ResponseSchema counts as declared",
            LlmAgentConfig {
                generate_content_config: Some(GenerateContentConfig {
                    response_schema: Some(declared.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        ),
        case(
            "GenerateContentConfig.ResponseJsonSchema counts as declared",
            LlmAgentConfig {
                generate_content_config: Some(GenerateContentConfig {
                    response_json_schema: Some(json!({"type": "object"})),
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        ),
        case(
            "GenerateContentConfig.ResponseMIMEType counts as declared",
            LlmAgentConfig {
                generate_content_config: Some(GenerateContentConfig {
                    response_mime_type: "text/plain".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        ),
        Case {
            with_sub_agent: true,
            ..case("agent that can transfer", LlmAgentConfig::default(), None)
        },
        Case {
            under_parent: true,
            ..case(
                "agent that can transfer to its parent",
                LlmAgentConfig::default(),
                None,
            )
        },
        Case {
            under_parent: true,
            ..case(
                "agent under a parent with transfers disallowed",
                LlmAgentConfig {
                    disallow_transfer_to_parent: true,
                    disallow_transfer_to_peers: true,
                    ..Default::default()
                },
                Some(bound.clone()),
            )
        },
        Case {
            bind_to: BindTo::SameNamedAgent,
            ..case(
                "binding for a same-named agent",
                LlmAgentConfig::default(),
                None,
            )
        },
        Case {
            bind_to: BindTo::OtherName("peer"),
            ..case(
                "binding for the same state under another name",
                LlmAgentConfig::default(),
                None,
            )
        },
    ];
    for tc in cases {
        let mut cfg = tc.cfg;
        cfg.name = "self".into();
        if tc.with_sub_agent {
            cfg.sub_agents = vec![
                gadk_agent::new_agent(AgentConfig {
                    name: "sub".into(),
                    ..Default::default()
                })
                .expect("agent.New"),
            ];
        }
        let a = llm(cfg);
        // Go parentmap.ToContext(base, {self: parent}): an LLM agent that lists this one.
        let parents = tc.under_parent.then(|| {
            let parent = llm(LlmAgentConfig {
                name: "parent".into(),
                sub_agents: vec![Arc::clone(&a)],
                ..Default::default()
            });
            Arc::new(ParentMap::new(&parent).expect("parent map"))
        });
        let (bind_to, bind_name) = match tc.bind_to {
            BindTo::Itself => (Arc::clone(&a), "self"),
            BindTo::SameNamedAgent => (
                llm(LlmAgentConfig {
                    name: "self".into(),
                    ..Default::default()
                }),
                "self",
            ),
            BindTo::OtherName(n) => (Arc::clone(&a), n),
        };
        let ctx = bound_ctx(
            &a,
            tc.mode,
            &bind_to,
            bind_name,
            Some(bound.clone()),
            parents,
        );

        let got = output_schema_for(&ctx, reveal(&*a).expect("llm agent"));

        assert_eq!(got, tc.want, "{}: outputSchemaFor mismatch", tc.name);
    }
}

/// Each request gets its own copy of a bound schema, so editing one request's schema does not
/// change what the next request is sent.
#[tokio::test]
async fn test_output_schema_for_copies_bound_schema() {
    let a = llm(LlmAgentConfig {
        name: "self".into(),
        ..Default::default()
    });
    let ctx = bound_ctx(&a, Mode::SingleTurn, &a, "self", Some(object("a")), None);
    let state = reveal(&*a).expect("llm agent");

    let mut first = output_schema_for(&ctx, state).expect("a bound schema");
    if let Some(p) = first.properties.get_mut("a") {
        p.description = "edited".into();
    }
    first.properties.insert(
        "b".into(),
        Schema {
            r#type: Some(Type::String),
            ..Default::default()
        },
    );
    let second = output_schema_for(&ctx, state);

    assert_eq!(
        second,
        Some(object("a")),
        "second request's schema changed after editing the first"
    );
}

/// A nil binding shadows an outer one, as an inner placement with no schema should.
#[tokio::test]
async fn test_with_bound_output_schema_nil_shadows_outer() {
    let a = llm(LlmAgentConfig {
        name: "self".into(),
        ..Default::default()
    });
    let outer = bound_ctx(&a, Mode::SingleTurn, &a, "self", Some(object("a")), None);
    let ctx = with_bound_output_schema(&outer, "self", a.agent_state(), None);

    let got = output_schema_for(&ctx, reveal(&*a).expect("llm agent"));

    assert_eq!(got, None, "outputSchemaFor under a nil inner binding");
}
