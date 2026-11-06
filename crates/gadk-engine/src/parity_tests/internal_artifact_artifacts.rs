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

//! Port of Go internal/artifact/artifacts_test.go.

use std::sync::Arc;

use gadk_agent::Artifacts;
use gadk_artifact::InMemoryArtifactService;
use gadk_genai::Part;
use pretty_assertions::assert_eq;

use crate::artifacts::ScopedArtifacts;

fn artifacts() -> Arc<dyn Artifacts> {
    Arc::new(ScopedArtifacts::new(
        Arc::new(InMemoryArtifactService::new()),
        "testApp",
        "testUser",
        "testSession",
    ))
}

#[tokio::test]
async fn test_artifacts() {
    let a = artifacts();
    let part = Part::from_text("test data");
    a.save("testArtifact", part.clone())
        .await
        .expect("Save failed");
    let load = a.load("testArtifact").await.expect("Load failed");
    assert_eq!(load.part, Some(part));
    let list = a.list().await.expect("List failed");
    assert_eq!(list.file_names, vec!["testArtifact".to_string()]);
}

#[tokio::test]
async fn test_artifacts_with_load_version() {
    let a = artifacts();
    a.save("testArtifact", Part::from_text("test data"))
        .await
        .expect("Save failed");
    let part2 = Part::from_text("test data 2");
    a.save("testArtifact", part2.clone())
        .await
        .expect("Save failed");
    let load = a
        .load_version("testArtifact", 0)
        .await
        .expect("Load failed");
    assert_eq!(load.part, Some(part2));
}

#[tokio::test]
async fn test_artifacts_errors() {
    let a = artifacts();
    assert!(
        a.load("nonExistentArtifact").await.is_err(),
        "Load(nonExistentArtifact) succeeded, want error"
    );
    assert!(
        a.load_version("nonExistentArtifact", 0).await.is_err(),
        "LoadVersion(nonExistentArtifact, 0) succeeded, want error"
    );
    a.save("existsArtifact", Part::from_text("test data"))
        .await
        .expect("Save(existsArtifact) failed");
    assert!(
        a.load_version("existsArtifact", 99).await.is_err(),
        "LoadVersion(existsArtifact, 99) succeeded, want error"
    );
}
