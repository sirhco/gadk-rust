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
//! Layout: `{root}/prompts/{id}` holds `name`, `labels` and `latest_version`;
//! `{root}/prompts/{id}/versions/{n}` holds each version. `id` is the prompt name with `%` and
//! `/` percent-encoded. Every write is one Firestore transaction.
//!
//! Read cost: [`load_all`](gadk_prompts::PromptStore::load_all), which every registry refresh
//! runs, reads the prompt documents (one read per prompt) and then only the version documents
//! it has not read before, each new version once, by document name. The store keeps the versions
//! it has read in memory (they are immutable), so a poll of an unchanged store costs one read
//! per prompt and nothing more. The first call after a restart reads every version once. Use a
//! dedicated non-empty root (e.g. `envs/prod`) and a refresh interval suited to the number of
//! prompts.
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
