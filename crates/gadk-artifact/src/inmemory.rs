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

//! [`InMemoryArtifactService`]: a process-local [`ArtifactService`] (port of Go
//! artifact/inmemory.go).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use gadk_genai::Part;
use gadk_platform::Platform;
use parking_lot::RwLock;
use serde_json::{Map, Value};

use crate::service::{
    ArtifactError, ArtifactService, ArtifactVersion, DeleteRequest, GetArtifactVersionRequest,
    GetArtifactVersionResponse, ListRequest, ListResponse, LoadRequest, LoadResponse, SaveRequest,
    SaveResponse, VersionsRequest, VersionsResponse,
};
use crate::util::{file_has_user_namespace, go_path_escape, go_sprint, validation_failed};

/// Port of Go artifactKey. Derived `Ord` (with the version reversed) reproduces the
/// `ordered.Encode(app, user, session, file, ordered.Rev(version))` byte order, so a
/// `BTreeMap<ArtifactKey, _>` range scan visits the newest version of a file first, as Go's
/// omap scan does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ArtifactKey {
    app_name: String,
    user_id: String,
    session_id: String,
    file_name: String,
    version: Reverse<i64>,
}

impl ArtifactKey {
    pub(crate) fn new(app: &str, user: &str, session: &str, file: &str, version: i64) -> Self {
        Self {
            app_name: app.to_owned(),
            user_id: user.to_owned(),
            session_id: session.to_owned(),
            file_name: file.to_owned(),
            version: Reverse(version),
        }
    }

    pub(crate) fn version(&self) -> i64 {
        self.version.0
    }
}

#[cfg(test)]
impl ArtifactKey {
    /// Port of Go artifactKey.Encode (exercised by TestArtifactKey only; the store keys by the
    /// struct).
    pub(crate) fn encode(&self) -> String {
        serde_json::json!([
            self.app_name,
            self.user_id,
            self.session_id,
            self.file_name,
            self.version.0
        ])
        .to_string()
    }

    /// Port of Go artifactKey.Decode.
    pub(crate) fn decode(s: &str) -> Result<Self, ArtifactError> {
        let invalid = || ArtifactError::InvalidRequest(format!("invalid artifact key: {s}"));
        let parsed: (String, String, String, String, i64) =
            serde_json::from_str(s).map_err(|_| invalid())?;
        let (app, user, session, file, version) = parsed;
        Ok(Self::new(&app, &user, &session, &file, version))
    }
}

/// Port of Go artifactEntry.
struct ArtifactEntry {
    part: Part,
    create_time: DateTime<Utc>,
    custom_metadata: Map<String, Value>,
}

/// Port of Go inMemoryService: an [`ArtifactService`] that keeps every artifact version in
/// process memory.
pub struct InMemoryArtifactService {
    platform: Platform,
    artifacts: RwLock<BTreeMap<ArtifactKey, ArtifactEntry>>,
}

/// Port of Go userScopedArtifactKey: the session slot user-scoped artifacts are stored under.
const USER_SCOPED_ARTIFACT_KEY: &str = "user";

/// Session slot an artifact is stored under: `"user"` for user-scoped file names.
fn storage_session<'a>(session_id: &'a str, file_name: &str) -> &'a str {
    if file_has_user_namespace(file_name) {
        USER_SCOPED_ARTIFACT_KEY
    } else {
        session_id
    }
}

/// The inclusive key range covering every version of one file, newest first.
fn file_range(
    app: &str,
    user: &str,
    session: &str,
    file: &str,
) -> std::ops::RangeInclusive<ArtifactKey> {
    ArtifactKey::new(app, user, session, file, i64::MAX)
        ..=ArtifactKey::new(app, user, session, file, 0)
}

/// File names stored directly under `(app, user, session)`.
fn session_file_names<'a>(
    artifacts: &'a BTreeMap<ArtifactKey, ArtifactEntry>,
    app: &'a str,
    user: &'a str,
    session: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    artifacts
        .range(ArtifactKey::new(app, user, session, "", i64::MAX)..)
        .map(|(k, _)| k)
        .take_while(move |k| k.app_name == app && k.user_id == user && k.session_id == session)
        .map(|k| k.file_name.as_str())
}

impl InMemoryArtifactService {
    /// Port of Go artifact.InMemoryService, using the default [`Platform`].
    pub fn new() -> Self {
        Self::with_platform(Platform::default())
    }

    /// An in-memory service whose create times come from `platform` (Go reads the clock from
    /// ctx via `platform.Now`).
    pub fn with_platform(platform: Platform) -> Self {
        Self {
            platform,
            artifacts: RwLock::new(BTreeMap::new()),
        }
    }
}

impl Default for InMemoryArtifactService {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemoryArtifactService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InMemoryArtifactService")
    }
}

/// Port of Go inMemoryService.find: the latest version of a file and its entry.
fn find<'a>(
    artifacts: &'a BTreeMap<ArtifactKey, ArtifactEntry>,
    app: &str,
    user: &str,
    session: &str,
    file: &str,
) -> Option<(i64, &'a ArtifactEntry)> {
    // first key is the latest one.
    artifacts
        .range(file_range(app, user, session, file))
        .next()
        .map(|(k, v)| (k.version(), v))
}

