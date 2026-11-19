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

//! Credentials: HTTP, OAuth2 and service-account credentials and the [`Credential`] that wraps
//! them.
//!
//! The credentials are adk-python's pydantic models: each field is written under its camelCase
//! alias and read under the alias or its snake_case Python name, the alias winning when an object
//! has both (the snake_case value is then dropped). Keys that match no field are kept in `extra`
//! and written back after the fields. JSON types must match the fields exactly: `"3600"` is not
//! an integer and `"true"` is not a bool. Errors never quote the rejected value.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use super::Extra;
use super::model_json::{
    GoMarshal, GoUnmarshal, ObjectWriter, decode_object, deserialize_go, extra_eq, go_string,
    go_string_list, go_string_map, serialize_go,
};

/// The kind of a credential, open like `SchemeType`; decode rejects anything but the five.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialType(Cow<'static, str>);

impl CredentialType {
    /// `"apiKey"`: an API key in [`Credential::api_key`].
    pub const API_KEY: CredentialType = CredentialType(Cow::Borrowed("apiKey"));
    /// `"http"`: HTTP authentication in [`Credential::http`].
    pub const HTTP: CredentialType = CredentialType(Cow::Borrowed("http"));
    /// `"oauth2"`: OAuth 2.0 in [`Credential::oauth2`].
    pub const OAUTH2: CredentialType = CredentialType(Cow::Borrowed("oauth2"));
    /// `"openIdConnect"`: OpenID Connect, carried in [`Credential::oauth2`].
    pub const OPEN_ID_CONNECT: CredentialType = CredentialType(Cow::Borrowed("openIdConnect"));
    /// `"serviceAccount"`: a Google service account in [`Credential::service_account`].
    pub const SERVICE_ACCOUNT: CredentialType = CredentialType(Cow::Borrowed("serviceAccount"));

    /// Wraps any string. Only the five constants decode inside a [`Credential`].
    pub fn new(s: impl Into<Cow<'static, str>>) -> Self {
        CredentialType(s.into())
    }

    /// The wire string, e.g. `"oauth2"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for CredentialType {
    // "".
    fn default() -> Self {
        CredentialType(Cow::Borrowed(""))
    }
}

impl From<&'static str> for CredentialType {
    fn from(s: &'static str) -> Self {
        CredentialType(Cow::Borrowed(s))
    }
}

impl From<String> for CredentialType {
    fn from(s: String) -> Self {
        CredentialType(Cow::Owned(s))
    }
}

/// A JSON string. Any string decodes; [`Credential`]'s decode
/// is what rejects an unknown type.
impl Serialize for CredentialType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CredentialType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(CredentialType::from)
    }
}

/// Writes `serde::Serialize` and `serde::Deserialize` for a model through its own encoder and
/// decoder.
macro_rules! go_serde {
    ($($ty:ty),* $(,)?) => {$(
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                serialize_go(self, s)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                deserialize_go(d)
            }
        }
    )*};
}

go_serde!(
    HttpCredentials,
    HttpAuth,
    OAuth2Auth,
    ServiceAccountCredential,
    ServiceAccount,
    Credential,
);

/// Writes each non-empty string field; empty ones are omitted.
fn non_empty(w: &mut ObjectWriter, fields: &[(&str, &String)]) {
    for (alias, value) in fields {
        if !value.is_empty() {
            w.field(alias, &go_string(value));
        }
    }
}

/// The secrets of an HTTP credential: a username and password for basic auth, or a token for
/// bearer auth. Empty fields are omitted when written.
#[derive(Clone, Default)]
pub struct HttpCredentials {
    /// `"username"`: the basic-auth user name.
    pub username: String,
    /// `"password"`: the basic-auth password.
    pub password: String,
    /// `"token"`: the bearer (or other scheme) token.
    pub token: String,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const HTTP_CREDENTIALS_FIELDS: &[&str] = &["username", "password", "token"];

impl GoUnmarshal for HttpCredentials {
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "HTTPCredentials";
        let mut d = decode_object(raw, T, HTTP_CREDENTIALS_FIELDS, true, &[])?;
        Ok(HttpCredentials {
            username: d.take("username", T, "string")?.unwrap_or_default(),
            password: d.take("password", T, "string")?.unwrap_or_default(),
            token: d.take("token", T, "string")?.unwrap_or_default(),
            extra: d.extra,
        })
    }
}

