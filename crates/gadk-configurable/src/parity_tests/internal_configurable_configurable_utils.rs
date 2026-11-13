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

//! Port of ADK-Go `internal/configurable/configurable_utils_test.go` (Task 3).

use std::path::{Path, PathBuf};

use crate::tests::{CfgErr, relative_to_cwd, temp_dir, write};
use crate::{GoMap, GoValue, Registry};

const TRAVERSAL_ERROR: &str = "path traversal detected";

/// Go `newAgentDir`: an agent directory with a sibling config, a file outside the directory,
/// and a symlink inside the directory pointing at that outside file. Returns the temp dir
/// guard, the base directory and the parent agent config path.
fn new_agent_dir() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (guard, base) = temp_dir();
    let agent_dir = base.join("agents").join("root");
    let parent_path = write(&agent_dir, "root_agent.yaml", "agent_class: LlmAgent\n");
    write(&agent_dir, "sub_agent.yaml", "agent_class: LlmAgent\n");
    let outside = write(&base, "outside.yaml", "agent_class: LlmAgent\n");
    std::os::unix::fs::symlink(&outside, agent_dir.join("link.yaml")).unwrap();
    (guard, base, parent_path)
}

// parity: adapted: Go's package-global registry is a `Registry` value with Go init()'s agent classes (`Registry::with_agent_classes`; the built-in tools are not needed here)
#[tokio::test]
async fn test_resolve_agent_reference_rejects_escaping_config_path() {
    let (_guard, _base, parent_path) = new_agent_dir();
    let registry = Registry::with_agent_classes();

    let tests = [
        (
            "absolute path",
            "/etc/passwd",
            "absolute paths are not allowed",
        ),
        ("parent traversal", "../../outside.yaml", TRAVERSAL_ERROR),
        (
            "symlink escaping the agent directory",
            "link.yaml",
            TRAVERSAL_ERROR,
        ),
    ];

    for (name, ref_path, want_err) in tests {
        // The reference must be rejected by the containment check itself, not merely fail
        // later while the config is being loaded.
        let err = registry
            .resolve_agent_reference(&parent_path, ref_path)
            .await
            .cfg_err()
            .to_string();
        assert!(
            err.contains(want_err),
            "{name}: ResolveAgentReference(_, {parent_path:?}, {ref_path:?}) = {err}, want error containing {want_err:?}"
        );
    }
}

// parity: adapted: Go's package-global registry is a `Registry` value with Go init()'s agent classes
#[tokio::test]
async fn test_resolve_agent_reference_allows_paths_inside_agent_dir() {
    let (_guard, _base, parent_path) = new_agent_dir();
    let registry = Registry::with_agent_classes();

    if let Err(err) = registry
        .resolve_agent_reference(&parent_path, "sub_agent.yaml")
        .await
    {
        assert!(
            !err.to_string().contains(TRAVERSAL_ERROR),
            "ResolveAgentReference(_, {parent_path:?}, \"sub_agent.yaml\") = {err}, want no traversal rejection"
        );
    }
}

#[tokio::test]
async fn test_resolve_tool_reference_mcp_toolset_non_string_args() {
    fn new_args(server_args: Vec<GoValue>, tool_filter: Vec<GoValue>) -> GoMap {
        let server_params = GoValue::Map(vec![
            ("command".to_owned(), GoValue::String("echo".into())),
            ("args".to_owned(), GoValue::Seq(server_args)),
        ]);
        let stdio = GoValue::Map(vec![("server_params".to_owned(), server_params)]);
        [
            ("stdio_connection_params".to_owned(), stdio),
            ("tool_filter".to_owned(), GoValue::Seq(tool_filter)),
        ]
        .into_iter()
        .collect()
    }
    let s = |v: &str| GoValue::String(v.into());

    let tests = [
        (
            "non-string in server args",
            new_args(vec![s("a"), GoValue::Int(1), GoValue::Int(2)], vec![s("a")]),
            "server_params.args[1]",
        ),
        (
            "non-string in tool filter",
            new_args(vec![s("a")], vec![GoValue::Bool(true)]),
            "tool_filter[0]",
        ),
        (
            "all strings is valid",
            new_args(vec![s("a"), s("b")], vec![s("t1")]),
            "",
        ),
    ];

    let registry = Registry::new();
    for (name, args, want_err) in tests {
        // A non-string element previously triggered a panic; the call must return normally
        // (with or without an error), never crash.
        let result = registry
            .resolve_tool_reference(&Default::default(), "McpToolset", Some(&args))
            .await;
        if want_err.is_empty() {
            match result {
                Ok((_, toolset)) => assert!(
                    toolset.is_some(),
                    "{name}: ResolveToolReference(McpToolset) returned a nil toolset, want non-nil"
                ),
                Err(err) => {
                    panic!("{name}: ResolveToolReference(McpToolset) = {err}, want no error")
                }
            }
            continue;
        }
        let err = result.cfg_err().to_string();
        assert!(
            err.contains(want_err),
            "{name}: ResolveToolReference(McpToolset) = {err}, want error containing {want_err:?}"
        );
    }
}

// parity: adapted: no os.Chdir (the working directory is process-wide and would race the parallel tests); the relative parent path is spelled relative to the current working directory instead
#[tokio::test]
async fn test_resolve_agent_reference_relative_parent_path() {
    let (_guard, _base, parent_path) = new_agent_dir();
    let registry = Registry::with_agent_classes();
    let relative_parent = relative_to_cwd(&parent_path);
    let relative_parent = Path::new(&relative_parent);

    if let Err(err) = registry
        .resolve_agent_reference(relative_parent, "sub_agent.yaml")
        .await
    {
        assert!(
            !err.to_string().contains(TRAVERSAL_ERROR),
            "ResolveAgentReference with a relative parent path = {err}, want no traversal rejection"
        );
    }

    assert!(
        registry
            .resolve_agent_reference(relative_parent, "../../outside.yaml")
            .await
            .is_err(),
        "ResolveAgentReference with a relative parent path and an escaping reference succeeded, want an error"
    );
}
