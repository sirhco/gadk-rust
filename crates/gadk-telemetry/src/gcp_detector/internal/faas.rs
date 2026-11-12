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

//! Port of `internal/faas.go`: Cloud Functions, Cloud Run services, jobs and worker pools.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use super::detector::{Detector, DetectorError};

impl Detector {
    /// Go `onCloudFunctions`: `FUNCTION_TARGET` is set.
    pub(super) fn on_cloud_functions(&self) -> bool {
        todo!()
    }

    /// Go `onCloudRun`: `K_CONFIGURATION` is set.
    pub(super) fn on_cloud_run(&self) -> bool {
        todo!()
    }

    /// Go `onCloudRunJob`: `CLOUD_RUN_JOB` is set.
    pub(super) fn on_cloud_run_job(&self) -> bool {
        todo!()
    }

    /// Go `onCloudRunWorkerPool`: `CLOUD_RUN_WORKER_POOL` is set.
    pub(super) fn on_cloud_run_worker_pool(&self) -> bool {
        todo!()
    }

    /// Go `FaaSName`: `K_SERVICE`, else `CLOUD_RUN_JOB`, else `CLOUD_RUN_WORKER_POOL`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn faas_name(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `FaaSVersion`: `CLOUD_RUN_REVISION`, else `K_REVISION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn faas_version(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `CloudRunJobExecution`: `CLOUD_RUN_EXECUTION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn cloud_run_job_execution(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `CloudRunJobTaskIndex`: `CLOUD_RUN_TASK_INDEX`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn cloud_run_job_task_index(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `FaaSID`: the instance ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn faas_id(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `FaaSCloudRegion`: the last path segment of `instance/region`.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn faas_cloud_region(&self) -> Result<String, DetectorError> {
        todo!()
    }
}
