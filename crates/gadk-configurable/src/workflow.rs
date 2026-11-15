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

//! Go `internal/configurable/configurable_workflow.go`: node-function registration, the
//! `Workflow` agent factory, edge parsing and node resolution. The config structs live in
//! [`crate::config`]; the node-function and node registries are fields of [`Registry`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::FutureExt;
use gadk_agent::{Agent, AgentConfig, AgentRunFn, Context, new_agent};
use gadk_engine::workflow::{
    AgentNode, DEFAULT, Edge, JoinNode, Node, NodeConfig, START, StringRoute, ToolNode, Workflow,
    WorkflowOptions, chain, function_node,
};
use gadk_goyaml::{Document, Kind};
use gadk_platform::{go_path_dir, go_path_join, go_quote, go_sprint_value};
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::Value;

use crate::config::{
    self, BaseAgentConfig, FunctionNodeYamlConfig, JoinNodeYamlConfig, ToolNodeYamlConfig,
    WorkflowYamlConfig,
};
use crate::{
    ConfigError, LoadCtx, NodeFn, NodeOutput, Registry, StrNodeFn, go_abs, go_is_abs, read_file,
};

impl Registry {
    /// Go `RegisterNodeFunction` with a `func(agent.Context, any) (any, error)`: overwrites an
    /// existing registration silently, as Go does. (Go's bad-signature panic cannot happen with
    /// the typed API.)
    pub fn register_node_function(&self, name: &str, f: NodeFn) {
        // Go: configurable_workflow.go RegisterNodeFunction
        self.inner.write().node_functions.insert(name.to_owned(), f);
    }

    /// Go `RegisterNodeFunction` with a `func(agent.Context, string) (string, error)`:
    /// registered through `cast_node_function`'s `fmt.Sprint` coercion; overwrites.
    pub fn register_string_node_function(&self, name: &str, f: StrNodeFn) {
        // Go: configurable_workflow.go RegisterNodeFunction
        self.register_node_function(name, cast_node_function(f));
    }
}

/// Go `newWorkflowAgent`: `failed to unmarshal Workflow config: %w`,
/// `workflow 'name' is required`, `failed to parse workflow edges: %w`,
/// `failed to build workflow: %w`; the agent is `agent.New` with the collected sub-agents and
/// the workflow's `Run`.
pub(crate) async fn new_workflow_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_path: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    // Go: configurable_workflow.go newWorkflowAgent
    let cfg: WorkflowYamlConfig = config::unmarshal(&data)
        .map_err(|e| ConfigError::wrap("failed to unmarshal Workflow config", e))?;

    if cfg.name.is_empty() {
        return Err(ConfigError::msg("workflow 'name' is required"));
    }

    // We track loaded sub-agents to populate the Agent hierarchy
    let collector = Arc::new(Mutex::new(Vec::new()));

    let edge_parser_ctx = ctx.with_sub_agents(collector.clone());

    let edges = parse_edges(&registry, &edge_parser_ctx, &config_path, &cfg.edges)
        .await
        .map_err(|e| ConfigError::wrap("failed to parse workflow edges", e))?;

    // Go `workflow.WithMaxConcurrency(n)` treats a negative `n` as 0 (unlimited).
    let max_concurrency = usize::try_from(cfg.max_concurrency.max(0)).unwrap_or(usize::MAX);
    let wf = Workflow::new(
        &cfg.name,
        edges,
        WorkflowOptions::default().with_max_concurrency(max_concurrency),
    )
    .map_err(|e| ConfigError::wrap("failed to build workflow", ConfigError::other(e)))?;

    let sub_agents = std::mem::take(&mut *collector.lock());

    // Wrap workflow execution in standard custom agent
    let run: AgentRunFn = Arc::new(move |ctx| wf.run(ctx));
    new_agent(AgentConfig {
        name: cfg.name,
        description: cfg.description,
        sub_agents,
        run: Some(run),
        ..Default::default()
    })
    .map_err(ConfigError::other)
}

