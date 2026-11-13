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

//! Rust mirrors of the probe's serde shapes (`scripts/gen-go-goyaml/shapes.go`), their
//! [`Options`] (the Go `yaml` tags), and the value Go holds for every serde golden case
//! ([`ShapeValue::expected`]).
//!
//! Go → Rust: a field tagged `yaml:"<key>"` is a serde field named `<key>` in a struct listed
//! in [`options`]' `verbatim_structs` (every shape but `GoDefaults`, whose untagged fields take
//! Go's default key, the lowercased Go field name = the serde name lowercased without `_`);
//! `,omitempty` is `skip_serializing_if`; `yaml:"-"` is `#[serde(skip)]`; `*T` is
//! `Option<T>`; `[]byte` is [`Bytes`] (`serialize_bytes`, Go's `!!int` sequence); slices are
//! `Vec`, `map[string]T` is `BTreeMap<String, T>` (Go maps have no order; encode sorts);
//! `interface{}` is [`GoValue`]; `yaml.Node` / `[]yaml.Node` are [`Document`] /
//! `Vec<Document>`; the `,inline` `map[string]interface{}` is an [`InlineMap`] under
//! [`INLINE_FIELD`]. Every struct carries `#[serde(default)]` (Go zero-fills absent keys) and
//! Go's type name (Go prints `main.<Name>` in type errors).

use std::collections::BTreeMap;
use std::fmt;

use chrono::DateTime;
use gadk_goyaml::{Document, Error, GoValue, InlineMap, NodeId, Options, TypeError};
use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use super::golden::{document_to_dump, dump_to_document, node_dump_from_json, normalize_go_value};

/// The serde shapes, in the probe's order.
pub const SHAPES: [&str; 8] = [
    "scalars",
    "nested",
    "optionals",
    "collections",
    "inline",
    "node",
    "godefaults",
    "recordings",
];

/// The Go structs whose fields all carry `yaml:"<key>"` tags equal to their serde names.
pub const VERBATIM_STRUCTS: [&str; 11] = [
    "Scalars",
    "Inner",
    "Nested",
    "Optionals",
    "Collections",
    "WithInline",
    "WithNode",
    "Recordings",
    "Recording",
    "LLMRecording",
    "ToolRecording",
];

/// The [`Options`] that reproduce the shapes' Go `yaml` tags: every tagged struct verbatim;
/// `GoDefaults` (untagged) keeps Go's default keys; no per-field overrides are needed (every
/// tag equals its serde field name).
pub fn options() -> Options {
    let mut opts = Options::default();
    opts.verbatim_structs.extend(VERBATIM_STRUCTS);
    opts
}

/// Go's `[]byte`: serialized with `serialize_bytes` (yaml.v3 writes a `!!int` sequence),
/// deserialized through `deserialize_byte_buf` from bytes (`!!binary`), a string (Go's
/// `[]byte(s)`), a sequence of `u8`, or null (Go's nil slice).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

struct BytesVisitor;

impl<'de> Visitor<'de> for BytesVisitor {
    type Value = Bytes;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a byte array")
    }

    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Bytes, E> {
        Ok(Bytes(v.to_vec()))
    }

    fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Bytes, E> {
        Ok(Bytes(v))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Bytes, E> {
        Ok(Bytes(v.as_bytes().to_vec()))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Bytes, E> {
        Ok(Bytes::default())
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Bytes, E> {
        Ok(Bytes::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Bytes, A::Error> {
        let mut out = Vec::new();
        while let Some(b) = seq.next_element::<u8>()? {
            out.push(b);
        }
        Ok(Bytes(out))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_byte_buf(BytesVisitor)
    }
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// Go `Scalars`: every scalar kind; all keys tagged (verbatim).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scalars {
    pub b: bool,
    pub i8: i8,
    pub i16: i16,
    pub i32: i32,
    pub i64: i64,
    pub u8: u8,
    pub u16: u16,
    pub u32: u32,
    pub u64: u64,
    pub f32: f32,
    pub f64: f64,
    pub s: String,
}

/// Go `Inner`: the nested struct of `Nested` and `Optionals` (`b` is `omitempty`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inner {
    pub a: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub b: String,
}

/// Go `Nested`: a struct value, a struct pointer (`omitempty`) and a slice of structs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Nested {
    pub name: String,
    pub inner: Inner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_ptr: Option<Inner>,
    pub list: Vec<Inner>,
}

/// Go `Optionals`: pointers with (`i`, `b`) and without `omitempty`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Optionals {
    pub s: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i: Option<i64>,
    pub f: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<bool>,
    pub inner: Option<Inner>,
}

/// Go `Collections`: `[]byte`, slices (`strs` is `omitempty`) and maps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collections {
    pub bytes: Bytes,
    pub ints: Vec<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub strs: Vec<String>,
    pub counts: BTreeMap<String, i64>,
    pub any: BTreeMap<String, GoValue>,
    pub items: Vec<GoValue>,
}

/// Go `WithInline`: a `,inline` `map[string]interface{}` catch-all (`count` is `omitempty`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WithInline {
    pub name: String,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub count: i64,
    #[serde(rename = "$gadk_goyaml::inline")]
    pub extra: InlineMap,
}

/// Go `WithNode`: `yaml.Node` and `[]yaml.Node` fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WithNode {
    pub kind: String,
    pub spec: Document,
    pub edges: Vec<Document>,
}

/// Go `GoDefaults`: untagged fields (Go's key is the field name lowercased:
/// `usermessageindex`, `agentname`, `httptimeout`, `enabled`); `Index` is `yaml:"-"`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoDefaults {
    pub user_message_index: i64,
    pub agent_name: String,
    pub http_timeout: f64,
    pub enabled: bool,
    #[serde(skip)]
    pub index: i64,
}

/// Go `Recordings` (ADK-Go's replayplugin recording types, payloads as generic maps).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recordings {
    pub recordings: Vec<Recording>,
}

/// Go `Recording`: one recordings entry (`llm_recording` / `tool_recording` are `omitempty`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recording {
    pub user_message_index: i64,
    pub agent_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_recording: Option<LLMRecording>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_recording: Option<ToolRecording>,
}

