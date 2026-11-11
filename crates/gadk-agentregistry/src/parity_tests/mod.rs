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

//! Ports of the ADK-Go `agentregistry` tests, one module per Go test file (plan Appendix A).
//! Each module's owning task fills it; the factory and e2e ports need the crate's features.

mod agentregistry_client;
#[cfg(all(feature = "a2a", feature = "mcp"))]
mod agentregistry_factory;
#[cfg(feature = "mcp")]
mod agentregistry_mcptoolset_e2e;
mod agentregistry_registry;
#[cfg(feature = "a2a")]
mod agentregistry_remoteagent_e2e;
mod agentregistry_requester;
mod agentregistry_resolve;
