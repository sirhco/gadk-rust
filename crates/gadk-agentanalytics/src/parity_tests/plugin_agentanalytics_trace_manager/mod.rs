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

//! Port of ADK-Go `plugin/agentanalytics/trace_manager_test.go` (ledger 7b-3a):
//! 245 Go tests. The tests that need no runner (direct trace-manager and plugin-callback tests)
//! are in [`direct_1`] … [`direct_4`], with their helpers in [`direct_support`]; the
//! runner-level tests in [`runner_1`] and [`runner_2`], with their helpers in
//! [`runner_support`]; and the 99 tests that need goroutine attribution (7b-3b) in the `attr_*`
//! files by mechanism — [`attr_fiber`] (A), [`attr_direct`] (B, F, H), [`attr_live`] (C),
//! [`attr_workflow_1`] / [`attr_workflow_2`] (D), [`attr_parallel`] (E), [`attr_spawns`] (G) —
//! with their shared helpers in [`attr_support`].

mod attr_direct;
mod attr_fiber;
mod attr_live;
mod attr_parallel;
mod attr_spawns;
pub(crate) mod attr_support;
mod attr_workflow_1;
mod attr_workflow_2;
mod direct_1;
mod direct_2;
mod direct_3;
mod direct_4;
pub(crate) mod direct_support;
mod runner_1;
mod runner_2;
pub(crate) mod runner_support;
