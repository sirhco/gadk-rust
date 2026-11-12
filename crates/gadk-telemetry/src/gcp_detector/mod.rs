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

//! The GCP resource detector (port of `go.opentelemetry.io/contrib/detectors/gcp` v1.47.0
//! `NewDetector`, `detector.go` and `types.go`, over its `internal` package).

pub mod internal;
pub mod metadata;

use std::sync::Arc;

use async_trait::async_trait;
use gadk_platform::env::EnvProvider;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;

pub use internal::{DetectorError, LocationType, Platform};
use metadata::MetadataClient;

use crate::resource::{SCHEMA_URL, new_with_attributes};

/// Go semconv v1.43.0 attribute keys and values the detector sets.
mod semconv {
    pub(super) const CLOUD_PROVIDER: &str = "cloud.provider";
    pub(super) const CLOUD_PROVIDER_GCP: &str = "gcp";
    pub(super) const CLOUD_PLATFORM: &str = "cloud.platform";
    pub(super) const CLOUD_PLATFORM_GCP_KUBERNETES_ENGINE: &str = "gcp_kubernetes_engine";
    pub(super) const CLOUD_PLATFORM_GCP_CLOUD_RUN: &str = "gcp_cloud_run";
    pub(super) const CLOUD_PLATFORM_GCP_CLOUD_FUNCTIONS: &str = "gcp_cloud_functions";
    pub(super) const CLOUD_PLATFORM_GCP_APP_ENGINE: &str = "gcp_app_engine";
    pub(super) const CLOUD_PLATFORM_GCP_COMPUTE_ENGINE: &str = "gcp_compute_engine";
    pub(super) const CLOUD_ACCOUNT_ID: &str = "cloud.account.id";
    pub(super) const CLOUD_AVAILABILITY_ZONE: &str = "cloud.availability_zone";
    pub(super) const CLOUD_REGION: &str = "cloud.region";
    pub(super) const K8S_CLUSTER_NAME: &str = "k8s.cluster.name";
    pub(super) const HOST_ID: &str = "host.id";
    pub(super) const HOST_TYPE: &str = "host.type";
    pub(super) const HOST_NAME: &str = "host.name";
    pub(super) const FAAS_NAME: &str = "faas.name";
    pub(super) const FAAS_VERSION: &str = "faas.version";
    pub(super) const FAAS_INSTANCE: &str = "faas.instance";
    pub(super) const GCP_CLOUD_RUN_JOB_EXECUTION: &str = "gcp.cloud_run.job.execution";
    pub(super) const GCP_CLOUD_RUN_JOB_TASK_INDEX: &str = "gcp.cloud_run.job.task_index";
    pub(super) const GCP_GCE_INSTANCE_NAME: &str = "gcp.gce.instance.name";
    pub(super) const GCP_GCE_INSTANCE_HOSTNAME: &str = "gcp.gce.instance.hostname";
}

/// Go `resource.ErrPartialResource` wrapping the joined per-attribute lookup errors: the
/// detected resource holds the attributes that were found.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("partial resource: {}", .errors.join("\n"))]
pub struct PartialResource {
    /// The attributes that were found, under the detector's schema URL.
    pub resource: Resource,
    /// Each failed lookup's error text, in detection order (Go `errors.Join`).
    pub errors: Vec<String>,
}

/// Go `resource.Detector`, as `resolve_resource` consumes it.
#[async_trait]
pub trait ResourceDetector: Send + Sync {
    /// Go `Detect`: `Ok(None)` adds nothing (Go `nil, nil`); a partial detection is an error
    /// carrying the attributes found.
    async fn detect(&self) -> Result<Option<Resource>, PartialResource>;
}