/// Go `LLMRecording`: a request and its responses (both `omitempty`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LLMRecording {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub llm_request: BTreeMap<String, GoValue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub llm_responses: Vec<BTreeMap<String, GoValue>>,
}

/// Go `ToolRecording`: a tool call and its response (both `omitempty`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolRecording {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_call: BTreeMap<String, GoValue>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_response: BTreeMap<String, GoValue>,
}

/// A value of one of the shapes (dispatch by the golden's `shape`).
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeValue {
    Scalars(Scalars),
    Nested(Nested),
    Optionals(Optionals),
    Collections(Collections),
    Inline(WithInline),
    Node(WithNode),
    GoDefaults(GoDefaults),
    Recordings(Recordings),
}

/// Runs `$body` with `$v` bound to the shape struct inside a [`ShapeValue`].
macro_rules! with_shape {
    ($value:expr, $v:ident => $body:expr) => {
        match $value {
            ShapeValue::Scalars($v) => $body,
            ShapeValue::Nested($v) => $body,
            ShapeValue::Optionals($v) => $body,
            ShapeValue::Collections($v) => $body,
            ShapeValue::Inline($v) => $body,
            ShapeValue::Node($v) => $body,
            ShapeValue::GoDefaults($v) => $body,
            ShapeValue::Recordings($v) => $body,
        }
    };
}

/// Runs `$body` with `$T` as the shape struct type of the shape name `$shape`, wrapping the
/// result's value through `$wrap` (a `ShapeValue` variant constructor).
macro_rules! for_shape_type {
    ($shape:expr, |$T:ident, $wrap:ident| $body:expr) => {
        match $shape {
            "scalars" => {
                type $T = Scalars;
                let $wrap = ShapeValue::Scalars;
                $body
            }
            "nested" => {
                type $T = Nested;
                let $wrap = ShapeValue::Nested;
                $body
            }
            "optionals" => {
                type $T = Optionals;
                let $wrap = ShapeValue::Optionals;
                $body
            }
            "collections" => {
                type $T = Collections;
                let $wrap = ShapeValue::Collections;
                $body
            }
            "inline" => {
                type $T = WithInline;
                let $wrap = ShapeValue::Inline;
                $body
            }
            "node" => {
                type $T = WithNode;
                let $wrap = ShapeValue::Node;
                $body
            }
            "godefaults" => {
                type $T = GoDefaults;
                let $wrap = ShapeValue::GoDefaults;
                $body
            }
            "recordings" => {
                type $T = Recordings;
                let $wrap = ShapeValue::Recordings;
                $body
            }
            other => panic!("unknown serde shape {other:?}"),
        }
    };
}

impl Serialize for ShapeValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        with_shape!(self, v => v.serialize(serializer))
    }
}

impl ShapeValue {
    /// The shape's name (`scalars`, …).
    pub fn shape(&self) -> &'static str {
        match self {
            ShapeValue::Scalars(_) => "scalars",
            ShapeValue::Nested(_) => "nested",
            ShapeValue::Optionals(_) => "optionals",
            ShapeValue::Collections(_) => "collections",
            ShapeValue::Inline(_) => "inline",
            ShapeValue::Node(_) => "node",
            ShapeValue::GoDefaults(_) => "godefaults",
            ShapeValue::Recordings(_) => "recordings",
        }
    }

    /// The zero value of `shape` (Go's `new(Shape)`; the golden case `zero`).
    pub fn zero(shape: &str) -> ShapeValue {
        for_shape_type!(shape, |T, wrap| wrap(T::default()))
    }

    /// `gadk_goyaml::unmarshal` of `input` into `shape` (Go's `yaml.Unmarshal(input, v)`).
    pub fn unmarshal(shape: &str, input: &[u8], opts: &Options) -> Result<ShapeValue, Error> {
        for_shape_type!(shape, |T, wrap| gadk_goyaml::unmarshal::<T>(input, opts)
            .map(wrap))
    }

    /// `gadk_goyaml::from_node_partial` of node `at` into `shape`: Go's partly filled value
    /// next to its `*TypeError`.
    pub fn from_node_partial(
        shape: &str,
        doc: &Document,
        at: NodeId,
        opts: &Options,
    ) -> Result<(ShapeValue, Option<TypeError>), Error> {
        for_shape_type!(shape, |T, wrap| {
            gadk_goyaml::from_node_partial::<T>(doc, at, opts).map(|(v, e)| (wrap(v), e))
        })
    }

    /// `gadk_goyaml::marshal` of the value (Go's `yaml.Marshal(v)`).
    pub fn marshal(&self, opts: &Options) -> Result<Vec<u8>, Error> {
        gadk_goyaml::marshal(self, opts)
    }

    /// `gadk_goyaml::to_node` of the value (Go's `node.Encode(v)`).
    pub fn to_node(&self, opts: &Options) -> Result<Document, Error> {
        gadk_goyaml::to_node(self, opts)
    }

    /// A comparable rendering: `Debug` of the value with every `GoValue`/`InlineMap` map
    /// sorted as Go's maps are unordered, and every `Document` field replaced by its node
    /// dump (arena layout is not identity). NaN renders as `NaN`, so NaN fields compare equal.
    pub fn canonical(&self) -> String {
        match self {
            ShapeValue::Collections(v) => {
                let mut v = v.clone();
                v.any = v
                    .any
                    .iter()
                    .map(|(k, x)| (k.clone(), normalize_go_value(x)))
                    .collect();
                v.items = v.items.iter().map(normalize_go_value).collect();
                format!("{v:?}")
            }
            ShapeValue::Inline(v) => {
                let mut v = v.clone();
                v.extra.0.sort_by(|a, b| a.0.cmp(&b.0));
                for (_, x) in &mut v.extra.0 {
                    *x = normalize_go_value(x);
                }
                format!("{v:?}")
            }
            ShapeValue::Node(v) => {
                let dump = |d: &Document| format!("{:?}", document_to_dump(d, d.root()));
                format!(
                    "WithNode {{ kind: {:?}, spec: {}, edges: [{}] }}",
                    v.kind,
                    dump(&v.spec),
                    v.edges.iter().map(dump).collect::<Vec<_>>().join(", ")
                )
            }
            ShapeValue::Recordings(v) => {
                let mut v = v.clone();
                let norm = |m: &mut BTreeMap<String, GoValue>| {
                    for x in m.values_mut() {
                        *x = normalize_go_value(x);
                    }
                };
                for r in &mut v.recordings {
                    if let Some(l) = &mut r.llm_recording {
                        norm(&mut l.llm_request);
                        l.llm_responses.iter_mut().for_each(norm);
                    }
                    if let Some(t) = &mut r.tool_recording {
                        norm(&mut t.tool_call);
                        norm(&mut t.tool_response);
                    }
                }
                format!("{v:?}")
            }
            other => with_shape!(other, v => format!("{v:?}")),
        }
    }

    /// Asserts that `self` and `want` are the same value ([`ShapeValue::canonical`]); the
    /// panic names `context` and both renderings.
    #[track_caller]
    pub fn assert_same(&self, want: &ShapeValue, context: &str) {
        let (got, want) = (self.canonical(), want.canonical());
        if got != want {
            panic!("{context}: shape value mismatch\n got: {got}\nwant: {want}");
        }
    }

    /// The value Go's shape holds for serde golden `shape`-`case`: the zero value for `zero`,
    /// else what `yaml.Unmarshal(input, v)` left in a new zero value (also when it returned an
    /// error). Printed from the Go values by reflection (`expected_<shape>` below).
    pub fn expected(shape: &str, case: &str) -> ShapeValue {
        match shape {
            "scalars" => ShapeValue::Scalars(expected_scalars(case)),
            "nested" => ShapeValue::Nested(expected_nested(case)),
            "optionals" => ShapeValue::Optionals(expected_optionals(case)),
            "collections" => ShapeValue::Collections(expected_collections(case)),
            "inline" => ShapeValue::Inline(expected_inline(case)),
            "node" => ShapeValue::Node(expected_node(case)),
            "godefaults" => ShapeValue::GoDefaults(expected_godefaults(case)),
            "recordings" => ShapeValue::Recordings(expected_recordings(case)),
            other => panic!("unknown serde shape {other:?}"),
        }
    }
}

