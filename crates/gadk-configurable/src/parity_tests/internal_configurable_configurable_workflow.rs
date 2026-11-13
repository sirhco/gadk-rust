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

//! Port of ADK-Go `internal/configurable/configurable_workflow_test.go`.

use std::any::Any;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use gadk_agent::{Agent, Context, InvocationContext, RunnableTool, Tool, ToolError};
use gadk_engine::workflow::{DEFAULT, Route};
use gadk_genai::{Content, Part};
use gadk_goyaml::Document;
use gadk_platform::go_sprint_value;
use gadk_session::Event;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tests::{fake_key_env, temp_dir, write};
use crate::workflow::parse_edges;
use crate::{LoadCtx, NodeFn, NodeOutput, Registry, StrNodeFn, ToolFactory};

/// Go `upperFn`: converts input to uppercase and sets it as a route if it is "ALPHA" or "BETA".
fn upper_fn() -> NodeFn {
    Arc::new(|ctx: Context, input: Value| {
        async move {
            let s = match input {
                Value::Null => String::new(),
                Value::String(val) => val,
                other => go_sprint_value(&other),
            };
            let val = strings_to_upper(&s);
            if val == "ALPHA" || val == "BETA" {
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.output = Some(Value::String(val.clone()));
                ev.routes = vec![val];
                return Ok(NodeOutput::Event(Box::new(ev)));
            }
            Ok(NodeOutput::Value(Value::String(val)))
        }
        .boxed()
    })
}

/// Go `stringsToUpper`: ASCII `a`–`z` only.
fn strings_to_upper(s: &str) -> String {
    s.chars()
        .map(|r| {
            if r.is_ascii_lowercase() {
                r.to_ascii_uppercase()
            } else {
                r
            }
        })
        .collect()
}

/// A Go `func(agent.Context, string) (string, error)` from a plain function.
fn str_fn(f: fn(&str) -> String) -> StrNodeFn {
    Arc::new(move |_ctx, input| {
        let out = f(&input);
        async move { Ok(out) }.boxed()
    })
}

/// Go `suffixFn`: appends " done" to input.
fn suffix_fn(input: &str) -> String {
    format!("{input} done")
}

/// Go `alphaFn`.
fn alpha_fn(input: &str) -> String {
    format!("alpha: {input}")
}

/// Go `betaFn`.
fn beta_fn(input: &str) -> String {
    format!("beta: {input}")
}

/// Go `routeHandler`: stands in for a route target; this test only parses the graph.
fn route_handler(input: &str) -> String {
    input.to_owned()
}

/// Go `testTool`.
struct TestTool;

impl Tool for TestTool {
    fn name(&self) -> &str {
        "test_tool"
    }

    fn description(&self) -> &str {
        "A simple test tool"
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl RunnableTool for TestTool {
    // The `async_trait` expansion of `async fn run` (gadk-configurable does not depend on
    // `async-trait`).
    fn run<'life0, 'async_trait>(
        &'life0 self,
        _ctx: Context,
        _args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Map<String, Value>, ToolError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async {
            let mut out = Map::new();
            out.insert("result".to_owned(), json!("tool_output"));
            Ok(out)
        })
    }
}

/// Go's package registries after the test file's `init()` functions: Go `init()`'s agent
/// classes and built-in tools, the node functions `upper_fn`, `suffix_fn`, `alpha_fn`,
/// `beta_fn`, `h1`…`h12`, and the `test_tool` tool factory.
fn go_init_registry() -> Registry {
    let r = Registry::new();
    r.set_env(fake_key_env());
    r.register_node_function("upper_fn", upper_fn());
    r.register_string_node_function("suffix_fn", str_fn(suffix_fn));
    r.register_string_node_function("alpha_fn", str_fn(alpha_fn));
    r.register_string_node_function("beta_fn", str_fn(beta_fn));
    for i in 1..=12 {
        r.register_string_node_function(&format!("h{i}"), str_fn(route_handler));
    }
    let test_tool: ToolFactory = Arc::new(|_r, _ctx, _args| {
        let t: Arc<dyn Tool> = Arc::new(TestTool);
        async move { Ok(t) }.boxed()
    });
    r.register_tool_factory("test_tool", test_tool).unwrap();
    r
}

/// Go's `MockInvocationContext{sess: &mockSession{}, userContent: text}` (invocation id
/// `test-inv-id`).
fn mock_ctx(text: &str) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id("test-inv-id")
        .user_content(Content {
            parts: vec![Part::from_text(text)],
            ..Content::default()
        })
        .build_invocation()
}

