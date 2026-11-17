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

//! `Debug` for the toolauth models, withholding secrets.
//!
//! The credential types print their secrets as `"<redacted>"`: the fields adk-python marks
//! `repr=False` in auth_credential.py, every value in `extra` and every value in
//! `additional_headers`, as its `BaseModelWithConfig.__repr_args__` does. Keys still show, and an
//! empty secret prints as `""`. Nested values redact themselves. Only the printed text is
//! redacted; the values keep their secrets. The scheme types hold no secret, so their `Debug`
//! is what `#[derive(Debug)]` would print. `{:?}` and `{:#?}` are the only forms.

use std::collections::BTreeMap;
use std::fmt::{self, Debug, Formatter};

use super::credential::{
    Credential, HttpAuth, HttpCredentials, OAuth2Auth, ServiceAccount, ServiceAccountCredential,
};
use super::schemes::{OAuthFlow, OAuthFlows, Scheme};
use super::tool::{Config, ToolArguments};

/// Printed in place of a secret that is set.
const REDACTED: &str = "<redacted>";

/// A non-empty string prints as `"<redacted>"`, an empty one as `""`.
struct Secret<'a>(&'a str);

impl Debug for Secret<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let shown = if self.0.is_empty() { "" } else { REDACTED };
        Debug::fmt(shown, f)
    }
}

/// The map with every value printed as `"<redacted>"`, so the keys still show.
struct RedactedValues<'a, V>(&'a BTreeMap<String, V>);

impl<V> Debug for RedactedValues<'_, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|k| (k, REDACTED)))
            .finish()
    }
}

impl Debug for OAuthFlow {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthFlow")
            .field("authorization_url", &self.authorization_url)
            .field("token_url", &self.token_url)
            .field("refresh_url", &self.refresh_url)
            .field("scopes", &self.scopes)
            .field("extra", &self.extra)
            .finish()
    }
}

impl Debug for OAuthFlows {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthFlows")
            .field("implicit", &self.implicit)
            .field("password", &self.password)
            .field("client_credentials", &self.client_credentials)
            .field("authorization_code", &self.authorization_code)
            .field("extra", &self.extra)
            .finish()
    }
}

impl Debug for Scheme {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scheme")
            .field("type", &self.r#type)
            .field("description", &self.description)
            .field("in", &self.r#in)
            .field("name", &self.name)
            .field("scheme", &self.scheme)
            .field("bearer_format", &self.bearer_format)
            .field("flows", &self.flows)
            .field("issuer_url", &self.issuer_url)
            .field("open_id_connect_url", &self.open_id_connect_url)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("userinfo_endpoint", &self.userinfo_endpoint)
            .field("revocation_endpoint", &self.revocation_endpoint)
            .field(
                "token_endpoint_auth_methods_supported",
                &self.token_endpoint_auth_methods_supported,
            )
            .field("grant_types_supported", &self.grant_types_supported)
            .field("scopes", &self.scopes)
            .field("extra", &self.extra)
            .finish()
    }
}

/// Password, token and the values in extra are redacted.
impl Debug for HttpCredentials {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpCredentials")
            .field("username", &self.username)
            .field("password", &Secret(&self.password))
            .field("token", &Secret(&self.token))
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// The values in additional headers and extra are redacted.
impl Debug for HttpAuth {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpAuth")
            .field("scheme", &self.scheme)
            .field("credentials", &self.credentials)
            .field(
                "additional_headers",
                &self.additional_headers.as_ref().map(RedactedValues),
            )
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// Client secret, auth response URI, auth code, access, refresh and ID
/// tokens, code verifier and the values in extra are redacted.
impl Debug for OAuth2Auth {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuth2Auth")
            .field("client_id", &self.client_id)
            .field("client_secret", &Secret(&self.client_secret))
            .field("auth_uri", &self.auth_uri)
            .field("nonce", &self.nonce)
            .field("state", &self.state)
            .field("redirect_uri", &self.redirect_uri)
            .field("auth_response_uri", &Secret(&self.auth_response_uri))
            .field("auth_code", &Secret(&self.auth_code))
            .field("access_token", &Secret(&self.access_token))
            .field("refresh_token", &Secret(&self.refresh_token))
            .field("id_token", &Secret(&self.id_token))
            .field("expires_at", &self.expires_at)
            .field("expires_in", &self.expires_in)
            .field("audience", &self.audience)
            .field("prompt", &self.prompt)
            .field("code_verifier", &Secret(&self.code_verifier))
            .field("code_challenge_method", &self.code_challenge_method)
            .field(
                "token_endpoint_auth_method",
                &self.token_endpoint_auth_method,
            )
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// Private key ID, private key and the values in extra are
/// redacted.
impl Debug for ServiceAccountCredential {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccountCredential")
            .field("type", &self.r#type)
            .field("project_id", &self.project_id)
            .field("private_key_id", &Secret(&self.private_key_id))
            .field("private_key", &Secret(&self.private_key))
            .field("client_email", &self.client_email)
            .field("client_id", &self.client_id)
            .field("auth_uri", &self.auth_uri)
            .field("token_uri", &self.token_uri)
            .field(
                "auth_provider_x509_cert_url",
                &self.auth_provider_x509_cert_url,
            )
            .field("client_x509_cert_url", &self.client_x509_cert_url)
            .field("universe_domain", &self.universe_domain)
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// The values in extra are redacted; the key redacts itself.
impl Debug for ServiceAccount {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccount")
            .field(
                "service_account_credential",
                &self.service_account_credential,
            )
            .field("scopes", &self.scopes)
            .field("use_default_credential", &self.use_default_credential)
            .field("use_id_token", &self.use_id_token)
            .field("audience", &self.audience)
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// The API key and the values in extra are redacted; the nested
/// credentials redact themselves.
impl Debug for Credential {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("auth_type", &self.auth_type)
            .field("resource_ref", &self.resource_ref)
            .field("api_key", &Secret(&self.api_key))
            .field("http", &self.http)
            .field("service_account", &self.service_account)
            .field("oauth2", &self.oauth2)
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// The values in extra are redacted; the credentials redact themselves.
impl Debug for Config {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("auth_scheme", &self.auth_scheme)
            .field("raw_auth_credential", &self.raw_auth_credential)
            .field("exchanged_auth_credential", &self.exchanged_auth_credential)
            .field("credential_key", &self.credential_key)
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}

/// The values in extra are redacted; the config redacts itself.
impl Debug for ToolArguments {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolArguments")
            .field("function_call_id", &self.function_call_id)
            .field("auth_config", &self.auth_config)
            .field("extra", &RedactedValues(&self.extra))
            .finish()
    }
}
