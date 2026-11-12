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

//! Port of `internal/bms.go`: Bare Metal Solution.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use super::detector::{Detector, DetectorError};

impl Detector {
    /// Go `onBareMetalSolution`: `BMS_PROJECT_ID`, `BMS_REGION` and `BMS_INSTANCE_ID` all set
    /// and non-empty.
    pub(super) fn on_bare_metal_solution(&self) -> bool {
        todo!()
    }

    /// Go `BareMetalSolutionInstanceID`: `BMS_INSTANCE_ID`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn bare_metal_solution_instance_id(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `BareMetalSolutionCloudRegion`: `BMS_REGION`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn bare_metal_solution_cloud_region(&self) -> Result<String, DetectorError> {
        todo!()
    }

    /// Go `BareMetalSolutionProjectID`: `BMS_PROJECT_ID`.
    ///
    /// # Errors
    ///
    /// [`DetectorError::EnvVarNotFound`].
    pub fn bare_metal_solution_project_id(&self) -> Result<String, DetectorError> {
        todo!()
    }
}
