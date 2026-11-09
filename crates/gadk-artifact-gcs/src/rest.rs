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

//! The GCS JSON API client behind [`GcsArtifactService::new`].
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use gadk_artifact::ArtifactError;

use crate::bucket::{GcsBucket, ObjectAttrs};
use crate::config::GcsConfig;
use crate::error::GcsError;
use crate::service::GcsArtifactService;

/// The [`GcsBucket`] that talks to the GCS JSON API.
pub(crate) struct RestBucket;

impl RestBucket {
    /// The client for bucket `bucket` configured by `config`.
    pub(crate) fn from_config(bucket: &str, config: GcsConfig) -> Result<Self, ArtifactError> {
        todo!()
    }
}

#[async_trait::async_trait]
impl GcsBucket for RestBucket {
    async fn list(&self, prefix: &str, names_only: bool) -> Result<Vec<String>, GcsError> {
        todo!()
    }

    async fn insert_if_absent(
        &self,
        name: &str,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        body: Bytes,
    ) -> Result<(), GcsError> {
        todo!()
    }

    async fn attrs(&self, name: &str) -> Result<ObjectAttrs, GcsError> {
        todo!()
    }

    async fn read(&self, name: &str) -> Result<Bytes, GcsError> {
        todo!()
    }

    async fn delete(&self, name: &str) -> Result<(), GcsError> {
        todo!()
    }
}

impl GcsArtifactService {
    /// Port of Go `NewService`: a service storing artifacts in bucket `bucket`, reached as
    /// `config` says.
    ///
    /// Requests go to `{base}/storage/v1/…` and `{base}/upload/storage/v1/…`, where `base` is
    /// [`GcsConfig::endpoint`], else `http://{STORAGE_EMULATOR_HOST}` (its scheme kept when it
    /// has one) when that variable is non-empty, else `https://storage.googleapis.com`. They
    /// carry [`GcsConfig::credentials`] or Application Default Credentials, except that a
    /// non-empty `STORAGE_EMULATOR_HOST` turns authentication off (as Go's storage client does).
    ///
    /// # Errors
    ///
    /// [`ArtifactError::InvalidRequest`] for an endpoint with a path other than `/storage/v1`;
    /// [`ArtifactError::Backend`] (`failed to create gcs service: …`) when Application Default
    /// Credentials cannot be resolved.
    pub fn new(bucket: impl Into<String>, config: GcsConfig) -> Result<Self, ArtifactError> {
        let bucket = bucket.into();
        let rest = RestBucket::from_config(&bucket, config)?;
        Ok(Self::from_bucket(bucket, Arc::new(rest)))
    }
}
