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

//! Credentials (port of ADK-Go `tool/toolauth/auth_credential.go`). Task 1 writes the types;
//! Task 6 replaces every `todo!` body and adds the trait impls.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::Extra;

/// Go `CredentialType`, open like `SchemeType`; decode rejects anything but the five.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialType(Cow<'static, str>);

impl CredentialType {
    pub const API_KEY: CredentialType = CredentialType(Cow::Borrowed("apiKey"));
    pub const HTTP: CredentialType = CredentialType(Cow::Borrowed("http"));
    pub const OAUTH2: CredentialType = CredentialType(Cow::Borrowed("oauth2"));
    pub const OPEN_ID_CONNECT: CredentialType = CredentialType(Cow::Borrowed("openIdConnect"));
    pub const SERVICE_ACCOUNT: CredentialType = CredentialType(Cow::Borrowed("serviceAccount"));

    #[allow(unused_variables, clippy::todo)]
    pub fn new(s: impl Into<Cow<'static, str>>) -> Self {
        todo!("Task 6")
    }

    #[allow(unused_variables, clippy::todo)]
    pub fn as_str(&self) -> &str {
        todo!("Task 6")
    }
}

impl Default for CredentialType {
    #[allow(unused_variables, clippy::todo)]
    fn default() -> Self {
        todo!("Task 6")
    }
}

impl From<&'static str> for CredentialType {
    #[allow(unused_variables, clippy::todo)]
    fn from(s: &'static str) -> Self {
        todo!("Task 6")
    }
}

impl From<String> for CredentialType {
    #[allow(unused_variables, clippy::todo)]
    fn from(s: String) -> Self {
        todo!("Task 6")
    }
}

#[derive(Clone, Default)]
pub struct HttpCredentials {
    pub username: String,
    pub password: String,
    pub token: String,
    pub extra: Extra,
}

#[derive(Clone, Default)]
pub struct HttpAuth {
    /// Required on decode.
    pub scheme: String,
    /// Required on decode.
    pub credentials: HttpCredentials,
    /// "additionalHeaders", omitzero.
    pub additional_headers: Option<BTreeMap<String, String>>,
    pub extra: Extra,
}

#[derive(Clone, Default)]
pub struct OAuth2Auth {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub nonce: String,
    pub state: String,
    pub redirect_uri: String,
    pub auth_response_uri: String,
    pub auth_code: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: i64,
    pub expires_in: i64,
    pub audience: String,
    pub prompt: String,
    pub code_verifier: String,
    pub code_challenge_method: String,
    /// Written as "client_secret_basic" when empty.
    pub token_endpoint_auth_method: String,
    pub extra: Extra,
}

#[derive(Clone, Default)]
pub struct ServiceAccountCredential {
    pub r#type: String,
    pub project_id: String,
    pub private_key_id: String,
    pub private_key: String,
    pub client_email: String,
    pub client_id: String,
    pub auth_uri: String,
    pub token_uri: String,
    pub auth_provider_x509_cert_url: String,
    pub client_x509_cert_url: String,
    pub universe_domain: String,
    pub extra: Extra,
}

#[derive(Clone, Default)]
pub struct ServiceAccount {
    pub service_account_credential: Option<ServiceAccountCredential>,
    /// omitzero.
    pub scopes: Option<Vec<String>>,
    /// Always written.
    pub use_default_credential: bool,
    /// Always written.
    pub use_id_token: bool,
    pub audience: String,
    pub extra: Extra,
}

#[derive(Clone, Default)]
pub struct Credential {
    /// Required on decode.
    pub auth_type: CredentialType,
    pub resource_ref: String,
    pub api_key: String,
    pub http: Option<HttpAuth>,
    pub service_account: Option<ServiceAccount>,
    pub oauth2: Option<OAuth2Auth>,
    pub extra: Extra,
}
