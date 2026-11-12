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

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub mod internal;
pub mod metadata;

use std::sync::Arc;

use async_trait::async_trait;
use gadk_platform::env::EnvProvider;
use opentelemetry_sdk::Resource;

pub use internal::{DetectorError, LocationType, Platform};
use metadata::MetadataClient;

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
pub fn new_detector(env: Arc<dyn EnvProvider>) -> Detector {
    todo!()
}

impl<D: GcpDetector> Detector<D> {
    /// A detector over `detector`'s lookups, gated on `metadata`'s `on_gce` (Go
    /// `&detector{detector: …}` in `TestDetect`).
    pub fn with_detector(detector: D, metadata: MetadataClient) -> Self {
        todo!()
    }
}

#[async_trait]
impl<D: GcpDetector> ResourceDetector for Detector<D> {
    async fn detect(&self) -> Result<Option<Resource>, PartialResource> {
        todo!()
    }
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
