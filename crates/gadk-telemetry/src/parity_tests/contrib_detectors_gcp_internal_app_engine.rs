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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `internal/app_engine_test.go`.

use pretty_assertions::assert_eq;

use super::contrib_detectors_gcp_internal_detector::{
    FAKE_REGION, FAKE_ZONE, failing_metadata, new_fake_metadata, new_test_detector,
};

#[tokio::test]
async fn test_app_engine_service_name() {
    let d = new_test_detector(
        &new_fake_metadata(&[]).await,
        &[("GAE_SERVICE", "my-service")],
    );
    assert_eq!(d.app_engine_service_name().unwrap(), "my-service");
}

#[tokio::test]
async fn test_app_engine_service_name_err() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert!(d.app_engine_service_name().is_err());
}

#[tokio::test]
async fn test_app_engine_service_version() {
    let d = new_test_detector(
        &new_fake_metadata(&[]).await,
        &[("GAE_VERSION", "my-version")],
    );
    assert_eq!(d.app_engine_service_version().unwrap(), "my-version");
}

#[tokio::test]
async fn test_app_engine_service_version_err() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert!(d.app_engine_service_version().is_err());
}

#[tokio::test]
async fn test_app_engine_service_instance() {
    let d = new_test_detector(
        &new_fake_metadata(&[]).await,
        &[("GAE_INSTANCE", "instance-123")],
    );
    assert_eq!(d.app_engine_service_instance().unwrap(), "instance-123");
}

#[tokio::test]
async fn test_app_engine_service_instance_err() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert!(d.app_engine_service_instance().is_err());
}

#[tokio::test]
async fn test_app_engine_standard_availability_zone() {
    let d = new_test_detector(&new_fake_metadata(&[("instance/zone", "us16")]).await, &[]);
    assert_eq!(
        d.app_engine_standard_availability_zone().await.unwrap(),
        "us16"
    );
}

#[tokio::test]
async fn test_app_engine_standard_availability_zone_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.app_engine_standard_availability_zone().await.is_err());
}

#[tokio::test]
async fn test_app_engine_standard_cloud_region() {
    let d = new_test_detector(
        &new_fake_metadata(&[("instance/region", "/projects/123/regions/us-central1")]).await,
        &[],
    );
    assert_eq!(
        d.app_engine_standard_cloud_region().await.unwrap(),
        "us-central1"
    );
}

#[tokio::test]
async fn test_app_engine_standard_cloud_region_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.app_engine_standard_cloud_region().await.is_err());
}

#[tokio::test]
async fn test_app_engine_flex_availability_zone_and_region() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    let (zone, region) = d
        .app_engine_flex_availability_zone_and_region()
        .await
        .unwrap();
    assert_eq!(zone, FAKE_ZONE);
    assert_eq!(region, FAKE_REGION);
}

#[tokio::test]
async fn test_app_engine_flex_availability_zone_and_region_malformed_zone() {
    let d = new_test_detector(
        &new_fake_metadata(&[("instance/zone", "us-central1")]).await,
        &[],
    );
    let err = d
        .app_engine_flex_availability_zone_and_region()
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "zone was not in the expected format: country-region-zone.  Got us-central1"
    );
}

#[tokio::test]
async fn test_app_engine_flex_availability_zone_and_region_no_zone() {
    let d = new_test_detector(&new_fake_metadata(&[("instance/zone", "")]).await, &[]);
    let err = d
        .app_engine_flex_availability_zone_and_region()
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "no zone detected from GCE metadata server");
}

#[tokio::test]
async fn test_app_engine_flex_availability_zone_and_region_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(
        d.app_engine_flex_availability_zone_and_region()
            .await
            .is_err()
    );
}