/// Go `gcpDetector` (`types.go`): the attribute lookups `Detect` uses, faked by `TestDetect`.
#[async_trait]
pub trait GcpDetector: Send + Sync {
    /// Go `ProjectID`.
    async fn project_id(&self) -> Result<String, DetectorError>;
    /// Go `CloudPlatform`.
    async fn cloud_platform(&self) -> Platform;
    /// Go `GKEAvailabilityZoneOrRegion`.
    async fn gke_availability_zone_or_region(
        &self,
    ) -> Result<(String, LocationType), DetectorError>;
    /// Go `GKEClusterName`.
    async fn gke_cluster_name(&self) -> Result<String, DetectorError>;
    /// Go `GKEHostID`.
    async fn gke_host_id(&self) -> Result<String, DetectorError>;
    /// Go `FaaSName`.
    async fn faas_name(&self) -> Result<String, DetectorError>;
    /// Go `FaaSVersion`.
    async fn faas_version(&self) -> Result<String, DetectorError>;
    /// Go `FaaSID`.
    async fn faas_id(&self) -> Result<String, DetectorError>;
    /// Go `FaaSCloudRegion`.
    async fn faas_cloud_region(&self) -> Result<String, DetectorError>;
    /// Go `AppEngineFlexAvailabilityZoneAndRegion`.
    async fn app_engine_flex_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError>;
    /// Go `AppEngineStandardAvailabilityZone`.
    async fn app_engine_standard_availability_zone(&self) -> Result<String, DetectorError>;
    /// Go `AppEngineStandardCloudRegion`.
    async fn app_engine_standard_cloud_region(&self) -> Result<String, DetectorError>;
    /// Go `AppEngineServiceName`.
    async fn app_engine_service_name(&self) -> Result<String, DetectorError>;
    /// Go `AppEngineServiceVersion`.
    async fn app_engine_service_version(&self) -> Result<String, DetectorError>;
    /// Go `AppEngineServiceInstance`.
    async fn app_engine_service_instance(&self) -> Result<String, DetectorError>;
    /// Go `GCEAvailabilityZoneAndRegion`.
    async fn gce_availability_zone_and_region(&self) -> Result<(String, String), DetectorError>;
    /// Go `GCEHostType`.
    async fn gce_host_type(&self) -> Result<String, DetectorError>;
    /// Go `GCEHostID`.
    async fn gce_host_id(&self) -> Result<String, DetectorError>;
    /// Go `GCEHostName`.
    async fn gce_host_name(&self) -> Result<String, DetectorError>;
    /// Go `GCEInstanceHostname`.
    async fn gce_instance_hostname(&self) -> Result<String, DetectorError>;
    /// Go `GCEInstanceName`.
    async fn gce_instance_name(&self) -> Result<String, DetectorError>;
    /// Go `CloudRunJobExecution`.
    async fn cloud_run_job_execution(&self) -> Result<String, DetectorError>;
    /// Go `CloudRunJobTaskIndex`.
    async fn cloud_run_job_task_index(&self) -> Result<String, DetectorError>;
}

/// Go's unexported `detector`: [`GcpDetector`] lookups gated on `metadata.OnGCE()`.
pub struct Detector<D = internal::Detector> {
    detector: D,
    metadata: MetadataClient,
}

/// Go `NewDetector`: detects GCE, GKE, App Engine, Cloud Run and Cloud Functions resources,
/// reading `env` and the metadata server it names.
///
/// Does no I/O: the metadata server is first asked in [`ResourceDetector::detect`].
pub fn new_detector(env: Arc<dyn EnvProvider>) -> Detector {
    let metadata = MetadataClient::new(Arc::clone(&env));
    Detector {
        detector: internal::Detector::new_with(metadata.clone(), env),
        metadata,
    }
}

impl<D: GcpDetector> Detector<D> {
    /// A detector over `detector`'s lookups, gated on `metadata`'s `on_gce` (Go
    /// `&detector{detector: …}` in `TestDetect`).
    pub fn with_detector(detector: D, metadata: MetadataClient) -> Self {
        Self { detector, metadata }
    }
}

