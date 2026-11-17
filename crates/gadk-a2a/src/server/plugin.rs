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

//! The `a2a-executor` plugin that captures the invocation session.

use std::sync::Arc;

use futures::FutureExt as _;
use gadk_agent::{InvocationContext, Plugin, PluginConfig};
use gadk_session::Session;
use parking_lot::Mutex;

/// The plugin's name.
pub(crate) const EXECUTOR_PLUGIN_NAME: &str = "a2a-executor";

/// A plugin whose before-run callback records the invocation's session, so
/// [`super::ExecutorContext`] can show the callbacks its state and events.
#[derive(Clone)]
pub(crate) struct ExecutorPlugin {
    /// The plugin to install in the runner.
    pub(crate) plugin: Plugin,
    /// The invocation session: `None` until the runner ran the before-run callback.
    pub(crate) invocation_session: Arc<Mutex<Option<Arc<dyn Session>>>>,
}

impl ExecutorPlugin {
    /// A fresh plugin with no captured session. Creating it cannot fail (`gadk_agent::Plugin::new`
    /// is infallible).
    pub(crate) fn new() -> Self {
        let invocation_session: Arc<Mutex<Option<Arc<dyn Session>>>> = Arc::default();
        let captured = Arc::clone(&invocation_session);
        let plugin = Plugin::new(PluginConfig {
            name: EXECUTOR_PLUGIN_NAME.to_owned(),
            before_run_callback: Some(Arc::new(move |ic: InvocationContext| {
                *captured.lock() = Some(ic.session());
                async { Ok(None) }.boxed()
            })),
            ..Default::default()
        });
        Self {
            plugin,
            invocation_session,
        }
    }

    /// The captured invocation session, if the runner ran the before-run callback.
    pub(crate) fn session(&self) -> Option<Arc<dyn Session>> {
        self.invocation_session.lock().clone()
    }
}
