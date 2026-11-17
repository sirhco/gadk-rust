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

//! Bare Metal Solution.

use super::detector::{Detector, DetectorError};

/// The environment variable holding the project ID.
const BMS_PROJECT_ID_ENV: &str = "BMS_PROJECT_ID";
/// The environment variable holding the region.
const BMS_REGION_ENV: &str = "BMS_REGION";
/// The environment variable holding the instance ID.
const BMS_INSTANCE_ID_ENV: &str = "BMS_INSTANCE_ID";

impl Detector {
    /// Whether `BMS_PROJECT_ID`, `BMS_REGION` and `BMS_INSTANCE_ID` are all set
    /// and non-empty.
    pub(super) fn on_bare_metal_solution(&self) -> bool {
        [BMS_PROJECT_ID_ENV, BMS_REGION_ENV, BMS_INSTANCE_ID_ENV]
            .iter()
            .all(|key| self.os.var(key).is_some_and(|v| !v.is_empty()))
    }

    /// The instance ID: `BMS_INSTANCE_ID`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn bare_metal_solution_instance_id(&self) -> Result<String, DetectorError> {
        self.os
            .var(BMS_INSTANCE_ID_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The cloud region: `BMS_REGION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn bare_metal_solution_cloud_region(&self) -> Result<String, DetectorError> {
        self.os
            .var(BMS_REGION_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }

    /// The project ID: `BMS_PROJECT_ID`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn bare_metal_solution_project_id(&self) -> Result<String, DetectorError> {
        self.os
            .var(BMS_PROJECT_ID_ENV)
            .ok_or(DetectorError::EnvVarNotFound)
    }
}
