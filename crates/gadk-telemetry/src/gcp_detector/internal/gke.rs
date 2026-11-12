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

//! Port of `internal/gke.go`: Kubernetes Engine, including the Compute API host type.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use super::detector::{Detector, DetectorError};

/// Go `computeInstancesGetScope`.
pub(super) const COMPUTE_INSTANCES_GET_SCOPE: &str =
    "https://www.googleapis.com/auth/compute.readonly";

/// Go `defaultComputeBaseURL`.
pub(super) const DEFAULT_COMPUTE_BASE_URL: &str = "https://compute.googleapis.com/compute/v1";

/// Go `LocationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocationType {
    /// Go `UndefinedLocation`.
    #[default]
    UndefinedLocation,
    /// Go `Zone`.
    Zone,
    /// Go `Region`.
    Region,
}

impl Detector {
    /// Go `onGKE`: `KUBERNETES_SERVICE_HOST` is set and the `cluster-location` attribute is
    /// readable.
    pub(super) async fn on_gke(&self) -> bool {
        todo!()
    }

    /// Go `GKEHostID`: GCE's host ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gke_host_id(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GKEClusterName`: the `cluster-name` attribute.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gke_cluster_name(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GKEHostType`: the instance's machine type from the Compute API `instances.get`.
    ///
    /// # Errors
    ///
    /// [`DetectorError`], including Go's `compute instances.get returned <status>`.
    pub async fn gke_host_type(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `computeInstanceMachineType`.
    async fn compute_instance_machine_type(
        &self,
        project_id: &str,
        zone: &str,
        instance_name: &str,
    ) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GKEAvailabilityZoneOrRegion`: the `cluster-location` attribute and whether it is a
    /// zone or a region.
    ///
    /// # Errors
    ///
    /// [`DetectorError`], including Go's `unrecognized format for cluster location: <loc>`.
    pub async fn gke_availability_zone_or_region(
        &self,
    ) -> Result<(String, LocationType), DetectorError> {
        todo!()
    }
}

/// Go `lastPathSegment`.
pub(super) fn last_path_segment(s: &str) -> &str {
    todo!()
}
