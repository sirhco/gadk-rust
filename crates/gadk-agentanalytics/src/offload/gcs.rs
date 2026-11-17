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

//! The Cloud Storage offloader ([`GcsOffloader`]: `new`, `upload_content`, `close`,
//! `is_precondition_failed`) over the Cloud Storage JSON API, with the wire behavior of the
//! Cloud Storage client libraries: one multipart request below 16 MiB, a resumable upload in
//! 16 MiB chunks from 16 MiB on, `ifGenerationMatch=0` (create only), the CRC32C in the
//! metadata / `X-Goog-Hash`, and the writer's retries of the idempotent upload (the retry
//! policy, backoff, request parameters and `X-Goog-Api-Client` tokens follow those libraries).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gadk_model::gcp_auth::{adc_credentials, credential_headers};
use gadk_model::rest::googleapi::check_response;
use gadk_model::rest::{
    CallTimeout, RequestHeaders, RestError, RestReply, RestTransport, RetryAfterRule, RetryPolicy,
};
use gadk_model::{HttpClient, ReqwestClient};
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::{Platform, go_query_escape, go_quote};
use google_cloud_auth::credentials::Credentials;
use http::header::{CONTENT_TYPE, LOCATION, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use serde_json::{Map, Value};
use tokio::time::Instant;

use crate::config::{ClientOptions, GcsContentOffloader, GcsUploadRequest};
use crate::context::{Context, ContextError};
use crate::crc32c;
use crate::error::Error;
use crate::gax::{Backoff, Pauses, random_id, sleep, wrapped_call_err};
use crate::offload::{DEFAULT_UPLOAD_CHUNK_SIZE, OwnedUploadError};

/// The OAuth scopes requested for Cloud Storage (`devstorage.full_control` and
/// `cloud-platform`).
pub(crate) const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/devstorage.full_control",
    "https://www.googleapis.com/auth/cloud-platform",
];

/// The default endpoint (`https://storage.<universe domain>/storage/v1/`, here for the
/// `googleapis.com` universe).
pub(crate) const DEFAULT_ENDPOINT: &str = "https://storage.googleapis.com/storage/v1/";

/// The per-chunk retry deadline of a resumable upload (no chunk retry deadline is configured on
/// the writer).
pub(crate) const CHUNK_RETRY_DEADLINE: Duration = Duration::from_secs(32);

/// The default backoff: initial 100 ms (maximum 30 s, multiplier 2).
const RETRY_BACKOFF: Backoff = Backoff {
    initial: Duration::from_millis(100),
    max: Duration::ZERO,
    multiplier: 0.0,
};

/// One attempt per request: the retry loops are in this module.
const SINGLE_ATTEMPT: RetryPolicy = RetryPolicy {
    max_retries: 0,
    max_backoff: Duration::ZERO,
    retry_after: RetryAfterRule::Unbounded,
    retry_dropped_body: false,
};

const X_GOOG_API_CLIENT: HeaderName = HeaderName::from_static("x-goog-api-client");
const X_GOOG_GCS_IDEMPOTENCY_TOKEN: HeaderName =
    HeaderName::from_static("x-goog-gcs-idempotency-token");
const X_UPLOAD_CONTENT_TYPE: HeaderName = HeaderName::from_static("x-upload-content-type");
const X_GUPLOADER_NO_308: HeaderName = HeaderName::from_static("x-guploader-no-308");
const X_GOOG_HASH: HeaderName = HeaderName::from_static("x-goog-hash");
const X_HTTP_STATUS_CODE_OVERRIDE: &str = "x-http-status-code-override";
const CONTENT_RANGE: HeaderName = HeaderName::from_static("content-range");

