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

//! The shared REST transport of the provider crates (`gadk-anthropic`, `gadk-openai`): one
//! `POST` with the Stainless SDKs' retry loop (anthropic-sdk-go v1.81.0 and openai-go v3.82.0
//! `internal/requestconfig`), parameterised by a [`RetryPolicy`], a [`CallTimeout`] and a
//! per-attempt [`RequestHeaders`] provider.
//!
//! - [`RestTransport::send`] returns a [`RestReply`]: the final response with its body read
//!   (every status ≥ 400, and a non-streaming 2xx), or a streaming 2xx with its live body.
//! - Failures are a provider-neutral [`RestError`]; each provider maps it to its own error
//!   type and text.
//! - Feature `test-util` adds [`test_util`]: a scripted in-process `HttpClient` and a
//!   127.0.0.1 fake server.


mod error;
mod policy;
#[cfg(feature = "test-util")]
pub mod test_util;

pub use error::RestError;
pub use policy::{
    DEFAULT_MAX_BACKOFF, DEFAULT_MAX_RETRIES, DEFAULT_MAX_RETRY_AFTER, GoParseFloatError,
    RetryAfter, RetryAfterRule, RetryPolicy, go_parse_float, header_str, parse_retry_after,
    retry_header, should_retry, should_retry_dropped_body,
};
