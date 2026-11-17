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

//! Cloud Functions, Cloud Run services, jobs and worker pools.

use super::detector::{Detector, DetectorError};

/// The environment variable set on Cloud Functions.
const CLOUD_FUNCTIONS_TARGET_ENV: &str = "FUNCTION_TARGET";
/// The environment variable set on Cloud Run.
const CLOUD_RUN_CONFIGURATION_ENV: &str = "K_CONFIGURATION";
/// The environment variable set on Cloud Run jobs.
const CLOUD_RUN_JOBS_ENV: &str = "CLOUD_RUN_JOB";
/// The environment variable holding the FaaS service name.
const FAAS_SERVICE_ENV: &str = "K_SERVICE";
/// The environment variable holding the FaaS revision.
const FAAS_REVISION_ENV: &str = "K_REVISION";
/// The environment variable holding the Cloud Run job execution.
const CLOUD_RUN_JOB_EXECUTION_ENV: &str = "CLOUD_RUN_EXECUTION";
/// The environment variable holding the Cloud Run job task index.
const CLOUD_RUN_JOB_TASK_INDEX_ENV: &str = "CLOUD_RUN_TASK_INDEX";
/// The environment variable set on Cloud Run worker pools.
const CLOUD_RUN_WORKER_POOL_ENV: &str = "CLOUD_RUN_WORKER_POOL";
/// The environment variable holding the Cloud Run revision.
const CLOUD_RUN_REVISION_ENV: &str = "CLOUD_RUN_REVISION";
/// The metadata attribute holding the region.
const REGION_METADATA_ATTR: &str = "instance/region";

impl Detector {
    /// Whether `FUNCTION_TARGET` is set.
    pub(super) fn on_cloud_functions(&self) -> bool {
        self.os.var(CLOUD_FUNCTIONS_TARGET_ENV).is_some()
    }

    /// Whether `K_CONFIGURATION` is set.
    pub(super) fn on_cloud_run(&self) -> bool {
        self.os.var(CLOUD_RUN_CONFIGURATION_ENV).is_some()
    }

    /// Whether `CLOUD_RUN_JOB` is set.
    pub(super) fn on_cloud_run_job(&self) -> bool {
        self.os.var(CLOUD_RUN_JOBS_ENV).is_some()
    }

    /// Whether `CLOUD_RUN_WORKER_POOL` is set.
    pub(super) fn on_cloud_run_worker_pool(&self) -> bool {
        self.os.var(CLOUD_RUN_WORKER_POOL_ENV).is_some()
    }

    /// The FaaS name: `K_SERVICE`, else `CLOUD_RUN_JOB`, else `CLOUD_RUN_WORKER_POOL`.
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

    /// The FaaS version: `CLOUD_RUN_REVISION`, else `K_REVISION`.
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

    /// The Cloud Run job execution: `CLOUD_RUN_EXECUTION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn cloud_run_job_execution(&self) -> Result<String, DetectorError> {
        self.os
            .var(CLOUD_RUN_JOB_EXECUTION_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The Cloud Run job task index: `CLOUD_RUN_TASK_INDEX`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn cloud_run_job_task_index(&self) -> Result<String, DetectorError> {
        self.os
            .var(CLOUD_RUN_JOB_TASK_INDEX_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The FaaS ID: the instance ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn faas_id(&self) -> Result<String, DetectorError> {
        self.instance_id().await
    }

    /// The FaaS cloud region: the last path segment of `instance/region`.
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