impl GoMarshal for HttpCredentials {
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        non_empty(
            &mut w,
            &[
                ("username", &self.username),
                ("password", &self.password),
                ("token", &self.token),
            ],
        );
        w.finish(&self.extra, HTTP_CREDENTIALS_FIELDS, true)
    }
}

impl PartialEq for HttpCredentials {
    fn eq(&self, other: &Self) -> bool {
        self.username == other.username
            && self.password == other.password
            && self.token == other.token
            && extra_eq(&self.extra, &other.extra)
    }
}

/// An HTTP credential: the scheme (e.g. `basic`, `bearer`), its secrets and optional extra
/// request headers.
#[derive(Clone, Default)]
pub struct HttpAuth {
    /// Required on decode.
    pub scheme: String,
    /// Required on decode.
    pub credentials: HttpCredentials,
    /// "additionalHeaders", omitzero.
    pub additional_headers: Option<BTreeMap<String, String>>,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const HTTP_AUTH_FIELDS: &[&str] = &["scheme", "credentials", "additionalHeaders"];

impl GoUnmarshal for HttpAuth {
    /// Decodes an [`HttpAuth`]: scheme and credentials are required.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "HTTPAuth";
        let mut d = decode_object(raw, T, HTTP_AUTH_FIELDS, true, &["scheme", "credentials"])?;
        Ok(HttpAuth {
            scheme: d.take("scheme", T, "string")?.unwrap_or_default(),
            credentials: d
                .take("credentials", T, "toolauth.HTTPCredentials")?
                .unwrap_or_default(),
            additional_headers: d.take_string_map("additionalHeaders", T)?,
            extra: d.extra,
        })
    }
}

impl GoMarshal for HttpAuth {
    /// Encodes an [`HttpAuth`]: credentials are written as `{}` even when empty.
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        w.field("scheme", &go_string(&self.scheme));
        w.field("credentials", &self.credentials.go_marshal());
        if let Some(headers) = &self.additional_headers {
            w.field("additionalHeaders", &go_string_map(headers));
        }
        w.finish(&self.extra, HTTP_AUTH_FIELDS, true)
    }
}

impl PartialEq for HttpAuth {
    fn eq(&self, other: &Self) -> bool {
        self.scheme == other.scheme
            && self.credentials == other.credentials
            && self.additional_headers == other.additional_headers
            && extra_eq(&self.extra, &other.extra)
    }
}

