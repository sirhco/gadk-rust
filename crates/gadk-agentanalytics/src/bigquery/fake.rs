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

//! Test fakes (feature `testutil`): ports of the ADK-Go test helpers the in-scope tests use —
//! `mockTableOps` (`schema_upgrade_test.go`), `fakeViewRunner` (`views_test.go`),
//! `recordingLogger`, `lookup` and `checkJSON` (`arrow_test_helper_test.go`) — and a
//! [`FakeOffloader`]. Go's `mockTransport` (an `http.RoundTripper`) is
//! `gadk_model::rest::test_util::ScriptedClient` / `FakeServer`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gadk_platform::BoxFuture;
use serde_json::Value;

use crate::config::{GcsContentOffloader, GcsUploadRequest, Logger};
use crate::context::{Context, ContextError};
use crate::error::{ApiError, Code, Error};
use crate::schema::events_schema;
use crate::schema_upgrade::{TableMetadata, TableMetadataOps, TableMetadataUpdate};
use crate::views::ViewRunner;

/// A scripted error a fake returns (a cloneable description of an [`Error`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeError {
    /// A `*googleapi.Error` with this HTTP code and message.
    Api {
        /// The HTTP status.
        code: u16,
        /// The message.
        message: String,
    },
    /// A gRPC status error.
    Status {
        /// The status code.
        code: Code,
        /// The status message.
        message: String,
    },
    /// A context error.
    Context(ContextError),
    /// A plain `errors.New` text.
    Message(String),
}

impl FakeError {
    /// The [`Error`] this describes.
    pub fn to_error(&self) -> Error {
        match self {
            FakeError::Api { code, message } => Error::from(ApiError {
                code: *code,
                message: message.clone(),
                ..ApiError::default()
            }),
            FakeError::Status { code, message } => Error::Status {
                code: *code,
                message: message.clone(),
            },
            FakeError::Context(c) => Error::Context(*c),
            FakeError::Message(text) => Error::msg(text.clone()),
        }
    }
}

/// The state of a [`MockTableOps`] (Go `mockTableOps`'s fields): scripted responses (the last
/// one repeats; with none, `metadata` returns `{schema: events_schema()}` and `update`
/// `{schema: update.schema}`) and the recorded calls.
#[derive(Debug, Clone, Default)]
pub struct MockTableOpsState {
    /// Go `metadataSeq`.
    pub metadata_seq: Vec<Result<TableMetadata, FakeError>>,
    /// Go `metadataCalls`.
    pub metadata_calls: usize,
    /// Go `createErr`.
    pub create_err: Option<FakeError>,
    /// Go `createCalls`.
    pub create_calls: usize,
    /// Go `createdMeta`.
    pub created_meta: Option<TableMetadata>,
    /// Go `updateSeq`.
    pub update_seq: Vec<Result<TableMetadata, FakeError>>,
    /// Go `updateCalls`.
    pub update_calls: usize,
    /// Go `updates`: each update and its etag.
    pub updates: Vec<(TableMetadataUpdate, String)>,
}

/// An in-memory [`TableMetadataOps`] (Go `mockTableOps`).
#[derive(Debug, Default)]
pub struct MockTableOps {
    state: Mutex<MockTableOpsState>,
}

impl MockTableOps {
    /// A mock with `state`'s scripted responses.
    pub fn new(state: MockTableOpsState) -> MockTableOps {
        MockTableOps {
            state: Mutex::new(state),
        }
    }

    /// A snapshot of the state (scripts and recorded calls).
    pub fn state(&self) -> MockTableOpsState {
        lock(&self.state).clone()
    }
}

#[async_trait::async_trait]
impl TableMetadataOps for MockTableOps {
    async fn metadata(&self, _ctx: &Context) -> Result<TableMetadata, Error> {
        let mut st = lock(&self.state);
        let idx = st.metadata_calls;
        st.metadata_calls += 1;
        match scripted(&st.metadata_seq, idx) {
            Some(resp) => resp,
            None => Ok(TableMetadata {
                schema: events_schema(),
                ..TableMetadata::default()
            }),
        }
    }

    async fn create(&self, _ctx: &Context, meta: &TableMetadata) -> Result<(), Error> {
        let mut st = lock(&self.state);
        st.create_calls += 1;
        st.created_meta = Some(meta.clone());
        match &st.create_err {
            Some(err) => Err(err.to_error()),
            None => Ok(()),
        }
    }

