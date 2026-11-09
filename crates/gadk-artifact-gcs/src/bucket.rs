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

//! The storage seam (Go's `gcsClient`, `gcsBucket`, `gcsObject` and `gcsWriter` interfaces folded
//! into one trait): the REST client implements it for GCS, the fake for tests.
#![allow(dead_code)]

use std::collections::BTreeMap;

use bytes::Bytes;
use chrono::{DateTime, Utc};

use crate::error::GcsError;

/// One bucket's objects.
#[async_trait::async_trait]
pub(crate) trait GcsBucket: Send + Sync {
    /// The names of every object whose name starts with `prefix`, across all listing pages (Go
    /// `bucket.objects` + `next` until `iterator.Done`). `names_only` asks only for the names
    /// (Go `Query.SetAttrSelection([]string{"Name"})`).
    async fn list(&self, prefix: &str, names_only: bool) -> Result<Vec<String>, GcsError>;
    /// Writes object `name` only if it does not exist yet (Go `object(name).ifNotExist()` +
    /// `newWriter` + `Close`); an existing object fails with [`GcsError::PreconditionFailed`].
    async fn insert_if_absent(
        &self,
        name: &str,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        body: Bytes,
    ) -> Result<(), GcsError>;
    /// Object `name`'s attributes (Go `object(name).attrs`); a missing object is
    /// [`GcsError::ObjectNotExist`].
    async fn attrs(&self, name: &str) -> Result<ObjectAttrs, GcsError>;
    /// Object `name`'s bytes (Go `object(name).newReader` + `io.ReadAll`).
    async fn read(&self, name: &str) -> Result<Bytes, GcsError>;
    /// Deletes object `name` (Go `object(name).delete`); a missing object is
    /// [`GcsError::ObjectNotExist`].
    async fn delete(&self, name: &str) -> Result<(), GcsError>;
}

/// The attributes of one object the service reads (Go `storage.ObjectAttrs`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ObjectAttrs {
    /// Go `ContentType`.
    pub(crate) content_type: String,
    /// Go `Metadata`.
    pub(crate) metadata: BTreeMap<String, String>,
    /// Go `Created` (the JSON API's `timeCreated`).
    pub(crate) created: DateTime<Utc>,
    /// Go `MediaLink`: the authenticated JSON API download URL, never the canonical URI.
    pub(crate) media_link: String,
}
