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

//! [`BigQueryClient`]: the subset of `cloud.google.com/go/bigquery` (v1.79.0) that ADK-Go's
//! `plugin/agentanalytics` uses, over gadk's REST transport (`gadk_model::rest`): table
//! metadata get / insert / patch (with `If-Match`), query jobs (`jobs.insert`, then Go's
//! `Job.Wait` polling), and the `googleapi.CheckResponse` error mapping ([`ApiError`]).
//! [`TableOps`] is Go's `bqTableAdapter` (one table's [`TableMetadataOps`]).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::fmt;
use std::sync::Arc;

use gadk_model::rest::RestTransport;
use gadk_platform::env::EnvProvider;
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;

use crate::config::ClientOptions;
use crate::context::Context;
use crate::error::{ApiError, BigQueryError, Error};
use crate::schema_upgrade::{TableMetadata, TableMetadataOps, TableMetadataUpdate};

/// A BigQuery job's identity (Go `bigquery.Job`'s project, ID and location).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobReference {
    /// The job's project.
    pub project_id: String,
    /// The job ID.
    pub job_id: String,
    /// The job's location (empty when unknown).
    pub location: String,
}

/// A job's status (Go `bigquery.JobStatus` subset).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobStatus {
    /// `PENDING`, `RUNNING` or `DONE`.
    pub state: String,
    /// The job's error (Go `JobStatus.Err()`), from `status.errorResult`.
    pub error_result: Option<BigQueryError>,
    /// All errors the job met (`status.errors`).
    pub errors: Vec<BigQueryError>,
}

impl JobStatus {
    /// Go `JobStatus.Done()`.
    pub fn done(&self) -> bool {
        self.state == "DONE"
    }

    /// Go `JobStatus.Err()`: the job's error result, `None` when it succeeded.
    pub fn err(&self) -> Option<Error> {
        self.error_result.clone().map(Error::BigQuery)
    }
}

/// A submitted job (Go `*bigquery.Job`): its reference and the status `jobs.insert` returned
/// (Go `Job.LastStatus()`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Job {
    /// The job's identity.
    pub reference: JobReference,
    /// Go `LastStatus()`.
    pub last_status: Option<JobStatus>,
}

/// A BigQuery REST client for one default project (Go `*bigquery.Client`).
pub struct BigQueryClient {
    /// The transport (HTTP client, retry policy).
    pub(crate) transport: RestTransport,
    /// `scheme://host[:port]/bigquery/v2` without a trailing `/`.
    pub(crate) base_url: String,
    /// The client's project (Go `Client.Project()`), for jobs and `Dataset(...)`.
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
    /// Go `bigquery.NewClient(ctx, projectID, opts...)`, reading `BIGQUERY_EMULATOR_HOST` and
    /// `GOOGLE_APPLICATION_CREDENTIALS` from the process environment.
    pub fn new(
        project_id: impl Into<String>,
        options: &ClientOptions,
    ) -> Result<BigQueryClient, Error> {
        todo!("Task 5")
    }

    /// [`new`](Self::new) with the environment read from `env`.
    pub fn new_with_env(
        project_id: impl Into<String>,
        options: &ClientOptions,
        env: &dyn EnvProvider,
    ) -> Result<BigQueryClient, Error> {
        todo!("Task 5")
    }

    /// Go `Client.Project()`.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Go `Table.Metadata(ctx)`: `GET .../projects/{p}/datasets/{d}/tables/{t}`.
    pub async fn get_table(
        &self,
        ctx: &Context,
        project_id: &str,
        dataset_id: &str,
        table_id: &str,
    ) -> Result<TableMetadata, Error> {
        todo!("Task 5")
    }

    /// Go `Table.Create(ctx, meta)`: `POST .../projects/{p}/datasets/{d}/tables`.
    pub async fn insert_table(
        &self,
        ctx: &Context,
        project_id: &str,
        dataset_id: &str,
        table_id: &str,
        meta: &TableMetadata,
    ) -> Result<TableMetadata, Error> {
        todo!("Task 5")
    }

    /// Go `Table.Update(ctx, update, etag)`: `PATCH .../tables/{t}` with `If-Match: etag` when
    /// `etag` is non-empty.
    pub async fn patch_table(
        &self,
        ctx: &Context,
        project_id: &str,
        dataset_id: &str,
        table_id: &str,
        update: &TableMetadataUpdate,
        etag: &str,
    ) -> Result<TableMetadata, Error> {
        todo!("Task 5")
    }

    /// Go `client.Query(query).Run(ctx)`: `POST .../projects/{client project}/jobs` (standard
    /// SQL query job) and the returned job with its last status.
    pub async fn insert_query_job(&self, ctx: &Context, query: &str) -> Result<Job, Error> {
        todo!("Task 5")
    }

    /// Go `Job.Wait(ctx)` for a query job: polls `jobs.getQueryResults` until the job is
    /// complete, then reads its status (`jobs.get`), with Go's request sequence and backoff.
    pub async fn wait_job(&self, ctx: &Context, job: &JobReference) -> Result<JobStatus, Error> {
        todo!("Task 5")
    }
}

/// Go `googleapi.CheckResponse` for a response with status ≥ 300: the [`ApiError`] Go builds
/// from the JSON error body (`error.code` / `message` / `errors` / `details`), the raw body and
/// the headers.
pub(crate) fn api_error_from_response(parts: &http::response::Parts, body: &[u8]) -> ApiError {
    todo!("Task 5")
}

/// One table's metadata RPCs (Go `bqTableAdapter` over `client.DatasetInProject(p, d).Table(t)`).
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
        todo!("Task 5")
    }

    async fn create(&self, ctx: &Context, meta: &TableMetadata) -> Result<(), Error> {
        todo!("Task 5")
    }

    async fn update(
        &self,
        ctx: &Context,
        update: &TableMetadataUpdate,
        etag: &str,
    ) -> Result<TableMetadata, Error> {
        todo!("Task 5")
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

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
