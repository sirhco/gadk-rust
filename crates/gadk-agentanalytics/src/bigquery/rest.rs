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

//! [`BigQueryClient`]: the subset of the Google Cloud BigQuery client library that the plugin uses,
//! over gadk's REST transport (`gadk_model::rest`): table metadata get / insert / patch (with
//! `If-Match`), query jobs (`jobs.insert`, then polling until done), and the error mapping of
//! [`gadk_model::rest::googleapi::check_response`] ([`ApiError`](crate::error::ApiError)). The
//! requests and retry policy are those of the client library. [`TableOps`] is one table's
//! [`TableMetadataOps`].

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
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
use google_cloud_auth::credentials::Credentials;
use http::header::{CONTENT_TYPE, IF_MATCH, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method, Uri};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::config::ClientOptions;
use crate::context::{Context, ContextError};
use crate::error::{BigQueryError, Error};
use crate::gax::{Backoff, Pauses, random_id, sleep, wrapped_call_err};
use crate::schema::{Schema, TableSchema, nullable};
use crate::schema_upgrade::{
    Clustering, DAY_PARTITIONING_TYPE, TableMetadata, TableMetadataOps, TableMetadataUpdate,
    TimePartitioning,
};

/// The OAuth scope the client requests.
pub(crate) const SCOPE: &str = "https://www.googleapis.com/auth/bigquery";

/// The REST base path without its trailing `/`.
pub(crate) const DEFAULT_BASE_URL: &str = "https://bigquery.googleapis.com/bigquery/v2";

/// One attempt per call: the retries are this module's retry loop, not the transport's.
const SINGLE_ATTEMPT: RetryPolicy = RetryPolicy {
    max_retries: 0,
    max_backoff: Duration::ZERO,
    retry_after: RetryAfterRule::Unbounded,
    retry_dropped_body: false,
};

/// The error reasons that are retried by default.
const DEFAULT_RETRY_REASONS: &[&str] = &["backendError", "rateLimitExceeded"];

/// The error reasons that are retried for `jobs.insert` and `jobs.getQueryResults`.
const JOB_RETRY_REASONS: &[&str] = &[
    "backendError",
    "rateLimitExceeded",
    "jobRateLimitExceeded",
    "internalError",
];

/// The HTTP status codes that are always retried.
const RETRY_5XX_CODES: [u16; 4] = [500, 502, 503, 504];

/// A BigQuery job's identity (project, ID and location).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobReference {
    /// The job's project.
    pub project_id: String,
    /// The job ID.
    pub job_id: String,
    /// The job's location (empty when unknown).
    pub location: String,
}

/// A job's status (a subset of a job's status).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobStatus {
    /// `PENDING`, `RUNNING` or `DONE`.
    pub state: String,
    /// The job's error from `status.errorResult`; set only once the job is `DONE`.
    pub error_result: Option<BigQueryError>,
    /// All errors the job met (`status.errors`).
    pub errors: Vec<BigQueryError>,
}

impl JobStatus {
    /// Whether the job is `DONE`.
    pub fn done(&self) -> bool {
        self.state == "DONE"
    }

    /// The job's error result, `None` when it succeeded.
    pub fn err(&self) -> Option<Error> {
        self.error_result.clone().map(Error::BigQuery)
    }
}

/// A submitted job: its reference, whether it is a query job and the status `jobs.insert` returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Job {
    /// The job's identity.
    pub reference: JobReference,
    /// The job resource carried a `configuration.query`. [`wait_job`] waits for a query job through
    /// `jobs.getQueryResults`, and polls `jobs.get` otherwise.
    ///
    /// [`wait_job`]: BigQueryClient::wait_job
    pub is_query: bool,
    /// The status of the last read.
    pub last_status: Option<JobStatus>,
}

/// A BigQuery REST client for one default project.
pub struct BigQueryClient {
    /// The transport (HTTP client, one attempt per request).
    pub(crate) transport: RestTransport,
    /// The REST base path without a trailing `/`, e.g.
    /// `https://bigquery.googleapis.com/bigquery/v2`; request paths are appended after `/`.
    pub(crate) base_url: String,
    /// The client's project for jobs and for datasets given without a project.
    pub(crate) project_id: String,
    /// The credentials sent with every request; `None` turns authentication off.
    pub(crate) credentials: Option<Credentials>,
    /// Extra headers sent with every request.
    pub(crate) headers: HeaderMap,
}

impl fmt::Debug for BigQueryClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BigQueryClient")
            .field("base_url", &self.base_url)
            .field("project_id", &self.project_id)
            .field("credentials", &self.credentials.is_some())
            .finish_non_exhaustive()
    }
}

impl BigQueryClient {
    /// Builds a client for `project_id`, reading `GOOGLE_APPLICATION_CREDENTIALS` from the process
    /// environment.
    ///
    /// Requests go to [`ClientOptions::endpoint`] (a URL is the REST base path, e.g.
    /// `http://127.0.0.1:9050/bigquery/v2/`; a bare `host[:port]` replaces the host of the default
    /// `https://bigquery.googleapis.com/bigquery/v2/`), else the default. They carry
    /// [`ClientOptions::credentials`], no credentials when
    /// [`ClientOptions::without_authentication`] or [`ClientOptions::http_client`] is set (a
    /// caller's HTTP client is used as is), else Application Default Credentials with the
    /// `bigquery` scope.
    ///
    /// # Errors
    ///
    /// `bigquery: constructing client: …` when credentials are both given and turned off, or
    /// Application Default Credentials cannot be resolved (also when no Tokio runtime runs on
    /// the calling thread).
    pub fn new(
        project_id: impl Into<String>,
        options: &ClientOptions,
    ) -> Result<BigQueryClient, Error> {
        Self::new_with_env(project_id, options, &ProcessEnv)
    }

