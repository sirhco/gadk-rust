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

//! The artifact service API: [`ArtifactService`], its request/response types, request
//! validation, and [`ArtifactError`].

use chrono::{DateTime, Utc};
use gadk_genai::Part;
use serde_json::{Map, Value};

use crate::artifact_util::is_artifact_ref;

/// Errors returned by an [`ArtifactService`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// The artifact (or the requested version) does not exist. The `Display` text is
    /// `artifact not found: file does not exist`.
    #[error("artifact not found: file does not exist")]
    NotFound,
    /// The request failed validation; the payload is the full error text, e.g.
    /// `invalid save request: missing required fields: app_name`.
    #[error("{0}")]
    InvalidRequest(String),
    /// A storage-backend failure.
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync>),
}

/// The artifact storage service.
#[async_trait::async_trait]
pub trait ArtifactService: Send + Sync {
    /// Save saves an artifact to the artifact service storage.
    /// The artifact is a file identified by the app name, user ID, session ID, and fileName.
    /// After saving the artifact, a revision ID is returned to identify the artifact version.
    /// Implementations that assign versions optimistically may fail to claim one
    /// while other writers save the same artifact; that failure is transient and
    /// the call can be retried.
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse, ArtifactError>;
    /// Load loads an artifact from the storage.
    /// The artifact is a file identified by the appName, userID, sessionID and fileName.
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse, ArtifactError>;
    /// Delete deletes an artifact. If `req.version` is 0, all versions are deleted,
    /// and deleting an artifact that does not exist is not an error. If `req.version`
    /// is set and that version does not exist, implementations may return an error.
    async fn delete(&self, req: DeleteRequest) -> Result<(), ArtifactError>;
    /// List lists all the artifact filenames within a session.
    async fn list(&self, req: ListRequest) -> Result<ListResponse, ArtifactError>;
    /// Versions lists all versions of an artifact.
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse, ArtifactError>;
    /// GetArtifactVersion gets the metadata for a specific version of an artifact.
    async fn get_artifact_version(
        &self,
        req: GetArtifactVersionRequest,
    ) -> Result<GetArtifactVersionResponse, ArtifactError>;
}

/// A named field and its value, for required-field validation.
pub(crate) struct RequiredField<'a> {
    pub(crate) name: &'a str,
    pub(crate) value: &'a str,
}

/// The names of the empty fields, in input order.
pub(crate) fn validate_required_strings<'a>(fields: &[RequiredField<'a>]) -> Vec<&'a str> {
    fields
        .iter()
        .filter(|f| f.value.is_empty())
        .map(|f| f.name)
        .collect()
}

/// Rejects a file name that contains a path separator (`/` or `\`).
fn validate_file_name(name: &str) -> Result<(), ArtifactError> {
    if name.contains('/') || name.contains('\\') {
        return Err(ArtifactError::InvalidRequest(
            "invalid name: filename cannot contain path separators".into(),
        ));
    }
    Ok(())
}

/// The `invalid <kind> request: missing required fields: ...` error every
/// `validate` builds, or `Ok` when no field is missing.
fn missing(kind: &str, fields: &[&str]) -> Result<(), ArtifactError> {
    if fields.is_empty() {
        return Ok(());
    }
    Err(ArtifactError::InvalidRequest(format!(
        "invalid {kind} request: missing required fields: {}",
        fields.join(", ")
    )))
}

/// The four identifying fields, in check order (the order they appear in the error text).
fn file_fields<'a>(
    app_name: &'a str,
    user_id: &'a str,
    session_id: &'a str,
    file_name: &'a str,
) -> [RequiredField<'a>; 4] {
    [
        RequiredField {
            name: "app_name",
            value: app_name,
        },
        RequiredField {
            name: "user_id",
            value: user_id,
        },
        RequiredField {
            name: "session_id",
            value: session_id,
        },
        RequiredField {
            name: "file_name",
            value: file_name,
        },
    ]
}

/// A request to save an artifact version.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveRequest {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
    /// The artifact's file name; it must not contain a path separator (`/` or `\`).
    pub file_name: String,
    /// The artifact to store.
    pub part: Option<Part>,
    /// Caller-defined metadata persisted with the artifact. Service implementations must
    /// convert each value to its string representation when saving it and return the value
    /// as a string.
    pub custom_metadata: Map<String, Value>,
    /// Deprecated: Save ignores `version`. Every Save creates a new version and returns it in
    /// [`SaveResponse::version`].
    pub version: i64,
}