    async fn update(
        &self,
        _ctx: &Context,
        update: &TableMetadataUpdate,
        etag: &str,
    ) -> Result<TableMetadata, Error> {
        let mut st = lock(&self.state);
        let idx = st.update_calls;
        st.update_calls += 1;
        st.updates.push((update.clone(), etag.to_owned()));
        match scripted(&st.update_seq, idx) {
            Some(resp) => resp,
            None => Ok(TableMetadata {
                schema: update.schema.clone().unwrap_or_default(),
                ..TableMetadata::default()
            }),
        }
    }
}

/// Go `fakeViewRunner.onRun`: called with the view's context and name after the call is
/// recorded; an error is returned instead of the scripted one.
pub type OnRun =
    Arc<dyn Fn(Context, String) -> BoxFuture<'static, Result<(), Error>> + Send + Sync>;

/// A scripted [`ViewRunner`] (Go `fakeViewRunner`).
#[derive(Default)]
pub struct FakeViewRunner {
    state: Mutex<FakeViewRunnerState>,
}

#[derive(Default)]
struct FakeViewRunnerState {
    names: Vec<String>,
    queries: Vec<String>,
    err_for: HashMap<String, FakeError>,
    on_run: Option<OnRun>,
}

impl FakeViewRunner {
    /// A runner failing the views in `err_for` (Go `errFor`) and calling `on_run` (Go `onRun`).
    pub fn new(err_for: HashMap<String, FakeError>, on_run: Option<OnRun>) -> FakeViewRunner {
        FakeViewRunner {
            state: Mutex::new(FakeViewRunnerState {
                err_for,
                on_run,
                ..FakeViewRunnerState::default()
            }),
        }
    }

    /// Go `executedNames()`.
    pub fn executed_names(&self) -> Vec<String> {
        lock(&self.state).names.clone()
    }

    /// The queries run, in order.
    pub fn queries(&self) -> Vec<String> {
        lock(&self.state).queries.clone()
    }
}

#[async_trait::async_trait]
impl ViewRunner for FakeViewRunner {
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error> {
        let (err, on_run) = {
            let mut st = lock(&self.state);
            st.names.push(view_name.to_owned());
            st.queries.push(query.to_owned());
            (st.err_for.get(view_name).cloned(), st.on_run.clone())
        };
        if let Some(on_run) = on_run {
            on_run(ctx.clone(), view_name.to_owned()).await?;
        }
        match err {
            Some(err) => Err(err.to_error()),
            None => Ok(()),
        }
    }
}

/// A [`Logger`] keeping what is logged (Go `recordingLogger`). `Display` is Go's `String()`:
/// the lines joined by `\n`.
#[derive(Debug, Default)]
pub struct RecordingLogger {
    lines: Mutex<Vec<String>>,
}

impl RecordingLogger {
    /// Go `contains(substr)`: whether any line contains `substr`.
    pub fn contains(&self, substr: &str) -> bool {
        lock(&self.lines).iter().any(|line| line.contains(substr))
    }

    /// The logged lines.
    pub fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }
}

impl Logger for RecordingLogger {
    fn printf(&self, args: fmt::Arguments<'_>) {
        lock(&self.lines).push(args.to_string());
    }

    /// Go `fmt.Sprintln`: the text and a trailing newline.
    fn println(&self, args: fmt::Arguments<'_>) {
        lock(&self.lines).push(format!("{args}\n"));
    }
}

impl fmt::Display for RecordingLogger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&lock(&self.lines).join("\n"))
    }
}

/// The function a [`FakeOffloader`] answers uploads with.
pub type OffloadFn = Arc<dyn Fn(&GcsUploadRequest) -> Result<String, FakeError> + Send + Sync>;

/// A scripted [`GcsContentOffloader`] that records requests and whether it was closed.
pub struct FakeOffloader {
    respond: OffloadFn,
    requests: Mutex<Vec<GcsUploadRequest>>,
    closed: Mutex<bool>,
}

impl FakeOffloader {
    /// An offloader answering with `respond`.
    pub fn new(respond: OffloadFn) -> FakeOffloader {
        FakeOffloader {
            respond,
            requests: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
        }
    }

    /// An offloader returning `gs://{bucket}/{path}` for every request.
    pub fn with_bucket(bucket: &str) -> FakeOffloader {
        let bucket = bucket.to_owned();
        FakeOffloader::new(Arc::new(move |req: &GcsUploadRequest| {
            Ok(format!("gs://{bucket}/{}", req.path))
        }))
    }

    /// The requests received, in order.
    pub fn requests(&self) -> Vec<GcsUploadRequest> {
        lock(&self.requests).clone()
    }

    /// Whether `close` was called.
    pub fn closed(&self) -> bool {
        *lock(&self.closed)
    }
}

