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

//! Go `internal/configurable/conformance`: the conformance agents' callbacks, function tools
//! and node function, the agent loader, the recording types
//! (`replayplugin/recording`), the record plugin (`recordplugin`) and the replay plugin
//! (`replayplugin`). [`gonode`] holds the Go-reflection-shaped YAML encoding and decoding of
//! the recordings that both plugins share.

pub mod callbacks;
pub mod functions;
pub mod gonode;
pub mod loader;
pub mod nodes;
pub mod record;
pub mod recording;
pub mod replay;

pub use callbacks::register_callbacks;
pub use functions::register_functions;
pub use loader::{ConformanceAgentLoader, new_conformance_agent_loader};
pub use nodes::register_node_functions;
