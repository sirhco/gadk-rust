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

//! A [`PromptStore`](gadk_prompts::PromptStore) in Google Cloud Firestore (REST v1, Application
//! Default Credentials).
//!
//! Layout: `{root}/prompts/{id}` holds `name`, `labels`, `latest_version` and, when any label has
//! a rollout, `rollouts` (a map of label → map `{version, percent}`, both integers; absent when
//! there are none); `{root}/prompts/{id}/versions/{n}` holds each version. `id` is the prompt name
//! with `%` and `/` percent-encoded. Every write is one Firestore transaction, and every write of
//! a prompt document rewrites all of its fields: a writer from before rollouts existed drops the
//! `rollouts` field when it changes the prompt.
//!
//! Read cost: every registry refresh calls
//! [`load_all_with_rollouts`](gadk_prompts::PromptStore::load_all_with_rollouts). It reads the
//! prompt documents (one read per prompt) and then only the version documents it has not read
//! before, each new version once, by document name. The rollouts come from the same prompt
//! documents, so they add no reads, and [`load_all`](gadk_prompts::PromptStore::load_all) costs
//! the same. The store keeps the versions it has read in memory (they are immutable), so a poll
//! of an unchanged store costs one read per prompt and nothing more. The first call after a
//! restart reads every version once.
//! [`load_prompt`](gadk_prompts::PromptStore::load_prompt) reads only the one prompt document
//! (one read) and then only that prompt's versions not yet read, sharing those in-memory
//! versions with `load_all_with_rollouts`; a name that fails
//! [`validate_name`](gadk_prompts::validate_name) reads nothing. Use a dedicated non-empty root
//! (e.g. `envs/prod`) and a refresh interval suited to the number of prompts.
//!
//! ```no_run
//! # fn demo() -> Result<(), gadk_prompts::StoreError> {
//! use gadk_prompts_firestore::{FirestoreConfig, FirestoreStore};
//! let store = FirestoreStore::new(FirestoreConfig {
//!     project: "my-project".into(),
//!     ..FirestoreConfig::default()
//! })?;
//! # let _ = store; Ok(()) }
//! ```

mod client;
mod config;
mod store;
mod value;

pub use config::FirestoreConfig;
pub use store::{FirestoreStore, encode_doc_id};
