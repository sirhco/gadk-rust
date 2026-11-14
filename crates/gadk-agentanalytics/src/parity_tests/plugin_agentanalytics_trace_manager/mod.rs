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
//! 245 Go tests. Task 12 owns the tests that need no runner (direct trace-manager and
//! plugin-callback tests): [`direct_1`] … [`direct_4`], with their helpers in [`direct_support`].
//! Task 13 owns the runner-level tests, [`runner_1`] and [`runner_2`], with their helpers in
//! [`runner_support`], and [`deferred`]: the `// parity: skipped TestX: deferred to 7b-3b
//! (goroutine attribution)` annotations of the 97 tests that need goroutine attribution (spec R10).
//! Each file holds a fixed set of Go tests (Appendix A); this file is fixed (neither task edits
//! it).

mod deferred;
mod direct_1;
mod direct_2;
mod direct_3;
mod direct_4;
pub(crate) mod direct_support;
mod runner_1;
mod runner_2;
pub(crate) mod runner_support;
