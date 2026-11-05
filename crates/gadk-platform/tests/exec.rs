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
use std::sync::atomic::{AtomicUsize, Ordering};

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