#[async_trait::async_trait]
impl GcsContentOffloader for FakeOffloader {
    async fn upload_content(&self, _ctx: &Context, req: GcsUploadRequest) -> Result<String, Error> {
        let result = (self.respond)(&req);
        lock(&self.requests).push(req);
        result.map_err(|e| e.to_error())
    }

    async fn close(&self) -> Result<(), Error> {
        *lock(&self.closed) = true;
        Ok(())
    }
}

/// Go `lookup(v, path...)`: the value at `path` through nested objects.
pub fn lookup<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for key in path {
        cur = cur.as_object()?.get(*key)?;
    }
    Some(cur)
}

/// Go `checkJSON(t, name, got, want)`: `Err` with a `-want +got` description when `got` and the
/// JSON document `want` differ (numbers compared by value).
pub fn check_json(name: &str, got: &Value, want: &str) -> Result<(), String> {
    let want: Value =
        serde_json::from_str(want).map_err(|e| format!("{name}: invalid want JSON {want}: {e}"))?;
    let (want, got) = (numbers_as_f64(&want), numbers_as_f64(got));
    if want == got {
        return Ok(());
    }
    Err(format!(
        "{name} mismatch (-want +got):\n-{}\n+{}",
        serde_json::to_string(&want).unwrap_or_default(),
        serde_json::to_string(&got).unwrap_or_default()
    ))
}

/// Go `json.Unmarshal` into `any`: every number becomes a `float64`.
fn numbers_as_f64(v: &Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| v.clone(), Value::Number),
        Value::Array(items) => Value::Array(items.iter().map(numbers_as_f64).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), numbers_as_f64(v)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// Locks `m`, ignoring poisoning (a panicking test thread leaves the data usable).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Go `mockTableOps`'s scripted response for call `idx`: that entry, else the last entry
