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

//! Go `agentregistry/factory.go`, shared part: the egress options and Go's `egressClient` /
//! `clientWithHeaders` choice. Go builds distinct `*http.Client` values (a header round-tripper
//! over a shallow copy); gadk records the choice as an [`Egress`] (which client, which headers)
//! and applies the headers where the client library takes them: an a2a-client
//! `CallInterceptor` for A2A, `McpTransport::StreamableHttp.headers` for MCP (ledgered).

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

/// Go `egressConfig`: the resolved options shared by the factory helpers.
#[derive(Debug, Clone, Default)]
pub(crate) struct EgressConfig {
    /// Go `httpClient`: the option's client (`None` = Go nil).
    pub(crate) http_client: Option<reqwest::Client>,
    /// Go `headers`: static headers added to (or overwriting) every request; keys as given.
    pub(crate) headers: BTreeMap<String, String>,
}

/// Go `addHeaders`: merges `h` into `e.headers` (adds and overwrites, never removes).
pub(crate) fn add_headers(e: &mut EgressConfig, h: &BTreeMap<String, String>) {
    e.headers
        .extend(h.iter().map(|(k, v)| (k.clone(), v.clone())));
}

/// Which HTTP client carries the egress (Go `egressClient`'s `base`).
#[derive(Debug, Clone)]
pub(crate) enum EgressClient {
    /// The option's client (Go `ec.httpClient`).
    Option(reqwest::Client),
    /// The registry's own authentication (Go `c.httpClient`): its caller-supplied client, or on
    /// the ADC path a default client plus the registry's ADC credential provider.
    // Chosen only by `Client::egress`, which only the MCP toolset calls.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    Registry,
    /// A default client (Go `http.DefaultClient`: no timeout).
    Default,
}

/// Go's egress `*http.Client` as a choice: the client and the headers layered on it (Go
/// `clientWithHeaders`; empty = the base unchanged).
#[derive(Debug, Clone)]
pub(crate) struct Egress {
    /// The base client.
    pub(crate) client: EgressClient,
    /// Go `headerRoundTripper.headers`, in key order; each is `Set` on every request.
    pub(crate) headers: Vec<(String, String)>,
}

impl Client {
    /// Go `egressClient` (MCP): the option's client, else [`EgressClient::Registry`] for a
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

/// Go `isGoogleAPI`: `raw_url` parses (Go `url.Parse`, via `gadk_platform::go_url`) and its
/// hostname is `googleapis.com` or ends with `.googleapis.com`.
#[cfg_attr(not(feature = "mcp"), allow(dead_code))]
pub(crate) fn is_google_api(raw_url: &str) -> bool {
    let Ok(u) = gadk_platform::go_url::parse(raw_url) else {
        return false;
    };
    let host = u.hostname();
    host == "googleapis.com" || host.ends_with(".googleapis.com")
}

/// Go `clientWithHeaders`: `base` with `headers` layered on (none: `base` unchanged).
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

/// A default HTTP client (Go `http.DefaultClient`: no overall timeout). Fails only when reqwest
/// cannot initialize TLS ([`crate::Error::HttpClient`]).
pub(crate) fn default_http_client() -> Result<reqwest::Client, crate::Error> {
    reqwest::Client::builder()
        .build()
        .map_err(|e| crate::Error::HttpClient(e.to_string()))
}
