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

//! `ParallelWorker` (struct by Task 8, `Node::run` by Task 12; port of Go
//! workflow/parallel_worker.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::Value;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::{BaseNode, Node, NodeConfig, RetryConfig};

/// Port of Go workflow.ParallelWorker: runs a wrapped node once per item of a list input.
pub struct ParallelWorker {
    pub(crate) base: BaseNode,
    pub(crate) wrapped: Arc<dyn Node>,
    pub(crate) max_concurrency: usize,
    pub(crate) retry_cfg: Option<RetryConfig>,
}

impl ParallelWorker {
    /// Port of Go NewParallelWorker. The wrapped node may not carry its own RetryConfig; the
    /// worker's own RetryConfig is taken out of `cfg` (hidden from the scheduler) and applied per
    /// item.
    pub fn new(
        name: &str,
        wrapped: Arc<dyn Node>,
        max_concurrency: usize,
        mut cfg: NodeConfig,
    ) -> Result<Arc<ParallelWorker>, WorkflowError> {
        if wrapped.config().retry_config.is_some() {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "ParallelWorker {name}: wrapped node {} cannot have RetryConfig",
                    wrapped.name()
                ),
            ));
        }
        let retry_cfg = cfg.retry_config.take();
        Ok(Arc::new(ParallelWorker {
            base: BaseNode::new(name, "", cfg),
            wrapped,
            max_concurrency,
            retry_cfg,
        }))
    }
}

impl Node for ParallelWorker {
    fn name(&self) -> &str {
        self.base.name()
    }
    fn description(&self) -> &str {
        self.base.description()
    }
    fn config(&self) -> &NodeConfig {
        self.base.config()
    }
    fn input_schema(&self) -> Option<&Value> {
        self.base.input_schema()
    }
    fn output_schema(&self) -> Option<&Value> {
        self.base.output_schema()
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    /// Implemented by Task 12 (port of Go workflow/parallel_worker.go Run); until then the node emits nothing.
    fn run(&self, _ctx: Context, _input: Value) -> EventStream {
        futures::stream::empty::<Result<Event, AgentError>>().boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
