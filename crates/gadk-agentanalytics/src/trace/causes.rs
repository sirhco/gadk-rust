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

//! Cancellation and failure causes of span records, from ADK-Go
//! `plugin/agentanalytics/trace_manager.go`: `wasCanceled`, `failureCause`,
//! `rawContextFailureCause`, `isParallelBranch`, `contextFailureCause`, `openFailureCause`,
//! `openFailureCauseLocked`, `canceledByStop`, `collateralFailureCause`, and Go's
//! `errors.Is(err, context.Canceled)` on recorded errors.
//!
//! These walk `parent` links, so they take a [`SpanRef`] and lock each record themselves: never
//! call one while holding the guard of the record it starts from (or of any of its parents).
//! Cause mapping (R3): a cancelled token is Go's `context.Canceled`; Go's bare
//! `context.DeadlineExceeded` (agent timeout) is not distinguishable in gadk (ledgered); the
//! errgroup cause is 7b-3b.

#![allow(dead_code, unused_variables)] // Task 8 implements the bodies.

use super::TraceError;
use super::invocation::InvocationState;
use super::span::SpanRef;
use crate::error::Error;

impl SpanRef {
    /// Go `wasCanceled`.
    pub(crate) fn was_canceled(&self) -> bool {
        todo!("Task 8")
    }

    /// Go `failureCause`.
    pub(crate) fn failure_cause(&self) -> Option<TraceError> {
        todo!("Task 8")
    }

    /// Go `rawContextFailureCause`.
    pub(crate) fn raw_context_failure_cause(&self) -> Option<TraceError> {
        todo!("Task 8")
    }

    /// Go `isParallelBranch` (with zero goroutine information: the branch differs from the
    /// parent's, Go `:4982-4984`).
    pub(crate) fn is_parallel_branch(&self) -> bool {
        todo!("Task 8")
    }

    /// Go `contextFailureCause`.
    pub(crate) fn context_failure_cause(&self) -> Option<TraceError> {
        todo!("Task 8")
    }

    /// Go `openFailureCause`.
    pub(crate) fn open_failure_cause(&self) -> Option<TraceError> {
        todo!("Task 8")
    }

    /// Go `canceledByStop`.
    pub(crate) fn canceled_by_stop(&self) -> bool {
        todo!("Task 8")
    }

    /// Go `collateralFailureCause`: `(cause, collateral)`.
    pub(crate) fn collateral_failure_cause(&self) -> (Option<TraceError>, bool) {
        todo!("Task 8")
    }
}

impl InvocationState {
    /// Go `openFailureCauseLocked`.
    pub(crate) fn open_failure_cause_locked(&self) -> Option<TraceError> {
        todo!("Task 8")
    }
}

/// Go `errors.Is(err, context.Canceled)`.
pub(crate) fn is_canceled(err: &Error) -> bool {
    todo!("Task 8")
}
