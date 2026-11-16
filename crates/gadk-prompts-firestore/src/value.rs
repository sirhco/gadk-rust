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

//! Firestore typed values ↔ prompt fields.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gadk_prompts::{Label, PromptVersion, StoreError, VersionMeta};
use serde_json::{Map, Value, json};

pub(crate) fn s(v: &str) -> Value {
    json!({ "stringValue": v })
}

pub(crate) fn i(v: u32) -> Value {
    json!({ "integerValue": v.to_string() })
}

fn decode(msg: impl std::fmt::Display) -> StoreError {
    StoreError::Decode(msg.to_string())
}

pub(crate) fn get_str(fields: &Map<String, Value>, k: &str) -> Result<String, StoreError> {
    fields
        .get(k)
        .and_then(|v| v.get("stringValue"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| decode(format!("field {k}: want stringValue")))
}

pub(crate) fn get_u32(fields: &Map<String, Value>, k: &str) -> Result<u32, StoreError> {
    let v = fields
        .get(k)
        .and_then(|v| v.get("integerValue"))
        .ok_or_else(|| decode(format!("field {k}: want integerValue")))?;
    let s = v
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string());
    s.parse().map_err(|e| decode(format!("field {k}: {e}")))
}

/// Fields of a prompt document.
pub(crate) fn prompt_fields(name: &str, latest: u32, labels: &BTreeMap<Label, u32>) -> Value {
    let lf: Map<String, Value> = labels
        .iter()
        .map(|(l, v)| (l.as_str().to_owned(), i(*v)))
        .collect();
    json!({
        "name": s(name),
        "latest_version": i(latest),
        "labels": { "mapValue": { "fields": lf } },
    })
}

/// `(latest_version, labels)` of a prompt document's fields.
pub(crate) fn read_prompt(
    fields: &Map<String, Value>,
) -> Result<(u32, BTreeMap<Label, u32>), StoreError> {
    let latest = get_u32(fields, "latest_version")?;
    let mut labels = BTreeMap::new();
    if let Some(lf) = fields
        .get("labels")
        .and_then(|v| v.pointer("/mapValue/fields"))
        .and_then(Value::as_object)
    {
        for (k, _) in lf {
            labels.insert(Label::new(k).map_err(decode)?, get_u32(lf, k)?);
        }
    }
    Ok((latest, labels))
}

pub(crate) fn version_fields(v: &PromptVersion) -> Value {
    let config = match &v.config {
        Some(c) => s(&c.to_string()),
        None => json!({ "nullValue": null }),
    };
    json!({
        "name": s(&v.name),
        "version": i(v.version),
        "template": s(&v.template),
        "config": config,
        "created_at": { "timestampValue": v.meta.created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true) },
        "author": s(&v.meta.author),
        "message": s(&v.meta.message),
        "hash": s(&v.meta.hash),
    })
}

pub(crate) fn read_version(fields: &Map<String, Value>) -> Result<PromptVersion, StoreError> {
    let config = match fields
        .get("config")
        .and_then(|v| v.get("stringValue"))
        .and_then(Value::as_str)
    {
        Some(txt) => Some(serde_json::from_str(txt).map_err(|e| decode(format!("config: {e}")))?),
        None => None,
    };
    let ts = fields
        .get("created_at")
        .and_then(|v| v.get("timestampValue"))
        .and_then(Value::as_str)
        .ok_or_else(|| decode("field created_at: want timestampValue"))?;
    let created_at: DateTime<Utc> = DateTime::parse_from_rfc3339(ts)
        .map_err(|e| decode(format!("created_at: {e}")))?
        .into();
    Ok(PromptVersion {
        name: get_str(fields, "name")?,
        version: get_u32(fields, "version")?,
        template: get_str(fields, "template")?,
        config,
        meta: VersionMeta {
            created_at,
            author: get_str(fields, "author").unwrap_or_default(),
            message: get_str(fields, "message").unwrap_or_default(),
            hash: get_str(fields, "hash")?,
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn version_round_trips_through_fields() {
        let v = PromptVersion {
            name: "a/b".into(),
            version: 3,
            template: "t {x}".into(),
            config: Some(json!({"k": [1, 2]})),
            meta: VersionMeta {
                created_at: "2026-11-15T01:02:03.000004Z".parse().unwrap(),
                author: "me".into(),
                message: "m".into(),
                hash: "h".into(),
            },
        };
        let f = version_fields(&v);
        assert_eq!(read_version(f.as_object().unwrap()).unwrap(), v);
        let mut none = v.clone();
        none.config = None;
        assert_eq!(
            read_version(version_fields(&none).as_object().unwrap()).unwrap(),
            none
        );
    }

    #[test]
    fn prompt_round_trips_through_fields() {
        let mut labels = BTreeMap::new();
        labels.insert(Label::production(), 2);
        let f = prompt_fields("p", 4, &labels);
        assert_eq!(read_prompt(f.as_object().unwrap()).unwrap(), (4, labels));
    }
}
