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

use super::detector::{Detector, DetectorError};

/// Go `cloudFunctionsTargetEnv`.
const CLOUD_FUNCTIONS_TARGET_ENV: &str = "FUNCTION_TARGET";
/// Go `cloudRunConfigurationEnv`.
const CLOUD_RUN_CONFIGURATION_ENV: &str = "K_CONFIGURATION";
/// Go `cloudRunJobsEnv`.
const CLOUD_RUN_JOBS_ENV: &str = "CLOUD_RUN_JOB";
/// Go `faasServiceEnv`.
const FAAS_SERVICE_ENV: &str = "K_SERVICE";
/// Go `faasRevisionEnv`.
const FAAS_REVISION_ENV: &str = "K_REVISION";
/// Go `cloudRunJobExecutionEnv`.
const CLOUD_RUN_JOB_EXECUTION_ENV: &str = "CLOUD_RUN_EXECUTION";
/// Go `cloudRunJobTaskIndexEnv`.
const CLOUD_RUN_JOB_TASK_INDEX_ENV: &str = "CLOUD_RUN_TASK_INDEX";
/// Go `cloudRunWorkerPoolEnv`.
const CLOUD_RUN_WORKER_POOL_ENV: &str = "CLOUD_RUN_WORKER_POOL";
/// Go `cloudRunRevisionEnv`.
const CLOUD_RUN_REVISION_ENV: &str = "CLOUD_RUN_REVISION";
/// Go `regionMetadataAttr`.
const REGION_METADATA_ATTR: &str = "instance/region";

impl Detector {
    /// Go `onCloudFunctions`: `FUNCTION_TARGET` is set.
    pub(super) fn on_cloud_functions(&self) -> bool {
        self.os.var(CLOUD_FUNCTIONS_TARGET_ENV).is_some()
    }

    /// Go `onCloudRun`: `K_CONFIGURATION` is set.
    pub(super) fn on_cloud_run(&self) -> bool {
        self.os.var(CLOUD_RUN_CONFIGURATION_ENV).is_some()
    }

    /// Go `onCloudRunJob`: `CLOUD_RUN_JOB` is set.
    pub(super) fn on_cloud_run_job(&self) -> bool {
        self.os.var(CLOUD_RUN_JOBS_ENV).is_some()
    }

    /// Go `onCloudRunWorkerPool`: `CLOUD_RUN_WORKER_POOL` is set.
    pub(super) fn on_cloud_run_worker_pool(&self) -> bool {
        self.os.var(CLOUD_RUN_WORKER_POOL_ENV).is_some()
    }

    /// Go `FaaSName`: `K_SERVICE`, else `CLOUD_RUN_JOB`, else `CLOUD_RUN_WORKER_POOL`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn faas_name(&self) -> Result<String, DetectorError> {
        self.os
            .var(FAAS_SERVICE_ENV)
            .or_else(|| self.os.var(CLOUD_RUN_JOBS_ENV))
            .or_else(|| self.os.var(CLOUD_RUN_WORKER_POOL_ENV))
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// Go `FaaSVersion`: `CLOUD_RUN_REVISION`, else `K_REVISION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn faas_version(&self) -> Result<String, DetectorError> {
        self.os
            .var(CLOUD_RUN_REVISION_ENV)
            .or_else(|| self.os.var(FAAS_REVISION_ENV))
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// Go `CloudRunJobExecution`: `CLOUD_RUN_EXECUTION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn cloud_run_job_execution(&self) -> Result<String, DetectorError> {
        self.os
            .var(CLOUD_RUN_JOB_EXECUTION_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// Go `CloudRunJobTaskIndex`: `CLOUD_RUN_TASK_INDEX`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn cloud_run_job_task_index(&self) -> Result<String, DetectorError> {
        self.os
            .var(CLOUD_RUN_JOB_TASK_INDEX_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// Go `FaaSID`: the instance ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn faas_id(&self) -> Result<String, DetectorError> {
        self.instance_id().await
    }

    /// Go `FaaSCloudRegion`: the last path segment of `instance/region`.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn faas_cloud_region(&self) -> Result<String, DetectorError> {
        // Format: /projects/<project_number>/regions/<region>.
        let region = self.metadata.get(REGION_METADATA_ATTR).await?;
        Ok(match region.rfind('/') {
            Some(i) => region[i + 1..].to_owned(),
            None => region,
        })
    }
}
