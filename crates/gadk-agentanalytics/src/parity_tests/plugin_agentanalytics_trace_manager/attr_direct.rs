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
//! groups B, F and H: direct trace-manager tests with goroutine information set by hand,
//! agent-tool helper runs on the tool's goroutine, and runner-level tests that need the
//! consumer's goroutine (plus the early-stop and sequential-root runner tests that need only it).
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestRunner_AfterToolShortCircuit_EarlyStopDoesNotFail: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_StopOnToolRunHelperPartialEvent_EmitsNoAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ToolRunHelperCanceledItselfAfterPartial_EmitsAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ToolRunHelperCanceledByOuterCtxAfterPartial_EmitsAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_TwoHelpersPartialThenHandledError_EmitsCompleted: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_StopOnContentlessPartialHelper_EmitsNoAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_SequentialRootWithFailedCustomChild_EmitsErrorTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_RecordNodeCompletionAttributesOnlyUnambiguousRuns: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_RunsOnThisGoroutineAreOverOnlyAfterTheirFrame: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelToolRunsOfSilentlyFailingAgents_EmitAgentErrors: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_StoppedStreamsKeepCompletedRunsBounded: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_FailedModelRunsKeepNoLateState: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRepro_FailedModelCallsPinCompletedCache: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestLateReviewShortcutUnderInvocationSpan: deferred to 7b-3b (goroutine attribution): Go's pushAgentSpan reads the goroutine of a context with Ended (currentGoroutine), and only popToolSpan's markToolRunsDoneLocked, which marks the runs started on the tool callback's goroutine after the call, makes the shortcut sub-agent's run done so that retireLateSettledLocked retires it; with zero attribution it stays on the late stack
