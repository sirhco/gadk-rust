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

//! Port of Go `controllers/agentgraph.go` and the agent-graph router of
//! `internal/routers/agentgraph.go` (plan 5a Task 9).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_agent::Loader;
use http::Method;
use serde::{Deserialize, Serialize};

use crate::rest::routing::{DEV_PREFIX, Request, Response, Route, controller_handler, route};

/// Port of Go `controllers.AgentGraphAPIController`.
pub struct AgentGraphApiController {
    loader: Option<Arc<dyn Loader>>,
}

impl AgentGraphApiController {
    /// Go `NewAgentGraphAPIController`.
    pub fn new(loader: Option<Arc<dyn Loader>>) -> Self {
        Self { loader }
    }

    /// Go `BuildGraphHandler`: the agent tree as [`AppInfoResponse`].
    pub async fn build_graph_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 9")
    }

    /// Go `BuildGraphImageHandler`: the agent tree's DOT source as [`DotGraph`].
    pub async fn build_graph_image_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 9")
    }
}

/// Port of Go `controllers.AgentNode`: one agent of the tree `build_graph` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNode {
    /// The agent's name.
    pub name: String,
    /// `omitempty`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The agent's sub-agents.
    #[serde(default)]
    pub sub_agents: Vec<AgentNode>,
}

/// Port of Go `controllers.AppInfoResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfoResponse {
    /// The root of the agent tree.
    pub root_agent: AgentNode,
    /// Always empty: gadk agents are defined in code.
    pub readme: String,
}

/// Port of Go `controllers.DotGraph`: Graphviz DOT source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DotGraph {
    /// Go `DotSrc` (`dotSrc`).
    #[serde(rename = "dotSrc")]
    pub dot_src: String,
}

/// Go `AgentGraphAPIRouter.Routes`.
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
