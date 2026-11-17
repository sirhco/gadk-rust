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

//! Differential tests against the Go probe's recording corpus (`tests/fixtures/`, recorded
//! from ADK-Go): Rust record bytes equal Go's, Rust replay decoding equals
//! Go's expected values (or Go's decode error), record → replay round trips, and the type-path
//! fixup table equals the probe's reflection-generated table.
//!
//! The record pipeline here is the record plugin's `saveRecordings` (without the file
//! system): read the existing file with `from_go_node_partial` (errors ignored), drop its
//! recordings of the current user message, append the new ones, `to_go_node` →
//! `clean_yaml_node` → `marshal_node`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use gadk_configurable::conformance::gonode::fixups::{
    Category, FIXUPS, JSONSCHEMA_FIELDS, ROOTS, SHAPES, Zero, shape,
};
use gadk_configurable::conformance::gonode::{
    from_go_node, from_go_node_partial, to_go_node, to_go_node_unmapped,
};
use gadk_configurable::conformance::record::clean::clean_yaml_node;
use gadk_configurable::conformance::recording::{
    LlmRecording, LlmRequestView, RecordedFunctionCall, RecordedFunctionResponse, Recording,
    Recordings, ToolRecording,
};
use gadk_configurable::conformance::replay::normalize::normalize_yaml_node;
use gadk_configurable::govalue::{go_map_to_json, json_map_to_go};
use gadk_goyaml::{Kind, Style};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

/// Every corpus case directory, sorted.
fn cases() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(fixtures().join("recordings"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    out.sort();
    assert_eq!(out.len(), 44, "corpus size");
    out
}

fn from_json<T: serde::de::DeserializeOwned + Default>(v: Option<&Value>) -> T {
    v.map(|v| serde_json::from_value(v.clone()).unwrap())
        .unwrap_or_default()
}

fn opt_map(v: Option<&Value>) -> Option<gadk_configurable::GoMap> {
    v.and_then(Value::as_object).map(json_map_to_go)
}

/// A recording of `input.json` / `replay.json` (FORMAT 1 of the probe).
fn recording_from_json(r: &Value) -> Recording {
    let llm_recording = r.get("llm_recording").map(|l| LlmRecording {
        llm_request: l.get("llm_request").map(|q| {
            let req = LlmRequest {
                model: q["model"].as_str().unwrap_or_default().to_owned(),
                contents: from_json(q.get("contents")),
                config: q
                    .get("config")
                    .map(|c| serde_json::from_value(c.clone()).unwrap()),
                ..Default::default()
            };
            LlmRequestView::from(&req)
        }),
        llm_responses: from_json(l.get("llm_responses")),
    });
    let tool_recording = r.get("tool_recording").map(|t| ToolRecording {
        tool_call: t.get("tool_call").map(|c| RecordedFunctionCall {
            id: from_json(c.get("id")),
            args: opt_map(c.get("args")),
            name: from_json(c.get("name")),
            partial_args: from_json(c.get("partialArgs")),
            will_continue: from_json(c.get("willContinue")),
        }),
        tool_response: t.get("tool_response").map(|c| RecordedFunctionResponse {
            will_continue: from_json(c.get("willContinue")),
            scheduling: from_json(c.get("scheduling")),
            parts: from_json(c.get("parts")),
            id: from_json(c.get("id")),
            name: from_json(c.get("name")),
            response: opt_map(c.get("response")),
        }),
    });
    Recording {
        user_message_index: r["user_message_index"].as_i64().unwrap(),
        agent_name: r["agent_name"].as_str().unwrap().to_owned(),
        llm_recording,
        tool_recording,
        index: 0,
    }
}

fn recordings_from_json(v: &Value) -> Recordings {
    Recordings {
        recordings: v
            .as_array()
            .unwrap()
            .iter()
            .map(recording_from_json)
            .collect(),
    }
}

/// The record file name of a case (`generated-recordings[-sse].yaml`).
fn golden_name(input: &Value) -> &'static str {
    if input["streaming_mode"] == "sse" {
        "generated-recordings-sse.yaml"
    } else {
        "generated-recordings.yaml"
    }
}

