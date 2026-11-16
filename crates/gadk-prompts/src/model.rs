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

//! Prompt versions, labels, selectors, validation and the content hash.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::store::StoreError;

/// One immutable version of a prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptVersion {
    pub name: String,
    pub version: u32,
    pub template: String,
    #[serde(default)]
    pub config: Option<Value>,
    pub meta: VersionMeta,
}

/// Metadata recorded when a version is created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionMeta {
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub message: String,
    pub hash: String,
}

/// Input to [`PromptStore::create_version`](crate::PromptStore::create_version).
#[derive(Debug, Clone)]
pub struct NewVersion {
    pub name: String,
    pub template: String,
    pub config: Option<Value>,
    pub author: String,
    pub message: String,
}

/// A movable pointer to a version (`production`, `staging`, …). `latest` is reserved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Label(String);

/// Which version of a prompt to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Label(Label),
    Version(u32),
    Latest,
}

/// Checks a prompt name: segments separated by single `/`, each matching
/// `[a-z0-9][a-z0-9._-]*` and not `.`/`..`; at most 128 bytes.
pub fn validate_name(name: &str) -> Result<(), StoreError> {
    let invalid = |why: &str| Err(StoreError::Invalid(format!("prompt name {name:?}: {why}")));
    if name.is_empty() || name.len() > 128 {
        return invalid("must be 1-128 bytes");
    }
    for seg in name.split('/') {
        let mut chars = seg.chars();
        let Some(first) = chars.next() else {
            return invalid("empty path segment");
        };
        if seg == "." || seg == ".." {
            return invalid("'.' and '..' segments are not allowed");
        }
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return invalid("each segment must start with [a-z0-9]");
        }
        if !chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        {
            return invalid("allowed characters are [a-z0-9._-] and '/'");
        }
    }
    Ok(())
}

/// Lower-hex SHA-256 of `template`, one `0x00` byte, and the compact JSON of `config` with
/// object keys sorted recursively (`null` when absent).
pub fn content_hash(template: &str, config: Option<&Value>) -> String {
    let mut h = Sha256::new();
    h.update(template.as_bytes());
    h.update([0u8]);
    let mut canon = String::new();
    write_canonical(config.unwrap_or(&Value::Null), &mut canon);
    h.update(canon.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Compact JSON with object keys sorted (independent of serde_json's `preserve_order`).
fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&m[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

impl Label {
    /// A label: `[a-z0-9][a-z0-9-]{0,62}`, not `latest`.
    pub fn new(s: &str) -> Result<Self, StoreError> {
        let ok = !s.is_empty()
            && s.len() <= 63
            && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok {
            return Err(StoreError::Invalid(format!(
                "label {s:?}: must match [a-z0-9][a-z0-9-]{{0,62}}"
            )));
        }
        if s == "latest" {
            return Err(StoreError::Invalid(
                "label \"latest\" is reserved; use Selector::Latest".into(),
            ));
        }
        Ok(Self(s.to_owned()))
    }
    pub fn production() -> Self {
        Self("production".into())
    }
    pub fn staging() -> Self {
        Self("staging".into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Label {
    type Error = StoreError;
    fn try_from(s: String) -> Result<Self, StoreError> {
        Label::new(&s)
    }
}

impl From<Label> for String {
    fn from(l: Label) -> String {
        l.0
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Selector::Label(l) => f.write_str(l.as_str()),
            Selector::Version(n) => write!(f, "v{n}"),
            Selector::Latest => f.write_str("latest"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validation_names() {
        for ok in ["a", "support-triage", "team/a.b_c", "a/b/c", "x9"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        let long = "a".repeat(129);
        for bad in [
            "",
            "A",
            "-a",
            "a//b",
            "a/./b",
            "a/../b",
            "/a",
            "a/",
            ".",
            "a b",
            long.as_str(),
        ] {
            assert!(
                matches!(validate_name(bad), Err(StoreError::Invalid(_))),
                "{bad:?}"
            );
        }
        assert!(validate_name(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn validation_labels() {
        for ok in ["production", "staging", "a", "canary-2"] {
            assert_eq!(Label::new(ok).unwrap().as_str(), ok);
        }
        let long = "a".repeat(64);
        for bad in ["", "latest", "Prod", "-x", "a_b", "a/b", long.as_str()] {
            assert!(
                matches!(Label::new(bad), Err(StoreError::Invalid(_))),
                "{bad:?}"
            );
        }
        assert!(Label::new(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn hash_is_canonical_sha256() {
        // Independent reference: sha256 over the exact bytes R2 specifies.
        fn reference(bytes: &[u8]) -> String {
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
        assert_eq!(content_hash("hi", None), reference(b"hi\0null"));
        let a = json!({"b": 1, "a": {"y": [1, {"d": 2, "c": 3}], "x": "s"}});
        let mut map = serde_json::Map::new();
        map.insert("a".into(), json!({"x": "s", "y": [1, {"c": 3, "d": 2}]}));
        map.insert("b".into(), json!(1));
        let b = Value::Object(map);
        let mut bytes = b"hi\0".to_vec();
        bytes.extend_from_slice(br#"{"a":{"x":"s","y":[1,{"c":3,"d":2}]},"b":1}"#);
        let want = reference(&bytes);
        assert_eq!(content_hash("hi", Some(&a)), want);
        assert_eq!(content_hash("hi", Some(&b)), want);
        assert_eq!(want.len(), 64);
        assert_ne!(content_hash("hi", Some(&a)), content_hash("hi", None));
    }

    #[test]
    fn version_serde_round_trip() {
        let v = PromptVersion {
            name: "p".into(),
            version: 1,
            template: "t {x}".into(),
            config: Some(json!({"temperature": 0.2})),
            meta: VersionMeta {
                created_at: "2026-11-15T00:00:00Z".parse().unwrap(),
                author: "me".into(),
                message: "m".into(),
                hash: content_hash("t {x}", Some(&json!({"temperature": 0.2}))),
            },
        };
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<PromptVersion>(&s).unwrap(), v);
    }

    #[test]
    fn selector_display() {
        assert_eq!(
            Selector::Label(Label::production()).to_string(),
            "production"
        );
        assert_eq!(Selector::Version(3).to_string(), "v3");
        assert_eq!(Selector::Latest.to_string(), "latest");
    }
}
