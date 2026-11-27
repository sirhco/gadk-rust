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

//! The toolset configuration ([`OpenApiConfig`], [`RequireConfirmation`]).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use gadk_agent::tool::{ConfirmationProvider, ToolPredicate};
use gadk_auth::CredentialProvider;

/// The default request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The default largest response body kept in a tool result, in bytes (256 KiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 262_144;

/// Which tool calls need the user's confirmation before the request is sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequireConfirmation {
    /// Calls of `post`, `put`, `patch` and `delete` operations (the default).
    #[default]
    Mutating,
    /// Every call.
    Always,
    /// No call.
    Never,
}

/// How [`OpenApiToolset`](crate::OpenApiToolset) calls the API. Start from
/// [`OpenApiConfig::new`] (the same as [`Default`]) and chain the builder methods.
#[derive(Clone)]
#[non_exhaustive]
pub struct OpenApiConfig {
    pub(crate) base_url: Option<String>,
    pub(crate) auth: Option<Arc<dyn CredentialProvider>>,
    pub(crate) query_api_key: Option<(String, String)>,
    pub(crate) prefix: String,
    pub(crate) tool_filter: Option<ToolPredicate>,
    pub(crate) require_confirmation: RequireConfirmation,
    pub(crate) confirmation_provider: Option<ConfirmationProvider>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) timeout: Duration,
    pub(crate) max_response_bytes: usize,
}

impl Default for OpenApiConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            auth: None,
            query_api_key: None,
            prefix: String::new(),
            tool_filter: None,
            require_confirmation: RequireConfirmation::default(),
            confirmation_provider: None,
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

impl OpenApiConfig {
    /// The default configuration: the document's server URL, no credentials, no prefix or
    /// filter, [`RequireConfirmation::Mutating`], a 30 s timeout and a 256 KiB response cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// The base URL of every request, instead of the document's `servers[0].url`. It must be
    /// absolute, without a query or fragment, or [`OpenApiToolset::new`] fails with
    /// [`Error::NoBaseUrl`].
    ///
    /// [`OpenApiToolset::new`]: crate::OpenApiToolset::new
    /// [`Error::NoBaseUrl`]: crate::Error::NoBaseUrl
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Sends every request through [`gadk_auth::client`] with `provider`, which writes the
    /// credential's headers. When the provider can refresh its credential (`adc` and
    /// `service_account`), a 401 or 403 is retried once with a refreshed one; `static_token`,
    /// `api_key` and `token_source` are not retried.
    pub fn auth(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.auth = Some(provider);
        self
    }

    /// Appends `name=value` to every request's query string.
    pub fn query_api_key(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query_api_key = Some((name.into(), value.into()));
        self
    }

    /// Exposes each tool as `<prefix>_<name>`. Empty (the default) keeps the names.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Keeps only the tools `filter` accepts. The filter sees each tool under its unprefixed
    /// name.
    pub fn tool_filter(mut self, filter: ToolPredicate) -> Self {
        self.tool_filter = Some(filter);
        self
    }

    /// Which calls need confirmation (default [`RequireConfirmation::Mutating`]).
    pub fn require_confirmation(mut self, mode: RequireConfirmation) -> Self {
        self.require_confirmation = mode;
        self
    }

    /// Decides per call whether confirmation is needed, instead of
    /// [`require_confirmation`](OpenApiConfig::require_confirmation). It is called with the
    /// exposed tool name and the call's arguments.
    pub fn confirmation_provider(mut self, provider: ConfirmationProvider) -> Self {
        self.confirmation_provider = Some(provider);
        self
    }

    /// Adds a header to every request.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The limit on one whole call, from sending the request to reading the body (default
    /// 30 s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The largest response body kept in a result (default 262,144 bytes); a longer body is
    /// cut and the result gains `"truncated": true`.
    pub fn max_response_bytes(mut self, max: usize) -> Self {
        self.max_response_bytes = max;
        self
    }
}

impl fmt::Debug for OpenApiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenApiConfig")
            .field("base_url", &self.base_url)
            .field("auth", &self.auth.as_ref().map(|_| "<provider>"))
            .field(
                "query_api_key",
                &self.query_api_key.as_ref().map(|(name, _)| name),
            )
            .field("prefix", &self.prefix)
            .field(
                "tool_filter",
                &self.tool_filter.as_ref().map(|_| "<filter>"),
            )
            .field("require_confirmation", &self.require_confirmation)
            .field(
                "confirmation_provider",
                &self.confirmation_provider.as_ref().map(|_| "<provider>"),
            )
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}
