// Copyright 2026 Chris Olson
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

//! A small BigQuery v2 REST client: datasets, tables, dry runs and queries.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gadk_model::{HttpClient, ModelError, ReqwestClient, collect_body, transport_error_detail};
use http::{HeaderValue, Method, header};
use serde_json::{Map, Value, json};

use crate::auth::{Adc, AuthHeaders};
use crate::error::{BigQueryError, api_error};
use crate::rows::decode_rows;
use crate::types::{DryRunInfo, QueryOptions, QueryResult, SessionInfo, TableRef};

/// The BigQuery v2 REST endpoint.
pub const ENDPOINT: &str = "https://bigquery.googleapis.com/bigquery/v2";

/// The per-request timeout of the default HTTP client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one `jobs.query` / `jobs.getQueryResults` call waits for the job, in ms.
const WAIT_MS: u64 = 10_000;

/// The most `jobs.getQueryResults` polls for a query that has not completed.
const MAX_POLLS: usize = 30;

/// The most items a list call returns.
const MAX_LIST: usize = 1000;

/// A BigQuery v2 REST client.
#[non_exhaustive]
pub struct BigQueryClient {
    http: Arc<dyn HttpClient>,
    auth: Arc<dyn AuthHeaders>,
    base: String,
}

impl std::fmt::Debug for BigQueryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigQueryClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Default for BigQueryClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BigQueryClient {
    /// The real endpoint, a reqwest client with a 60 s timeout and lazily built Application
    /// Default Credentials ([`Adc::lazy`]).
    pub fn new() -> Self {
        Self::with_auth(Arc::new(Adc::lazy()))
    }

    /// The real endpoint and a reqwest client with a 60 s timeout, authenticated by `auth`.
    pub fn with_auth(auth: Arc<dyn AuthHeaders>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self::with_client(Arc::new(ReqwestClient::new(client)), auth)
    }

    /// The real endpoint with an explicit HTTP client and credentials.
    pub fn with_client(http: Arc<dyn HttpClient>, auth: Arc<dyn AuthHeaders>) -> Self {
        Self::with_parts(http, auth, ENDPOINT)
    }

    /// Every part given: the HTTP client, the credentials and the endpoint `base` (such as
    /// [`ENDPOINT`], without a trailing `/`).
    pub fn with_parts(
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn AuthHeaders>,
        base: impl Into<String>,
    ) -> Self {
        Self {
            http,
            auth,
            base: base.into(),
        }
    }

    /// The dataset ids of `project` (`datasets.list`, every page, at most 1,000).
    ///
    /// # Errors
    ///
    /// Any [`BigQueryError`] of the calls.
    pub async fn list_datasets(&self, project: &str) -> Result<Vec<String>, BigQueryError> {
        let url = format!(
            "{}/projects/{project}/datasets?all=false&maxResults=1000",
            self.base
        );
        self.list(&url, "datasets", "/datasetReference/datasetId")
            .await
    }

    /// The `datasets.get` resource of `project.dataset`.
    ///
    /// # Errors
    ///
    /// Any [`BigQueryError`] of the call.
    pub async fn get_dataset(&self, project: &str, dataset: &str) -> Result<Value, BigQueryError> {
        let url = format!("{}/projects/{project}/datasets/{dataset}", self.base);
        self.call(Method::GET, &url, None).await
    }

    /// The table ids of `project.dataset` (`tables.list`, every page, at most 1,000).
    ///
    /// # Errors
    ///
    /// Any [`BigQueryError`] of the calls.
    pub async fn list_tables(
        &self,
        project: &str,
        dataset: &str,
    ) -> Result<Vec<String>, BigQueryError> {
        let url = format!(
            "{}/projects/{project}/datasets/{dataset}/tables?maxResults=1000",
            self.base
        );
        self.list(&url, "tables", "/tableReference/tableId").await
    }

