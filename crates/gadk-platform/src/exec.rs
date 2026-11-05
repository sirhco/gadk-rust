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

/// Port of ADK-Go platform/exec.go `TaskRunner`: runs all tasks, returns after all complete.
#[async_trait::async_trait]
pub trait TaskRunner: Send + Sync {
    /// Runs every task and returns once all of them have completed.
    async fn run(&self, tasks: Vec<BoxFuture<'static, ()>>);
}

/// Default runner: one tokio task per future; a task panic is re-raised on the caller (Go crash semantics).
pub struct TokioTaskRunner;

#[async_trait::async_trait]
impl TaskRunner for TokioTaskRunner {
    async fn run(&self, tasks: Vec<BoxFuture<'static, ()>>) {
        let handles: Vec<_> = tasks.into_iter().map(tokio::spawn).collect();
        for h in handles {
            if let Err(e) = h.await
                && e.is_panic()
            {
                std::panic::resume_unwind(e.into_panic());
            }
        }
    }
}