/// Go `saveRecordings` without the file system: merge with `existing`, encode, clean, marshal.
fn record(index: i64, existing: Option<&[u8]>, new: &Recordings) -> Vec<u8> {
    let mut filtered = Vec::new();
    if let Some(data) = existing
        && let Ok(doc) = gadk_goyaml::unmarshal_node(data)
        && let Ok((old, _)) = from_go_node_partial(&doc, doc.root())
    {
        filtered.extend(
            old.recordings
                .into_iter()
                .filter(|r| r.user_message_index != index),
        );
    }
    filtered.extend(new.recordings.iter().cloned());
    let mut doc = to_go_node(&Recordings {
        recordings: filtered,
    })
    .unwrap();
    let root = doc.root();
    clean_yaml_node(&mut doc, root, false);
    gadk_goyaml::marshal_node(&doc, doc.root()).unwrap()
}

/// Go `loadInvocationState`'s decode: `yaml.Unmarshal` → `normalizeYAMLNode` → `Decode`.
fn replay(data: &[u8]) -> Result<Recordings, String> {
    let mut doc = gadk_goyaml::unmarshal_node(data).map_err(|e| e.to_string())?;
    let root = doc.root();
    normalize_yaml_node(&mut doc, root);
    from_go_node(&doc, doc.root()).map_err(|e| e.to_string())
}

/// Every JSON number as a float64 (Go's `any` numbers; the replay fixture is Go's
/// `json.Marshal`, which writes `2.0` as `2`).
fn float_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(a) => Value::Array(a.into_iter().map(float_numbers).collect()),
        Value::Object(m) => {
            Value::Object(m.into_iter().map(|(k, v)| (k, float_numbers(v))).collect())
        }
        other => other,
    }
}

/// The comparable form of decoded recordings: gadk serde's JSON of the typed values (lossy
/// exactly where Go's `json.Marshal` of the replay values is: whole-second durations,
/// float32 digits), Go maps as JSON, numbers as float64.
fn comparable(recs: &Recordings) -> Value {
    // Go's `json.Marshal` omits an empty `args`/`response` (`omitempty`), so the fixture
    // cannot tell Go's empty map from its nil map: both compare as null.
    let go_map = |m: &Option<gadk_configurable::GoMap>| {
        m.as_ref()
            .filter(|m| !m.is_empty())
            .map(|m| Value::Object(go_map_to_json(m).unwrap()))
    };
    let out: Vec<Value> = recs
        .recordings
        .iter()
        .map(|r| {
            let mut o = Map::new();
            o.insert("user_message_index".into(), json!(r.user_message_index));
            o.insert("agent_name".into(), json!(r.agent_name));
            o.insert("index".into(), json!(r.index));
            if let Some(l) = &r.llm_recording {
                o.insert(
                    "llm_request".into(),
                    match &l.llm_request {
                        Some(q) => json!({
                            "model": q.model,
                            "contents": q.contents,
                            "config": q.config,
                            "tools": go_map(&Some(q.tools.clone())),
                        }),
                        None => Value::Null,
                    },
                );
                o.insert("llm_responses".into(), json!(l.llm_responses));
            }
            if let Some(t) = &r.tool_recording {
                o.insert(
                    "tool_call".into(),
                    match &t.tool_call {
                        Some(c) => json!({
                            "id": c.id, "args": go_map(&c.args), "name": c.name,
                            "partial_args": c.partial_args, "will_continue": c.will_continue,
                        }),
                        None => Value::Null,
                    },
                );
                o.insert(
                    "tool_response".into(),
                    match &t.tool_response {
                        Some(c) => json!({
                            "will_continue": c.will_continue, "scheduling": c.scheduling,
                            "parts": c.parts, "id": c.id, "name": c.name,
                            "response": go_map(&c.response),
                        }),
                        None => Value::Null,
                    },
                );
            }
            Value::Object(o)
        })
        .collect();
    float_numbers(Value::Array(out))
}

