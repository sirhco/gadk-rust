// Copyright 2026 Chris Olson
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

//! Versioned, labelled prompts for gadk agents.
//!
//! A [`PromptStore`] keeps immutable [`PromptVersion`]s and movable [`Label`]s; [`FileStore`]
//! keeps them in YAML files. A [`PromptRegistry`] serves them from a refreshing in-memory
//! snapshot, and [`PromptInstruction`] renders one as an agent instruction.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use gadk_prompts::{FileStore, Label, PromptRegistry, Selector};
//! let registry = PromptRegistry::builder(Arc::new(FileStore::new("prompts"))).load().await?;
//! let provider = registry
//!     .instruction("support-triage", Selector::Label(Label::production()))
//!     .with_fallback("You are a helpful support agent.")
//!     .into_provider();
//! # let _ = provider; Ok(()) }
//! ```

mod file;
mod metrics;
mod model;
mod provider;
mod registry;
mod rollout;
mod snapshot;
mod store;

pub use file::FileStore;
pub use model::{
    Label, NewVersion, PromptVersion, Selector, VersionMeta, content_hash, validate_name,
};
pub use provider::PromptInstruction;
pub use registry::{DEFAULT_REFRESH_INTERVAL, PromptRegistry, PromptRegistryBuilder};
pub use rollout::{Rollout, RolloutSnapshot};
pub use snapshot::{PromptEntry, PromptSnapshot};
pub use store::{PromptStore, StoreError};

#[cfg(feature = "testsuite")]
pub mod testsuite;