/// Stores objects in one Cloud Storage bucket.
pub struct GcsOffloader {
    /// The transport (one attempt per request; the upload retries itself).
    pub(crate) transport: RestTransport,
    /// The JSON API base, e.g. `https://storage.googleapis.com/storage/v1/`.
    pub(crate) base_url: String,
    /// The upload base without a trailing `/`, e.g.
    /// `https://storage.googleapis.com/upload/storage/v1` (`/upload/storage/v1/b/{bucket}/o`
    /// resolved against the base path).
    pub(crate) upload_url: String,
    /// The credentials sent with every request; `None` turns authentication off.
    pub(crate) credentials: Option<Credentials>,
    /// Extra headers sent with every request.
    pub(crate) headers: HeaderMap,
    /// The bucket.
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
    /// Creates a client for `bucket` reaching [`ClientOptions::endpoint`] (e.g.
    /// `http://127.0.0.1:9000/storage/v1/`; a bare `host[:port]` replaces the host of the
    /// default `https://storage.googleapis.com/storage/v1/`) or Cloud Storage, authenticated as
    /// `BigQueryClient::new` authenticates (scopes `devstorage.full_control` and
    /// `cloud-platform`; none with [`ClientOptions::http_client`], which is used as is). Reads
    /// `STORAGE_EMULATOR_HOST` and `GOOGLE_APPLICATION_CREDENTIALS` from the process
    /// environment.
    ///
    /// # Errors
    ///
    /// `failed to create Cloud Storage client: …`.
    pub fn new(bucket: impl Into<String>, options: &ClientOptions) -> Result<GcsOffloader, Error> {
        Self::new_with_env(bucket, options, &ProcessEnv)
    }

    /// [`new`](Self::new) with the environment read from `env`. A non-empty
    /// `STORAGE_EMULATOR_HOST` (`host[:port]` or a URL) makes `http://<host>/storage/v1/` the
    /// default endpoint and turns authentication off.
    ///
    /// # Errors
    ///
    /// `failed to create Cloud Storage client: …`.
    pub fn new_with_env(
        bucket: impl Into<String>,
        options: &ClientOptions,
        env: &dyn EnvProvider,
    ) -> Result<GcsOffloader, Error> {
        let creating = |e: Error| Error::wrap("failed to create Cloud Storage client", e);
        let emulator = env
            .var("STORAGE_EMULATOR_HOST")
            .filter(|host| !host.is_empty());
        let default_endpoint = match &emulator {
            None => DEFAULT_ENDPOINT.to_owned(),
            Some(host) => emulator_endpoint(host).map_err(creating)?,
        };
        let without_authentication = options.without_authentication || emulator.is_some();
        if without_authentication && options.credentials.is_some() {
            return Err(creating(Error::wrap(
                "dialing",
                Error::msg(
                    "options.WithoutAuthentication is incompatible with any option that \
                     provides credentials",
                ),
            )));
        }
        // A caller-supplied HTTP client is used as is, with no credential lookup and no
        // authentication.
        let credentials = if without_authentication || options.http_client.is_some() {
            None
        } else if let Some(credentials) = &options.credentials {
            Some(credentials.clone())
        } else {
            let file = env.var("GOOGLE_APPLICATION_CREDENTIALS");
            Some(
                adc_credentials(file.as_deref(), &SCOPES)
                    .map_err(|e| creating(Error::wrap("dialing", Error::other(e))))?,
            )
        };
        let base_url = match options.endpoint.as_deref().filter(|e| !e.is_empty()) {
            None => default_endpoint,
            Some(endpoint) if endpoint.contains("://") => endpoint.to_owned(),
            Some(endpoint) => merge_endpoints(&default_endpoint, endpoint),
        };
        let parsed = gadk_platform::go_url::parse(&base_url).map_err(|e| {
            creating(Error::wrap_text(
                format!(
                    "supplied endpoint {} is not valid: {e}",
                    go_quote(&base_url)
                ),
                Error::other(e),
            ))
        })?;
        let upload_url = format!("{}://{}/upload/storage/v1", parsed.scheme, parsed.host);
        let http: Arc<dyn HttpClient> = options
            .http_client
            .clone()
            .unwrap_or_else(|| Arc::new(ReqwestClient::default()));
        Ok(GcsOffloader {
            transport: RestTransport {
                http,
                policy: SINGLE_ATTEMPT,
                response_header_timeout: None,
            },
            base_url,
            upload_url,
            credentials,
            headers: options.headers.clone(),
            bucket: bucket.into(),
        })
    }

