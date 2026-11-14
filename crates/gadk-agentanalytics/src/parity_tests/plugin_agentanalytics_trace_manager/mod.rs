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

//! Port of ADK-Go `plugin/agentanalytics/trace_manager_test.go` (ledger `docs/parity/7b-3a.md`):
//! 245 Go tests. The tests that need no runner (direct trace-manager and plugin-callback tests)
//! are in [`direct_1`] … [`direct_4`], with their helpers in [`direct_support`]; the
//! runner-level tests in [`runner_1`] and [`runner_2`], with their helpers in
//! [`runner_support`]; and [`deferred`] holds the `// parity: skipped TestX: deferred to 7b-3b
//! (goroutine attribution)` annotations of the 97 tests of spec R10 that need goroutine
//! attribution (two more deferred tests are annotated where their Go neighbours are ported:
//! `direct_4.rs`, `runner_2.rs`).

mod deferred;
mod direct_1;
mod direct_2;
mod direct_3;
mod direct_4;
pub(crate) mod direct_support;
mod runner_1;
mod runner_2;
pub(crate) mod runner_support;
