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

//! The cleanup guard around the execute and cancel streams: runs Go `Executor.Cleanup` exactly
//! once (spec R4). Bodies: plan 5b Task 7.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use futures::stream::BoxStream;

use super::executor::Executor;

/// Wraps an execute or cancel stream, folds the yielded events into a task snapshot and runs
/// the executor's cleanup on the first terminal event, error, end of stream or drop.
pub(crate) struct CleanupGuard {
    inner: BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>,
}

impl CleanupGuard {
    /// Guards `inner`, an execute or cancel stream for `ctx`.
    pub(crate) fn new(
        executor: Executor,
        ctx: a2a_server::ExecutorContext,
        inner: BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>,
    ) -> Self {
        todo!("plan 5b Task 7")
    }
}

impl Stream for CleanupGuard {
    type Item = Result<a2a::StreamResponse, a2a::A2AError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!("plan 5b Task 7")
    }
}