    /// Writes the object, creating it only if it does not exist (`ifGenerationMatch=0`): the
    /// error the upload returns.
    async fn write_object(&self, ctx: &Context, req: &GcsUploadRequest) -> Result<(), Error> {
        // A context that already ended sends nothing.
        if let Some(err) = ctx.err() {
            return Err(Error::Context(err));
        }
        let data = &req.data;
        let media_type = media_type(&req.content_type, data);
        // Below `DEFAULT_UPLOAD_CHUNK_SIZE` the data fits one chunk (a multipart upload), else
        // the default chunk size applies (a resumable upload).
        let single_chunk = data.len() < DEFAULT_UPLOAD_CHUNK_SIZE;
        let crc = crc32c::checksum(data);
        let url = self.insert_url(
            &req.path,
            if single_chunk {
                "multipart"
            } else {
                "resumable"
            },
        );
        let (parts, body) = if single_chunk {
            // The auto checksum goes into the metadata of a single-chunk upload.
            let metadata = metadata_json(&self.bucket, req, Some(crc))?;
            let boundary = multipart_boundary();
            let body = multipart_body(&boundary, &metadata, media_type, data);
            let mut headers = HeaderMap::new();
            headers.insert(
                CONTENT_TYPE,
                header_value(&format!("multipart/related; boundary={boundary}"))?,
            );
            let (parts, body) = self.send_and_retry(ctx, &url, body, headers).await?;
            check_response(&parts, &body).map_err(Error::Api)?;
            (parts, body)
        } else {
            let metadata = metadata_json(&self.bucket, req, None)?;
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(X_UPLOAD_CONTENT_TYPE, header_value(media_type)?);
            let (parts, body) = self.send_and_retry(ctx, &url, metadata, headers).await?;
            check_response(&parts, &body).map_err(Error::Api)?;
            let location = parts
                .headers
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let upload = ResumableUpload {
                offloader: self,
                uri: location,
                data,
                media_type,
                crc,
            };
            let (parts, body) = upload.upload(ctx).await?;
            check_response(&parts, &body).map_err(Error::Api)?;
            (parts, body)
        };
        decode_object(&parts, &body)
    }