    /// The `tables.get` resource of `project.dataset.table`.
    ///
    /// # Errors
    ///
    /// Any [`BigQueryError`] of the call.
    pub async fn get_table(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
    ) -> Result<Value, BigQueryError> {
        let url = format!(
            "{}/projects/{project}/datasets/{dataset}/tables/{table}",
            self.base
        );
        self.call(Method::GET, &url, None).await
    }

    /// Dry-runs `sql` as a standard-SQL query job in `compute_project` (`jobs.insert` with
    /// `dryRun`): its statement type and destination, without running it.
    ///
    /// # Errors
    ///
    /// Any [`BigQueryError`] of the call; [`BigQueryError::Decode`] when the reply has no
    /// statement type.
    pub async fn dry_run(
        &self,
        compute_project: &str,
        sql: &str,
        opts: &QueryOptions,
    ) -> Result<DryRunInfo, BigQueryError> {
        let mut job_ref = json!({ "projectId": compute_project });
        if let Some(loc) = &opts.location {
            job_ref["location"] = json!(loc);
        }
        let mut query = json!({ "query": sql, "useLegacySql": false });
        if let Some(bytes) = opts.maximum_bytes_billed {
            query["maximumBytesBilled"] = json!(bytes.to_string());
        }
        if let Some(id) = &opts.session_id {
            query["connectionProperties"] = json!([{ "key": "session_id", "value": id }]);
        }
        let body = json!({
            "jobReference": job_ref,
            "configuration": { "dryRun": true, "labels": opts.labels, "query": query },
        });
        let url = format!("{}/projects/{compute_project}/jobs", self.base);
        let job = job_ok(self.call(Method::POST, &url, Some(body)).await?)?;
        let statement_type = job
            .pointer("/statistics/query/statementType")
            .and_then(Value::as_str)
            .ok_or_else(|| BigQueryError::Decode("the dry run has no statement type".into()))?;
        let destination = job
            .pointer("/configuration/query/destinationTable")
            .and_then(table_ref)
            .or_else(|| {
                job.pointer("/statistics/query/ddlTargetTable")
                    .and_then(table_ref)
            });
        Ok(DryRunInfo::new(statement_type, destination))
    }

    /// Runs `sql` as standard SQL in `compute_project` (`jobs.query`, polling
    /// `jobs.getQueryResults` until it completes).
    ///
    /// # Errors
    ///
    /// Any [`BigQueryError`] of the calls; [`BigQueryError::Job`] when the query does not
    /// complete within 30 polls; [`BigQueryError::Decode`] for a reply without the fields read.
    pub async fn query(
        &self,
        compute_project: &str,
        sql: &str,
        opts: &QueryOptions,
    ) -> Result<QueryResult, BigQueryError> {
        let mut body = json!({
            "query": sql,
            "useLegacySql": false,
            "timeoutMs": WAIT_MS,
            "formatOptions": { "useInt64Timestamp": true },
            "labels": opts.labels,
        });
        if let Some(n) = opts.max_rows {
            body["maxResults"] = json!(n);
        }
        if let Some(loc) = &opts.location {
            body["location"] = json!(loc);
        }
        if let Some(bytes) = opts.maximum_bytes_billed {
            body["maximumBytesBilled"] = json!(bytes.to_string());
        }
        if let Some(id) = &opts.session_id {
            body["connectionProperties"] = json!([{ "key": "session_id", "value": id }]);
        }
        if opts.create_session {
            body["createSession"] = json!(true);
        }
        let url = format!("{}/projects/{compute_project}/queries", self.base);
        let first = job_ok(self.call(Method::POST, &url, Some(body)).await?)?;
        let job_ref = first.get("jobReference").cloned().unwrap_or(Value::Null);
        let session_id = first
            .pointer("/sessionInfo/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let mut reply = first;
        let mut polls = 0;
        while reply.get("jobComplete").and_then(Value::as_bool) != Some(true) {
            if polls == MAX_POLLS {
                return Err(BigQueryError::Job("query did not complete".into()));
            }
            polls += 1;
            reply = job_ok(
                self.call(Method::GET, &self.results_url(&job_ref, opts)?, None)
                    .await?,
            )?;
        }

        let rows = decode_rows(reply.get("schema"), reply.get("rows"))?;
        let total_rows = reply
            .get("totalRows")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok());
        let has_more =
            reply.get("pageToken").is_some() || total_rows.is_some_and(|t| t > rows.len() as u64);
        let session = if opts.create_session {
            Some(
                self.session_info(compute_project, &job_ref, session_id)
                    .await?,
            )
        } else {
            None
        };
        Ok(QueryResult {
            rows,
            total_rows,
            has_more,
            session,
        })
    }