impl SaveRequest {
    /// Validates the request. A part must carry text or inline data: an
    /// `artifact://` reference part is refused here, as every backend but the in-memory one
    /// relies on.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        self.validate_with(false)
    }

    /// The shared validation, with `allow_ref` tolerating an
    /// `artifact://` reference part. Only [`crate::InMemoryArtifactService`] understands
    /// references, so it is the sole caller that sets it.
    pub(crate) fn validate_with(&self, allow_ref: bool) -> Result<(), ArtifactError> {
        let mut missing_fields = validate_required_strings(&file_fields(
            &self.app_name,
            &self.user_id,
            &self.session_id,
            &self.file_name,
        ));
        if self.part.is_none() {
            missing_fields.push("part");
        }
        missing("save", &missing_fields)?;
        if let Some(part) = &self.part
            && part.text.is_empty()
            && part.inline_data.is_none()
            && !(allow_ref && is_artifact_ref(Some(part)))
        {
            return Err(ArtifactError::InvalidRequest(
                "invalid save request: part.inline_data or part.text has to be set".into(),
            ));
        }
        validate_file_name(&self.file_name)
    }
}

/// The result of a save: the version that was created.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveResponse {
    /// The number of the version the save created.
    pub version: i64,
}

/// A request to load an artifact. `version` is optional (0 means the latest).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadRequest {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
    /// The artifact's file name; it must not contain a path separator (`/` or `\`).
    pub file_name: String,
    /// The version to load; 0 loads the latest.
    pub version: i64,
}

impl LoadRequest {
    /// Validates the request: every identifying field is set and the file name has no path
    /// separator.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let fields = file_fields(
            &self.app_name,
            &self.user_id,
            &self.session_id,
            &self.file_name,
        );
        missing("load", &validate_required_strings(&fields))?;
        validate_file_name(&self.file_name)
    }
}

/// The result of a load.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadResponse {
    /// The artifact stored.
    pub part: Option<Part>,
}

/// A request to delete an artifact. `version` is optional (0 means every version).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteRequest {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
    /// The artifact's file name; it must not contain a path separator (`/` or `\`).
    pub file_name: String,
    /// The version to delete; 0 deletes every version.
    pub version: i64,
}

impl DeleteRequest {
    /// Validates the request: every identifying field is set and the file name has no path
    /// separator.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let fields = file_fields(
            &self.app_name,
            &self.user_id,
            &self.session_id,
            &self.file_name,
        );
        missing("delete", &validate_required_strings(&fields))?;
        validate_file_name(&self.file_name)
    }
}

/// A request to list the artifact file names of a session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListRequest {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
}

impl ListRequest {
    /// Validates the request: the app name, user ID and session ID are set.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let fields = [
            RequiredField {
                name: "app_name",
                value: &self.app_name,
            },
            RequiredField {
                name: "user_id",
                value: &self.user_id,
            },
            RequiredField {
                name: "session_id",
                value: &self.session_id,
            },
        ];
        missing("list", &validate_required_strings(&fields))
    }
}

/// The artifact file names of a session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListResponse {
    /// The artifact file names.
    pub file_names: Vec<String>,
}

/// A request to list the versions of an artifact.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionsRequest {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
    /// The artifact's file name; it must not contain a path separator (`/` or `\`).
    pub file_name: String,
}

impl VersionsRequest {
    /// Validates the request: every identifying field is set and the file name has no path
    /// separator.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let fields = file_fields(
            &self.app_name,
            &self.user_id,
            &self.session_id,
            &self.file_name,
        );
        missing("versions", &validate_required_strings(&fields))?;
        validate_file_name(&self.file_name)
    }
}

/// The versions of an artifact.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionsResponse {
    /// The version numbers of the artifact.
    pub versions: Vec<i64>,
}

/// Metadata of one stored artifact version.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactVersion {
    /// The version number.
    pub version: i64,
    /// A stable identity for the stored payload. Implementations with a native scheme should
    /// use it, such as `gs://` for Google Cloud Storage; other implementations should define a
    /// stable service-specific scheme. It is not necessarily a download endpoint or an
    /// authenticated HTTP URL.
    pub canonical_uri: String,
    /// Contains only string values; empty when no custom metadata was saved.
    pub custom_metadata: Map<String, Value>,
    /// When this artifact version was created.
    pub create_time: DateTime<Utc>,
    /// The media type associated with the stored part. It may be empty when the part was
    /// saved without a media type.
    pub mime_type: String,
}

/// A request for the metadata of one artifact version. `version` is optional (0 means the latest).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GetArtifactVersionRequest {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
    /// The artifact's file name; it must not contain a path separator (`/` or `\`).
    pub file_name: String,
    /// The version to describe; 0 means the latest.
    pub version: i64,
}

impl GetArtifactVersionRequest {
    /// Validates the request: every identifying field is set and the file name has no path
    /// separator.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let fields = file_fields(
            &self.app_name,
            &self.user_id,
            &self.session_id,
            &self.file_name,
        );
        missing("get artifact version", &validate_required_strings(&fields))?;
        validate_file_name(&self.file_name)
    }
}

/// The metadata of one artifact version.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GetArtifactVersionResponse {
    /// The version's metadata.
    pub artifact_version: Option<ArtifactVersion>,
}
