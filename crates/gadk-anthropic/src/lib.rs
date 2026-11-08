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

//! gadk-anthropic: Claude models on the Anthropic Messages API and on Vertex AI (port of
//! ADK-Go `model/anthropicmodel`), with gadk's own REST transport.
//!
//! [`AnthropicModel::new`] builds an [`Llm`](gadk_model::Llm) from a model name
//! (`claude-…`, `anthropic/claude-…`, or a Vertex resource name) and a [`ClientConfig`].

mod cache;
mod client;
mod config;
mod error;
mod model;
mod request;
mod response;
mod retry;
mod stream;
mod thinking;
mod tool_id;
mod tools;
mod validate;
mod wire;

pub use config::{
    Backend, ClientConfig, DEFAULT_MAX_TOKENS, Effort, PromptCacheConfig, PromptCacheTtl,
};
pub use error::{AnthropicError, ApiError, ErrorKind, STATUS_OVERLOADED};
pub use gadk_platform::env::{EnvProvider, MapEnv, ProcessEnv};
pub use model::AnthropicModel;
pub use response::{CACHE_CREATION_INPUT_TOKENS_METADATA_KEY, STOP_REASON_METADATA_KEY};
