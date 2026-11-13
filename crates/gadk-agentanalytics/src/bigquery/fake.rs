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

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use gadk_platform::BoxFuture;
use serde_json::Value;

use crate::config::{GcsContentOffloader, GcsUploadRequest, Logger};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error};
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
        todo!("Task 5")
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
        todo!("Task 5")
    }

    /// A snapshot of the state (scripts and recorded calls).
    pub fn state(&self) -> MockTableOpsState {
        todo!("Task 5")
    }
}

#[async_trait::async_trait]
impl TableMetadataOps for MockTableOps {
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
        todo!("Task 5")
    }

    /// Go `executedNames()`.
    pub fn executed_names(&self) -> Vec<String> {
        todo!("Task 5")
    }

    /// The queries run, in order.
    pub fn queries(&self) -> Vec<String> {
        todo!("Task 5")
    }
}

#[async_trait::async_trait]
impl ViewRunner for FakeViewRunner {
    async fn run_view_ddl(&self, ctx: &Context, view_name: &str, query: &str) -> Result<(), Error> {
        todo!("Task 5")
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
        todo!("Task 5")
    }

    /// The logged lines.
    pub fn lines(&self) -> Vec<String> {
        todo!("Task 5")
    }
}

impl Logger for RecordingLogger {
    fn printf(&self, args: fmt::Arguments<'_>) {
        todo!("Task 5")
    }

    fn println(&self, args: fmt::Arguments<'_>) {
        todo!("Task 5")
    }
}

impl fmt::Display for RecordingLogger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("Task 5")
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
        todo!("Task 5")
    }

    /// An offloader returning `gs://{bucket}/{path}` for every request.
    pub fn with_bucket(bucket: &str) -> FakeOffloader {
        todo!("Task 5")
    }

    /// The requests received, in order.
    pub fn requests(&self) -> Vec<GcsUploadRequest> {
        todo!("Task 5")
    }

    /// Whether `close` was called.
    pub fn closed(&self) -> bool {
        todo!("Task 5")
    }
}

#[async_trait::async_trait]
impl GcsContentOffloader for FakeOffloader {
    async fn upload_content(&self, ctx: &Context, req: GcsUploadRequest) -> Result<String, Error> {
        todo!("Task 5")
    }

    async fn close(&self) -> Result<(), Error> {
        todo!("Task 5")
    }
}

/// Go `lookup(v, path...)`: the value at `path` through nested objects.
pub fn lookup<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    todo!("Task 5")
}

/// Go `checkJSON(t, name, got, want)`: `Err` with a `-want +got` description when `got` and the
/// JSON document `want` differ (numbers compared by value).
pub fn check_json(name: &str, got: &Value, want: &str) -> Result<(), String> {
    todo!("Task 5")
}