    /// [`new`](Self::new) with the environment read from `env`.
    pub fn new_with_env(
        project_id: impl Into<String>,
        options: &ClientOptions,
        env: &dyn EnvProvider,
    ) -> Result<BigQueryClient, Error> {
        let constructing = |e: Error| Error::wrap("bigquery: constructing client", e);
        if options.without_authentication && options.credentials.is_some() {
            return Err(constructing(Error::msg(
                "options.WithoutAuthentication is incompatible with any option that provides \
                 credentials",
            )));
        }
        // A caller-supplied HTTP client is used as is, with no credential lookup and no
        // authentication.
        let credentials = if options.without_authentication || options.http_client.is_some() {
            None
        } else if let Some(credentials) = &options.credentials {
            Some(credentials.clone())
        } else {
            let file = env.var("GOOGLE_APPLICATION_CREDENTIALS");
            Some(
                adc_credentials(file.as_deref(), &[SCOPE])
                    .map_err(|e| constructing(Error::other(e)))?,
            )
        };
        let http: Arc<dyn HttpClient> = options
            .http_client
            .clone()
            .unwrap_or_else(|| Arc::new(ReqwestClient::default()));
        let base_url = match options.endpoint.as_deref().filter(|e| !e.is_empty()) {
            Some(endpoint) => endpoint_base_url(endpoint),
            None => DEFAULT_BASE_URL.to_owned(),
        };
        Ok(BigQueryClient {
            transport: RestTransport {
                http,
                policy: SINGLE_ATTEMPT,
                response_header_timeout: None,
            },
            base_url,
            project_id: project_id.into(),
            credentials,
            headers: options.headers.clone(),
        })
    }

    /// The client's default project.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// `GET .../projects/{p}/datasets/{d}/tables/{t}`, retried on 5xx, `backendError`,
    /// `rateLimitExceeded`.
    pub async fn get_table(
        &self,
        ctx: &Context,
        project_id: &str,
        dataset_id: &str,
        table_id: &str,
    ) -> Result<TableMetadata, Error> {
        let url = self.url(&table_path(project_id, dataset_id, table_id), &[]);
        let body = run_with_retry(ctx, DEFAULT_RETRY_REASONS, || {
            self.call(ctx, Method::GET, &url, None, None)
        })
        .await?;
        table_metadata_from_reply(&body)
    }

    /// `POST .../projects/{p}/datasets/{d}/tables` with the table resource (and its
    /// `tableReference`), retried as `get_table` is.
    pub async fn insert_table(
        &self,
        ctx: &Context,
        project_id: &str,
        dataset_id: &str,
        table_id: &str,
        meta: &TableMetadata,
    ) -> Result<TableMetadata, Error> {
        let mut table = table_resource(meta)?;
        table.insert(
            "tableReference".to_owned(),
            json!({"projectId": project_id, "datasetId": dataset_id, "tableId": table_id}),
        );
        let path = format!(
            "projects/{}/datasets/{}/tables",
            escape(project_id),
            escape(dataset_id)
        );
        let url = self.url(&path, &[]);
        let payload = json_body(&Value::Object(table))?;
        let body = run_with_retry(ctx, DEFAULT_RETRY_REASONS, || {
            self.call(ctx, Method::POST, &url, Some(payload.clone()), None)
        })
        .await?;
        table_metadata_from_reply(&body)
    }

    /// `PATCH .../tables/{t}` with `If-Match: etag` when `etag` is non-empty, retried as
    /// `get_table` is.
    pub async fn patch_table(
        &self,
        ctx: &Context,
        project_id: &str,
        dataset_id: &str,
        table_id: &str,
        update: &TableMetadataUpdate,
        etag: &str,
    ) -> Result<TableMetadata, Error> {
        let if_match = if etag.is_empty() {
            None
        } else {
            Some(HeaderValue::from_str(etag).map_err(Error::other)?)
        };
        let url = self.url(&table_path(project_id, dataset_id, table_id), &[]);
        let payload = json_body(&table_update_resource(update)?)?;
        let body = run_with_retry(ctx, DEFAULT_RETRY_REASONS, || {
            self.call(
                ctx,
                Method::PATCH,
                &url,
                Some(payload.clone()),
                if_match.clone(),
            )
        })
        .await?;
        table_metadata_from_reply(&body)
    }

    /// `POST .../projects/{client project}/jobs` with a standard-SQL query job under a new random
    /// 32-hex-digit job ID, retried with the same ID on the job retry reasons; the returned job and
    /// its last status.
    ///
    /// # Errors
    ///
    /// The `jobs.insert` error (a 409 is returned as it is: the existing job is not fetched), or
    /// `unexpected job state: …` for an unknown status.
    pub async fn insert_query_job(&self, ctx: &Context, query: &str) -> Result<Job, Error> {
        let reference = JobReference {
            project_id: self.project_id.clone(),
            job_id: random_id(),
            location: String::new(),
        };
        let job = json!({
            "configuration": {"query": {"query": query, "useLegacySql": false}},
            "jobReference": {"projectId": reference.project_id, "jobId": reference.job_id},
        });
        let url = self.url(&format!("projects/{}/jobs", escape(&self.project_id)), &[]);
        let payload = json_body(&job)?;
        let body = run_with_retry(ctx, JOB_RETRY_REASONS, || {
            self.call(ctx, Method::POST, &url, Some(payload.clone()), None)
        })
        .await?;
        let reply: JobReply = decode(&body)?;
        let reference = match reply.job_reference {
            Some(r) => JobReference {
                project_id: r.project_id,
                job_id: r.job_id,
                location: r.location,
            },
            // A reply without a `jobReference` keeps the reference sent.
            None => reference,
        };
        Ok(Job {
            reference,
            is_query: reply.configuration.is_some_and(|c| c.query.is_some()),
            last_status: job_status(reply.status)?,
        })
    }

    /// Waits for the job: for a query job, polls `jobs.getQueryResults` (`maxResults=0`) until
    /// `jobComplete`, then reads the status with `jobs.get` (`fields=status,statistics`); for
    /// another job, polls `jobs.get` until the job is done. The polling backoff and retry rules are
    /// those of the client library. The returned status may carry the job's error
    /// ([`JobStatus::err`]); an `Err` is a failure to learn the status.
    pub async fn wait_job(&self, ctx: &Context, job: &Job) -> Result<JobStatus, Error> {
        let mut last = job.last_status.clone();
        if job.is_query {
            self.wait_for_query(ctx, &job.reference).await?;
            return self.job_status(ctx, &job.reference, &mut last).await;
        }
        // The retry loop stops at the first error, so a pause cut short by `ctx` returns the bare
        // context error.
        let mut backoff = Pauses::new(Backoff::default());
        loop {
            let status = self.job_status(ctx, &job.reference, &mut last).await?;
            if status.done() {
                return Ok(status);
            }
            sleep(ctx, backoff.pause()).await?;
        }
    }