    /// `POST <upload base>/b/{bucket}/o?…&uploadType=<upload_type>` with the query parameters
    /// sorted by key.
    fn insert_url(&self, name: &str, upload_type: &str) -> String {
        let params = [
            ("alt", "json"),
            ("ifGenerationMatch", "0"),
            ("name", name),
            ("prettyPrint", "false"),
            ("projection", "full"),
            ("uploadType", upload_type),
        ];
        let query = params
            .iter()
            .map(|(k, v)| format!("{}={}", go_query_escape(k), go_query_escape(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!(
            "{}/b/{}/o?{query}",
            self.upload_url,
            expand_escape(&self.bucket)
        )
    }

    /// Sends the multipart upload or the resumable initiation with the default backoff and
    /// [`should_retry`], until it succeeds, fails without a retryable status or error, or `ctx`
    /// ends. Every attempt carries the same invocation ID (`X-Goog-Gcs-Idempotency-Token`) and
    /// its attempt count.
    async fn send_and_retry(
        &self,
        ctx: &Context,
        url: &str,
        body: Bytes,
        headers: HeaderMap,
    ) -> Result<(http::response::Parts, Bytes), Error> {
        let invocation_id = Platform::default().new_uuid();
        let mut backoff = Pauses::new(RETRY_BACKOFF);
        let mut attempts: u32 = 1;
        let mut pause = Duration::ZERO;
        let mut last_err: Option<Error> = None;
        loop {
            let waited = sleep(ctx, pause).await;
            if let Err(ctx_err) = waited.and_then(|()| match ctx.err() {
                Some(err) => Err(Error::Context(err)),
                None => Ok(()),
            }) {
                return Err(match last_err {
                    Some(last) => wrapped_call_err(ctx_err, last),
                    None => ctx_err,
                });
            }
            let mut attempt_headers = headers.clone();
            attempt_headers.insert(
                X_GOOG_API_CLIENT,
                header_value(&format!(
                    "gccl-invocation-id/{invocation_id} gccl-attempt-count/{attempts} {}",
                    client_version_tokens()
                ))?,
            );
            attempt_headers.insert(X_GOOG_GCS_IDEMPOTENCY_TOKEN, header_value(&invocation_id)?);
            let result = self
                .send_once(ctx, url, body.clone(), attempt_headers)
                .await
                .map_err(|e| url_error("POST", url, e));
            let retry = match &result {
                Ok((parts, _)) => should_retry(parts.status.as_u16(), None),
                Err(err) => should_retry(0, Some(err)),
            };
            if !retry {
                return result;
            }
            attempts += 1;
            pause = backoff.pause();
            last_err = result.err();
        }
    }

    /// One request: the response with its body read, [`Error::Context`] when `ctx` ended first,
    /// else the transport's [`Error::Rest`].
    async fn send_once(
        &self,
        ctx: &Context,
        url: &str,
        body: Bytes,
        headers: HeaderMap,
    ) -> Result<(http::response::Parts, Bytes), Error> {
        let uri: Uri = url
            .parse()
            .map_err(|e: http::uri::InvalidUri| Error::msg(e.to_string()))?;
        let headers = UploadHeaders {
            offloader: self,
            headers,
        };
        ctx.run(async {
            let reply = self
                .transport
                .send_method(Method::POST, &uri, body, false, &headers, CallTimeout::None)
                .await
                .map_err(Error::Rest)?;
            match reply {
                RestReply::Read(parts, body) => Ok((parts, body)),
                RestReply::Streaming(_) => Err(Error::msg("storage: unexpected streaming reply")),
            }
        })
        .await
    }
}

#[async_trait::async_trait]
impl GcsContentOffloader for GcsOffloader {
    /// Creates `req.path` unless it exists and returns `gs://<bucket>/<path>`; an existing
    /// object (HTTP 412) is a success.
    ///
    /// # Errors
    ///
    /// The owned upload error `failed to write the Cloud Storage object: <err>`
    /// (`crate::offload::OwnedUploadError`).
    async fn upload_content(&self, ctx: &Context, req: GcsUploadRequest) -> Result<String, Error> {
        match self.write_object(ctx, &req).await {
            Err(err) if !is_precondition_failed(&err) => Err(OwnedUploadError::wrap(Error::wrap(
                "failed to write the Cloud Storage object",
                err,
            ))),
            _ => Ok(format!("gs://{}/{}", self.bucket, req.path)),
        }
    }

    /// Closes the client (the HTTP client closes its idle connections; never an error).
    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// Whether the first HTTP API error in `err`'s tree has status 412.
pub(crate) fn is_precondition_failed(err: &Error) -> bool {
    matches!(
        err.find(&mut |e| matches!(e, Error::Api(_))),
        Some(Error::Api(api)) if api.code == 412
    )
}

/// A resumable upload of the in-memory payload: chunks of [`DEFAULT_UPLOAD_CHUNK_SIZE`]
/// `POST`ed to the session URI.
struct ResumableUpload<'a> {
    offloader: &'a GcsOffloader,
    /// The session URI (the initiation's `Location`).
    uri: String,
    data: &'a Bytes,
    media_type: &'a str,
    /// The CRC32C of the whole payload (sent with the last chunk).
    crc: u32,
}

/// What the last turn of [`ResumableUpload::upload_chunk_with_retries`] left (a response and
/// error pair).
enum Last {
    /// No request was sent yet.
    None,
    /// A response.
    Response(http::response::Parts, Bytes),
    /// An error.
    Err(Error),
}

impl ResumableUpload<'_> {
    /// Uploads each chunk until the server stops answering "resume incomplete"; returns the
    /// response that ended the upload.
    async fn upload(&self, ctx: &Context) -> Result<(http::response::Parts, Bytes), Error> {
        let total = self.data.len();
        let mut off = 0usize;
        loop {
            let end = (off + DEFAULT_UPLOAD_CHUNK_SIZE).min(total);
            // The chunk is the last one when the payload ended before the chunk filled — so a
            // payload of a whole number of chunks ends with an empty one.
            let done = end - off < DEFAULT_UPLOAD_CHUNK_SIZE;
            let chunk = self.data.slice(off..end);
            let (result, attempts) = self.upload_chunk_with_retries(ctx, chunk, off, done).await;
            let (parts, body) = match result {
                Last::Response(parts, body) => (parts, body),
                Last::Err(err) if attempts > 1 => {
                    return Err(Error::wrap_text(
                        format!(
                            "chunk upload failed after {attempts} attempts, final error: {err}"
                        ),
                        err,
                    ));
                }
                Last::Err(err) => return Err(err),
                Last::None => {
                    return Err(Error::msg(format!(
                        "upload request to {} not sent, choose larger value for ChunkRetryDeadline",
                        self.uri
                    )));
                }
            };
            if status_resume_incomplete(&parts) {
                off = end;
                continue;
            }
            return Ok((parts, body));
        }
    }

    /// Uploads one chunk, retried per [`should_retry`] with the default backoff until
    /// [`CHUNK_RETRY_DEADLINE`] after the chunk started or `ctx` ends. Each chunk gets a new
    /// invocation ID; a resend keeps it and counts the attempt. Returns the last turn's result
    /// and the attempt count.
    async fn upload_chunk_with_retries(
        &self,
        ctx: &Context,
        chunk: Bytes,
        off: usize,
        done: bool,
    ) -> (Last, u32) {
        let deadline = Instant::now() + CHUNK_RETRY_DEADLINE;
        let mut backoff = Pauses::new(RETRY_BACKOFF);
        let invocation_id = Platform::default().new_uuid();
        let mut attempts: u32 = 1;
        let mut pause = Duration::ZERO;
        let mut last = Last::None;
        loop {
            // Wait on the context, the pause and the retry deadline, then check the context and
            // the deadline once more: an ended context or a passed deadline wins.
            let pause_end = Instant::now() + pause;
            tokio::select! {
                biased;
                () = ctx.done() => {}
                () = tokio::time::sleep_until(deadline) => {}
                () = tokio::time::sleep_until(pause_end) => {}
            }
            if let Some(err) = ctx.err() {
                // Return the last response and error, with the context's error when the last
                // turn had none.
                return match last {
                    Last::Err(e) => (Last::Err(e), attempts),
                    _ => (Last::Err(Error::Context(err)), attempts),
                };
            }
            if Instant::now() >= deadline {
                return (last, attempts);
            }
            let result = self
                .transfer_chunk(ctx, chunk.clone(), off, done, &invocation_id, attempts)
                .await;
            let status = match &result {
                Ok((parts, _)) => parts.status.as_u16(),
                Err(_) => 0,
            };
            // "X-GUploader-No-308: yes" was sent, so a 308 is unexpected.
            if status == 308 {
                return (
                    Last::Err(Error::msg("unexpected 308 response status code")),
                    attempts,
                );
            }
            let retry = should_retry(status, result.as_ref().err());
            last = match result {
                Ok((parts, body)) => Last::Response(parts, body),
                Err(err) => Last::Err(err),
            };
            if !retry {
                return (last, attempts);
            }
            attempts += 1;
            pause = backoff.pause();
        }
    }

    /// One `POST` of the chunk at `off` to the session URI (no chunk transfer timeout). An
    /// error of a request that `ctx` ended is the context's error.
    async fn transfer_chunk(
        &self,
        ctx: &Context,
        chunk: Bytes,
        off: usize,
        done: bool,
        invocation_id: &str,
        attempts: u32,
    ) -> Result<(http::response::Parts, Bytes), Error> {
        let size = chunk.len();
        let content_range = if done {
            if size == 0 {
                format!("bytes */{off}")
            } else {
                format!("bytes {off}-{}/{}", off + size - 1, off + size)
            }
        } else {
            format!("bytes {off}-{}/*", off + size - 1)
        };
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, header_value(&content_range)?);
        headers.insert(CONTENT_TYPE, header_value(self.media_type)?);
        headers.insert(
            X_GOOG_API_CLIENT,
            header_value(&format!(
                "{} gccl-invocation-id/{invocation_id} gccl-attempt-count/{attempts}",
                client_version_tokens()
            ))?,
        );
        headers.insert(X_GOOG_GCS_IDEMPOTENCY_TOKEN, header_value(invocation_id)?);
        headers.insert(X_GUPLOADER_NO_308, HeaderValue::from_static("yes"));
        // The server accepts the checksum only with the final chunk.
        if done {
            headers.insert(
                X_GOOG_HASH,
                header_value(&format!("crc32c={}", crc32c::base64_be(self.crc)))?,
            );
        }
        match self
            .offloader
            .send_once(ctx, &self.uri, chunk, headers)
            .await
        {
            Err(Error::Context(err)) => Err(Error::Context(err)),
            Err(err) => Err(url_error("POST", &self.uri, err)),
            Ok(reply) => Ok(reply),
        }
    }
}

/// Whether the server answered "resume incomplete" to a client that sent
/// `X-GUploader-No-308: yes`.
fn status_resume_incomplete(parts: &http::response::Parts) -> bool {
    parts
        .headers
        .get(X_HTTP_STATUS_CODE_OVERRIDE)
        .is_some_and(|v| v.as_bytes() == b"308")
}

/// One request's headers: gadk's tracking `User-Agent`, the client's headers, the credentials'
/// headers (fetched per request, so a refreshed token is used) and the request's own.
struct UploadHeaders<'a> {
    offloader: &'a GcsOffloader,
    headers: HeaderMap,
}

