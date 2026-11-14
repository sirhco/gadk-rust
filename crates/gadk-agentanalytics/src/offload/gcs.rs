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

//! Port of ADK-Go `plugin/agentanalytics/gcs_offloader.go`'s `gcsOffloader` (`newGCSOffloader`,
//! `UploadContent`, `Close`, `isPreconditionFailed`) over the Cloud Storage JSON API as Go's
//! `cloud.google.com/go/storage` writer sends it: one multipart request below 16 MiB, a
//! resumable upload in 16 MiB chunks from 16 MiB on, `ifGenerationMatch=0` (create only), the
//! CRC32C in the metadata / `X-Goog-Hash`, and the writer's retries of the idempotent upload.

#![allow(dead_code, unused_variables)] // Task 7 implements the bodies.

use std::fmt;

use gadk_model::rest::RestTransport;
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;

use crate::config::{ClientOptions, GcsContentOffloader, GcsUploadRequest};
use crate::context::Context;
use crate::error::Error;

/// Go `gcsOffloader`: stores objects in one Cloud Storage bucket.
pub struct GcsOffloader {
    /// The transport (one attempt per request; the upload retries itself).
    pub(crate) transport: RestTransport,
    /// The JSON API base, e.g. `https://storage.googleapis.com/storage/v1/`.
    pub(crate) base_url: String,
    /// The upload base, e.g. `https://storage.googleapis.com/upload/storage/v1/`.
    pub(crate) upload_url: String,
    /// The credentials sent with every request; `None` turns authentication off.
    pub(crate) credentials: Option<Credentials>,
    /// Extra headers sent with every request.
    pub(crate) headers: HeaderMap,
    /// Go `name`: the bucket.
    pub(crate) bucket: String,
}

impl fmt::Debug for GcsOffloader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GcsOffloader")
            .field("base_url", &self.base_url)
            .field("upload_url", &self.upload_url)
            .field("credentials", &self.credentials.is_some())
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl GcsOffloader {
    /// Go `newGCSOffloader(ctx, bucket, opts...)`: a client for `bucket` reaching
    /// [`ClientOptions::endpoint`] (Go `option.WithEndpoint`, e.g.
    /// `http://127.0.0.1:9000/storage/v1/`) or Cloud Storage, authenticated as
    /// `BigQueryClient::new` authenticates (scope `devstorage.full_control`).
    ///
    /// # Errors
    ///
    /// `failed to create Cloud Storage client: …`.
    pub fn new(bucket: impl Into<String>, options: &ClientOptions) -> Result<GcsOffloader, Error> {
        todo!("Task 7")
    }
}

#[async_trait::async_trait]
impl GcsContentOffloader for GcsOffloader {
    /// Go `UploadContent(ctx, req)`: creates `req.path` unless it exists and returns
    /// `gs://<bucket>/<path>`; an existing object (HTTP 412) is a success.
    ///
    /// # Errors
    ///
    /// Go's owned upload error `failed to write the Cloud Storage object: <err>`
    /// ([`crate::offload::OwnedUploadError`]).
    async fn upload_content(&self, ctx: &Context, req: GcsUploadRequest) -> Result<String, Error> {
        todo!("Task 7")
    }

    /// Go `Close()`.
    async fn close(&self) -> Result<(), Error> {
        todo!("Task 7")
    }
}

/// Go `isPreconditionFailed(err)`: a `googleapi` error with HTTP 412 in `err`'s tree.
pub(crate) fn is_precondition_failed(err: &Error) -> bool {
    todo!("Task 7")
}
