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

//! Port of ADK-Go `plugin/agentanalytics/bigquery_agent_analytics_plugin_test.go` (ledger
//! `docs/parity/7b-3a.md`): 61 Go tests, split by the Go file's order into [`part_1`] (the first
//! 30), [`part_2`] (the next 18) and [`part_3`] (the last 13), with the Go file's own helpers
//! they share in [`helpers`]. [`support`] holds the Go test helpers that
//! every 7b-3a port shares (Go's `mockInvocationContext`, `fakeBigQueryWriteServer`, the
//! row-capturing plugins and scripted models of the four Go test files); the other port modules
//! use it as `crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support`.

mod helpers;
mod part_1;
mod part_2;
mod part_3;
pub(crate) mod support;
