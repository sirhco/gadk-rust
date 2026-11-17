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

//! The auth config a tool requests a credential with: [`Config`], the key it names its
//! credential by
//! ([`Config::key`]), its secret-free copy ([`Config::without_secrets`]), the arguments of an
//! `adk_request_credential` call ([`ToolArguments`]) and that call's name
//! ([`REQUEST_CREDENTIAL_FUNCTION_CALL_NAME`]).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use super::Extra;
use super::credential::Credential;
use super::model_json::{
    GoJson, GoMarshal, ObjectWriter, decode_object, extra_eq, go_string, serialize_go,
};
use super::schemes::Scheme;

/// The name of the function call ADK sends to the client to ask the end user for a credential
/// (the `adk_request_credential` call). ADK Web and adk-python recognize the
/// request by this exact name.
pub const REQUEST_CREDENTIAL_FUNCTION_CALL_NAME: &str = "adk_request_credential";

/// `Config`'s JSON field names, in struct order.
const CONFIG_ALIASES: &[&str] = &[
    "authScheme",
    "rawAuthCredential",
    "exchangedAuthCredential",
    "credentialKey",
];

/// `ToolArguments`' JSON field names, in struct order.
const TOOL_ARGUMENTS_ALIASES: &[&str] = &["functionCallId", "authConfig"];

