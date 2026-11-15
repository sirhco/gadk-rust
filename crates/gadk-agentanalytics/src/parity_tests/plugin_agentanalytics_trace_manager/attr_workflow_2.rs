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
//! group D, second half (Go line order): workflow scheduler nodes, ParallelWorker items and
//! function nodes.
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestRunner_WorkflowRootContentlessPartialStoppedAfterSiblingNodeStarts_EmitsCompleted: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_RetiredPeerTrailerDoesNotClearOpenFailingPeerError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_FunctionNodeConcurrentCachedChildIsNotRetry: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_FunctionNodeConcurrentChildDistinctCancelScopes: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_FunctionNodeReturnsBeforeUnjoinedCachedChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_RoutedNodeStartedAgainWhileRunning: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_FunctionNodeNestedChildRuns_EmitNoAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_FunctionNodeResumesChildAfterNestedRun: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_FunctionNodeRerunsChildWithoutRetryConfig_KeepsFailure: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ReportedNodeOutcomeDoesNotEndRunningRun: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_RetriedParallelNodeWithSilentBranchFailure_EmitsBranchAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_FinishTaskEndsTheRunThatMadeTheCall: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ReportedNodeOutcomeBeforeDelayedResponse_EmitsNoAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_CallsWithoutCallbacksHoldNoCompletedRuns: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_UnjoinedSubAgentCallOutlivesAfterRun: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRepro_ParallelWorkerGuardrailPinsNothing: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ReportedNodeOutcomeBeforeDelayedResponse_CallInFlight_EmitsNoAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_FinishTaskWithReusedCallIDEndsTheRunOnTheEventPath: deferred to 7b-3b (workflow node run: run_llm_agent_as_node is crate-private and node paths are reset)
