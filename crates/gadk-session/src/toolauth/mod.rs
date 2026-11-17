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

//! adk-python's auth data model in its wire format: how a
//! resource expects to be authenticated ([`Scheme`]), a credential a tool was configured with or
//! obtained ([`Credential`]), and the two together with the key the credential is stored under
//! ([`Config`]). When a tool needs the end user's consent, ADK sends a [`Config`] to the client
//! in an `adk_request_credential` call, so these types write and read the same JSON as
//! adk-python and ADK Web.
//!
//! The types are data that travels in events and to the client. They do not write themselves
//! onto requests, which is what a `gadk_auth::Credential` does. [`Config::key`] is the string
//! adk-python names a credential by, not a `gadk_auth::CredentialKey`.

mod credential;
pub(crate) mod model_json;
mod redact;
mod schemes;
mod tool;

pub use credential::{
    Credential, CredentialType, HttpAuth, HttpCredentials, OAuth2Auth, ServiceAccount,
    ServiceAccountCredential,
};
pub use schemes::{OAuthFlow, OAuthFlows, OAuthGrantType, Scheme, SchemeType};
pub use tool::{Config, REQUEST_CREDENTIAL_FUNCTION_CALL_NAME, ToolArguments};

/// Unknown JSON keys, each kept as the raw text it arrived as (numbers keep their literal
/// text). Needs serde_json's `raw_value` feature.
pub type Extra = std::collections::BTreeMap<String, Box<serde_json::value::RawValue>>;
