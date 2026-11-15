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

//! Ports of ADK-Go tests that need crate-internal access, one module per Go test file (plan
//! Appendix A): Go internal-package tests, plus the external-package tests that Appendix A marks
//! `external→internal` (plan decision D2).
//! Go packages: `agent`, `internal/...`, `plugin`, `tool`.
//! Each module's owning task fills it.

mod agent_agent;
mod agent_agent_on_error;
mod agent_callback_context_wrapper;
mod agent_common_context;
mod agent_common_context_delta;
mod agent_context_mock;
mod agent_identity;
mod agent_identity_concurrency;
mod agent_identity_matrix;
mod agent_loader;
mod agent_with_agent_timeout_wrapper;
mod internal_adkcontext_adkcontext;
mod internal_agent_runconfig_limits;
mod internal_context_context;
mod internal_context_from_context;
mod internal_context_identity_concurrency;
mod internal_context_identity_mappings;
mod internal_plugininternal_plugin_manager_context;
mod plugin_plugin;
mod tool_context;
