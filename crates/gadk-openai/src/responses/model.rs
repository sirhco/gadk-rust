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

//! The Responses model (port of ADK-Go `internal/responses/model.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 9 fills generate_content.

use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};

use crate::client::OpenAiClient;

/// Go `FinishMessageKey` of this package: the custom-metadata key a turn that ended badly
/// reports the provider's account under; [`crate::FINISH_MESSAGE_KEY`] must equal it.
pub(crate) const FINISH_MESSAGE_KEY: &str = "openai_finish_message";

/// Go `responses.Model`: talks to the Responses API.
#[derive(Debug, Clone)]
pub(crate) struct Model {
    /// The client.
    pub(crate) client: Arc<OpenAiClient>,
    /// The model name a request is sent with when it names none.
    pub(crate) name: String,
}

impl Model {
    /// Go `responses.New`.
    pub(crate) fn new(client: Arc<OpenAiClient>, model_name: &str) -> Self {
        Self {
            client,
            name: model_name.to_string(),
        }
    }
}

impl Llm for Model {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// Go `GenerateContent`: builds the params, then one blocking call (`generate`, errors
    /// wrapped `openai: call failed: %w`, metadata and finish signal attached) or one stream
    /// (`generateStream`: terminal-event tracking, partials through the aggregator, terminal
    /// tool-call adoption, `finalizeStreamResponse`). Nothing is sent before the stream is
    /// first polled; the `http_options.timeout` deadline starts then (D7).
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        todo!("Task 9")
    }
}