/// An OAuth 2.0 (or OpenID Connect) credential: the client registration, the state of an
/// authorization-code exchange, and the tokens it produced. Empty strings and zero integers are
/// omitted when written.
#[derive(Clone, Default)]
pub struct OAuth2Auth {
    /// `"clientId"`: the OAuth client ID.
    pub client_id: String,
    /// `"clientSecret"`: the OAuth client secret.
    pub client_secret: String,
    /// `"authUri"`: the authorization URL the end user is sent to.
    pub auth_uri: String,
    /// `"nonce"`: the OpenID Connect nonce.
    pub nonce: String,
    /// `"state"`: the OAuth `state` parameter of the authorization request.
    pub state: String,
    /// `"redirectUri"`: the redirect URI registered for the client.
    pub redirect_uri: String,
    /// `"authResponseUri"`: the full redirect URL the client returned after consent.
    pub auth_response_uri: String,
    /// `"authCode"`: the authorization code.
    pub auth_code: String,
    /// `"accessToken"`: the access token.
    pub access_token: String,
    /// `"refreshToken"`: the refresh token.
    pub refresh_token: String,
    /// `"idToken"`: the OpenID Connect ID token.
    pub id_token: String,
    /// `"expiresAt"`: when the access token expires; omitted when 0.
    pub expires_at: i64,
    /// `"expiresIn"`: the access token lifetime as reported by the token endpoint; omitted when
    /// 0.
    pub expires_in: i64,
    /// `"audience"`: the requested token audience.
    pub audience: String,
    /// `"prompt"`: the OAuth `prompt` parameter.
    pub prompt: String,
    /// `"codeVerifier"`: the PKCE code verifier.
    pub code_verifier: String,
    /// `"codeChallengeMethod"`: the PKCE code challenge method.
    pub code_challenge_method: String,
    /// Written as "client_secret_basic" when empty.
    pub token_endpoint_auth_method: String,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const OAUTH2_AUTH_FIELDS: &[&str] = &[
    "clientId",
    "clientSecret",
    "authUri",
    "nonce",
    "state",
    "redirectUri",
    "authResponseUri",
    "authCode",
    "accessToken",
    "refreshToken",
    "idToken",
    "expiresAt",
    "expiresIn",
    "audience",
    "prompt",
    "codeVerifier",
    "codeChallengeMethod",
    "tokenEndpointAuthMethod",
];

impl GoUnmarshal for OAuth2Auth {
    /// Decodes an [`OAuth2Auth`]: rejects a token endpoint auth method other than
    /// client_secret_basic, client_secret_post, client_secret_jwt and private_key_jwt.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "OAuth2Auth";
        let mut d = decode_object(raw, T, OAUTH2_AUTH_FIELDS, true, &[])?;
        let mut s = |alias: &str| -> Result<String, String> {
            Ok(d.take(alias, T, "string")?.unwrap_or_default())
        };
        let client_id = s("clientId")?;
        let client_secret = s("clientSecret")?;
        let auth_uri = s("authUri")?;
        let nonce = s("nonce")?;
        let state = s("state")?;
        let redirect_uri = s("redirectUri")?;
        let auth_response_uri = s("authResponseUri")?;
        let auth_code = s("authCode")?;
        let access_token = s("accessToken")?;
        let refresh_token = s("refreshToken")?;
        let id_token = s("idToken")?;
        let expires_at = d.take_i64("expiresAt", T)?.unwrap_or_default();
        let expires_in = d.take_i64("expiresIn", T)?.unwrap_or_default();
        let mut s = |alias: &str| -> Result<String, String> {
            Ok(d.take(alias, T, "string")?.unwrap_or_default())
        };
        let audience = s("audience")?;
        let prompt = s("prompt")?;
        let code_verifier = s("codeVerifier")?;
        let code_challenge_method = s("codeChallengeMethod")?;
        let token_endpoint_auth_method = s("tokenEndpointAuthMethod")?;
        match token_endpoint_auth_method.as_str() {
            ""
            | "client_secret_basic"
            | "client_secret_post"
            | "client_secret_jwt"
            | "private_key_jwt" => {}
            _ => return Err("toolauth: OAuth2Auth: unsupported tokenEndpointAuthMethod".to_owned()),
        }
        Ok(OAuth2Auth {
            client_id,
            client_secret,
            auth_uri,
            nonce,
            state,
            redirect_uri,
            auth_response_uri,
            auth_code,
            access_token,
            refresh_token,
            id_token,
            expires_at,
            expires_in,
            audience,
            prompt,
            code_verifier,
            code_challenge_method,
            token_endpoint_auth_method,
            extra: d.extra,
        })
    }
}

impl GoMarshal for OAuth2Auth {
    /// Encodes an [`OAuth2Auth`]: an empty token endpoint auth method is written as
    /// client_secret_basic, which is what adk-python sends.
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        non_empty(
            &mut w,
            &[
                ("clientId", &self.client_id),
                ("clientSecret", &self.client_secret),
                ("authUri", &self.auth_uri),
                ("nonce", &self.nonce),
                ("state", &self.state),
                ("redirectUri", &self.redirect_uri),
                ("authResponseUri", &self.auth_response_uri),
                ("authCode", &self.auth_code),
                ("accessToken", &self.access_token),
                ("refreshToken", &self.refresh_token),
                ("idToken", &self.id_token),
            ],
        );
        if self.expires_at != 0 {
            w.field("expiresAt", &self.expires_at.to_string());
        }
        if self.expires_in != 0 {
            w.field("expiresIn", &self.expires_in.to_string());
        }
        non_empty(
            &mut w,
            &[
                ("audience", &self.audience),
                ("prompt", &self.prompt),
                ("codeVerifier", &self.code_verifier),
                ("codeChallengeMethod", &self.code_challenge_method),
            ],
        );
        let method = if self.token_endpoint_auth_method.is_empty() {
            "client_secret_basic"
        } else {
            &self.token_endpoint_auth_method
        };
        w.field("tokenEndpointAuthMethod", &go_string(method));
        w.finish(&self.extra, OAUTH2_AUTH_FIELDS, true)
    }
}

