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
//! group G: goroutines that test agents and tools start themselves (custom fan-outs,
//! unjoined children).
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestTraceManager_ConcurrentChildrenSameBranchCachedEndInvocationRace: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_CachedChildOnDescendantBranchUnderUndeclaredSibling: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_SameOverrideBranchSiblingAndCachedChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_NestedDescendantBranchCachedChildUnderOverrideSibling: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ConcurrentSameCustomAgent_FailureNotMaskedByPeerResponse: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ConcurrentSilentCustomAgentRuns_EmitsInvocationCompleted: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ConcurrentSameLLMAgent_ModelErrorNotClearedByPeerSuccess: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ParallelLookingBranchOverrideUnderCustomParent: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_CustomParentReturnsBeforeUnjoinedChildEndsInvocation: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_SequentialAgentUnjoinedLateChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_FunctionToolReturnsBeforeUnjoinedCachedChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ToolRunCustomParentWithUnjoinedCachedChild_EmitsNoAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ToolReturnsBeforeHelperModelCallEnds_KeepsHelperOpen: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ForwardedFinishTaskEventDoesNotEndRunningChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ContentEventKeepsInFlightSubAgentModelCall: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRepro_UnjoinedCallStartedAfterLauncherReturnedMovesToLate: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_AgentStartingAcrossAfterRunStartsInLateState: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_UnjoinedStoppedStreamLeavesNoLateState: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestCustomFanOutHandlesCanceledChildWithSecondSubAgent: deferred to 7b-3b (goroutine attribution)
