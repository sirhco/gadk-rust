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

//! Authentication schemes.
//!
//! The schemes are fastapi's OpenAPI models: each field is read under its exact OpenAPI name
//! only, so a snake_case spelling of a camelCase field is an unknown key, kept in `extra`.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use super::Extra;
use super::model_json::{
    GoMarshal, GoUnmarshal, ObjectWriter, decode_object, deserialize_go, extra_eq, go_string,
    go_string_list, go_string_map, serialize_go,
};

/// An OAuth grant type; `as_str` is the RFC 6749 wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OAuthGrantType {
    /// `client_credentials`.
    ClientCredentials,
    /// `authorization_code`.
    AuthorizationCode,
    /// `implicit`.
    Implicit,
    /// `password`.
    Password,
}

impl OAuthGrantType {
    /// "client_credentials" "authorization_code" "implicit" "password".
    pub fn as_str(self) -> &'static str {
        match self {
            OAuthGrantType::ClientCredentials => "client_credentials",
            OAuthGrantType::AuthorizationCode => "authorization_code",
            OAuthGrantType::Implicit => "implicit",
            OAuthGrantType::Password => "password",
        }
    }
}

/// One OAuth flow. Fields are written in the order authorizationUrl, tokenUrl, refreshUrl,
/// scopes.
#[derive(Clone, Default)]
pub struct OAuthFlow {
    /// `"authorizationUrl"`: the authorization endpoint; omitted when empty.
    pub authorization_url: String,
    /// `"tokenUrl"`: the token endpoint; omitted when empty.
    pub token_url: String,
    /// `"refreshUrl"`: the refresh endpoint; omitted when empty.
    pub refresh_url: String,
    /// Always written, `{}` when empty.
    pub scopes: BTreeMap<String, String>,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const OAUTH_FLOW_FIELDS: &[&str] = &["authorizationUrl", "tokenUrl", "refreshUrl", "scopes"];

impl GoUnmarshal for OAuthFlow {
    /// Decodes an [`OAuthFlow`]. Which URLs are required depends on the flow, so
    /// [`OAuthFlows`] checks them.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "OAuthFlow";
        let mut d = decode_object(raw, T, OAUTH_FLOW_FIELDS, false, &[])?;
        Ok(OAuthFlow {
            authorization_url: d.take("authorizationUrl", T, "String")?.unwrap_or_default(),
            token_url: d.take("tokenUrl", T, "String")?.unwrap_or_default(),
            refresh_url: d.take("refreshUrl", T, "String")?.unwrap_or_default(),
            scopes: d.take_string_map("scopes", T)?.unwrap_or_default(),
            extra: d.extra,
        })
    }
}

impl GoMarshal for OAuthFlow {
    /// Encodes an [`OAuthFlow`]: empty scopes are written as `{}`, as adk-python writes them.
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        if !self.authorization_url.is_empty() {
            w.field("authorizationUrl", &go_string(&self.authorization_url));
        }
        if !self.token_url.is_empty() {
            w.field("tokenUrl", &go_string(&self.token_url));
        }
        if !self.refresh_url.is_empty() {
            w.field("refreshUrl", &go_string(&self.refresh_url));
        }
        w.field("scopes", &go_string_map(&self.scopes));
        w.finish(&self.extra, OAUTH_FLOW_FIELDS, false)
    }
}

impl PartialEq for OAuthFlow {
    fn eq(&self, other: &Self) -> bool {
        self.authorization_url == other.authorization_url
            && self.token_url == other.token_url
            && self.refresh_url == other.refresh_url
            && self.scopes == other.scopes
            && extra_eq(&self.extra, &other.extra)
    }
}

impl Serialize for OAuthFlow {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_go(self, s)
    }
}

impl<'de> Deserialize<'de> for OAuthFlow {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        deserialize_go(d)
    }
}

