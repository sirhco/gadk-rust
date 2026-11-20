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

//! The BigQuery toolset and its five tools.

use std::any::Any;
use std::sync::Arc;

use gadk_agent::{Context, ReadonlyContext, Tool, ToolError, Toolset};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::client::BigQueryClient;
use crate::config::{BigQueryToolConfig, WriteMode};
use crate::error::BigQueryError;
use crate::ident::{valid_dataset, valid_project, valid_table};
use crate::types::{QueryOptions, QueryResult};

/// The toolset's name.
pub const TOOLSET_NAME: &str = "bigquery_toolset";

/// The label key the toolset adds to every job it creates.
pub(crate) const LABEL_KEY: &str = "gadk-bigquery-tool";

/// The refusal for a non-`SELECT` statement in [`WriteMode::Blocked`].
pub(crate) const READ_ONLY_MSG: &str = "Read-only mode only supports SELECT statements.";

/// The fields of a `datasets.get` resource `get_dataset_info` returns.
const DATASET_FIELDS: &[&str] = &[
    "datasetReference",
    "friendlyName",
    "description",
    "location",
    "labels",
    "creationTime",
    "lastModifiedTime",
    "defaultTableExpirationMs",
];

/// The fields of a `tables.get` resource `get_table_info` returns (plus `view.query`).
const TABLE_FIELDS: &[&str] = &[
    "tableReference",
    "type",
    "friendlyName",
    "description",
    "schema",
    "numRows",
    "numBytes",
    "creationTime",
    "lastModifiedTime",
    "timePartitioning",
    "clustering",
    "labels",
];

/// Arguments naming a project.
#[derive(Deserialize, JsonSchema)]
struct ProjectArgs {
    /// The Google Cloud project id.
    project_id: String,
}

/// Arguments naming a dataset.
#[derive(Deserialize, JsonSchema)]
struct DatasetArgs {
    /// The Google Cloud project id.
    project_id: String,
    /// The BigQuery dataset id.
    dataset_id: String,
}

/// Arguments naming a table.
#[derive(Deserialize, JsonSchema)]
struct TableArgs {
    /// The Google Cloud project id.
    project_id: String,
    /// The BigQuery dataset id.
    dataset_id: String,
    /// The BigQuery table or view id.
    table_id: String,
}

/// Arguments of `execute_sql`.
#[derive(Deserialize, JsonSchema)]
struct SqlArgs {
    /// The Google Cloud project id the query runs in (unless the toolset sets a compute project).
    project_id: String,
    /// The GoogleSQL statement to run; qualify tables as `dataset.table` or `project.dataset.table`.
    query: String,
}

/// A BigQuery toolset: `list_dataset_ids`, `get_dataset_info`, `list_table_ids`,
/// `get_table_info` and `execute_sql`.
#[non_exhaustive]
pub struct BigQueryToolset {
    config: Arc<BigQueryToolConfig>,
    tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for BigQueryToolset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigQueryToolset")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BigQueryToolset {
    /// The toolset with a default [`BigQueryClient`] (Application Default Credentials).
    ///
    /// # Errors
    ///
    /// A [`ToolError`] when a tool cannot be built (an internal schema problem).
    pub fn new(config: BigQueryToolConfig) -> Result<Self, ToolError> {
        Self::with_client(config, Arc::new(BigQueryClient::new()))
    }

