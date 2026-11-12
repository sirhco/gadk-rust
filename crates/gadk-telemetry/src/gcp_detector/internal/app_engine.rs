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

//! Port of `internal/app_engine.go`: App Engine standard and flexible.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use super::detector::{Detector, DetectorError};

impl Detector {
    /// Go `onAppEngineStandard`: `GAE_ENV` is `standard`.
    pub(super) fn on_app_engine_standard(&self) -> bool {
        todo!()
    }

    /// Go `onAppEngine`: `GAE_SERVICE` is set.
    pub(super) fn on_app_engine(&self) -> bool {
        todo!()
    }

    /// Go `AppEngineServiceName`: `GAE_SERVICE`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn app_engine_service_name(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `AppEngineServiceVersion`: `GAE_VERSION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn app_engine_service_version(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `AppEngineServiceInstance`: `GAE_INSTANCE`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn app_engine_service_instance(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `AppEngineFlexAvailabilityZoneAndRegion`: GCE's zone and region.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn app_engine_flex_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError> {
        todo!()
    }

    /// Go `AppEngineStandardAvailabilityZone`: the metadata zone.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn app_engine_standard_availability_zone(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `AppEngineStandardCloudRegion`: `FaaSCloudRegion`.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn app_engine_standard_cloud_region(&self) -> Result<String, DetectorError> {
        todo!()
    }
}
