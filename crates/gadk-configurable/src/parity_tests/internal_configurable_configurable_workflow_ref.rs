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

//! Port of ADK-Go `internal/configurable/configurable_workflow_ref_test.go`.
//!
//! The tests drive the containment check on workflow edge references through the public
//! `FromConfig`. The referenced node is a JoinNode: it is built entirely inside
//! `resolveNodeFromYAML`, so the second check inside `ResolveAgentReference`, which an
//! agent-class node goes through, cannot refuse the reference on behalf of the workflow check
//! under test. Go's package-global registries are one [`Registry`] per test
//! ([`test_registry`]); Go's sentinels are matched by their text.

use std::path::{Path, PathBuf};

use crate::ConfigError;
use crate::tests::{temp_dir, test_registry, write};

const JOIN_NODE_YAML: &str = "name: join_node\nagent_class: JoinNode\n";

/// Go `utils.ErrNotValidRelativePath`'s text.
const NOT_VALID_RELATIVE_PATH: &str = "path is not a relative path inside the base directory";
/// Go `utils.ErrSymlinkInRelativePath`'s text.
const SYMLINK_IN_RELATIVE_PATH: &str = "path leads outside the base directory through a link";

/// Go `isContainmentRejection` (`configurable_utils_test.go`).
fn is_containment_rejection<T>(result: &Result<T, ConfigError>) -> bool {
    match result {
        Ok(_) => false,
        Err(e) => {
            let err = e.to_string();
            err.contains(NOT_VALID_RELATIVE_PATH) || err.contains(SYMLINK_IN_RELATIVE_PATH)
        }
    }
}

/// Go `writeWorkflow`: a workflow whose only edge leads from START to `node_ref`.
fn write_workflow(dir: &Path, name: &str, node_ref: &str) -> PathBuf {
    write(
        dir,
        name,
        &format!("name: wf\nagent_class: Workflow\nedges:\n  - - START\n    - {node_ref:?}\n"),
    )
}

/// Go `newWorkflowLayout`: a temporary base with `agent/` for the workflow under test and
/// `outside/join.yaml`, a valid node the workflow must not reach. Returns the temp dir guard,
/// the base, the agent directory and the outside node.
fn new_workflow_layout() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let (guard, base) = temp_dir();
    let agent_dir = base.join("agent");
    let outside_node = write(&base.join("outside"), "join.yaml", JOIN_NODE_YAML);
    std::fs::create_dir_all(&agent_dir).unwrap();
    (guard, base, agent_dir, outside_node)
}

#[tokio::test]
async fn test_workflow_node_reference_rejects_escaping_path() {
    let (_guard, _base, agent_dir, outside_node) = new_workflow_layout();
    let registry = test_registry();
    let outside = outside_node.to_string_lossy().into_owned();

    for (name, node_ref) in [
        ("parent traversal", "../outside/join.yaml"),
        ("absolute path", outside.as_str()),
    ] {
        // A workflow file of its own per case, so the node registry cannot hand back a node
        // cached by an earlier case.
        let wf = write_workflow(&agent_dir, &format!("wf_{name}.yaml"), node_ref);
        let result = registry.from_config(&wf).await;
        assert!(
            is_containment_rejection(&result),
            "{name}: FromConfig({wf:?}) with node ref {node_ref:?} = {:?}, want a containment rejection",
            result.err().map(|e| e.to_string())
        );
    }
}

#[tokio::test]
async fn test_workflow_node_reference_allows_paths_inside_workflow_dir() {
    let (_guard, _base, agent_dir, _) = new_workflow_layout();
    write(&agent_dir, "nodes/join.yaml", JOIN_NODE_YAML);
    let registry = test_registry();

    // Literal spellings, which would be cleaned of the interior ".." before the check by a
    // path join.
    for (i, node_ref) in [
        "nodes/join.yaml",
        "./nodes/join.yaml",
        "nodes/../nodes/join.yaml",
    ]
    .into_iter()
    .enumerate()
    {
        let wf = write_workflow(&agent_dir, &format!("wf_inside_{i}.yaml"), node_ref);
        if let Err(err) = registry.from_config(&wf).await {
            panic!(
                "FromConfig({wf:?}) with node ref {node_ref:?} inside the workflow directory = {err}, want success"
            );
        }
    }
}