/// Go `parseEdges`: each edge node must be a sequence of scalars (nodes) and mappings (route
/// maps, `invalid route map format: %w`); chains without routes need two nodes, routed chains
/// a source; route keys sorted, `default` (case-insensitive) is the default route.
pub(crate) async fn parse_edges(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    nodes: &[Document],
) -> Result<Vec<Edge>, ConfigError> {
    // Go: configurable_workflow.go parseEdges
    let mut edges = Vec::new();

    for edge_doc in nodes {
        let edge_node = &edge_doc[edge_doc.root()];
        if edge_node.kind != Kind::Sequence {
            return Err(ConfigError::msg(
                "workflow edge chain must be a YAML list/sequence",
            ));
        }

        let mut chain_nodes: Vec<Arc<dyn Node>> = Vec::new();
        // Go `map[string]string` iterated through `slices.Sorted(maps.Keys(m))`: a BTreeMap
        // iterates its keys in the same byte order.
        let mut route_maps: Vec<BTreeMap<String, String>> = Vec::new();

        for &item in &edge_node.content {
            match edge_doc[item].kind {
                Kind::Scalar => {
                    let n = resolve_node_like(registry, ctx, parent_path, &edge_doc[item].value)
                        .await?;
                    chain_nodes.push(n);
                }
                Kind::Mapping => {
                    let m: BTreeMap<String, String> =
                        gadk_goyaml::from_node(edge_doc, item, config::options())
                            .map_err(|e| ConfigError::wrap("invalid route map format", e))?;
                    route_maps.push(m);
                }
                // Go prints the `yaml.Kind` number (`%v` of a `uint32` without `String`).
                kind => {
                    return Err(ConfigError::msg(format!(
                        "unsupported YAML node kind in edge chain: {}",
                        kind.bits()
                    )));
                }
            }
        }

        if route_maps.is_empty() {
            if chain_nodes.len() < 2 {
                return Err(ConfigError::msg(
                    "workflow edge chain must have at least 2 nodes",
                ));
            }
            edges.extend(chain(&chain_nodes));
        } else {
            let Some(router_node) = chain_nodes.last().cloned() else {
                return Err(ConfigError::msg(
                    "routing edge chain must have a source node",
                ));
            };
            if chain_nodes.len() >= 2 {
                edges.extend(chain(&chain_nodes));
            }

            for route_map in &route_maps {
                // Edge order is observable at runtime: it drives successor
                // dispatch order and the pending queue under max concurrency.
                // Sorted within each route map. A chain declaring several
                // route maps keeps them in document order, which is already
                // deterministic.
                for (route_val, target_ref) in route_map {
                    let target_node =
                        resolve_node_like(registry, ctx, parent_path, target_ref).await?;

                    // Go `strings.EqualFold(routeVal, "default")`: no non-ASCII rune
                    // case-folds to a letter of "default", so ASCII folding is exact.
                    let route = if route_val.eq_ignore_ascii_case("default") {
                        DEFAULT.clone()
                    } else {
                        Arc::new(StringRoute(route_val.clone()))
                    };

                    edges.push(Edge {
                        from: router_node.clone(),
                        to: target_node,
                        route: Some(route),
                    });
                }
            }
        }
    }

    Ok(edges)
}

/// Go `resolveNodeLike`: `START`; a `.yaml`/`.yml` reference (absolute, or relative to the
/// parent's directory) through the node cache and [`resolve_node_from_yaml`]; otherwise a
/// registered node function (never cached: a new function node per use), else
/// `node or function reference %q not found in registries`.
pub(crate) async fn resolve_node_like(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    node_ref: &str,
) -> Result<Arc<dyn Node>, ConfigError> {
    // Go: configurable_workflow.go resolveNodeLike
    if node_ref == "START" {
        return Ok(START.clone());
    }

    let is_yaml = node_ref.ends_with(".yaml") || node_ref.ends_with(".yml");

    let (cache_key, abs_path) = if is_yaml {
        let target_path = if go_is_abs(node_ref) {
            node_ref.to_owned()
        } else {
            let parent = parent_path.to_string_lossy();
            go_path_join(&[go_path_dir(&parent).as_str(), node_ref])
        };
        let abs_path = go_abs(Path::new(&target_path))
            .map_err(|e| ConfigError::wrap("failed to resolve absolute path", e))?;
        (abs_path.to_string_lossy().into_owned(), abs_path)
    } else {
        (node_ref.to_owned(), PathBuf::new())
    };

    let cached = registry.inner.read().nodes.get(&cache_key).cloned();
    if let Some(n) = cached {
        return Ok(n);
    }

    if !is_yaml {
        let typed_fn = registry.inner.read().node_functions.get(node_ref).cloned();
        let Some(typed_fn) = typed_fn else {
            return Err(ConfigError::msg(format!(
                "node or function reference {} not found in registries",
                go_quote(node_ref)
            )));
        };
        return Ok(new_function_node(node_ref, typed_fn, NodeConfig::default()));
    }

    let n = resolve_node_from_yaml(registry, ctx, parent_path, node_ref, &abs_path).await?;

    registry.inner.write().nodes.insert(cache_key, n.clone());
    Ok(n)
}