#[async_trait::async_trait]
impl RequestHeaders for UploadHeaders<'_> {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        let mut h = HeaderMap::new();
        let tracking = HeaderValue::from_str(&gadk_platform::tracking_header())
            .map_err(|e| RestError::Headers(Box::new(e)))?;
        h.insert(USER_AGENT, tracking);
        let mut user = self.offloader.headers.clone();
        for v in user.values_mut() {
            v.set_sensitive(true);
        }
        h.extend(user);
        if let Some(credentials) = &self.offloader.credentials {
            let auth = credential_headers(credentials)
                .await
                .map_err(|e| RestError::Headers(Box::new(e)))?;
            h.extend(auth);
        }
        h.extend(self.headers.clone());
        Ok(h)
    }
}

/// Whether the response or error is retryable: 5xx, 429 and 408 responses; an unexpected EOF, a
/// closed connection, a refused / reset connection or broken pipe, and temporary errors (a
/// deadline, a timeout).
fn should_retry(status: u16, err: Option<&Error>) -> bool {
    if (500..=599).contains(&status) || status == 429 || status == 408 {
        return true;
    }
    err.is_some_and(retryable_error)
}

/// The error half of [`should_retry`], over the error's source chain.
fn retryable_error(err: &Error) -> bool {
    err.find(&mut |e| match e {
        // A deadline is temporary; a cancellation is not.
        Error::Context(c) => *c == ContextError::DeadlineExceeded,
        Error::Rest(rest) => match rest {
            RestError::ResponseHeaderTimeout | RestError::DeadlineExceeded => true,
            RestError::Transport(e) => connection_error(e),
            RestError::Headers(e) => connection_error(e.as_ref()),
            // A body is read after the request was sent; a failed read is never retried.
            RestError::DroppedBody { .. } => false,
        },
        _ => false,
    })
    .is_some()
}

