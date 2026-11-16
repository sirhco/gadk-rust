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