    /// The toolset using `client` for every call.
    ///
    /// # Errors
    ///
    /// A [`ToolError`] when a tool cannot be built (an internal schema problem).
    pub fn with_client(
        config: BigQueryToolConfig,
        client: Arc<BigQueryClient>,
    ) -> Result<Self, ToolError> {
        let config = Arc::new(config);
        let mut tools = Vec::with_capacity(5);

        let c = client.clone();
        tools.push(FunctionTool::new(
            FunctionToolConfig::new(
                "list_dataset_ids",
                "Lists the ids of the BigQuery datasets in a Google Cloud project.",
            ),
            move |_ctx: Context, args: ProjectArgs| {
                let c = c.clone();
                async move { list_dataset_ids(&c, args).await }
            },
        )?);

        let c = client.clone();
        tools.push(FunctionTool::new(
            FunctionToolConfig::new(
                "get_dataset_info",
                "Returns metadata about a BigQuery dataset: location, description, labels and times.",
            ),
            move |_ctx: Context, args: DatasetArgs| {
                let c = c.clone();
                async move { get_dataset_info(&c, args).await }
            },
        )?);

        let c = client.clone();
        tools.push(FunctionTool::new(
            FunctionToolConfig::new(
                "list_table_ids",
                "Lists the ids of the tables and views in a BigQuery dataset.",
            ),
            move |_ctx: Context, args: DatasetArgs| {
                let c = c.clone();
                async move { list_table_ids(&c, args).await }
            },
        )?);

        let c = client.clone();
        tools.push(FunctionTool::new(
            FunctionToolConfig::new(
                "get_table_info",
                "Returns metadata about a BigQuery table or view, including its schema (column names, types and modes), row count, partitioning and, for a view, its query.",
            ),
            move |_ctx: Context, args: TableArgs| {
                let c = c.clone();
                async move { get_table_info(&c, args).await }
            },
        )?);

        let c = client;
        let cfg = config.clone();
        tools.push(FunctionTool::new(
            FunctionToolConfig::new("execute_sql", execute_sql_description(&config)),
            move |ctx: Context, args: SqlArgs| {
                let c = c.clone();
                let cfg = cfg.clone();
                async move { execute_sql(ctx, &c, &cfg, args).await }
            },
        )?);

        Ok(Self { config, tools })
    }
}

#[async_trait::async_trait]
impl Toolset for BigQueryToolset {
    fn name(&self) -> &str {
        TOOLSET_NAME
    }

    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.tools.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The `execute_sql` description: what it returns, the row cap and the write mode.
fn execute_sql_description(config: &BigQueryToolConfig) -> String {
    let mode = match config.write_mode {
        WriteMode::Blocked => "Only SELECT statements are allowed.",
        WriteMode::Protected => {
            "SELECT statements are allowed, and so are writes to temporary tables (CREATE TEMP TABLE …), which last for this conversation."
        }
        WriteMode::Allowed => "Any statement is allowed, including writes.",
    };
    format!(
        "Runs a GoogleSQL statement in BigQuery and returns the result rows as JSON objects (at most {} rows; `result_is_likely_truncated` is true when there are more). {mode} Look up table schemas with get_table_info first, and qualify table names with their dataset.",
        config.max_query_result_rows
    )
}

/// `{"status":"ERROR","error_details": message}`.
pub(crate) fn error_result(message: impl Into<String>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), json!("ERROR"));
    m.insert("error_details".into(), Value::String(message.into()));
    m
}

/// `{"status":"SUCCESS", key: value}`.
fn success(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), json!("SUCCESS"));
    m.insert(key.into(), value);
    m
}

/// A credential failure fails the call; any other BigQuery error goes back to the model.
pub(crate) fn map_err(e: BigQueryError) -> Result<Map<String, Value>, ToolError> {
    match e {
        BigQueryError::Credentials(_) => Err(ToolError::Execution(Box::new(e))),
        other => Ok(error_result(other.to_string())),
    }
}

/// The ERROR result for an invalid identifier, or `None` when every one is valid.
fn check_ids(
    project: &str,
    dataset: Option<&str>,
    table: Option<&str>,
) -> Option<Map<String, Value>> {
    if !valid_project(project) {
        return Some(error_result(format!(
            "invalid project_id {project:?} (expected [A-Za-z0-9.:_-]+)"
        )));
    }
    if let Some(d) = dataset.filter(|d| !valid_dataset(d)) {
        return Some(error_result(format!(
            "invalid dataset_id {d:?} (expected [A-Za-z0-9_]+)"
        )));
    }
    if let Some(t) = table.filter(|t| !valid_table(t)) {
        return Some(error_result(format!(
            "invalid table_id {t:?} (expected [A-Za-z0-9_-]+)"
        )));
    }
    None
}

/// The listed `fields` of `resource`, skipping missing ones.
fn pick(resource: &Value, fields: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    for f in fields {
        if let Some(v) = resource.get(*f) {
            out.insert((*f).to_owned(), v.clone());
        }
    }
    out
}

