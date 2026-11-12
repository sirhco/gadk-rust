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

//! Port of `internal/detector.go`: [`Detector`], [`Platform`] and platform detection.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::fmt;
use std::sync::Arc;

use gadk_platform::env::EnvProvider;

use crate::gcp_detector::metadata::{MetadataClient, MetadataError};

/// A lookup failure; `Display` is Go's text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DetectorError {
    /// Go `errEnvVarNotFound`.
    #[error("environment variable not found")]
    EnvVarNotFound,
    /// A metadata server error.
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    /// Any other Go `errors.New` / `fmt.Errorf` text.
    #[error("{0}")]
    Other(String),
}

/// Go `Platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Go `UnknownPlatform`.
    UnknownPlatform,
    /// Go `GKE`.
    Gke,
    /// Go `GCE`.
    Gce,
    /// Go `CloudRun`.
    CloudRun,
    /// Go `CloudRunJob`.
    CloudRunJob,
    /// Go `CloudRunWorkerPool`.
    CloudRunWorkerPool,
    /// Go `CloudFunctions`.
    CloudFunctions,
    /// Go `AppEngineStandard`.
    AppEngineStandard,
    /// Go `AppEngineFlex`.
    AppEngineFlex,
    /// Go `BareMetalSolution`.
    BareMetalSolution,
}

/// Go `*Detector`: collects resource information for all GCP platforms from the environment
/// (Go `osProvider`), the metadata server and, for `GKEHostType`, the Compute API.
#[derive(Clone)]
pub struct Detector {
    pub(super) metadata: MetadataClient,
    pub(super) os: Arc<dyn EnvProvider>,
    pub(super) http_client: Option<reqwest::Client>,
    pub(super) compute_base_url: String,
}

impl Detector {
    /// Go `NewDetector`: the metadata server named by `env` and `env` itself.
    pub fn new(env: Arc<dyn EnvProvider>) -> Self {
        todo!()
    }

    /// Go `NewTestDetector`: an injected metadata client and environment.
    pub fn new_with(metadata: MetadataClient, env: Arc<dyn EnvProvider>) -> Self {
        todo!()
    }

    /// Go's test seam `d.httpClient = …; d.computeBaseURL = …`: the Compute API client (used
    /// as is, without credentials) and base URL for `GKEHostType`.
    #[must_use]
    pub fn with_compute(
        self,
        http_client: reqwest::Client,
        compute_base_url: impl Into<String>,
    ) -> Self {
        todo!()
    }

    /// Go `CloudPlatform`.
    pub async fn cloud_platform(&self) -> Platform {
        todo!()
    }

    /// Go `ProjectID`: `project/project-id`, trimmed.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn project_id(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `instanceID`: `instance/id`, trimmed.
    pub(super) async fn instance_id(&self) -> Result<String, DetectorError> {
        todo!()
    }
}

impl fmt::Debug for Detector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Detector")
            .field("metadata", &self.metadata)
            .field("compute_base_url", &self.compute_base_url)
            .finish_non_exhaustive()
    }
}