/// Runs `ag` and returns every non-nil event output (Go fails the test on a run error).
async fn run_outputs(ag: &Arc<dyn Agent>, text: &str) -> Vec<Value> {
    let mut events = ag.run(mock_ctx(text));
    let mut outputs = Vec::new();
    while let Some(ev) = events.next().await {
        let ev = ev.unwrap_or_else(|e| panic!("run failed: {e}"));
        if let Some(out) = ev.output {
            outputs.push(out);
        }
    }
    outputs
}

/// `FromConfig` that fails the test on an error (the agent has no `Debug`).
async fn from_config(r: &Registry, path: &Path) -> Arc<dyn Agent> {
    match r.from_config(path).await {
        Ok(ag) => ag,
        Err(e) => panic!("FromConfig failed: {e}"),
    }
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry); Go MockInvocationContext/mockSession → InvocationContext::builder() (a concrete struct; default in-memory session)
#[tokio::test]
async fn test_load_workflow_yaml() {
    let (_guard, temp_dir) = temp_dir();
    let yaml_content = "
name: my_wf
agent_class: Workflow
edges:
  - - START
    - upper_fn
    - suffix_fn
";
    let config_path = write(&temp_dir, "workflow.yaml", yaml_content);

    let r = go_init_registry();
    let ag = from_config(&r, &config_path).await;

    assert_eq!(ag.name(), "my_wf", "expected workflow name 'my_wf'");

    let outputs = run_outputs(&ag, "hello").await;

    assert_eq!(outputs.len(), 2, "expected 2 outputs, got {outputs:?}");
    assert_eq!(
        outputs[outputs.len() - 1],
        json!("HELLO done"),
        "expected final output 'HELLO done'"
    );
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry); Go MockInvocationContext/mockSession → InvocationContext::builder() (a concrete struct; default in-memory session)
#[tokio::test]
async fn test_load_workflow_with_routing_yaml() {
    let (_guard, temp_dir) = temp_dir();

    // Test conditional string route matching
    let yaml_content = "
name: routing_wf
agent_class: Workflow
edges:
  - - START
    - upper_fn
    - default: suffix_fn
";
    let config_path = write(&temp_dir, "routing_workflow.yaml", yaml_content);

    let r = go_init_registry();
    let ag = from_config(&r, &config_path).await;

    let outputs = run_outputs(&ag, "routing").await;

    assert_eq!(outputs.len(), 2, "expected 2 outputs, got {outputs:?}");
    assert_eq!(
        outputs[1],
        json!("ROUTING done"),
        "expected output 'ROUTING done'"
    );
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry); Go MockInvocationContext/mockSession → InvocationContext::builder() (a concrete struct; default in-memory session)
#[tokio::test]
async fn test_load_complex_workflow_with_sub_agents_yaml() {
    let (_guard, temp_dir) = temp_dir();

    // 1. Write the sub-agent YAML config
    let sub_agent_yaml = "
name: agent_alpha
agent_class: FunctionNode
func_code: alpha_fn
";
    write(&temp_dir, "agent_alpha.yaml", sub_agent_yaml);

    // 2. Write the main Workflow YAML config referencing the sub-agent and beta function
    let workflow_yaml = "
name: complex_wf
agent_class: Workflow
edges:
  - - START
    - upper_fn
    - ALPHA: agent_alpha.yaml
      BETA: beta_fn
";
    let workflow_path = write(&temp_dir, "complex_workflow.yaml", workflow_yaml);

    let r = go_init_registry();
    let ag = from_config(&r, &workflow_path).await;

    assert_eq!(
        ag.name(),
        "complex_wf",
        "expected complex workflow name 'complex_wf'"
    );

    // Verify that sub-agents are correctly loaded and registered in the Agent hierarchy
    let subs = ag.sub_agents();
    assert_eq!(
        subs.len(),
        0,
        "expected 0 sub-agents in hierarchy for pure function nodes"
    );

    // Test Branch 1 (ALPHA route)
    {
        let outputs = run_outputs(&ag, "alpha").await;

        // START -> upper_fn (produces "ALPHA") -> ALPHA route -> agent_alpha (produces "alpha: ALPHA")
        assert_eq!(
            outputs.len(),
            2,
            "expected 2 outputs for ALPHA branch, got {outputs:?}"
        );
        assert_eq!(outputs[0], json!("ALPHA"), "expected first output 'ALPHA'");
        assert_eq!(
            outputs[1],
            json!("alpha: ALPHA"),
            "expected final output 'alpha: ALPHA'"
        );
    }

    // Test Branch 2 (BETA route)
    {
        let outputs = run_outputs(&ag, "beta").await;

        // START -> upper_fn (produces "BETA") -> BETA route -> beta_fn (produces "beta: BETA")
        assert_eq!(
            outputs.len(),
            2,
            "expected 2 outputs for BETA branch, got {outputs:?}"
        );
        assert_eq!(outputs[0], json!("BETA"), "expected first output 'BETA'");
        assert_eq!(
            outputs[1],
            json!("beta: BETA"),
            "expected final output 'beta: BETA'"
        );
    }
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry)
#[tokio::test]
async fn test_load_complex_workflow_with_actual_sub_agents_yaml() {
    let (_guard, temp_dir) = temp_dir();

    // 1. Write the actual sub-agent YAML config (using LoopAgent, which is standard agent)
    let sub_agent_yaml = "
name: agent_loop
agent_class: LoopAgent
max_iterations: 5
";
    write(&temp_dir, "agent_loop.yaml", sub_agent_yaml);

    // 2. Write the main Workflow YAML config referencing the sub-agent
    let workflow_yaml = "
name: loop_wf
agent_class: Workflow
edges:
  - - START
    - agent_loop.yaml
";
    let workflow_path = write(&temp_dir, "loop_workflow.yaml", workflow_yaml);

    let r = go_init_registry();
    let ag = from_config(&r, &workflow_path).await;

    assert_eq!(ag.name(), "loop_wf", "expected workflow name 'loop_wf'");

    // Verify that sub-agents are correctly loaded and registered in the Agent hierarchy
    let subs = ag.sub_agents();
    assert_eq!(subs.len(), 1, "expected 1 sub-agent in hierarchy");
    assert_eq!(
        subs[0].name(),
        "agent_loop",
        "expected sub-agent name 'agent_loop'"
    );
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry); Go MockInvocationContext/mockSession → InvocationContext::builder() (a concrete struct; default in-memory session)
#[tokio::test]
async fn test_load_workflow_with_join_node_yaml() {
    let (_guard, temp_dir) = temp_dir();

    // 1. Write the join-node YAML config
    let join_yaml = "
name: aggregate_join
agent_class: JoinNode
";
    write(&temp_dir, "aggregate_join.yaml", join_yaml);

    // 2. Write the main Workflow YAML config referencing the join-node
    let workflow_yaml = "
name: join_wf
agent_class: Workflow
edges:
  - - START
    - alpha_fn
    - aggregate_join.yaml
  - - START
    - beta_fn
    - aggregate_join.yaml
";
    let workflow_path = write(&temp_dir, "join_workflow.yaml", workflow_yaml);

    let r = go_init_registry();
    let ag = from_config(&r, &workflow_path).await;

    assert_eq!(ag.name(), "join_wf", "expected workflow name 'join_wf'");

    let outputs = run_outputs(&ag, "hello").await;

    // Expect outputs from alpha_fn, beta_fn, and aggregate_join (the aggregated map)
    assert_eq!(outputs.len(), 3, "expected 3 outputs, got {outputs:?}");

    // Find the aggregated map from JoinNode
    let aggregated_map = outputs
        .iter()
        .find_map(Value::as_object)
        .unwrap_or_else(|| panic!("JoinNode aggregated output not found in outputs: {outputs:?}"));

    assert_eq!(
        aggregated_map.get("alpha_fn").and_then(Value::as_str),
        Some("alpha: hello"),
        "expected alpha_fn output 'alpha: hello'"
    );
    assert_eq!(
        aggregated_map.get("beta_fn").and_then(Value::as_str),
        Some("beta: hello"),
        "expected beta_fn output 'beta: hello'"
    );
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry); Go MockInvocationContext/mockSession → InvocationContext::builder() (a concrete struct; default in-memory session)
#[tokio::test]
async fn test_load_workflow_with_tool_node_yaml() {
    let (_guard, temp_dir) = temp_dir();

    // 1. Write the tool-node YAML config
    let tool_node_yaml = "
name: my_tool_node
agent_class: ToolNode
tool_code: test_tool
";
    write(&temp_dir, "my_tool_node.yaml", tool_node_yaml);

    // 2. Write the main Workflow YAML config referencing the tool-node
    let workflow_yaml = "
name: tool_wf
agent_class: Workflow
edges:
  - - START
    - my_tool_node.yaml
";
    let workflow_path = write(&temp_dir, "tool_workflow.yaml", workflow_yaml);

    let r = go_init_registry();
    let ag = from_config(&r, &workflow_path).await;

    let outputs = run_outputs(&ag, "{}").await;

    assert_eq!(outputs.len(), 1, "expected 1 output, got {outputs:?}");

    let tool_out = outputs[0]
        .as_object()
        .unwrap_or_else(|| panic!("expected tool output to be a map, got {:?}", outputs[0]));

    assert_eq!(
        tool_out.get("result").and_then(Value::as_str),
        Some("tool_output"),
        "expected tool output result 'tool_output'"
    );
}