async fn list_dataset_ids(
    c: &BigQueryClient,
    args: ProjectArgs,
) -> Result<Map<String, Value>, ToolError> {
    if let Some(bad) = check_ids(&args.project_id, None, None) {
        return Ok(bad);
    }
    match c.list_datasets(&args.project_id).await {
        Ok(ids) => Ok(success("dataset_ids", json!(ids))),
        Err(e) => map_err(e),
    }
}

async fn get_dataset_info(
    c: &BigQueryClient,
    args: DatasetArgs,
) -> Result<Map<String, Value>, ToolError> {
    if let Some(bad) = check_ids(&args.project_id, Some(&args.dataset_id), None) {
        return Ok(bad);
    }
    match c.get_dataset(&args.project_id, &args.dataset_id).await {
        Ok(v) => Ok(success("info", Value::Object(pick(&v, DATASET_FIELDS)))),
        Err(e) => map_err(e),
    }
}

async fn list_table_ids(
    c: &BigQueryClient,
    args: DatasetArgs,
) -> Result<Map<String, Value>, ToolError> {
    if let Some(bad) = check_ids(&args.project_id, Some(&args.dataset_id), None) {
        return Ok(bad);
    }
    match c.list_tables(&args.project_id, &args.dataset_id).await {
        Ok(ids) => Ok(success("table_ids", json!(ids))),
        Err(e) => map_err(e),
    }
}

async fn get_table_info(
    c: &BigQueryClient,
    args: TableArgs,
) -> Result<Map<String, Value>, ToolError> {
    if let Some(bad) = check_ids(
        &args.project_id,
        Some(&args.dataset_id),
        Some(&args.table_id),
    ) {
        return Ok(bad);
    }
    match c
        .get_table(&args.project_id, &args.dataset_id, &args.table_id)
        .await
    {
        Ok(v) => {
            let mut info = pick(&v, TABLE_FIELDS);
            if let Some(q) = v.pointer("/view/query") {
                info.insert("view".into(), json!({ "query": q }));
            }
            Ok(success("info", Value::Object(info)))
        }
        Err(e) => map_err(e),
    }
}

/// The query options every `execute_sql` job carries: row cap, location, cost cap and labels.
pub(crate) fn query_options(config: &BigQueryToolConfig) -> QueryOptions {
    let mut labels = config.job_labels.clone();
    labels.insert(LABEL_KEY.into(), "execute_sql".into());
    let mut o = QueryOptions::new()
        .with_max_rows(u32::try_from(config.max_query_result_rows).unwrap_or(u32::MAX))
        .with_labels(labels);
    if let Some(loc) = &config.location {
        o = o.with_location(loc.clone());
    }
    if let Some(bytes) = config.maximum_bytes_billed {
        o = o.with_maximum_bytes_billed(bytes);
    }
    o
}

/// `{"status":"SUCCESS","rows":[…]}`, plus `result_is_likely_truncated: true` when more rows exist.
pub(crate) fn rows_result(r: QueryResult) -> Map<String, Value> {
    let mut m = success(
        "rows",
        Value::Array(r.rows.into_iter().map(Value::Object).collect()),
    );
    if r.has_more {
        m.insert("result_is_likely_truncated".into(), json!(true));
    }
    m
}