    /// The `jobs.getQueryResults` URL of the job in `job_ref`.
    fn results_url(&self, job_ref: &Value, opts: &QueryOptions) -> Result<String, BigQueryError> {
        let project = job_ref.get("projectId").and_then(Value::as_str);
        let job = job_ref.get("jobId").and_then(Value::as_str);
        let (Some(project), Some(job)) = (project, job) else {
            return Err(BigQueryError::Decode(
                "the query reply has no job reference".into(),
            ));
        };
        let mut url = format!("{}/projects/{project}/queries/{}?", self.base, encode(job));
        if let Some(loc) = job_ref.get("location").and_then(Value::as_str) {
            url.push_str(&format!("location={}&", encode(loc)));
        }
        if let Some(n) = opts.max_rows {
            url.push_str(&format!("maxResults={n}&"));
        }
        url.push_str(&format!(
            "timeoutMs={WAIT_MS}&formatOptions.useInt64Timestamp=true"
        ));
        Ok(url)
    }

    /// The session a `createSession` query started: its id and anonymous dataset (`jobs.get`).
    async fn session_info(
        &self,
        compute_project: &str,
        job_ref: &Value,
        session_id: Option<String>,
    ) -> Result<SessionInfo, BigQueryError> {
        let session_id = session_id
            .ok_or_else(|| BigQueryError::Decode("the reply has no session id".into()))?;
        let job = job_ref
            .get("jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| BigQueryError::Decode("the query reply has no job reference".into()))?;
        let mut url = format!(
            "{}/projects/{compute_project}/jobs/{}?",
            self.base,
            encode(job)
        );
        if let Some(loc) = job_ref.get("location").and_then(Value::as_str) {
            url.push_str(&format!("location={}&", encode(loc)));
        }
        url.push_str("fields=configuration.query.destinationTable");
        let reply = job_ok(self.call(Method::GET, &url, None).await?)?;
        let dataset = reply
            .pointer("/configuration/query/destinationTable/datasetId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BigQueryError::Decode("the session job has no destination dataset".into())
            })?;
        Ok(SessionInfo::new(compute_project, session_id, dataset))
    }

    /// Every page of a list call: the strings at `id_pointer` of each `items_key` entry.
    async fn list(
        &self,
        first_url: &str,
        items_key: &str,
        id_pointer: &str,
    ) -> Result<Vec<String>, BigQueryError> {
        let mut out = Vec::new();
        let mut url = first_url.to_owned();
        loop {
            let page = self.call(Method::GET, &url, None).await?;
            for item in page
                .get(items_key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(id) = item.pointer(id_pointer).and_then(Value::as_str) {
                    out.push(id.to_owned());
                }
            }
            if out.len() >= MAX_LIST {
                out.truncate(MAX_LIST);
                return Ok(out);
            }
            match page.get("nextPageToken").and_then(Value::as_str) {
                Some(token) => url = format!("{first_url}&pageToken={}", encode(token)),
                None => return Ok(out),
            }
        }
    }

