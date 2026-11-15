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
//! group E: ParallelAgent branches (errgroup origin and cancel cause).
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestRunner_ParallelRootStoppedMidStream_SiblingModelReturnsContextCanceled_EmitsCompletedTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelStreamStop_CanceledWorkAfterPartialIsNotAnError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_AgentCanceledSubAgentContextAfterPartial_EmitsErrorTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelBranchFailsWhileSiblingStreams_EmitsErrorTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelBranchFailsWhileSiblingStreams_StopAtErrorEmitsErrorTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelBranchFailsBeforePartialAck_EmitsErrorTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelThreeBranchesAndStreamingBranchFailure_RecordsCallerError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelRespondedBranchFailsWhileSiblingStreams_ConsistentAcrossDrainAndStop: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_CustomAgentHandlesParallelFailure_RecoveryModes: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_StopReadingOnPartialEventWithoutContent_IsNotAnError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_CachedChildStopBeforeParentRetires: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_CachedChildRetiredWhileOtherBranchToolRuns: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_CrossBranchToolRunsCachedChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_ContentlessEventCancelAndSiblingFailureEndInvocationRace: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestFailedParallelAgent_RecoveredByRootAfterAgentCallbackReportsCompleted: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestFailedParallelAgent_RecoveredByAfterAgentCallbackWithConcurrentUndeclaredSibling: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ParallelAgentUnjoinedLateChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestParallelAgentBranchFailureAfterContent_EmitsInvocationError: deferred to 7b-3b (goroutine attribution)