#[async_trait]
impl<D: GcpDetector> ResourceDetector for Detector<D> {
    async fn detect(&self) -> Result<Option<Resource>, PartialResource> {
        if !self.metadata.on_gce().await {
            return Ok(None);
        }
        let d = &self.detector;
        let mut b = ResourceBuilder::default();
        b.attr(semconv::CLOUD_PROVIDER, semconv::CLOUD_PROVIDER_GCP);
        b.add(semconv::CLOUD_ACCOUNT_ID, d.project_id().await);

        match d.cloud_platform().await {
            Platform::Gke => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_KUBERNETES_ENGINE,
                );
                b.add_zone_or_region(d.gke_availability_zone_or_region().await);
                b.add(semconv::K8S_CLUSTER_NAME, d.gke_cluster_name().await);
                b.add(semconv::HOST_ID, d.gke_host_id().await);
            }
            Platform::CloudRun => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_CLOUD_RUN,
                );
                b.add(semconv::FAAS_NAME, d.faas_name().await);
                b.add(semconv::FAAS_VERSION, d.faas_version().await);
                b.add(semconv::FAAS_INSTANCE, d.faas_id().await);
                b.add(semconv::CLOUD_REGION, d.faas_cloud_region().await);
            }
            Platform::CloudRunJob => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_CLOUD_RUN,
                );
                b.add(semconv::FAAS_NAME, d.faas_name().await);
                b.add(semconv::FAAS_INSTANCE, d.faas_id().await);
                b.add(
                    semconv::GCP_CLOUD_RUN_JOB_EXECUTION,
                    d.cloud_run_job_execution().await,
                );
                b.add_int(
                    semconv::GCP_CLOUD_RUN_JOB_TASK_INDEX,
                    d.cloud_run_job_task_index().await,
                );
                b.add(semconv::CLOUD_REGION, d.faas_cloud_region().await);
            }
            Platform::CloudFunctions => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_CLOUD_FUNCTIONS,
                );
                b.add(semconv::FAAS_NAME, d.faas_name().await);
                b.add(semconv::FAAS_VERSION, d.faas_version().await);
                b.add(semconv::FAAS_INSTANCE, d.faas_id().await);
                b.add(semconv::CLOUD_REGION, d.faas_cloud_region().await);
            }
            Platform::AppEngineFlex => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_APP_ENGINE,
                );
                b.add_zone_and_region(d.app_engine_flex_availability_zone_and_region().await);
                b.add(semconv::FAAS_NAME, d.app_engine_service_name().await);
                b.add(semconv::FAAS_VERSION, d.app_engine_service_version().await);
                b.add(
                    semconv::FAAS_INSTANCE,
                    d.app_engine_service_instance().await,
                );
            }
            Platform::AppEngineStandard => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_APP_ENGINE,
                );
                b.add(
                    semconv::CLOUD_AVAILABILITY_ZONE,
                    d.app_engine_standard_availability_zone().await,
                );
                b.add(
                    semconv::CLOUD_REGION,
                    d.app_engine_standard_cloud_region().await,
                );
                b.add(semconv::FAAS_NAME, d.app_engine_service_name().await);
                b.add(semconv::FAAS_VERSION, d.app_engine_service_version().await);
                b.add(
                    semconv::FAAS_INSTANCE,
                    d.app_engine_service_instance().await,
                );
            }
            Platform::Gce => {
                b.attr(
                    semconv::CLOUD_PLATFORM,
                    semconv::CLOUD_PLATFORM_GCP_COMPUTE_ENGINE,
                );
                b.add_zone_and_region(d.gce_availability_zone_and_region().await);
                b.add(semconv::HOST_TYPE, d.gce_host_type().await);
                b.add(semconv::HOST_ID, d.gce_host_id().await);
                b.add(semconv::HOST_NAME, d.gce_host_name().await);
                b.add(semconv::GCP_GCE_INSTANCE_NAME, d.gce_instance_name().await);
                b.add(
                    semconv::GCP_GCE_INSTANCE_HOSTNAME,
                    d.gce_instance_hostname().await,
                );
            }
            // We don't support this platform yet, so just return with what we have.
            Platform::UnknownPlatform
            | Platform::CloudRunWorkerPool
            | Platform::BareMetalSolution => {}
        }
        b.build()
    }
}

/// Go `resourceBuilder`: the attributes found and the lookup errors, in detection order.
#[derive(Default)]
struct ResourceBuilder {
    errs: Vec<String>,
    attrs: Vec<KeyValue>,
}

impl ResourceBuilder {
    fn attr(&mut self, key: &'static str, value: &'static str) {
        self.attrs.push(KeyValue::new(key, value));
    }

    /// Go `add`.
    fn add(&mut self, key: &'static str, detected: Result<String, DetectorError>) {
        match detected {
            Ok(v) => self.attrs.push(KeyValue::new(key, v)),
            Err(e) => self.errs.push(e.to_string()),
        }
    }

    /// Go `addInt`: the value parsed with `strconv.Atoi`.
    fn add_int(&mut self, key: &'static str, detected: Result<String, DetectorError>) {
        match detected {
            Ok(v) => match atoi(&v) {
                Ok(i) => self.attrs.push(KeyValue::new(key, i)),
                Err(e) => self.errs.push(e),
            },
            Err(e) => self.errs.push(e.to_string()),
        }
    }

    /// Go `addZoneAndRegion`.
    fn add_zone_and_region(&mut self, detected: Result<(String, String), DetectorError>) {
        match detected {
            Ok((zone, region)) => {
                self.attrs
                    .push(KeyValue::new(semconv::CLOUD_AVAILABILITY_ZONE, zone));
                self.attrs
                    .push(KeyValue::new(semconv::CLOUD_REGION, region));
            }
            Err(e) => self.errs.push(e.to_string()),
        }
    }

