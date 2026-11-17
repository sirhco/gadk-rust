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

//! A Google Cloud Storage [`ArtifactService`](gadk_artifact::ArtifactService).
//!
//! Artifacts are stored under the object names, versions, content types and metadata that ADK's
//! GCS artifact service uses (`{app}/{user}/{session}/{file}/{version}`, or
//! `{app}/{user}/user/{file}/{version}` for a `user:` file), so a gadk service and an ADK
//! service can share one bucket. Requests go to
//! the GCS JSON API through gadk's own REST transport, authenticated with Application Default
//! Credentials unless [`GcsConfig::credentials`] are given.
//!
//! ```no_run
//! # fn demo() -> Result<(), gadk_artifact::ArtifactError> {
//! use gadk_artifact_gcs::{GcsArtifactService, GcsConfig};
//! let artifacts = GcsArtifactService::new("my-bucket", GcsConfig::default())?;
//! # Ok(()) }
//! ```

mod bucket;
mod config;
mod error;
#[cfg(any(test, feature = "testutil"))]
mod fake;
mod rest;
mod service;
#[cfg(any(test, feature = "testutil"))]
#[doc(hidden)]
pub mod testutil;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use config::GcsConfig;
pub use error::{GcsError, is_object_not_exist, is_version_conflict};
#[cfg(any(test, feature = "testutil"))]
pub use fake::FakeBucket;
pub use service::GcsArtifactService;
