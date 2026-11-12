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

//! Port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0 `detector_test.go`, plus the
//! detector's off-GCP, end-to-end and `gadk_telemetry::new` checks.
//!
//! Go's `t.Setenv("GCE_METADATA_HOST", …)` is the metadata client's injected environment.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use pretty_assertions::assert_eq;

use super::contrib_detectors_gcp_internal_detector::{
    FAKE_INSTANCE_HOSTNAME, FAKE_INSTANCE_ID, FAKE_INSTANCE_MACHINE_TYPE, FAKE_INSTANCE_NAME,
    FAKE_PROJECT_ID, FAKE_REGION, FAKE_ZONE, new_fake_metadata,
};
use crate::gcp_detector::internal::Detector as InternalDetector;
use crate::gcp_detector::metadata::{METADATA_HOST_ENV, MetadataClient};
use crate::gcp_detector::{
    Detector, DetectorError, GcpDetector, LocationType, PartialResource, Platform,
    ResourceDetector, new_detector,
};
use crate::providers::new_with;
use crate::resource::{SCHEMA_URL, new_with_attributes};
use crate::test_support::env;

/// Go `fakeGCPDetector`: fixed values, or `err` from every lookup.
#[derive(Default)]
struct FakeGcpDetector {
    err: Option<&'static str>,
    project_id: &'static str,
    cloud_platform: Option<Platform>,
    gke_availability_zone: &'static str,
    gke_region: &'static str,
    gke_cluster_name: &'static str,
    gke_host_id: &'static str,
    faas_name: &'static str,
    faas_version: &'static str,
    faas_id: &'static str,
    faas_cloud_region: &'static str,
    app_engine_availability_zone: &'static str,
    app_engine_region: &'static str,
    app_engine_service_name: &'static str,
    app_engine_service_version: &'static str,
    app_engine_service_instance: &'static str,
    gce_availability_zone: &'static str,
    gce_region: &'static str,
    gce_host_type: &'static str,
    gce_host_id: &'static str,
    gce_host_name: &'static str,
    gcp_gce_instance_name: &'static str,
    gcp_gce_instance_hostname: &'static str,
    cloud_run_job_execution: &'static str,
    cloud_run_job_task_index: &'static str,
}

impl FakeGcpDetector {
    fn value(&self, v: &'static str) -> Result<String, DetectorError> {
        match self.err {
            Some(err) => Err(DetectorError::Other(err.to_owned())),
            None => Ok(v.to_owned()),
        }
    }

    fn pair(&self, a: &'static str, b: &'static str) -> Result<(String, String), DetectorError> {
        Ok((self.value(a)?, self.value(b)?))
    }
}