    /// Go `addZoneOrRegion`.
    fn add_zone_or_region(&mut self, detected: Result<(String, LocationType), DetectorError>) {
        match detected {
            Ok((v, LocationType::Zone)) => self
                .attrs
                .push(KeyValue::new(semconv::CLOUD_AVAILABILITY_ZONE, v)),
            Ok((v, LocationType::Region)) => {
                self.attrs.push(KeyValue::new(semconv::CLOUD_REGION, v));
            }
            // Go prints the `LocationType` integer: `UndefinedLocation` is 0.
            Ok((_, LocationType::UndefinedLocation)) => self
                .errs
                .push("location must be zone or region. Got 0".to_owned()),
            Err(e) => self.errs.push(e.to_string()),
        }
    }

    /// Go `build`: the resource under semconv's schema URL, and Go's
    /// `fmt.Errorf("%w: %w", resource.ErrPartialResource, errors.Join(errs...))` when any lookup
    /// failed.
    fn build(self) -> Result<Option<Resource>, PartialResource> {
        let resource = new_with_attributes(SCHEMA_URL, self.attrs);
        if self.errs.is_empty() {
            Ok(Some(resource))
        } else {
            Err(PartialResource {
                resource,
                errors: self.errs,
            })
        }
    }
}

/// Go `strconv.Atoi` with its error texts.
fn atoi(s: &str) -> Result<i64, String> {
    s.parse::<i64>().map_err(|e| {
        let reason = match e.kind() {
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                "value out of range"
            }
            _ => "invalid syntax",
        };
        format!(
            "strconv.Atoi: parsing {}: {reason}",
            gadk_platform::go_quote(s)
        )
    })
}

#[async_trait]
impl GcpDetector for internal::Detector {
    async fn project_id(&self) -> Result<String, DetectorError> {
        internal::Detector::project_id(self).await
    }

    async fn cloud_platform(&self) -> Platform {
        internal::Detector::cloud_platform(self).await
    }

    async fn gke_availability_zone_or_region(
        &self,
    ) -> Result<(String, LocationType), DetectorError> {
        internal::Detector::gke_availability_zone_or_region(self).await
    }

    async fn gke_cluster_name(&self) -> Result<String, DetectorError> {
        internal::Detector::gke_cluster_name(self).await
    }

    async fn gke_host_id(&self) -> Result<String, DetectorError> {
        internal::Detector::gke_host_id(self).await
    }

    async fn faas_name(&self) -> Result<String, DetectorError> {
        internal::Detector::faas_name(self)
    }

    async fn faas_version(&self) -> Result<String, DetectorError> {
        internal::Detector::faas_version(self)
    }

    async fn faas_id(&self) -> Result<String, DetectorError> {
        internal::Detector::faas_id(self).await
    }

    async fn faas_cloud_region(&self) -> Result<String, DetectorError> {
        internal::Detector::faas_cloud_region(self).await
    }

    async fn app_engine_flex_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError> {
        internal::Detector::app_engine_flex_availability_zone_and_region(self).await
    }

    async fn app_engine_standard_availability_zone(&self) -> Result<String, DetectorError> {
        internal::Detector::app_engine_standard_availability_zone(self).await
    }

    async fn app_engine_standard_cloud_region(&self) -> Result<String, DetectorError> {
        internal::Detector::app_engine_standard_cloud_region(self).await
    }

    async fn app_engine_service_name(&self) -> Result<String, DetectorError> {
        internal::Detector::app_engine_service_name(self)
    }

    async fn app_engine_service_version(&self) -> Result<String, DetectorError> {
        internal::Detector::app_engine_service_version(self)
    }

    async fn app_engine_service_instance(&self) -> Result<String, DetectorError> {
        internal::Detector::app_engine_service_instance(self)
    }

    async fn gce_availability_zone_and_region(&self) -> Result<(String, String), DetectorError> {
        internal::Detector::gce_availability_zone_and_region(self).await
    }

    async fn gce_host_type(&self) -> Result<String, DetectorError> {
        internal::Detector::gce_host_type(self).await
    }

    async fn gce_host_id(&self) -> Result<String, DetectorError> {
        internal::Detector::gce_host_id(self).await
    }

    async fn gce_host_name(&self) -> Result<String, DetectorError> {
        internal::Detector::gce_host_name(self).await
    }

    async fn gce_instance_hostname(&self) -> Result<String, DetectorError> {
        internal::Detector::gce_instance_hostname(self).await
    }

    async fn gce_instance_name(&self) -> Result<String, DetectorError> {
        internal::Detector::gce_instance_name(self).await
    }

    async fn cloud_run_job_execution(&self) -> Result<String, DetectorError> {
        internal::Detector::cloud_run_job_execution(self)
    }

    async fn cloud_run_job_task_index(&self) -> Result<String, DetectorError> {
        internal::Detector::cloud_run_job_task_index(self)
    }
}