    /// `GET .../projects/{p}/queries/{job}` until `jobComplete`.
    async fn wait_for_query(&self, ctx: &Context, job: &JobReference) -> Result<(), Error> {
        let path = format!(
            "projects/{}/queries/{}",
            escape(&job.project_id),
            escape(&job.job_id)
        );
        let url = self.url(
            &path,
            &[
                ("formatOptions.useInt64Timestamp", "true"),
                ("location", job.location.as_str()),
                ("maxResults", "0"),
            ],
        );
        let mut backoff = Pauses::new(Backoff {
            initial: Duration::from_millis(50),
            max: Duration::from_secs(60),
            multiplier: 1.3,
        });
        let url = url.as_str();
        retry(ctx, &mut backoff, move || async move {
            match self.call(ctx, Method::GET, url, None, None).await {
                Err(e) => {
                    if retryable_error(&e, JOB_RETRY_REASONS) {
                        Step::Again(Some(e))
                    } else {
                        Step::Stop(Err(e))
                    }
                }
                Ok(body) => match decode::<QueryResultsReply>(&body) {
                    Err(e) => Step::Stop(Err(e)),
                    Ok(reply) if reply.job_complete => Step::Stop(Ok(())),
                    Ok(_) => Step::Again(None),
                },
            }
        })
        .await
    }

    /// `jobs.get` with `fields=status,statistics` (retried as `get_table` is), updating `last` when
    /// the reply has a status.
    async fn job_status(
        &self,
        ctx: &Context,
        job: &JobReference,
        last: &mut Option<JobStatus>,
    ) -> Result<JobStatus, Error> {
        let project = if job.project_id.is_empty() {
            &self.project_id
        } else {
            &job.project_id
        };
        let path = format!("projects/{}/jobs/{}", escape(project), escape(&job.job_id));
        let mut params = vec![("fields", "status,statistics")];
        if !job.location.is_empty() {
            params.push(("location", job.location.as_str()));
        }
        let url = self.url(&path, &params);
        let body = run_with_retry(ctx, DEFAULT_RETRY_REASONS, || {
            self.call(ctx, Method::GET, &url, None, None)
        })
        .await?;
        let reply: JobReply = decode(&body)?;
        if let Some(status) = job_status(reply.status)? {
            *last = Some(status);
        }
        Ok(last.clone().unwrap_or_default())
    }

    /// `{base_url}/{path}?{query}`: `alt=json`, `prettyPrint=false` and `params`, sorted by key and
    /// query-escaped.
    fn url(&self, path: &str, params: &[(&str, &str)]) -> String {
        let mut all: BTreeMap<&str, &str> = params.iter().copied().collect();
        all.insert("alt", "json");
        all.insert("prettyPrint", "false");
        let query = all
            .iter()
            .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}/{path}?{query}", self.base_url)
    }

    /// One request, cancelled when `ctx` ends: a 2xx response's body, else the
    /// [`ApiError`](crate::error::ApiError).
    async fn call(
        &self,
        ctx: &Context,
        method: Method,
        url: &str,
        body: Option<Bytes>,
        if_match: Option<HeaderValue>,
    ) -> Result<Bytes, Error> {
        let uri: Uri = url.parse().map_err(Error::other)?;
        let headers = CallHeaders {
            client: self,
            json_body: body.is_some(),
            if_match,
        };
        ctx.run(async {
            let reply = self
                .transport
                .send_method(
                    method,
                    &uri,
                    body.unwrap_or_default(),
                    false,
                    &headers,
                    CallTimeout::None,
                )
                .await?;
            let RestReply::Read(parts, body) = reply else {
                return Err(Error::msg("bigquery: unexpected streaming reply"));
            };
            if parts.status == http::StatusCode::NO_CONTENT {
                // A 204 decodes to the zero value.
                Ok(Bytes::from_static(b"{}"))
            } else {
                check_response(&parts, &body).map_err(Error::Api)?;
                Ok(body)
            }
        })
        .await
    }
}

/// One request's headers: gadk's tracking `User-Agent`, the client's headers, the credentials'
/// headers (fetched per request, so a refreshed token is used), `Content-Type: application/json`
/// with a body, and `If-Match` for an etag-guarded patch.
struct CallHeaders<'a> {
    client: &'a BigQueryClient,
    json_body: bool,
    if_match: Option<HeaderValue>,
}

#[async_trait::async_trait]
impl RequestHeaders for CallHeaders<'_> {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        let mut h = HeaderMap::new();
        let tracking = HeaderValue::from_str(&gadk_platform::tracking_header())
            .map_err(|e| RestError::Headers(Box::new(e)))?;
        h.insert(USER_AGENT, tracking);
        let mut user = self.client.headers.clone();
        for v in user.values_mut() {
            v.set_sensitive(true);
        }
        h.extend(user);
        if let Some(credentials) = &self.client.credentials {
            let auth = credential_headers(credentials)
                .await
                .map_err(|e| RestError::Headers(Box::new(e)))?;
            h.extend(auth);
        }
        if self.json_body {
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if let Some(etag) = &self.if_match {
            h.insert(IF_MATCH, etag.clone());
        }
        Ok(h)
    }
}

/// The base path of an endpoint option: a value with `://` is the base path itself, which keeps
/// everything up to its last `/` (request paths are resolved against it); a bare `host[:port]`
/// replaces the host of the default.
fn endpoint_base_url(endpoint: &str) -> String {
    let Some(scheme_end) = endpoint.find("://") else {
        return format!("https://{endpoint}/bigquery/v2");
    };
    let after = scheme_end + 3;
    match endpoint[after..].rfind('/') {
        Some(i) => endpoint[..after + i].to_owned(),
        None => endpoint.to_owned(),
    }
}

/// `projects/{p}/datasets/{d}/tables/{t}` with each ID escaped.
fn table_path(project_id: &str, dataset_id: &str, table_id: &str) -> String {
    format!(
        "projects/{}/datasets/{}/tables/{}",
        escape(project_id),
        escape(dataset_id),
        escape(table_id)
    )
}

