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

//! Compute Engine.

use super::detector::{Detector, DetectorError};
use super::gke::LocationType;
use crate::gcp_detector::metadata::MetadataError;

/// The metadata attribute holding the machine type.
const MACHINE_TYPE_METADATA_ATTR: &str = "instance/machine-type";

/// The metadata attribute naming the creator of the instance.
const CREATED_BY_INSTANCE_ATTR: &str = "created-by";

/// A managed instance group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManagedInstanceGroup {
    /// The group name.
    pub name: String,
    /// The group location.
    pub location: String,
    /// Whether the location is a zone or a region.
    pub location_type: LocationType,
}

impl Detector {
    /// Whether `instance/machine-type` is readable.
    pub(super) async fn on_gce(&self) -> bool {
        self.metadata.get(MACHINE_TYPE_METADATA_ATTR).await.is_ok()
    }

    /// The host type: `instance/machine-type`, untrimmed.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_host_type(&self) -> Result<String, DetectorError> {
        Ok(self.metadata.get(MACHINE_TYPE_METADATA_ATTR).await?)
    }

    /// The host ID: the instance ID.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_host_id(&self) -> Result<String, DetectorError> {
        self.instance_id().await
    }

    /// The host name: the instance name.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_host_name(&self) -> Result<String, DetectorError> {
        Ok(self.metadata.instance_name().await?)
    }

    /// The instance name.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_instance_name(&self) -> Result<String, DetectorError> {
        Ok(self.metadata.instance_name().await?)
    }

    /// The instance hostname.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn gce_instance_hostname(&self) -> Result<String, DetectorError> {
        Ok(self.metadata.hostname().await?)
    }

    /// The zone and its `country-region` prefix.
    ///
    /// # Errors
    ///
    /// [`DetectorError`], including `no zone detected from GCE metadata server` and
    /// `zone was not in the expected format: country-region-zone.  Got <zone>`.
    pub async fn gce_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError> {
        let zone = self.metadata.zone().await?;
        if zone.is_empty() {
            return Err(DetectorError::Other(
                "no zone detected from GCE metadata server".to_owned(),
            ));
        }
        let split: Vec<&str> = zone.splitn(3, '-').collect();
        if split.len() != 3 {
            return Err(DetectorError::Other(format!(
                "zone was not in the expected format: country-region-zone.  Got {zone}"
            )));
        }
        let region = split[..2].join("-");
        Ok((zone, region))
    }

    /// The managed instance group named by the `created-by` attribute, or the
    /// default (empty) group when there is none.
    ///
    /// # Errors
    ///
    /// [`DetectorError`] other than a not-defined `created-by`.
    pub async fn gce_managed_instance_group(&self) -> Result<ManagedInstanceGroup, DetectorError> {
        let created_by = match self
            .metadata
            .instance_attribute_value(CREATED_BY_INSTANCE_ATTR)
            .await
        {
            Ok(v) => v,
            Err(MetadataError::NotDefined(_)) => return Ok(ManagedInstanceGroup::default()),
            Err(e) => return Err(e.into()),
        };
        // The "created-by" key exists, but when it doesn't describe a MIG something else must
        // have created this VM.
        Ok(match_created_by_mig(&created_by).unwrap_or_default())
    }
}

/// The pattern of the `created-by` attribute of a managed instance group
/// (`^projects/[^/]+/(zones|regions)/([^/]+)/instanceGroupManagers/([^/]+)$`).
fn match_created_by_mig(created_by: &str) -> Option<ManagedInstanceGroup> {
    let parts: Vec<&str> = created_by.split('/').collect();
    let [
        "projects",
        project,
        kind,
        location,
        "instanceGroupManagers",
        name,
    ] = parts.as_slice()
    else {
        return None;
    };
    if project.is_empty() || location.is_empty() || name.is_empty() {
        return None;
    }
    let location_type = match *kind {
        "zones" => LocationType::Zone,
        "regions" => LocationType::Region,
        _ => return None,
    };
    Some(ManagedInstanceGroup {
        name: (*name).to_owned(),
        location: (*location).to_owned(),
        location_type,
    })
}