impl PartialEq for OAuth2Auth {
    fn eq(&self, other: &Self) -> bool {
        self.client_id == other.client_id
            && self.client_secret == other.client_secret
            && self.auth_uri == other.auth_uri
            && self.nonce == other.nonce
            && self.state == other.state
            && self.redirect_uri == other.redirect_uri
            && self.auth_response_uri == other.auth_response_uri
            && self.auth_code == other.auth_code
            && self.access_token == other.access_token
            && self.refresh_token == other.refresh_token
            && self.id_token == other.id_token
            && self.expires_at == other.expires_at
            && self.expires_in == other.expires_in
            && self.audience == other.audience
            && self.prompt == other.prompt
            && self.code_verifier == other.code_verifier
            && self.code_challenge_method == other.code_challenge_method
            && self.token_endpoint_auth_method == other.token_endpoint_auth_method
            && extra_eq(&self.extra, &other.extra)
    }
}

/// A Google service-account key, as in a downloaded JSON key file. Decoding requires every field
/// but `type`; every field is written, even when empty.
#[derive(Clone, Default)]
pub struct ServiceAccountCredential {
    /// `"type"`: the key type, normally `service_account`.
    pub r#type: String,
    /// `"projectId"`: the Google Cloud project ID.
    pub project_id: String,
    /// `"privateKeyId"`: the ID of the private key.
    pub private_key_id: String,
    /// `"privateKey"`: the PEM-encoded private key.
    pub private_key: String,
    /// `"clientEmail"`: the service account's email address.
    pub client_email: String,
    /// `"clientId"`: the service account's client ID.
    pub client_id: String,
    /// `"authUri"`: the OAuth authorization endpoint.
    pub auth_uri: String,
    /// `"tokenUri"`: the OAuth token endpoint.
    pub token_uri: String,
    /// `"authProviderX509CertUrl"`: the URL of the auth provider's X.509 certificates.
    pub auth_provider_x509_cert_url: String,
    /// `"clientX509CertUrl"`: the URL of the service account's X.509 certificate.
    pub client_x509_cert_url: String,
    /// `"universeDomain"`: the Google Cloud universe domain.
    pub universe_domain: String,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const SERVICE_ACCOUNT_CREDENTIAL_FIELDS: &[&str] = &[
    "type",
    "projectId",
    "privateKeyId",
    "privateKey",
    "clientEmail",
    "clientId",
    "authUri",
    "tokenUri",
    "authProviderX509CertUrl",
    "clientX509CertUrl",
    "universeDomain",
];

impl GoUnmarshal for ServiceAccountCredential {
    /// Decodes a [`ServiceAccountCredential`]: reads a key file too, whose names are
    /// snake_case. Every field but type is required.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "ServiceAccountCredential";
        let mut d = decode_object(
            raw,
            T,
            SERVICE_ACCOUNT_CREDENTIAL_FIELDS,
            true,
            &SERVICE_ACCOUNT_CREDENTIAL_FIELDS[1..],
        )?;
        let mut s = |alias: &str| -> Result<String, String> {
            Ok(d.take(alias, T, "string")?.unwrap_or_default())
        };
        Ok(ServiceAccountCredential {
            r#type: s("type")?,
            project_id: s("projectId")?,
            private_key_id: s("privateKeyId")?,
            private_key: s("privateKey")?,
            client_email: s("clientEmail")?,
            client_id: s("clientId")?,
            auth_uri: s("authUri")?,
            token_uri: s("tokenUri")?,
            auth_provider_x509_cert_url: s("authProviderX509CertUrl")?,
            client_x509_cert_url: s("clientX509CertUrl")?,
            universe_domain: s("universeDomain")?,
            extra: d.extra,
        })
    }
}

