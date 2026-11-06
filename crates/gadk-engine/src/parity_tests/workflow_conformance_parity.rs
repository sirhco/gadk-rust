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

//! Port of Go workflow/conformance_parity_test.go.

use std::sync::Arc;

use gadk_agent::{Context, Tool, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{FunctionTool, FunctionToolConfig};
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{
    Node, NodeConfig, ToolNode, Workflow, WorkflowNode, WorkflowOptions, function_node,
};

#[rstest::rstest]
#[case::root_wrapper_suppresses_prefix("synthetic_root", true, "leaf", vec!["leaf"])]
#[case::named_workflow_includes_ancestors("parent_wf", false, "parent_wf@1/leaf@1", vec!["parent_wf@1/leaf@1", "parent_wf@1"])]
#[tokio::test]
async fn test_workflow_pathing_parity(
    #[case] wf_name: &str,
    #[case] root_wrapper: bool,
    #[case] want_path: &str,
    #[case] want_output_for: Vec<&str>,
) {
    let node = function_node::<String, String, _, _>(
        "leaf",
        |_c: Context, _i: String| async { Ok("result".to_string()) },
        NodeConfig::default(),
    );
    let wf = Workflow::new(
        wf_name,
        vec![edge(&start(), &node)],
        WorkflowOptions {
            root_wrapper,
            ..WorkflowOptions::default()
        },
    )
    .expect("New() error");
    let events = drain(wf.run(seeded_mock_ctx())).await;
    let (path, output_for) = events
        .iter()
        .filter(|e| e.output.is_some())
        .filter_map(|e| e.node_info.as_ref())
        .map(|i| (i.path.clone(), i.output_for.clone()))
        .last()
        .unwrap_or_default();
    assert_eq!(path, want_path, "NodeInfo.Path");
    assert_eq!(output_for, want_output_for, "NodeInfo.OutputFor");
}

#[rstest::rstest]
#[case::compound_path_strips_workflow_prefix("sub_wf", "child_leaf", "child_output")]
#[case::nested_hierarchical_path_resolves_terminal("deep/nested_wf", "leaf_step", "nested_output")]
#[tokio::test]
async fn test_workflow_node_terminal_output_capture(
    #[case] sub_wf_name: &str,
    #[case] sub_node: &str,
    #[case] want_output: &'static str,
) {
    let child = function_node::<String, String, _, _>(
        sub_node,
        move |_c: Context, _i: String| async move { Ok(want_output.to_string()) },
        NodeConfig::default(),
    );
    let wf_node: Arc<dyn Node> = WorkflowNode::new(sub_wf_name, vec![edge(&start(), &child)])
        .expect("NewWorkflowNode() error");
    let outer = Workflow::new(
        "outer",
        vec![edge(&start(), &wf_node)],
        WorkflowOptions::default(),
    )
    .expect("New() error");
    let events = drain(outer.run(seeded_mock_ctx())).await;
    let got: Option<Value> = events.iter().filter_map(|e| e.output.clone()).last();
    assert_eq!(got, Some(json!(want_output)), "captured output");
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Empty {}

#[rstest::rstest]
#[case::explicit_name_overrides_default("custom_node_name", "custom_node_name")]
#[case::empty_name_falls_back_to_tool_name("", "default_tool_name")]
#[tokio::test]
async fn test_tool_node_explicit_naming(#[case] explicit_name: &str, #[case] want_name: &str) {
    let dummy_tool: Arc<dyn Tool> = FunctionTool::new(
        FunctionToolConfig::new("default_tool_name", "dummy description"),
        |_c: Context, _i: Empty| async move { Ok::<_, ToolError>("ok".to_string()) },
    )
    .expect("functiontool.New() error");
    let node = ToolNode::named(explicit_name, dummy_tool, NodeConfig::default())
        .expect("NewNamedToolNode() error");
    assert_eq!(node.name(), want_name, "node.Name()");
}
