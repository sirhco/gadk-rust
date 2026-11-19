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

//! Ports of ADK-Go `internal/compactionvalidate/validate_test.go`.

use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, AgentError, Loader, LoaderError, SingleLoader};
use gadk_session::InMemorySessionService;
use gadk_session::compaction::Config;

use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::compactionvalidate::{against_agents, against_root_agent};
use crate::llm::{self, LlmAgentConfig};
use crate::runner::{PluginConfig, RunnerConfig};

/// Go `runner.Config{SessionService: session.InMemoryService()}`: a `RunnerConfig` needs an
/// agent too, which the validation replaces per application.
fn base_config() -> RunnerConfig {
    RunnerConfig {
        app_name: String::new(),
        agent: gadk_agent::new_agent(AgentConfig {
            name: "placeholder".into(),
            ..Default::default()
        })
        .unwrap(),
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Default::default(),
        compaction: None,
    }
}

#[test]
fn test_against_agents_refuses_a_config_no_agent_can_serve() {
    let leaf = llm::agent::new(LlmAgentConfig {
        name: "leaf".into(),
        ..Default::default()
    })
    .unwrap();
    let wf = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "wfapp".into(),
            sub_agents: vec![leaf],
            ..Default::default()
        },
    })
    .unwrap();

    let cfg = Config {
        compaction_interval: 2,
        ..Default::default()
    };
    let base = base_config();
    let loader = SingleLoader::new(wf);

    match against_agents(Some(&cfg), Some(&loader), &base) {
        Ok(()) => panic!(
            "AgainstAgents() = nil, want a refusal: no Summarizer and no model to default to"
        ),
        Err(e) => assert!(
            e.to_string().contains("wfapp"),
            "error {e:?} does not name the app that cannot be served"
        ),
    }
    // The Go text, with the cause wrapped as `%w`.
    let err = against_agents(Some(&cfg), Some(&loader), &base).unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Compaction config cannot serve app \"wfapp\": the Compaction config needs a \
         Summarizer: root agent \"wfapp\" is not an LLM agent, so no default model is available"
    );
    let AgentError::Other(wrapped) = &err else {
        panic!("want AgentError::Other(GoWrapped), got {err:?}")
    };
    assert!(
        wrapped.source().is_some(),
        "the cause is kept as the source (Go %w)"
    );

    // A nil config is not a problem, and neither is a nil loader.
    if let Err(e) = against_agents(None, Some(&loader), &base) {
        panic!("AgainstAgents(nil) = {e}, want nil");
    }
    if let Err(e) = against_agents(Some(&cfg), None, &base) {
        panic!("AgainstAgents(nil loader) = {e}, want nil");
    }
}

/// Go `hostileLoader`: panics if anything asks it to enumerate applications.
struct HostileLoader;

impl Loader for HostileLoader {
    fn list_agents(&self) -> Vec<String> {
        panic!("ListAgents called while compaction is disabled")
    }
    fn load_agent(&self, _name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        unimplemented!()
    }
    fn root_agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
}

#[test]
fn test_validation_does_not_touch_the_loader_when_compaction_is_off() {
    if let Err(e) = against_agents(None, Some(&HostileLoader), &base_config()) {
        panic!("AgainstAgents(nil config) = {e}, want nil");
    }
    if let Err(e) = against_root_agent(None, None, &base_config()) {
        panic!("AgainstRootAgent(nil config) = {e}, want nil");
    }
}

#[test]
fn invalid_shape_is_wrapped_as_invalid_compaction() {
    let cfg = Config {
        compaction_interval: -1,
        ..Default::default()
    };
    let err: AgentError = against_agents(Some(&cfg), None, &base_config()).unwrap_err();
    let shape = cfg.validate().unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("invalid compaction config: {shape}")
    );
}

#[test]
fn against_root_agent_checks_only_the_root() {
    let cfg = Config {
        compaction_interval: 2,
        ..Default::default()
    };
    let wf = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "root_wf".into(),
            ..Default::default()
        },
    })
    .unwrap();
    let err = against_root_agent(Some(&cfg), Some(wf), &base_config()).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("the Compaction config cannot serve app \"root_wf\": ")
    );
    assert!(against_root_agent(Some(&cfg), None, &base_config()).is_ok());
}
