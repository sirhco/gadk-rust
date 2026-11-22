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
use gadk_prompts::{Label, PromptVersion, Rollout, StoreError, VersionMeta};
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

/// `(latest_version, labels, rollouts)` of a prompt document.
pub(crate) type PromptState = (u32, BTreeMap<Label, u32>, BTreeMap<Label, Rollout>);

/// Fields of a prompt document. `rollouts` maps each label to `{version, percent}` and is
/// omitted when there are none, so a prompt without rollouts is written as before.
pub(crate) fn prompt_fields(
    name: &str,
    latest: u32,
    labels: &BTreeMap<Label, u32>,
    rollouts: &BTreeMap<Label, Rollout>,
) -> Value {
    let lf: Map<String, Value> = labels
        .iter()
        .map(|(l, v)| (l.as_str().to_owned(), i(*v)))
        .collect();
    let mut fields = json!({
        "name": s(name),
        "latest_version": i(latest),
        "labels": { "mapValue": { "fields": lf } },
    });
    if !rollouts.is_empty() {
        let rf: Map<String, Value> = rollouts
            .iter()
            .map(|(l, r)| {
                let f = json!({ "version": i(r.version), "percent": i(u32::from(r.percent)) });
                let rollout = json!({ "mapValue": { "fields": f } });
                (l.as_str().to_owned(), rollout)
            })
            .collect();
        fields["rollouts"] = json!({ "mapValue": { "fields": rf } });
    }
    fields
}

/// `(latest_version, labels, rollouts)` of a prompt document's fields. A document without
/// `rollouts` has none; a rollout outside [`Rollout::new`]'s bounds is a `Decode` error.
pub(crate) fn read_prompt(fields: &Map<String, Value>) -> Result<PromptState, StoreError> {
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
    let mut rollouts = BTreeMap::new();
    if let Some(rf) = fields
        .get("rollouts")
        .and_then(|v| v.pointer("/mapValue/fields"))
        .and_then(Value::as_object)
    {
        for (k, v) in rf {
            let label = Label::new(k).map_err(decode)?;
            let f = v
                .pointer("/mapValue/fields")
                .and_then(Value::as_object)
                .ok_or_else(|| decode(format!("rollouts.{k}: want mapValue")))?;
            let version = get_u32(f, "version")?;
            let percent = u8::try_from(get_u32(f, "percent")?)
                .map_err(|e| decode(format!("rollouts.{k}.percent: {e}")))?;
            let rollout =
                Rollout::new(version, percent).map_err(|e| decode(format!("rollouts.{k}: {e}")))?;
            rollouts.insert(label, rollout);
        }
    }
    Ok((latest, labels, rollouts))
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
        let f = prompt_fields("p", 4, &labels, &BTreeMap::new());
        assert_eq!(
            read_prompt(f.as_object().unwrap()).unwrap(),
            (4, labels, BTreeMap::new())
        );
    }

    #[test]
    fn prompt_fields_without_rollouts_has_no_rollouts_key() {
        let mut labels = BTreeMap::new();
        labels.insert(Label::production(), 2);
        let f = prompt_fields("p", 4, &labels, &BTreeMap::new());
        assert!(f.get("rollouts").is_none(), "{f}");
    }

    #[test]
    fn read_prompt_of_a_document_without_rollouts_gives_empty_rollouts() {
        // A prompt document as written before rollouts existed.
        let old = json!({
            "name": {"stringValue": "p"},
            "latest_version": {"integerValue": "4"},
            "labels": {"mapValue": {"fields": {"production": {"integerValue": "2"}}}},
        });
        let (latest, labels, rollouts) = read_prompt(old.as_object().unwrap()).unwrap();
        assert_eq!((latest, labels.get(&Label::production())), (4, Some(&2)));
        assert!(rollouts.is_empty(), "{rollouts:?}");
    }

    #[test]
    fn prompt_with_rollouts_round_trips_through_fields() {
        let (prod, staging) = (Label::production(), Label::staging());
        let labels = BTreeMap::from([(prod.clone(), 2), (staging.clone(), 3)]);
        let rollouts = BTreeMap::from([
            (prod.clone(), Rollout::new(4, 10).unwrap()),
            (staging.clone(), Rollout::new(1, 99).unwrap()),
        ]);
        let f = prompt_fields("p", 4, &labels, &rollouts);
        assert_eq!(
            f["rollouts"]["mapValue"]["fields"]["production"],
            json!({"mapValue": {"fields": {
                "version": {"integerValue": "4"},
                "percent": {"integerValue": "10"},
            }}})
        );
        assert_eq!(
            read_prompt(f.as_object().unwrap()).unwrap(),
            (4, labels, rollouts)
        );
    }

    #[test]
    fn read_prompt_rejects_an_out_of_range_rollout() {
        for percent in ["0", "100", "300"] {
            let doc = json!({
                "name": {"stringValue": "p"},
                "latest_version": {"integerValue": "4"},
                "labels": {"mapValue": {"fields": {"production": {"integerValue": "2"}}}},
                "rollouts": {"mapValue": {"fields": {"production": {"mapValue": {"fields": {
                    "version": {"integerValue": "4"},
                    "percent": {"integerValue": percent},
                }}}}}},
            });
            match read_prompt(doc.as_object().unwrap()) {
                Err(StoreError::Decode(msg)) => assert!(msg.contains("production"), "{msg}"),
                other => panic!("percent {percent}: {other:?}"),
            }
        }
    }
}