    /// Sends one request with an optional JSON body; the 2xx JSON reply, or the mapped error.
    async fn call(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> Result<Value, BigQueryError> {
        let mut headers = self
            .auth
            .headers()
            .await
            .map_err(BigQueryError::Credentials)?;
        let bytes = match &body {
            Some(v) => {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                Bytes::from(v.to_string())
            }
            None => Bytes::new(),
        };
        let mut req = http::Request::builder()
            .method(method)
            .uri(url)
            .body(bytes)
            .map_err(|e| BigQueryError::Http(format!("request {url}: {e}")))?;
        *req.headers_mut() = headers;
        let resp = self.http.send(req).await.map_err(transport)?;
        let status = resp.status();
        let body = collect_body(resp.into_body()).await.map_err(transport)?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body));
        }
        if body.is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        serde_json::from_slice(&body).map_err(|e| BigQueryError::Decode(e.to_string()))
    }
}

/// `status.errorResult` of a job reply as [`BigQueryError::Job`], or `Ok(reply)`.
fn job_ok(reply: Value) -> Result<Value, BigQueryError> {
    match reply
        .pointer("/status/errorResult/message")
        .and_then(Value::as_str)
    {
        Some(message) => Err(BigQueryError::Job(message.to_owned())),
        None => Ok(reply),
    }
}

/// A `{projectId, datasetId, tableId}` reference.
fn table_ref(v: &Value) -> Option<TableRef> {
    Some(TableRef::new(
        v.get("projectId")?.as_str()?,
        v.get("datasetId")?.as_str()?,
        v.get("tableId")?.as_str()?,
    ))
}

fn transport(err: ModelError) -> BigQueryError {
    BigQueryError::Http(match err {
        ModelError::Transport(e) => transport_error_detail(e),
        other => other.to_string(),
    })
}

