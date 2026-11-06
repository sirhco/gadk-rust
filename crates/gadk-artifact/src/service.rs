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
//! validation, and [`ArtifactError`] (port of Go artifact/service.go).

use chrono::{DateTime, Utc};
use gadk_genai::Part;
use serde_json::{Map, Value};

/// Errors returned by an [`ArtifactService`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// The artifact (or the requested version) does not exist. Go:
    /// `fmt.Errorf("artifact not found: %w", fs.ErrNotExist)`.
    #[error("artifact not found: file does not exist")]
    NotFound,
    /// The request failed validation; the payload is Go's exact error text.
    #[error("{0}")]
    InvalidRequest(String),
    /// A storage-backend failure.
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync>),
}

/// Port of Go artifact.Service: the artifact storage service.
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

/// Port of Go artifact/service.go requiredField.
pub(crate) struct RequiredField<'a> {
    pub(crate) name: &'a str,
    pub(crate) value: &'a str,
}

/// Port of Go validateRequiredStrings: names of empty fields, in input order.
pub(crate) fn validate_required_strings<'a>(fields: &[RequiredField<'a>]) -> Vec<&'a str> {
    fields
        .iter()
        .filter(|f| f.value.is_empty())
        .map(|f| f.name)
        .collect()
}

/// Port of Go validateFileName.
fn validate_file_name(name: &str) -> Result<(), ArtifactError> {
    if name.contains('/') || name.contains('\\') {
        return Err(ArtifactError::InvalidRequest(
            "invalid name: filename cannot contain path separators".into(),
        ));
    }
    Ok(())
}

/// The `invalid <kind> request: missing required fields: ...` error Go builds in every
/// `Validate`, or `Ok` when no field is missing.
fn missing(kind: &str, fields: &[&str]) -> Result<(), ArtifactError> {
    if fields.is_empty() {
        return Ok(());
    }
    Err(ArtifactError::InvalidRequest(format!(
        "invalid {kind} request: missing required fields: {}",
        fields.join(", ")
    )))
}

/// The four identifying fields, in Go's check order.
fn file_fields<'a>(
    app_name: &'a str,
    user_id: &'a str,
    session_id: &'a str,
    file_name: &'a str,
) -> [RequiredField<'a>; 4] {
    [
        RequiredField {
            name: "AppName",
            value: app_name,
        },
        RequiredField {
            name: "UserID",
            value: user_id,
        },
        RequiredField {
            name: "SessionID",
            value: session_id,
        },
        RequiredField {
            name: "FileName",
            value: file_name,
        },
    ]
}

/// Port of Go artifact.SaveRequest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
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
    /// Port of Go SaveRequest.Validate.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let mut missing_fields = validate_required_strings(&file_fields(
            &self.app_name,
            &self.user_id,
            &self.session_id,
            &self.file_name,
        ));
        if self.part.is_none() {
            missing_fields.push("Part");
        }
        missing("save", &missing_fields)?;
        if let Some(part) = &self.part
            && part.text.is_empty()
            && part.inline_data.is_none()
        {
            return Err(ArtifactError::InvalidRequest(
                "invalid save request: Part.InlineData or Part.Text has to be set".into(),
            ));
        }
        validate_file_name(&self.file_name)
    }
}

/// Port of Go artifact.SaveResponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveResponse {
    pub version: i64,
}

/// Port of Go artifact.LoadRequest. `version` is optional (0 means the latest).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
    pub version: i64,
}

impl LoadRequest {
    /// Port of Go LoadRequest.Validate.
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

/// Port of Go artifact.LoadResponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadResponse {
    /// The artifact stored.
    pub part: Option<Part>,
}

/// Port of Go artifact.DeleteRequest. `version` is optional (0 means every version).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
    pub version: i64,
}

impl DeleteRequest {
    /// Port of Go DeleteRequest.Validate.
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

/// Port of Go artifact.ListRequest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
}

impl ListRequest {
    /// Port of Go ListRequest.Validate.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let fields = [
            RequiredField {
                name: "AppName",
                value: &self.app_name,
            },
            RequiredField {
                name: "UserID",
                value: &self.user_id,
            },
            RequiredField {
                name: "SessionID",
                value: &self.session_id,
            },
        ];
        missing("list", &validate_required_strings(&fields))
    }
}

/// Port of Go artifact.ListResponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListResponse {
    pub file_names: Vec<String>,
}

/// Port of Go artifact.VersionsRequest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionsRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
}

impl VersionsRequest {
    /// Port of Go VersionsRequest.Validate.
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

/// Port of Go artifact.VersionsResponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionsResponse {
    pub versions: Vec<i64>,
}

/// Port of Go artifact.ArtifactVersion: metadata of one stored artifact version.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactVersion {
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

/// Port of Go artifact.GetArtifactVersionRequest. `version` is optional (0 means the latest).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GetArtifactVersionRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
    pub version: i64,
}

impl GetArtifactVersionRequest {
    /// Port of Go GetArtifactVersionRequest.Validate.
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

/// Port of Go artifact.GetArtifactVersionResponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GetArtifactVersionResponse {
    pub artifact_version: Option<ArtifactVersion>,
}
