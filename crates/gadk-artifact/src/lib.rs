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

//! gadk-artifact: artifact storage service (port of ADK-Go artifact/).

pub mod inmemory;
pub mod service;
#[cfg(feature = "testsuite")]
pub mod testsuite;
pub mod util;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use inmemory::InMemoryArtifactService;
pub use service::{
    ArtifactError, ArtifactService, ArtifactVersion, DeleteRequest, GetArtifactVersionRequest,
    GetArtifactVersionResponse, ListRequest, ListResponse, LoadRequest, LoadResponse, SaveRequest,
    SaveResponse, VersionsRequest, VersionsResponse,
};
