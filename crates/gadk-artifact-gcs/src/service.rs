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

//! [`GcsArtifactService`] (port of Go `gcsartifact/service.go`).

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use gadk_artifact::util::{file_has_user_namespace, go_sprint, validation_failed};
use gadk_artifact::{
    ArtifactError, ArtifactService, ArtifactVersion, DeleteRequest, GetArtifactVersionRequest,
    GetArtifactVersionResponse, ListRequest, ListResponse, LoadRequest, LoadResponse, SaveRequest,
    SaveResponse, VersionsRequest, VersionsResponse,
};
use gadk_genai::Part;
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::bucket::GcsBucket;
use crate::error::{BoxError, GcsError, GoWrapped, VersionConflict, backend, chain_any};

/// Go `maxSaveAttempts`: how many colliding writers `save` tolerates before it returns the
/// version-conflict error.
pub(crate) const MAX_SAVE_ATTEMPTS: u32 = 16;
/// Go `saveRetryBaseDelay`.
pub(crate) const SAVE_RETRY_BASE_DELAY: Duration = Duration::from_millis(10);
/// Go `saveRetryMaxDelay`.
pub(crate) const SAVE_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

/// Go's `sleep` field: waits for the given duration; cancellation is dropping the future.
pub(crate) type SleepFn = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

/// A Google Cloud Storage [`ArtifactService`] (Go `gcsService`).
///
/// GCS cannot list and write atomically, so `save` assigns versions optimistically: it writes
/// version `max + 1` with a does-not-exist precondition and, when another writer took that
/// version first, backs off and retries with a fresh one (at most 16 attempts), failing with an
/// error [`is_version_conflict`](crate::is_version_conflict) detects rather than overwriting a
/// version. The scheme only holds if every writer uses the precondition.
pub struct GcsArtifactService {
    pub(crate) bucket_name: String,
    pub(crate) bucket: Arc<dyn GcsBucket>,
    pub(crate) sleep: SleepFn,
}

impl GcsArtifactService {
    /// A service over `bucket` named `bucket_name`, waiting between save retries with
    /// [`sleep_context`].
    pub(crate) fn from_bucket(bucket_name: String, bucket: Arc<dyn GcsBucket>) -> Self {
        Self {
            bucket_name,
            bucket,
            sleep: Arc::new(sleep_context),
        }
    }
}

impl std::fmt::Debug for GcsArtifactService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsArtifactService")
            .field("bucket_name", &self.bucket_name)
            .finish_non_exhaustive()
    }
}

/// Go `sleepContext`: waits for `d` (at once when `d` is zero).
pub(crate) fn sleep_context(d: Duration) -> BoxFuture<'static, ()> {
    if d.is_zero() {
        return Box::pin(async {});
    }
    Box::pin(tokio::time::sleep(d))
}

/// Go `buildBlobName`: `{app}/{user}/{session}/{file}/{version}`, or
/// `{app}/{user}/user/{file}/{version}` for a `user:` file.
pub(crate) fn build_blob_name(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    file_name: &str,
    version: i64,
) -> String {
    if file_has_user_namespace(file_name) {
        format!("{app_name}/{user_id}/user/{file_name}/{version}")
    } else {
        format!("{app_name}/{user_id}/{session_id}/{file_name}/{version}")
    }
}

/// Go `buildBlobNamePrefix`: every version of one file.
pub(crate) fn build_blob_name_prefix(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    file_name: &str,
) -> String {
    if file_has_user_namespace(file_name) {
        format!("{app_name}/{user_id}/user/{file_name}/")
    } else {
        format!("{app_name}/{user_id}/{session_id}/{file_name}/")
    }
}

/// Go `buildSessionPrefix`.
pub(crate) fn build_session_prefix(app_name: &str, user_id: &str, session_id: &str) -> String {
    format!("{app_name}/{user_id}/{session_id}/")
}

/// Go `buildUserPrefix`. Session ID `user` shares this path with user-scoped artifacts, so the
/// prefix includes the `user:` file-name namespace to exclude that session's private files.
pub(crate) fn build_user_prefix(app_name: &str, user_id: &str) -> String {
    format!("{app_name}/{user_id}/user/user:")
}

/// Go `backoffDelay`: a full-jitter backoff in `[0, min(10ms << attempt, 1s)]` for the
/// zero-based retry `attempt`.
pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let max = u64::try_from(SAVE_RETRY_MAX_DELAY.as_nanos()).unwrap_or(u64::MAX);
    let base = u64::try_from(SAVE_RETRY_BASE_DELAY.as_nanos()).unwrap_or(u64::MAX);
    let mut d = max;
    if attempt < 63
        && let Some(scaled) = 1u64.checked_shl(attempt).and_then(|m| base.checked_mul(m))
        && scaled > 0
        && scaled < d
    {
        d = scaled;
    }
    Duration::from_nanos(rand::random_range(0..=d))
}

