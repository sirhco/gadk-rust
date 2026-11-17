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

//! App Engine standard and flexible.

use super::detector::{Detector, DetectorError};

/// The environment variable holding the service name.
const GAE_SERVICE_ENV: &str = "GAE_SERVICE";
/// The environment variable holding the service version.
const GAE_VERSION_ENV: &str = "GAE_VERSION";
/// The environment variable holding the service instance.
const GAE_INSTANCE_ENV: &str = "GAE_INSTANCE";
/// The environment variable holding the App Engine environment.
const GAE_ENV: &str = "GAE_ENV";
/// The `GAE_ENV` value of App Engine standard.
const GAE_STANDARD: &str = "standard";

impl Detector {
    /// Whether `GAE_ENV` is `standard`.
    pub(super) fn on_app_engine_standard(&self) -> bool {
        self.os.var(GAE_ENV).is_some_and(|env| env == GAE_STANDARD)
    }

    /// Whether `GAE_SERVICE` is set.
    pub(super) fn on_app_engine(&self) -> bool {
        self.os.var(GAE_SERVICE_ENV).is_some()
    }

    /// The service name: `GAE_SERVICE`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn app_engine_service_name(&self) -> Result<String, DetectorError> {
        self.os
            .var(GAE_SERVICE_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The service version: `GAE_VERSION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn app_engine_service_version(&self) -> Result<String, DetectorError> {
        self.os
            .var(GAE_VERSION_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The service instance: `GAE_INSTANCE`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn app_engine_service_instance(&self) -> Result<String, DetectorError> {
        self.os
            .var(GAE_INSTANCE_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The flex availability zone and region: GCE's zone and region.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn app_engine_flex_availability_zone_and_region(
        &self,
    ) -> Result<(String, String), DetectorError> {
        // The GCE metadata server is available on App Engine Flex.
        self.gce_availability_zone_and_region().await
    }

    /// The standard availability zone: the metadata zone.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn app_engine_standard_availability_zone(&self) -> Result<String, DetectorError> {
        Ok(self.metadata.zone().await?)
    }

    /// The standard cloud region: the FaaS cloud region.
    ///
    /// # Errors
    ///
    /// [`DetectorError`].
    pub async fn app_engine_standard_cloud_region(&self) -> Result<String, DetectorError> {
        self.faas_cloud_region().await
    }
}
