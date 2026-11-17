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

//! The executor context handed to callbacks.

use std::collections::HashMap;

use gadk_genai::Content;
use gadk_session::Event;
use serde_json::Value;

use super::plugin::ExecutorPlugin;

/// What the after-event and after-execute callbacks see of an execution.
pub trait ExecutorContext: Send + Sync {
    /// The session ID (the A2A context ID).
    fn session_id(&self) -> &str;
    /// The ADK user ID of the request.
    fn user_id(&self) -> &str;
    /// The root agent's name.
    fn agent_name(&self) -> &str;
    /// The session state; empty until the executor plugin captured the invocation session.
    fn readonly_state(&self) -> serde_json::Map<String, Value>;
    /// The session events; empty until the executor plugin captured the invocation session.
    fn events(&self) -> Vec<Event>;
    /// The converted A2A message passed to the runner.
    fn user_content(&self) -> &Content;
    /// The original A2A request context.
    fn request_context(&self) -> &a2a_server::ExecutorContext;
    /// What the before-execute callback stored (see `BeforeExecuteCallback`).
    fn extensions(&self) -> &http::Extensions;
}

/// The IDs of one execution and the metadata every event it emits carries.
#[derive(Debug, Clone, Default)]
pub(crate) struct InvocationMeta {
    pub(crate) user_id: String,
    pub(crate) session_id: String,
    pub(crate) agent_name: String,
    /// The metadata every emitted event carries: `adk_app_name`, `adk_user_id`, `adk_session_id`.
    pub(crate) event_meta: HashMap<String, Value>,
}

/// The [`ExecutorContext`] of one execution.
pub(crate) struct ExecutorContextImpl {
    meta: InvocationMeta,
    plugin: ExecutorPlugin,
    user_content: Content,
    request: a2a_server::ExecutorContext,
    extensions: http::Extensions,
}

impl ExecutorContextImpl {
    /// The context of one execution.
    pub(crate) fn new(
        meta: InvocationMeta,
        plugin: ExecutorPlugin,
        user_content: Content,
        request: a2a_server::ExecutorContext,
        extensions: http::Extensions,
    ) -> Self {
        Self {
            meta,
            plugin,
            user_content,
            request,
            extensions,
        }
    }
}

impl ExecutorContext for ExecutorContextImpl {
    fn session_id(&self) -> &str {
        &self.meta.session_id
    }

    fn user_id(&self) -> &str {
        &self.meta.user_id
    }

    fn agent_name(&self) -> &str {
        &self.meta.agent_name
    }

    fn readonly_state(&self) -> serde_json::Map<String, Value> {
        match self.plugin.session() {
            Some(session) => session.state().all().into_iter().collect(),
            None => serde_json::Map::new(),
        }
    }

    fn events(&self) -> Vec<Event> {
        match self.plugin.session() {
            Some(session) => session.events(),
            None => Vec::new(),
        }
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn request_context(&self) -> &a2a_server::ExecutorContext {
        &self.request
    }

    fn extensions(&self) -> &http::Extensions {
        &self.extensions
    }
}

/// A copy of `ctx` (`a2a_server::ExecutorContext` is not `Clone`).
pub(crate) fn clone_request(ctx: &a2a_server::ExecutorContext) -> a2a_server::ExecutorContext {
    a2a_server::ExecutorContext {
        message: ctx.message.clone(),
        task_id: ctx.task_id.clone(),
        stored_task: ctx.stored_task.clone(),
        context_id: ctx.context_id.clone(),
        metadata: ctx.metadata.clone(),
        user: ctx.user.clone(),
        service_params: ctx.service_params.clone(),
        tenant: ctx.tenant.clone(),
    }
}