/// A `time.Time` from its RFC 3339 text.
fn ts(s: &str) -> GoValue {
    GoValue::Timestamp(DateTime::parse_from_rfc3339(s).unwrap_or_else(|e| panic!("{s:?}: {e}")))
}

/// A `yaml.Node` field from its node dump JSON (the node as Go decoded it, with its input's
/// lines and columns).
fn node(json: &str) -> Document {
    dump_to_document(&node_dump_from_json(json))
}

/// Go's `Scalars` value for each `scalars` case.
fn expected_scalars(case: &str) -> Scalars {
    match case {
        "zero" => Scalars::default(),
        "full" => Scalars {
            b: true,
            i8: -128,
            i16: 32767,
            i32: -2147483648,
            i64: 9223372036854775807,
            u8: 255,
            u16: 65535,
            u32: 4294967295,
            u64: 18446744073709551615,
            f32: 0.7,
            f64: 0.1,
            s: "hello".to_string(),
        },
        "float-special" => Scalars {
            f32: f32::INFINITY,
            f64: f64::NEG_INFINITY,
            ..Default::default()
        },
        "float-nan" => Scalars {
            f32: f32::NAN,
            f64: f64::NAN,
            ..Default::default()
        },
        "float-format" => Scalars {
            f32: 3.4028235e+38,
            f64: 1e+21,
            ..Default::default()
        },
        "float-small" => Scalars {
            f32: 1e-07,
            f64: 1e-05,
            ..Default::default()
        },
        "float-from-int" => Scalars {
            f32: 1.6777216e+07,
            ..Default::default()
        },
        "int-from-float" => Scalars {
            i32: 200,
            i64: 1,
            u8: 1,
            ..Default::default()
        },
        "overflow" => Scalars::default(),
        "mismatch" => Scalars {
            b: true,
            ..Default::default()
        },
        "string-from-int" => Scalars {
            s: "123".to_string(),
            ..Default::default()
        },
        "string-from-bool" => Scalars {
            s: "true".to_string(),
            ..Default::default()
        },
        "string-from-null" => Scalars::default(),
        "string-from-timestamp" => Scalars {
            s: "2001-12-14T21:59:43Z".to_string(),
            ..Default::default()
        },
        "string-from-binary" => Scalars {
            s: "hello".to_string(),
            ..Default::default()
        },
        "string-needs-quotes" => Scalars {
            s: "123".to_string(),
            ..Default::default()
        },
        "string-multiline" => Scalars {
            s: "a\nb".to_string(),
            ..Default::default()
        },
        "bool-forms" => Scalars {
            b: true,
            ..Default::default()
        },
        "bool-from-int" => Scalars::default(),
        "int-forms" => Scalars {
            i8: -16,
            i16: 5,
            i32: 15,
            i64: 31,
            u32: 1000,
            ..Default::default()
        },
        "unknown-keys" => Scalars {
            b: true,
            ..Default::default()
        },
        "alias" => Scalars {
            i64: 42,
            s: "42".to_string(),
            ..Default::default()
        },
        "merge" => Scalars {
            i8: 1,
            s: "own".to_string(),
            ..Default::default()
        },
        "merge-alias" => Scalars {
            b: true,
            u8: 7,
            s: "d".to_string(),
            ..Default::default()
        },
        "duplicate-key" => Scalars::default(),
        "not-a-map" => Scalars::default(),
        "scalar-root" => Scalars::default(),
        "null-root" => Scalars::default(),
        "empty" => Scalars::default(),
        "syntax-error" => Scalars::default(),
        _ => panic!("no serde golden case scalars-{case}"),
    }
}

