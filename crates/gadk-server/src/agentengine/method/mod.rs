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
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

//! Go `server/agentengine/controllers/method`: `MethodHandler` and the six methods.

pub(crate) mod create_session;
pub(crate) mod delete_session;
pub(crate) mod get_session;
pub(crate) mod list_sessions;
pub(crate) mod stream_query;
pub(crate) mod streaming_agent_run_with_events;

use bytes::Bytes;
use serde_json::{Map, Value};

use crate::mux::Response;

/// Go `MethodHandler`: one `class_method` of the Agent Engine API.
#[async_trait::async_trait]
pub(crate) trait MethodHandler: Send + Sync {
    /// Go `Name`: the `class_method` it serves.
    fn name(&self) -> &str;

    /// Go `Handle(ctx, rw, payload)`: `payload` is the whole (size-limited) request body.
    /// `Ok` is the response to send (a streaming body for the stream methods); `Err(text)` is a
    /// Go handler error, answered 500 `text` and logged `handleQuery failed: text` by the
    /// controller (only before anything is written, as in Go).
    async fn handle(&self, payload: Bytes) -> Result<Response, String>;

    /// Go `Metadata`: the map Go passes to `structpb.NewStruct` (`api_mode`, `name`,
    /// `parameters`, `description`).
    fn metadata(&self) -> Map<String, Value>;
}

/// Go `Metadata()`'s map: `api_mode`, `name`, `parameters` (a JSON schema) and `description`.
pub(crate) fn method_metadata(
    method_name: &str,
    api_mode: &str,
    parameters: Value,
    description: &str,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("api_mode".to_owned(), Value::String(api_mode.to_owned()));
    m.insert("name".to_owned(), Value::String(method_name.to_owned()));
    m.insert("parameters".to_owned(), parameters);
    m.insert(
        "description".to_owned(),
        Value::String(description.to_owned()),
    );
    m
}

/// A plain Go `json.NewEncoder(rw).Encode(v)` response of a session method: 200, the JSON
/// (`gadk_model::to_go_json`) plus `\n`, `Content-Type: text/plain; charset=utf-8` (Go's
/// sniffed type; no other headers). An encoding failure is Go's
/// `json.NewEncoder failed: <err>`.
pub(crate) fn encode_plain<T: serde::Serialize>(v: &T) -> Result<Response, String> {
    todo!("Task 7")
}