/// The escaping of a `{var}` path parameter: everything but ASCII letters, digits and `-._~` is
/// percent-encoded (upper-case hex).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Like [`escape`], with a space as `+`.
fn query_escape(s: &str) -> String {
    escape(s).replace("%20", "+")
}

/// The JSON request body: compact JSON with a trailing newline.
fn json_body(v: &Value) -> Result<Bytes, Error> {
    let mut out = serde_json::to_vec(v).map_err(Error::other)?;
    out.push(b'\n');
    Ok(Bytes::from(out))
}

/// Decodes a 2xx response other than 204 (which [`BigQueryClient::call`] turns into `{}`): the body
/// must be JSON.
fn decode<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, Error> {
    if body.is_empty() {
        // An empty 2xx body is an `EOF` error.
        return Err(Error::msg("EOF"));
    }
    serde_json::from_slice(body).map_err(Error::other)
}

/// The `tables` resource for the fields [`TableMetadata`] has.
fn table_resource(meta: &TableMetadata) -> Result<Map<String, Value>, Error> {
    // An empty schema counts as none: it is not sent as `"schema":{}` and is not rejected beside a
    // view query.
    if !meta.schema.is_empty() && !meta.view_query.is_empty() {
        return Err(Error::msg(
            "bigquery: provide Schema or ViewQuery, not both",
        ));
    }
    let mut t = Map::new();
    if !meta.labels.is_empty() {
        t.insert("labels".to_owned(), json!(meta.labels));
    }
    if !meta.schema.is_empty() {
        t.insert("schema".to_owned(), schema_value(&meta.schema)?);
    }
    if !meta.view_query.is_empty() {
        t.insert(
            "view".to_owned(),
            json!({"query": meta.view_query, "useLegacySql": false}),
        );
    }
    if let Some(p) = &meta.time_partitioning {
        t.insert("timePartitioning".to_owned(), time_partitioning_value(p));
    }
    if let Some(c) = &meta.clustering {
        let mut m = Map::new();
        if !c.fields.is_empty() {
            m.insert("fields".to_owned(), json!(c.fields));
        }
        t.insert("clustering".to_owned(), Value::Object(m));
    }
    if !meta.table_type.is_empty() {
        t.insert("type".to_owned(), json!(meta.table_type));
    }
    if !meta.etag.is_empty() {
        t.insert("etag".to_owned(), json!(meta.etag));
    }
    Ok(t)
}

/// The `tables.patch` body for a schema and set labels.
fn table_update_resource(update: &TableMetadataUpdate) -> Result<Value, Error> {
    let mut t = Map::new();
    if let Some(schema) = &update.schema {
        t.insert("schema".to_owned(), schema_value(schema)?);
    }
    if !update.set_labels.is_empty() {
        t.insert("labels".to_owned(), json!(update.set_labels));
    }
    Ok(Value::Object(t))
}

/// `{"fields": [...]}` (`{}` for an empty schema).
fn schema_value(schema: &Schema) -> Result<Value, Error> {
    serde_json::to_value(TableSchema {
        fields: schema.clone(),
    })
    .map_err(Error::other)
}

/// An empty type is `DAY`; `expirationMs` is an int64 string.
fn time_partitioning_value(p: &TimePartitioning) -> Value {
    let mut m = Map::new();
    let partition_type = if p.partition_type.is_empty() {
        DAY_PARTITIONING_TYPE
    } else {
        p.partition_type.as_str()
    };
    m.insert("type".to_owned(), json!(partition_type));
    let ms = p.expiration.as_millis();
    if ms != 0 {
        m.insert("expirationMs".to_owned(), json!(ms.to_string()));
    }
    if !p.field.is_empty() {
        m.insert("field".to_owned(), json!(p.field));
    }
    if p.require_partition_filter {
        m.insert("requirePartitionFilter".to_owned(), json!(true));
    }
    Value::Object(m)
}

