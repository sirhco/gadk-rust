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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `internal/faas_test.go`,
//! including the Cloud Run worker pool cases.

use pretty_assertions::assert_eq;

use super::contrib_detectors_gcp_internal_detector::{
    FAKE_INSTANCE_ID, empty_metadata, failing_metadata, new_fake_metadata, new_test_detector,
};

#[tokio::test]
async fn test_faa_s_name() {
    let d = new_test_detector(&empty_metadata().await, &[("K_SERVICE", "my-service")]);
    assert_eq!(d.faas_name().unwrap(), "my-service");
}

#[tokio::test]
async fn test_faa_s_jobs_name() {
    let d = new_test_detector(&empty_metadata().await, &[("CLOUD_RUN_JOB", "my-service")]);
    assert_eq!(d.faas_name().unwrap(), "my-service");
}

#[tokio::test]
async fn test_faa_s_worker_pool_name() {
    let d = new_test_detector(
        &empty_metadata().await,
        &[("CLOUD_RUN_WORKER_POOL", "my-service")],
    );
    assert_eq!(d.faas_name().unwrap(), "my-service");
}

#[tokio::test]
async fn test_faa_s_name_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    assert!(d.faas_name().is_err());
}

#[tokio::test]
async fn test_faa_s_version() {
    let d = new_test_detector(&empty_metadata().await, &[("K_REVISION", "version-123")]);
    assert_eq!(d.faas_version().unwrap(), "version-123");
}

#[tokio::test]
async fn test_faa_s_worker_pool_version() {
    let d = new_test_detector(
        &empty_metadata().await,
        &[
            ("CLOUD_RUN_WORKER_POOL", "my-pool"),
            ("CLOUD_RUN_REVISION", "version-123"),
        ],
    );
    assert_eq!(d.faas_version().unwrap(), "version-123");
}

#[tokio::test]
async fn test_faa_s_version_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    assert!(d.faas_version().is_err());
}

#[tokio::test]
async fn test_faa_s_job_execution() {
    let d = new_test_detector(
        &empty_metadata().await,
        &[("CLOUD_RUN_EXECUTION", "version-123")],
    );
    assert_eq!(d.cloud_run_job_execution().unwrap(), "version-123");
}

#[tokio::test]
async fn test_faa_s_job_execution_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    assert!(d.cloud_run_job_execution().is_err());
}

#[tokio::test]
async fn test_faa_s_job_task_index() {
    let d = new_test_detector(&empty_metadata().await, &[("CLOUD_RUN_TASK_INDEX", "5")]);
    assert_eq!(d.cloud_run_job_task_index().unwrap(), "5");
}

#[tokio::test]
async fn test_faa_s_job_task_index_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    assert!(d.cloud_run_job_task_index().is_err());
}

#[tokio::test]
async fn test_faa_sid() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.faas_id().await.unwrap(), FAKE_INSTANCE_ID);
}

#[tokio::test]
async fn test_faa_sid_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.faas_id().await.is_err());
}

#[tokio::test]
async fn test_faa_s_cloud_region() {
    let d = new_test_detector(
        &new_fake_metadata(&[("instance/region", "/projects/123/regions/us-central1")]).await,
        &[],
    );
    assert_eq!(d.faas_cloud_region().await.unwrap(), "us-central1");
}

#[tokio::test]
async fn test_faa_s_cloud_region_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.faas_cloud_region().await.is_err());
}