async fn execute_sql(
    ctx: Context,
    c: &BigQueryClient,
    config: &BigQueryToolConfig,
    args: SqlArgs,
) -> Result<Map<String, Value>, ToolError> {
    if let Some(bad) = check_ids(&args.project_id, None, None) {
        return Ok(bad);
    }
    let compute = config
        .compute_project
        .clone()
        .unwrap_or_else(|| args.project_id.clone());
    let opts = query_options(config);
    let outcome = match config.write_mode {
        WriteMode::Allowed => c.query(&compute, &args.query, &opts).await.map(rows_result),
        WriteMode::Blocked => match c.dry_run(&compute, &args.query, &opts).await {
            Ok(info) if info.statement_type == "SELECT" => {
                c.query(&compute, &args.query, &opts).await.map(rows_result)
            }
            Ok(_) => Ok(error_result(READ_ONLY_MSG)),
            Err(e) => Err(e),
        },
        WriteMode::Protected => Ok(error_result("Protected write mode is not available yet.")),
    };
    let _ = &ctx;
    match outcome {
        Ok(m) => Ok(m),
        Err(e) => map_err(e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::client::tests::{FakeHttp, client};

    fn toolset(fake: &Arc<FakeHttp>, config: BigQueryToolConfig) -> BigQueryToolset {
        BigQueryToolset::with_client(config, Arc::new(client(fake))).unwrap()
    }

    async fn call(
        ts: &BigQueryToolset,
        ctx: &Context,
        name: &str,
        args: Value,
    ) -> Result<Map<String, Value>, ToolError> {
        let tool = ts.tools.iter().find(|t| t.name() == name).unwrap().clone();
        tool.as_runnable().unwrap().run(ctx.clone(), args).await
    }

    fn ctx() -> Context {
        Context::builder().build()
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn five_tools_in_order_with_required_args() {
        let ts = toolset(&FakeHttp::new(vec![]), BigQueryToolConfig::new());
        assert_eq!(ts.name(), "bigquery_toolset");
        let names: Vec<&str> = ts.tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            [
                "list_dataset_ids",
                "get_dataset_info",
                "list_table_ids",
                "get_table_info",
                "execute_sql"
            ]
        );
        let required = |i: usize| {
            let decl = ts.tools[i].declaration().unwrap();
            let mut r: Vec<String> = decl.parameters_json_schema.unwrap()["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            r.sort();
            r
        };
        assert_eq!(required(0), ["project_id"]);
        assert_eq!(required(1), ["dataset_id", "project_id"]);
        assert_eq!(required(2), ["dataset_id", "project_id"]);
        assert_eq!(required(3), ["dataset_id", "project_id", "table_id"]);
        assert_eq!(required(4), ["project_id", "query"]);
        for t in &ts.tools {
            assert!(!t.description().is_empty(), "{}", t.name());
        }
    }

    #[tokio::test]
    async fn list_tools_results() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "datasets": [{ "datasetReference": { "datasetId": "gadk_examples" } }] }),
            ),
            (
                200,
                json!({ "tables": [{ "tableReference": { "tableId": "orders" } }] }),
            ),
        ]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        assert_eq!(
            call(
                &ts,
                &ctx(),
                "list_dataset_ids",
                json!({ "project_id": "p" })
            )
            .await
            .unwrap(),
            obj(json!({ "status": "SUCCESS", "dataset_ids": ["gadk_examples"] }))
        );
        assert_eq!(
            call(
                &ts,
                &ctx(),
                "list_table_ids",
                json!({ "project_id": "p", "dataset_id": "gadk_examples" })
            )
            .await
            .unwrap(),
            obj(json!({ "status": "SUCCESS", "table_ids": ["orders"] }))
        );
    }

    #[tokio::test]
    async fn info_tools_keep_the_listed_fields() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "kind": "bigquery#dataset", "etag": "x", "datasetReference": { "projectId": "p", "datasetId": "d" },
                "location": "US", "description": "demo", "creationTime": "1" }),
            ),
            (
                200,
                json!({ "kind": "bigquery#table", "etag": "x", "tableReference": { "projectId": "p", "datasetId": "d", "tableId": "v" },
                "type": "VIEW", "schema": { "fields": [{ "name": "m", "type": "DATE" }] }, "numRows": "0",
                "view": { "query": "SELECT 1", "useLegacySql": false } }),
            ),
        ]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        assert_eq!(
            call(
                &ts,
                &ctx(),
                "get_dataset_info",
                json!({ "project_id": "p", "dataset_id": "d" })
            )
            .await
            .unwrap(),
            obj(
                json!({ "status": "SUCCESS", "info": { "datasetReference": { "projectId": "p", "datasetId": "d" },
                "location": "US", "description": "demo", "creationTime": "1" } })
            )
        );
        assert_eq!(
            call(
                &ts,
                &ctx(),
                "get_table_info",
                json!({ "project_id": "p", "dataset_id": "d", "table_id": "v" })
            )
            .await
            .unwrap(),
            obj(
                json!({ "status": "SUCCESS", "info": { "tableReference": { "projectId": "p", "datasetId": "d", "tableId": "v" },
                "type": "VIEW", "schema": { "fields": [{ "name": "m", "type": "DATE" }] }, "numRows": "0",
                "view": { "query": "SELECT 1" } } })
            )
        );
    }

    #[tokio::test]
    async fn invalid_identifiers_send_nothing() {
        let fake = FakeHttp::new(vec![]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        let r = call(
            &ts,
            &ctx(),
            "get_dataset_info",
            json!({ "project_id": "p", "dataset_id": "a.b" }),
        )
        .await
        .unwrap();
        assert_eq!(
            r,
            obj(
                json!({ "status": "ERROR", "error_details": "invalid dataset_id \"a.b\" (expected [A-Za-z0-9_]+)" })
            )
        );
        let r = call(
            &ts,
            &ctx(),
            "get_table_info",
            json!({ "project_id": "p", "dataset_id": "d", "table_id": "t$1" }),
        )
        .await
        .unwrap();
        assert_eq!(
            r["error_details"],
            json!("invalid table_id \"t$1\" (expected [A-Za-z0-9_-]+)")
        );
        let r = call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "a/b", "query": "SELECT 1" }),
        )
        .await
        .unwrap();
        assert_eq!(
            r["error_details"],
            json!("invalid project_id \"a/b\" (expected [A-Za-z0-9.:_-]+)")
        );
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn missing_arguments_are_tool_errors() {
        let ts = toolset(&FakeHttp::new(vec![]), BigQueryToolConfig::new());
        assert!(
            call(&ts, &ctx(), "execute_sql", json!({ "project_id": "p" }))
                .await
                .is_err()
        );
    }

    fn select_dry_run(kind: &str) -> (u16, Value) {
        (
            200,
            json!({ "statistics": { "query": { "statementType": kind } } }),
        )
    }

    fn rows_reply(n: usize, total: usize) -> (u16, Value) {
        let rows: Vec<Value> = (0..n)
            .map(|i| json!({ "f": [{ "v": i.to_string() }] }))
            .collect();
        (
            200,
            json!({ "jobComplete": true, "jobReference": { "projectId": "p", "jobId": "j" },
            "schema": { "fields": [{ "name": "n", "type": "INT64" }] }, "rows": rows, "totalRows": total.to_string() }),
        )
    }

    #[tokio::test]
    async fn blocked_runs_select_and_flags_truncation() {
        let fake = FakeHttp::new(vec![select_dry_run("SELECT"), rows_reply(2, 5)]);
        let ts = toolset(
            &fake,
            BigQueryToolConfig::new().with_max_query_result_rows(2),
        );
        let r = call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "p", "query": "SELECT n FROM d.t" }),
        )
        .await
        .unwrap();
        assert_eq!(
            r,
            obj(
                json!({ "status": "SUCCESS", "rows": [{ "n": 0 }, { "n": 1 }], "result_is_likely_truncated": true })
            )
        );
        assert_eq!(fake.body(1)["maxResults"], json!(2));
    }

    #[tokio::test]
    async fn complete_results_have_no_truncation_flag() {
        let fake = FakeHttp::new(vec![select_dry_run("SELECT"), rows_reply(1, 1)]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        let r = call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "p", "query": "SELECT 0" }),
        )
        .await
        .unwrap();
        assert_eq!(r, obj(json!({ "status": "SUCCESS", "rows": [{ "n": 0 }] })));
    }

    #[tokio::test]
    async fn blocked_refuses_writes_without_running_them() {
        let fake = FakeHttp::new(vec![select_dry_run("INSERT")]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        let r = call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "p", "query": "INSERT d.t VALUES (1)" }),
        )
        .await
        .unwrap();
        assert_eq!(
            r,
            obj(
                json!({ "status": "ERROR", "error_details": "Read-only mode only supports SELECT statements." })
            )
        );
        assert_eq!(fake.uris().len(), 1);
        assert!(fake.uris()[0].ends_with("/projects/p/jobs"));
    }

    #[tokio::test]
    async fn allowed_sends_no_dry_run_and_dml_has_no_rows() {
        let fake = FakeHttp::new(vec![(
            200,
            json!({ "jobComplete": true, "numDmlAffectedRows": "1",
            "jobReference": { "projectId": "p", "jobId": "j" } }),
        )]);
        let ts = toolset(
            &fake,
            BigQueryToolConfig::new().with_write_mode(WriteMode::Allowed),
        );
        let r = call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "p", "query": "INSERT d.t VALUES (1)" }),
        )
        .await
        .unwrap();
        assert_eq!(r, obj(json!({ "status": "SUCCESS", "rows": [] })));
        assert_eq!(fake.uris().len(), 1);
        assert!(fake.uris()[0].ends_with("/projects/p/queries"));
    }

    #[tokio::test]
    async fn compute_project_overrides_the_argument_and_labels_merge() {
        let fake = FakeHttp::new(vec![select_dry_run("SELECT"), rows_reply(0, 0)]);
        let labels = BTreeMap::from([
            ("team".to_string(), "data".to_string()),
            ("gadk-bigquery-tool".to_string(), "mine".to_string()),
        ]);
        let ts = toolset(
            &fake,
            BigQueryToolConfig::new()
                .with_compute_project("cp")
                .with_location("US")
                .with_maximum_bytes_billed(99)
                .with_job_labels(labels),
        );
        call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "data-proj", "query": "SELECT 1" }),
        )
        .await
        .unwrap();
        let want_labels = json!({ "team": "data", "gadk-bigquery-tool": "execute_sql" });
        assert!(fake.uris()[0].ends_with("/projects/cp/jobs"));
        assert!(fake.uris()[1].ends_with("/projects/cp/queries"));
        let dry = fake.body(0);
        assert_eq!(dry["configuration"]["labels"], want_labels);
        assert_eq!(
            dry["configuration"]["query"]["maximumBytesBilled"],
            json!("99")
        );
        assert_eq!(dry["configuration"]["query"]["useLegacySql"], json!(false));
        assert_eq!(dry["jobReference"]["location"], json!("US"));
        let q = fake.body(1);
        assert_eq!(q["labels"], want_labels);
        assert_eq!(q["maximumBytesBilled"], json!("99"));
        assert_eq!(q["location"], json!("US"));
        assert_eq!(q["useLegacySql"], json!(false));
    }

    #[tokio::test]
    async fn compute_project_defaults_to_the_argument() {
        let fake = FakeHttp::new(vec![select_dry_run("SELECT"), rows_reply(0, 0)]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "data-proj", "query": "SELECT 1" }),
        )
        .await
        .unwrap();
        assert!(fake.uris()[0].ends_with("/projects/data-proj/jobs"));
    }

    #[tokio::test]
    async fn bigquery_errors_become_error_results() {
        let fake = FakeHttp::new(vec![(
            400,
            json!({ "error": { "message": "Syntax error at [1:1]", "errors": [{ "reason": "invalidQuery" }] } }),
        )]);
        let ts = toolset(&fake, BigQueryToolConfig::new());
        let r = call(
            &ts,
            &ctx(),
            "execute_sql",
            json!({ "project_id": "p", "query": "SELEC" }),
        )
        .await
        .unwrap();
        assert_eq!(
            r,
            obj(
                json!({ "status": "ERROR", "error_details": "BigQuery: HTTP 400: Syntax error at [1:1]" })
            )
        );
    }

    struct FailingAuth;

    #[async_trait::async_trait]
    impl crate::AuthHeaders for FailingAuth {
        async fn headers(&self) -> Result<http::HeaderMap, String> {
            Err("no credentials found".into())
        }
    }

    #[tokio::test]
    async fn credential_failures_are_tool_errors() {
        let fake = FakeHttp::new(vec![]);
        let c = BigQueryClient::with_parts(fake.clone(), Arc::new(FailingAuth), "http://bq.test");
        let ts = BigQueryToolset::with_client(BigQueryToolConfig::new(), Arc::new(c)).unwrap();
        let err = call(
            &ts,
            &ctx(),
            "list_dataset_ids",
            json!({ "project_id": "p" }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no credentials found"), "{err}");
    }
}
