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

//! Ports of ADK-Go's `plugin/agentanalytics` test files in 7b-1's scope (ledger
//! `docs/parity/7b-1.md`), 7b-2's (`batch_processor_test.go`, `gcs_offloader_test.go`;
//! ledger `docs/parity/7b-2.md`) and 7b-3a's (`bigquery_agent_analytics_plugin_test.go`,
//! `events_test.go`, `late_callbacks_test.go`, `trace_manager_test.go`; ledger
//! `docs/parity/7b-3a.md`): one module per Go file, named by `scripts/parity-check`'s
//! `target_name` (`plugin/agentanalytics/views_test.go` → `plugin_agentanalytics_views`),
//! holding `fn test_<snake of TestName>` (`go_test_to_rust`).

mod plugin_agentanalytics_batch_processor;
mod plugin_agentanalytics_bigquery_agent_analytics_plugin;
mod plugin_agentanalytics_events;
mod plugin_agentanalytics_gcs_offloader;
mod plugin_agentanalytics_json_formatter;
mod plugin_agentanalytics_late_callbacks;
mod plugin_agentanalytics_schema_upgrade;
mod plugin_agentanalytics_trace_manager;
mod plugin_agentanalytics_views;
