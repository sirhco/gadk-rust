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

//! gadk-tools: the built-in tools that need no authentication: the Gemini native tools
//! ([`gemini`]), [`load_artifacts`], [`load_memory`], [`preload_memory`], the few-shot
//! [`example`] tool, the gating [`toolsearch`] toolset and the [`skills`] toolset. The auth wire
//! model lives in `gadk_session::toolauth`; it is re-exported here as [`toolauth`] so the
//! `gadk_tools::toolauth` path keeps working.

pub mod example;
pub mod gemini;
pub(crate) mod go_fmt;
pub mod load_artifacts;
pub mod load_memory;
pub mod preload_memory;
pub(crate) mod ranksearch;
pub mod skills;
pub use gadk_session::toolauth;
pub mod toolsearch;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
