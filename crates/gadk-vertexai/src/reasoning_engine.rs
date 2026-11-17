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

//! A Vertex AI Reasoning Engine client (the `CreateReasoningEngine`, `UpdateReasoningEngine`
//! and operation-wait calls that the ADK deploy CLI makes for an Agent Engine).
//!
//! Requests follow the `v1beta1` REST API: `$alt=json;enum-encoding=int`, the
//! `x-goog-request-params` routing header, protojson (camelCase) bodies given as
//! [`serde_json::Value`], and operations polled at `/ui/{operation name}` with a backoff
//! (initial 1 s, ×2, cap 60 s). Requests are authenticated with
//! Application Default Credentials unless credentials are given.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! use gadk_vertexai::reasoning_engine::{ReasoningEngineClient, ReasoningEngineOptions};
//! let client = ReasoningEngineClient::new("us-central1", ReasoningEngineOptions::default())?;
//! let op = client
//!     .create(
//!         "projects/my-project/locations/us-central1",
//!         serde_json::json!({"displayName": "my-agent"}),
//!     )
//!     .await?;
//! let engine = client.wait(op).await?;
//! println!("{}", engine["name"]);
//! # Ok(()) }
//! ```

use std::sync::Arc;

use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;
use http::Method;
use serde_json::Value;

use crate::client::VertexClient;
use crate::config::resolve;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

/// How a [`ReasoningEngineClient`] reaches Vertex AI.
#[derive(Default, Clone)]
pub struct ReasoningEngineOptions {
    /// The base URL; `None` (or empty) =
    /// `https://{location}-aiplatform.googleapis.com`.
    pub endpoint: Option<String>,
    /// The credentials sent with every request; `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the
    /// `cloud-platform` scope.
    pub credentials: Option<Credentials>,
    /// Send no credentials.
    pub without_authentication: bool,
    /// The HTTP client; `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Where `GOOGLE_APPLICATION_CREDENTIALS` is read; `None` = the process environment.
    pub env: Option<Arc<dyn EnvProvider>>,
}

impl std::fmt::Debug for ReasoningEngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReasoningEngineOptions")
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .field("env", &self.env.is_some())
            .finish()
    }
}

/// A Vertex AI Reasoning Engine client (REST transport).
pub struct ReasoningEngineClient {
    client: VertexClient,
}

impl ReasoningEngineClient {
    /// A client for the engines of `location`, at `https://{location}-aiplatform.googleapis.com`.
    /// Synchronous; only Application Default Credentials need a Tokio runtime.
    ///
    /// # Errors
    ///
    /// The Application Default Credentials error, unwrapped (callers add the
    /// `cannot create ReasoningEngineClient: ` prefix).
    pub fn new(
        location: &str,
        opts: ReasoningEngineOptions,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // REST v1beta1 to `{location}-aiplatform.googleapis.com`, with no `global` special
        // case.
        let default_endpoint = format!("https://{location}-aiplatform.googleapis.com");
        let env = opts.env.unwrap_or_else(|| Arc::new(ProcessEnv));
        let resolved = resolve(
            opts.endpoint.as_deref(),
            &default_endpoint,
            opts.credentials,
            opts.without_authentication,
            opts.http_client,
            env.as_ref(),
        )?;
        Ok(Self {
            client: VertexClient::new(resolved),
        })
    }

    /// `CreateReasoningEngine`: `POST /v1beta1/{parent}/reasoningEngines` with body
    /// `engine` (a protojson `ReasoningEngine`) and routing header `parent={parent}`. Returns
    /// the long-running operation; pass it to [`wait`](Self::wait).
    ///
    /// # Errors
    ///
    /// A [`VertexError`](crate::VertexError) for a non-2xx status (displayed as
    /// `rpc error: code = … desc = …`), or the transport or JSON error.
    pub async fn create(
        &self,
        parent: &str,
        engine: Value,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        self.client
            .call(
                Method::POST,
                &format!("/v1beta1/{parent}/reasoningEngines"),
                &[],
                ("parent", parent),
                Some(&engine),
            )
            .await
    }

    /// `UpdateReasoningEngine`: `PATCH /v1beta1/{engine.name}` with query
    /// `updateMask={update_mask joined by ","}` (the protojson field-mask form, e.g.
    /// `["spec.sourceCodeSpec", "spec.classMethods"]`), body `engine` and routing header
    /// `reasoning_engine.name={engine.name}`. Returns the long-running operation; pass it to
    /// [`wait`](Self::wait).
    ///
    /// # Errors
    ///
    /// As [`create`](Self::create).
    pub async fn update(
        &self,
        engine: Value,
        update_mask: &[&str],
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let name = engine
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.client
            .call(
                Method::PATCH,
                &format!("/v1beta1/{name}"),
                &[("updateMask", update_mask.join(","))],
                ("reasoning_engine.name", &name),
                Some(&engine),
            )
            .await
    }

    /// Waits for a create or update operation: polls `op` until done (backoff initial 1 s, ×2,
    /// cap 60 s; unbounded, so wrap it in `tokio::time::timeout` for a deadline) and returns
    /// the operation's `response` (the `ReasoningEngine`) without its `@type`.
    ///
    /// # Errors
    ///
    /// The operation's error as a [`VertexError`](crate::VertexError)
    /// (`rpc error: code = … desc = …`), a failed poll's error, or
    /// `unsupported result type <nil>: <nil>` when the operation is done without a response.
    /// Callers add the `operation failed: ` prefix.
    pub async fn wait(&self, op: Value) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let mut done = self.client.wait_operation(op).await?;
        match done.get_mut("response").map(Value::take) {
            Some(Value::Object(mut response)) => {
                response.remove("@type");
                Ok(Value::Object(response))
            }
            _ => Err("unsupported result type <nil>: <nil>".into()),
        }
    }
}
