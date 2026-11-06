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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Runs the artifact conformance suite against `InMemoryArtifactService` with a deterministic
//! platform (an extra Rust target, not a parity target).

use std::sync::Arc;

use gadk_artifact::{ArtifactService, InMemoryArtifactService};
use gadk_platform::Platform;

#[tokio::test]
async fn in_memory_service_passes_conformance_suite() {
    gadk_artifact::testsuite::run(|| async {
        Arc::new(InMemoryArtifactService::with_platform(
            Platform::deterministic(7),
        )) as Arc<dyn ArtifactService>
    })
    .await;
}