/// Port of Go inMemoryService.get.
fn get<'a>(
    artifacts: &'a BTreeMap<ArtifactKey, ArtifactEntry>,
    app: &str,
    user: &str,
    session: &str,
    file: &str,
    version: i64,
) -> Option<&'a ArtifactEntry> {
    artifacts.get(&ArtifactKey::new(app, user, session, file, version))
}

#[async_trait::async_trait]
impl ArtifactService for InMemoryArtifactService {
    /// Port of Go inMemoryService.Save.
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let session_id = storage_session(&req.session_id, &req.file_name);
        let custom_metadata: Map<String, Value> = req
            .custom_metadata
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(go_sprint(v))))
            .collect();
        // Read the caller-provided clock outside the service lock. A provider may call back
        // into the service, so invoking it while locked could deadlock.
        let create_time = self.platform.now();
        let Some(part) = req.part else {
            // Unreachable: validate() rejects a missing part.
            return Err(ArtifactError::InvalidRequest(
                "request validation failed: invalid save request: missing required fields: Part"
                    .into(),
            ));
        };

        let mut artifacts = self.artifacts.write();
        let next_version = find(
            &artifacts,
            &req.app_name,
            &req.user_id,
            session_id,
            &req.file_name,
        )
        .map_or(1, |(v, _)| v + 1);
        artifacts.insert(
            ArtifactKey::new(
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
                next_version,
            ),
            ArtifactEntry {
                part,
                create_time,
                custom_metadata,
            },
        );
        Ok(SaveResponse {
            version: next_version,
        })
    }

    /// Port of Go inMemoryService.Load.
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let session_id = storage_session(&req.session_id, &req.file_name);
        let artifacts = self.artifacts.read();
        let entry = if req.version > 0 {
            get(
                &artifacts,
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
                req.version,
            )
        } else {
            // pick the latest version
            find(
                &artifacts,
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
            )
            .map(|(_, e)| e)
        };
        let entry = entry.ok_or(ArtifactError::NotFound)?;
        Ok(LoadResponse {
            part: Some(entry.part.clone()),
        })
    }

    /// Port of Go inMemoryService.Delete.
    async fn delete(&self, req: DeleteRequest) -> Result<(), ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let session_id = storage_session(&req.session_id, &req.file_name);
        let mut artifacts = self.artifacts.write();
        if req.version != 0 {
            artifacts.remove(&ArtifactKey::new(
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
                req.version,
            ));
            return Ok(());
        }
        let keys: Vec<ArtifactKey> = artifacts
            .range(file_range(
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
            ))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            artifacts.remove(&key);
        }
        Ok(())
    }

    /// Port of Go inMemoryService.List.
    async fn list(&self, req: ListRequest) -> Result<ListResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let artifacts = self.artifacts.read();
        let mut files: BTreeSet<&str> =
            session_file_names(&artifacts, &req.app_name, &req.user_id, &req.session_id).collect();
        // Besides the session specific artifacts, also retrieve user scoped artifacts.
        // Session ID "user" shares this key range with user-scoped artifacts, so skip that
        // session's private files. Python rejects the ID instead, but Go already accepts it.
        files.extend(
            session_file_names(
                &artifacts,
                &req.app_name,
                &req.user_id,
                USER_SCOPED_ARTIFACT_KEY,
            )
            .filter(|f| file_has_user_namespace(f)),
        );
        Ok(ListResponse {
            file_names: files.into_iter().map(str::to_owned).collect(),
        })
    }

    /// Port of Go inMemoryService.Versions.
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let session_id = storage_session(&req.session_id, &req.file_name);
        let artifacts = self.artifacts.read();
        let versions: Vec<i64> = artifacts
            .range(file_range(
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
            ))
            .map(|(k, _)| k.version())
            .collect();
        if versions.is_empty() {
            return Err(ArtifactError::NotFound);
        }
        Ok(VersionsResponse { versions })
    }

    /// Port of Go inMemoryService.GetArtifactVersion.
    async fn get_artifact_version(
        &self,
        req: GetArtifactVersionRequest,
    ) -> Result<GetArtifactVersionResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let user_scoped = file_has_user_namespace(&req.file_name);
        let session_id = storage_session(&req.session_id, &req.file_name);
        let artifacts = self.artifacts.read();
        let found = if req.version > 0 {
            get(
                &artifacts,
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
                req.version,
            )
            .map(|e| (req.version, e))
        } else {
            find(
                &artifacts,
                &req.app_name,
                &req.user_id,
                session_id,
                &req.file_name,
            )
        };
        let (version, entry) = found.ok_or(ArtifactError::NotFound)?;

        let app = go_path_escape(&req.app_name);
        let user = go_path_escape(&req.user_id);
        let file = go_path_escape(&req.file_name);
        let canonical_uri = if user_scoped {
            format!("memory://apps/{app}/users/{user}/artifacts/{file}/versions/{version}")
        } else {
            let session = go_path_escape(session_id);
            format!(
                "memory://apps/{app}/users/{user}/sessions/{session}/artifacts/{file}/versions/{version}"
            )
        };
        let mime_type = entry
            .part
            .inline_data
            .as_ref()
            .map_or_else(|| "text/plain".to_owned(), |b| b.mime_type.clone());
        Ok(GetArtifactVersionResponse {
            artifact_version: Some(ArtifactVersion {
                version,
                canonical_uri,
                custom_metadata: entry.custom_metadata.clone(),
                create_time: entry.create_time,
                mime_type,
            }),
        })
    }
}
