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

//! The auth config a tool requests a credential with (port of ADK-Go
//! `tool/toolauth/auth_tool.go`): [`Config`], the key it names its credential by
//! ([`Config::key`]), its secret-free copy ([`Config::without_secrets`]), the arguments of an
//! `adk_request_credential` call ([`ToolArguments`]) and that call's name
//! ([`REQUEST_CREDENTIAL_FUNCTION_CALL_NAME`]).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use super::Extra;
use super::credential::Credential;
use super::model_json::{decode_object, extra_eq};
use super::schemes::Scheme;

/// The name of the function call ADK sends to the client to ask the end user for a credential
/// (auth_tool.go `RequestCredentialFunctionCallName`). ADK Web and adk-python recognize the
/// request by this exact name.
pub const REQUEST_CREDENTIAL_FUNCTION_CALL_NAME: &str = "adk_request_credential";

/// `Config`'s JSON field names (Go json tags), in Go struct order.
const CONFIG_ALIASES: &[&str] = &[
    "authScheme",
    "rawAuthCredential",
    "exchangedAuthCredential",
    "credentialKey",
];

/// `ToolArguments`' JSON field names (Go json tags), in Go struct order.
const TOOL_ARGUMENTS_ALIASES: &[&str] = &["functionCallId", "authConfig"];

/// What a tool needs in order to get a credential (Go `Config`): how the resource
/// authenticates, the credential the tool was configured with, and the credential the flow
/// produces. ADK sends it to the client in an `adk_request_credential` call, and the client
/// fills `exchanged_auth_credential` in and sends it back.
///
/// It reads and writes adk-python's `AuthConfig` JSON: each field under its camelCase name,
/// read under that name or its snake_case Python name (camelCase wins when both are present),
/// and unknown keys kept in `extra`. `credentialKey` is always written, as [`Config::key`].
#[derive(Clone, Default)]
pub struct Config {
    /// How the resource authenticates. Required on decode.
    pub auth_scheme: Scheme,
    /// The credential the tool was configured with, such as an OAuth2 client ID and secret. A
    /// scheme that needs no exchange, such as an API key, may leave it `None`.
    pub raw_auth_credential: Option<Credential>,
    /// Filled in by ADK and the client together. For OAuth2 and OpenID Connect, ADK adds the
    /// authorization URI and state, and the client adds the auth response.
    pub exchanged_auth_credential: Option<Credential>,
    /// Names the credential in a credential service. Serialized as [`Config::key`].
    pub credential_key: String,
    /// JSON keys that match no field, each as the raw text it arrived as.
    pub extra: Extra,
}

impl Config {
    /// Go `Config.Key`: infallible (spec R12).
    #[allow(unused_variables, clippy::todo)]
    pub fn key(&self) -> String {
        todo!("Task 7")
    }

    /// Go `Config.WithoutSecrets`: a deep copy with secrets dropped and `credential_key` set.
    #[allow(unused_variables, clippy::todo)]
    pub fn without_secrets(&self) -> Config {
        todo!("Task 7")
    }

    /// Go `Config.UnmarshalJSON`: `decodeModel(data, …, "Config", "authScheme")`.
    fn decode(raw: &RawValue) -> Result<Config, String> {
        let mut d = decode_object(raw, "Config", CONFIG_ALIASES, true, &["authScheme"])?;
        let auth_scheme = d
            .take::<Scheme>("authScheme", "Config", "toolauth.Scheme")?
            .unwrap_or_default();
        let raw_auth_credential = d
            .take::<Option<Credential>>("rawAuthCredential", "Config", "toolauth.Credential")?
            .flatten();
        let exchanged_auth_credential = d
            .take::<Option<Credential>>("exchangedAuthCredential", "Config", "toolauth.Credential")?
            .flatten();
        let credential_key = d
            .take::<Option<String>>("credentialKey", "Config", "string")?
            .flatten()
            .unwrap_or_default();
        Ok(Config {
            auth_scheme,
            raw_auth_credential,
            exchanged_auth_credential,
            credential_key,
            extra: d.extra,
        })
    }
}

/// Go `cmp.Equal`: every field, with extras compared as JSON trees.
impl PartialEq for Config {
    fn eq(&self, other: &Self) -> bool {
        self.auth_scheme == other.auth_scheme
            && self.raw_auth_credential == other.raw_auth_credential
            && self.exchanged_auth_credential == other.exchanged_auth_credential
            && self.credential_key == other.credential_key
            && extra_eq(&self.extra, &other.extra)
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Config::decode(&raw).map_err(D::Error::custom)
    }
}

/// The arguments of an `adk_request_credential` call (Go `ToolArguments`): the ID of the tool
/// call that needs the credential, and its config.
#[derive(Clone, Default)]
pub struct ToolArguments {
    /// The ID of the function call that needs the credential. Required on decode.
    pub function_call_id: String,
    /// The config the credential is requested with. Required on decode.
    pub auth_config: Config,
    /// JSON keys that match no field, each as the raw text it arrived as.
    pub extra: Extra,
}

impl ToolArguments {
    /// Go `ToolArguments.UnmarshalJSON`:
    /// `decodeModel(data, …, "ToolArguments", "functionCallId", "authConfig")`.
    fn decode(raw: &RawValue) -> Result<ToolArguments, String> {
        let mut d = decode_object(
            raw,
            "ToolArguments",
            TOOL_ARGUMENTS_ALIASES,
            true,
            &["functionCallId", "authConfig"],
        )?;
        let function_call_id = d
            .take::<String>("functionCallId", "ToolArguments", "string")?
            .unwrap_or_default();
        let auth_config = d
            .take::<Config>("authConfig", "ToolArguments", "toolauth.Config")?
            .unwrap_or_default();
        Ok(ToolArguments {
            function_call_id,
            auth_config,
            extra: d.extra,
        })
    }
}

/// Go `cmp.Equal`: every field, with extras compared as JSON trees.
impl PartialEq for ToolArguments {
    fn eq(&self, other: &Self) -> bool {
        self.function_call_id == other.function_call_id
            && self.auth_config == other.auth_config
            && extra_eq(&self.extra, &other.extra)
    }
}

impl<'de> Deserialize<'de> for ToolArguments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        ToolArguments::decode(&raw).map_err(D::Error::custom)
    }
}