/// What a tool needs in order to get a credential: how the resource
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
    /// The key that names this config's credential in a credential service.
    ///
    /// It is `credential_key` when that is set. Otherwise it is the first non-empty string
    /// under `credential_key` or `credentialKey` among the unknown keys of
    /// `raw_auth_credential`, then of `auth_scheme`, which is where adk-python's `AuthConfig`
    /// looks. Otherwise it is derived the way adk-python's `get_credential_key` derives it:
    /// `adk_<type>_<digest>_` followed, when there is a raw credential, by
    /// `<authType>_<digest>`. Each digest covers the JSON of the scheme or the credential
    /// without its own unknown keys. The OAuth2 fields that change during a flow (`auth_uri`,
    /// `state`, `auth_response_uri`, `auth_code`, `access_token`, `refresh_token`,
    /// `expires_at`, `expires_in` and `redirect_uri`) are left out, so the key stays put while
    /// the flow runs.
    ///
    /// Every field of [`Scheme`] counts towards the key, whatever its type, but the scheme's
    /// `extra` does not. A custom scheme's own fields are in `extra`, so two schemes of one
    /// custom type that differ only there get the same key; set `credential_key` to tell them
    /// apart.
    ///
    /// `key` trusts the config it is called on. A config a client sends back in an
    /// `adk_request_credential` response can carry any `credentialKey`, so take only its
    /// `exchanged_auth_credential` and store the credential under the key of the config ADK
    /// sent, as adk-python's `store_auth_response` does.
    ///
    /// Infallible: `extra` holds only valid JSON text, so nothing in it can fail to encode.
    pub fn key(&self) -> String {
        if !self.credential_key.is_empty() {
            return self.credential_key.clone();
        }
        let cred_extra = self.raw_auth_credential.as_ref().map(|c| &c.extra);
        for extra in [cred_extra, Some(&self.auth_scheme.extra)]
            .into_iter()
            .flatten()
        {
            for name in ["credential_key", "credentialKey"] {
                if let Some(k) = extra_string(extra, name)
                    && !k.is_empty()
                {
                    return k;
                }
            }
        }

        let scheme = Scheme {
            extra: Extra::new(),
            ..self.auth_scheme.clone()
        };
        let mut key = format!("adk_{}_{}_", scheme.r#type.as_str(), digest(&scheme));
        let Some(raw) = &self.raw_auth_credential else {
            return key;
        };
        let mut cred = Credential {
            extra: Extra::new(),
            ..raw.clone()
        };
        if let Some(o) = cred.oauth2.as_mut() {
            o.auth_uri = String::new();
            o.state = String::new();
            o.auth_response_uri = String::new();
            o.auth_code = String::new();
            o.access_token = String::new();
            o.refresh_token = String::new();
            o.redirect_uri = String::new();
            o.expires_at = 0;
            o.expires_in = 0;
        }
        key.push_str(cred.auth_type.as_str());
        key.push('_');
        key.push_str(&digest(&cred));
        key
    }

    /// A deep copy of the config without the secrets the agent was configured with, for
    /// sending to the client and keeping in the session.
    ///
    /// In both `raw_auth_credential` and `exchanged_auth_credential` it drops the API key, the
    /// OAuth2 client secret, the HTTP password, token and additional headers, and a service
    /// account's private key and key ID. That is adk-python's `_without_configured_secrets`,
    /// which leaves the service account key in. Everything else, the PKCE code verifier
    /// included, is kept. The copy carries this config's [`Config::key`] in `credential_key`,
    /// since its stripped content would derive a different one.
    ///
    /// The copy shares nothing with `self`: `Clone` copies every map, list and extra value.
    /// Infallible, because [`Config::key`] is.
    pub fn without_secrets(&self) -> Config {
        let mut out = self.clone();
        out.credential_key = self.key();
        for cred in [
            out.raw_auth_credential.as_mut(),
            out.exchanged_auth_credential.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            cred.api_key = String::new();
            if let Some(o) = cred.oauth2.as_mut() {
                o.client_secret = String::new();
            }
            if let Some(h) = cred.http.as_mut() {
                h.credentials.password = String::new();
                h.credentials.token = String::new();
                h.additional_headers = None;
            }
            if let Some(sa) = cred.service_account.as_mut()
                && let Some(key) = sa.service_account_credential.as_mut()
            {
                key.private_key = String::new();
                key.private_key_id = String::new();
            }
        }
        out
    }

    /// Decodes a [`Config`] with `authScheme` required.
    pub(crate) fn decode(raw: &RawValue) -> Result<Config, String> {
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

/// Equality of every field, with extras compared as JSON trees.
impl PartialEq for Config {
    fn eq(&self, other: &Self) -> bool {
        self.auth_scheme == other.auth_scheme
            && self.raw_auth_credential == other.raw_auth_credential
            && self.exchanged_auth_credential == other.exchanged_auth_credential
            && self.credential_key == other.credential_key
            && extra_eq(&self.extra, &other.extra)
    }
}

/// Encodes the fields in struct order with `credentialKey` written as
/// [`Config::key`], then the extras that name no field.
impl GoMarshal for Config {
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        w.field("authScheme", &self.auth_scheme.go_marshal());
        if let Some(cred) = &self.raw_auth_credential {
            w.field("rawAuthCredential", &cred.go_marshal());
        }
        if let Some(cred) = &self.exchanged_auth_credential {
            w.field("exchangedAuthCredential", &cred.go_marshal());
        }
        let key = self.key();
        if !key.is_empty() {
            w.field("credentialKey", &go_string(&key));
        }
        w.finish(&self.extra, CONFIG_ALIASES, true)
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_go(self, serializer)
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Config::decode(&raw).map_err(D::Error::custom)
    }
}

/// The arguments of an `adk_request_credential` call: the ID of the tool
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
    /// Decodes [`ToolArguments`] with `functionCallId` and `authConfig` required.
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

/// Equality of every field, with extras compared as JSON trees.
impl PartialEq for ToolArguments {
    fn eq(&self, other: &Self) -> bool {
        self.function_call_id == other.function_call_id
            && self.auth_config == other.auth_config
            && extra_eq(&self.extra, &other.extra)
    }
}

/// Encodes the fields in struct order, then the extras that name no
/// field.
impl GoMarshal for ToolArguments {
    fn go_marshal(&self) -> String {
        let mut w = ObjectWriter::new();
        w.field("functionCallId", &go_string(&self.function_call_id));
        w.field("authConfig", &self.auth_config.go_marshal());
        w.finish(&self.extra, TOOL_ARGUMENTS_ALIASES, true)
    }
}

impl Serialize for ToolArguments {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_go(self, serializer)
    }
}

impl<'de> Deserialize<'de> for ToolArguments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        ToolArguments::decode(&raw).map_err(D::Error::custom)
    }
}