impl GoMarshal for ServiceAccountCredential {
    /// Encodes a [`ServiceAccountCredential`]: every field is written, even when empty.
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        let fields = [
            ("type", &self.r#type),
            ("projectId", &self.project_id),
            ("privateKeyId", &self.private_key_id),
            ("privateKey", &self.private_key),
            ("clientEmail", &self.client_email),
            ("clientId", &self.client_id),
            ("authUri", &self.auth_uri),
            ("tokenUri", &self.token_uri),
            ("authProviderX509CertUrl", &self.auth_provider_x509_cert_url),
            ("clientX509CertUrl", &self.client_x509_cert_url),
            ("universeDomain", &self.universe_domain),
        ];
        for (alias, value) in fields {
            w.field(alias, &go_string(value));
        }
        w.finish(&self.extra, SERVICE_ACCOUNT_CREDENTIAL_FIELDS, true)
    }
}

impl PartialEq for ServiceAccountCredential {
    fn eq(&self, other: &Self) -> bool {
        self.r#type == other.r#type
            && self.project_id == other.project_id
            && self.private_key_id == other.private_key_id
            && self.private_key == other.private_key
            && self.client_email == other.client_email
            && self.client_id == other.client_id
            && self.auth_uri == other.auth_uri
            && self.token_uri == other.token_uri
            && self.auth_provider_x509_cert_url == other.auth_provider_x509_cert_url
            && self.client_x509_cert_url == other.client_x509_cert_url
            && self.universe_domain == other.universe_domain
            && extra_eq(&self.extra, &other.extra)
    }
}

/// A service-account credential: either an explicit key or Application Default Credentials, with
/// the scopes to request or, for an ID token, its audience. Decoding rejects a value with neither
/// a key nor `useDefaultCredential`, and `useIdToken` without an audience.
#[derive(Clone, Default)]
pub struct ServiceAccount {
    /// `"serviceAccountCredential"`: the key; required unless
    /// [`use_default_credential`](Self::use_default_credential) is set.
    pub service_account_credential: Option<ServiceAccountCredential>,
    /// omitzero.
    pub scopes: Option<Vec<String>>,
    /// Always written.
    pub use_default_credential: bool,
    /// Always written.
    pub use_id_token: bool,
    /// `"audience"`: the ID token audience; required with
    /// [`use_id_token`](Self::use_id_token).
    pub audience: String,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const SERVICE_ACCOUNT_FIELDS: &[&str] = &[
    "serviceAccountCredential",
    "scopes",
    "useDefaultCredential",
    "useIdToken",
    "audience",
];

impl GoUnmarshal for ServiceAccount {
    /// Decodes a [`ServiceAccount`]: rejects neither a key nor ADC, and an ID token without
    /// an audience.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "ServiceAccount";
        let mut d = decode_object(raw, T, SERVICE_ACCOUNT_FIELDS, true, &[])?;
        let sa = ServiceAccount {
            service_account_credential: d.take(
                "serviceAccountCredential",
                T,
                "*toolauth.ServiceAccountCredential",
            )?,
            scopes: d.take_string_list("scopes", T)?,
            use_default_credential: d
                .take("useDefaultCredential", T, "bool")?
                .unwrap_or_default(),
            use_id_token: d.take("useIdToken", T, "bool")?.unwrap_or_default(),
            audience: d.take("audience", T, "string")?.unwrap_or_default(),
            extra: d.extra,
        };
        if !sa.use_default_credential && sa.service_account_credential.is_none() {
            return Err("toolauth: ServiceAccount: serviceAccountCredential is required unless useDefaultCredential is set".to_owned());
        }
        if sa.use_id_token && sa.audience.is_empty() {
            return Err(
                "toolauth: ServiceAccount: audience is required with useIdToken".to_owned(),
            );
        }
        Ok(sa)
    }
}

impl GoMarshal for ServiceAccount {
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        if let Some(key) = &self.service_account_credential {
            w.field("serviceAccountCredential", &key.go_marshal());
        }
        if let Some(scopes) = &self.scopes {
            w.field("scopes", &go_string_list(scopes));
        }
        w.field("useDefaultCredential", go_bool(self.use_default_credential));
        w.field("useIdToken", go_bool(self.use_id_token));
        non_empty(&mut w, &[("audience", &self.audience)]);
        w.finish(&self.extra, SERVICE_ACCOUNT_FIELDS, true)
    }
}