/// Go's `Nested` value for each `nested` case.
fn expected_nested(case: &str) -> Nested {
    match case {
        "zero" => Nested::default(),
        "full" => Nested {
            name: "n".to_string(),
            inner: Inner {
                a: 1,
                b: "x".to_string(),
            },
            inner_ptr: Some(Inner {
                a: 2,
                ..Default::default()
            }),
            list: vec![
                Inner {
                    a: 3,
                    ..Default::default()
                },
                Inner {
                    a: 4,
                    b: "y".to_string(),
                },
            ],
        },
        "nulls" => Nested::default(),
        "list-mismatch" => Nested {
            list: vec![Inner::default()],
            ..Default::default()
        },
        "alias-struct" => Nested {
            inner: Inner {
                a: 5,
                b: "z".to_string(),
            },
            inner_ptr: Some(Inner {
                a: 5,
                b: "z".to_string(),
            }),
            list: vec![
                Inner {
                    a: 5,
                    b: "z".to_string(),
                },
                Inner {
                    a: 5,
                    b: "z".to_string(),
                },
            ],
            ..Default::default()
        },
        "merge-into-inner" => Nested {
            inner: Inner {
                a: 9,
                b: "m".to_string(),
            },
            ..Default::default()
        },
        "empty-ptr" => Nested {
            inner_ptr: Some(Inner::default()),
            ..Default::default()
        },
        "inner-not-map" => Nested::default(),
        _ => panic!("no serde golden case nested-{case}"),
    }
}

