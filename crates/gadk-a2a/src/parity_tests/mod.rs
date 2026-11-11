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

//! In-crate parity targets: ports of ADK-Go tests that reach crate-private items, one module per
//! Go test file (plan Appendix A). Each module's owning task fills it. Targets that exercise the
//! server or the remote agent are compiled only with the features they need.

#[cfg(feature = "client")]
mod agent_remoteagent_v2_a2a_agent;
#[cfg(feature = "client")]
mod agent_remoteagent_v2_a2a_agent_run_processor;
#[cfg(feature = "client")]
mod agent_remoteagent_v2_auth;
#[cfg(feature = "client")]
mod agent_remoteagent_v2_before_run;
#[cfg(feature = "client")]
mod agent_remoteagent_v2_utils;
#[cfg(feature = "client")]
mod internal_agent_remoteagent_auth_scope;
#[cfg(all(feature = "server", feature = "client"))]
mod server_adka2a_v2_cancel_auth;
#[cfg(all(feature = "server", feature = "client"))]
mod server_adka2a_v2_executor;
mod server_adka2a_v2_metadata;
#[cfg(feature = "server")]
mod server_adka2a_v2_processor;
