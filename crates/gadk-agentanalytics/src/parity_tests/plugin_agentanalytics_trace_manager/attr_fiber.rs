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

//! Goroutine-attribution ports (7b-3b) of ADK-Go `plugin/agentanalytics/trace_manager_test.go`:
//! group A: the goroutine-identity unit tests (`parseGoroutineTrace`, `currentGoroutine`, the
//! creators and attempt sites), adapted to `gadk_agent::fiber`.
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestParseGoroutineTrace: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestCurrentGoroutine: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestGoroutineCreatorsMatchADK: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestAttemptSiteOfTrace: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestAttemptSitesMatchADK: deferred to 7b-3b (goroutine attribution)
