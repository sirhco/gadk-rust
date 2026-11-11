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

//! In-crate parity targets: ports of ADK-Go tests that reach crate-private items (the wire DTOs,
//! the origin guard, the route table, the agent-graph writer, the debug store, the OIDC
//! validator seam), one module per Go test file (plan Appendix A). Each module's owning task
//! fills it.

mod internal_originguard_originguard;
mod server_adkrest_internal_models_event;
mod server_adkrest_internal_models_event_actions;
mod server_adkrest_internal_routers_auth;
mod server_adkrest_internal_services_agentgraphgenerator;
mod server_adkrest_internal_services_debugtelemetry;
#[cfg(feature = "agentengine")]
mod server_agentengine_controllers_method_stream_query;
#[cfg(feature = "agentengine")]
mod server_agentengine_controllers_method_streaming_agent_run_with_events;
#[cfg(feature = "agentengine")]
mod server_agentengine_handler;
#[cfg(feature = "agentengine")]
mod server_agentengine_internal_helper_encode;
mod server_authn_googleoidc;
