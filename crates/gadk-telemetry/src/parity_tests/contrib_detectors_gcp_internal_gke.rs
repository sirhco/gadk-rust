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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `internal/gke_test.go`. Go's
//! `httptest` Compute server is a local axum server injected with `Detector::with_compute`.

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;

use super::contrib_detectors_gcp_internal_detector::{
    FAKE_INSTANCE_ID, FakeMetadata, failing_metadata, new_fake_metadata, new_test_detector,
};
use crate::gcp_detector::LocationType;

const FAKE_CLUSTER_NAME: &str = "my-cluster";
/// Note this is different from `FAKE_ZONE`.
const FAKE_CLUSTER_LOCATION: &str = "us-central1-c";

/// Go `newGKEFakeMetadataTransport`.
async fn new_gke_fake_metadata(key_values: &[(&str, &str)]) -> FakeMetadata {
    let mut all = vec![
        ("instance/attributes/cluster-name", FAKE_CLUSTER_NAME),
        (
            "instance/attributes/cluster-location",
            FAKE_CLUSTER_LOCATION,
        ),
    ];
    all.extend_from_slice(key_values);
    new_fake_metadata(&all).await
}

/// A Compute API server answering every request with `status` and `body`, recording the
/// request paths.
async fn compute_server(status: u16, body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let paths: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&paths);
    let app = axum::Router::new().fallback(move |uri: http::Uri| {
        let seen = Arc::clone(&seen);
        async move {
            seen.lock().unwrap().push(uri.path().to_owned());
            (http::StatusCode::from_u16(status).unwrap(), body)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, paths)
}

#[tokio::test]
async fn test_gke_host_id() {
    let d = new_test_detector(&new_gke_fake_metadata(&[]).await, &[]);
    assert_eq!(d.gke_host_id().await.unwrap(), FAKE_INSTANCE_ID);
}

#[tokio::test]
async fn test_gke_host_id_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gke_host_id().await.is_err());
}

#[tokio::test]
async fn test_gke_cluster_name() {
    let d = new_test_detector(&new_gke_fake_metadata(&[]).await, &[]);
    assert_eq!(d.gke_cluster_name().await.unwrap(), FAKE_CLUSTER_NAME);
}

#[tokio::test]
async fn test_gke_cluster_name_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gke_cluster_name().await.is_err());
}

#[tokio::test]
async fn test_gke_host_type() {
    let (url, paths) = compute_server(
        200,
        r#"{"machineType":"https://www.googleapis.com/compute/v1/projects/my-project/zones/us-central1-a/machineTypes/e2-standard-4"}"#,
    )
    .await;
    let d = new_test_detector(&new_gke_fake_metadata(&[]).await, &[])
        .with_compute(reqwest::Client::new(), url);
    assert_eq!(d.gke_host_type().await.unwrap(), "e2-standard-4");
    assert_eq!(
        *paths.lock().unwrap(),
        ["/projects/my-project/zones/us-central1-a/instances/my-instance"]
    );
}

#[tokio::test]
async fn test_gke_host_type_err() {
    let (url, _) = compute_server(403, "forbidden\n").await;
    let d = new_test_detector(&new_gke_fake_metadata(&[]).await, &[])
        .with_compute(reqwest::Client::new(), url);
    let err = d.gke_host_type().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "compute instances.get returned 403 Forbidden"
    );
}

#[tokio::test]
async fn test_gke_availability_zone_or_region_zonal() {
    let d = new_test_detector(&new_gke_fake_metadata(&[]).await, &[]);
    let (location, zone_or_region) = d.gke_availability_zone_or_region().await.unwrap();
    assert_eq!(zone_or_region, LocationType::Zone);
    assert_eq!(location, "us-central1-c");
}

#[tokio::test]
async fn test_gke_availability_zone_or_region_regional() {
    let d = new_test_detector(
        &new_gke_fake_metadata(&[("instance/attributes/cluster-location", "us-central1")]).await,
        &[],
    );
    let (location, zone_or_region) = d.gke_availability_zone_or_region().await.unwrap();
    assert_eq!(zone_or_region, LocationType::Region);
    assert_eq!(location, "us-central1");
}

#[tokio::test]
async fn test_gke_availability_zone_or_region_malformed() {
    let d = new_test_detector(
        &new_gke_fake_metadata(&[("instance/attributes/cluster-location", "uscentral1c")]).await,
        &[],
    );
    let err = d.gke_availability_zone_or_region().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "unrecognized format for cluster location: uscentral1c"
    );
}

#[tokio::test]
async fn test_gke_availability_zone_or_region_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.gke_availability_zone_or_region().await.is_err());
}
