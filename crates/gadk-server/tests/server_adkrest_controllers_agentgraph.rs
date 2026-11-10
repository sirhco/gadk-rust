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

//! Ports of ADK-Go `server/adkrest/controllers/agentgraph_test.go`, plus the agent-graph wire
//! fixtures (`main_40` … `main_45`) replayed against the controller directly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use gadk_agent::{Agent, AgentConfig, Loader, MultiLoader, SingleLoader, Tool};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use gadk_server::rest::PathVars;
use gadk_server::rest::controllers::{
    AgentGraphApiController, AgentNode, AppInfoResponse, DotGraph,
};
use http::{Request, StatusCode};
use serde_json::Value;

const GRAPH_APP_NAME: &str = "test-app";
/// Go `services.LightTheme.Background`.
const LIGHT_BACKGROUND: &str = "\"#ffffff\"";
/// Go `services.DarkTheme.Background`.
const DARK_BACKGROUND: &str = "\"#333537\"";

/// Go `newGraphAgent`: a plain agent with the given children.
fn new_graph_agent(
    name: &str,
    description: &str,
    sub_agents: Vec<Arc<dyn Agent>>,
) -> Arc<dyn Agent> {
    gadk_agent::agent::new(AgentConfig {
        name: name.into(),
        description: description.into(),
        sub_agents,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("agent.New({name:?}) failed: {e}"))
}

/// Go `graphController`: `root` behind a single-agent loader.
fn graph_controller(root: Arc<dyn Agent>) -> AgentGraphApiController {
    AgentGraphApiController::new(Some(Arc::new(SingleLoader::new(root))))
}

#[derive(Clone, Copy)]
enum Handler {
    BuildGraph,
    BuildGraphImage,
}

struct Recorded {
    status: StatusCode,
    content_type: String,
    body: String,
}

/// Go `callGraph`: invokes a graph handler with `app_name` set and the given query.
async fn call_graph(
    c: &AgentGraphApiController,
    h: Handler,
    app_name: &str,
    query: &[(&str, &str)],
) -> Recorded {
    let mut target = format!("/dev/apps/{app_name}/build_graph");
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query)
        .finish();
    if !encoded.is_empty() {
        target = format!("{target}?{encoded}");
    }
    call(c, h, app_name, &target).await
}

