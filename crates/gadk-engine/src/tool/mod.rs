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

//! Built-in tools: function, streaming function, agent and exit-loop tools, and the task tools.

pub mod agent_tool;
#[allow(unused_imports)]
pub use agent_tool::*;
pub mod exit_loop;
#[allow(unused_imports)]
pub use exit_loop::*;
pub mod function;
#[allow(unused_imports)]
pub use function::*;
pub mod streaming_function;
#[allow(unused_imports)]
pub use streaming_function::*;

pub(crate) mod finish_task;
pub(crate) mod single_turn;
pub(crate) mod task_agent_tool;
pub(crate) mod utils;

pub use utils::function_calls;
