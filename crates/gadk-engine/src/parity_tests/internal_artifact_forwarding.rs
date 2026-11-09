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

//! Port of Go internal/artifact/forwarding_test.go.

use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{AgentError, Artifacts, Context, ContextDelta};
use gadk_artifact::{
    ArtifactError, DeleteRequest, GetArtifactVersionRequest, InMemoryArtifactService, ListRequest,
    ListResponse, LoadRequest, LoadResponse, SaveRequest, SaveResponse, VersionsRequest,
};
use gadk_genai::Part;

use crate::artifacts::{ScopedArtifacts, new_forwarding_service};

/// Go forwardingParent: a tool context whose artifacts are tracked into its actions.
fn forwarding_parent() -> (Arc<dyn Artifacts>, Context) {
    let scoped: Arc<dyn Artifacts> = Arc::new(ScopedArtifacts::new(
        Arc::new(InMemoryArtifactService::new()),
        "parent_app",
        "parent_user",
        "parent_session",
    ));
    // Go NewToolContext(ctx, "call", actions, nil): a function_call_id delta gives the context its own
    // actions (ContextInner::apply), so artifact saves are tracked into artifact_delta.
    let ctx = Context::builder()
        .artifacts(scoped)
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("call".into()),
            ..Default::default()
        });
    let parent = ctx.artifacts().expect("context artifacts");
    (parent, ctx)
}

fn text(r: &LoadResponse) -> String {
    r.part.as_ref().map(|p| p.text.clone()).unwrap_or_default()
}

fn is_unsupported(e: &ArtifactError) -> bool {
    matches!(e, ArtifactError::Backend(b)
        if b.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::Unsupported))
}