async fn call(c: &AgentGraphApiController, h: Handler, app_name: &str, target: &str) -> Recorded {
    let mut req = Request::get(target).body(Body::empty()).unwrap();
    req.extensions_mut().insert(PathVars(BTreeMap::from([(
        "app_name".to_owned(),
        app_name.to_owned(),
    )])));
    let resp = match h {
        Handler::BuildGraph => c.build_graph_handler(req).await,
        Handler::BuildGraphImage => c.build_graph_image_handler(req).await,
    };
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    Recorded {
        status,
        content_type,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

fn sorted_keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn test_build_graph_handler_root_shape() {
    let root = new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![]);
    let rr = call_graph(
        &graph_controller(root),
        Handler::BuildGraph,
        GRAPH_APP_NAME,
        &[],
    )
    .await;

    assert_eq!(
        rr.status,
        StatusCode::OK,
        "build_graph status; body: {}",
        rr.body
    );
    assert!(
        rr.body.contains(r#""sub_agents":[]"#),
        "build_graph raw body = {:?}, want it to contain \"sub_agents\":[], not a null sub_agents",
        rr.body
    );
    let body: Value = serde_json::from_str(&rr.body).unwrap();
    assert_eq!(
        sorted_keys(&body),
        ["readme", "root_agent"],
        "build_graph top-level field names"
    );
    let root_agent = &body["root_agent"];
    assert_eq!(
        sorted_keys(root_agent),
        ["description", "name", "sub_agents"],
        "build_graph root_agent field names"
    );
    assert_eq!(root_agent["name"], GRAPH_APP_NAME);
    assert_eq!(root_agent["description"], "the root agent");
    assert_eq!(
        root_agent["sub_agents"],
        Value::Array(vec![]),
        "build_graph root_agent sub_agents, want an empty JSON list"
    );
    assert_eq!(body["readme"], "", "build_graph readme, want it empty");
}

#[tokio::test]
async fn test_build_graph_handler_nested_tree() {
    let grandchild = new_graph_agent("grandchild", "leaf", vec![]);
    let child = new_graph_agent("child", "middle", vec![grandchild]);
    let root = new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![child]);

    let rr = call_graph(
        &graph_controller(root),
        Handler::BuildGraph,
        GRAPH_APP_NAME,
        &[],
    )
    .await;
    assert_eq!(
        rr.status,
        StatusCode::OK,
        "build_graph status; body: {}",
        rr.body
    );

    let got: AppInfoResponse = serde_json::from_str(&rr.body).unwrap();
    let want = AppInfoResponse {
        root_agent: AgentNode {
            name: GRAPH_APP_NAME.into(),
            description: "the root agent".into(),
            sub_agents: vec![AgentNode {
                name: "child".into(),
                description: "middle".into(),
                sub_agents: vec![AgentNode {
                    name: "grandchild".into(),
                    description: "leaf".into(),
                    sub_agents: vec![],
                }],
            }],
        },
        readme: String::new(),
    };
    assert_eq!(got, want, "build_graph nested tree mismatch");
}

#[tokio::test]
async fn test_build_graph_image_handler_without_node_is_keyed_by_root() {
    let child = new_graph_agent("child", "middle", vec![]);
    let root = new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![child]);

    let rr = call_graph(
        &graph_controller(root),
        Handler::BuildGraphImage,
        GRAPH_APP_NAME,
        &[],
    )
    .await;
    assert_eq!(
        rr.status,
        StatusCode::OK,
        "build_graph_image status; body: {}",
        rr.body
    );

    let got: BTreeMap<String, DotGraph> = serde_json::from_str(&rr.body).unwrap();
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        [GRAPH_APP_NAME],
        "build_graph_image map keys"
    );
    let dot = &got[GRAPH_APP_NAME].dot_src;
    assert!(
        !dot.is_empty(),
        "build_graph_image root entry dotSrc is empty, want Graphviz source"
    );
    assert!(
        dot.contains("child"),
        "build_graph_image root dotSrc = {dot:?}, want it to include the sub-agent"
    );
}

#[tokio::test]
async fn test_build_graph_image_handler_with_node_is_bare() {
    let child = new_graph_agent("child", "middle", vec![]);
    let root = new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![child]);

    let rr = call_graph(
        &graph_controller(root),
        Handler::BuildGraphImage,
        GRAPH_APP_NAME,
        &[("node", "child")],
    )
    .await;
    assert_eq!(
        rr.status,
        StatusCode::OK,
        "build_graph_image?node=child status; body: {}",
        rr.body
    );

    let body: Value = serde_json::from_str(&rr.body).unwrap();
    assert_eq!(
        sorted_keys(&body),
        ["dotSrc"],
        "build_graph_image?node=child field names"
    );
    let got: DotGraph = serde_json::from_str(&rr.body).unwrap();
    assert!(
        got.dot_src.contains("child"),
        "build_graph_image?node=child dotSrc = {:?}, want it to draw the child",
        got.dot_src
    );
    assert!(
        !got.dot_src.contains(GRAPH_APP_NAME),
        "build_graph_image?node=child dotSrc = {:?}, want the child subtree, not the root graph",
        got.dot_src
    );
}

#[tokio::test]
async fn test_build_graph_image_handler_unknown_node_is404() {
    let child = new_graph_agent("child", "middle", vec![]);
    let root = new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![child]);

    let rr = call_graph(
        &graph_controller(root),
        Handler::BuildGraphImage,
        GRAPH_APP_NAME,
        &[("node", "no_such_agent")],
    )
    .await;
    assert_eq!(
        rr.status,
        StatusCode::NOT_FOUND,
        "build_graph_image?node=no_such_agent status; body: {}",
        rr.body
    );
    assert!(
        !rr.body.contains("dotSrc"),
        "build_graph_image?node=no_such_agent body = {:?}, want an error rather than a fallback graph",
        rr.body
    );
}

