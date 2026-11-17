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

use futures::future::BoxFuture;
use tokio::task::JoinSet;

/// Runs all tasks and returns after all complete.
///
/// Contract for implementations:
/// - the future returned by [`run`](Self::run) must not complete before every task has
///   completed;
/// - dropping that future before it completes cancels the tasks that are still running
///   (Rust cancellation): no task may outlive the call that started it.
#[async_trait::async_trait]
pub trait TaskRunner: Send + Sync {
    /// Runs every task and returns once all of them have completed. Dropping the returned
    /// future cancels the tasks still running.
    async fn run(&self, tasks: Vec<BoxFuture<'static, ()>>);
}

/// Default runner: one tokio task per future, held in a [`JoinSet`] so that dropping the
/// `run` future aborts them. A task panic aborts the remaining tasks, waits for them to
/// stop, and is then re-raised on the caller.
pub struct TokioTaskRunner;

#[async_trait::async_trait]
impl TaskRunner for TokioTaskRunner {
    async fn run(&self, tasks: Vec<BoxFuture<'static, ()>>) {
        let mut set = JoinSet::new();
        for t in tasks {
            set.spawn(t);
        }
        while let Some(res) = set.join_next().await {
            if let Err(e) = res
                && e.is_panic()
            {
                set.abort_all();
                // Wait for the aborted siblings to stop before re-raising.
                while set.join_next().await.is_some() {}
                std::panic::resume_unwind(e.into_panic());
            }
        }
    }
}