/// Go `resolveNodeFromYAML`: `FunctionNode`, `JoinNode`, `ToolNode`, else an agent reference
/// wrapped in an agent node and appended to the context's sub-agent collector.
pub(crate) async fn resolve_node_from_yaml(
    registry: &Registry,
    ctx: &LoadCtx,
    parent_path: &Path,
    node_ref: &str,
    abs_path: &Path,
) -> Result<Arc<dyn Node>, ConfigError> {
    // Go: configurable_workflow.go resolveNodeFromYAML
    let data = read_file(abs_path)?;
    let quoted_path = go_quote(&abs_path.to_string_lossy());

    let base_config: BaseAgentConfig = config::unmarshal(&data)
        .map_err(|e| ConfigError::wrap(format!("failed to parse config {quoted_path}"), e))?;

    if base_config.agent_class == "FunctionNode" {
        let cfg: FunctionNodeYamlConfig = config::unmarshal(&data).map_err(|e| {
            ConfigError::wrap(
                format!("failed to parse FunctionNode config {quoted_path}"),
                e,
            )
        })?;

        if cfg.name.is_empty() {
            return Err(ConfigError::msg("function node 'name' is required"));
        }
        if cfg.func_code.is_empty() {
            return Err(ConfigError::msg("function node 'func_code' is required"));
        }

        let typed_fn = registry
            .inner
            .read()
            .node_functions
            .get(&cfg.func_code)
            .cloned();
        let Some(typed_fn) = typed_fn else {
            return Err(ConfigError::msg(format!(
                "function {} not found in registry",
                go_quote(&cfg.func_code)
            )));
        };

        let node_config = NodeConfig {
            rerun_on_resume: cfg.rerun_on_resume,
            parallel_worker: cfg.parallel_worker,
            ..Default::default()
        };

        return Ok(new_function_node(&cfg.name, typed_fn, node_config));
    }

    if base_config.agent_class == "JoinNode" {
        let cfg: JoinNodeYamlConfig = config::unmarshal(&data).map_err(|e| {
            ConfigError::wrap(format!("failed to parse JoinNode config {quoted_path}"), e)
        })?;

        if cfg.name.is_empty() {
            return Err(ConfigError::msg("join node 'name' is required"));
        }

        return Ok(JoinNode::new(&cfg.name));
    }

    if base_config.agent_class == "ToolNode" {
        let cfg: ToolNodeYamlConfig = config::unmarshal(&data).map_err(|e| {
            ConfigError::wrap(format!("failed to parse ToolNode config {quoted_path}"), e)
        })?;

        if cfg.name.is_empty() {
            return Err(ConfigError::msg("tool node 'name' is required"));
        }
        if cfg.tool_code.is_empty() {
            return Err(ConfigError::msg("tool node 'tool_code' is required"));
        }

        let (t, _) = registry
            .resolve_tool_reference(ctx, &cfg.tool_code, cfg.args.as_ref())
            .await
            .map_err(|e| {
                ConfigError::wrap(
                    format!(
                        "failed to resolve tool {} for tool node {}",
                        go_quote(&cfg.tool_code),
                        go_quote(&cfg.name)
                    ),
                    e,
                )
            })?;

        let node_config = NodeConfig {
            rerun_on_resume: cfg.rerun_on_resume,
            parallel_worker: cfg.parallel_worker,
            ..Default::default()
        };

        // Go passes the nil tool of a toolset reference on to NewNamedToolNode.
        let Some(t) = t else {
            return Err(ConfigError::msg("tool cannot be nil"));
        };
        let node = ToolNode::named(&cfg.name, t, node_config).map_err(ConfigError::other)?;
        return Ok(node);
    }

    // Otherwise, it is a standard agent class. Resolve it as an agent and wrap in NewAgentNode.
    let ag = registry
        .resolve_agent_reference_with_ctx(ctx, parent_path, node_ref)
        .await?;

    // Track subagent dependency
    if let Some(collector) = &ctx.sub_agents {
        collector.lock().push(ag.clone());
    }

    // Wrap standard agent in workflow Node
    let node = AgentNode::new(ag, NodeConfig::default()).map_err(ConfigError::other)?;
    Ok(node)
}

/// Go `castNodeFunction`'s string branch: a nil input is `""`, a string input itself, any other
/// input `fmt.Sprint(input)`; the string result is the node's output.
// Divergence (ledger 6c-2): inputs are JSON values, so `fmt.Sprint` of a number prints it as a
// float64 (`1e+06`) even where Go's producer returned an `int` (`1000000`).
pub(crate) fn cast_node_function(f: StrNodeFn) -> NodeFn {
    // Go: configurable_workflow.go castNodeFunction
    Arc::new(move |ctx: Context, input: Value| {
        let s = match input {
            Value::Null => String::new(),
            Value::String(val) => val,
            other => go_sprint_value(&other),
        };
        let fut = f(ctx, s);
        async move { Ok(NodeOutput::Value(Value::String(fut.await?))) }.boxed()
    })
}