/// Go's `Optionals` value for each `optionals` case.
fn expected_optionals(case: &str) -> Optionals {
    match case {
        "zero" => Optionals::default(),
        "full" => Optionals {
            s: Some("x".to_string()),
            i: Some(0),
            f: Some(1.5),
            b: Some(false),
            inner: Some(Inner {
                a: 1,
                ..Default::default()
            }),
        },
        "nulls" => Optionals::default(),
        "empty-string" => Optionals {
            s: Some("".to_string()),
            ..Default::default()
        },
        "mismatch" => Optionals {
            i: Some(0),
            f: Some(0.0),
            ..Default::default()
        },
        "alias" => Optionals {
            s: Some("v".to_string()),
            inner: Some(Inner {
                b: "v".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
        _ => panic!("no serde golden case optionals-{case}"),
    }
}

/// Go's `Collections` value for each `collections` case.
fn expected_collections(case: &str) -> Collections {
    match case {
        "zero" => Collections::default(),
        "full" => Collections {
            bytes: Bytes(vec![1, 2, 255]),
            ints: vec![1, -2],
            strs: vec!["a".to_string(), "b".to_string()],
            counts: BTreeMap::from([("a".to_string(), 1), ("b".to_string(), 2)]),
            any: BTreeMap::from([
                ("k".to_string(), GoValue::String("v".to_string())),
                (
                    "l".to_string(),
                    GoValue::Seq(vec![GoValue::Int(1), GoValue::String("x".to_string())]),
                ),
                (
                    "m".to_string(),
                    GoValue::Map(vec![
                        ("a".to_string(), GoValue::Int(2)),
                        ("z".to_string(), GoValue::Int(1)),
                    ]),
                ),
                ("n".to_string(), GoValue::Int(1)),
            ]),
            items: vec![
                GoValue::Int(1),
                GoValue::Float(1.5),
                GoValue::String("x".to_string()),
                GoValue::Bool(true),
                GoValue::Null,
                ts("2001-12-14T00:00:00Z"),
            ],
        },
        "binary" => Collections::default(),
        "bytes-from-string" => Collections::default(),
        "bytes-overflow" => Collections {
            bytes: Bytes(vec![]),
            ..Default::default()
        },
        "mismatch" => Collections {
            ints: vec![1],
            counts: BTreeMap::from([("b".to_string(), 2)]),
            ..Default::default()
        },
        "nonstring-keys" => Collections {
            counts: BTreeMap::from([("1".to_string(), 1)]),
            any: BTreeMap::from([
                ("1".to_string(), GoValue::String("a".to_string())),
                ("true".to_string(), GoValue::String("b".to_string())),
            ]),
            ..Default::default()
        },
        "sorted-keys" => Collections {
            counts: BTreeMap::from([
                ("1".to_string(), 6),
                ("10".to_string(), 7),
                ("9".to_string(), 8),
                ("B".to_string(), 5),
                ("a".to_string(), 2),
                ("a-b".to_string(), 9),
                ("a10".to_string(), 3),
                ("a2".to_string(), 4),
                ("ab".to_string(), 10),
                ("z".to_string(), 1),
            ]),
            ..Default::default()
        },
        "empty-strs" => Collections {
            ints: vec![],
            strs: vec![],
            ..Default::default()
        },
        "nested-items" => Collections {
            items: vec![
                GoValue::Seq(vec![GoValue::Int(1), GoValue::Int(2)]),
                GoValue::Map(vec![("a".to_string(), GoValue::String("b".to_string()))]),
                GoValue::MapAny(vec![(GoValue::Int(1), GoValue::String("x".to_string()))]),
                GoValue::Map(vec![]),
            ],
            ..Default::default()
        },
        "null-maps" => Collections::default(),
        _ => panic!("no serde golden case collections-{case}"),
    }
}

/// Go's `WithInline` value for each `inline` case.
fn expected_inline(case: &str) -> WithInline {
    match case {
        "zero" => WithInline::default(),
        "full" => WithInline {
            name: "n".to_string(),
            count: 2,
            extra: InlineMap(vec![
                (
                    "alpha".to_string(),
                    GoValue::Seq(vec![GoValue::String("x".to_string())]),
                ),
                (
                    "beta".to_string(),
                    GoValue::Map(vec![("k".to_string(), GoValue::String("v".to_string()))]),
                ),
                ("zeta".to_string(), GoValue::Int(1)),
            ]),
        },
        "only-extras" => WithInline {
            extra: InlineMap(vec![("x".to_string(), GoValue::Int(1))]),
            ..Default::default()
        },
        "count-zero" => WithInline {
            name: "n".to_string(),
            ..Default::default()
        },
        "merge-extras" => WithInline {
            name: "merged".to_string(),
            extra: InlineMap(vec![
                ("extra1".to_string(), GoValue::Int(1)),
                ("extra2".to_string(), GoValue::Int(2)),
            ]),
            ..Default::default()
        },
        "alias-extras" => WithInline {
            name: "q".to_string(),
            extra: InlineMap(vec![
                (
                    "a".to_string(),
                    GoValue::Map(vec![("k".to_string(), GoValue::String("v".to_string()))]),
                ),
                (
                    "b".to_string(),
                    GoValue::Map(vec![("k".to_string(), GoValue::String("v".to_string()))]),
                ),
            ]),
            ..Default::default()
        },
        "mismatch" => WithInline {
            extra: InlineMap(vec![("extra".to_string(), GoValue::Int(1))]),
            ..Default::default()
        },
        "nonstring-keys" => WithInline {
            name: "n".to_string(),
            extra: InlineMap(vec![
                ("1".to_string(), GoValue::String("one".to_string())),
                ("true".to_string(), GoValue::String("t".to_string())),
            ]),
            ..Default::default()
        },
        _ => panic!("no serde golden case inline-{case}"),
    }
}

/// Go's `WithNode` value for each `node` case.
fn expected_node(case: &str) -> WithNode {
    match case {
        "zero" => WithNode::default(),
        "full" => WithNode {
            kind: "k".to_string(),
            spec: node(
                r##"{"kind":"mapping","style":[],"tag":"!!map","value":"","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":3,"column":3,"content":[{"kind":"scalar","style":[],"tag":"!!str","value":"a","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":3,"column":3,"content":[]},{"kind":"scalar","style":[],"tag":"!!int","value":"1","anchor":"","alias":null,"head":"","line_comment":"# comment","foot":"","line":3,"column":6,"content":[]},{"kind":"scalar","style":[],"tag":"!!str","value":"b","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":4,"column":3,"content":[]},{"kind":"sequence","style":["flow"],"tag":"!!seq","value":"","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":4,"column":6,"content":[{"kind":"scalar","style":[],"tag":"!!str","value":"x","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":4,"column":7,"content":[]},{"kind":"scalar","style":[],"tag":"!!str","value":"y","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":4,"column":10,"content":[]}]}]}"##,
            ),
            edges: vec![
                node(
                    r##"{"kind":"mapping","style":["flow"],"tag":"!!map","value":"","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":6,"column":5,"content":[{"kind":"scalar","style":[],"tag":"!!str","value":"from","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":6,"column":6,"content":[]},{"kind":"scalar","style":[],"tag":"!!str","value":"a","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":6,"column":12,"content":[]},{"kind":"scalar","style":[],"tag":"!!str","value":"to","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":6,"column":15,"content":[]},{"kind":"scalar","style":[],"tag":"!!str","value":"b","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":6,"column":19,"content":[]}]}"##,
                ),
                node(
                    r##"{"kind":"scalar","style":[],"tag":"!!str","value":"scalar","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":7,"column":5,"content":[]}"##,
                ),
            ],
        },
        "spec-scalar" => WithNode {
            kind: "k".to_string(),
            spec: node(
                r##"{"kind":"scalar","style":["single"],"tag":"!!str","value":"quoted","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":7,"content":[]}"##,
            ),
            ..Default::default()
        },
        "anchor-within" => WithNode {
            spec: node(
                r##"{"kind":"mapping","style":[],"tag":"!!map","value":"","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":3,"content":[{"kind":"scalar","style":[],"tag":"!!str","value":"base","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":3,"content":[]},{"kind":"mapping","style":["flow"],"tag":"!!map","value":"","anchor":"b","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":9,"content":[{"kind":"scalar","style":[],"tag":"!!str","value":"x","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":13,"content":[]},{"kind":"scalar","style":[],"tag":"!!int","value":"1","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":16,"content":[]}]},{"kind":"scalar","style":[],"tag":"!!str","value":"use","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":3,"column":3,"content":[]},{"kind":"alias","style":[],"tag":"","value":"b","anchor":"","alias":[1],"head":"","line_comment":"","foot":"","line":3,"column":8,"content":[]}]}"##,
            ),
            ..Default::default()
        },
        "comments" => WithNode {
            kind: "k".to_string(),
            spec: node(
                r##"{"kind":"mapping","style":[],"tag":"!!map","value":"","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":5,"column":3,"content":[{"kind":"scalar","style":[],"tag":"!!str","value":"a","anchor":"","alias":null,"head":"# inner head","line_comment":"","foot":"# inner foot","line":5,"column":3,"content":[]},{"kind":"scalar","style":[],"tag":"!!int","value":"1","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":5,"column":6,"content":[]}]}"##,
            ),
            edges: vec![],
        },
        "edges-not-seq" => WithNode::default(),
        _ => panic!("no serde golden case node-{case}"),
    }
}

/// Go's `GoDefaults` value for each `godefaults` case.
fn expected_godefaults(case: &str) -> GoDefaults {
    match case {
        "zero" => GoDefaults::default(),
        "full" => GoDefaults {
            user_message_index: 3,
            agent_name: "root".to_string(),
            http_timeout: 2.5,
            enabled: true,
            ..Default::default()
        },
        "snake-keys" => GoDefaults::default(),
        "field-name-keys" => GoDefaults::default(),
        _ => panic!("no serde golden case godefaults-{case}"),
    }
}

/// Go's `Recordings` value for each `recordings` case.
fn expected_recordings(case: &str) -> Recordings {
    match case {
        "zero" => Recordings::default(),
        "llm" => Recordings { recordings: vec![Recording { agent_name: "root_agent".to_string(), llm_recording: Some(LLMRecording { llm_request: BTreeMap::from([("config".to_string(), GoValue::Map(vec![("labels".to_string(), GoValue::Map(vec![])), ("system_instruction".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("You are a helpful weather assistant.\nAnswer in one sentence.".to_string()))])])), ("role".to_string(), GoValue::String("user".to_string()))])), ("tools".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("function_declarations".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("description".to_string(), GoValue::String("Returns the weather for a city.".to_string())), ("name".to_string(), GoValue::String("get_weather".to_string())), ("parameters".to_string(), GoValue::Map(vec![("properties".to_string(), GoValue::Map(vec![("city".to_string(), GoValue::Map(vec![("description".to_string(), GoValue::String("The city name, e.g. \"Paris\".".to_string())), ("type".to_string(), GoValue::String("STRING".to_string()))]))])), ("required".to_string(), GoValue::Seq(vec![GoValue::String("city".to_string())])), ("type".to_string(), GoValue::String("OBJECT".to_string()))]))])]))])]))])), ("contents".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("What's the weather in Paris?".to_string()))])])), ("role".to_string(), GoValue::String("user".to_string()))])])), ("model".to_string(), GoValue::String("gemini-2.5-flash".to_string()))]), llm_responses: vec![BTreeMap::from([("content".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("function_call".to_string(), GoValue::Map(vec![("args".to_string(), GoValue::Map(vec![("city".to_string(), GoValue::String("Paris".to_string()))])), ("id".to_string(), GoValue::String("adk-0001".to_string())), ("name".to_string(), GoValue::String("get_weather".to_string()))])), ("thought_signature".to_string(), GoValue::String("SGVsbG8=".to_string()))])])), ("role".to_string(), GoValue::String("model".to_string()))])), ("usage_metadata".to_string(), GoValue::Map(vec![("candidates_token_count".to_string(), GoValue::Int(7)), ("prompt_token_count".to_string(), GoValue::Int(42)), ("total_token_count".to_string(), GoValue::Int(49))]))])] }), ..Default::default() }, Recording { agent_name: "root_agent".to_string(), tool_recording: Some(ToolRecording { tool_call: BTreeMap::from([("args".to_string(), GoValue::Map(vec![("city".to_string(), GoValue::String("Paris".to_string()))])), ("id".to_string(), GoValue::String("adk-0001".to_string())), ("name".to_string(), GoValue::String("get_weather".to_string()))]), tool_response: BTreeMap::from([("id".to_string(), GoValue::String("adk-0001".to_string())), ("name".to_string(), GoValue::String("get_weather".to_string())), ("response".to_string(), GoValue::Map(vec![("count".to_string(), GoValue::Int(0)), ("ok".to_string(), GoValue::Bool(true)), ("result".to_string(), GoValue::String("sunny, 21°C".to_string()))]))]) }), ..Default::default() }, Recording { agent_name: "root_agent".to_string(), llm_recording: Some(LLMRecording { llm_request: BTreeMap::from([("config".to_string(), GoValue::Map(vec![("max_output_tokens".to_string(), GoValue::Int(1024)), ("temperature".to_string(), GoValue::Float(0.7)), ("top_p".to_string(), GoValue::Float(0.95))])), ("contents".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("What's the weather in Paris?".to_string()))])])), ("role".to_string(), GoValue::String("user".to_string()))]), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("function_call".to_string(), GoValue::Map(vec![("args".to_string(), GoValue::Map(vec![("city".to_string(), GoValue::String("Paris".to_string()))])), ("id".to_string(), GoValue::String("adk-0001".to_string())), ("name".to_string(), GoValue::String("get_weather".to_string()))]))])])), ("role".to_string(), GoValue::String("model".to_string()))]), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("function_response".to_string(), GoValue::Map(vec![("id".to_string(), GoValue::String("adk-0001".to_string())), ("name".to_string(), GoValue::String("get_weather".to_string())), ("response".to_string(), GoValue::Map(vec![("result".to_string(), GoValue::String("sunny, 21°C".to_string()))]))]))])])), ("role".to_string(), GoValue::String("user".to_string()))])])), ("model".to_string(), GoValue::String("gemini-2.5-flash".to_string()))]), llm_responses: vec![BTreeMap::from([("content".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("It is sunny and 21°C in Paris.".to_string()))])])), ("role".to_string(), GoValue::String("model".to_string()))])), ("finish_reason".to_string(), GoValue::String("STOP".to_string()))])] }), ..Default::default() }] },
        "tools" => Recordings { recordings: vec![Recording { agent_name: "coordinator".to_string(), llm_recording: Some(LLMRecording { llm_request: BTreeMap::from([("config".to_string(), GoValue::Map(vec![("system_instruction".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("You coordinate tools.\nAlways explain your reasoning.".to_string()))])])), ("role".to_string(), GoValue::String("user".to_string()))])), ("thinking_config".to_string(), GoValue::Map(vec![("include_thoughts".to_string(), GoValue::Bool(true)), ("thinking_budget".to_string(), GoValue::Int(-1))])), ("tools".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("function_declarations".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("name".to_string(), GoValue::String("add".to_string())), ("parameters".to_string(), GoValue::Map(vec![("properties".to_string(), GoValue::Map(vec![("a".to_string(), GoValue::Map(vec![("type".to_string(), GoValue::String("INTEGER".to_string()))])), ("b".to_string(), GoValue::Map(vec![("type".to_string(), GoValue::String("INTEGER".to_string()))]))])), ("required".to_string(), GoValue::Seq(vec![GoValue::String("a".to_string()), GoValue::String("b".to_string())])), ("type".to_string(), GoValue::String("OBJECT".to_string()))]))]), GoValue::Map(vec![("name".to_string(), GoValue::String("capital_of".to_string())), ("parameters_json_schema".to_string(), GoValue::Map(vec![("properties".to_string(), GoValue::Map(vec![("country".to_string(), GoValue::Map(vec![("enum".to_string(), GoValue::Seq(vec![GoValue::String("France".to_string()), GoValue::String("Spain".to_string()), GoValue::String("N/A".to_string())])), ("type".to_string(), GoValue::String("string".to_string()))]))])), ("type".to_string(), GoValue::String("object".to_string()))]))])]))])]))])), ("contents".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("Add 2 and 3, then look up the capital of France.".to_string()))])])), ("role".to_string(), GoValue::String("user".to_string()))])])), ("model".to_string(), GoValue::String("gemini-2.5-pro".to_string()))]), llm_responses: vec![BTreeMap::from([("content".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("I should call add first.".to_string())), ("thought".to_string(), GoValue::Bool(true))])])), ("role".to_string(), GoValue::String("model".to_string()))])), ("partial".to_string(), GoValue::Bool(true))]), BTreeMap::from([("content".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("function_call".to_string(), GoValue::Map(vec![("args".to_string(), GoValue::Map(vec![("a".to_string(), GoValue::Int(2)), ("b".to_string(), GoValue::Int(3))])), ("id".to_string(), GoValue::String("call-1".to_string())), ("name".to_string(), GoValue::String("add".to_string()))]))]), GoValue::Map(vec![("function_call".to_string(), GoValue::Map(vec![("args".to_string(), GoValue::Map(vec![("country".to_string(), GoValue::String("France".to_string()))])), ("id".to_string(), GoValue::String("call-2".to_string())), ("name".to_string(), GoValue::String("capital_of".to_string()))]))])])), ("role".to_string(), GoValue::String("model".to_string()))])), ("usage_metadata".to_string(), GoValue::Map(vec![("prompt_token_count".to_string(), GoValue::Int(120)), ("thoughts_token_count".to_string(), GoValue::Int(15))]))])] }), ..Default::default() }, Recording { agent_name: "coordinator".to_string(), tool_recording: Some(ToolRecording { tool_call: BTreeMap::from([("args".to_string(), GoValue::Map(vec![("a".to_string(), GoValue::Int(2)), ("b".to_string(), GoValue::Int(3))])), ("id".to_string(), GoValue::String("call-1".to_string())), ("name".to_string(), GoValue::String("add".to_string()))]), tool_response: BTreeMap::from([("id".to_string(), GoValue::String("call-1".to_string())), ("name".to_string(), GoValue::String("add".to_string())), ("response".to_string(), GoValue::Map(vec![("result".to_string(), GoValue::Int(5))]))]) }), ..Default::default() }, Recording { agent_name: "coordinator".to_string(), tool_recording: Some(ToolRecording { tool_call: BTreeMap::from([("args".to_string(), GoValue::Map(vec![("country".to_string(), GoValue::String("France".to_string()))])), ("id".to_string(), GoValue::String("call-2".to_string())), ("name".to_string(), GoValue::String("capital_of".to_string()))]), tool_response: BTreeMap::from([("id".to_string(), GoValue::String("call-2".to_string())), ("name".to_string(), GoValue::String("capital_of".to_string())), ("response".to_string(), GoValue::Map(vec![("notes".to_string(), GoValue::Null), ("result".to_string(), GoValue::String("Paris".to_string())), ("sources".to_string(), GoValue::Seq(vec![]))]))]) }), ..Default::default() }, Recording { user_message_index: 1, agent_name: "coordinator".to_string(), llm_recording: Some(LLMRecording { llm_request: BTreeMap::from([("contents".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("Summarize everything so far in a single, deliberately long sentence that keeps going well past eighty columns so that line width matters.".to_string()))])])), ("role".to_string(), GoValue::String("user".to_string()))])])), ("model".to_string(), GoValue::String("gemini-2.5-pro".to_string()))]), llm_responses: vec![BTreeMap::from([("avg_logprobs".to_string(), GoValue::Float(-0.125)), ("content".to_string(), GoValue::Map(vec![("parts".to_string(), GoValue::Seq(vec![GoValue::Map(vec![("text".to_string(), GoValue::String("2 + 3 = 5, and the capital of France is Paris — both answers came from tools: add and capital_of.".to_string()))])])), ("role".to_string(), GoValue::String("model".to_string()))])), ("finish_reason".to_string(), GoValue::String("STOP".to_string()))])] }), ..Default::default() }] },
        "mismatch" => Recordings { recordings: vec![Recording::default()] },
        _ => panic!("no serde golden case recordings-{case}"),
    }
}