// parity: adapted: request-mutation checks are moot (requests passed by value); fs.ErrNotExist → ArtifactError::NotFound
#[tokio::test]
async fn test_forwarding_service() {
    let (parent, ctx) = forwarding_parent();
    let service = new_forwarding_service(Some(parent.clone()));
    for t in ["first", "second"] {
        service
            .save(SaveRequest {
                app_name: "child_app".into(),
                user_id: "child_user".into(),
                session_id: "child_session".into(),
                file_name: "report.txt".into(),
                part: Some(Part::from_text(t)),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    assert_eq!(
        ctx.actions()
            .artifact_delta
            .unwrap_or_default()
            .get("report.txt"),
        Some(&2),
        "parent delta does not contain the latest saved version"
    );
    assert_eq!(
        text(&parent.load("report.txt").await.unwrap()),
        "second",
        "forwarded save did not reach the parent"
    );
    for (version, want) in [(0, "second"), (1, "first")] {
        let got = service
            .load(LoadRequest {
                app_name: "child_app".into(),
                user_id: "child_user".into(),
                session_id: "child_session".into(),
                file_name: "report.txt".into(),
                version,
            })
            .await
            .unwrap();
        assert_eq!(
            text(&got),
            want,
            "load did not return the requested parent version"
        );
    }
    let listed = service
        .list(ListRequest {
            app_name: "child_app".into(),
            user_id: "child_user".into(),
            session_id: "child_session".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        listed,
        ListResponse {
            file_names: vec!["report.txt".into()]
        },
        "list did not use parent scope"
    );

    let versions_req = VersionsRequest {
        app_name: "child_app".into(),
        user_id: "child_user".into(),
        session_id: "child_session".into(),
        file_name: "report.txt".into(),
    };
    let versions = service.versions(versions_req.clone()).await.unwrap();
    assert_eq!(versions.versions.len(), 2);
    assert!(
        versions.versions.contains(&1) && versions.versions.contains(&2),
        "versions did not use parent scope"
    );

    let meta = service
        .get_artifact_version(GetArtifactVersionRequest {
            app_name: "child_app".into(),
            user_id: "child_user".into(),
            session_id: "child_session".into(),
            file_name: "report.txt".into(),
            version: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        meta.artifact_version.unwrap().version,
        1,
        "metadata did not use parent scope and requested version"
    );

    service
        .delete(DeleteRequest {
            app_name: "child_app".into(),
            user_id: "child_user".into(),
            session_id: "child_session".into(),
            file_name: "report.txt".into(),
            version: 2,
        })
        .await
        .unwrap();
    assert!(
        matches!(
            parent.load_version("report.txt", 2).await,
            Err(AgentError::Artifact(ArtifactError::NotFound))
        ),
        "delete did not remove the requested parent version"
    );
    parent
        .load_version("report.txt", 1)
        .await
        .expect("delete removed another version");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_forwarding_service_parallel_saves() {
    const SAVES: usize = 64;
    let (parent, ctx) = forwarding_parent();
    let service = new_forwarding_service(Some(parent));
    let start = Arc::new(tokio::sync::Barrier::new(SAVES));
    let handles: Vec<_> = (0..SAVES)
        .map(|_| {
            let (service, start) = (service.clone(), start.clone());
            tokio::spawn(async move {
                start.wait().await;
                let res = service
                    .save(SaveRequest {
                        file_name: "report.txt".into(),
                        part: Some(Part::from_text("result")),
                        ..Default::default()
                    })
                    .await;
                assert!(res.is_ok(), "parallel save failed");
            })
        })
        .collect();
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(
        ctx.actions().artifact_delta.unwrap_or_default()["report.txt"],
        SAVES as i64,
        "parallel saves lost the latest artifact version"
    );
}

struct FailingParent;

#[async_trait]
impl Artifacts for FailingParent {
    async fn save(&self, _: &str, _: Part) -> Result<SaveResponse, AgentError> {
        Err(AgentError::other("backend failed"))
    }
    async fn list(&self) -> Result<ListResponse, AgentError> {
        Err(AgentError::other("backend failed"))
    }
    async fn load(&self, _: &str) -> Result<LoadResponse, AgentError> {
        Err(AgentError::other("latest load failed"))
    }
    async fn load_version(&self, _: &str, _: i64) -> Result<LoadResponse, AgentError> {
        Err(AgentError::other("backend failed"))
    }
}

/// What an operation's error must satisfy for a parent with a custom (backing-less) handle.
enum Want {
    Contains(&'static str),
    Unsupported,
}

// parity: adapted: errors.Is identity on a sentinel → message/kind match
#[tokio::test]
async fn test_forwarding_service_errors() {
    let cases: [(&str, Option<Arc<dyn Artifacts>>); 2] = [
        ("no parent service", None),
        (
            "custom parent",
            Some(Arc::new(FailingParent) as Arc<dyn Artifacts>),
        ),
    ];
    for (name, parent) in cases {
        let has_parent = parent.is_some();
        let service = new_forwarding_service(parent);
        let results: Vec<(&str, Want, Result<(), ArtifactError>)> = vec![
            (
                "save",
                Want::Contains("backend failed"),
                service.save(SaveRequest::default()).await.map(drop),
            ),
            (
                "load latest",
                Want::Contains("latest load failed"),
                service.load(LoadRequest::default()).await.map(drop),
            ),
            (
                "load version",
                Want::Contains("backend failed"),
                service
                    .load(LoadRequest {
                        version: 1,
                        ..Default::default()
                    })
                    .await
                    .map(drop),
            ),
            (
                "list",
                Want::Contains("backend failed"),
                service.list(ListRequest::default()).await.map(drop),
            ),
            (
                "delete",
                Want::Unsupported,
                service.delete(DeleteRequest::default()).await,
            ),
            (
                "versions",
                Want::Unsupported,
                service.versions(VersionsRequest::default()).await.map(drop),
            ),
            (
                "metadata",
                Want::Unsupported,
                service
                    .get_artifact_version(GetArtifactVersionRequest::default())
                    .await
                    .map(drop),
            ),
        ];
        for (op, want, res) in results {
            let err = res.expect_err(&format!("{name}/{op}: operation unexpectedly succeeded"));
            if has_parent {
                match want {
                    Want::Contains(s) => assert!(
                        err.to_string().contains(s),
                        "{name}/{op}: operation did not preserve the expected error: {err}"
                    ),
                    Want::Unsupported => assert!(
                        is_unsupported(&err),
                        "{name}/{op}: operation did not preserve the expected error: {err}"
                    ),
                }
            } else {
                assert!(
                    !is_unsupported(&err),
                    "{name}/{op}: missing service was treated as an unsupported operation"
                );
                assert_eq!(
                    err.to_string(),
                    "parent artifact service is not set",
                    "{name}/{op}"
                );
            }
        }
    }
}