#[tokio::test]
async fn test_build_graph_image_handler_resolves_by_path() {
    let shared_under_a = new_graph_agent(
        "shared",
        "under branch_a",
        vec![new_graph_agent("leaf_a", "a", vec![])],
    );
    let branch_a = new_graph_agent("branch_a", "first branch", vec![shared_under_a]);
    let shared_under_b = new_graph_agent(
        "shared",
        "under branch_b",
        vec![new_graph_agent("leaf_b", "b", vec![])],
    );
    let mid = new_graph_agent("mid", "middle", vec![shared_under_b]);
    let branch_b = new_graph_agent("branch_b", "second branch", vec![mid]);
    let root = new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![branch_a, branch_b]);
    let c = graph_controller(root);

    let repeating_root = format!("{GRAPH_APP_NAME}/branch_b/mid/shared");
    let cases = [
        ("shallow duplicate", "branch_a/shared", "leaf_a", "leaf_b"),
        ("deep duplicate", "branch_b/mid/shared", "leaf_b", "leaf_a"),
        (
            "path repeating the root name",
            repeating_root.as_str(),
            "leaf_b",
            "leaf_a",
        ),
        // The UI suffixes a segment with "@" and a discriminator when the same agent shows up
        // more than once in a trace.
        (
            "segments carrying an @ suffix",
            "branch_a@1/shared@2",
            "leaf_a",
            "leaf_b",
        ),
    ];
    for (name, node, want_leaf, want_no_leaf) in cases {
        let rr = call_graph(
            &c,
            Handler::BuildGraphImage,
            GRAPH_APP_NAME,
            &[("node", node)],
        )
        .await;
        assert_eq!(
            rr.status,
            StatusCode::OK,
            "{name}: build_graph_image?node={node}; body: {}",
            rr.body
        );
        let got: DotGraph = serde_json::from_str(&rr.body).unwrap();
        assert!(
            got.dot_src.contains(want_leaf),
            "{name}: dotSrc = {:?}, want it to contain {want_leaf:?}",
            got.dot_src
        );
        assert!(
            !got.dot_src.contains(want_no_leaf),
            "{name}: dotSrc = {:?}, want it not to contain {want_no_leaf:?} from the other branch",
            got.dot_src
        );
    }
}

#[tokio::test]
async fn test_build_graph_image_handler_honours_dark_mode() {
    let root = new_graph_agent(
        GRAPH_APP_NAME,
        "the root agent",
        vec![new_graph_agent("child", "middle", vec![])],
    );
    let c = graph_controller(root);
    let dot_for = async |dark_mode: &str| {
        let rr = call_graph(
            &c,
            Handler::BuildGraphImage,
            GRAPH_APP_NAME,
            &[("dark_mode", dark_mode)],
        )
        .await;
        assert_eq!(
            rr.status,
            StatusCode::OK,
            "build_graph_image?dark_mode={dark_mode}; body: {}",
            rr.body
        );
        let mut got: BTreeMap<String, DotGraph> = serde_json::from_str(&rr.body).unwrap();
        got.remove(GRAPH_APP_NAME).unwrap_or_default().dot_src
    };

    let light = dot_for("false").await;
    let dark = dot_for("true").await;

    assert_ne!(
        light, dark,
        "build_graph_image returned the same dotSrc for both modes"
    );
    assert!(
        light.contains(LIGHT_BACKGROUND),
        "dark_mode=false dotSrc = {light:?}, want the light background"
    );
    assert!(
        dark.contains(DARK_BACKGROUND),
        "dark_mode=true dotSrc = {dark:?}, want the dark background"
    );
    assert!(
        !light.contains(DARK_BACKGROUND),
        "dark_mode=false dotSrc = {light:?}, want it not to carry the dark background"
    );
}

