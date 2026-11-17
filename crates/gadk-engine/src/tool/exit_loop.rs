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

//! The `exit_loop` tool.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Context, RunnableTool, Tool, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::tool::{FunctionTool, FunctionToolConfig};

// Empty args struct. A plain comment: a doc comment would become the schema's `description`,
// which this empty-struct schema does not have.
#[derive(Deserialize, JsonSchema)]
struct ExitLoopArgs {}

/// A tool that sets `escalate` and `skip_summarization` on the
/// calling tool context's actions, ending the enclosing loop.
///
/// Construction can fail only if schema inference fails for
/// these fixed types, which it never does, so this signature is infallible and the impossible
/// error is surfaced at call time instead of panicking.
pub fn exit_loop() -> Arc<dyn Tool> {
    let built = FunctionTool::new(
        FunctionToolConfig::new(
            "exit_loop",
            "Exits the loop.\n\nCall this function only when you are instructed to do so.\n",
        ),
        |ctx: Context, _args: ExitLoopArgs| async move {
            ctx.update_actions(|a| {
                a.escalate = true;
                a.skip_summarization = true;
            });
            // A Rust result map cannot be nil, so the empty map stands in for "nothing": the
            // flow wraps a tool's empty result as {"result": null}.
            Ok::<_, ToolError>(BTreeMap::<String, String>::new())
        },
    );
    match built {
        Ok(t) => t,
        Err(e) => Arc::new(UnavailableExitLoop(format!(
            "error creating exit loop tool: {e}"
        ))),
    }
}

/// Stand-in returned only if FunctionTool construction failed; every run reports the
/// construction error.
struct UnavailableExitLoop(String);

impl Tool for UnavailableExitLoop {
    fn name(&self) -> &str {
        "exit_loop"
    }

    fn description(&self) -> &str {
        &self.0
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for UnavailableExitLoop {
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        Err(ToolError::Execution(self.0.clone().into()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn exit_loop_sets_escalate_and_skip_summarization() {
        let t = exit_loop();
        assert_eq!(t.name(), "exit_loop");
        assert_eq!(
            t.description(),
            "Exits the loop.\n\nCall this function only when you are instructed to do so.\n"
        );
        // A tool context that owns its actions (Go NewToolContext: a function_call_id delta
        // allocates them), so the escalate/skip_summarization writes are observable.
        let ctx = Context::builder()
            .build()
            .with_delta(gadk_agent::ContextDelta {
                function_call_id: Some("call".into()),
                ..Default::default()
            });
        let out = t
            .as_runnable()
            .unwrap()
            .run(ctx.clone(), json!({}))
            .await
            .unwrap();
        assert!(out.is_empty());
        let actions = ctx.actions();
        assert!(actions.escalate && actions.skip_summarization);
    }
}
