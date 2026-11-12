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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `internal/detector_test.go`, plus
//! the fixtures of `internal/utils_test.go` shared by the other `internal` test ports.
//!
//! Go's `FakeMetadataTransport` is a local axum metadata server reached through
//! `MetadataClient::with_base_url`, and `FakeOSProvider` the injected environment. Go's failing
//! transport (`Err: fmt.Errorf("fake error")`) is a server answering every request with
//! `403 fake error`: like Go's error, a failure the metadata client does not retry.

use std::collections::HashMap;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::gcp_detector::metadata::MetadataClient;
use crate::gcp_detector::{Platform, internal::Detector};
use crate::test_support::env;

pub(super) const FAKE_REGION: &str = "us-central1";
pub(super) const FAKE_ZONE: &str = "us-central1-a";
pub(super) const FAKE_PROJECT_ID: &str = "my-project";
pub(super) const FAKE_PROJECT_NUMBER: &str = "1234567890";
pub(super) const FAKE_INSTANCE_ID: &str = "1234567891";
pub(super) const FAKE_INSTANCE_NAME: &str = "my-instance";
pub(super) const FAKE_INSTANCE_HOSTNAME: &str = "my-instance.us-central1-a.c.my-project.internal";
pub(super) const FAKE_INSTANCE_MACHINE_TYPE: &str = "n1-standard1";

/// Go `computeMetadataPrefix`.
const COMPUTE_METADATA_PREFIX: &str = "/computeMetadata/v1/";

/// Go `FakeMetadataTransport`: a metadata server answering `metadata` (suffix → value) with
/// 200, other paths with 404, or every request with 403 `err`.
pub(super) struct FakeMetadata {
    pub(super) url: String,
}

/// Starts a [`FakeMetadata`] server.
pub(super) async fn fake_metadata(
    metadata: HashMap<String, String>,
    err: Option<&'static str>,
) -> FakeMetadata {
    let metadata = Arc::new(metadata);
    let app = axum::Router::new().fallback(move |uri: http::Uri| {
        let metadata = Arc::clone(&metadata);
        async move {
            if let Some(err) = err {
                return (http::StatusCode::FORBIDDEN, err.to_owned());
            }
            match uri
                .path()
                .strip_prefix(COMPUTE_METADATA_PREFIX)
                .and_then(|suffix| metadata.get(suffix))
            {
                Some(content) => (http::StatusCode::OK, content.clone()),
                None => (http::StatusCode::NOT_FOUND, String::new()),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    FakeMetadata { url }
}

/// Go `newFakeMetadataTransport`: the basic GCE metadata fields, overridden or extended by
/// `key_values`.
pub(super) async fn new_fake_metadata(key_values: &[(&str, &str)]) -> FakeMetadata {
    let zone = format!("projects/{FAKE_PROJECT_NUMBER}/zones/{FAKE_ZONE}");
    let mut out: HashMap<String, String> = [
        ("project/project-id", FAKE_PROJECT_ID),
        ("project/numeric-project-id", FAKE_PROJECT_NUMBER),
        ("instance/id", FAKE_INSTANCE_ID),
        ("instance/name", FAKE_INSTANCE_NAME),
        ("instance/hostname", FAKE_INSTANCE_HOSTNAME),
        ("instance/machine-type", FAKE_INSTANCE_MACHINE_TYPE),
        ("instance/zone", zone.as_str()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (k, v) in key_values {
        out.insert((*k).to_owned(), (*v).to_owned());
    }
    fake_metadata(out, None).await
}

/// Go `&FakeMetadataTransport{}`: no metadata at all.
pub(super) async fn empty_metadata() -> FakeMetadata {
    fake_metadata(HashMap::new(), None).await
}

/// Go `&FakeMetadataTransport{Err: fmt.Errorf("fake error")}`.
pub(super) async fn failing_metadata() -> FakeMetadata {
    fake_metadata(HashMap::new(), Some("fake error")).await
}

/// Go `NewTestDetector(metadataTransport, &FakeOSProvider{Vars: vars})`.
pub(super) fn new_test_detector(metadata: &FakeMetadata, vars: &[(&str, &str)]) -> Detector {
    let os = env(vars);
    Detector::new_with(
        MetadataClient::with_base_url(Arc::clone(&os), &metadata.url),
        os,
    )
}

#[tokio::test]
async fn test_cloud_platform_app_engine_flex() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[("GAE_SERVICE", "foo")]);
    assert_eq!(d.cloud_platform().await, Platform::AppEngineFlex);
}

#[tokio::test]
async fn test_cloud_platform_app_engine_standard() {
    let d = new_test_detector(
        &new_fake_metadata(&[]).await,
        &[("GAE_SERVICE", "foo"), ("GAE_ENV", "standard")],
    );
    assert_eq!(d.cloud_platform().await, Platform::AppEngineStandard);
}

#[tokio::test]
async fn test_cloud_platform_gke() {
    let d = new_test_detector(
        &new_fake_metadata(&[("instance/attributes/cluster-location", "us-central-1a")]).await,
        &[("KUBERNETES_SERVICE_HOST", "foo")],
    );
    assert_eq!(d.cloud_platform().await, Platform::Gke);
}

#[tokio::test]
async fn test_cloud_platform_k8s_not_gke() {
    let d = new_test_detector(
        &new_fake_metadata(&[]).await,
        &[("KUBERNETES_SERVICE_HOST", "foo")],
    );
    assert_eq!(d.cloud_platform().await, Platform::Gce);
}

#[tokio::test]
async fn test_cloud_platform_unknown() {
    let d = new_test_detector(
        &failing_metadata().await,
        &[("KUBERNETES_SERVICE_HOST", "foo")],
    );
    assert_eq!(d.cloud_platform().await, Platform::UnknownPlatform);
}

#[tokio::test]
async fn test_cloud_platform_gce() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.cloud_platform().await, Platform::Gce);
}

#[tokio::test]
async fn test_cloud_platform_cloud_run() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[("K_CONFIGURATION", "foo")]);
    assert_eq!(d.cloud_platform().await, Platform::CloudRun);
}

#[tokio::test]
async fn test_cloud_platform_cloud_run_jobs() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[("CLOUD_RUN_JOB", "foo")]);
    assert_eq!(d.cloud_platform().await, Platform::CloudRunJob);
}

#[tokio::test]
async fn test_cloud_platform_cloud_run_worker_pool() {
    let d = new_test_detector(
        &new_fake_metadata(&[]).await,
        &[("CLOUD_RUN_WORKER_POOL", "foo")],
    );
    assert_eq!(d.cloud_platform().await, Platform::CloudRunWorkerPool);
}

#[tokio::test]
async fn test_cloud_platform_cloud_functions() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[("FUNCTION_TARGET", "foo")]);
    assert_eq!(d.cloud_platform().await, Platform::CloudFunctions);
}

#[tokio::test]
async fn test_cloud_platform_bare_metal_solution() {
    let d = new_test_detector(
        &failing_metadata().await,
        &[
            ("BMS_INSTANCE_ID", "foo"),
            ("BMS_PROJECT_ID", "bar"),
            ("BMS_REGION", "qux"),
        ],
    );
    assert_eq!(d.cloud_platform().await, Platform::BareMetalSolution);
}

#[tokio::test]
async fn test_project_id() {
    let d = new_test_detector(&new_fake_metadata(&[]).await, &[]);
    assert_eq!(d.project_id().await.unwrap(), FAKE_PROJECT_ID);
}

#[tokio::test]
async fn test_project_id_err() {
    let d = new_test_detector(&failing_metadata().await, &[]);
    assert!(d.project_id().await.is_err());
}