#[cfg(test)]
mod tests {
    use gadk_goyaml::INLINE_FIELD;

    use super::*;
    use crate::support::golden::{NodeDump, golden_paths, parse_go_float, read_serde_golden};
    use crate::support::runs_self_tests;

    /// Whether the serde_json rendering `json` of a shape value has the structure, keys and
    /// scalar values of Go's `node.Encode(v)` dump (`key` maps a serde field name to its YAML
    /// key). serde_json writes non-finite floats as `null`; floats compare by value (Go's
    /// `float32` text parsed as `f32`, too).
    fn json_matches_dump(
        json: &serde_json::Value,
        dump: &NodeDump,
        key: &dyn Fn(&str) -> String,
        path: &str,
    ) -> Result<(), String> {
        use serde_json::Value;
        let fail = |what: &str| Err(format!("{path}: {what}: json {json}, dump {dump:?}"));
        let scalar = |tags: &[&str]| dump.kind == "scalar" && tags.contains(&dump.tag.as_str());
        let go_float = |s: &str| match s {
            ".inf" => f64::INFINITY,
            "-.inf" => f64::NEG_INFINITY,
            ".nan" => f64::NAN,
            _ => parse_go_float(s),
        };
        match json {
            Value::Null => {
                let non_finite = scalar(&["!!float"])
                    && matches!(dump.value.as_str(), ".inf" | "-.inf" | ".nan");
                if !(scalar(&["!!null"]) || non_finite) {
                    return fail("null");
                }
            }
            Value::Bool(b) => {
                if !scalar(&["!!bool"]) || dump.value != b.to_string() {
                    return fail("bool");
                }
            }
            Value::Number(n) if n.is_f64() => {
                let f = n.as_f64().unwrap_or(f64::NAN);
                let want = go_float(&dump.value);
                let want32 = dump.value.parse::<f32>().map(f64::from).ok();
                if !scalar(&["!!float", "!!int"]) || !(want == f || want32 == Some(f)) {
                    return fail("float");
                }
            }
            Value::Number(n) => {
                if !scalar(&["!!int"]) || dump.value != n.to_string() {
                    return fail("int");
                }
            }
            Value::String(s) => {
                if !scalar(&["!!str"]) || dump.value != *s {
                    return fail("string");
                }
            }
            Value::Array(items) => {
                if dump.kind != "sequence" || items.len() != dump.content.len() {
                    return fail("sequence");
                }
                for (i, (item, d)) in items.iter().zip(&dump.content).enumerate() {
                    json_matches_dump(item, d, key, &format!("{path}[{i}]"))?;
                }
            }
            Value::Object(fields) => {
                if dump.kind != "mapping" || fields.len() * 2 != dump.content.len() {
                    return fail("mapping");
                }
                for (k, v) in fields {
                    let yaml_key = key(k);
                    let Some(pair) = dump
                        .content
                        .chunks(2)
                        .find(|pair| pair[0].value == yaml_key)
                    else {
                        return fail(&format!("missing key {yaml_key:?}"));
                    };
                    json_matches_dump(v, &pair[1], key, &format!("{path}.{yaml_key}"))?;
                }
            }
        }
        Ok(())
    }

