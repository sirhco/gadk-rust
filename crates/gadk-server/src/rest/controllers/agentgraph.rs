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

//! The endpoints behind the web UI's "Agent structure" panel.

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::{Agent, Loader};
use gadk_platform::go_quote;
use http::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use super::handlers::path_vars;
use crate::rest::agent_graph::{get_agent_graph_with_theme, theme_for};
use crate::rest::http_error::{encode_json_response, http_error};
use crate::rest::routing::{
    DEV_PREFIX, Request, Response, Route, controller_handler, query_get, route,
};

/// Serves the agent tree and its DOT graph.
pub struct AgentGraphApiController {
    loader: Option<Arc<dyn Loader>>,
}

impl AgentGraphApiController {
    /// A controller over the agents of `loader`.
    pub fn new(loader: Option<Arc<dyn Loader>>) -> Self {
        Self { loader }
    }

    /// The agent tree as [`AppInfoResponse`], which the UI builds its
    /// breadcrumb navigation from.
    pub async fn build_graph_handler(&self, req: Request) -> Response {
        let root = match self.load_agent(&req) {
            Ok(root) => root,
            Err(resp) => return resp,
        };
        encode_json_response(
            &AppInfoResponse {
                root_agent: to_agent_node(&*root),
                readme: String::new(),
            },
            StatusCode::OK,
        )
    }

    /// The agent tree's DOT source. Without a `node` query parameter
    /// the body maps the root agent's name to its [`DotGraph`] (the UI preloads every level);
    /// with one it is the bare [`DotGraph`] of the subtree that breadcrumb path names. The
    /// palette follows `dark_mode=true`.
    pub async fn build_graph_image_handler(&self, req: Request) -> Response {
        let root = match self.load_agent(&req) {
            Ok(root) => root,
            Err(resp) => return resp,
        };
        let theme = theme_for(query_get(req.uri().query(), "dark_mode") == "true");
        let node = query_get(req.uri().query(), "node");
        if node.is_empty() {
            let dot = get_agent_graph_with_theme(&*root, &[], theme);
            let body = BTreeMap::from([(root.name().to_owned(), DotGraph { dot_src: dot })]);
            return encode_json_response(&body, StatusCode::OK);
        }
        let target = match resolve_agent_path(&root, &node) {
            Ok(target) => target,
            Err(msg) => return http_error(StatusCode::NOT_FOUND, &msg),
        };
        let dot = get_agent_graph_with_theme(&*target, &[], theme);
        encode_json_response(&DotGraph { dot_src: dot }, StatusCode::OK)
    }

    /// Resolves the `app_name` path variable to its agent, or the error response to send.
    #[allow(clippy::result_large_err)] // the Err side is the response itself
    fn load_agent(&self, req: &Request) -> Result<Arc<dyn Agent>, Response> {
        let Some(loader) = &self.loader else {
            return Err(http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "no agent loader configured",
            ));
        };
        let vars = path_vars(req);
        let app_name = vars
            .0
            .get("app_name")
            .map(String::as_str)
            .unwrap_or_default();
        if app_name.is_empty() {
            return Err(http_error(
                StatusCode::BAD_REQUEST,
                "app_name parameter is required",
            ));
        }
        loader
            .load_agent(app_name)
            .map_err(|e| http_error(StatusCode::NOT_FOUND, &e.to_string()))
    }
}

/// Walks a slash-separated breadcrumb path from the root agent, with no
/// fallback to a name search. A segment's `@<discriminator>` suffix is dropped, empty segments
/// are skipped, and a first segment equal to the root's name is skipped.
fn resolve_agent_path(root: &Arc<dyn Agent>, path: &str) -> Result<Arc<dyn Agent>, String> {
    let mut current = root.clone();
    for (i, segment) in path.split('/').enumerate() {
        let name = segment.split_once('@').map_or(segment, |(name, _)| name);
        if name.is_empty() || (i == 0 && name == root.name()) {
            continue;
        }
        let Some(next) = current.find_sub_agent(name) else {
            return Err(format!(
                "agent {} not found under {} in path {}",
                go_quote(name),
                go_quote(current.name()),
                go_quote(path)
            ));
        };
        current = next;
    }
    Ok(current)
}

/// The [`AgentNode`] tree of `agent` and its sub-agents.
fn to_agent_node(agent: &dyn Agent) -> AgentNode {
    AgentNode {
        name: agent.name().to_owned(),
        description: agent.description().to_owned(),
        sub_agents: agent
            .sub_agents()
            .iter()
            .map(|s| to_agent_node(&**s))
            .collect(),
    }
}

/// One agent of the tree `build_graph` returns. The UI walks
/// children through `sub_agents` entries carrying a `name`, so both are part of the contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNode {
    /// The agent's name.
    pub name: String,
    /// Omitted from the JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The agent's sub-agents; always a list (never `null`), even when empty.
    #[serde(default)]
    pub sub_agents: Vec<AgentNode>,
}

/// The agent tree response: the root agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfoResponse {
    /// The root of the agent tree.
    pub root_agent: AgentNode,
    /// Always empty: gadk agents are defined in code.
    pub readme: String,
}

/// Graphviz DOT source, which the UI renders client-side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DotGraph {
    /// The DOT text (`dotSrc`).
    #[serde(rename = "dotSrc")]
    pub dot_src: String,
}

/// The agent graph routes.
pub(crate) fn agent_graph_routes(c: Arc<AgentGraphApiController>) -> Vec<Route> {
    vec![
        route(
            "GetAppInfo",
            &[Method::GET],
            format!("{DEV_PREFIX}/build_graph"),
            controller_handler!(c, build_graph_handler),
        ),
        route(
            "GetAgentGraphImage",
            &[Method::GET],
            format!("{DEV_PREFIX}/build_graph_image"),
            controller_handler!(c, build_graph_image_handler),
        ),
    ]
}
