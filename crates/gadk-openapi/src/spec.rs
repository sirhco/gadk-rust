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

//! A loaded OpenAPI document ([`OpenApiSpec`]).

use std::fmt;
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;
use url::Url;

use crate::error::Error;

/// How long [`OpenApiSpec::fetch`] waits for the whole download.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest document [`OpenApiSpec::fetch`] accepts, in bytes (10 MiB).
pub(crate) const FETCH_MAX_BYTES: usize = 10 * 1024 * 1024;

/// An OpenAPI 3.0 or 3.1 document, parsed from JSON or YAML.
///
/// Build it with [`str::parse`] (through [`FromStr`]), [`from_file`](OpenApiSpec::from_file) or
/// [`fetch`](OpenApiSpec::fetch), then turn it into tools with `OpenApiToolset::new`.
#[derive(Debug, Clone)]
pub struct OpenApiSpec {
    doc: Value,
    /// Each path with its keys, both in document order.
    order: Vec<(String, Vec<String>)>,
    source_url: Option<Url>,
}

impl FromStr for OpenApiSpec {
    type Err = Error;

    /// Parses `text` as JSON, or as YAML when it is not JSON, and checks the version.
    fn from_str(text: &str) -> Result<Self, Error> {
        Self::parse(text, None)
    }
}

impl OpenApiSpec {
    /// Reads the file at `path` and parses it as [`FromStr`] does. A read failure is
    /// [`Error::Parse`] naming the path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|err| Error::Parse {
            message: format!("cannot read {}: {err}", path.display()),
        })?;
        text.parse()
    }

    /// Downloads the document at `url` (30 s timeout, at most 10 MiB) and parses it as
    /// [`FromStr`] does. A non-2xx status, a transport failure or a larger body is
    /// [`Error::Fetch`]. The URL is kept as the base for a relative server URL.
    pub async fn fetch(url: &str) -> Result<Self, Error> {
        let fetch_error = |message: String| Error::Fetch {
            url: url.to_string(),
            message,
        };
        let parsed = Url::parse(url).map_err(|err| fetch_error(format!("invalid URL: {err}")))?;
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|err| fetch_error(err.to_string()))?;
        let mut response = client
            .get(parsed)
            .send()
            .await
            .map_err(|err| fetch_error(err.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(fetch_error(format!("HTTP status {status}")));
        }
        let final_url = response.url().clone();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| fetch_error(err.to_string()))?
        {
            if body.len() + chunk.len() > FETCH_MAX_BYTES {
                return Err(fetch_error(format!(
                    "the document is larger than {FETCH_MAX_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(body)
            .map_err(|err| fetch_error(format!("the document is not UTF-8: {err}")))?;
        Self::parse(&text, Some(final_url))
    }

    /// The whole document as JSON.
    pub fn document(&self) -> &Value {
        &self.doc
    }

    /// The document's `openapi` version string.
    pub fn version(&self) -> &str {
        self.doc
            .get("openapi")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The URL the document was fetched from, for a document loaded by
    /// [`fetch`](OpenApiSpec::fetch).
    pub fn source_url(&self) -> Option<&str> {
        self.source_url.as_ref().map(Url::as_str)
    }

    /// Parses `text` and records `source_url` as the document's origin.
    pub(crate) fn parse(text: &str, source_url: Option<Url>) -> Result<Self, Error> {
        let (doc, order) = match serde_json::from_str::<Value>(text) {
            Ok(doc) => (doc, serde_json::from_str::<DocOrder>(text).ok()),
            Err(json_err) => match serde_saphyr::from_str::<Value>(text) {
                Ok(doc) => (doc, serde_saphyr::from_str::<DocOrder>(text).ok()),
                Err(yaml_err) => {
                    let message = if text.trim_start().starts_with(['{', '[']) {
                        format!("invalid JSON: {json_err}")
                    } else {
                        format!("invalid YAML: {yaml_err}")
                    };
                    return Err(Error::Parse { message });
                }
            },
        };
        if !doc.is_object() {
            return Err(Error::Parse {
                message: "the document is not a JSON or YAML object".to_string(),
            });
        }
        check_version(&doc)?;
        let order = match order {
            Some(order) => order
                .paths
                .0
                .into_iter()
                .map(|(path, item)| (path, item.0.into_iter().map(|(key, _)| key).collect()))
                .collect(),
            None => sorted_order(&doc),
        };
        Ok(Self {
            doc,
            order,
            source_url,
        })
    }

    /// Whether the document is OpenAPI 3.0 (whose `nullable` is rewritten into `type`).
    pub(crate) fn is_3_0(&self) -> bool {
        self.version().starts_with("3.0.")
    }

    /// Each path with its keys, both in document order.
    pub(crate) fn path_order(&self) -> &[(String, Vec<String>)] {
        &self.order
    }

    /// The URL the document was fetched from.
    pub(crate) fn source(&self) -> Option<&Url> {
        self.source_url.as_ref()
    }
}

/// Accepts only a string `openapi` field starting with `3.0.` or `3.1.`.
fn check_version(doc: &Value) -> Result<(), Error> {
    let found = match doc.get("openapi") {
        Some(Value::String(version))
            if version.starts_with("3.0.") || version.starts_with("3.1.") =>
        {
            return Ok(());
        }
        Some(Value::String(version)) => version.clone(),
        Some(other) => other.to_string(),
        None => match doc.get("swagger") {
            Some(Value::String(version)) => format!("swagger {version}"),
            Some(other) => format!("swagger {other}"),
            None => "no openapi field".to_string(),
        },
    };
    Err(Error::UnsupportedVersion { found })
}

/// The path order when the document's paths cannot be read in order (a malformed path item):
/// the JSON object's own order.
fn sorted_order(doc: &Value) -> Vec<(String, Vec<String>)> {
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };
    paths
        .iter()
        .map(|(path, item)| {
            let keys = item
                .as_object()
                .map(|item| item.keys().cloned().collect())
                .unwrap_or_default();
            (path.clone(), keys)
        })
        .collect()
}

/// The parts of the document whose key order matters: `paths` and each path item's keys.
#[derive(Deserialize, Default)]
struct DocOrder {
    #[serde(default)]
    paths: Keys<Keys<IgnoredAny>>,
}

/// A map's entries in document order.
struct Keys<V>(Vec<(String, V)>);

impl<V> Default for Keys<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Keys<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor<V>(PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for KeysVisitor<V> {
            type Value = Keys<V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys<V>, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, V>()? {
                    entries.push(entry);
                }
                Ok(Keys(entries))
            }
        }

        deserializer.deserialize_map(KeysVisitor(PhantomData))
    }
}

#[cfg(test)]
#[path = "spec_tests.rs"]
pub(crate) mod tests;
