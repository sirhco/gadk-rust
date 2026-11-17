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

//! Scoped artifacts and the forwarding artifact service for agent contexts.

use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{AgentError, ArtifactBacking, Artifacts};
use gadk_artifact::{
    ArtifactError, ArtifactService, DeleteRequest, GetArtifactVersionRequest,
    GetArtifactVersionResponse, ListRequest, ListResponse, LoadRequest, LoadResponse, SaveRequest,
    SaveResponse, VersionsRequest, VersionsResponse,
};
use gadk_genai::Part;

/// An [`Artifacts`] scoped to one app/user/session.
#[derive(Clone)]
pub(crate) struct ScopedArtifacts {
    service: Arc<dyn ArtifactService>,
    app_name: String,
    user_id: String,
    session_id: String,
}

impl ScopedArtifacts {
    /// Scopes `service` to `app`, `user` and `session`.
    pub(crate) fn new(
        service: Arc<dyn ArtifactService>,
        app: &str,
        user: &str,
        session: &str,
    ) -> Self {
        Self {
            service,
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
        }
    }
}

#[async_trait]
impl Artifacts for ScopedArtifacts {
    /// Saves `data` under `name` in this scope.
    async fn save(&self, name: &str, data: Part) -> Result<SaveResponse, AgentError> {
        Ok(self
            .service
            .save(SaveRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
                file_name: name.to_string(),
                part: Some(data),
                ..Default::default()
            })
            .await?)
    }

    /// Loads the latest version of `name` (version `0` means latest).
    async fn load(&self, name: &str) -> Result<LoadResponse, AgentError> {
        self.load_version(name, 0).await
    }

    /// Loads a specific `version` of `name`.
    async fn load_version(&self, name: &str, version: i64) -> Result<LoadResponse, AgentError> {
        Ok(self
            .service
            .load(LoadRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
                file_name: name.to_string(),
                version,
            })
            .await?)
    }

    /// Lists the artifacts in this scope.
    async fn list(&self) -> Result<ListResponse, AgentError> {
        Ok(self
            .service
            .list(ListRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
            })
            .await?)
    }

    fn backing_service(&self) -> Option<ArtifactBacking> {
        Some(ArtifactBacking {
            service: self.service.clone(),
            app_name: self.app_name.clone(),
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
        })
    }
}

/// Forwards artifact operations to `parent` and records saves in its event delta.
pub(crate) struct ForwardingArtifactService {
    parent: Option<Arc<dyn Artifacts>>,
    /// Parallel child agents share the parent's event delta. Held across the parent save.
    save_lock: tokio::sync::Mutex<()>,
}

/// Builds a [`ForwardingArtifactService`]: operations fail if `parent` is `None`; delete, versions
/// and get_artifact_version fail with an unsupported-operation error when the parent exposes no
/// backing service.
pub(crate) fn new_forwarding_service(
    parent: Option<Arc<dyn Artifacts>>,
) -> Arc<dyn ArtifactService> {
    Arc::new(ForwardingArtifactService {
        parent,
        save_lock: tokio::sync::Mutex::new(()),
    })
}

/// The parent artifact service is missing.
#[derive(Debug, thiserror::Error)]
enum ForwardingError {
    #[error("parent artifact service is not set")]
    ParentNotSet,
}

fn not_set() -> ArtifactError {
    ArtifactError::Backend(Box::new(ForwardingError::ParentNotSet))
}

/// The error for a parent that exposes no backing service: an `Unsupported` I/O error.
fn unsupported() -> ArtifactError {
    ArtifactError::Backend(Box::new(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "parent artifacts do not expose a backing service: unsupported operation",
    )))
}

/// An [`AgentError`] from the parent, as an [`ArtifactError`] for the `ArtifactService` surface.
fn to_artifact_error(e: AgentError) -> ArtifactError {
    match e {
        AgentError::Artifact(a) => a,
        other => ArtifactError::Backend(Box::new(other)),
    }
}

impl ForwardingArtifactService {
    fn parent(&self) -> Result<&Arc<dyn Artifacts>, ArtifactError> {
        self.parent.as_ref().ok_or_else(not_set)
    }

    /// The parent's backing storage; wrappers around the parent delegate `backing_service`, so no
    /// unwrapping is needed.
    fn parent_storage(&self) -> Result<ArtifactBacking, ArtifactError> {
        self.parent()?.backing_service().ok_or_else(unsupported)
    }
}

#[async_trait]
impl ArtifactService for ForwardingArtifactService {
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse, ArtifactError> {
        let parent = self.parent()?;
        let _guard = self.save_lock.lock().await;
        parent
            .save(&req.file_name, req.part.unwrap_or_default())
            .await
            .map_err(to_artifact_error)
    }

    async fn load(&self, req: LoadRequest) -> Result<LoadResponse, ArtifactError> {
        let parent = self.parent()?;
        let res = if req.version == 0 {
            parent.load(&req.file_name).await
        } else {
            parent.load_version(&req.file_name, req.version).await
        };
        res.map_err(to_artifact_error)
    }

    async fn list(&self, _req: ListRequest) -> Result<ListResponse, ArtifactError> {
        self.parent()?.list().await.map_err(to_artifact_error)
    }

    async fn delete(&self, mut req: DeleteRequest) -> Result<(), ArtifactError> {
        let parent = self.parent_storage()?;
        (req.app_name, req.user_id, req.session_id) =
            (parent.app_name, parent.user_id, parent.session_id);
        parent.service.delete(req).await
    }

    async fn versions(&self, mut req: VersionsRequest) -> Result<VersionsResponse, ArtifactError> {
        let parent = self.parent_storage()?;
        (req.app_name, req.user_id, req.session_id) =
            (parent.app_name, parent.user_id, parent.session_id);
        parent.service.versions(req).await
    }

    async fn get_artifact_version(
        &self,
        mut req: GetArtifactVersionRequest,
    ) -> Result<GetArtifactVersionResponse, ArtifactError> {
        let parent = self.parent_storage()?;
        (req.app_name, req.user_id, req.session_id) =
            (parent.app_name, parent.user_id, parent.session_id);
        parent.service.get_artifact_version(req).await
    }
}