/// Whether the error chain holds an unexpected EOF, a closed connection, a refused or reset
/// connection, a broken pipe (matched by text) or a temporary timeout.
fn connection_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::TimedOut
            )
        {
            return true;
        }
        let text = e.to_string();
        if [
            "connection refused",
            "connection reset",
            "broken pipe",
            "unexpected EOF",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            return true;
        }
        cur = e.source();
    }
    false
}

/// A failed request's error, `Post "<url>": <err>`.
fn url_error(method: &str, url: &str, err: Error) -> Error {
    // The operation name: the method with only its first letter upper-case.
    let mut op = method.to_ascii_lowercase();
    if let Some(first) = op.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Error::wrap(format!("{op} {}", go_quote(url)), err)
}

/// The client-version tokens of `X-Goog-Api-Client` on the insert request and on a resumable
/// chunk.
fn client_version_tokens() -> String {
    // gadk sends its tracking header (`gadk-rust/<v> rust/<v>`) as the client-version tokens;
    // the `gccl-invocation-id/…` and `gccl-attempt-count/…` tokens and their order around it
    // follow the Cloud Storage client libraries.
    gadk_platform::tracking_header()
}

/// A multipart boundary: 60 lower-case hex digits.
fn multipart_boundary() -> String {
    // Two v4 UUIDs supply the digits, which fixes 12 of their bits. The boundary's shape
    // (`^[0-9a-f]{60}$`) is the standard one.
    let mut boundary = random_id();
    boundary.push_str(&random_id());
    boundary.truncate(60);
    boundary
}

/// The content type of the payload: the given one, else sniffed from the payload's first 512
/// bytes. The object metadata carries no `contentType` either way.
fn media_type<'a>(content_type: &'a str, data: &[u8]) -> &'a str {
    if content_type.is_empty() {
        gadk_platform::go_mime::detect_content_type(data)
    } else {
        content_type
    }
}

/// The object resource sent as the multipart metadata part, JSON-encoded with HTML characters
/// escaped and a trailing newline: `bucket`, `contentType` (when set), `crc32c` (a
/// single-chunk upload's auto checksum), `name`.
fn metadata_json(bucket: &str, req: &GcsUploadRequest, crc: Option<u32>) -> Result<Bytes, Error> {
    let mut object = Map::new();
    if !bucket.is_empty() {
        object.insert("bucket".to_owned(), Value::from(bucket));
    }
    if !req.content_type.is_empty() {
        object.insert(
            "contentType".to_owned(),
            Value::from(req.content_type.as_str()),
        );
    }
    if let Some(crc) = crc {
        object.insert("crc32c".to_owned(), Value::from(crc32c::base64_be(crc)));
    }
    if !req.path.is_empty() {
        object.insert("name".to_owned(), Value::from(req.path.as_str()));
    }
    let mut out = gadk_model::to_go_json(&Value::Object(object)).map_err(Error::other)?;
    out.push(b'\n');
    Ok(Bytes::from(out))
}