#[test]
fn record_bytes_equal_go() {
    let mut failures = Vec::new();
    for dir in cases() {
        let input = read_json(&dir.join("input.json"));
        let recs = recordings_from_json(&input["recordings"]);
        let existing = input["existing"]
            .as_str()
            .map(|name| fs::read(dir.join(name)).unwrap());
        let index = input["user_message_index"].as_i64().unwrap();
        let got = record(index, existing.as_deref(), &recs);
        let want = fs::read(dir.join(golden_name(&input))).unwrap();
        if got != want {
            failures.push(format!(
                "{}:\n--- go\n{}\n--- rust\n{}",
                dir.display(),
                String::from_utf8_lossy(&want),
                String::from_utf8_lossy(&got)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 44 cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn replay_decode_equals_go() {
    let mut failures = Vec::new();
    for dir in cases() {
        let input = read_json(&dir.join("input.json"));
        let want = read_json(&dir.join("replay.json"));
        let golden = fs::read(dir.join(golden_name(&input))).unwrap();
        let got = replay(&golden);
        let case = dir.file_name().unwrap().to_string_lossy().into_owned();
        match (&want["error"], got) {
            (Value::String(e), Err(g)) if *e == g => {}
            (Value::String(e), other) => {
                failures.push(format!("{case}: want error {e:?}, got {other:?}"))
            }
            (_, Err(g)) => failures.push(format!("{case}: unexpected error {g}")),
            (_, Ok(got)) => {
                let want = comparable(&recordings_from_json(&want["recordings"]));
                let got = comparable(&got);
                if want != got {
                    failures.push(format!(
                        "{case}:\n--- go\n{}\n--- rust\n{}",
                        serde_json::to_string_pretty(&want).unwrap(),
                        serde_json::to_string_pretty(&got).unwrap()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The decoded function declarations' `parametersJsonSchema`/`responseJsonSchema` (Go: a
/// yaml-keyed `map[string]any`, which Go re-encodes as a map; gadk cannot tell it from a
/// live `*jsonschema.Schema` and reshapes it: ledger 6c-2 "decoded JSON schemas re-encode
/// reshaped").
fn strip_json_schemas(recs: &mut Recordings) -> usize {
    let mut n = 0;
    for r in &mut recs.recordings {
        let Some(cfg) = r
            .llm_recording
            .as_mut()
            .and_then(|l| l.llm_request.as_mut())
            .and_then(|q| q.config.as_mut())
        else {
            continue;
        };
        for t in &mut cfg.tools {
            for d in &mut t.function_declarations {
                n += usize::from(d.parameters_json_schema.take().is_some());
                n += usize::from(d.response_json_schema.take().is_some());
            }
        }
    }
    n
}

/// Record → replay → record: decoding what was recorded and recording it again decodes to the
/// same values (replay drops what Go's replay drops, e.g. `thought_signature`, once).
#[test]
fn record_replay_round_trips() {
    let mut decoded = 0;
    let mut schemas = 0;
    for dir in cases() {
        let input = read_json(&dir.join("input.json"));
        let golden = fs::read(dir.join(golden_name(&input))).unwrap();
        let Ok(mut first) = replay(&golden) else {
            continue;
        };
        schemas += strip_json_schemas(&mut first);
        let again = replay(&record(-1, None, &first)).unwrap();
        assert_eq!(first, again, "{}", dir.display());
        decoded += 1;
    }
    assert_eq!((decoded, schemas), (42, 18));
}

/// gadk serde names every field it writes with a Go JSON tag the shapes know (nothing is
/// silently dropped by the Go-shaped encoding).
#[test]
fn every_gadk_field_maps_to_a_go_field() {
    for dir in cases() {
        let input = read_json(&dir.join("input.json"));
        let recs = recordings_from_json(&input["recordings"]);
        let (_, unmapped) = to_go_node_unmapped(&recs).unwrap();
        assert!(unmapped.is_empty(), "{}: {unmapped:?}", dir.display());
    }
}

#[test]
fn replay_type_errors_are_yaml_v3_s() {
    let data = br#"recordings:
  - user_message_index: 0
    agent_name: a
    llm_recording:
      llm_responses:
        - content:
            parts:
              - thoughtsignature: abc
                inlinedata:
                  data: [1, 300, x, 2]
              - thoughtsignature: !!binary AQI=
"#;
    let doc = gadk_goyaml::unmarshal_node(data).unwrap();
    let (recs, terr) = from_go_node_partial(&doc, doc.root()).unwrap();
    assert_eq!(
        terr.unwrap().errors,
        vec![
            "line 8: cannot unmarshal !!str `abc` into []uint8",
            "line 10: cannot unmarshal !!int `300` into uint8",
            "line 10: cannot unmarshal !!str `x` into uint8",
            "line 11: cannot unmarshal !!binary `AQI=` into []uint8",
        ]
    );
    let parts = &recs.recordings[0]
        .llm_recording
        .as_ref()
        .unwrap()
        .llm_responses[0]
        .content
        .as_ref()
        .unwrap()
        .parts;
    assert!(parts[0].thought_signature.is_empty());
    assert_eq!(parts[0].inline_data.as_ref().unwrap().data.as_ref(), [1, 2]);
    assert!(from_go_node(&doc, doc.root()).is_err());
}

// ---------------------------------------------------------------------------------------------
// The fixup/shape tables equal the probe's `fixups.json`.
// ---------------------------------------------------------------------------------------------

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn zero_json(z: Zero) -> Value {
    match z {
        Zero::Struct(r) => json!({"kind": "struct", "ref": r}),
        Zero::Unencodable => json!({"kind": "unencodable"}),
        _ => {
            let (kind, tag, style, value) = z.node().unwrap();
            let mut o = Map::new();
            let kind = match kind {
                Kind::Scalar => "scalar",
                Kind::Sequence => "sequence",
                Kind::Mapping => "mapping",
                _ => "other",
            };
            o.insert("kind".into(), json!(kind));
            o.insert("tag".into(), json!(tag));
            let mut styles = Vec::new();
            if style.contains(Style::DOUBLE_QUOTED) {
                styles.push("double");
            }
            if style.contains(Style::FLOW) {
                styles.push("flow");
            }
            if !styles.is_empty() {
                o.insert("style".into(), json!(styles));
            }
            if kind == "scalar" {
                o.insert("value".into(), json!(value));
            }
            Value::Object(o)
        }
    }
}

#[test]
fn fixup_tables_equal_probe() {
    let fx = read_json(&fixtures().join("fixups.json"));

    let roots: Vec<(&str, &str)> = fx["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (s(&r["name"]), s(&r["go_type"])))
        .collect();
    assert_eq!(roots, ROOTS.to_vec());

    let shapes = fx["shapes"].as_object().unwrap();
    assert_eq!(shapes.len(), SHAPES.len());
    for sh in SHAPES {
        let want = &shapes[sh.go_type];
        assert_eq!(s(&want["go_type"]), sh.go_type);
        let fields = want["fields"].as_array().unwrap();
        assert_eq!(fields.len(), sh.fields.len(), "{}", sh.go_type);
        for (w, f) in fields.iter().zip(sh.fields) {
            let got = json!({
                "key": f.key, "go_name": f.go_name, "json": f.json,
                "category": f.category.as_str(), "go_type": f.go_type, "elem": f.elem,
                "zero": zero_json(f.zero),
            });
            assert_eq!(*w, got, "{}.{}", sh.go_type, f.key);
        }
    }

    let fixups = fx["fixups"].as_array().unwrap();
    assert_eq!(fixups.len(), FIXUPS.len());
    for (w, f) in fixups.iter().zip(FIXUPS) {
        let got = json!({
            "path": f.path, "owner": f.owner, "key": f.key,
            "class": f.class.as_str(), "go_type": f.go_type,
        });
        assert_eq!(*w, got);
    }

    let js = fx["jsonschema_fields"].as_array().unwrap();
    assert_eq!(js.len(), JSONSCHEMA_FIELDS.len());
    for (i, (w, f)) in js.iter().zip(JSONSCHEMA_FIELDS).enumerate() {
        let got = json!({
            "order": i, "go_name": f.go_name, "yaml_key": f.yaml_key, "json_tag": f.json_tag,
            "json_keyword": f.json_keyword, "go_type": f.go_type, "kind": f.kind,
            "class": f.class,
        });
        assert_eq!(*w, got);
    }

    // Every struct position of the walk has a shape, and every fixup names a field of the
    // matching category.
    for p in fx["shape_paths"].as_array().unwrap() {
        assert!(shape(s(&p["type"])).is_some(), "{p}");
    }
    for f in FIXUPS {
        let field = shape(f.owner)
            .unwrap()
            .fields
            .iter()
            .find(|x| x.key == f.key)
            .unwrap();
        let ok = match f.class.as_str() {
            "Bytes" => field.category == Category::Bytes,
            "Duration" => matches!(field.category, Category::Duration | Category::Ptr),
            "Time" => field.category == Category::Time,
            "TextMarshaler" => field.category == Category::TextMarshaler,
            "SchemaTitle" => field.elem == "genai.Schema",
            _ => field.category == Category::Iface,
        };
        assert!(ok, "{}", f.path);
    }
    assert_eq!(
        fx["shape_selfcheck"]["go_numbers"]["pass"], 44,
        "the probe's own shapes rebuild"
    );
}

/// `src/conformance/gonode/shapes.rs` is exactly what [`render_shapes`] makes of the fixture.
/// `GADK_GONODE_REGEN=1 cargo test -p gadk-configurable --test recordings_differential
/// shapes_rs_is_generated` rewrites it after a probe update.
#[test]
fn shapes_rs_is_generated_from_fixture() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/conformance/gonode/shapes.rs");
    let want = render_shapes(&read_json(&fixtures().join("fixups.json")));
    if std::env::var_os("GADK_GONODE_REGEN").is_some() {
        fs::write(&path, &want).unwrap();
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), want);
}

fn zero_rs(z: &Value) -> String {
    let tag = z.get("tag").and_then(Value::as_str).unwrap_or_default();
    let value = z.get("value").and_then(Value::as_str).unwrap_or_default();
    match (s(&z["kind"]), tag, value) {
        ("struct", _, _) => format!("Z::Struct({:?})", s(&z["ref"])),
        ("unencodable", _, _) => "Z::Unencodable".to_owned(),
        ("sequence", _, _) => "Z::EmptySeq".to_owned(),
        ("mapping", _, _) => "Z::EmptyMap".to_owned(),
        (_, "!!null", _) => "Z::Null".to_owned(),
        (_, "!!int", _) => "Z::Int".to_owned(),
        (_, "!!bool", _) => "Z::False".to_owned(),
        (_, "!!timestamp", _) => "Z::Time".to_owned(),
        (_, "!!str", "0s") => "Z::Duration".to_owned(),
        (_, "!!str", "0000-00-00") => "Z::Date".to_owned(),
        (_, "!!str", "") => "Z::EmptyString".to_owned(),
        other => panic!("unknown zero node {other:?}"),
    }
}

fn category_rs(c: &str) -> String {
    let mut out = String::from("C::");
    match c {
        "textmarshaler" => out.push_str("TextMarshaler"),
        other => {
            out.push_str(&other[..1].to_uppercase());
            out.push_str(&other[1..]);
        }
    }
    out
}

/// The generated `shapes.rs` source for a `fixups.json`.
fn render_shapes(fx: &Value) -> String {
    let mut o = String::new();
    o.push_str(
        "// @generated by render_shapes in tests/recordings_differential.rs; do not edit.
// Copyright 2026 Chris Olson
//
// Licensed under the Apache License, Version 2.0 (the \"License\");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an \"AS IS\" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Generated from `tests/fixtures/fixups.json` (recorded from an ADK server's type
//! definitions through yaml.v3's reflection of genai v1.77.0, the ADK `model` types and
//! the JSON-schema types) by
//! `tests/recordings_differential.rs` `render_shapes`; do not edit. Regenerate with
//! `GADK_GONODE_REGEN=1 cargo test -p gadk-configurable --test recordings_differential
//! shapes_rs_is_generated`.
//!
//! - [`ROOTS`]: the walk's roots (name, declared type).
//! - [`SHAPES`]: every struct type reachable from the roots, sorted by name, with its fields
//!   in declaration order (YAML key, field name, JSON tag, category, declared type, struct
//!   element, zero node).
//! - [`FIXUPS`]: the type-path fixup table (path, owner, key, class, declared type).
//! - [`JSONSCHEMA_FIELDS`]: the JSON-schema `Schema` fields in declaration order.

use super::fixups::{
    Category as C, Field, Fixup, FixupClass as X, JsonSchemaField, Shape, Zero as Z,
};

/// The walk's roots: (name, declared type).
#[rustfmt::skip]
pub static ROOTS: &[(&str, &str)] = &[
",
    );
    for r in fx["roots"].as_array().unwrap() {
        o.push_str(&format!(
            "    ({:?}, {:?}),\n",
            s(&r["name"]),
            s(&r["go_type"])
        ));
    }
    o.push_str("];\n\n/// Every struct type's shape, sorted by declared type name.\n#[rustfmt::skip]\npub static SHAPES: &[Shape] = &[\n");
    for (name, sh) in fx["shapes"].as_object().unwrap() {
        o.push_str(&format!("    Shape {{ go_type: {name:?}, fields: &[\n"));
        for f in sh["fields"].as_array().unwrap() {
            o.push_str(&format!(
                "        Field {{ key: {:?}, go_name: {:?}, json: {:?}, category: {}, go_type: {:?}, elem: {:?}, zero: {} }},\n",
                s(&f["key"]),
                s(&f["go_name"]),
                s(&f["json"]),
                category_rs(s(&f["category"])),
                s(&f["go_type"]),
                s(&f["elem"]),
                zero_rs(&f["zero"]),
            ));
        }
        o.push_str("    ] },\n");
    }
    o.push_str("];\n\n/// The type-path fixup table, in depth-first field declaration order.\n#[rustfmt::skip]\npub static FIXUPS: &[Fixup] = &[\n");
    for f in fx["fixups"].as_array().unwrap() {
        o.push_str(&format!(
            "    Fixup {{ path: {:?}, owner: {:?}, key: {:?}, class: X::{}, go_type: {:?} }},\n",
            s(&f["path"]),
            s(&f["owner"]),
            s(&f["key"]),
            s(&f["class"]),
            s(&f["go_type"]),
        ));
    }
    o.push_str("];\n\n/// The JSON-schema `Schema` fields in declaration order.\n#[rustfmt::skip]\npub static JSONSCHEMA_FIELDS: &[JsonSchemaField] = &[\n");
    for f in fx["jsonschema_fields"].as_array().unwrap() {
        o.push_str(&format!(
            "    JsonSchemaField {{ go_name: {:?}, yaml_key: {:?}, json_tag: {:?}, json_keyword: {:?}, go_type: {:?}, kind: {:?}, class: {:?} }},\n",
            s(&f["go_name"]),
            s(&f["yaml_key"]),
            s(&f["json_tag"]),
            s(&f["json_keyword"]),
            s(&f["go_type"]),
            s(&f["kind"]),
            s(&f["class"]),
        ));
    }
    o.push_str("];\n");
    o
}