#[async_trait]
impl GcpDetector for FakeGcpDetector {
    async fn project_id(&self) -> Result<String, DetectorError> {
        self.value(self.project_id)
    }
    async fn cloud_platform(&self) -> Platform {
        // Go's zero `internal.Platform` is `UnknownPlatform`.
        self.cloud_platform.unwrap_or(Platform::UnknownPlatform)
    }
    async fn gke_availability_zone_or_region(
        &self,
    ) -> Result<(String, LocationType), DetectorError> {
        if let Some(err) = self.err {
            return Err(DetectorError::Other(err.to_owned()));
        }
        if !self.gke_availability_zone.is_empty() {
            return Ok((self.gke_availability_zone.to_owned(), LocationType::Zone));
        }
        Ok((self.gke_region.to_owned(), LocationType::Region))
    }
    async fn gke_cluster_name(&self) -> Result<String, DetectorError> {
        self.value(self.gke_cluster_name)
    }
    async fn gke_host_id(&self) -> Result<String, DetectorError> {
        self.value(self.gke_host_id)
    }
    async fn faas_name(&self) -> Result<String, DetectorError> {
        self.value(self.faas_name)
    }
    async fn faas_version(&self) -> Result<String, DetectorError> {
        self.value(self.faas_version)
    }
    async fn faas_id(&self) -> Result<String, DetectorError> {
        self.value(self.faas_id)
    }
    async fn faas_cloud_region(&self) -> Result<String, DetectorError> {
        self.value(self.faas_cloud_region)
    }
    async fn app_engine_flex_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError> {
        self.pair(self.app_engine_availability_zone, self.app_engine_region)
    }
    async fn app_engine_standard_availability_zone(&self) -> Result<String, DetectorError> {
        self.value(self.app_engine_availability_zone)
    }
    async fn app_engine_standard_cloud_region(&self) -> Result<String, DetectorError> {
        self.value(self.app_engine_region)
    }
    async fn app_engine_service_name(&self) -> Result<String, DetectorError> {
        self.value(self.app_engine_service_name)
    }
    async fn app_engine_service_version(&self) -> Result<String, DetectorError> {
        self.value(self.app_engine_service_version)
    }
    async fn app_engine_service_instance(&self) -> Result<String, DetectorError> {
        self.value(self.app_engine_service_instance)
    }
    async fn gce_availability_zone_and_region(&self) -> Result<(String, String), DetectorError> {
        self.pair(self.gce_availability_zone, self.gce_region)
    }
    async fn gce_host_type(&self) -> Result<String, DetectorError> {
        self.value(self.gce_host_type)
    }
    async fn gce_host_id(&self) -> Result<String, DetectorError> {
        self.value(self.gce_host_id)
    }
    async fn gce_host_name(&self) -> Result<String, DetectorError> {
        self.value(self.gce_host_name)
    }
    async fn gce_instance_hostname(&self) -> Result<String, DetectorError> {
        self.value(self.gcp_gce_instance_hostname)
    }
    async fn gce_instance_name(&self) -> Result<String, DetectorError> {
        self.value(self.gcp_gce_instance_name)
    }
    async fn cloud_run_job_execution(&self) -> Result<String, DetectorError> {
        self.value(self.cloud_run_job_execution)
    }
    async fn cloud_run_job_task_index(&self) -> Result<String, DetectorError> {
        self.value(self.cloud_run_job_task_index)
    }
}

/// Go `&detector{detector: fake}` with `GCE_METADATA_HOST` set, so `OnGCE` is true.
fn on_gce(fake: FakeGcpDetector) -> Detector<FakeGcpDetector> {
    Detector::with_detector(
        fake,
        MetadataClient::new(env(&[(METADATA_HOST_ENV, "169.254.169.254")])),
    )
}

/// Go `resource.NewWithAttributes(semconv.SchemaURL, attrs...)`.
fn resource(attrs: &[(&'static str, &'static str)]) -> Resource {
    new_with_attributes(SCHEMA_URL, attrs.iter().map(|(k, v)| KeyValue::new(*k, *v)))
}

const PROVIDER: (&str, &str) = ("cloud.provider", "gcp");
const ACCOUNT: (&str, &str) = ("cloud.account.id", "my-project");