/// Go `isPreconditionFailed`: `err` or an error it wraps is an HTTP 412.
pub(crate) fn is_precondition_failed(err: &(dyn StdError + 'static)) -> bool {
    chain_any(err, |e| {
        matches!(
            e.downcast_ref::<GcsError>(),
            Some(GcsError::PreconditionFailed { .. } | GcsError::Http { status: 412, .. })
        )
    })
}

/// The object's content type and bytes for `part` (Go `writeArtifact`): the inline data and
/// its MIME type, else the text as `text/plain`.
fn payload(part: Option<&Part>) -> (String, Bytes) {
    match part {
        Some(Part {
            inline_data: Some(blob),
            ..
        }) => (blob.mime_type.clone(), blob.data.clone()),
        Some(part) => ("text/plain".to_owned(), Bytes::from(part.text.clone())),
        None => ("text/plain".to_owned(), Bytes::new()),
    }
}

/// A failed attributes call: a missing object is [`ArtifactError::NotFound`] (Go wraps
/// `fs.ErrNotExist`), anything else Go's `could not get blob attributes: …`.
fn attrs_error(err: GcsError) -> ArtifactError {
    if err.is_object_not_exist() {
        ArtifactError::NotFound
    } else {
        backend("could not get blob attributes", err)
    }
}

impl GcsArtifactService {
    /// Go `writeArtifact`: uploads one version under the does-not-exist precondition. A
    /// precondition failure surfaces where Go's writer reports it, from `Close`.
    async fn write_artifact(
        &self,
        blob_name: &str,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        body: Bytes,
    ) -> Result<(), GoWrapped> {
        self.bucket
            .insert_if_absent(blob_name, content_type, metadata, body)
            .await
            .map_err(|e| GoWrapped::new("failed to close blob writer", e))
    }

    /// Go `versions` after its validation: every version stored under the file's prefix whose
    /// last path segment parses as an `i64` (others are skipped); empty when there are none.
    async fn stored_versions(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Result<Vec<i64>, GoWrapped> {
        let prefix = build_blob_name_prefix(app_name, user_id, session_id, file_name);
        let names = self
            .bucket
            .list(&prefix, false)
            .await
            .map_err(|e| GoWrapped::new("error iterating blobs", e))?;
        Ok(names
            .iter()
            .filter_map(|name| name.rsplit('/').next()?.parse::<i64>().ok())
            .collect())
    }

    /// Go `resolveVersion`: `version`, or the latest stored version when it is 0.
    async fn resolve_version(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
        version: i64,
    ) -> Result<i64, ArtifactError> {
        if version != 0 {
            return Ok(version);
        }
        let versions = self
            .stored_versions(app_name, user_id, session_id, file_name)
            .await
            .map_err(|e| backend("failed to list artifact versions", e))?;
        versions.into_iter().max().ok_or(ArtifactError::NotFound)
    }

    /// Go `fetchFilenamesFromPrefix`: adds the file name (the second-to-last path segment) of
    /// every object under `prefix` to `file_names`.
    async fn fetch_filenames_from_prefix(
        &self,
        prefix: &str,
        file_names: &mut BTreeSet<String>,
    ) -> Result<(), BoxError> {
        let names = self
            .bucket
            .list(prefix, true)
            .await
            .map_err(|e| GoWrapped::new("error iterating blobs", e))?;
        for name in names {
            let segments: Vec<&str> = name.split('/').collect();
            let [.., file_name, _] = segments.as_slice() else {
                return Err(format!(
                    "error iterating blobs: incorrect number of segments in path {}",
                    go_quote(&name)
                )
                .into());
            };
            file_names.insert((*file_name).to_owned());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl ArtifactService for GcsArtifactService {
    /// Port of Go `gcsService.Save`: each attempt costs one listing plus one upload.
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let (app_name, user_id, session_id, file_name) =
            (&req.app_name, &req.user_id, &req.session_id, &req.file_name);
        let metadata: BTreeMap<String, String> = req
            .custom_metadata
            .iter()
            .map(|(k, v)| (k.clone(), go_sprint(v)))
            .collect();
        let (content_type, body) = payload(req.part.as_ref());

        let mut last_err: Option<GoWrapped> = None;
        for attempt in 0..MAX_SAVE_ATTEMPTS {
            let versions = self
                .stored_versions(app_name, user_id, session_id, file_name)
                .await
                .map_err(|e| backend("failed to list artifact versions", e))?;
            let next_version = versions.iter().max().map_or(1, |v| v.wrapping_add(1));

            let blob_name = build_blob_name(app_name, user_id, session_id, file_name, next_version);
            let err = match self
                .write_artifact(&blob_name, &content_type, &metadata, body.clone())
                .await
            {
                Ok(()) => {
                    return Ok(SaveResponse {
                        version: next_version,
                    });
                }
                Err(err) => err,
            };
            if !is_precondition_failed(&err) {
                return Err(backend("failed to save artifact", err));
            }
            last_err = Some(err);
            // Lost the race for this version. Back off (skip after the final attempt) and
            // retry with a fresh version.
            if attempt < MAX_SAVE_ATTEMPTS - 1 {
                (self.sleep)(backoff_delay(attempt)).await;
            }
        }
        let last = last_err.map(|e| e.to_string()).unwrap_or_default();
        Err(backend(
            format!(
                "failed to save artifact {} after {MAX_SAVE_ATTEMPTS} attempts (last error: {last})",
                go_quote(file_name)
            ),
            VersionConflict,
        ))
    }

    /// Port of Go `gcsService.Load`.
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let version = self
            .resolve_version(
                &req.app_name,
                &req.user_id,
                &req.session_id,
                &req.file_name,
                req.version,
            )
            .await?;
        let blob_name = build_blob_name(
            &req.app_name,
            &req.user_id,
            &req.session_id,
            &req.file_name,
            version,
        );
        // Check that the object exists before reading it.
        let attrs = self.bucket.attrs(&blob_name).await.map_err(attrs_error)?;
        let data = self.bucket.read(&blob_name).await.map_err(|e| {
            let context = if e.is_body_read_failure() {
                "could not read data from blob"
            } else {
                "could not create reader for blob"
            };
            backend(format!("{context} '{blob_name}'"), e)
        })?;
        Ok(LoadResponse {
            part: Some(Part::from_bytes(data, &attrs.content_type)),
        })
    }

    /// Port of Go `gcsService.Delete`: one version, or every version concurrently (the first
    /// error is returned).
    async fn delete(&self, req: DeleteRequest) -> Result<(), ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let (app_name, user_id, session_id, file_name) =
            (&req.app_name, &req.user_id, &req.session_id, &req.file_name);
        if req.version != 0 {
            let blob_name = build_blob_name(app_name, user_id, session_id, file_name, req.version);
            return self
                .bucket
                .delete(&blob_name)
                .await
                .map_err(|e| backend("failed to delete artifact", e));
        }
        let versions = self
            .stored_versions(app_name, user_id, session_id, file_name)
            .await
            .map_err(|e| backend("failed to fetch versions on delete artifact", e))?;
        let deletes = versions.into_iter().map(|version| {
            let blob_name = build_blob_name(app_name, user_id, session_id, file_name, version);
            async move {
                self.bucket
                    .delete(&blob_name)
                    .await
                    .map_err(|e| backend(format!("failed to delete artifact {blob_name}"), e))
            }
        });
        futures::future::try_join_all(deletes).await?;
        Ok(())
    }

    /// Port of Go `gcsService.List`: the session's files and the user's `user:` files, sorted
    /// and without duplicates.
    async fn list(&self, req: ListRequest) -> Result<ListResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let mut file_names = BTreeSet::new();
        self.fetch_filenames_from_prefix(
            &build_session_prefix(&req.app_name, &req.user_id, &req.session_id),
            &mut file_names,
        )
        .await
        .map_err(|e| backend("failed to fetch session filenames", e))?;
        self.fetch_filenames_from_prefix(
            &build_user_prefix(&req.app_name, &req.user_id),
            &mut file_names,
        )
        .await
        .map_err(|e| backend("failed to fetch user filenames", e))?;
        Ok(ListResponse {
            file_names: file_names.into_iter().collect(),
        })
    }

    /// Port of Go `gcsService.Versions`: [`ArtifactError::NotFound`] when there are none.
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let versions = self
            .stored_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
            .await
            .map_err(|e| ArtifactError::Backend(Box::new(e)))?;
        if versions.is_empty() {
            return Err(ArtifactError::NotFound);
        }
        Ok(VersionsResponse { versions })
    }

    /// Port of Go `gcsService.GetArtifactVersion`. The canonical URI is always
    /// `gs://{bucket}/{object name}` with the name literal (never the object's media link,
    /// which a consumer handed the URI cannot fetch).
    async fn get_artifact_version(
        &self,
        req: GetArtifactVersionRequest,
    ) -> Result<GetArtifactVersionResponse, ArtifactError> {
        req.validate().map_err(validation_failed)?;
        let version = self
            .resolve_version(
                &req.app_name,
                &req.user_id,
                &req.session_id,
                &req.file_name,
                req.version,
            )
            .await?;
        let blob_name = build_blob_name(
            &req.app_name,
            &req.user_id,
            &req.session_id,
            &req.file_name,
            version,
        );
        let attrs = self.bucket.attrs(&blob_name).await.map_err(attrs_error)?;
        let custom_metadata: Map<String, Value> = attrs
            .metadata
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        Ok(GetArtifactVersionResponse {
            artifact_version: Some(ArtifactVersion {
                version,
                canonical_uri: format!("gs://{}/{blob_name}", self.bucket_name),
                custom_metadata,
                create_time: attrs.created,
                mime_type: attrs.content_type,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::FakeBucket;

    fn service() -> (GcsArtifactService, Arc<FakeBucket>) {
        let fake = Arc::new(FakeBucket::new());
        (
            GcsArtifactService::with_bucket_for_testing("bucket", fake.clone()),
            fake,
        )
    }

    fn save(session_id: &str, file_name: &str) -> SaveRequest {
        SaveRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: session_id.into(),
            file_name: file_name.into(),
            part: Some(Part::from_text("fixture")),
            ..Default::default()
        }
    }

    #[test]
    fn names_and_prefixes_are_gos() {
        assert_eq!(build_blob_name("a", "u", "s", "f.txt", 3), "a/u/s/f.txt/3");
        assert_eq!(
            build_blob_name("a", "u", "s", "user:f.txt", 3),
            "a/u/user/user:f.txt/3"
        );
        assert_eq!(
            build_blob_name_prefix("a", "u", "s", "f.txt"),
            "a/u/s/f.txt/"
        );
        assert_eq!(
            build_blob_name_prefix("a", "u", "s", "user:f.txt"),
            "a/u/user/user:f.txt/"
        );
        assert_eq!(build_session_prefix("a", "u", "s"), "a/u/s/");
        assert_eq!(build_user_prefix("a", "u"), "a/u/user/user:");
    }

    #[tokio::test]
    async fn saved_object_names_are_gos() {
        let (svc, fake) = service();
        for (session_id, file_name) in [
            ("s1", "report.txt"),
            ("s1", "report.txt"),
            ("s1", "user:shared.txt"),
            ("user", "private.txt"),
        ] {
            svc.save(save(session_id, file_name)).await.unwrap();
        }
        assert_eq!(
            fake.object_names(),
            [
                "app/u/s1/report.txt/1",
                "app/u/s1/report.txt/2",
                "app/u/user/private.txt/1",
                "app/u/user/user:shared.txt/1",
            ]
        );
    }

    #[tokio::test]
    async fn validation_errors_are_gos_text() {
        let (svc, fake) = service();
        let err = svc
            .save(SaveRequest {
                app_name: String::new(),
                ..save("s", "f")
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ArtifactError::InvalidRequest(_)), "{err:?}");
        assert_eq!(
            err.to_string(),
            "request validation failed: invalid save request: missing required fields: AppName"
        );
        let err = svc
            .list(ListRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "request validation failed: invalid list request: missing required fields: SessionID"
        );
        assert_eq!(fake.close_calls(), 0);
    }

    #[tokio::test]
    async fn versions_skip_non_numeric_names() {
        let (svc, fake) = service();
        svc.save(save("s", "f")).await.unwrap();
        for name in ["app/u/s/f/notanumber", "app/u/s/f/+7", "app/u/s/f/"] {
            fake.insert_if_absent(name, "text/plain", &BTreeMap::new(), Bytes::new())
                .await
                .unwrap();
        }
        let mut got = svc
            .versions(VersionsRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: "s".into(),
                file_name: "f".into(),
            })
            .await
            .unwrap()
            .versions;
        got.sort_unstable();
        assert_eq!(got, [1, 7]);
        // The next save takes max + 1.
        assert_eq!(svc.save(save("s", "f")).await.unwrap().version, 8);
    }

    #[tokio::test]
    async fn delete_all_returns_the_first_error() {
        let (svc, fake) = service();
        svc.save(save("s", "f")).await.unwrap();
        svc.save(save("s", "f")).await.unwrap();
        fake.set_delete_err(|| GcsError::Http {
            status: 403,
            message: "forbidden".into(),
            body: String::new(),
        });
        let err = svc
            .delete(DeleteRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: "s".into(),
                file_name: "f".into(),
                version: 0,
            })
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("failed to delete artifact app/u/s/f/")
                && text.ends_with(": googleapi: Error 403: forbidden"),
            "{text}"
        );
    }
}