/// Go `workflow.NewFunctionNode(name, fn, cfg)` for a registered [`NodeFn`] (input `any`), with
/// Go's output type switch on the returned [`crate::NodeOutput`].
pub(crate) fn new_function_node(name: &str, f: NodeFn, cfg: NodeConfig) -> Arc<dyn Node> {
    // Go: workflow NewFunctionNode (as configurable_workflow.go calls it)
    // The body returns the event gadk-engine's function node would build for the output (its
    // `Event` output path forwards it unchanged), so all three Go output kinds keep their shape.
    function_node::<Value, Event, _, _>(
        name,
        move |ctx: Context, input: Value| {
            let f = f.clone();
            async move {
                let out = f(ctx.clone(), input).await?;
                Ok(match out {
                    NodeOutput::Event(ev) => *ev,
                    NodeOutput::Content(c) => {
                        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                        ev.response.content = Some(c);
                        ev
                    }
                    NodeOutput::Value(v) => {
                        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                        ev.output = (!v.is_null()).then_some(v);
                        ev
                    }
                })
            }
        },
        cfg,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use futures::StreamExt;
    use gadk_agent::{AgentError, InvocationContext};
    use gadk_genai::{Content, Part};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::tests::{
        CfgErr, ToolSeen, temp_dir, test_registry, test_tool_factory, test_toolset_factory, write,
    };
    use crate::{GoMap, GoValue, SubAgentsCollector};

    /// A string node function returning `prefix + input`.
    fn prefix_fn(prefix: &'static str) -> StrNodeFn {
        Arc::new(move |_ctx, input| async move { Ok(format!("{prefix}{input}")) }.boxed())
    }

    /// A node function returning `out` whatever its input.
    fn const_fn(out: NodeOutput) -> NodeFn {
        Arc::new(move |_ctx, _input| {
            let out = out.clone();
            async move { Ok(out) }.boxed()
        })
    }

    /// [`test_registry`] with the string node functions `a`, `b`, `c` (identity) registered.
    fn registry() -> Registry {
        let r = test_registry();
        for name in ["a", "b", "c"] {
            r.register_string_node_function(name, prefix_fn(""));
        }
        r
    }

    /// Go `yaml.Unmarshal` of `edges:` into `[]yaml.Node`.
    fn edge_docs(yaml: &str) -> Vec<Document> {
        let cfg: WorkflowYamlConfig = config::unmarshal(yaml.as_bytes()).unwrap();
        cfg.edges
    }

    fn label(e: &Edge) -> String {
        let route = match &e.route {
            None => "none".to_owned(),
            Some(r) => match r.summary() {
                gadk_engine::workflow::RouteSummary::Default => "<default>".to_owned(),
                gadk_engine::workflow::RouteSummary::String(s) => s,
                other => format!("{other:?}"),
            },
        };
        format!("{}->{}({route})", e.from.name(), e.to.name())
    }

    async fn parse(r: &Registry, parent: &Path, yaml: &str) -> Result<Vec<String>, ConfigError> {
        let docs = edge_docs(yaml);
        let edges = parse_edges(r, &LoadCtx::default(), parent, &docs).await?;
        Ok(edges.iter().map(label).collect())
    }

    async fn parse_err(r: &Registry, yaml: &str) -> String {
        parse(r, Path::new(""), yaml).await.cfg_err().to_string()
    }

    fn ctx() -> Context {
        InvocationContext::builder().invocation_id("inv-1").build()
    }

    /// Runs `node` on `input` and returns its single event.
    async fn run_node(node: &Arc<dyn Node>, input: Value) -> Result<Event, AgentError> {
        let mut events: Vec<Result<Event, AgentError>> = node.run(ctx(), input).collect().await;
        assert_eq!(events.len(), 1);
        events.remove(0)
    }

    // ---- RegisterNodeFunction / castNodeFunction ----

    #[tokio::test]
    async fn register_node_function_overwrites() {
        let r = Registry::empty();
        r.register_node_function("f", const_fn(NodeOutput::Value(json!(1))));
        r.register_node_function("f", const_fn(NodeOutput::Value(json!(2))));
        let f = r.inner.read().node_functions.get("f").cloned().unwrap();
        let NodeOutput::Value(v) = f(ctx(), Value::Null).await.unwrap() else {
            panic!("want a value");
        };
        assert_eq!(v, json!(2));

        r.register_string_node_function("f", prefix_fn("s:"));
        let f = r.inner.read().node_functions.get("f").cloned().unwrap();
        let NodeOutput::Value(v) = f(ctx(), json!("x")).await.unwrap() else {
            panic!("want a value");
        };
        assert_eq!(v, json!("s:x"));
        assert_eq!(r.inner.read().node_functions.len(), 1);
    }

    #[tokio::test]
    async fn cast_node_function_coerces_like_fmt_sprint() {
        let f = cast_node_function(prefix_fn(">"));
        for (input, want) in [
            (Value::Null, ">"),
            (json!("text"), ">text"),
            (json!(42), ">42"),
            (json!(1.5), ">1.5"),
            (json!(true), ">true"),
            (json!([1, "a"]), ">[1 a]"),
            (json!({"b": 2, "a": "x"}), ">map[a:x b:2]"),
        ] {
            let NodeOutput::Value(v) = f(ctx(), input.clone()).await.unwrap() else {
                panic!("want a value");
            };
            assert_eq!(v, json!(want), "input {input}");
        }
    }

    #[tokio::test]
    async fn cast_node_function_returns_the_error() {
        let failing: StrNodeFn =
            Arc::new(|_ctx, _input| async { Err(AgentError::other("boom")) }.boxed());
        let err = cast_node_function(failing)(ctx(), json!("x"))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    // ---- new_function_node: Go's output type switch ----

    #[tokio::test]
    async fn function_node_value_output_is_the_event_output() {
        let node = new_function_node(
            "n",
            const_fn(NodeOutput::Value(json!({"k": 1}))),
            NodeConfig::default(),
        );
        assert_eq!(node.name(), "n");
        let ev = run_node(&node, json!("in")).await.unwrap();
        assert_eq!(ev.output, Some(json!({"k": 1})));
        assert_eq!(ev.invocation_id, "inv-1");
        assert!(ev.response.content.is_none());
    }

    #[tokio::test]
    async fn function_node_nil_output_has_no_output() {
        let node = new_function_node(
            "n",
            const_fn(NodeOutput::Value(Value::Null)),
            NodeConfig::default(),
        );
        let ev = run_node(&node, Value::Null).await.unwrap();
        assert_eq!(ev.output, None);
    }

    #[tokio::test]
    async fn function_node_content_output_is_the_event_content() {
        let content = Content {
            role: "model".into(),
            parts: vec![Part::from_text("hi")],
        };
        let node = new_function_node(
            "n",
            const_fn(NodeOutput::Content(content.clone())),
            NodeConfig::default(),
        );
        let ev = run_node(&node, Value::Null).await.unwrap();
        assert_eq!(ev.response.content, Some(content));
        assert_eq!(ev.output, None);
    }

    #[tokio::test]
    async fn function_node_event_output_is_forwarded() {
        let f: NodeFn = Arc::new(|ctx: Context, input: Value| {
            async move {
                let mut ev = Event::new(ctx.platform(), "");
                ev.output = Some(input.clone());
                ev.routes = vec!["R".to_owned()];
                Ok(NodeOutput::Event(Box::new(ev)))
            }
            .boxed()
        });
        let node = new_function_node("n", f, NodeConfig::default());
        let ev = run_node(&node, json!("x")).await.unwrap();
        assert_eq!(ev.output, Some(json!("x")));
        assert_eq!(ev.routes, ["R"]);
        // An empty invocation id is filled in, as gadk-engine's function node does.
        assert_eq!(ev.invocation_id, "inv-1");
    }

    #[tokio::test]
    async fn function_node_errors_are_returned() {
        let f: NodeFn = Arc::new(|_ctx, _input| async { Err(AgentError::other("bad")) }.boxed());
        let node = new_function_node("n", f, NodeConfig::default());
        let err = run_node(&node, Value::Null).await.unwrap_err();
        assert_eq!(err.to_string(), "bad");
    }

    // ---- parseEdges ----

    #[tokio::test]
    async fn parse_edges_chains_and_routes() {
        let r = registry();
        let got = parse(
            &r,
            Path::new(""),
            "edges:\n  - [START, a, b]\n  - - b\n    - {Z: a, default: c}\n    - {DeFaUlT: a}\n",
        )
        .await
        .unwrap();
        assert_eq!(
            got,
            [
                "START->a(none)",
                "a->b(none)",
                "b->a(Z)",
                "b->c(<default>)",
                "b->a(<default>)",
            ]
        );
    }

    #[tokio::test]
    async fn parse_edges_router_is_the_last_scalar_anywhere_in_the_chain() {
        // Go collects every scalar into the chain, wherever the route maps sit.
        let r = registry();
        let got = parse(
            &r,
            Path::new(""),
            "edges:\n  - - START\n    - {X: c}\n    - a\n",
        )
        .await
        .unwrap();
        assert_eq!(got, ["START->a(none)", "a->c(X)"]);
    }

    #[tokio::test]
    async fn parse_edges_single_source_with_routes_has_no_chain_edge() {
        let r = registry();
        let got = parse(&r, Path::new(""), "edges:\n  - - a\n    - {\"1\": b}\n")
            .await
            .unwrap();
        assert_eq!(got, ["a->b(1)"]);
    }

    #[tokio::test]
    async fn parse_edges_empty_is_no_edges() {
        let r = registry();
        assert!(
            parse(&r, Path::new(""), "edges: []\n")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            parse(&r, Path::new(""), "name: x\n")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn parse_edges_errors_have_go_texts() {
        let r = registry();
        for (yaml, want) in [
            (
                "edges:\n  - lone\n",
                "workflow edge chain must be a YAML list/sequence",
            ),
            (
                "edges:\n  - {a: b}\n",
                "workflow edge chain must be a YAML list/sequence",
            ),
            (
                "edges:\n  - - START\n    - [a, b]\n",
                "unsupported YAML node kind in edge chain: 2",
            ),
            (
                "edges:\n  - - &x START\n    - *x\n",
                "unsupported YAML node kind in edge chain: 16",
            ),
            (
                "edges:\n  - - START\n    - a\n    - {A: [x]}\n",
                "invalid route map format: yaml: unmarshal errors:\n  line 4: cannot unmarshal !!seq into string",
            ),
            (
                "edges:\n  - - START\n    - a\n    - {A: ~}\n",
                "node or function reference \"\" not found in registries",
            ),
            (
                "edges:\n  - - a\n",
                "workflow edge chain must have at least 2 nodes",
            ),
            (
                "edges:\n  - - {A: a}\n",
                "routing edge chain must have a source node",
            ),
            (
                "edges:\n  - - START\n    - nope\n",
                "node or function reference \"nope\" not found in registries",
            ),
        ] {
            assert_eq!(parse_err(&r, yaml).await, want, "yaml {yaml:?}");
        }
    }

    // ---- resolveNodeLike ----

    #[tokio::test]
    async fn resolve_node_like_start_is_the_start_node() {
        let r = Registry::empty();
        let n = resolve_node_like(&r, &LoadCtx::default(), Path::new(""), "START")
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&n, &START));
    }

    #[tokio::test]
    async fn resolve_node_like_function_refs_are_never_cached() {
        let r = registry();
        let ctx = LoadCtx::default();
        let n1 = resolve_node_like(&r, &ctx, Path::new(""), "a")
            .await
            .unwrap();
        let n2 = resolve_node_like(&r, &ctx, Path::new(""), "a")
            .await
            .unwrap();
        assert_eq!(n1.name(), "a");
        assert!(!Arc::ptr_eq(&n1, &n2), "a new function node per use");
        assert!(r.inner.read().nodes.is_empty());
    }

    #[tokio::test]
    async fn resolve_node_like_looks_function_refs_up_in_the_node_cache_first() {
        // Go checks nodeRegistry for every reference (nothing in Go inserts a function name).
        let r = registry();
        let join: Arc<dyn Node> = JoinNode::new("cached");
        r.inner.write().nodes.insert("a".to_owned(), join.clone());
        let n = resolve_node_like(&r, &LoadCtx::default(), Path::new(""), "a")
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&n, &join));
    }

    #[tokio::test]
    async fn resolve_node_like_caches_yaml_nodes_by_absolute_path() {
        let (_guard, base) = temp_dir();
        let parent = write(&base, "wf/workflow.yaml", "");
        let join = write(&base, "wf/nodes/j.yml", "agent_class: JoinNode\nname: j\n");
        let r = registry();
        let ctx = LoadCtx::default();

        let n1 = resolve_node_like(&r, &ctx, &parent, "nodes/j.yml")
            .await
            .unwrap();
        assert_eq!(n1.name(), "j");
        // The same file through another spelling (and an absolute reference) is a cache hit.
        let n2 = resolve_node_like(&r, &ctx, &parent, "./nodes/../nodes/j.yml")
            .await
            .unwrap();
        let n3 = resolve_node_like(&r, &ctx, &parent, &join.to_string_lossy())
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&n1, &n2) && Arc::ptr_eq(&n1, &n3));
        let keys: Vec<String> = r.inner.read().nodes.keys().cloned().collect();
        assert_eq!(keys, [join.to_string_lossy().into_owned()]);

        // A cached node is returned even after the file changed.
        std::fs::write(&join, "agent_class: JoinNode\nname: other\n").unwrap();
        let n4 = resolve_node_like(&r, &ctx, &parent, "nodes/j.yml")
            .await
            .unwrap();
        assert_eq!(n4.name(), "j");
    }

    #[tokio::test]
    async fn resolve_node_like_does_not_cache_failures() {
        let (_guard, base) = temp_dir();
        let parent = write(&base, "workflow.yaml", "");
        write(&base, "j.yaml", "agent_class: JoinNode\n");
        let r = registry();
        let ctx = LoadCtx::default();
        let err = resolve_node_like(&r, &ctx, &parent, "j.yaml")
            .await
            .cfg_err();
        assert_eq!(err.to_string(), "join node 'name' is required");
        assert!(r.inner.read().nodes.is_empty());
        write(&base, "j.yaml", "agent_class: JoinNode\nname: j\n");
        let n = resolve_node_like(&r, &ctx, &parent, "j.yaml")
            .await
            .unwrap();
        assert_eq!(n.name(), "j");
    }

    // ---- resolveNodeFromYAML ----

    /// Resolves `file` (written with `content` next to a workflow config) as a node.
    async fn yaml_node(
        r: &Registry,
        ctx: &LoadCtx,
        file: &str,
        content: &str,
    ) -> (
        Result<Arc<dyn Node>, ConfigError>,
        PathBuf,
        tempfile::TempDir,
    ) {
        let (guard, base) = temp_dir();
        let parent = write(&base, "workflow.yaml", "");
        let path = write(&base, file, content);
        (resolve_node_like(r, ctx, &parent, file).await, path, guard)
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_read_and_parse_errors_have_go_texts() {
        let r = registry();
        let ctx = LoadCtx::default();
        let (guard, base) = temp_dir();
        let parent = write(&base, "workflow.yaml", "");
        let missing = base.join("missing.yaml");
        let err = resolve_node_like(&r, &ctx, &parent, "missing.yaml")
            .await
            .cfg_err();
        assert_eq!(
            err.to_string(),
            format!("open {}: no such file or directory", missing.display())
        );
        drop(guard);

        for (content, want) in [
            (
                "name: [\n",
                "failed to parse config {path}: yaml: line 1: did not find expected node content",
            ),
            (
                "name: [a]\n",
                "failed to parse config {path}: yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into string",
            ),
            (
                "name: f\nagent_class: FunctionNode\nfunc_code: a\nparallel_worker: x\n",
                "failed to parse FunctionNode config {path}: yaml: unmarshal errors:\n  line 4: cannot unmarshal !!str `x` into bool",
            ),
            (
                "name: t\nagent_class: ToolNode\ntool_code: test_tool\nargs: x\n",
                "failed to parse ToolNode config {path}: yaml: unmarshal errors:\n  line 4: cannot unmarshal !!str `x` into map[string]interface {}",
            ),
        ] {
            let (res, path, _guard) = yaml_node(&r, &ctx, "n.yaml", content).await;
            let want = want.replace("{path}", &go_quote(&path.to_string_lossy()));
            assert_eq!(res.cfg_err().to_string(), want, "content {content:?}");
        }
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_function_node() {
        let r = registry();
        let ctx = LoadCtx::default();
        let (res, _path, _guard) = yaml_node(
            &r,
            &ctx,
            "f.yaml",
            "agent_class: FunctionNode\nname: my_fn\nfunc_code: a\nrerun_on_resume: false\nparallel_worker: true\n",
        )
        .await;
        let node = res.unwrap();
        assert_eq!(node.name(), "my_fn");
        assert_eq!(node.config().rerun_on_resume, Some(false));
        assert!(node.config().parallel_worker);
        let ev = run_node(&node, json!("in")).await.unwrap();
        assert_eq!(ev.output, Some(json!("in")));

        let (res, _path, _guard) = yaml_node(
            &r,
            &ctx,
            "f.yaml",
            "agent_class: FunctionNode\nname: f2\nfunc_code: a\n",
        )
        .await;
        let node = res.unwrap();
        assert_eq!(node.config().rerun_on_resume, None);
        assert!(!node.config().parallel_worker);
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_function_node_errors() {
        let r = registry();
        let ctx = LoadCtx::default();
        for (content, want) in [
            (
                "agent_class: FunctionNode\nfunc_code: a\n",
                "function node 'name' is required",
            ),
            (
                "agent_class: FunctionNode\nname: f\n",
                "function node 'func_code' is required",
            ),
            (
                "agent_class: FunctionNode\nname: f\nfunc_code: nope\n",
                "function \"nope\" not found in registry",
            ),
        ] {
            let (res, _path, _guard) = yaml_node(&r, &ctx, "f.yaml", content).await;
            assert_eq!(res.cfg_err().to_string(), want);
        }
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_join_node() {
        let r = registry();
        let ctx = LoadCtx::default();
        let (res, _path, _guard) =
            yaml_node(&r, &ctx, "j.yaml", "agent_class: JoinNode\nname: agg\n").await;
        let node = res.unwrap();
        assert_eq!(node.name(), "agg");
        assert!(node.as_any().downcast_ref::<JoinNode>().is_some());

        let (res, _path, _guard) = yaml_node(&r, &ctx, "j.yaml", "agent_class: JoinNode\n").await;
        assert_eq!(res.cfg_err().to_string(), "join node 'name' is required");
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_tool_node_passes_args_and_ctx() {
        let r = registry();
        let seen = ToolSeen::default();
        r.register_tool_factory("test_tool", test_tool_factory(seen.clone()))
            .unwrap();
        let ctx = LoadCtx::default().with_parent_path(Path::new("/outer/agent.yaml"));
        let (res, _path, _guard) = yaml_node(
            &r,
            &ctx,
            "t.yaml",
            "agent_class: ToolNode\nname: my_tool\ntool_code: test_tool\nrerun_on_resume: true\nparallel_worker: true\nargs:\n  k: v\n",
        )
        .await;
        let node = res.unwrap();
        assert_eq!(node.name(), "my_tool");
        assert!(node.as_any().downcast_ref::<ToolNode>().is_some());
        assert_eq!(node.config().rerun_on_resume, Some(true));
        assert!(node.config().parallel_worker);
        let want_args: GoMap = [("k".to_owned(), GoValue::String("v".into()))]
            .into_iter()
            .collect();
        assert_eq!(
            seen.lock().clone(),
            [(Some(PathBuf::from("/outer/agent.yaml")), Some(want_args))]
        );
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_tool_node_errors() {
        let r = registry();
        r.register_toolset_factory("ts", test_toolset_factory("ts", ToolSeen::default()))
            .unwrap();
        let ctx = LoadCtx::default();
        for (content, want) in [
            (
                "agent_class: ToolNode\ntool_code: ts\n",
                "tool node 'name' is required",
            ),
            (
                "agent_class: ToolNode\nname: t\n",
                "tool node 'tool_code' is required",
            ),
            (
                "agent_class: ToolNode\nname: t\ntool_code: nope\n",
                "failed to resolve tool \"nope\" for tool node \"t\": tool 'nope' not found",
            ),
            (
                "agent_class: ToolNode\nname: t\ntool_code: ts\n",
                "tool cannot be nil",
            ),
        ] {
            let (res, _path, _guard) = yaml_node(&r, &ctx, "t.yaml", content).await;
            assert_eq!(res.cfg_err().to_string(), want);
        }
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_agent_node_is_collected_once() {
        let (_guard, base) = temp_dir();
        let parent = write(&base, "workflow.yaml", "");
        write(
            &base,
            "loop.yaml",
            "agent_class: LoopAgent\nname: looper\nmax_iterations: 1\n",
        );
        let r = registry();
        let collector: SubAgentsCollector = Default::default();
        let ctx = LoadCtx::default().with_sub_agents(collector.clone());

        let n1 = resolve_node_like(&r, &ctx, &parent, "loop.yaml")
            .await
            .unwrap();
        assert_eq!(n1.name(), "looper");
        let agent_node = n1.as_any().downcast_ref::<AgentNode>().unwrap();
        assert_eq!(agent_node.agent().name(), "looper");
        assert_eq!(collector.lock().len(), 1);

        // A cache hit returns the same node and does not collect the agent again.
        let n2 = resolve_node_like(&r, &ctx, &parent, "loop.yaml")
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&n1, &n2));
        assert_eq!(collector.lock().len(), 1);
        // The agent is also in the agent registry (ResolveAgentReference's cache).
        assert!(Arc::ptr_eq(
            &r.inner.read().agents[&base.join("loop.yaml")],
            agent_node.agent()
        ));
    }

    #[tokio::test]
    async fn resolve_node_from_yaml_agent_errors_are_returned_unwrapped() {
        let r = registry();
        let ctx = LoadCtx::default();
        let (res, _path, _guard) =
            yaml_node(&r, &ctx, "u.yaml", "agent_class: Nope\nname: u\n").await;
        assert_eq!(
            res.cfg_err().to_string(),
            "invalid agent class 'Nope': not registered. Ensure the package is imported"
        );

        // An absolute agent reference fails ResolveAgentReference (YAML nodes accept it).
        let (guard, base) = temp_dir();
        let parent = write(&base, "workflow.yaml", "");
        let abs = write(&base, "u.yaml", "agent_class: LoopAgent\nname: u\n");
        let err = resolve_node_like(&r, &ctx, &parent, &abs.to_string_lossy())
            .await
            .cfg_err();
        assert_eq!(
            err.to_string(),
            format!(
                "absolute paths are not allowed in AgentTool config_path: {}",
                abs.display()
            )
        );
        drop(guard);
    }

    // ---- newWorkflowAgent ----

    async fn load(r: &Registry, content: &str) -> (Result<Arc<dyn Agent>, ConfigError>, PathBuf) {
        let (guard, base) = temp_dir();
        let path = write(&base, "wf.yaml", content);
        let res = r.from_config(&path).await;
        drop(guard);
        (res, path)
    }

    #[tokio::test]
    async fn workflow_agent_errors_have_go_texts() {
        let r = registry();
        for (content, want) in [
            (
                "agent_class: Workflow\n",
                "workflow 'name' is required".to_owned(),
            ),
            (
                "agent_class: Workflow\nname: w\nedges:\n  - lone\n",
                "failed to parse workflow edges: workflow edge chain must be a YAML list/sequence"
                    .to_owned(),
            ),
            (
                "agent_class: Workflow\nname: w\nedges:\n  - [a, b]\n",
                "failed to build workflow: no start node found".to_owned(),
            ),
        ] {
            let (res, _path) = load(&r, content).await;
            assert_eq!(res.cfg_err().to_string(), want, "content {content:?}");
        }
    }

    #[tokio::test]
    async fn workflow_agent_unmarshal_errors_are_wrapped() {
        let r = registry();
        for (content, want) in [
            // Go: `into int` (gadk-goyaml names an i64 entry point `int64`; ledger 6c-2).
            (
                "agent_class: Workflow\nname: w\nmax_concurrency: x\n",
                "failed to unmarshal Workflow config: yaml: unmarshal errors:\n  line 3: cannot unmarshal !!str `x` into int64",
            ),
            // Go: `into []yaml.Node` (gadk-goyaml names a sequence `[]interface {}`; ledger 6c-2).
            (
                "agent_class: Workflow\nname: w\nedges: x\n",
                "failed to unmarshal Workflow config: yaml: unmarshal errors:\n  line 3: cannot unmarshal !!str `x` into []interface {}",
            ),
        ] {
            let (res, _path) = load(&r, content).await;
            assert_eq!(res.cfg_err().to_string(), want);
        }
    }

    #[tokio::test]
    async fn workflow_agent_has_name_description_and_collected_sub_agents() {
        let (_guard, base) = temp_dir();
        write(&base, "l1.yaml", "agent_class: LoopAgent\nname: l1\n");
        write(&base, "l2.yaml", "agent_class: LoopAgent\nname: l2\n");
        write(&base, "j.yaml", "agent_class: JoinNode\nname: j\n");
        let path = write(
            &base,
            "wf.yaml",
            "agent_class: Workflow\nname: w\ndescription: d\nmax_concurrency: -3\nedges:\n  - [START, l1.yaml, j.yaml]\n  - [START, l2.yaml, j.yaml]\n  - - j.yaml\n    - {default: l1.yaml}\n",
        );
        let r = registry();
        let ag = r.from_config(&path).await.unwrap();
        assert_eq!(ag.name(), "w");
        assert_eq!(ag.description(), "d");
        let subs: Vec<&str> = ag.sub_agents().iter().map(|a| a.name()).collect();
        assert_eq!(subs, ["l1", "l2"]);

        // The node cache is registry-wide: a second workflow reusing l1.yaml gets the cached
        // agent node, so l1 is not one of its sub-agents (Go behavior).
        let path2 = write(
            &base,
            "wf2.yaml",
            "agent_class: Workflow\nname: w2\nedges:\n  - [START, l1.yaml]\n",
        );
        let ag2 = r.from_config(&path2).await.unwrap();
        assert!(ag2.sub_agents().is_empty());
    }

    #[tokio::test]
    async fn workflow_agent_nested_workflow_collects_into_its_own_list() {
        let (_guard, base) = temp_dir();
        write(&base, "l.yaml", "agent_class: LoopAgent\nname: l\n");
        write(
            &base,
            "inner.yaml",
            "agent_class: Workflow\nname: inner\nedges:\n  - [START, l.yaml]\n",
        );
        let path = write(
            &base,
            "outer.yaml",
            "agent_class: Workflow\nname: outer\nedges:\n  - [START, inner.yaml]\n",
        );
        let r = registry();
        let ag = r.from_config(&path).await.unwrap();
        let subs: Vec<&str> = ag.sub_agents().iter().map(|a| a.name()).collect();
        assert_eq!(subs, ["inner"]);
        let inner_subs: Vec<&str> = ag.sub_agents()[0]
            .sub_agents()
            .iter()
            .map(|a| a.name())
            .collect();
        assert_eq!(inner_subs, ["l"]);
    }

    #[test]
    fn workflow_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}
        let r = Registry::empty();
        let ctx = LoadCtx::default();
        assert_send(new_workflow_agent(
            r.clone(),
            ctx.clone(),
            Vec::new(),
            PathBuf::new(),
        ));
        assert_send(parse_edges(&r, &ctx, Path::new(""), &[]));
        assert_send(resolve_node_like(&r, &ctx, Path::new(""), "x"));
    }
}
