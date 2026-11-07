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

//! Transport resolution (port of ADK-Go `tool/mcptoolset/set.go` `buildTransport`): validates a
//! [`Config`] and turns its [`McpTransport`] into a [`TransportFactory`].

use std::sync::Arc;

use gadk_auth::{AuthContext, CredentialProvider};

use crate::config::{Config, McpTransport, TransportFactory};
use crate::error::Error;

/// Go `buildTransport`: validates `cfg` (R1) and returns the connector factory of its transport.
/// An HTTP transport's factory builds an [`AdkHttpClient`](crate::http::AdkHttpClient) with
/// `auth: None`; with `auth`, the credential session pool instead calls [`http_factory_for`] per
/// session, so the returned factory is unused.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_transport(cfg: &Config) -> Result<Arc<dyn TransportFactory>, Error> {
    todo!("Task 7")
}

/// The HTTP factory of one pooled session: every request carries `auth`'s credential resolved
/// for `cx`, the identity that created the session (Go `identityOnlyContext`).
#[allow(unused_variables, dead_code, clippy::todo)]
pub(crate) fn http_factory_for(
    target: &HttpTarget,
    auth: Arc<dyn CredentialProvider>,
    cx: AuthContext,
) -> Arc<dyn TransportFactory> {
    todo!("Task 7")
}

/// The URL, base client and extra headers of an `Endpoint` or `StreamableHttp` transport.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct HttpTarget {
    /// The MCP endpoint URL.
    pub(crate) url: String,
    /// The base HTTP client (`reqwest::Client::new()` for `Endpoint`).
    pub(crate) client: reqwest::Client,
    /// Extra headers for every request (empty for `Endpoint`).
    pub(crate) headers: http::HeaderMap,
}

impl HttpTarget {
    /// `Some` for `Endpoint` (non-empty URL) and `StreamableHttp`, `None` otherwise.
    #[allow(unused_variables, clippy::todo)]
    pub(crate) fn from_transport(t: &McpTransport) -> Option<HttpTarget> {
        todo!("Task 7")
    }
}