fn go_bool(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

impl PartialEq for ServiceAccount {
    fn eq(&self, other: &Self) -> bool {
        self.service_account_credential == other.service_account_credential
            && self.scopes == other.scopes
            && self.use_default_credential == other.use_default_credential
            && self.use_id_token == other.use_id_token
            && self.audience == other.audience
            && extra_eq(&self.extra, &other.extra)
    }
}

/// A credential: its [`CredentialType`] and the matching payload. Decoding rejects a missing or
/// unknown auth type and an invalid payload in any field; it does not check that the payload
/// matches the type.
#[derive(Clone, Default)]
pub struct Credential {
    /// `"authType"`: the credential kind. Required on decode.
    pub auth_type: CredentialType,
    /// `"resourceRef"`: a reference to the resource the credential is for; omitted when empty.
    pub resource_ref: String,
    /// `"apiKey"`: the API key, for [`CredentialType::API_KEY`]; omitted when empty.
    pub api_key: String,
    /// `"http"`: the HTTP credential, for [`CredentialType::HTTP`].
    pub http: Option<HttpAuth>,
    /// `"serviceAccount"`: the service account, for [`CredentialType::SERVICE_ACCOUNT`].
    pub service_account: Option<ServiceAccount>,
    /// `"oauth2"`: the OAuth 2.0 credential, for [`CredentialType::OAUTH2`] and
    /// [`CredentialType::OPEN_ID_CONNECT`].
    pub oauth2: Option<OAuth2Auth>,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const CREDENTIAL_FIELDS: &[&str] = &[
    "authType",
    "resourceRef",
    "apiKey",
    "http",
    "serviceAccount",
    "oauth2",
];

impl GoUnmarshal for Credential {
    /// Decodes a [`Credential`]: rejects a missing or unknown auth type and an invalid
    /// credential in any field.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "Credential";
        let mut d = decode_object(raw, T, CREDENTIAL_FIELDS, true, &["authType"])?;
        let c = Credential {
            auth_type: d
                .take("authType", T, "toolauth.CredentialType")?
                .unwrap_or_default(),
            resource_ref: d.take("resourceRef", T, "string")?.unwrap_or_default(),
            api_key: d.take("apiKey", T, "string")?.unwrap_or_default(),
            http: d.take("http", T, "*toolauth.HTTPAuth")?,
            service_account: d.take("serviceAccount", T, "*toolauth.ServiceAccount")?,
            oauth2: d.take("oauth2", T, "*toolauth.OAuth2Auth")?,
            extra: d.extra,
        };
        let known = [
            CredentialType::API_KEY,
            CredentialType::HTTP,
            CredentialType::OAUTH2,
            CredentialType::OPEN_ID_CONNECT,
            CredentialType::SERVICE_ACCOUNT,
        ];
        if !known.contains(&c.auth_type) {
            return Err("toolauth: Credential: unknown authType".to_owned());
        }
        Ok(c)
    }
}

impl GoMarshal for Credential {
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        w.field("authType", &go_string(self.auth_type.as_str()));
        non_empty(
            &mut w,
            &[
                ("resourceRef", &self.resource_ref),
                ("apiKey", &self.api_key),
            ],
        );
        if let Some(http) = &self.http {
            w.field("http", &http.go_marshal());
        }
        if let Some(sa) = &self.service_account {
            w.field("serviceAccount", &sa.go_marshal());
        }
        if let Some(oauth2) = &self.oauth2 {
            w.field("oauth2", &oauth2.go_marshal());
        }
        w.finish(&self.extra, CREDENTIAL_FIELDS, true)
    }
}

impl PartialEq for Credential {
    fn eq(&self, other: &Self) -> bool {
        self.auth_type == other.auth_type
            && self.resource_ref == other.resource_ref
            && self.api_key == other.api_key
            && self.http == other.http
            && self.service_account == other.service_account
            && self.oauth2 == other.oauth2
            && extra_eq(&self.extra, &other.extra)
    }
}
