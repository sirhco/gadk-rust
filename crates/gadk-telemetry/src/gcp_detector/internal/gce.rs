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

//! Port of `internal/gce.go`: Compute Engine.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use super::detector::{Detector, DetectorError};
use super::gke::LocationType;

/// Go `ManagedInstanceGroup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedInstanceGroup {
    /// Go `Name`.
    pub name: String,
    /// Go `Location`.
    pub location: String,
    /// Go `Type`.
    pub location_type: LocationType,
}

impl Detector {
    /// Go `onGCE`: `instance/machine-type` is readable.
    pub(super) async fn on_gce(&self) -> bool {
        todo!()
    }

    /// Go `GCEHostType`: `instance/machine-type`, untrimmed.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_host_type(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GCEHostID`: the instance ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_host_id(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GCEHostName`: the instance name.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_host_name(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GCEInstanceName`: the instance name.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_instance_name(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GCEInstanceHostname`: the instance hostname.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_instance_hostname(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `GCEAvailabilityZoneAndRegion`: the zone and its `country-region` prefix.
    ///
    /// # Errors
    ///
    /// [`DetectorError`], including Go's `no zone detected from GCE metadata server` and
    /// `zone was not in the expected format: country-region-zone.  Got <zone>`.
    pub async fn gce_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError> {
        todo!()
    }

    /// Go `GCEManagedInstanceGroup`: the MIG named by the `created-by` attribute, or the
    /// default (empty) group when there is none.
    ///
    /// # Errors
    ///
    /// [`DetectorError`] other than a not-defined `created-by`.
    pub async fn gce_managed_instance_group(&self) -> Result<ManagedInstanceGroup, DetectorError> {
        todo!()
    }
}
