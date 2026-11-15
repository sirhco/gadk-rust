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
//! group C: `RunLive` tool calls on the live session's goroutine.
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestTraceManager_LiveToolCachedChildStopRace: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ConcurrentSameAgentWithPromotedSubNodePath: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_LiveToolConcurrentSameChildWithPromotedPathUnderInvocationRoot: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_LiveToolReturnsBeforeUnjoinedCachedChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_JoinedChildStoppedAtErrorKeepsCompletedIdentities: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_LiveToolSubAgentCallMovesToLateState: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_LiveToolReturnsBeforeHelperModelCallEnds_KeepsHelperOpen: deferred to 7b-3b (goroutine attribution)
