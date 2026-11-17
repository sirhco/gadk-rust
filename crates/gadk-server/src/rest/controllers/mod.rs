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

//! The REST API controllers. Every handler is `async fn(&self, Request) -> Response`: path
//! variables come from the
//! request's [`PathVars`](crate::rest::PathVars) extension and the authenticated caller from its
//! [`Caller`](crate::authn::Caller) extension.

pub mod agentgraph;
pub mod apps;
pub mod artifacts;
pub mod debug;
pub mod devfeatures;
pub mod handlers;
pub mod live;
pub mod runtime;
pub mod sessions;
pub mod version;

pub use agentgraph::{AgentGraphApiController, AgentNode, AppInfoResponse, DotGraph};
pub use apps::AppsApiController;
pub use artifacts::ArtifactsApiController;
pub use debug::DebugApiController;
pub use devfeatures::{
    ErrorResponse, MetricsInfoResponse, NOT_INSTALLED_DETAIL, agent_builder_config_handler,
    metrics_info_handler, new_not_implemented_handler,
};
pub use runtime::{CheckOrigin, RuntimeApiController, RuntimeApiControllerConfig};
pub use sessions::SessionsApiController;
pub use version::{VersionApiController, VersionResponse};