    /// Every serde golden case has an expected value, and the zero case is the zero value.
    #[test]
    fn support_every_serde_case_has_an_expected_value() {
        if !runs_self_tests() {
            return;
        }
        let mut n = 0;
        for path in golden_paths("serde") {
            let g = read_serde_golden(&path);
            let v = ShapeValue::expected(&g.shape, &g.case);
            assert_eq!(v.shape(), g.shape);
            if g.case == "zero" {
                v.assert_same(&ShapeValue::zero(&g.shape), &g.case);
            }
            n += 1;
        }
        assert_eq!(n, 77);
    }

    /// For the shapes without `GoValue` / `Document` fields (`golden_serde` checks those through
    /// the codec; serde_json renders a `Document` as unit),
    /// the expected value serialized by serde_json has exactly the keys (incl. `omitempty`
    /// skips) and scalars of Go's `node.Encode(v)` for that golden.
    #[test]
    fn support_expected_values_match_go_nodes() {
        if !runs_self_tests() {
            return;
        }
        let mut checked = 0;
        for path in golden_paths("serde") {
            let g = read_serde_golden(&path);
            if !matches!(
                g.shape.as_str(),
                "scalars" | "nested" | "optionals" | "godefaults"
            ) {
                continue;
            }
            let v = ShapeValue::expected(&g.shape, &g.case);
            let json = serde_json::to_value(&v).expect("serde_json");
            let opts = options();
            let key = |field: &str| -> String {
                let field: &'static str = Box::leak(field.to_string().into_boxed_str());
                let s = if g.shape == "godefaults" {
                    "GoDefaults"
                } else {
                    "Scalars"
                };
                opts.key_for(s, field).into_owned()
            };
            let dump = g.node.as_ref().expect("node");
            json_matches_dump(&json, dump, &key, &format!("{}-{}", g.shape, g.case))
                .unwrap_or_else(|e| panic!("{e}"));
            checked += 1;
        }
        assert_eq!(checked, 30 + 8 + 6 + 4);
    }