/// The multipart body: the JSON metadata part, then the media part.
fn multipart_body(boundary: &str, metadata: &[u8], media_type: &str, data: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(metadata.len() + data.len() + 3 * boundary.len() + 128);
    out.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(metadata);
    out.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Type: {media_type}\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(data);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Bytes::from(out)
}

/// Decodes the response as the object resource: a 204 has no body; any other 2xx body must be
/// JSON.
fn decode_object(parts: &http::response::Parts, body: &[u8]) -> Result<(), Error> {
    if parts.status == http::StatusCode::NO_CONTENT {
        return Ok(());
    }
    if body.is_empty() {
        return Err(Error::msg("unexpected end of JSON input"));
    }
    // Only JSON is required of the body, not a particular object shape (the URI is the bucket
    // and the path, not the server's answer).
    serde_json::from_slice::<Value>(body)
        .map(|_| ())
        .map_err(Error::other)
}

/// Escapes a `{var}` path parameter: everything but ASCII letters, digits and `-._~`
/// percent-encoded (upper-case hex).
fn expand_escape(s: &str) -> String {
    go_query_escape(s).replace('+', "%20")
}

/// The endpoint for `STORAGE_EMULATOR_HOST`: the host (or the URL's scheme and host) with the
/// path `storage/v1/`.
fn emulator_endpoint(host: &str) -> Result<String, Error> {
    if host.contains("://") {
        let u = gadk_platform::go_url::parse(host).map_err(Error::other)?;
        Ok(format!("{}://{}/storage/v1/", u.scheme, u.host))
    } else {
        Ok(format!("http://{host}/storage/v1/"))
    }
}

/// The default endpoint with its host replaced by a bare `host[:port]`.
fn merge_endpoints(default: &str, endpoint: &str) -> String {
    match gadk_platform::go_url::parse(default) {
        Ok(u) if !u.host.is_empty() => default.replacen(&u.host, endpoint, 1),
        _ => endpoint.to_owned(),
    }
}

