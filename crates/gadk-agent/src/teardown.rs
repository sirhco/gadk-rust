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

//! Per-invocation teardown registry: the Rust stand-in for Go's synchronous unwinding when a
//! consumer stops reading early.
//!
//! In ADK-Go a consumer that `break`s out of a run's iterator unwinds every nested iterator on
//! its own goroutine before the runner's deferred `RunAfterRunCallback` runs: a parallel agent
//! acknowledges the event the reader stopped on, cancels its branches and waits for each one's
//! teardown (parallelagent/agent.go:118-142), a workflow scheduler cancels its nodes and drains
//! until every node goroutine returned (workflow/scheduler.go:560), and `platform.RunTasks`
//! waits for every tool task (platform/exec.go:68). Dropping a Rust stream cannot wait, so a
//! component that owns still-running work cancels it plainly (never aborts it) and hands the
//! rest of it to [`Teardowns`] as a spawned task ([`InvocationContext::spawn_teardown`] /
//! [`InvocationContext::register_teardown`]). The runner's deferred after-run callbacks await
//! [`Teardowns::wait`] first, so the started work finishes before them, as in Go. A context
//! without a registry (a stream used outside the runner) detaches the work instead: it still
//! finishes, nobody waits for it.

use std::future::Future;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::context::InvocationContext;
use crate::fiber;

/// The still-running teardown work of one invocation (see the module docs). Shared by every
/// context derived within the invocation.
#[derive(Debug, Default)]
pub struct Teardowns {
    tasks: parking_lot::Mutex<Vec<JoinHandle<()>>>,
}

impl Teardowns {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a spawned teardown task; [`Teardowns::wait`] awaits it.
    pub fn register(&self, task: JoinHandle<()>) {
        self.tasks.lock().push(task);
    }

    /// Awaits every registered task, including tasks registered while waiting (a teardown can
    /// drop a nested component that registers its own), until none is left. A task that panicked
    /// or was aborted counts as finished.
    pub async fn wait(&self) {
        loop {
            let next = self.tasks.lock().pop();
            let Some(task) = next else { return };
            if let Err(e) = task.await
                && e.is_panic()
            {
                tracing::warn!("adk: a teardown task panicked");
            }
        }
    }

    /// Whether no teardown task is registered (finished or not).
    pub fn is_empty(&self) -> bool {
        self.tasks.lock().is_empty()
    }
}

impl InvocationContext {
    /// The invocation's teardown registry, if the runner installed one
    /// ([`crate::ContextBuilder::teardowns`]).
    pub fn teardowns(&self) -> Option<Arc<Teardowns>> {
        self.inner().shared.teardowns.clone()
    }

    /// Hands an already spawned task that finishes started work to the invocation's teardown
    /// registry; without a registry the task is detached (it still runs to completion).
    pub fn register_teardown(&self, task: JoinHandle<()>) {
        if let Some(t) = self.teardowns() {
            t.register(task);
        }
    }

    /// Spawns `work` (the rest of the started work of a component being dropped) on the current
    /// tokio runtime, in the current fiber and tracing span, and registers it
    /// ([`InvocationContext::register_teardown`]). Outside a runtime `work` is dropped (there is
    /// nothing to run it on).
    pub fn spawn_teardown<F>(&self, work: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let task = handle.spawn(fiber::current().scope(work).in_current_span());
        self.register_teardown(task);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::context::{ContextDelta, InvocationContext};

    use super::Teardowns;

    #[tokio::test]
    async fn wait_awaits_tasks_registered_while_waiting() {
        let reg = Arc::new(Teardowns::new());
        let ctx = InvocationContext::builder()
            .teardowns(reg.clone())
            .build_invocation();
        let done = Arc::new(AtomicUsize::new(0));
        let (inner_ctx, d) = (ctx.clone(), done.clone());
        ctx.spawn_teardown(async move {
            tokio::task::yield_now().await;
            let d2 = d.clone();
            // A nested teardown registered by the first one.
            inner_ctx.spawn_teardown(async move {
                tokio::task::yield_now().await;
                d2.fetch_add(1, Ordering::SeqCst);
            });
            d.fetch_add(1, Ordering::SeqCst);
        });
        reg.wait().await;
        assert_eq!(done.load(Ordering::SeqCst), 2);
        assert!(reg.is_empty());
    }

    #[tokio::test]
    async fn every_derived_context_shares_the_registry() {
        let reg = Arc::new(Teardowns::new());
        let ctx = InvocationContext::builder()
            .teardowns(reg.clone())
            .build_invocation();
        let derived = ctx.with_delta(ContextDelta {
            fresh_invocation: true,
            ..Default::default()
        });
        assert!(Arc::ptr_eq(&derived.teardowns().unwrap(), &reg));
        let (scoped, _guard) = ctx.clone().into_context().with_agent_cancel();
        assert!(Arc::ptr_eq(&scoped.teardowns().unwrap(), &reg));
    }

    #[tokio::test]
    async fn without_a_registry_the_work_is_detached_and_still_runs() {
        let ctx = InvocationContext::builder().build_invocation();
        assert!(ctx.teardowns().is_none());
        let (tx, rx) = tokio::sync::oneshot::channel();
        ctx.spawn_teardown(async move {
            let _ = tx.send(());
        });
        rx.await.unwrap();
    }
}