/// The value under `name` in `extra` when it is a JSON string;
/// any other JSON value is ignored.
fn extra_string(extra: &Extra, name: &str) -> Option<String> {
    match GoJson::from_raw(extra.get(name)?) {
        GoJson::String(s) => Some(s),
        _ => None,
    }
}

/// The first 16 hex digits of the SHA-256 of `v`'s JSON in canonical form.
fn digest<T: GoMarshal>(v: &T) -> String {
    digest_text(&v.go_marshal())
}

/// Computes the digest of the encoded text `marshaled`:
/// 1. decode `marshaled` keeping each number's literal text;
/// 2. re-encode it compactly with keys sorted bytewise, `&` `<` `>` and non-ASCII raw, and only
///    U+2028/U+2029 escaped, which is the JSON
///    Python's `json.dumps(sort_keys=True, ensure_ascii=False, separators=(",", ":"))` writes
///    except for those two escapes and for numbers written as they arrived;
/// 3. SHA-256 it (no trailing newline is written);
/// 4. keep the first 8 bytes as lowercase hex.
///
/// `marshaled` is a model's `go_marshal()` output, which always parses. Should it ever not,
/// the text is hashed as is, so the key stays infallible without a panic.
fn digest_text(marshaled: &str) -> String {
    let canonical = match GoJson::parse(marshaled) {
        Ok(tree) => tree.encode(false),
        Err(_) => marshaled.to_owned(),
    };
    let sum = Sha256::digest(canonical.as_bytes());
    hex_prefix(sum.as_slice())
}

/// The first 8 bytes as 16 lowercase hex digits.
fn hex_prefix(sum: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(16);
    for b in sum.iter().take(8) {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expected digest below was computed with Go's own digest body
    // (json.NewDecoder + UseNumber, json.NewEncoder + SetEscapeHTML(false), TrimSuffix "\n",
    // sha256, hex of sum[:8]) on the same input text.

    #[test]
    fn digest_sorts_keys() {
        // Go's Scheme field order in, sorted keys out: the "API key scheme" vector's digest.
        assert_eq!(
            digest_text(r#"{"type":"apiKey","in":"header","name":"X-Api-Key"}"#),
            "12037b6b68d3604d"
        );
        assert_eq!(
            digest_text(r#"{"in":"header","name":"X-Api-Key","type":"apiKey"}"#),
            "12037b6b68d3604d"
        );
    }

    #[test]
    fn digest_undoes_html_escaping_and_writes_non_ascii_raw() {
        // json.Marshal writes < & > as \u003c \u0026 \u003e; the canonical form writes them
        // raw, and é raw: {"a":"<&>","b":"é"}.
        assert_eq!(
            digest_text(r#"{"b":"é","a":"\u003c\u0026\u003e"}"#),
            "67cc612da7899777"
        );
        assert_eq!(digest_text(r#"{"a":"<&>","b":"é"}"#), "67cc612da7899777");
    }

    #[test]
    fn digest_escapes_line_and_paragraph_separators() {
        // A raw U+2028 is hashed as the six characters `\u2028`, as Go's encoder writes it.
        assert_eq!(digest_text("{\"a\":\"\u{2028}\"}"), "952a2834102e2573");
        assert_eq!(digest_text(r#"{"a":"\u2028"}"#), "952a2834102e2573");
    }

    #[test]
    fn digest_keeps_number_text() {
        // 12345678901234567890 is past i64 and loses digits as an f64; 1e3 is not rewritten
        // as 1000 or 1000.0.
        assert_eq!(
            digest_text(r#"{"x":12345678901234567890}"#),
            "80c05588fb76e5cf"
        );
        assert_eq!(digest_text(r#"{"x":1e3}"#), "b4d5049d8c391b49");
    }

    #[test]
    fn hex_prefix_takes_eight_bytes_lowercase() {
        let sum = [0x00, 0x0f, 0xf0, 0xff, 0x12, 0x34, 0xab, 0xcd, 0xee, 0x01];
        assert_eq!(hex_prefix(&sum), "000ff0ff1234abcd");
    }
}