fn header_value(v: &str) -> Result<HeaderValue, Error> {
    HeaderValue::from_str(v).map_err(Error::other)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use gadk_model::rest::googleapi::ApiError;
    use pretty_assertions::assert_eq;

    use super::*;

    fn api(code: u16) -> Error {
        Error::Api(Box::new(ApiError {
            code,
            ..ApiError::default()
        }))
    }

    #[test]
    fn precondition_failed_is_the_first_googleapi_error_with_412() {
        assert!(is_precondition_failed(&api(412)));
        assert!(is_precondition_failed(&Error::wrap("x", api(412))));
        assert!(!is_precondition_failed(&api(403)));
        assert!(!is_precondition_failed(&Error::Joined(vec![
            api(403),
            api(412)
        ])));
        // gRPC FailedPrecondition is not Go's isPreconditionFailed.
        assert!(!is_precondition_failed(&Error::Status {
            code: crate::error::Code::FailedPrecondition,
            message: String::new(),
        }));
        assert!(!is_precondition_failed(&Error::msg("412")));
    }

    #[test]
    fn should_retry_follows_gensupport() {
        for status in [408, 429, 500, 502, 503, 504, 599] {
            assert!(should_retry(status, None), "{status}");
        }
        for status in [0, 200, 308, 400, 401, 403, 404, 412] {
            assert!(!should_retry(status, None), "{status}");
        }
        let deadline = Error::Context(ContextError::DeadlineExceeded);
        assert!(should_retry(0, Some(&deadline)));
        assert!(should_retry(0, Some(&url_error("POST", "u", deadline))));
        assert!(!should_retry(
            0,
            Some(&Error::Context(ContextError::Canceled))
        ));
        let refused = Error::Rest(RestError::Transport(gadk_model::ModelError::Transport(
            Box::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
        )));
        assert!(should_retry(0, Some(&url_error("POST", "u", refused))));
        let other = Error::Rest(RestError::Transport(gadk_model::ModelError::Transport(
            Box::new(std::io::Error::other("tls handshake failure")),
        )));
        assert!(!should_retry(0, Some(&other)));
        // gensupport `shouldRetry` does not retry a client-side ECONNABORTED.
        let aborted = Error::Rest(RestError::Transport(gadk_model::ModelError::Transport(
            Box::new(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
        )));
        assert!(!should_retry(0, Some(&url_error("POST", "u", aborted))));
        assert!(should_retry(
            0,
            Some(&Error::Rest(RestError::ResponseHeaderTimeout))
        ));
    }

    #[test]
    fn url_errors_have_gos_text() {
        assert_eq!(
            url_error(
                "POST",
                "http://h/o?a=1",
                Error::Context(ContextError::Canceled)
            )
            .to_string(),
            r#"Post "http://h/o?a=1": context canceled"#
        );
    }

    #[test]
    fn backoff_is_gensupports() {
        let mut b = Pauses::new(RETRY_BACKOFF);
        let caps: Vec<u128> = (0..12).map(|_| b.next(|n| n - 1).as_millis()).collect();
        assert_eq!(
            caps,
            [
                100, 200, 400, 800, 1600, 3200, 6400, 12800, 25600, 30000, 30000, 30000
            ]
        );
    }

    #[test]
    fn multipart_body_is_gos_mime_multipart() {
        let body = multipart_body("B", b"{}\n", "image/png", b"xy");
        assert_eq!(
            body.as_ref(),
            b"--B\r\nContent-Type: application/json\r\n\r\n{}\n\r\n--B\r\nContent-Type: image/png\r\n\r\nxy\r\n--B--\r\n"
        );
        let boundary = multipart_boundary();
        assert_eq!(boundary.len(), 60);
        assert!(
            boundary
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    #[test]
    fn an_empty_content_type_is_sniffed_like_go() {
        assert_eq!(media_type("image/png", b"GIF89a"), "image/png");
        assert_eq!(media_type("", b"GIF89a..."), "image/gif");
        assert_eq!(media_type("", b"{\"a\":1}"), "text/plain; charset=utf-8");
        assert_eq!(media_type("", &[0, 1, 2, 3]), "application/octet-stream");
        assert_eq!(media_type("", b""), "text/plain; charset=utf-8");
    }

    #[test]
    fn metadata_omits_empty_fields_and_escapes_html() {
        let req = GcsUploadRequest {
            path: "a<b>&c".into(),
            data: Bytes::new(),
            content_type: String::new(),
        };
        assert_eq!(
            metadata_json("bk", &req, None).unwrap().as_ref(),
            b"{\"bucket\":\"bk\",\"name\":\"a\\u003cb\\u003e\\u0026c\"}\n"
        );
    }

    #[tokio::test]
    async fn upload_errors_are_owned_and_a_412_is_not() {
        use gadk_model::rest::test_util::{FakeResponse, ScriptedClient};
        let client = ScriptedClient::sequence(vec![
            FakeResponse::json_status(403, r#"{"error":{"code":403,"message":"forbidden"}}"#),
            FakeResponse::json_status(412, r#"{"error":{"code":412,"message":"exists"}}"#),
        ]);
        let options = ClientOptions {
            endpoint: Some("http://fake/storage/v1/".into()),
            without_authentication: true,
            http_client: Some(client),
            ..ClientOptions::default()
        };
        let env = gadk_platform::env::MapEnv(Default::default());
        let o = GcsOffloader::new_with_env("bk", &options, &env).unwrap();
        let req = || GcsUploadRequest {
            path: "a/b.png".into(),
            data: Bytes::from_static(b"x"),
            content_type: "image/png".into(),
        };
        let ctx = Context::background();
        let err = o.upload_content(&ctx, req()).await.unwrap_err();
        assert!(OwnedUploadError::find(&err).is_some());
        assert!(!is_precondition_failed(&err));
        assert_eq!(
            err.to_string(),
            "failed to write the Cloud Storage object: googleapi: Error 403: forbidden"
        );
        assert_eq!(
            o.upload_content(&ctx, req()).await.unwrap(),
            "gs://bk/a/b.png"
        );
    }

    #[test]
    fn endpoints_resolve_as_go() {
        assert_eq!(
            merge_endpoints(DEFAULT_ENDPOINT, "localhost:9000"),
            "https://localhost:9000/storage/v1/"
        );
        assert_eq!(
            emulator_endpoint("localhost:9023").unwrap(),
            "http://localhost:9023/storage/v1/"
        );
        assert_eq!(
            emulator_endpoint("https://emu:1/x").unwrap(),
            "https://emu:1/storage/v1/"
        );
        assert_eq!(expand_escape("a b/c+d"), "a%20b%2Fc%2Bd");
    }
}