/// Go `writeRoutedWorkflow`: a workflow whose edge routes from `router` to `target` through a
/// route map, the other path by which parseEdges resolves a node.
fn write_routed_workflow(dir: &Path, name: &str, router: &str, target: &str) -> PathBuf {
    write(
        dir,
        name,
        &format!(
            "name: wf\nagent_class: Workflow\nedges:\n  - - START\n    - {router:?}\n    - default: {target:?}\n"
        ),
    )
}

#[tokio::test]
async fn test_workflow_route_map_reference_is_checked() {
    let (_guard, _base, agent_dir, outside_node) = new_workflow_layout();
    write(
        &agent_dir,
        "nodes/router.yaml",
        "name: router_node\nagent_class: JoinNode\n",
    );
    write(&agent_dir, "nodes/join.yaml", JOIN_NODE_YAML);
    let registry = test_registry();

    let inside = write_routed_workflow(
        &agent_dir,
        "wf_route_inside.yaml",
        "nodes/router.yaml",
        "nodes/join.yaml",
    );
    if let Err(err) = registry.from_config(&inside).await {
        panic!(
            "FromConfig({inside:?}) with a route-map target inside the workflow directory = {err}, want success"
        );
    }

    let outside = outside_node.to_string_lossy().into_owned();
    for (name, node_ref) in [
        ("parent traversal", "../outside/join.yaml"),
        ("absolute path", outside.as_str()),
    ] {
        let wf = write_routed_workflow(
            &agent_dir,
            &format!("wf_route_{name}.yaml"),
            "nodes/router.yaml",
            node_ref,
        );
        let result = registry.from_config(&wf).await;
        assert!(
            is_containment_rejection(&result),
            "{name}: FromConfig({wf:?}) with route-map target {node_ref:?} = {:?}, want a containment rejection",
            result.as_ref().err().map(|e| e.to_string())
        );
        let err = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            err.contains(&format!("{node_ref:?}")),
            "{name}: FromConfig({wf:?}) = {err:?}, want the error to name the reference {node_ref:?}"
        );
    }
}

#[tokio::test]
async fn test_workflow_node_registry_does_not_leak_across_directories() {
    let (_guard, base, agent_dir, _) = new_workflow_layout();
    let real = write(&agent_dir, "join.node", JOIN_NODE_YAML);
    std::os::unix::fs::symlink("join.node", agent_dir.join("n.yaml")).unwrap();
    let registry = test_registry();

    let legit = write_workflow(&agent_dir, "wf_link.yaml", "n.yaml");
    if let Err(err) = registry.from_config(&legit).await {
        panic!(
            "FromConfig({legit:?}) = {err}, want success: the link stays inside the workflow directory"
        );
    }
    let real_key = real.to_string_lossy().into_owned();
    assert!(
        registry.inner.read().nodes.contains_key(&real_key),
        "nodeRegistry has no entry for the real path {real_key:?}, the test would not exercise the leak"
    );

    let other = write_workflow(&base.join("other"), "wf_other.yaml", &real_key);
    let err = registry
        .from_config(&other)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        err.contains("not found in registries"),
        "FromConfig({other:?}) with edge {real_key:?} = {err:?}, want the name lookup to fail as on main"
    );
}

#[tokio::test]
async fn test_workflow_node_reference_cache_does_not_bypass_check() {
    let (_guard, base, agent_dir, outside_node) = new_workflow_layout();
    let registry = test_registry();

    let legit = write_workflow(outside_node.parent().unwrap(), "wf_legit.yaml", "join.yaml");
    if let Err(err) = registry.from_config(&legit).await {
        panic!(
            "FromConfig({legit:?}) = {err}, want success: the node is inside its own workflow directory"
        );
    }
    let key = outside_node.to_string_lossy().into_owned();
    assert!(
        registry.inner.read().nodes.contains_key(&key),
        "nodeRegistry has no entry for {key:?} after loading it, the test would not exercise the cache"
    );

    let escaping = write_workflow(&agent_dir, "wf_cached.yaml", "../outside/join.yaml");
    let result = registry.from_config(&escaping).await;
    assert!(
        is_containment_rejection(&result),
        "FromConfig({escaping:?}) from under {base:?} with a ref to the cached node = {:?}, want a containment rejection",
        result.err().map(|e| e.to_string())
    );
}