/// repeated, else `None` (the caller's default).
fn scripted(
    seq: &[Result<TableMetadata, FakeError>],
    idx: usize,
) -> Option<Result<TableMetadata, Error>> {
    let resp = seq.get(idx).or_else(|| seq.last())?;
    Some(resp.clone().map_err(|e| e.to_error()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::error::{is_conflict_error, is_not_found_error, status_code};

    fn md(etag: &str) -> TableMetadata {
        TableMetadata {
            etag: etag.into(),
            ..TableMetadata::default()
        }
    }

    #[test]
    fn fake_errors_become_crate_errors() {
        let api = FakeError::Api {
            code: 404,
            message: "not found".into(),
        }
        .to_error();
        assert_eq!(api.to_string(), "googleapi: Error 404: not found");
        assert!(is_not_found_error(&api));
        let st = FakeError::Status {
            code: Code::AlreadyExists,
            message: "exists".into(),
        }
        .to_error();
        assert!(is_conflict_error(&st));
        assert_eq!(status_code(&st), Code::AlreadyExists);
        assert!(
            FakeError::Context(ContextError::Canceled)
                .to_error()
                .is_context(ContextError::Canceled)
        );
        assert_eq!(
            FakeError::Message("boom".into()).to_error().to_string(),
            "boom"
        );
    }

    #[tokio::test]
    async fn mock_table_ops_scripts_and_records_like_go() {
        let ops = MockTableOps::new(MockTableOpsState {
            metadata_seq: vec![
                Err(FakeError::Api {
                    code: 404,
                    message: "nf".into(),
                }),
                Ok(md("e1")),
            ],
            create_err: Some(FakeError::Api {
                code: 409,
                message: "dup".into(),
            }),
            update_seq: vec![Ok(md("u1"))],
            ..MockTableOpsState::default()
        });
        let ctx = Context::background();
        assert!(is_not_found_error(&ops.metadata(&ctx).await.unwrap_err()));
        assert_eq!(ops.metadata(&ctx).await.unwrap().etag, "e1");
        // The last scripted response repeats.
        assert_eq!(ops.metadata(&ctx).await.unwrap().etag, "e1");
        let created = md("c");
        assert!(is_conflict_error(
            &ops.create(&ctx, &created).await.unwrap_err()
        ));
        let mut update = TableMetadataUpdate::default();
        update.set_label("k", "v");
        assert_eq!(ops.update(&ctx, &update, "e1").await.unwrap().etag, "u1");
        assert_eq!(ops.update(&ctx, &update, "").await.unwrap().etag, "u1");
        let st = ops.state();
        assert_eq!(
            (st.metadata_calls, st.create_calls, st.update_calls),
            (3, 1, 2)
        );
        assert_eq!(st.created_meta, Some(created));
        assert_eq!(
            st.updates,
            vec![(update.clone(), "e1".to_owned()), (update, String::new())]
        );
    }

    #[tokio::test]
    async fn mock_update_without_script_echoes_the_schema() {
        let ops = MockTableOps::default();
        let schema = vec![crate::schema::FieldSchema {
            name: "x".into(),
            field_type: "STRING".into(),
            ..crate::schema::FieldSchema::default()
        }];
        let update = TableMetadataUpdate {
            schema: Some(schema.clone()),
            ..TableMetadataUpdate::default()
        };
        let got = ops
            .update(&Context::background(), &update, "e")
            .await
            .unwrap();
        assert_eq!(got.schema, schema);
        assert!(ops.create(&Context::background(), &md("")).await.is_ok());
    }

    #[tokio::test]
    async fn fake_view_runner_records_then_fails_as_scripted() {
        let on_run: OnRun = Arc::new(|_ctx: Context, name: String| {
            Box::pin(async move {
                if name == "v_hook" {
                    Err(Error::msg("hook failed"))
                } else {
                    Ok(())
                }
            })
        });
        let runner = FakeViewRunner::new(
            HashMap::from([("v_bad".to_owned(), FakeError::Message("ddl failed".into()))]),
            Some(on_run),
        );
        let ctx = Context::background();
        assert!(runner.run_view_ddl(&ctx, "v_ok", "q1").await.is_ok());
        assert_eq!(
            runner
                .run_view_ddl(&ctx, "v_bad", "q2")
                .await
                .unwrap_err()
                .to_string(),
            "ddl failed"
        );
        assert_eq!(
            runner
                .run_view_ddl(&ctx, "v_hook", "q3")
                .await
                .unwrap_err()
                .to_string(),
            "hook failed"
        );
        assert_eq!(runner.executed_names(), ["v_ok", "v_bad", "v_hook"]);
        assert_eq!(runner.queries(), ["q1", "q2", "q3"]);
    }

    #[test]
    fn recording_logger_keeps_lines() {
        let logger = RecordingLogger::default();
        logger.printf(format_args!("Table {} not found. Creating it...", "t"));
        logger.println(format_args!("a {}", 1));
        assert!(logger.contains("not found"));
        assert!(!logger.contains("absent"));
        assert_eq!(
            logger.lines(),
            ["Table t not found. Creating it...", "a 1\n"]
        );
        assert_eq!(
            logger.to_string(),
            "Table t not found. Creating it...\na 1\n"
        );
        assert!(!logger.is_std_default());
    }

    #[tokio::test]
    async fn fake_offloader_answers_and_records() {
        let off = FakeOffloader::with_bucket("bkt");
        let req = GcsUploadRequest {
            path: "p/o.txt".into(),
            data: bytes::Bytes::from_static(b"hi"),
            content_type: "text/plain".into(),
        };
        let ctx = Context::background();
        assert_eq!(
            off.upload_content(&ctx, req.clone()).await.unwrap(),
            "gs://bkt/p/o.txt"
        );
        assert!(!off.closed());
        off.close().await.unwrap();
        assert!(off.closed());
        assert_eq!(off.requests(), vec![req.clone()]);

        let failing = FakeOffloader::new(Arc::new(|_: &GcsUploadRequest| {
            Err(FakeError::Message("upload failed".into()))
        }));
        assert_eq!(
            failing
                .upload_content(&ctx, req)
                .await
                .unwrap_err()
                .to_string(),
            "upload failed"
        );
        assert_eq!(failing.requests().len(), 1);
    }

    #[test]
    fn lookup_walks_objects_only() {
        let v = json!({"a": {"b": [1, {"c": 2}]}, "n": null});
        assert_eq!(lookup(&v, &["a", "b"]), Some(&json!([1, {"c": 2}])));
        assert_eq!(lookup(&v, &[]), Some(&v));
        assert_eq!(lookup(&v, &["n"]), Some(&Value::Null));
        assert_eq!(lookup(&v, &["a", "b", "0"]), None);
        assert_eq!(lookup(&v, &["missing"]), None);
    }

    #[test]
    fn check_json_compares_numbers_by_value() {
        assert_eq!(
            check_json("x", &json!({"a": 1, "b": [2.0]}), r#"{"b":[2],"a":1.0}"#),
            Ok(())
        );
        let err = check_json("x", &json!({"a": 1}), r#"{"a":2}"#).unwrap_err();
        assert_eq!(err, "x mismatch (-want +got):\n-{\"a\":2.0}\n+{\"a\":1.0}");
        assert!(
            check_json("x", &json!(1), "{")
                .unwrap_err()
                .starts_with("x: invalid want JSON {: ")
        );
    }
}
