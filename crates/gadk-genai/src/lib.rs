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

//! gadk-genai: Gemini-compatible content and config types.
mod config;
mod content;
mod enums;
mod helpers;
mod live;
mod media;
mod response;
mod schema;
pub mod serde_util;
mod tool;

pub use config::*;
pub use content::*;
pub use helpers::*;
pub use live::*;
pub use media::*;
pub use response::*;
pub use schema::*;
pub use tool::*;

/// Re-export used by [`string_enum!`] so callers need no direct `serde` dependency.
#[doc(hidden)]
pub use serde as __private_serde;