/// Go's anonymous `struct { Edges []yaml.Node \`yaml:"edges"\` }`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct EdgesConfig {
    edges: Vec<Document>,
}

// parity: adapted: Go's package globals and test init()s → a Registry value (go_init_registry)
#[tokio::test]
async fn test_parse_edges_route_order_is_deterministic() {
    // A bare range cannot pass this test by luck, and the declaration order
    // below is what guarantees it. On go1.26 twelve entries fill two map
    // groups, each holding its keys in declaration order, and iteration
    // starts at a random slot and wraps around the table. So a bare range
    // yields sorted order only when the keys, read in sorted order around a
    // circle (zulu back to "10"), split into two arcs of four to eight keys,
    // each declared in the order the arc runs. "10", "2" and ALPHA are
    // neighbors on that circle and are declared in reverse, so both gaps
    // around "2" would have to fall between arcs, leaving "2" alone in one.
    // Keep ALPHA declared before "2", and "2" before "10".
    //
    // Three key choices pin which order, not just that there is one:
    // "10" before "2" separates byte order from numeric, "zulu" after
    // "default" separates it from an implementation that pins the default
    // route last, and the mixed cases separate it from case-folded order.
    const CONFIG: &str = r#"
edges:
  - - START
    - upper_fn
    - ZETA: h1
      ALPHA: h2
      MIKE: h3
      OSCAR: h4
      BRAVO: h5
      YANKEE: h6
      CHARLIE: h7
      TANGO: h8
      default: h9
      "2": h10
      zulu: h11
      "10": h12
"#;
    let cfg: EdgesConfig = gadk_goyaml::unmarshal(CONFIG.as_bytes(), crate::config::options())
        .unwrap_or_else(|e| panic!("yaml.Unmarshal() error = {e}"));

    let r = go_init_registry();
    let edges = match parse_edges(&r, &LoadCtx::default(), Path::new(""), &cfg.edges).await {
        Ok(edges) => edges,
        Err(e) => panic!("parseEdges() error = {e}"),
    };

    let got: Vec<String> = edges
        .iter()
        .map(|e| {
            format!(
                "{}->{}({})",
                e.from.name(),
                e.to.name(),
                route_label(e.route.as_ref())
            )
        })
        .collect();
    let want = [
        "START->upper_fn(none)",
        "upper_fn->h12(10)",
        "upper_fn->h10(2)",
        "upper_fn->h2(ALPHA)",
        "upper_fn->h5(BRAVO)",
        "upper_fn->h7(CHARLIE)",
        "upper_fn->h3(MIKE)",
        "upper_fn->h4(OSCAR)",
        "upper_fn->h8(TANGO)",
        "upper_fn->h6(YANKEE)",
        "upper_fn->h1(ZETA)",
        "upper_fn->h9(<default>)",
        "upper_fn->h11(zulu)",
    ];
    assert_eq!(got, want, "parseEdges() =\n\t{got:?}\nwant\n\t{want:?}");
}

/// Go `routeLabel`.
fn route_label(r: Option<&Arc<dyn Route>>) -> String {
    // Checked first because the summary below has no arm for the
    // unexported type behind workflow.Default, so a Default route reaching it
    // would be labeled with its Go type instead.
    let Some(r) = r else {
        return "none".to_owned();
    };
    if Arc::ptr_eq(r, &DEFAULT) {
        return "<default>".to_owned();
    }
    match r.summary() {
        gadk_engine::workflow::RouteSummary::String(v) => v,
        other => format!("{other:?}"),
    }
}