/// Percent-encodes every byte except the RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _ ~`).
fn encode(s: &str) -> String {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;

    use futures::StreamExt as _;
    use gadk_model::BodyStream;
    use http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::auth::NoAuth;

    /// One request the fake client saw.
    pub(crate) struct Seen {
        pub(crate) method: Method,
        pub(crate) uri: String,
        pub(crate) body: Bytes,
    }

    impl Seen {
        pub(crate) fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    /// Records requests and answers each with the next scripted `(status, body)`.
    #[derive(Default)]
    pub(crate) struct FakeHttp {
        replies: Mutex<VecDeque<(u16, String)>>,
        pub(crate) seen: Mutex<Vec<Seen>>,
    }

    impl FakeHttp {
        pub(crate) fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(
                    replies
                        .into_iter()
                        .map(|(s, b)| (s, b.to_string()))
                        .collect(),
                ),
                seen: Mutex::default(),
            })
        }

        pub(crate) fn uris(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.uri.clone())
                .collect()
        }

        pub(crate) fn body(&self, i: usize) -> Value {
            self.seen.lock().unwrap()[i].json()
        }
    }

    #[async_trait::async_trait]
    impl HttpClient for FakeHttp {
        async fn send(
            &self,
            req: http::Request<Bytes>,
        ) -> Result<http::Response<BodyStream>, ModelError> {
            let (parts, body) = req.into_parts();
            self.seen.lock().unwrap().push(Seen {
                method: parts.method,
                uri: parts.uri.to_string(),
                body,
            });
            let (status, text) = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unscripted request");
            let stream: BodyStream =
                futures::stream::once(async move { Ok(Bytes::from(text)) }).boxed();
            let mut resp = http::Response::new(stream);
            *resp.status_mut() = StatusCode::from_u16(status).unwrap();
            Ok(resp)
        }
    }

    const B: &str = "http://bq.test/bigquery/v2";

    pub(crate) fn client(fake: &Arc<FakeHttp>) -> BigQueryClient {
        BigQueryClient::with_parts(fake.clone(), Arc::new(NoAuth), B)
    }

    #[tokio::test]
    async fn list_datasets_follows_pages() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "datasets": [{ "datasetReference": { "datasetId": "a" } }], "nextPageToken": "p/2" }),
            ),
            (
                200,
                json!({ "datasets": [{ "datasetReference": { "datasetId": "b" } }] }),
            ),
        ]);
        let ids = client(&fake).list_datasets("proj").await.unwrap();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(
            fake.uris(),
            [
                format!("{B}/projects/proj/datasets?all=false&maxResults=1000"),
                format!("{B}/projects/proj/datasets?all=false&maxResults=1000&pageToken=p%2F2"),
            ]
        );
    }

    #[tokio::test]
    async fn list_stops_at_1000_items() {
        let page = |n: usize, next: bool| {
            let items: Vec<Value> = (0..n)
                .map(|i| json!({ "tableReference": { "tableId": format!("t{i}") } }))
                .collect();
            let mut v = json!({ "tables": items });
            if next {
                v["nextPageToken"] = json!("more");
            }
            v
        };
        let fake = FakeHttp::new(vec![(200, page(600, true)), (200, page(600, true))]);
        let ids = client(&fake).list_tables("p", "d").await.unwrap();
        assert_eq!(ids.len(), 1000);
        assert_eq!(fake.uris().len(), 2);
        assert_eq!(
            fake.uris()[0],
            format!("{B}/projects/p/datasets/d/tables?maxResults=1000")
        );
    }

    #[tokio::test]
    async fn empty_lists_are_empty() {
        let fake = FakeHttp::new(vec![(200, json!({})), (200, json!({ "kind": "x" }))]);
        assert!(client(&fake).list_datasets("p").await.unwrap().is_empty());
        assert!(
            client(&fake)
                .list_tables("p", "d")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn get_dataset_and_table_urls() {
        let fake = FakeHttp::new(vec![
            (200, json!({ "id": "d" })),
            (200, json!({ "id": "t" })),
        ]);
        let c = client(&fake);
        assert_eq!(c.get_dataset("p", "d").await.unwrap(), json!({ "id": "d" }));
        assert_eq!(
            c.get_table("p", "d", "t").await.unwrap(),
            json!({ "id": "t" })
        );
        assert_eq!(
            fake.uris(),
            [
                format!("{B}/projects/p/datasets/d"),
                format!("{B}/projects/p/datasets/d/tables/t")
            ]
        );
    }

    #[tokio::test]
    async fn api_errors_are_mapped() {
        let fake = FakeHttp::new(vec![(
            404,
            json!({ "error": { "message": "Not found: Dataset p:x", "errors": [{ "reason": "notFound" }] } }),
        )]);
        let err = client(&fake).get_dataset("p", "x").await.unwrap_err();
        assert_eq!(
            err,
            BigQueryError::Api {
                status: 404,
                message: "Not found: Dataset p:x".into(),
                reason: Some("notFound".into())
            }
        );
    }

    fn opts() -> QueryOptions {
        let mut labels = BTreeMap::new();
        labels.insert("team".to_string(), "data".to_string());
        QueryOptions::new()
            .with_max_rows(3)
            .with_location("US")
            .with_maximum_bytes_billed(1_000_000)
            .with_labels(labels)
            .with_session_id(Some("S1".into()))
    }

    #[tokio::test]
    async fn dry_run_body_and_destination() {
        let fake = FakeHttp::new(vec![(
            200,
            json!({
                "configuration": { "query": { "destinationTable": { "projectId": "p", "datasetId": "_anon", "tableId": "t" } } },
                "statistics": { "query": { "statementType": "INSERT" } }
            }),
        )]);
        let info = client(&fake)
            .dry_run("cp", "INSERT x", &opts())
            .await
            .unwrap();
        assert_eq!(
            info,
            DryRunInfo::new("INSERT", Some(TableRef::new("p", "_anon", "t")))
        );
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].method, Method::POST);
        assert_eq!(seen[0].uri, format!("{B}/projects/cp/jobs"));
        assert_eq!(
            seen[0].json(),
            json!({
                "jobReference": { "projectId": "cp", "location": "US" },
                "configuration": {
                    "dryRun": true,
                    "labels": { "team": "data" },
                    "query": {
                        "query": "INSERT x",
                        "useLegacySql": false,
                        "maximumBytesBilled": "1000000",
                        "connectionProperties": [{ "key": "session_id", "value": "S1" }]
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn dry_run_falls_back_to_ddl_target_and_none() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "statistics": { "query": { "statementType": "CREATE_TABLE",
                "ddlTargetTable": { "projectId": "p", "datasetId": "_anon", "tableId": "tmp" } } } }),
            ),
            (
                200,
                json!({ "statistics": { "query": { "statementType": "DROP_TABLE" } } }),
            ),
        ]);
        let c = client(&fake);
        let o = QueryOptions::new();
        assert_eq!(
            c.dry_run("p", "CREATE TEMP TABLE tmp AS SELECT 1", &o)
                .await
                .unwrap()
                .destination,
            Some(TableRef::new("p", "_anon", "tmp"))
        );
        assert_eq!(
            c.dry_run("p", "DROP TABLE d.t", &o)
                .await
                .unwrap()
                .destination,
            None
        );
        // No location, no bytes cap, no session: those keys are absent.
        assert_eq!(
            fake.body(1),
            json!({
                "jobReference": { "projectId": "p" },
                "configuration": { "dryRun": true, "labels": {}, "query": { "query": "DROP TABLE d.t", "useLegacySql": false } }
            })
        );
    }

    #[tokio::test]
    async fn dry_run_without_statement_type_is_a_decode_error() {
        let fake = FakeHttp::new(vec![(200, json!({ "statistics": {} }))]);
        let err = client(&fake)
            .dry_run("p", "x", &QueryOptions::new())
            .await
            .unwrap_err();
        assert!(matches!(err, BigQueryError::Decode(_)), "{err:?}");
    }

    fn schema() -> Value {
        json!({ "fields": [ { "name": "n", "type": "INT64" }, { "name": "at", "type": "TIMESTAMP" } ] })
    }

    #[tokio::test]
    async fn query_body_rows_and_truncation() {
        let fake = FakeHttp::new(vec![(
            200,
            json!({
                "jobComplete": true,
                "jobReference": { "projectId": "cp", "jobId": "j1", "location": "US" },
                "schema": schema(),
                "rows": [ { "f": [ { "v": "1" }, { "v": "0" } ] } ],
                "totalRows": "5",
                "pageToken": "next"
            }),
        )]);
        let r = client(&fake)
            .query("cp", "SELECT 1", &opts())
            .await
            .unwrap();
        assert_eq!(
            r.rows,
            vec![
                json!({ "n": 1, "at": "1970-01-01T00:00:00Z" })
                    .as_object()
                    .unwrap()
                    .clone()
            ]
        );
        assert_eq!(r.total_rows, Some(5));
        assert!(r.has_more);
        assert_eq!(r.session, None);
        assert_eq!(fake.uris(), [format!("{B}/projects/cp/queries")]);
        assert_eq!(
            fake.body(0),
            json!({
                "query": "SELECT 1",
                "useLegacySql": false,
                "timeoutMs": 10000,
                "formatOptions": { "useInt64Timestamp": true },
                "labels": { "team": "data" },
                "maxResults": 3,
                "location": "US",
                "maximumBytesBilled": "1000000",
                "connectionProperties": [{ "key": "session_id", "value": "S1" }]
            })
        );
    }

    #[tokio::test]
    async fn query_polls_until_complete() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "jobComplete": false, "jobReference": { "projectId": "cp", "jobId": "j1", "location": "US" } }),
            ),
            (
                200,
                json!({ "jobComplete": true, "schema": schema(), "rows": [ { "f": [ { "v": "2" }, { "v": "1000000" } ] } ], "totalRows": "1" }),
            ),
        ]);
        let r = client(&fake)
            .query("cp", "SELECT 1", &QueryOptions::new().with_max_rows(3))
            .await
            .unwrap();
        assert_eq!(r.rows[0]["at"], json!("1970-01-01T00:00:01Z"));
        assert!(!r.has_more);
        assert_eq!(
            fake.uris()[1],
            format!(
                "{B}/projects/cp/queries/j1?location=US&maxResults=3&timeoutMs=10000&formatOptions.useInt64Timestamp=true"
            )
        );
    }

    #[tokio::test]
    async fn dml_reply_has_no_rows() {
        let fake = FakeHttp::new(vec![(
            200,
            json!({ "jobComplete": true, "numDmlAffectedRows": "2",
            "jobReference": { "projectId": "p", "jobId": "j" } }),
        )]);
        let r = client(&fake)
            .query("p", "INSERT x", &QueryOptions::new())
            .await
            .unwrap();
        assert!(r.rows.is_empty());
        assert!(!r.has_more);
    }

    #[tokio::test]
    async fn create_session_reads_the_anonymous_dataset() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "jobComplete": true, "jobReference": { "projectId": "cp", "jobId": "j1", "location": "US" },
                "sessionInfo": { "sessionId": "SESS" }, "schema": { "fields": [] }, "rows": [], "totalRows": "1" }),
            ),
            (
                200,
                json!({ "configuration": { "query": { "destinationTable": { "projectId": "cp", "datasetId": "_anon1", "tableId": "x" } } } }),
            ),
        ]);
        let o = QueryOptions::new()
            .with_create_session(true)
            .with_location("US");
        let r = client(&fake).query("cp", "SELECT 1", &o).await.unwrap();
        assert_eq!(r.session, Some(SessionInfo::new("cp", "SESS", "_anon1")));
        assert_eq!(fake.body(0)["createSession"], json!(true));
        assert_eq!(
            fake.uris()[1],
            format!(
                "{B}/projects/cp/jobs/j1?location=US&fields=configuration.query.destinationTable"
            )
        );
    }

    #[tokio::test]
    async fn create_session_without_session_info_is_a_decode_error() {
        let fake = FakeHttp::new(vec![(
            200,
            json!({ "jobComplete": true, "jobReference": { "projectId": "cp", "jobId": "j1" } }),
        )]);
        let err = client(&fake)
            .query(
                "cp",
                "SELECT 1",
                &QueryOptions::new().with_create_session(true),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, BigQueryError::Decode(_)), "{err:?}");
    }

    #[tokio::test]
    async fn job_error_results_are_job_errors() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "status": { "errorResult": { "reason": "invalidQuery", "message": "Session S1 has expired" } } }),
            ),
            (
                200,
                json!({ "jobComplete": false, "jobReference": { "projectId": "p", "jobId": "j" } }),
            ),
            (
                200,
                json!({ "jobComplete": true, "status": { "errorResult": { "message": "Resources exceeded" } } }),
            ),
        ]);
        let c = client(&fake);
        let err = c
            .dry_run("p", "SELECT 1", &QueryOptions::new())
            .await
            .unwrap_err();
        assert_eq!(err, BigQueryError::Job("Session S1 has expired".into()));
        assert!(err.is_session_gone());
        let err = c
            .query("p", "SELECT 1", &QueryOptions::new())
            .await
            .unwrap_err();
        assert_eq!(err, BigQueryError::Job("Resources exceeded".into()));
    }

    #[test]
    fn debug_shows_only_the_base() {
        let fake = FakeHttp::new(vec![]);
        assert_eq!(
            format!("{:?}", client(&fake)),
            format!("BigQueryClient {{ base: {B:?}, .. }}")
        );
    }
}
