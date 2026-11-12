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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `internal/bms_test.go`.

use pretty_assertions::assert_eq;

use super::contrib_detectors_gcp_internal_detector::{empty_metadata, new_test_detector};

#[tokio::test]
async fn test_bare_metal_solution_instance_id() {
    let d = new_test_detector(
        &empty_metadata().await,
        &[("BMS_INSTANCE_ID", "my-host-123")],
    );
    assert_eq!(d.bare_metal_solution_instance_id().unwrap(), "my-host-123");
}

#[tokio::test]
async fn test_bare_metal_solution_instance_id_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    assert!(d.bare_metal_solution_instance_id().is_err());
}

#[tokio::test]
async fn test_bare_metal_solution_cloud_region() {
    let d = new_test_detector(&empty_metadata().await, &[("BMS_REGION", "us-central1")]);
    assert_eq!(d.bare_metal_solution_cloud_region().unwrap(), "us-central1");
}

#[tokio::test]
async fn test_bare_metal_solution_cloud_region_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    assert!(d.bare_metal_solution_cloud_region().is_err());
}

#[tokio::test]
async fn test_bare_metal_solution_project_id() {
    let d = new_test_detector(
        &empty_metadata().await,
        &[("BMS_PROJECT_ID", "my-test-project")],
    );
    assert_eq!(
        d.bare_metal_solution_project_id().unwrap(),
        "my-test-project"
    );
}

#[tokio::test]
async fn test_bare_metal_solution_project_id_err() {
    let d = new_test_detector(&empty_metadata().await, &[]);
    let err = d.bare_metal_solution_project_id().unwrap_err();
    assert_eq!(err.to_string(), "environment variable not found");
}
