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

//! Port of ADK-Go `server/adka2a/v2/workflow_retry_test.go`.

#![cfg(feature = "server")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
use std::time::Duration;

use futures::StreamExt as _;
use gadk_a2a::a2a;
use gadk_a2a::a2a_server::{self, AgentExecutor as _};
use gadk_a2a::server::{Executor, ExecutorConfig};
use gadk_agent::{AgentError, Context};
use gadk_engine::runner::{PluginConfig, RunnerConfig};
use gadk_engine::workflow::{
    Edge, NodeConfig, RetryConfig, START, default_retry_config, function_node,
};
use gadk_engine::workflow_agent::{self, WorkflowAgentConfig};
use gadk_platform::Platform;
use gadk_session::InMemorySessionService;
use serde_json::Value;

#[tokio::test]
async fn test_executor_workflow_retry_outcome() {
    for exhausted in [false, true] {
        let name = if exhausted { "exhausted" } else { "recovered" };
        let calls = Arc::new(AtomicI32::new(0));
        let counted = Arc::clone(&calls);
        let node = function_node(
            "flaky",
            move |_ctx: Context, _input: Value| {
                let n = counted.fetch_add(1, SeqCst) + 1;
                async move {
                    if n == 1 || exhausted {
                        Err::<String, _>(AgentError::other("scripted failure"))
                    } else {
                        Ok("ok".to_owned())
                    }
                }
            },
            NodeConfig {
                retry_config: Some(RetryConfig {
                    max_attempts: 2,
                    initial_delay: Duration::from_millis(1),
                    ..default_retry_config()
                }),
                ..Default::default()
            },
        );
        let wf = workflow_agent::new(WorkflowAgentConfig {
            name: "wf".into(),
            edges: vec![Edge {
                from: START.clone(),
                to: node,
                route: None,
            }],
            ..Default::default()
        })
        .expect("workflow construction failed");
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(RunnerConfig {
                app_name: "wf".into(),
                agent: wf,
                session_service: Arc::new(InMemorySessionService::new()),
                artifact_service: None,
                memory_service: None,
                plugin_config: PluginConfig::default(),
                auto_create_session: false,
                platform: Platform::default(),
                compaction: None,
            }),
            ..Default::default()
        });
        let req = a2a_server::ExecutorContext {
            message: Some(a2a::Message::new(
                a2a::Role::User,
                vec![a2a::Part::text("start")],
            )),
            task_id: a2a::new_task_id(),
            stored_task: None,
            context_id: a2a::new_context_id(),
            metadata: None,
            user: None,
            service_params: HashMap::new(),
            tenant: None,
        };

        let mut final_state = a2a::TaskState::Unspecified;
        let mut events = executor.execute(req);
        while let Some(item) = events.next().await {
            let event = item
                .unwrap_or_else(|_| panic!("{name}: executor failed to dispatch workflow events"));
            if let a2a::StreamResponse::StatusUpdate(update) = event {
                final_state = update.status.state;
            }
        }
        let want = if exhausted {
            a2a::TaskState::Failed
        } else {
            a2a::TaskState::Completed
        };
        assert!(
            final_state == want && calls.load(SeqCst) == 2,
            "{name}: A2A final state did not reflect the recovered workflow outcome \
             (state {final_state:?}, calls {})",
            calls.load(SeqCst)
        );
    }
}
