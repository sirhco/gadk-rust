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

//! Port of Go `models/redact.go` (the requested-auth-configs half, ADK-Go #2114): an event's
//! `requestedAuthConfigs` reach REST clients without the credential fields that can hold a
//! secret, at any depth. The sign-in half (credential parts and state, #2231) is 8c.

use std::collections::BTreeMap;

use gadk_session::toolauth;
use serde::ser::{Error as _, SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::value::RawValue;

/// Go `credentialSecretKeys`: the JSON keys of credential fields that can hold a secret. Both
/// spellings are listed because `Extra` keeps a key under the name it arrived with.
const CREDENTIAL_SECRET_KEYS: &[&str] = &[
    "password",
    "token",
    "additionalHeaders",
    "additional_headers",
    "clientSecret",
    "client_secret",
    "authResponseUri",
    "auth_response_uri",
    "authCode",
    "auth_code",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "idToken",
    "id_token",
    "codeVerifier",
    "code_verifier",
    "privateKeyId",
    "private_key_id",
    "privateKey",
    "private_key",
    "apiKey",
    "api_key",
];

/// Go `authConfigs.MarshalJSON`: the configs encoded, decoded back into generic JSON with every
/// number kept as written (Go `UseNumber`), [`CREDENTIAL_SECRET_KEYS`] dropped from every
/// object at any depth, and encoded again (object keys sorted, as Go encodes a map).
pub(crate) fn serialize_auth_configs<S: Serializer>(
    configs: &BTreeMap<String, toolauth::Config>,
    s: S,
) -> Result<S::Ok, S::Error> {
    let fail = |e: serde_json::Error| S::Error::custom(format!("requested auth configs: {e}"));
    let raw = serde_json::value::to_raw_value(configs).map_err(fail)?;
    let mut tree = Json::parse(&raw).map_err(fail)?;
    tree.drop_secret_keys();
    tree.serialize(s)
}

/// A generic JSON value whose numbers (and literals) stay as their source text.
enum Json {
    Object(BTreeMap<String, Json>),
    Array(Vec<Json>),
    String(String),
    Other(Box<RawValue>),
}

impl Json {
    fn parse(raw: &RawValue) -> Result<Json, serde_json::Error> {
        let text = raw.get().trim_start();
        Ok(match text.as_bytes().first() {
            Some(b'{') => {
                let members: BTreeMap<String, Box<RawValue>> = serde_json::from_str(text)?;
                Json::Object(
                    members
                        .into_iter()
                        .map(|(k, v)| Ok((k, Json::parse(&v)?)))
                        .collect::<Result<_, serde_json::Error>>()?,
                )
            }
            Some(b'[') => {
                let items: Vec<Box<RawValue>> = serde_json::from_str(text)?;
                Json::Array(
                    items
                        .iter()
                        .map(|v| Json::parse(v))
                        .collect::<Result<_, _>>()?,
                )
            }
            Some(b'"') => Json::String(serde_json::from_str(text)?),
            _ => Json::Other(raw.to_owned()),
        })
    }

    /// Go `dropSecretKeys`: deletes [`CREDENTIAL_SECRET_KEYS`] from every JSON object in `self`.
    fn drop_secret_keys(&mut self) {
        match self {
            Json::Object(members) => {
                members.retain(|k, _| !CREDENTIAL_SECRET_KEYS.contains(&k.as_str()));
                members.values_mut().for_each(Json::drop_secret_keys);
            }
            Json::Array(items) => items.iter_mut().for_each(Json::drop_secret_keys),
            Json::String(_) | Json::Other(_) => {}
        }
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Object(members) => {
                let mut m = s.serialize_map(Some(members.len()))?;
                for (k, v) in members {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
            Json::Array(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for v in items {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Json::String(text) => s.serialize_str(text),
            Json::Other(raw) => raw.serialize(s),
        }
    }
}
