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

//! The GCS JSON API client behind [`GcsArtifactService::new`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use gadk_artifact::ArtifactError;
use gadk_artifact::util::go_uri_template_escape;
use gadk_model::gcp_auth::credential_headers;
use gadk_model::rest::{
    CallTimeout, RequestHeaders, RestError, RestReply, RestTransport, RetryAfterRule, RetryPolicy,
};
use gadk_model::{HttpClient, ReqwestClient};
use google_cloud_auth::credentials::Credentials;
use http::header::{CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method, Uri};
use serde::{Deserialize, Serialize};

use crate::bucket::{GcsBucket, ObjectAttrs};
use crate::config::GcsConfig;
use crate::error::GcsError;
use crate::service::GcsArtifactService;

#[cfg(test)]
pub(crate) mod wire_tests;

/// Listing, metadata, read and conditional-upload calls: 3 retries (on 408, 409, 429, ≥ 500
/// and connection errors), backoff capped at 32 s.
pub(crate) const RETRY_POLICY: RetryPolicy = RetryPolicy {
    max_retries: 3,
    max_backoff: Duration::from_secs(32),
    retry_after: RetryAfterRule::Unbounded,
    retry_dropped_body: true,
};

/// Delete calls are not retried.
pub(crate) const DELETE_POLICY: RetryPolicy = RetryPolicy {
    max_retries: 0,
    ..RETRY_POLICY
};

/// Every request's headers: gadk's tracking `User-Agent`, the configured headers, then the
/// credentials' headers (fetched per attempt, so a refreshed token is used).
struct GcsHeaders {
    credentials: Option<Credentials>,
    headers: HeaderMap,
}

impl GcsHeaders {
    async fn build(&self) -> Result<HeaderMap, RestError> {
        let mut h = HeaderMap::new();
        let tracking = HeaderValue::from_str(&gadk_platform::tracking_header())
            .map_err(|e| RestError::Headers(Box::new(e)))?;
        h.insert(USER_AGENT, tracking);
        let mut user = self.headers.clone();
        for v in user.values_mut() {
            v.set_sensitive(true);
        }
        h.extend(user);
        if let Some(credentials) = &self.credentials {
            let auth = credential_headers(credentials)
                .await
                .map_err(|e| RestError::Headers(Box::new(e)))?;
            h.extend(auth);
        }
        Ok(h)
    }
}

/// One call's headers: [`GcsHeaders`] plus the call's body content type, if any.
struct CallHeaders<'a> {
    base: &'a GcsHeaders,
    content_type: Option<HeaderValue>,
}

#[async_trait::async_trait]
impl RequestHeaders for CallHeaders<'_> {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        let mut h = self.base.build().await?;
        if let Some(content_type) = &self.content_type {
            h.insert(CONTENT_TYPE, content_type.clone());
        }
        Ok(h)
    }
}

/// The [`GcsBucket`] that talks to the GCS JSON API.
pub(crate) struct RestBucket {
    /// `{base}/storage/v1/b/{bucket}/o`.
    objects_url: String,
    /// `{base}/upload/storage/v1/b/{bucket}/o`.
    upload_url: String,
    /// The transport for every call but delete ([`RETRY_POLICY`]).
    transport: RestTransport,
    /// The transport for delete ([`DELETE_POLICY`]).
    delete_transport: RestTransport,
    headers: GcsHeaders,
}

impl RestBucket {
    /// The client for bucket `bucket` configured by `config` (see [`GcsArtifactService::new`]).
    pub(crate) fn from_config(bucket: &str, config: GcsConfig) -> Result<Self, ArtifactError> {
        let resolved = config.resolve()?;
        let http: Arc<dyn HttpClient> = config
            .http_client
            .unwrap_or_else(|| Arc::new(ReqwestClient::default()));
        let bucket = go_uri_template_escape(bucket);
        Ok(Self {
            objects_url: format!("{}/storage/v1/b/{bucket}/o", resolved.base_url),
            upload_url: format!("{}/upload/storage/v1/b/{bucket}/o", resolved.base_url),
            transport: RestTransport {
                http: http.clone(),
                policy: RETRY_POLICY,
                response_header_timeout: None,
            },
            delete_transport: RestTransport {
                http,
                policy: DELETE_POLICY,
                response_header_timeout: None,
            },
            headers: GcsHeaders {
                credentials: resolved.credentials,
                headers: config.headers,
            },
        })
    }

