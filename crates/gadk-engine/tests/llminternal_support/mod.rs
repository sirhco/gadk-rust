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
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Runner harness for the Task 20 external targets: the Go tests' own
//! `session.InMemoryService()` + `Create` + `runner.New` + `r.Run` sequence.

use std::sync::Arc;

use gadk_agent::{Agent, AgentError, RunConfig, StreamingMode};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::collect_events;
use gadk_genai::Content;
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};

/// A Runner over a fresh InMemorySessionService with one pre-created session.
pub struct TestRunner {
    pub runner: Runner,
    pub user_id: String,
    pub session_id: String,
    pub session_service: Arc<dyn SessionService>,
}

impl TestRunner {
    pub async fn new(agent: Arc<dyn Agent>, app: &str, user: &str, session: &str) -> Self {
        let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        session_service
            .create(CreateRequest {
                app_name: app.into(),
                user_id: user.into(),
                session_id: Some(session.into()),
                state: None,
            })
            .await
            .unwrap();
        let runner = Runner::new(RunnerConfig {
            app_name: app.into(),
            agent,
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: Platform::default(),
            compaction: None,
        })
        .unwrap();
        Self {
            runner,
            user_id: user.into(),
            session_id: session.into(),
            session_service,
        }
    }

    /// Port of `r.Run(ctx, user, session, msg, agent.RunConfig{StreamingMode: mode})`, collected.
    pub async fn run(&self, msg: Content, mode: StreamingMode) -> Result<Vec<Event>, AgentError> {
        collect_events(self.runner.run(
            &self.user_id,
            &self.session_id,
            Some(msg),
            RunConfig {
                streaming_mode: mode,
                ..Default::default()
            },
            RunOptions::default(),
        ))
        .await
    }
}