/// The OAuth flows of a scheme. Decoding rejects a flow without the URLs it needs (an empty URL
/// counts as missing): implicit needs `authorizationUrl`, password and client credentials need
/// `tokenUrl`, authorization code needs both.
#[derive(Clone, Default)]
pub struct OAuthFlows {
    /// `"implicit"`: the implicit flow.
    pub implicit: Option<OAuthFlow>,
    /// `"password"`: the resource-owner password flow.
    pub password: Option<OAuthFlow>,
    /// `"clientCredentials"`: the client-credentials flow.
    pub client_credentials: Option<OAuthFlow>,
    /// `"authorizationCode"`: the authorization-code flow.
    pub authorization_code: Option<OAuthFlow>,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const OAUTH_FLOWS_FIELDS: &[&str] = &[
    "implicit",
    "password",
    "clientCredentials",
    "authorizationCode",
];

impl OAuthFlows {
    /// The grant type: client credentials, authorization code, implicit,
    /// password, in that order; `None` when no flow is set.
    pub fn grant_type(&self) -> Option<OAuthGrantType> {
        if self.client_credentials.is_some() {
            Some(OAuthGrantType::ClientCredentials)
        } else if self.authorization_code.is_some() {
            Some(OAuthGrantType::AuthorizationCode)
        } else if self.implicit.is_some() {
            Some(OAuthGrantType::Implicit)
        } else if self.password.is_some() {
            Some(OAuthGrantType::Password)
        } else {
            None
        }
    }
}

impl GoUnmarshal for OAuthFlows {
    /// Decodes an [`OAuthFlows`]: rejects a flow without the URLs it needs. An empty URL
    /// counts as missing.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "OAuthFlows";
        const FLOW: &str = "OAuthFlow";
        let mut d = decode_object(raw, T, OAUTH_FLOWS_FIELDS, false, &[])?;
        let flows = OAuthFlows {
            implicit: d.take("implicit", T, FLOW)?,
            password: d.take("password", T, FLOW)?,
            client_credentials: d.take("clientCredentials", T, FLOW)?,
            authorization_code: d.take("authorizationCode", T, FLOW)?,
            extra: d.extra,
        };
        let checks = [
            ("implicit", &flows.implicit, true, false),
            ("password", &flows.password, false, true),
            ("clientCredentials", &flows.client_credentials, false, true),
            ("authorizationCode", &flows.authorization_code, true, true),
        ];
        for (name, flow, need_auth, need_token) in checks {
            let Some(flow) = flow else { continue };
            if need_auth && flow.authorization_url.is_empty() {
                return Err(format!(
                    "toolauth: OAuthFlows: {name} flow needs authorizationUrl"
                ));
            }
            if need_token && flow.token_url.is_empty() {
                return Err(format!("toolauth: OAuthFlows: {name} flow needs tokenUrl"));
            }
        }
        Ok(flows)
    }
}

impl GoMarshal for OAuthFlows {
    /// Encodes an [`OAuthFlows`].
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        let flows = [
            ("implicit", &self.implicit),
            ("password", &self.password),
            ("clientCredentials", &self.client_credentials),
            ("authorizationCode", &self.authorization_code),
        ];
        for (alias, flow) in flows {
            if let Some(flow) = flow {
                w.field(alias, &flow.go_marshal());
            }
        }
        w.finish(&self.extra, OAUTH_FLOWS_FIELDS, false)
    }
}

impl PartialEq for OAuthFlows {
    fn eq(&self, other: &Self) -> bool {
        self.implicit == other.implicit
            && self.password == other.password
            && self.client_credentials == other.client_credentials
            && self.authorization_code == other.authorization_code
            && extra_eq(&self.extra, &other.extra)
    }
}

impl Serialize for OAuthFlows {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_go(self, s)
    }
}

impl<'de> Deserialize<'de> for OAuthFlows {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        deserialize_go(d)
    }
}

/// The type of a scheme, an open string ("gcp_provider" and other custom types round-trip).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemeType(Cow<'static, str>);

impl SchemeType {
    /// `"apiKey"`: an API key sent in a header, query parameter or cookie.
    pub const API_KEY: SchemeType = SchemeType(Cow::Borrowed("apiKey"));
    /// `"http"`: HTTP authentication such as basic or bearer.
    pub const HTTP: SchemeType = SchemeType(Cow::Borrowed("http"));
    /// `"oauth2"`: OAuth 2.0 with [`OAuthFlows`].
    pub const OAUTH2: SchemeType = SchemeType(Cow::Borrowed("oauth2"));
    /// `"openIdConnect"`: OpenID Connect.
    pub const OPEN_ID_CONNECT: SchemeType = SchemeType(Cow::Borrowed("openIdConnect"));

