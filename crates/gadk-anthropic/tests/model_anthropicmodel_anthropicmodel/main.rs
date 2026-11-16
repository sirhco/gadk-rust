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

//! Port of ADK-Go `model/anthropicmodel/anthropicmodel_test.go` (a directory target: every
//! `.rs` file here). One module per porting task:
//! `generate`, `new_model`, `tools_thinking`, `content`,
//! `behavior`; `harness` holds the shared test helpers.

// parity: skipped TestAgentRunner_MultiTurnImageAndTool: ported in crates/gadk/tests/model_anthropicmodel_agent_runner.rs (needs the runner)

mod behavior;
mod content;
mod generate;
mod harness;
mod new_model;
mod tools_thinking;