/// The `tables` resource fields that [`TableMetadata`] has.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TableReply {
    schema: Option<TableSchema>,
    time_partitioning: Option<TimePartitioningReply>,
    clustering: Option<ClusteringReply>,
    #[serde(deserialize_with = "nullable")]
    labels: BTreeMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    etag: String,
    #[serde(rename = "type", deserialize_with = "nullable")]
    table_type: String,
    view: Option<ViewReply>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TimePartitioningReply {
    #[serde(rename = "type", deserialize_with = "nullable")]
    partition_type: String,
    expiration_ms: Option<Value>,
    #[serde(deserialize_with = "nullable")]
    field: String,
    #[serde(deserialize_with = "nullable")]
    require_partition_filter: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ClusteringReply {
    #[serde(deserialize_with = "nullable")]
    fields: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ViewReply {
    #[serde(deserialize_with = "nullable")]
    query: String,
}

/// The [`TableMetadata`] of a `tables` reply.
fn table_metadata_from_reply(body: &[u8]) -> Result<TableMetadata, Error> {
    let reply: TableReply = decode(body)?;
    let time_partitioning = match reply.time_partitioning {
        None => None,
        Some(p) => {
            // `expirationMs` is an int64 string. A bare JSON number is also accepted (a non-u64 one
            // as 0), and a negative string is a parse error (a `Duration` cannot be negative).
            let ms = match &p.expiration_ms {
                None | Some(Value::Null) => 0,
                Some(Value::String(s)) => s.parse::<u64>().map_err(Error::other)?,
                Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
                Some(other) => {
                    return Err(Error::msg(format!(
                        "invalid `timePartitioning.expirationMs`: expected an integer or an \
                         integer string, found {} ({other})",
                        gadk_platform::json_kind_article(other)
                    )));
                }
            };
            Some(TimePartitioning {
                partition_type: p.partition_type,
                expiration: Duration::from_millis(ms),
                field: p.field,
                require_partition_filter: p.require_partition_filter,
            })
        }
    };
    Ok(TableMetadata {
        schema: reply.schema.map(|s| s.fields).unwrap_or_default(),
        time_partitioning,
        clustering: reply.clustering.map(|c| Clustering { fields: c.fields }),
        labels: reply.labels,
        etag: reply.etag,
        table_type: reply.table_type,
        view_query: reply.view.map(|v| v.query).unwrap_or_default(),
    })
}

/// The `jobs` resource fields that are read.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JobReply {
    job_reference: Option<JobReferenceReply>,
    configuration: Option<JobConfigurationReply>,
    status: Option<JobStatusReply>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JobReferenceReply {
    #[serde(deserialize_with = "nullable")]
    project_id: String,
    #[serde(deserialize_with = "nullable")]
    job_id: String,
    #[serde(deserialize_with = "nullable")]
    location: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JobConfigurationReply {
    query: Option<Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JobStatusReply {
    #[serde(deserialize_with = "nullable")]
    state: String,
    error_result: Option<ErrorProtoReply>,
    #[serde(deserialize_with = "nullable")]
    errors: Vec<Option<ErrorProtoReply>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ErrorProtoReply {
    #[serde(deserialize_with = "nullable")]
    location: String,
    #[serde(deserialize_with = "nullable")]
    message: String,
    #[serde(deserialize_with = "nullable")]
    reason: String,
}

impl From<ErrorProtoReply> for BigQueryError {
    fn from(e: ErrorProtoReply) -> Self {
        BigQueryError {
            location: e.location,
            message: e.message,
            reason: e.reason,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct QueryResultsReply {
    #[serde(deserialize_with = "nullable")]
    job_complete: bool,
}

/// The status of a reply: `None` for a missing status; `unexpected job state: …` for a state other
/// than `PENDING` / `RUNNING` / `DONE`; the error result kept only once `DONE`.
fn job_status(status: Option<JobStatusReply>) -> Result<Option<JobStatus>, Error> {
    let Some(status) = status else {
        return Ok(None);
    };
    if !matches!(status.state.as_str(), "PENDING" | "RUNNING" | "DONE") {
        return Err(Error::msg(format!(
            "unexpected job state: {}",
            status.state
        )));
    }
    let done = status.state == "DONE";
    Ok(Some(JobStatus {
        error_result: status
            .error_result
            .filter(|_| done)
            .map(BigQueryError::from),
        // A null entry of `status.errors` is skipped.
        errors: status
            .errors
            .into_iter()
            .flatten()
            .map(BigQueryError::from)
            .collect(),
        state: status.state,
    }))
}

/// Whether `err` is worth retrying: a Google API error whose first reason is allowed or whose code
/// is 500/502/503/504; a context deadline; a transport failure that refused or reset the
/// connection, timed out, or cut a body short.
pub(crate) fn retryable_error(err: &Error, reasons: &[&str]) -> bool {
    // The gadk transport's HTTP/2 failures are retried only as a refused or reset connection or a
    // timeout. (The client library also retries an error whose text is exactly `http2: stream
    // closed` or `http2: client connection lost`, which this transport does not produce.)
    match err {
        Error::Api(api) => {
            api.errors
                .first()
                .is_some_and(|item| reasons.contains(&item.reason.as_str()))
                || RETRY_5XX_CODES.contains(&api.code)
        }
        Error::Context(c) => *c == ContextError::DeadlineExceeded,
        Error::Rest(rest) => match rest {
            RestError::ResponseHeaderTimeout
            | RestError::DeadlineExceeded
            | RestError::DroppedBody { .. } => true,
            RestError::Transport(e) => connection_error(e),
            // A failed credential fetch is checked like any transport failure.
            RestError::Headers(e) => connection_error(e.as_ref()),
        },
        Error::Wrapped { source, .. } => retryable_error(source, reasons),
        _ => false,
    }
}

/// Whether a connection was refused or reset (`connection refused` / `connection reset` in the
/// text) or timed out, anywhere in the error's source chain.
fn connection_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::TimedOut
            )
        {
            return true;
        }
        let text = e.to_string();
        if text.contains("connection refused") || text.contains("connection reset") {
            return true;
        }
        cur = e.source();
    }
    false
}

/// The retry backoff (the BigQuery SLA suggestion): 1 s → 32 s, ×2.
const BIGQUERY_BACKOFF: Backoff = Backoff {
    initial: Duration::from_secs(1),
    max: Duration::from_secs(32),
    multiplier: 2.0,
};

/// What one turn of [`retry`] decided.
enum Step<T> {
    /// Stop with this result.
    Stop(Result<T, Error>),
    /// Try again after a pause; the error (if any) is remembered as the last one.
    Again(Option<Error>),
}

/// Runs `f` until it stops; between turns it pauses per `backoff` under `ctx`. When `ctx` ends
/// during a pause the result is the context error, or the wrapped call error
/// (`retry failed with <ctx err>; last error: <err>`) when a turn failed with an error other than a
/// bare context error.
async fn retry<T, F, Fut>(ctx: &Context, backoff: &mut Pauses, mut f: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Step<T>>,
{
    let mut last_err: Option<Error> = None;
    loop {
        match f().await {
            Step::Stop(result) => return result,
            Step::Again(err) => {
                if let Some(err) = err
                    && !matches!(err, Error::Context(_))
                {
                    last_err = Some(err);
                }
            }
        }
        if let Err(ctx_err) = sleep(ctx, backoff.pause()).await {
            return Err(match last_err {
                Some(last) => wrapped_call_err(ctx_err, last),
                None => ctx_err,
            });
        }
    }
}

/// `call` until it succeeds or fails with an error [`retryable_error`] rejects, with
/// [`BIGQUERY_BACKOFF`].
async fn run_with_retry<T, F, Fut>(ctx: &Context, reasons: &[&str], mut call: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let mut backoff = Pauses::new(BIGQUERY_BACKOFF);
    retry(ctx, &mut backoff, || {
        let fut = call();
        async move {
            match fut.await {
                Ok(v) => Step::Stop(Ok(v)),
                Err(e) if retryable_error(&e, reasons) => Step::Again(Some(e)),
                Err(e) => Step::Stop(Err(e)),
            }
        }
    })
    .await
}

/// One table's metadata RPCs.
#[derive(Debug, Clone)]
pub struct TableOps {
    pub(crate) client: Arc<BigQueryClient>,
    pub(crate) project_id: String,
    pub(crate) dataset_id: String,
    pub(crate) table_id: String,
}

impl TableOps {
    /// The table `project_id.dataset_id.table_id` through `client`.
    pub fn new(
        client: Arc<BigQueryClient>,
        project_id: impl Into<String>,
        dataset_id: impl Into<String>,
        table_id: impl Into<String>,
    ) -> TableOps {
        TableOps {
            client,
            project_id: project_id.into(),
            dataset_id: dataset_id.into(),
            table_id: table_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl TableMetadataOps for TableOps {
    async fn metadata(&self, ctx: &Context) -> Result<TableMetadata, Error> {
        self.client
            .get_table(ctx, &self.project_id, &self.dataset_id, &self.table_id)
            .await
    }

    async fn create(&self, ctx: &Context, meta: &TableMetadata) -> Result<(), Error> {
        self.client
            .insert_table(
                ctx,
                &self.project_id,
                &self.dataset_id,
                &self.table_id,
                meta,
            )
            .await
            .map(drop)
    }

    async fn update(
        &self,
        ctx: &Context,
        update: &TableMetadataUpdate,
        etag: &str,
    ) -> Result<TableMetadata, Error> {
        self.client
            .patch_table(
                ctx,
                &self.project_id,
                &self.dataset_id,
                &self.table_id,
                update,
                etag,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::HashMap;

    use gadk_model::rest::googleapi::api_error_from_response;
    use gadk_model::rest::test_util::{FakeResponse, ScriptedClient, bearer_credentials};
    use gadk_platform::env::MapEnv;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::error::{
        ApiError, Code, ErrorItem, is_conflict_error, is_not_found_error,
        is_precondition_failed_error,
    };

    fn parts(status: u16) -> http::response::Parts {
        let (mut parts, ()) = http::Response::new(()).into_parts();
        parts.status = http::StatusCode::from_u16(status).unwrap();
        parts
    }

    fn api(code: u16, reason: &str) -> Error {
        Error::from(ApiError {
            code,
            message: "m".into(),
            errors: if reason.is_empty() {
                Vec::new()
            } else {
                vec![ErrorItem {
                    reason: reason.into(),
                    message: "m".into(),
                }]
            },
            ..ApiError::default()
        })
    }

    fn scripted_client(responses: Vec<FakeResponse>) -> (BigQueryClient, Arc<ScriptedClient>) {
        let http = ScriptedClient::sequence(responses);
        let options = ClientOptions {
            http_client: Some(http.clone()),
            without_authentication: true,
            endpoint: Some("http://bq.test/bigquery/v2/".into()),
            ..ClientOptions::default()
        };
        (BigQueryClient::new("p", &options).unwrap(), http)
    }

    #[test]
    fn endpoint_base_urls_resolve_like_go() {
        for (endpoint, want) in [
            (
                "http://127.0.0.1:9050/bigquery/v2/",
                "http://127.0.0.1:9050/bigquery/v2",
            ),
            (
                "http://127.0.0.1:9050/bigquery/v2",
                "http://127.0.0.1:9050/bigquery",
            ),
            ("https://bq.example/", "https://bq.example"),
            ("https://bq.example", "https://bq.example"),
            ("bq.example:8443", "https://bq.example:8443/bigquery/v2"),
        ] {
            assert_eq!(endpoint_base_url(endpoint), want, "{endpoint}");
        }
        let client = BigQueryClient::new(
            "p",
            &ClientOptions {
                without_authentication: true,
                ..ClientOptions::default()
            },
        )
        .unwrap();
        assert_eq!(client.base_url, DEFAULT_BASE_URL);
        assert_eq!(client.project_id(), "p");
    }

    #[test]
    fn urls_escape_ids_and_sort_the_query() {
        assert_eq!(escape("example.com:proj-1_x~"), "example.com%3Aproj-1_x~");
        assert_eq!(escape("a b/é"), "a%20b%2F%C3%A9");
        assert_eq!(query_escape("status,statistics x"), "status%2Cstatistics+x");
        let (client, _) = scripted_client(vec![FakeResponse::json("{}")]);
        assert_eq!(
            client.url("projects/p/jobs/j", &[("location", ""), ("fields", "a,b")]),
            "http://bq.test/bigquery/v2/projects/p/jobs/j?alt=json&fields=a%2Cb&location=&\
             prettyPrint=false"
        );
    }

    #[test]
    fn options_without_authentication_and_credentials_conflict() {
        let err = BigQueryClient::new(
            "p",
            &ClientOptions {
                credentials: Some(bearer_credentials("t")),
                without_authentication: true,
                ..ClientOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "bigquery: constructing client: options.WithoutAuthentication is incompatible with \
             any option that provides credentials"
        );
    }

    #[tokio::test]
    async fn unreadable_adc_file_fails_construction() {
        let env = MapEnv(HashMap::from([(
            "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
            "/nonexistent/gadk-agentanalytics-adc.json".to_owned(),
        )]));
        let err = BigQueryClient::new_with_env("p", &ClientOptions::default(), &env).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("bigquery: constructing client: "),
            "{err}"
        );
    }

    /// Go `htransport.NewClient`: a `WithHTTPClient` client is used as is, so construction
    /// needs no Application Default Credentials and requests carry no `Authorization`, even
    /// with credentials given.
    #[tokio::test]
    async fn a_caller_http_client_skips_credentials() {
        let env = MapEnv(HashMap::from([(
            "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
            "/nonexistent/gadk-agentanalytics-adc.json".to_owned(),
        )]));
        for credentials in [None, Some(bearer_credentials("t"))] {
            let http = ScriptedClient::json(r#"{"tableReference":{"tableId":"t"}}"#);
            let options = ClientOptions {
                credentials,
                http_client: Some(http.clone()),
                endpoint: Some("http://bq.test/bigquery/v2/".into()),
                ..ClientOptions::default()
            };
            let client = BigQueryClient::new_with_env("p", &options, &env).unwrap();
            client
                .get_table(&Context::background(), "p", "d", "t")
                .await
                .unwrap();
            assert!(
                !http.last_request().headers.contains_key("authorization"),
                "{:?}",
                http.last_request().headers
            );
        }
    }

    #[test]
    fn api_errors_follow_check_response() {
        let body = br#"{"error":{"code":404,"message":"Not found: Table p:d.t","errors":[{"domain":"global","reason":"notFound","message":"Not found: Table p:d.t"}],"status":"NOT_FOUND"}}"#;
        let err = api_error_from_response(&parts(404), body);
        assert_eq!(err.code, 404);
        assert_eq!(err.body, String::from_utf8_lossy(body));
        assert_eq!(
            err.to_string(),
            "googleapi: Error 404: Not found: Table p:d.t, notFound"
        );
        assert!(is_not_found_error(&Error::from(err)));

        // The body's code wins over the status; a JSON array's first element is read.
        let err = api_error_from_response(
            &parts(400),
            br#"[{"error":{"code":409,"message":"dup","errors":[{"reason":"duplicate","message":"dup"}]}}]"#,
        );
        assert_eq!(err.code, 409);
        assert!(is_conflict_error(&Error::from(err)));

        // A zero code takes the status.
        let err = api_error_from_response(
            &parts(412),
            br#"{"error":{"message":"Precondition check failed."}}"#,
        );
        assert_eq!(err.code, 412);
        assert_eq!(
            err.to_string(),
            "googleapi: Error 412: Precondition check failed."
        );
        assert!(is_precondition_failed_error(&Error::from(err)));

        // Not an error reply: the status and the raw body.
        for body in [
            &b"upstream down"[..],
            br#"{"error":null}"#,
            br#"{"error":"x"}"#,
            b"",
        ] {
            let err = api_error_from_response(&parts(502), body);
            assert_eq!(err.code, 502);
            assert_eq!(
                err.to_string(),
                format!(
                    "googleapi: got HTTP response code 502 with body: {}",
                    String::from_utf8_lossy(body)
                )
            );
        }
    }

    #[test]
    fn retryable_errors_match_go() {
        for code in [500, 502, 503, 504] {
            assert!(
                retryable_error(&api(code, ""), DEFAULT_RETRY_REASONS),
                "{code}"
            );
        }
        for code in [400, 403, 404, 409, 412, 429, 501] {
            assert!(
                !retryable_error(&api(code, ""), DEFAULT_RETRY_REASONS),
                "{code}"
            );
        }
        assert!(retryable_error(
            &api(403, "rateLimitExceeded"),
            DEFAULT_RETRY_REASONS
        ));
        assert!(retryable_error(
            &api(400, "backendError"),
            DEFAULT_RETRY_REASONS
        ));
        assert!(!retryable_error(
            &api(400, "internalError"),
            DEFAULT_RETRY_REASONS
        ));
        assert!(retryable_error(
            &api(400, "internalError"),
            JOB_RETRY_REASONS
        ));
        assert!(retryable_error(
            &api(403, "jobRateLimitExceeded"),
            JOB_RETRY_REASONS
        ));
        assert!(!retryable_error(&api(409, "duplicate"), JOB_RETRY_REASONS));
        assert!(retryable_error(
            &Error::Context(ContextError::DeadlineExceeded),
            DEFAULT_RETRY_REASONS
        ));
        assert!(!retryable_error(
            &Error::Context(ContextError::Canceled),
            DEFAULT_RETRY_REASONS
        ));
        assert!(retryable_error(
            &Error::wrap("x", api(503, "")),
            DEFAULT_RETRY_REASONS
        ));
        let refused = gadk_model::ModelError::Transport(Box::new(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )));
        assert!(retryable_error(
            &Error::Rest(RestError::Transport(refused)),
            &[]
        ));
        let other = gadk_model::ModelError::Transport(Box::new(std::io::Error::other("tls")));
        assert!(!retryable_error(
            &Error::Rest(RestError::Transport(other)),
            &[]
        ));
        assert!(!retryable_error(
            &Error::Status {
                code: Code::Unavailable,
                message: String::new()
            },
            DEFAULT_RETRY_REASONS
        ));
    }

    /// Go decodes a `null` entry of `error.errors` into a zero `ErrorItem` and keeps it, so it
    /// is the first reason the retry check reads and a line of "More details".
    #[test]
    fn api_error_keeps_null_error_items_as_zero_items() {
        let body = br#"{"error":{"code":403,"message":"slow down","errors":[null,{"reason":"rateLimitExceeded","message":"m"}]}}"#;
        let err = api_error_from_response(&parts(403), body);
        assert_eq!(
            err.errors,
            vec![
                ErrorItem::default(),
                ErrorItem {
                    reason: "rateLimitExceeded".into(),
                    message: "m".into(),
                },
            ]
        );
        assert!(!retryable_error(
            &Error::from(err.clone()),
            DEFAULT_RETRY_REASONS
        ));
        assert_eq!(
            err.to_string(),
            "googleapi: Error 403: slow down\nMore details:\nReason: , Message: \nReason: rateLimitExceeded, Message: m\n"
        );
    }

    /// Go retries a credential fetch that failed with a `*url.Error` refusing or resetting the
    /// connection, like any other transport failure.
    #[test]
    fn retryable_headers_error_is_a_connection_error() {
        let refused = || {
            RestError::Headers(Box::new(std::io::Error::other(
                "Post \"https://oauth2.googleapis.com/token\": dial tcp: connection refused",
            )))
        };
        assert!(retryable_error(&Error::Rest(refused()), &[]));
        let reset = RestError::Headers(Box::new(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset,
        )));
        assert!(retryable_error(&Error::Rest(reset), &[]));
        let denied =
            RestError::Headers(Box::new(std::io::Error::other("oauth2: \"invalid_grant\"")));
        assert!(!retryable_error(&Error::Rest(denied), &[]));
    }

    #[test]
    fn bigquery_backoff_pauses_like_gax() {
        let mut b = Pauses::new(BIGQUERY_BACKOFF);
        let caps: Vec<u64> = (0..8).map(|_| b.next(|n| n - 1).as_secs()).collect();
        assert_eq!(caps, [1, 2, 4, 8, 16, 32, 32, 32]);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_end_with_go_wrapped_call_error() {
        let unavailable = r#"{"error":{"code":503,"message":"Backend error","errors":[{"reason":"backendError","message":"Backend error"}]}}"#;
        let (client, http) = scripted_client(vec![FakeResponse::json_status(503, unavailable)]);
        let ctx = Context::background().with_timeout(Duration::from_secs(5));
        let err = client.get_table(&ctx, "p", "d", "t").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "retry failed with context deadline exceeded; last error: googleapi: Error 503: \
             Backend error, backendError"
        );
        assert!(err.is_context(ContextError::DeadlineExceeded));
        assert!(crate::error::has_error_code(&err, 503, Code::Unavailable));
        assert!(http.hits() >= 3, "{}", http.hits());

        // A canceled context before any failure is the bare context error.
        let (client, http) = scripted_client(vec![FakeResponse::json("{}")]);
        let ctx = Context::background().with_cancel();
        ctx.cancel();
        let err = client.get_table(&ctx, "p", "d", "t").await.unwrap_err();
        assert!(
            matches!(err, Error::Context(ContextError::Canceled)),
            "{err:?}"
        );
        assert_eq!(http.hits(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn non_query_job_wait_polls_until_done() {
        let running = r#"{"jobReference":{"projectId":"p","jobId":"j","location":"EU"},"status":{"state":"RUNNING"}}"#;
        let done = r#"{"status":{"state":"DONE","errorResult":{"reason":"invalid","message":"bad","location":"q"},"errors":[{"reason":"invalid","message":"bad"},null]}}"#;
        let (client, http) = scripted_client(vec![
            FakeResponse::json(running),
            FakeResponse::json(running),
            FakeResponse::json(done),
        ]);
        let ctx = Context::background();
        let job = client.insert_query_job(&ctx, "SELECT 1").await.unwrap();
        assert!(!job.is_query);
        assert_eq!(
            job.reference,
            JobReference {
                project_id: "p".into(),
                job_id: "j".into(),
                location: "EU".into()
            }
        );
        let status = client.wait_job(&ctx, &job).await.unwrap();
        assert!(status.done());
        assert_eq!(
            status.err().unwrap().to_string(),
            r#"{Location: "q"; Message: "bad"; Reason: "invalid"}"#
        );
        assert_eq!(status.errors.len(), 1);
        let requests = http.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[2].url,
            "http://bq.test/bigquery/v2/projects/p/jobs/j?alt=json&fields=status%2Cstatistics&\
             location=EU&prettyPrint=false"
        );
        // Go's gax.Backoff{} pauses at most 1 s before the first poll after RUNNING.
        assert!(requests[2].at - requests[1].at <= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn job_status_rejects_unknown_states() {
        let (client, _) = scripted_client(vec![FakeResponse::json(
            r#"{"jobReference":{"projectId":"p","jobId":"j"},"status":{"state":"WEIRD"}}"#,
        )]);
        let err = client
            .insert_query_job(&Context::background(), "SELECT 1")
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unexpected job state: WEIRD");
        // The error result counts only once the job is done (Go `setStatus`).
        let status = job_status(Some(JobStatusReply {
            state: "RUNNING".into(),
            error_result: Some(ErrorProtoReply::default()),
            errors: Vec::new(),
        }))
        .unwrap()
        .unwrap();
        assert!(status.err().is_none());
    }

    #[tokio::test]
    async fn table_resources_follow_go_to_bq() {
        let meta = TableMetadata {
            schema: vec![crate::schema::FieldSchema {
                name: "a".into(),
                field_type: "STRING".into(),
                ..crate::schema::FieldSchema::default()
            }],
            view_query: "SELECT 1".into(),
            ..TableMetadata::default()
        };
        let (client, http) = scripted_client(vec![FakeResponse::json("{}")]);
        let ctx = Context::background();
        let err = client
            .insert_table(&ctx, "p", "d", "t", &meta)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "bigquery: provide Schema or ViewQuery, not both"
        );
        assert_eq!(http.hits(), 0);

        let meta = TableMetadata {
            time_partitioning: Some(TimePartitioning {
                expiration: Duration::from_secs(2),
                require_partition_filter: true,
                ..TimePartitioning::default()
            }),
            clustering: Some(Clustering::default()),
            view_query: "SELECT 1".into(),
            ..TableMetadata::default()
        };
        assert_eq!(
            Value::Object(table_resource(&meta).unwrap()),
            json!({
                "timePartitioning": {"type": "DAY", "expirationMs": "2000", "requirePartitionFilter": true},
                "clustering": {},
                "view": {"query": "SELECT 1", "useLegacySql": false},
            })
        );
        assert_eq!(
            table_update_resource(&TableMetadataUpdate {
                schema: Some(Vec::new()),
                set_labels: BTreeMap::new(),
            })
            .unwrap(),
            json!({"schema": {}})
        );

        // A 204 decodes to empty metadata; partition expiry comes back from its string form.
        let (client, _) = scripted_client(vec![
            FakeResponse::new(204),
            FakeResponse::json(
                r#"{"timePartitioning":{"type":"DAY","expirationMs":"1500","field":"ts"},"labels":null,"clustering":{"fields":["a"]}}"#,
            ),
        ]);
        assert_eq!(
            client.get_table(&ctx, "p", "d", "t").await.unwrap(),
            TableMetadata::default()
        );
        let md = client.get_table(&ctx, "p", "d", "t").await.unwrap();
        assert_eq!(
            md.time_partitioning,
            Some(TimePartitioning {
                partition_type: "DAY".into(),
                expiration: Duration::from_millis(1500),
                field: "ts".into(),
                require_partition_filter: false,
            })
        );
        assert_eq!(
            md.clustering,
            Some(Clustering {
                fields: vec!["a".into()]
            })
        );
    }

    #[test]
    fn job_status_done_and_err() {
        let running = JobStatus {
            state: "RUNNING".into(),
            ..JobStatus::default()
        };
        assert!(!running.done());
        assert!(running.err().is_none());
        let failed = JobStatus {
            state: "DONE".into(),
            error_result: Some(BigQueryError {
                location: String::new(),
                message: "boom".into(),
                reason: "invalidQuery".into(),
            }),
            errors: Vec::new(),
        };
        assert!(failed.done());
        assert_eq!(
            failed.err().map(|e| e.to_string()).as_deref(),
            Some(r#"{Location: ""; Message: "boom"; Reason: "invalidQuery"}"#)
        );
    }
}