#[tokio::test]
async fn test_graph_handlers_unknown_app() {
    let c = graph_controller(new_graph_agent(GRAPH_APP_NAME, "the root agent", vec![]));
    for (name, h) in [
        ("build_graph", Handler::BuildGraph),
        ("build_graph_image", Handler::BuildGraphImage),
    ] {
        let rr = call_graph(&c, h, "not-an-app", &[]).await;
        assert_eq!(
            rr.status,
            StatusCode::NOT_FOUND,
            "{name} for an unknown app; body: {}",
            rr.body
        );
    }
}

#[tokio::test]
async fn test_graph_handlers_nil_agent_loader() {
    let c = AgentGraphApiController::new(None);
    for (name, h) in [
        ("build_graph", Handler::BuildGraph),
        ("build_graph_image", Handler::BuildGraphImage),
    ] {
        let rr = call_graph(&c, h, GRAPH_APP_NAME, &[]).await;
        assert_eq!(
            rr.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{name} with no agent loader; body: {}",
            rr.body
        );
    }
}

// parity: skipped TestAgentGraphHandlersSurviveANilAgent: a gadk Loader returns Arc<dyn Agent>, which cannot be null, so the nil-agent guard has nothing to guard

/// A tool that only has a name (the probe's `save_note` as the graph sees it).
struct NamedTool(&'static str);

impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        ""
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn llm(name: &str, description: &str, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn Agent> {
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        tools,
        ..Default::default()
    })
    .unwrap()
}

/// The probe's loader (`scripts/gen-go-adkrest/SCENARIO.md`, "Sample agents"), as far as the
/// graph endpoints see it: names, descriptions, the sub-agent tree and tool names.
fn scenario_loader() -> Arc<dyn Loader> {
    let assistant = llm(
        "assistant",
        "Answers greetings and saves notes.",
        vec![Arc::new(NamedTool("save_note"))],
    );
    let pipeline = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "pipeline".into(),
            description: "Drafts, then reviews.".into(),
            sub_agents: vec![
                llm("drafter", "Writes a draft.", vec![]),
                llm("reviewer", "Reviews the draft.", vec![]),
            ],
            ..Default::default()
        },
    })
    .unwrap();
    Arc::new(MultiLoader::new(assistant, vec![pipeline]).unwrap())
}

/// Fixtures `main_40` … `main_45`: status, Content-Type and body byte for byte.
#[tokio::test]
async fn agent_graph_wire_fixtures_match_go() {
    let c = AgentGraphApiController::new(Some(scenario_loader()));
    for fixture in [
        include_str!("../testdata/wire/main_40_build_graph.json"),
        include_str!("../testdata/wire/main_41_build_graph_image.json"),
        include_str!("../testdata/wire/main_42_build_graph_image_pipeline_dark.json"),
        include_str!("../testdata/wire/main_43_build_graph_image_node.json"),
        include_str!("../testdata/wire/main_44_build_graph_image_node_missing.json"),
        include_str!("../testdata/wire/main_45_build_graph_unknown_app.json"),
    ] {
        let f: Value = serde_json::from_str(fixture).unwrap();
        let name = f["name"].as_str().unwrap();
        let target = f["request"]["target"].as_str().unwrap();
        let path = target.split('?').next().unwrap();
        let app_name = path.split('/').nth(3).unwrap();
        let h = if path.ends_with("/build_graph") {
            Handler::BuildGraph
        } else {
            Handler::BuildGraphImage
        };
        let rr = call(&c, h, app_name, target).await;
        let want = &f["response"];
        assert_eq!(
            rr.status.as_u16(),
            want["status"].as_u64().unwrap() as u16,
            "{name}: status"
        );
        let want_ct = want["headers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h[0] == "Content-Type")
            .and_then(|h| h[1].as_str())
            .unwrap();
        assert_eq!(rr.content_type, want_ct, "{name}: Content-Type");
        pretty_assertions::assert_eq!(rr.body, want["body"].as_str().unwrap(), "{name}: body");
    }
}
