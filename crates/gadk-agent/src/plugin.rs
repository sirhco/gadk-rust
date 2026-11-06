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

//! Plugins (port of Go `plugin/plugin.go`).

use std::sync::Arc;

use futures::FutureExt;

use crate::callback::{
    AfterAgentCallback, AfterModelCallback, AfterRunCallback, AfterToolCallback,
    BeforeAgentCallback, BeforeModelCallback, BeforeRunCallback, BeforeToolCallback, CloseFn,
    OnEventCallback, OnModelErrorCallback, OnToolErrorCallback, OnUserMessageCallback,
};

/// Go `plugin.Config`: a plugin's name and callbacks.
#[derive(Clone, Default)]
pub struct PluginConfig {
    /// The plugin's name, unique among the registered plugins.
    pub name: String,
    /// See [`OnUserMessageCallback`].
    pub on_user_message_callback: Option<OnUserMessageCallback>,
    /// See [`OnEventCallback`].
    pub on_event_callback: Option<OnEventCallback>,
    /// See [`BeforeRunCallback`].
    pub before_run_callback: Option<BeforeRunCallback>,
    /// See [`AfterRunCallback`].
    pub after_run_callback: Option<AfterRunCallback>,
    /// See [`BeforeAgentCallback`].
    pub before_agent_callback: Option<BeforeAgentCallback>,
    /// See [`AfterAgentCallback`].
    pub after_agent_callback: Option<AfterAgentCallback>,
    /// See [`BeforeModelCallback`].
    pub before_model_callback: Option<BeforeModelCallback>,
    /// See [`AfterModelCallback`].
    pub after_model_callback: Option<AfterModelCallback>,
    /// See [`OnModelErrorCallback`].
    pub on_model_error_callback: Option<OnModelErrorCallback>,
    /// See [`BeforeToolCallback`].
    pub before_tool_callback: Option<BeforeToolCallback>,
    /// See [`AfterToolCallback`].
    pub after_tool_callback: Option<AfterToolCallback>,
    /// See [`OnToolErrorCallback`].
    pub on_tool_error_callback: Option<OnToolErrorCallback>,
    /// Go `CloseFunc`: called when the plugin manager closes.
    pub close: Option<CloseFn>,
}

/// Go `plugin.Plugin`.
#[derive(Clone)]
pub struct Plugin {
    cfg: Arc<PluginConfig>,
}

impl Plugin {
    /// Go `plugin.New` (which never fails): a missing close function becomes a no-op, so `close()`
    /// is always `Some`.
    pub fn new(mut cfg: PluginConfig) -> Plugin {
        if cfg.close.is_none() {
            let noop: CloseFn = Arc::new(|| async { Ok(()) }.boxed());
            cfg.close = Some(noop);
        }
        Plugin { cfg: Arc::new(cfg) }
    }

    /// The plugin's name.
    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// The plugin's on-user-message callback.
    pub fn on_user_message_callback(&self) -> Option<OnUserMessageCallback> {
        self.cfg.on_user_message_callback.clone()
    }

    /// The plugin's on-event callback.
    pub fn on_event_callback(&self) -> Option<OnEventCallback> {
        self.cfg.on_event_callback.clone()
    }

    /// The plugin's before-run callback.
    pub fn before_run_callback(&self) -> Option<BeforeRunCallback> {
        self.cfg.before_run_callback.clone()
    }

    /// The plugin's after-run callback.
    pub fn after_run_callback(&self) -> Option<AfterRunCallback> {
        self.cfg.after_run_callback.clone()
    }

    /// The plugin's before-agent callback.
    pub fn before_agent_callback(&self) -> Option<BeforeAgentCallback> {
        self.cfg.before_agent_callback.clone()
    }

    /// The plugin's after-agent callback.
    pub fn after_agent_callback(&self) -> Option<AfterAgentCallback> {
        self.cfg.after_agent_callback.clone()
    }

    /// The plugin's before-model callback.
    pub fn before_model_callback(&self) -> Option<BeforeModelCallback> {
        self.cfg.before_model_callback.clone()
    }

    /// The plugin's after-model callback.
    pub fn after_model_callback(&self) -> Option<AfterModelCallback> {
        self.cfg.after_model_callback.clone()
    }

    /// The plugin's on-model-error callback.
    pub fn on_model_error_callback(&self) -> Option<OnModelErrorCallback> {
        self.cfg.on_model_error_callback.clone()
    }

    /// The plugin's before-tool callback.
    pub fn before_tool_callback(&self) -> Option<BeforeToolCallback> {
        self.cfg.before_tool_callback.clone()
    }

    /// The plugin's after-tool callback.
    pub fn after_tool_callback(&self) -> Option<AfterToolCallback> {
        self.cfg.after_tool_callback.clone()
    }

    /// The plugin's on-tool-error callback.
    pub fn on_tool_error_callback(&self) -> Option<OnToolErrorCallback> {
        self.cfg.on_tool_error_callback.clone()
    }

    /// The plugin's close function; always `Some` (Go `Plugin.Close`).
    pub fn close(&self) -> Option<CloseFn> {
        self.cfg.close.clone()
    }
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.cfg.name)
            .finish_non_exhaustive()
    }
}