    /// `{objects_url}/{escaped object name}`, with the object name escaped as a URI-template
    /// path segment (see `go_uri_template_escape`).
    fn object_url(&self, name: &str) -> String {
        format!("{}/{}", self.objects_url, go_uri_template_escape(name))
    }

    /// Sends one call and returns a 2xx response's body, else the status mapped to a
    /// [`GcsError`] (`object` calls — metadata, download, delete — map 404 to
    /// [`GcsError::ObjectNotExist`]).
    async fn call(
        &self,
        transport: &RestTransport,
        method: Method,
        url: &str,
        body: Bytes,
        content_type: Option<HeaderValue>,
        object: bool,
    ) -> Result<Bytes, GcsError> {
        let uri: Uri = url.parse().map_err(|e| GcsError::Other(Box::new(e)))?;
        let headers = CallHeaders {
            base: &self.headers,
            content_type,
        };
        let reply = transport
            .send_method(method, &uri, body, false, &headers, CallTimeout::None)
            .await
            .map_err(GcsError::Transport)?;
        let RestReply::Read(parts, body) = reply else {
            return Err(GcsError::Decode("unexpected streaming reply".to_owned()));
        };
        if parts.status.is_success() {
            return Ok(body);
        }
        Err(status_error(parts.status.as_u16(), &body, object))
    }
}

/// Maps a non-2xx status to a [`GcsError`] carrying the JSON error body's `error.message` and
/// the whole body. For `object` calls (metadata, download and delete) a 404 is
/// [`GcsError::ObjectNotExist`].
fn status_error(status: u16, body: &[u8], object: bool) -> GcsError {
    if object && status == 404 {
        return GcsError::ObjectNotExist;
    }
    let message = serde_json::from_slice::<ErrorReply>(body)
        .ok()
        .and_then(|reply| reply.error)
        .map(|error| error.message)
        .unwrap_or_default();
    let body = String::from_utf8_lossy(body).into_owned();
    if status == 412 {
        GcsError::PreconditionFailed { message, body }
    } else {
        GcsError::Http {
            status,
            message,
            body,
        }
    }
}

#[derive(Deserialize)]
struct ErrorReply {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
}

/// One page of an object listing.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListPage {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct ListItem {
    name: String,
}

/// The object resource fields the service reads.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawObject {
    content_type: String,
    metadata: BTreeMap<String, String>,
    time_created: Option<DateTime<Utc>>,
    media_link: String,
}

/// The JSON metadata part of a multipart upload.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadMetadata<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "str_is_empty")]
    content_type: &'a str,
    #[serde(skip_serializing_if = "map_is_empty")]
    metadata: &'a BTreeMap<String, String>,
}

fn str_is_empty(s: &&str) -> bool {
    s.is_empty()
}

fn map_is_empty(m: &&BTreeMap<String, String>) -> bool {
    m.is_empty()
}

fn decode_error(e: serde_json::Error) -> GcsError {
    GcsError::Decode(e.to_string())
}

/// A `multipart/related` upload body: the JSON metadata part, then the media part (typed
/// `application/octet-stream` when the content type is empty or not a valid header value).
fn multipart_body(boundary: &str, metadata: &[u8], content_type: &str, data: &[u8]) -> Bytes {
    let media_type = if content_type.is_empty() || content_type.contains(char::is_control) {
        "application/octet-stream"
    } else {
        content_type
    };
    let mut out = Vec::with_capacity(metadata.len() + data.len() + 4 * boundary.len() + 128);
    out.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(metadata);
    out.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Type: {media_type}\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(data);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Bytes::from(out)
}

/// A listing's query string, form-encoded: `prefix`, the name-only field selection, and the next
/// page's token.
fn list_query(prefix: &str, names_only: bool, page_token: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("prefix", prefix);
    if names_only {
        query.append_pair("fields", "items(name),nextPageToken");
    }
    if let Some(token) = page_token {
        query.append_pair("pageToken", token);
    }
    query.finish()
}

