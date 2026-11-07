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

//! gadk-tools: ADK-Go's built-in tools that need no authentication (port of ADK-Go
//! `tool/geminitool`, `tool/loadartifactstool`, `tool/loadmemorytool`, `tool/preloadmemorytool`,
//! `tool/exampletool`, `tool/toolsearch` with `internal/ranksearch`, and `tool/skilltoolset`).

pub mod example;
pub mod gemini;
// Callers land in later tasks (example: Task 4, skills::skill: Task 6, toolsearch: Task 9,
// skills: Task 10); Task 11 removes this allow once every function has a caller.
#[allow(dead_code)]
pub(crate) mod go_fmt;
pub mod load_artifacts;
pub mod load_memory;
pub mod preload_memory;
pub(crate) mod ranksearch;
pub mod skills;
pub mod toolsearch;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
