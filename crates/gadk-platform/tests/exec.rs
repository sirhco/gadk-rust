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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_platform::{BoxFuture, Platform, TaskRunner};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

fn counting_tasks(n: usize, counter: &Arc<AtomicUsize>) -> Vec<BoxFuture<'static, ()>> {
    (0..n)
        .map(|_| {
            let c = counter.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            }) as BoxFuture<'static, ()>
        })
        .collect()
}

#[tokio::test]
async fn test_run_tasks_default_runs_all_tasks() {
    let c = Arc::new(AtomicUsize::new(0));
    Platform::default().run_tasks(counting_tasks(10, &c)).await;
    assert_eq!(c.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn test_run_tasks_empty_is_no_op() {
    Platform::default().run_tasks(Vec::new()).await;
}

struct SerialRunner(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl TaskRunner for SerialRunner {
    async fn run(&self, tasks: Vec<BoxFuture<'static, ()>>) {
        self.0.fetch_add(1, Ordering::SeqCst);
        for t in tasks {
            t.await;
        }
    }
}

#[tokio::test]
async fn test_with_task_runner_is_used() {
    let used = Arc::new(AtomicUsize::new(0));
    let c = Arc::new(AtomicUsize::new(0));
    Platform::default()
        .with_task_runner(SerialRunner(used.clone()))
        .run_tasks(counting_tasks(3, &c))
        .await;
    assert_eq!(
        (used.load(Ordering::SeqCst), c.load(Ordering::SeqCst)),
        (1, 3)
    );
}

/// Sets its flag when dropped (that is, when the task future holding it is dropped).
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A task that sets `ran` after `delay`, holding a guard that sets `dropped` when the
/// task future is dropped.
fn slow_task(
    delay: Duration,
    ran: &Arc<AtomicBool>,
    dropped: &Arc<AtomicBool>,
) -> BoxFuture<'static, ()> {
    let ran = ran.clone();
    let guard = DropFlag(dropped.clone());
    Box::pin(async move {
        let _guard = guard;
        tokio::time::sleep(delay).await;
        ran.store(true, Ordering::SeqCst);
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_run_tasks_drop_cancels_tasks() {
    let ran = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let platform = Platform::default();
    let tasks = vec![slow_task(Duration::from_millis(200), &ran, &dropped)];
    // The run future is dropped when the timeout fires, long before the task finishes.
    let r = tokio::time::timeout(Duration::from_millis(20), platform.run_tasks(tasks)).await;
    assert!(r.is_err(), "run_tasks completed before the timeout");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!ran.load(Ordering::SeqCst), "task kept running after drop");
    assert!(
        dropped.load(Ordering::SeqCst),
        "task future was not dropped"
    );
}

async fn panicking_task() {
    panic!("task panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_run_tasks_panic_aborts_siblings() {
    let ran = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let tasks = vec![
        Box::pin(panicking_task()) as BoxFuture<'static, ()>,
        slow_task(Duration::from_millis(200), &ran, &dropped),
    ];
    let joined = tokio::spawn(async move { Platform::default().run_tasks(tasks).await }).await;
    let err = joined.expect_err("the task panic is re-raised on the caller");
    assert!(err.is_panic());
    // The sibling was aborted and had stopped by the time the panic was re-raised.
    assert!(dropped.load(Ordering::SeqCst), "sibling not aborted");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !ran.load(Ordering::SeqCst),
        "sibling kept running after the panic"
    );
}
