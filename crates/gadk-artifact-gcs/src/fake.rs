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

//! [`FakeBucket`]: an in-memory bucket for tests, and
//! [`GcsArtifactService::with_bucket_for_testing`].

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::bucket::{GcsBucket, ObjectAttrs};
use crate::error::GcsError;
use crate::service::GcsArtifactService;

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
type ErrorHook = Arc<dyn Fn() -> GcsError + Send + Sync>;

/// One stored object.
struct FakeObject {
    data: Bytes,
    content_type: String,
    metadata: BTreeMap<String, String>,
    created: DateTime<Utc>,
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, FakeObject>,
    now: Option<Clock>,
    write_err: Option<ErrorHook>,
    attrs_err: Option<ErrorHook>,
    delete_err: Option<ErrorHook>,
    close_calls: usize,
    metadata_calls: Vec<BTreeMap<String, String>>,
}

/// An in-memory bucket for tests (feature `testutil`). Uploads honour the
/// does-not-exist precondition atomically, so concurrent saves race as they do on GCS; hooks
/// inject errors and record every upload attempt.
#[derive(Default)]
pub struct FakeBucket {
    state: Mutex<State>,
}

impl std::fmt::Debug for FakeBucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeBucket")
            .field("objects", &self.object_names())
            .finish_non_exhaustive()
    }
}

impl FakeBucket {
    /// An empty bucket whose objects are created at `Utc::now()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the creation time of objects uploaded from now on.
    pub fn set_now(&self, now: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) {
        self.state.lock().now = Some(Arc::new(now));
    }

    /// Every upload attempt fails with `err()` (it is still counted and its
    /// metadata recorded).
    pub fn set_write_err(&self, err: impl Fn() -> GcsError + Send + Sync + 'static) {
        self.state.lock().write_err = Some(Arc::new(err));
    }

    /// Every attributes call fails with `err()`.
    pub fn set_attrs_err(&self, err: impl Fn() -> GcsError + Send + Sync + 'static) {
        self.state.lock().attrs_err = Some(Arc::new(err));
    }

    /// Every delete fails with `err()`.
    pub fn set_delete_err(&self, err: impl Fn() -> GcsError + Send + Sync + 'static) {
        self.state.lock().delete_err = Some(Arc::new(err));
    }

    /// How many uploads were attempted.
    pub fn close_calls(&self) -> usize {
        self.state.lock().close_calls
    }

    /// The metadata of every upload attempt, in order.
    pub fn metadata_calls(&self) -> Vec<BTreeMap<String, String>> {
        self.state.lock().metadata_calls.clone()
    }

    /// The names of the stored objects, sorted.
    pub fn object_names(&self) -> Vec<String> {
        self.state.lock().objects.keys().cloned().collect()
    }
}

#[async_trait::async_trait]
impl GcsBucket for FakeBucket {
    async fn list(&self, prefix: &str, _names_only: bool) -> Result<Vec<String>, GcsError> {
        Ok(self
            .state
            .lock()
            .objects
            .keys()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn insert_if_absent(
        &self,
        name: &str,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        body: Bytes,
    ) -> Result<(), GcsError> {
        let mut state = self.state.lock();
        state.close_calls += 1;
        state.metadata_calls.push(metadata.clone());
        if let Some(err) = &state.write_err {
            return Err(err());
        }
        if state.objects.contains_key(name) {
            return Err(GcsError::PreconditionFailed {
                message: "conditionNotMet".to_owned(),
                body: String::new(),
            });
        }
        let created = state.now.as_ref().map_or_else(Utc::now, |now| now());
        state.objects.insert(
            name.to_owned(),
            FakeObject {
                data: body,
                content_type: content_type.to_owned(),
                metadata: metadata.clone(),
                created,
            },
        );
        Ok(())
    }

    async fn attrs(&self, name: &str) -> Result<ObjectAttrs, GcsError> {
        let state = self.state.lock();
        if let Some(err) = &state.attrs_err {
            return Err(err());
        }
        let object = state.objects.get(name).ok_or(GcsError::ObjectNotExist)?;
        // Real GCS returns a media link for every object; the service must never use it.
        Ok(ObjectAttrs {
            content_type: object.content_type.clone(),
            metadata: object.metadata.clone(),
            created: object.created,
            media_link: format!(
                "https://storage.googleapis.com/download/storage/v1/b/bucket/o/{name}?alt=media"
            ),
        })
    }

    async fn read(&self, name: &str) -> Result<Bytes, GcsError> {
        let state = self.state.lock();
        let object = state.objects.get(name).ok_or(GcsError::ObjectNotExist)?;
        Ok(object.data.clone())
    }

    async fn delete(&self, name: &str) -> Result<(), GcsError> {
        let mut state = self.state.lock();
        if let Some(err) = &state.delete_err {
            return Err(err());
        }
        state
            .objects
            .remove(name)
            .map(|_| ())
            .ok_or(GcsError::ObjectNotExist)
    }
}

impl GcsArtifactService {
    /// A service over `bucket` (feature `testutil`): save retries do not wait.
    pub fn with_bucket_for_testing(
        bucket_name: impl Into<String>,
        bucket: Arc<FakeBucket>,
    ) -> Self {
        let mut service = Self::from_bucket(bucket_name.into(), bucket);
        service.sleep = Arc::new(|_| Box::pin(async {}));
        service
    }
}