    #[test]
    fn support_options_reproduce_go_tags() {
        if !runs_self_tests() {
            return;
        }
        let opts = options();
        assert_eq!(opts.key_for("Scalars", "i8"), "i8");
        assert_eq!(opts.key_for("Nested", "inner_ptr"), "inner_ptr");
        assert_eq!(
            opts.key_for("Recording", "user_message_index"),
            "user_message_index"
        );
        assert_eq!(opts.key_for("LLMRecording", "llm_request"), "llm_request");
        assert_eq!(
            opts.key_for("GoDefaults", "user_message_index"),
            "usermessageindex"
        );
        assert_eq!(opts.key_for("GoDefaults", "http_timeout"), "httptimeout");
        assert!(opts.field_keys.is_empty());
        // The inline field's serde name is INLINE_FIELD; `count` is omitempty.
        let json = serde_json::to_value(WithInline::default()).expect("serde_json");
        let keys: Vec<&String> = json.as_object().expect("object").keys().collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|k| *k == "name") && keys.iter().any(|k| *k == INLINE_FIELD));
        // `index` (yaml:"-") is skipped both ways.
        let json = serde_json::to_value(GoDefaults {
            index: 9,
            ..Default::default()
        })
        .expect("serde_json");
        assert!(json.get("index").is_none());
        let back: GoDefaults =
            serde_json::from_str(r#"{"index": 9, "enabled": true}"#).expect("from_str");
        assert_eq!((back.index, back.enabled), (0, true));
    }

    #[test]
    fn support_bytes_serde() {
        if !runs_self_tests() {
            return;
        }
        assert_eq!(
            serde_json::to_string(&Bytes(vec![1, 2, 255])).expect("json"),
            "[1,2,255]"
        );
        let b: Bytes = serde_json::from_str("[1,2,255]").expect("seq");
        assert_eq!(b, Bytes(vec![1, 2, 255]));
        let b: Bytes = serde_json::from_str("\"hi\"").expect("string");
        assert_eq!(b, Bytes(b"hi".to_vec()));
        assert!(serde_json::from_str::<Bytes>("[256]").is_err());
    }

    #[test]
    fn support_shape_values() {
        if !runs_self_tests() {
            return;
        }
        for shape in SHAPES {
            assert_eq!(ShapeValue::zero(shape).shape(), shape);
        }
        // canonical: map order and NaN do not matter; Document arenas compare by dump.
        let a = ShapeValue::Collections(Collections {
            items: vec![GoValue::Map(vec![
                ("b".into(), GoValue::Float(f64::NAN)),
                ("a".into(), GoValue::Null),
            ])],
            ..Default::default()
        });
        let b = ShapeValue::Collections(Collections {
            items: vec![GoValue::Map(vec![
                ("a".into(), GoValue::Null),
                ("b".into(), GoValue::Float(f64::NAN)),
            ])],
            ..Default::default()
        });
        a.assert_same(&b, "maps");
        assert_ne!(a, b);
        let full = ShapeValue::expected("node", "full");
        let ShapeValue::Node(n) = &full else {
            panic!("{full:?}")
        };
        let mut moved = n.clone();
        moved.spec = moved.spec.subtree(moved.spec.root());
        moved.spec.push(gadk_goyaml::NodeData::default());
        ShapeValue::Node(moved).assert_same(&full, "arena");
        assert_eq!(n.spec[n.spec.root()].line, 3);
        let result = std::panic::catch_unwind(|| {
            ShapeValue::zero("scalars").assert_same(&ShapeValue::expected("scalars", "full"), "c")
        });
        assert!(result.is_err());
    }
}
