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

//! gadk-openai: OpenAI models on the Responses and Chat Completions APIs, with gadk's own REST
//! transport ([`gadk_model::rest`]).
//!
//! [`new_model`] builds an [`Llm`](gadk_model::Llm) from a model name and a [`ClientConfig`];
//! [`ClientConfig::api`] selects the API ([`Api::Responses`] by default). Every
//! `GenerateContentConfig` field is translated, rejected with an error naming it, or ignored,
//! per API.
//!
//! A successful fallback from a `response.completed` conversion failure records the `tracing`
//! event `openai.terminal_response_conversion_failed` (an OpenTelemetry layer turns it into a
//! span event) for unsupported output items, unsupported message content, or invalid
//! function-call arguments. The event carries only an `error.type` category.

mod client;
mod completions;
mod config;
mod error;
mod model;
mod oaishared;
mod responses;
mod shared;

#[cfg(test)]
mod parity_tests;

pub use config::{Api, ClientConfig, DEFAULT_BASE_URL};
pub use error::{ApiError, ErrorKind, ErrorKind as OpenAiErrorKind, OpenAiError};
pub use gadk_platform::env::{EnvProvider, MapEnv, ProcessEnv};
pub use model::{FINISH_MESSAGE_KEY, new_model};
