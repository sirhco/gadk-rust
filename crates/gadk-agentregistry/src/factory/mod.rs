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

//! Shared egress options of the factory helpers. The choice of HTTP client and static headers is
//! recorded as an [`Egress`] (which client, which headers), and the headers are applied where the
//! client library takes them: an a2a-client `CallInterceptor` for A2A,
//! `McpTransport::StreamableHttp.headers` for MCP.

#[cfg(feature = "a2a")]
pub(crate) mod a2a;
#[cfg(feature = "mcp")]
pub(crate) mod mcp;

use std::collections::BTreeMap;

#[cfg(feature = "a2a")]
pub use a2a::{RemoteAgentOption, with_a2a_headers, with_a2a_http_client};
#[cfg(feature = "mcp")]
pub use mcp::{McpToolsetOption, with_mcp_headers, with_mcp_http_client};

use crate::config::Client;

/// The resolved options shared by the factory helpers.
#[derive(Debug, Clone, Default)]
pub(crate) struct EgressConfig {
    /// The option's client, if set.
    pub(crate) http_client: Option<reqwest::Client>,
    /// Static headers added to (or overwriting) every request; keys as given.
    pub(crate) headers: BTreeMap<String, String>,
}

/// Merges `h` into `e.headers` (adds and overwrites, never removes).
pub(crate) fn add_headers(e: &mut EgressConfig, h: &BTreeMap<String, String>) {
    e.headers
        .extend(h.iter().map(|(k, v)| (k.clone(), v.clone())));
}

/// Which HTTP client carries the egress.
#[derive(Debug, Clone)]
pub(crate) enum EgressClient {
    /// The option's client.
    Option(reqwest::Client),
    /// The registry's own authentication: its caller-supplied client, or on
    /// the ADC path a default client plus the registry's ADC credential provider.
    // Chosen only by `Client::egress`, which only the MCP toolset calls.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    Registry,
    /// A default client (no timeout).
    Default,
}

/// The egress as a choice: the base client and the headers layered on it (no headers: the base
/// unchanged).
#[derive(Debug, Clone)]
pub(crate) struct Egress {
    /// The base client.
    pub(crate) client: EgressClient,
    /// Headers in key order; each is set (replacing any existing value) on every request.
    pub(crate) headers: Vec<(String, String)>,
}

impl Client {
    /// The egress for an MCP server: the option's client, else [`EgressClient::Registry`] for a
    /// [`is_google_api`] URL, else [`EgressClient::Default`]; then [`client_with_headers`].
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub(crate) fn egress(&self, raw_url: &str, ec: &EgressConfig) -> Egress {
        let base = match &ec.http_client {
            Some(c) => EgressClient::Option(c.clone()),
            None if is_google_api(raw_url) => EgressClient::Registry,
            None => EgressClient::Default,
        };
        client_with_headers(base, &ec.headers)
    }
}

/// Whether `raw_url` parses as a URL and its hostname is `googleapis.com` or ends with
/// `.googleapis.com`.
#[cfg_attr(not(feature = "mcp"), allow(dead_code))]
pub(crate) fn is_google_api(raw_url: &str) -> bool {
    let Ok(u) = gadk_platform::go_url::parse(raw_url) else {
        return false;
    };
    let host = u.hostname();
    host == "googleapis.com" || host.ends_with(".googleapis.com")
}

/// `base` with `headers` layered on (none: `base` unchanged).
pub(crate) fn client_with_headers(
    base: EgressClient,
    headers: &BTreeMap<String, String>,
) -> Egress {
    Egress {
        client: base,
        headers: headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

/// A default HTTP client (no overall timeout). Fails only when reqwest
/// cannot initialize TLS ([`crate::Error::HttpClient`]).
pub(crate) fn default_http_client() -> Result<reqwest::Client, crate::Error> {
    reqwest::Client::builder()
        .build()
        .map_err(|e| crate::Error::HttpClient(e.to_string()))
}