    /// Wraps any string; custom types are kept as they are.
    pub fn new(s: impl Into<Cow<'static, str>>) -> Self {
        SchemeType(s.into())
    }

    /// The wire string, e.g. `"oauth2"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SchemeType {
    // "".
    fn default() -> Self {
        SchemeType(Cow::Borrowed(""))
    }
}

impl From<&'static str> for SchemeType {
    fn from(s: &'static str) -> Self {
        SchemeType(Cow::Borrowed(s))
    }
}

impl From<String> for SchemeType {
    fn from(s: String) -> Self {
        SchemeType(Cow::Owned(s))
    }
}

/// A JSON string.
impl Serialize for SchemeType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SchemeType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(SchemeType::from)
    }
}

/// An authentication scheme: an OpenAPI security scheme plus adk-python's OpenID Connect
/// fields. Fields are written in declaration order (wire names below); empty strings are
/// omitted, and a `None` list is omitted while `Some(vec![])` writes `[]`.
///
/// Decoding does not check that the fields fit the type. A missing `type` is inferred: `in` and
/// `name` give API key, `flows` gives OAuth 2.0, `openIdConnectUrl` (or both the authorization and
/// token endpoints) gives OpenID Connect, `scheme` gives HTTP; with none of these, decoding fails.
#[derive(Clone, Default)]
pub struct Scheme {
    /// `"type"`: the scheme type.
    pub r#type: SchemeType,
    /// `"description"`: a human-readable description.
    pub description: String,
    /// `"in"`: where an API key goes (`header`, `query` or `cookie`).
    pub r#in: String,
    /// `"name"`: the API key's header, query parameter or cookie name.
    pub name: String,
    /// `"scheme"`: the HTTP authentication scheme, e.g. `basic` or `bearer`.
    pub scheme: String,
    /// `"bearerFormat"`: a hint for the bearer token's format, e.g. `JWT`.
    pub bearer_format: String,
    /// `"flows"`: the OAuth 2.0 flows.
    pub flows: Option<OAuthFlows>,
    /// `"issuer_url"`: the OpenID Connect issuer.
    pub issuer_url: String,
    /// `"openIdConnectUrl"`: the OpenID Connect discovery URL.
    pub open_id_connect_url: String,
    /// `"authorization_endpoint"`: the OpenID Connect authorization endpoint.
    pub authorization_endpoint: String,
    /// `"token_endpoint"`: the OpenID Connect token endpoint.
    pub token_endpoint: String,
    /// `"userinfo_endpoint"`: the OpenID Connect userinfo endpoint.
    pub userinfo_endpoint: String,
    /// `"revocation_endpoint"`: the token revocation endpoint.
    pub revocation_endpoint: String,
    /// `"token_endpoint_auth_methods_supported"`: the client authentication methods the token
    /// endpoint accepts.
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    /// `"grant_types_supported"`: the supported OAuth grant types.
    pub grant_types_supported: Option<Vec<String>>,
    /// `"scopes"`: the scopes to request.
    pub scopes: Option<Vec<String>>,
    /// Unknown keys, written back after the fields.
    pub extra: Extra,
}

const SCHEME_FIELDS: &[&str] = &[
    "type",
    "description",
    "in",
    "name",
    "scheme",
    "bearerFormat",
    "flows",
    "issuer_url",
    "openIdConnectUrl",
    "authorization_endpoint",
    "token_endpoint",
    "userinfo_endpoint",
    "revocation_endpoint",
    "token_endpoint_auth_methods_supported",
    "grant_types_supported",
    "scopes",
];

