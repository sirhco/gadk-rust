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

//! Authentication schemes (port of ADK-Go `tool/toolauth/auth_schemes.go`). Task 1 writes the
//! types; Task 6 replaces every `todo!` body and adds the trait impls.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::Extra;

/// Go `OAuthGrantType` (auth_schemes.go:23); `as_str` is the RFC 6749 wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OAuthGrantType {
    ClientCredentials,
    AuthorizationCode,
    Implicit,
    Password,
}

impl OAuthGrantType {
    /// "client_credentials" "authorization_code" "implicit" "password".
    #[allow(unused_variables, clippy::todo)]
    pub fn as_str(self) -> &'static str {
        todo!("Task 6")
    }
}

/// Go `OAuthFlow`. Go field order: authorizationUrl, tokenUrl, refreshUrl, scopes.
#[derive(Clone, Default)]
pub struct OAuthFlow {
    pub authorization_url: String,
    pub token_url: String,
    pub refresh_url: String,
    /// Always written, `{}` when empty.
    pub scopes: BTreeMap<String, String>,
    pub extra: Extra,
}

/// Go `OAuthFlows`.
#[derive(Clone, Default)]
pub struct OAuthFlows {
    pub implicit: Option<OAuthFlow>,
    pub password: Option<OAuthFlow>,
    pub client_credentials: Option<OAuthFlow>,
    pub authorization_code: Option<OAuthFlow>,
    pub extra: Extra,
}

impl OAuthFlows {
    /// Go `GrantType` (auth_schemes.go): client credentials, authorization code, implicit,
    /// password, in that order; `None` is Go's "".
    #[allow(unused_variables, clippy::todo)]
    pub fn grant_type(&self) -> Option<OAuthGrantType> {
        todo!("Task 6")
    }
}

/// Go `SchemeType`, an open string ("gcp_provider" and other custom types round-trip).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemeType(Cow<'static, str>);

impl SchemeType {
    pub const API_KEY: SchemeType = SchemeType(Cow::Borrowed("apiKey"));
    pub const HTTP: SchemeType = SchemeType(Cow::Borrowed("http"));
    pub const OAUTH2: SchemeType = SchemeType(Cow::Borrowed("oauth2"));
    pub const OPEN_ID_CONNECT: SchemeType = SchemeType(Cow::Borrowed("openIdConnect"));

    #[allow(unused_variables, clippy::todo)]
    pub fn new(s: impl Into<Cow<'static, str>>) -> Self {
        todo!("Task 6")
    }

    #[allow(unused_variables, clippy::todo)]
    pub fn as_str(&self) -> &str {
        todo!("Task 6")
    }
}

impl Default for SchemeType {
    // "".
    #[allow(unused_variables, clippy::todo)]
    fn default() -> Self {
        todo!("Task 6")
    }
}

impl From<&'static str> for SchemeType {
    #[allow(unused_variables, clippy::todo)]
    fn from(s: &'static str) -> Self {
        todo!("Task 6")
    }
}

impl From<String> for SchemeType {
    #[allow(unused_variables, clippy::todo)]
    fn from(s: String) -> Self {
        todo!("Task 6")
    }
}

/// Go `Scheme`. Go field order is the declaration order below (wire names in comments).
#[derive(Clone, Default)]
pub struct Scheme {
    pub r#type: SchemeType,                                         // "type"
    pub description: String,                                        // "description"
    pub r#in: String,                                               // "in"
    pub name: String,                                               // "name"
    pub scheme: String,                                             // "scheme"
    pub bearer_format: String,                                      // "bearerFormat"
    pub flows: Option<OAuthFlows>,                                  // "flows"
    pub issuer_url: String,                                         // "issuer_url"
    pub open_id_connect_url: String,                                // "openIdConnectUrl"
    pub authorization_endpoint: String,                             // "authorization_endpoint"
    pub token_endpoint: String,                                     // "token_endpoint"
    pub userinfo_endpoint: String,                                  // "userinfo_endpoint"
    pub revocation_endpoint: String,                                // "revocation_endpoint"
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>, // omitzero: None omitted, Some(vec![]) writes []
    pub grant_types_supported: Option<Vec<String>>,
    pub scopes: Option<Vec<String>>,
    pub extra: Extra,
}