#[tokio::test]
async fn test_detect() {
    struct Case {
        desc: &'static str,
        detector: Detector<FakeGcpDetector>,
        expect_err: Option<&'static str>,
        expected_resource: Resource,
    }
    let cases = vec![
        Case {
            desc: "zonal GKE cluster",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::Gke),
                gke_host_id: "1472385723456792345",
                gke_cluster_name: "my-cluster",
                gke_availability_zone: "us-central1-c",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_kubernetes_engine"),
                ("k8s.cluster.name", "my-cluster"),
                ("cloud.availability_zone", "us-central1-c"),
                ("host.id", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "regional GKE cluster",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::Gke),
                gke_host_id: "1472385723456792345",
                gke_cluster_name: "my-cluster",
                gke_region: "us-central1",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_kubernetes_engine"),
                ("k8s.cluster.name", "my-cluster"),
                ("cloud.region", "us-central1"),
                ("host.id", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "GCE",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::Gce),
                gce_host_id: "1472385723456792345",
                gce_host_name: "my-gke-node-1234",
                gce_host_type: "n1-standard1",
                gce_availability_zone: "us-central1-c",
                gce_region: "us-central1",
                gcp_gce_instance_name: "my-gke-node-1234",
                gcp_gce_instance_hostname: "hostname",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_compute_engine"),
                ("host.id", "1472385723456792345"),
                ("host.name", "my-gke-node-1234"),
                ("gcp.gce.instance.name", "my-gke-node-1234"),
                ("gcp.gce.instance.hostname", "hostname"),
                ("host.type", "n1-standard1"),
                ("cloud.region", "us-central1"),
                ("cloud.availability_zone", "us-central1-c"),
            ]),
        },
        Case {
            desc: "Cloud Run",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::CloudRun),
                faas_id: "1472385723456792345",
                faas_cloud_region: "us-central1",
                faas_name: "my-service",
                faas_version: "123456",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_cloud_run"),
                ("cloud.region", "us-central1"),
                ("faas.name", "my-service"),
                ("faas.version", "123456"),
                ("faas.instance", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "Cloud Run Job",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::CloudRunJob),
                faas_id: "1472385723456792345",
                faas_cloud_region: "us-central1",
                faas_name: "my-service",
                cloud_run_job_execution: "my-service-ekdih",
                cloud_run_job_task_index: "0",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: new_with_attributes(
                SCHEMA_URL,
                [
                    KeyValue::new("cloud.provider", "gcp"),
                    KeyValue::new("cloud.account.id", "my-project"),
                    KeyValue::new("cloud.platform", "gcp_cloud_run"),
                    KeyValue::new("cloud.region", "us-central1"),
                    KeyValue::new("faas.name", "my-service"),
                    KeyValue::new("gcp.cloud_run.job.execution", "my-service-ekdih"),
                    KeyValue::new("gcp.cloud_run.job.task_index", 0_i64),
                    KeyValue::new("faas.instance", "1472385723456792345"),
                ],
            ),
        },
        Case {
            desc: "Cloud Run Job Bad Index",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::CloudRunJob),
                faas_id: "1472385723456792345",
                faas_cloud_region: "us-central1",
                faas_name: "my-service",
                cloud_run_job_execution: "my-service-ekdih",
                cloud_run_job_task_index: "bad-value",
                ..Default::default()
            }),
            expect_err: Some(
                r#"partial resource: strconv.Atoi: parsing "bad-value": invalid syntax"#,
            ),
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_cloud_run"),
                ("cloud.region", "us-central1"),
                ("faas.name", "my-service"),
                ("gcp.cloud_run.job.execution", "my-service-ekdih"),
                ("faas.instance", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "Cloud Functions",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::CloudFunctions),
                faas_id: "1472385723456792345",
                faas_cloud_region: "us-central1",
                faas_name: "my-service",
                faas_version: "123456",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_cloud_functions"),
                ("cloud.region", "us-central1"),
                ("faas.name", "my-service"),
                ("faas.version", "123456"),
                ("faas.instance", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "App Engine Flex",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::AppEngineFlex),
                app_engine_service_instance: "1472385723456792345",
                app_engine_availability_zone: "us-central1-c",
                app_engine_region: "us-central1",
                app_engine_service_name: "my-service",
                app_engine_service_version: "123456",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_app_engine"),
                ("cloud.region", "us-central1"),
                ("cloud.availability_zone", "us-central1-c"),
                ("faas.name", "my-service"),
                ("faas.version", "123456"),
                ("faas.instance", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "App Engine Standard",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::AppEngineStandard),
                app_engine_service_instance: "1472385723456792345",
                app_engine_availability_zone: "us-central1-c",
                app_engine_region: "us-central1",
                app_engine_service_name: "my-service",
                app_engine_service_version: "123456",
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[
                PROVIDER,
                ACCOUNT,
                ("cloud.platform", "gcp_app_engine"),
                ("cloud.region", "us-central1"),
                ("cloud.availability_zone", "us-central1-c"),
                ("faas.name", "my-service"),
                ("faas.version", "123456"),
                ("faas.instance", "1472385723456792345"),
            ]),
        },
        Case {
            desc: "Unknown Platform",
            detector: on_gce(FakeGcpDetector {
                project_id: "my-project",
                cloud_platform: Some(Platform::UnknownPlatform),
                ..Default::default()
            }),
            expect_err: None,
            expected_resource: resource(&[PROVIDER, ACCOUNT]),
        },
        Case {
            desc: "error",
            detector: on_gce(FakeGcpDetector {
                err: Some("failed to get metadata"),
                ..Default::default()
            }),
            expect_err: Some("partial resource: failed to get metadata"),
            expected_resource: resource(&[PROVIDER]),
        },
    ];
    for tc in cases {
        let (res, err) = match tc.detector.detect().await {
            Ok(Some(res)) => (res, None),
            Ok(None) => panic!("{}: nothing detected", tc.desc),
            Err(partial) => {
                let text = partial.to_string();
                (partial.resource, Some(text))
            }
        };
        assert_eq!(err.as_deref(), tc.expect_err, "{}", tc.desc);
        assert_eq!(
            res, tc.expected_resource,
            "{}: Resource object returned is incorrect",
            tc.desc
        );
    }
}