impl GoUnmarshal for Scheme {
    /// Decodes a [`Scheme`]: does not check that the fields fit the type, and infers a
    /// missing type from the fields, as adk-python's models do.
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String> {
        const T: &str = "Scheme";
        let mut d = decode_object(raw, T, SCHEME_FIELDS, false, &[])?;
        let mut s = Scheme {
            r#type: d.take("type", T, "SchemeType")?.unwrap_or_default(),
            description: d.take("description", T, "String")?.unwrap_or_default(),
            r#in: d.take("in", T, "String")?.unwrap_or_default(),
            name: d.take("name", T, "String")?.unwrap_or_default(),
            scheme: d.take("scheme", T, "String")?.unwrap_or_default(),
            bearer_format: d.take("bearerFormat", T, "String")?.unwrap_or_default(),
            flows: d.take("flows", T, "OAuthFlows")?,
            issuer_url: d.take("issuer_url", T, "String")?.unwrap_or_default(),
            open_id_connect_url: d.take("openIdConnectUrl", T, "String")?.unwrap_or_default(),
            authorization_endpoint: d
                .take("authorization_endpoint", T, "String")?
                .unwrap_or_default(),
            token_endpoint: d.take("token_endpoint", T, "String")?.unwrap_or_default(),
            userinfo_endpoint: d
                .take("userinfo_endpoint", T, "String")?
                .unwrap_or_default(),
            revocation_endpoint: d
                .take("revocation_endpoint", T, "String")?
                .unwrap_or_default(),
            token_endpoint_auth_methods_supported: d
                .take_string_list("token_endpoint_auth_methods_supported", T)?,
            grant_types_supported: d.take_string_list("grant_types_supported", T)?,
            scopes: d.take_string_list("scopes", T)?,
            extra: d.extra,
        };
        if s.r#type.as_str().is_empty() {
            s.r#type = if !s.r#in.is_empty() && !s.name.is_empty() {
                SchemeType::API_KEY
            } else if s.flows.is_some() {
                SchemeType::OAUTH2
            } else if !s.open_id_connect_url.is_empty()
                || (!s.authorization_endpoint.is_empty() && !s.token_endpoint.is_empty())
            {
                SchemeType::OPEN_ID_CONNECT
            } else if !s.scheme.is_empty() {
                SchemeType::HTTP
            } else {
                return Err("toolauth: Scheme: missing type, and no field says which".to_owned());
            };
        }
        Ok(s)
    }
}

impl GoMarshal for Scheme {
    /// Encodes a [`Scheme`].
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        w.field("type", &go_string(self.r#type.as_str()));
        let strings = [
            ("description", &self.description),
            ("in", &self.r#in),
            ("name", &self.name),
            ("scheme", &self.scheme),
            ("bearerFormat", &self.bearer_format),
        ];
        for (alias, value) in strings {
            if !value.is_empty() {
                w.field(alias, &go_string(value));
            }
        }
        if let Some(flows) = &self.flows {
            w.field("flows", &flows.go_marshal());
        }
        let strings = [
            ("issuer_url", &self.issuer_url),
            ("openIdConnectUrl", &self.open_id_connect_url),
            ("authorization_endpoint", &self.authorization_endpoint),
            ("token_endpoint", &self.token_endpoint),
            ("userinfo_endpoint", &self.userinfo_endpoint),
            ("revocation_endpoint", &self.revocation_endpoint),
        ];
        for (alias, value) in strings {
            if !value.is_empty() {
                w.field(alias, &go_string(value));
            }
        }
        let lists = [
            (
                "token_endpoint_auth_methods_supported",
                &self.token_endpoint_auth_methods_supported,
            ),
            ("grant_types_supported", &self.grant_types_supported),
            ("scopes", &self.scopes),
        ];
        for (alias, value) in lists {
            if let Some(value) = value {
                w.field(alias, &go_string_list(value));
            }
        }
        w.finish(&self.extra, SCHEME_FIELDS, false)
    }
}

impl PartialEq for Scheme {
    fn eq(&self, other: &Self) -> bool {
        self.r#type == other.r#type
            && self.description == other.description
            && self.r#in == other.r#in
            && self.name == other.name
            && self.scheme == other.scheme
            && self.bearer_format == other.bearer_format
            && self.flows == other.flows
            && self.issuer_url == other.issuer_url
            && self.open_id_connect_url == other.open_id_connect_url
            && self.authorization_endpoint == other.authorization_endpoint
            && self.token_endpoint == other.token_endpoint
            && self.userinfo_endpoint == other.userinfo_endpoint
            && self.revocation_endpoint == other.revocation_endpoint
            && self.token_endpoint_auth_methods_supported
                == other.token_endpoint_auth_methods_supported
            && self.grant_types_supported == other.grant_types_supported
            && self.scopes == other.scopes
            && extra_eq(&self.extra, &other.extra)
    }
}

impl Serialize for Scheme {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_go(self, s)
    }
}

impl<'de> Deserialize<'de> for Scheme {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        deserialize_go(d)
    }
}
