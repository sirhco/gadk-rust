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

//! Ports of ADK-Go `agent/loader_test.go`, plus the loaders' Go error texts.

use std::sync::Arc;

use crate::{Agent, AgentConfig, Loader, LoaderError, MultiLoader, SingleLoader, new_agent};

/// One `TestDuplicateName` case: name, root, further agents, whether an error is wanted.
type Case = (&'static str, Arc<dyn Agent>, Vec<Arc<dyn Agent>>, bool);

/// Go `testAgent`: an agent with only a name.
fn test_agent(name: &str) -> Arc<dyn Agent> {
    new_agent(AgentConfig {
        name: name.into(),
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn test_duplicate_name() {
    let agent1 = test_agent("weather_time_agent");
    // duplicate name
    let agent2 = test_agent("weather_time_agent");
    let agent3 = test_agent("unique");

    let tests: Vec<Case> = vec![
        ("root only", agent1.clone(), vec![], false),
        (
            "root duplicate object",
            agent1.clone(),
            vec![agent1.clone()],
            true,
        ),
        (
            "root duplicate name",
            agent1.clone(),
            vec![agent2.clone()],
            true,
        ),
        (
            "non-root duplicate name",
            agent3.clone(),
            vec![agent1.clone(), agent2.clone()],
            true,
        ),
        (
            "non-root duplicate object",
            agent3.clone(),
            vec![agent1.clone(), agent1.clone()],
            true,
        ),
        ("no duplicates", agent1.clone(), vec![agent3.clone()], false),
    ];
    for (name, root, agents, want_err) in tests {
        let err = MultiLoader::new(root, agents).err();
        assert_eq!(
            err.is_some(),
            want_err,
            "NewMultiLoader() name={name}, error = {err:?}, wantErr {want_err}"
        );
    }
}

#[test]
fn test_multi_loader_list_agents_sorted() {
    let names = [
        "transfer_agent",
        "sequential_agent",
        "hello_agent",
        "tools_agent",
        "agent_tool_agent",
        "loop_agent",
        "parallel_agent",
        "remote_agent",
        "template_agent",
    ];
    let root = test_agent("root_agent");
    let agents = names.iter().map(|n| test_agent(n)).collect();
    let loader = MultiLoader::new(root, agents).unwrap();

    let mut want: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    want.push("root_agent".into());
    want.sort();
    // Go repeats the call because map iteration order is randomized; Rust's order is fixed, so
    // one call decides.
    assert_eq!(loader.list_agents(), want, "ListAgents()");
}

#[test]
fn multi_loader_errors_carry_go_text_and_sorted_names() {
    let err = MultiLoader::new(test_agent("root"), vec![test_agent("b"), test_agent("b")]).err();
    assert_eq!(err, Some(LoaderError("duplicate agent name: b".into())));

    let loader =
        MultiLoader::new(test_agent("root"), vec![test_agent("b"), test_agent("a")]).unwrap();
    assert_eq!(loader.list_agents(), ["a", "b", "root"]);
    assert_eq!(loader.load_agent("a").unwrap().name(), "a");
    assert_eq!(loader.root_agent().name(), "root");
    assert_eq!(
        loader.load_agent("zzz").err().unwrap().to_string(),
        "agent zzz not found. Please specify one of those: [a b root]"
    );
}

#[test]
fn single_loader_serves_the_root_under_its_name_and_the_empty_name() {
    let loader = SingleLoader::new(test_agent("root"));
    assert_eq!(loader.list_agents(), ["root"]);
    assert_eq!(loader.load_agent("").unwrap().name(), "root");
    assert_eq!(loader.load_agent("root").unwrap().name(), "root");
    assert_eq!(loader.root_agent().name(), "root");
    assert_eq!(
        loader.load_agent("other").err().unwrap().to_string(),
        "cannot load agent 'other' - provide an empty string or use 'root'"
    );
}