#[async_trait::async_trait]
impl GcsBucket for RestBucket {
    async fn list(&self, prefix: &str, names_only: bool) -> Result<Vec<String>, GcsError> {
        let mut names = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let query = list_query(prefix, names_only, page_token.as_deref());
            let url = format!("{}?{query}", self.objects_url);
            let body = self
                .call(
                    &self.transport,
                    Method::GET,
                    &url,
                    Bytes::new(),
                    None,
                    false,
                )
                .await?;
            let page: ListPage = serde_json::from_slice(&body).map_err(decode_error)?;
            names.extend(page.items.into_iter().map(|item| item.name));
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok(names),
            }
        }
    }

    async fn insert_if_absent(
        &self,
        name: &str,
        content_type: &str,
        metadata: &BTreeMap<String, String>,
        body: Bytes,
    ) -> Result<(), GcsError> {
        let json = serde_json::to_vec(&UploadMetadata {
            name,
            content_type,
            metadata,
        })
        .map_err(|e| GcsError::Other(Box::new(e)))?;
        let boundary = format!("{:032x}", rand::random::<u128>());
        let header = HeaderValue::from_str(&format!("multipart/related; boundary={boundary}"))
            .map_err(|e| GcsError::Other(Box::new(e)))?;
        let url = format!(
            "{}?uploadType=multipart&ifGenerationMatch=0",
            self.upload_url
        );
        // An upload's errors are reported unmapped: a 404 stays a [`GcsError::Http`].
        self.call(
            &self.transport,
            Method::POST,
            &url,
            multipart_body(&boundary, &json, content_type, &body),
            Some(header),
            false,
        )
        .await?;
        Ok(())
    }

    async fn attrs(&self, name: &str) -> Result<ObjectAttrs, GcsError> {
        let body = self
            .call(
                &self.transport,
                Method::GET,
                &self.object_url(name),
                Bytes::new(),
                None,
                true,
            )
            .await?;
        let raw: RawObject = serde_json::from_slice(&body).map_err(decode_error)?;
        Ok(ObjectAttrs {
            content_type: raw.content_type,
            metadata: raw.metadata,
            created: raw.time_created.unwrap_or_default(),
            media_link: raw.media_link,
        })
    }

    async fn read(&self, name: &str) -> Result<Bytes, GcsError> {
        let url = format!("{}?alt=media", self.object_url(name));
        self.call(&self.transport, Method::GET, &url, Bytes::new(), None, true)
            .await
    }

    async fn delete(&self, name: &str) -> Result<(), GcsError> {
        self.call(
            &self.delete_transport,
            Method::DELETE,
            &self.object_url(name),
            Bytes::new(),
            None,
            true,
        )
        .await?;
        Ok(())
    }
}

impl GcsArtifactService {
    /// A service storing artifacts in bucket `bucket`, reached as
    /// `config` says.
    ///
    /// Requests go to `{base}/storage/v1/…` and `{base}/upload/storage/v1/…`, where `base` is
    /// [`GcsConfig::endpoint`], else `http://{STORAGE_EMULATOR_HOST}` (its scheme kept when it
    /// has one) when that variable is non-empty, else `https://storage.googleapis.com`. They
    /// carry [`GcsConfig::credentials`] or Application Default Credentials, except that a
    /// non-empty `STORAGE_EMULATOR_HOST` turns authentication off.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::InvalidRequest`] for an endpoint with a path other than `/storage/v1`;
    /// [`ArtifactError::Backend`] (`failed to create gcs service: …`) when Application Default
    /// Credentials cannot be resolved, including when they are needed (no
    /// [`GcsConfig::credentials`] and no `STORAGE_EMULATOR_HOST`) and no Tokio runtime is
    /// running on the calling thread: call `new` from within a runtime in that case.
    pub fn new(bucket: impl Into<String>, config: GcsConfig) -> Result<Self, ArtifactError> {
        let bucket = bucket.into();
        let rest = RestBucket::from_config(&bucket, config)?;
        Ok(Self::from_bucket(bucket, Arc::new(rest)))
    }
}