/// A base URL nothing listens on.
async fn unreachable_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}

/// The real lookups over a metadata server at `url`, gated on `vars`' `GCE_METADATA_HOST`.
fn real_detector(url: &str, vars: &[(&str, &str)]) -> Detector {
    let os = env(vars);
    let metadata = MetadataClient::with_base_url(Arc::clone(&os), url);
    Detector::with_detector(InternalDetector::new_with(metadata.clone(), os), metadata)
}

#[tokio::test]
async fn detect_adds_nothing_off_gcp_quickly() {
    let d = real_detector(&unreachable_url().await, &[]);
    let started = Instant::now();
    assert_eq!(d.detect().await, Ok(None));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn detect_on_gce_reads_the_metadata_server() {
    let metadata = new_fake_metadata(&[]).await;
    let d = real_detector(&metadata.url, &[(METADATA_HOST_ENV, "169.254.169.254")]);
    let want = resource(&[
        PROVIDER,
        ("cloud.account.id", FAKE_PROJECT_ID),
        ("cloud.platform", "gcp_compute_engine"),
        ("cloud.availability_zone", FAKE_ZONE),
        ("cloud.region", FAKE_REGION),
        ("host.type", FAKE_INSTANCE_MACHINE_TYPE),
        ("host.id", FAKE_INSTANCE_ID),
        ("host.name", FAKE_INSTANCE_NAME),
        ("gcp.gce.instance.name", FAKE_INSTANCE_NAME),
        ("gcp.gce.instance.hostname", FAKE_INSTANCE_HOSTNAME),
    ]);
    assert_eq!(d.detect().await, Ok(Some(want)));
}

#[tokio::test]
async fn detect_reports_gos_partial_resource_text() {
    // GKE (the cluster location is readable) without a cluster name.
    let metadata =
        new_fake_metadata(&[("instance/attributes/cluster-location", "us-central1")]).await;
    let d = real_detector(
        &metadata.url,
        &[
            (METADATA_HOST_ENV, "169.254.169.254"),
            ("KUBERNETES_SERVICE_HOST", "10.0.0.1"),
        ],
    );
    let err = d.detect().await.unwrap_err();
    assert_eq!(
        err,
        PartialResource {
            resource: resource(&[
                PROVIDER,
                ("cloud.account.id", FAKE_PROJECT_ID),
                ("cloud.platform", "gcp_kubernetes_engine"),
                ("cloud.region", "us-central1"),
                ("host.id", FAKE_INSTANCE_ID),
            ]),
            errors: vec![
                r#"metadata: GCE metadata "instance/attributes/cluster-name" not defined"#
                    .to_owned()
            ],
        }
    );
    assert_eq!(
        err.to_string(),
        r#"partial resource: metadata: GCE metadata "instance/attributes/cluster-name" not defined"#
    );
}

#[tokio::test]
async fn new_without_cloud_export_never_detects() {
    // `gadk_telemetry::new` always constructs the real detector; with cloud export off it is
    // never asked, so no metadata request is made.
    let os = env(&[]);
    let detector = new_detector(Arc::clone(&os));
    let providers = new_with(os, &detector, &[]).await.unwrap();
    assert!(providers.tracer_provider.is_none());
    assert!(providers.logger_provider.is_none());
}
