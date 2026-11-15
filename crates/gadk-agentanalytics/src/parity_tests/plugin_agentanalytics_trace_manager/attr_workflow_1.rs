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
//! group D, first half (Go line order): workflow scheduler nodes, ParallelWorker items and
//! function nodes.
//! Shared helpers: [`attr_support`](super::attr_support).

// parity: skipped TestTraceManager_RunnerConcurrentWorkflowRunNodeSameAgent: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_GraphSequentialSiblingAfterShortcut: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_GraphSequentialFailureDoesNotBlameShortcut: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_StopReadingMidStream_IsNotAnError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ConcurrentGraphParentsSameChildNames: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_InternallyRecoveredErrors_SingleTurnToolAndWorkflowRetry_EmitsCompletedTerminalRows: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_RetriedCompositeNodeSkipsCachedChildAgentError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_PurgedRetryAttemptTimestampPrecedesNextAttempt: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_StopOnPartialEventOfAsyncAuthor_IsNotAnError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_WorkflowNodeTimeoutAfterBeforeAgentShortcut_ConsumerStopCompletes: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_WorkflowNodeFailureWhileSiblingStreams_EmitsInvocationError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_WorkflowRetryParallelRespondedBranches_PurgesFailedAttemptAndCompletes: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ConcurrentWorkflowRetries_OrdersPurgedAgentErrorBeforeNextStarting: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_ParallelWorkerPipelineWithCachedStep_EmitsInvocationCompleted: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_WorkflowAgentRunAgainAfterForwardedNodeError_EmitsInvocationError: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestTraceManager_CrossBranchDynamicNodeRunsCachedChild: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestConcurrencyRace_ConcurrentSameAgentOnSameBranchAndPath: deferred to 7b-3b (goroutine attribution)
// parity: skipped TestRunner_WorkflowAgentWithAfterAgentCallbackDrainedAfterBranchError_EmitsInvocationError: deferred to 7b-3b (goroutine attribution)
