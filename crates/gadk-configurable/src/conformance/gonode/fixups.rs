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

//! The type-path fixup table and its application: each entry names a Go type path from a
//! recording root (e.g. `LLMRequest.Contents[].Parts[].InlineData.Data`, never a user map such
//! as `args` or `response`) and its class: Go `[]byte` (an `!!int` sequence), a non-pointer
//! `TextMarshaler` (`civil.Date`), `time.Duration` / `time.Time`, `genai.Schema` titles, and
//! `*jsonschema.Schema` values. The table equals the probe's reflection-generated fixture
//! (`tests/recordings_differential.rs`). Task 5 owns this file; nothing outside `gonode` uses
//! its items directly.
//!
//! The tables themselves ([`SHAPES`], [`FIXUPS`], [`JSONSCHEMA_FIELDS`], [`ROOTS`]) are
//! generated from the probe's `fixups.json` into [`super::shapes`]. This file holds their
//! types and lookups, the encoder that applies them ([`Builder`]: class 0 GoShape zero-fill
//! and the leaf classes), and the decoder's inverse fixups ([`invert`]).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, FixedOffset, Utc};
use gadk_goyaml::{Document, GoValue, Kind, NodeData, NodeId, Options, Style};
use serde_json::{Map, Value};

use crate::govalue::json_to_go_value;

pub use super::shapes::{FIXUPS, JSONSCHEMA_FIELDS, ROOTS, SHAPES};

/// How the probe classifies a struct field's Go type (`fixups.json` `shapes[].fields[].category`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// `[]byte` / `json.RawMessage`: an `!!int` sequence.
    Bytes,
    /// A non-pointer `time.Duration`: `Duration.String()`.
    Duration,
    /// A non-pointer `time.Time`: yaml.v3's timestamp.
    Time,
    /// A non-pointer `encoding.TextMarshaler` (`civil.Date`): `MarshalText`.
    TextMarshaler,
    /// A pointer.
    Ptr,
    /// An interface (`any`).
    Iface,
    /// A slice or array.
    Slice,
    /// A map.
    Map,
    /// A string kind (named string enums included).
    String,
    /// A signed integer kind.
    Int,
    /// An unsigned integer kind.
    Uint,
    /// `float32` / `float64`.
    Float,
    /// `bool`.
    Bool,
    /// A non-pointer struct.
    Struct,
    /// A func or chan (yaml.v3 cannot encode it).
    Func,
}

impl Category {
    /// The fixture's name of the category.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Bytes => "bytes",
            Category::Duration => "duration",
            Category::Time => "time",
            Category::TextMarshaler => "textmarshaler",
            Category::Ptr => "ptr",
            Category::Iface => "iface",
            Category::Slice => "slice",
            Category::Map => "map",
            Category::String => "string",
            Category::Int => "int",
            Category::Uint => "uint",
            Category::Float => "float",
            Category::Bool => "bool",
            Category::Struct => "struct",
            Category::Func => "func",
        }
    }
}

/// Go's node for a field's zero value (`fixups.json` `shapes[].fields[].zero`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zero {
    /// A nil pointer, interface or func: `!!null null`.
    Null,
    /// `""`: `!!str ""`, double-quoted.
    EmptyString,
    /// Integers and floats: `!!int 0` (a zero float re-parses as `!!int`).
    Int,
    /// `false`: `!!bool false`.
    False,
    /// A nil slice or `[]byte`: an empty flow `!!seq`.
    EmptySeq,
    /// A nil map: an empty flow `!!map`.
    EmptyMap,
    /// `time.Duration(0)`: `!!str 0s`.
    Duration,
    /// `time.Time{}`: `!!timestamp 0001-01-01T00:00:00Z`.
    Time,
    /// `civil.Date{}`: `!!str 0000-00-00`.
    Date,
    /// A non-pointer struct: the zero mapping of the named shape.
    Struct(&'static str),
    /// A value yaml.v3 panics on (`genai.HTTPOptions.ExtrasRequestProvider`).
    Unencodable,
}

impl Zero {
    /// The scalar/collection node of a non-struct zero: `(kind, tag, style, value)`.
    pub fn node(self) -> Option<(Kind, &'static str, Style, &'static str)> {
        Some(match self {
            Zero::Null => (Kind::Scalar, "!!null", Style::empty(), "null"),
            Zero::EmptyString => (Kind::Scalar, "!!str", Style::DOUBLE_QUOTED, ""),
            Zero::Int => (Kind::Scalar, "!!int", Style::empty(), "0"),
            Zero::False => (Kind::Scalar, "!!bool", Style::empty(), "false"),
            Zero::EmptySeq => (Kind::Sequence, "!!seq", Style::FLOW, ""),
            Zero::EmptyMap => (Kind::Mapping, "!!map", Style::FLOW, ""),
            Zero::Duration => (Kind::Scalar, "!!str", Style::empty(), "0s"),
            Zero::Time => (
                Kind::Scalar,
                "!!timestamp",
                Style::empty(),
                "0001-01-01T00:00:00Z",
            ),
            Zero::Date => (Kind::Scalar, "!!str", Style::empty(), "0000-00-00"),
            Zero::Struct(_) | Zero::Unencodable => return None,
        })
    }
}

/// One exported struct field, in Go field order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The YAML key (`strings.ToLower(GoFieldName)`).
    pub key: &'static str,
    /// The Go field name.
    pub go_name: &'static str,
    /// The JSON tag name (`""` untagged, `"-"` ignored): what gadk serde names the field.
    pub json: &'static str,
    /// The field's category.
    pub category: Category,
    /// The declared Go type.
    pub go_type: &'static str,
    /// The struct type reached through pointers, slices and maps, or `""`.
    pub elem: &'static str,
    /// Go's node for the zero value.
    pub zero: Zero,
}

/// A Go struct type's fields as yaml.v3 encodes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The Go type name (`genai.Part`).
    pub go_type: &'static str,
    /// The exported fields in Go order.
    pub fields: &'static [Field],
}

/// A fixup class (spec Design §"Recording types and the Go-shaped encoding").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixupClass {
    /// Go `[]byte`.
    Bytes,
    /// Go `time.Duration`.
    Duration,
    /// Go `time.Time`.
    Time,
    /// A non-pointer `encoding.TextMarshaler` (`civil.Date`).
    TextMarshaler,
    /// A `*genai.Schema` (its `title` is zero-filled).
    SchemaTitle,
    /// An `any` holding a `*jsonschema.Schema`.
    JsonSchema,
}

impl FixupClass {
    /// The fixture's name of the class.
    pub fn as_str(self) -> &'static str {
        match self {
            FixupClass::Bytes => "Bytes",
            FixupClass::Duration => "Duration",
            FixupClass::Time => "Time",
            FixupClass::TextMarshaler => "TextMarshaler",
            FixupClass::SchemaTitle => "SchemaTitle",
            FixupClass::JsonSchema => "JsonSchema",
        }
    }
}

/// One type-path fixup (`fixups.json` `fixups[]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fixup {
    /// The type path from the root (`LLMResponse.citationmetadata.citations[].publicationdate`).
    pub path: &'static str,
    /// The Go struct type holding the field.
    pub owner: &'static str,
    /// The field's YAML key.
    pub key: &'static str,
    /// The class.
    pub class: FixupClass,
    /// The declared Go type at the position.
    pub go_type: &'static str,
}

/// One jsonschema-go v0.4.3 `Schema` field (`fixups.json` `jsonschema_fields[]`), in
/// declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonSchemaField {
    /// The Go field name.
    pub go_name: &'static str,
    /// The YAML key (lowercased Go name).
    pub yaml_key: &'static str,
    /// The full JSON tag.
    pub json_tag: &'static str,
    /// The JSON keyword the field comes from (`type`, `items`, `dependencies` for the split
    /// fields, `*` for `Extra`, `""` for `PropertyOrder`).
    pub json_keyword: &'static str,
    /// The declared Go type.
    pub go_type: &'static str,
    /// The reflect kind.
    pub kind: &'static str,
    /// `Bytes` for `Default` (`json.RawMessage`), else `""`.
    pub class: &'static str,
}

/// The shape of Go struct type `go_type`.
pub fn shape(go_type: &str) -> Option<&'static Shape> {
    SHAPES
        .binary_search_by(|s| s.go_type.cmp(go_type))
        .ok()
        .map(|i| &SHAPES[i])
}

/// The fixup class of field `key` of struct type `owner` (the table is keyed by type path;
/// every path of an owner/key pair has the same class).
pub fn fixup_class(owner: &str, key: &str) -> Option<FixupClass> {
    FIXUPS
        .iter()
        .find(|f| f.owner == owner && f.key == key)
        .map(|f| f.class)
}

/// The Go type name of jsonschema-go's `Schema` (the `JsonSchema` root).
pub const JSONSCHEMA_TYPE: &str = "jsonschema.Schema";

/// Where a struct's field values come from when encoding.
#[derive(Clone, Copy)]
pub(crate) enum Keys {
    /// gadk serde's JSON (the field's JSON tag name).
    Json,
    /// The reshaped jsonschema object (the field's YAML key, see [`super::jsonschema`]).
    Yaml,
}

/// Builds the node tree Go's `node.Encode` builds, from the shapes table: struct mappings in
/// Go field order with every field (class 0 zero-fill), present leaves encoded on their own
/// by yaml.v3's rules (`gadk_goyaml::to_node`), the leaf fixup classes applied.
pub(crate) struct Builder {
    pub(crate) doc: Document,
    /// gadk serde keys no shape field consumed (`<go type>.<json key>`), for the tests.
    pub(crate) unmapped: Vec<String>,
}

impl Builder {
    pub(crate) fn new() -> Builder {
        Builder {
            doc: Document::default(),
            unmapped: Vec::new(),
        }
    }

    fn push(&mut self, kind: Kind, tag: &str, style: Style, value: &str) -> NodeId {
        self.doc.push(NodeData {
            kind,
            tag: tag.to_owned(),
            style,
            value: value.to_owned(),
            ..NodeData::default()
        })
    }

    /// A mapping (`!!map`, flow style when empty, as Go's re-parse gives it).
    pub(crate) fn mapping(&mut self, content: Vec<NodeId>) -> NodeId {
        let style = if content.is_empty() {
            Style::FLOW
        } else {
            Style::empty()
        };
        let id = self.push(Kind::Mapping, "!!map", style, "");
        self.doc.get_mut(id).content = content;
        id
    }

    /// A sequence (`!!seq`, flow style when empty).
    pub(crate) fn sequence(&mut self, content: Vec<NodeId>) -> NodeId {
        let style = if content.is_empty() {
            Style::FLOW
        } else {
            Style::empty()
        };
        let id = self.push(Kind::Sequence, "!!seq", style, "");
        self.doc.get_mut(id).content = content;
        id
    }

    /// `null`.
    pub(crate) fn null(&mut self) -> NodeId {
        self.push(Kind::Scalar, "!!null", Style::empty(), "null")
    }

    /// Copies node `at` of `src` (and its subtree) into this builder's document.
    fn graft(&mut self, src: &Document, at: NodeId) -> NodeId {
        let n = &src[at];
        let content: Vec<NodeId> = n.content.iter().map(|&c| self.graft(src, c)).collect();
        let mut data = n.clone();
        data.content = content;
        data.alias = None;
        self.doc.push(data)
    }

    /// Go's `node.Encode(v)` of a leaf value on its own, grafted in.
    pub(crate) fn leaf(&mut self, v: &GoValue) -> Result<NodeId, gadk_goyaml::Error> {
        let d = gadk_goyaml::to_node(v, &Options::default())?;
        Ok(self.graft(&d, d.root()))
    }

    /// A mapping key: a Go string encoded on its own (yaml.v3's `stringv` quoting, e.g. `"y"`).
    pub(crate) fn key(&mut self, k: &str) -> Result<NodeId, gadk_goyaml::Error> {
        self.leaf(&GoValue::String(k.to_owned()))
    }

    /// Go's zero node for a field.
    pub(crate) fn zero(&mut self, z: Zero) -> Result<NodeId, gadk_goyaml::Error> {
        match z {
            Zero::Struct(name) => self.struct_node(name, None, Keys::Json),
            Zero::Unencodable => Err(gadk_goyaml::Error::Unsupported(
                "cannot marshal type: func(map[string]interface {}) map[string]interface {}"
                    .to_owned(),
            )),
            _ => {
                let (kind, tag, style, value) = z.node().unwrap_or_default();
                Ok(self.push(kind, tag, style, value))
            }
        }
    }

    /// The mapping of struct type `go_type` from its gadk JSON object (`None` = every field
    /// zero): every Go field in Go order, absent ones as Go's zero node.
    pub(crate) fn struct_node(
        &mut self,
        go_type: &str,
        obj: Option<&Map<String, Value>>,
        keys: Keys,
    ) -> Result<NodeId, gadk_goyaml::Error> {
        let Some(s) = shape(go_type) else {
            return Err(gadk_goyaml::Error::Unsupported(format!(
                "no Go shape for {go_type}"
            )));
        };
        if let (Some(obj), Keys::Json) = (obj, keys) {
            for k in obj.keys() {
                if !s.fields.iter().any(|f| f.json == k.as_str()) {
                    self.unmapped.push(format!("{go_type}.{k}"));
                }
            }
        }
        let mut content = Vec::with_capacity(s.fields.len() * 2);
        for f in s.fields {
            let name = match keys {
                Keys::Json => f.json,
                Keys::Yaml => f.key,
            };
            let v = obj.and_then(|o| o.get(name));
            let k = self.key(f.key)?;
            let n = self.field_node(s.go_type, f, v)?;
            content.push(k);
            content.push(n);
        }
        Ok(self.mapping(content))
    }

    /// The node of field `f` of struct `owner` holding gadk JSON value `v` (`None` = absent).
    fn field_node(
        &mut self,
        owner: &str,
        f: &Field,
        v: Option<&Value>,
    ) -> Result<NodeId, gadk_goyaml::Error> {
        let Some(v) = v else {
            return self.zero(f.zero);
        };
        match f.category {
            Category::Bytes => self.bytes_node(v),
            Category::Duration => self.duration_node(v),
            Category::Time => match v.as_str().and_then(gadk_platform::go_parse_rfc3339) {
                Some(t) => self.leaf(&GoValue::Timestamp(t.fixed_offset())),
                None => self.zero(Zero::Time),
            },
            Category::TextMarshaler => self.date_node(v),
            Category::Iface if fixup_class(owner, f.key) == Some(FixupClass::JsonSchema) => {
                if v.is_null() {
                    return Ok(self.null());
                }
                self.jsonschema_node(v)
            }
            Category::Ptr | Category::Slice | Category::Map | Category::Struct
                if !f.elem.is_empty() =>
            {
                self.elem_node(f.go_type, f.elem, v)
            }
            Category::Func => self.zero(Zero::Unencodable),
            _ => {
                let g = leaf_value(f.go_type, v);
                self.leaf(&g)
            }
        }
    }

    /// A value of declared type `go_type` whose struct element type is `elem`.
    fn elem_node(
        &mut self,
        go_type: &str,
        elem: &str,
        v: &Value,
    ) -> Result<NodeId, gadk_goyaml::Error> {
        let keys = if elem == JSONSCHEMA_TYPE {
            Keys::Yaml
        } else {
            Keys::Json
        };
        let t = go_type.trim_start_matches('*');
        if let Some(inner) = t.strip_prefix("[]") {
            let Value::Array(items) = v else {
                return if v.is_null() {
                    self.zero(Zero::EmptySeq)
                } else {
                    self.elem_node(inner, elem, v)
                };
            };
            let mut content = Vec::with_capacity(items.len());
            for item in items {
                content.push(self.elem_node(inner, elem, item)?);
            }
            return Ok(self.sequence(content));
        }
        if let Some(inner) = t.strip_prefix("map[string]") {
            let Value::Object(m) = v else {
                return self.zero(Zero::EmptyMap);
            };
            let order = go_key_order(m.keys().map(String::as_str))?;
            let mut content = Vec::with_capacity(m.len() * 2);
            for k in order {
                let key = self.key(&k)?;
                let val = match m.get(&k) {
                    Some(item) => self.elem_node(inner, elem, item)?,
                    None => self.null(),
                };
                content.push(key);
                content.push(val);
            }
            return Ok(self.mapping(content));
        }
        // A (pointer to a) struct.
        if v.is_null() {
            return Ok(self.null());
        }
        if elem == JSONSCHEMA_TYPE {
            return self.jsonschema_node(v);
        }
        self.struct_node(elem, v.as_object(), keys)
    }

    /// A `*jsonschema.Schema` held as JSON (an object or a boolean schema).
    pub(crate) fn jsonschema_node(&mut self, v: &Value) -> Result<NodeId, gadk_goyaml::Error> {
        let reshaped = super::jsonschema::reshape(v);
        self.struct_node(JSONSCHEMA_TYPE, Some(&reshaped), Keys::Yaml)
    }

    /// Go `[]byte` from gadk's base64 string: an `!!int` sequence.
    fn bytes_node(&mut self, v: &Value) -> Result<NodeId, gadk_goyaml::Error> {
        let bytes = match v {
            Value::String(s) => STANDARD.decode(s.as_bytes()).unwrap_or_default(),
            _ => Vec::new(),
        };
        if bytes.is_empty() {
            return self.zero(Zero::EmptySeq);
        }
        let seq = GoValue::Seq(bytes.into_iter().map(|b| GoValue::Int(b.into())).collect());
        self.leaf(&seq)
    }

    /// Go `Duration.String()`: a JSON integer is the exact nanoseconds (the typed overlay of
    /// [`super::roots`]), a string is parsed with Go's `time.ParseDuration`.
    fn duration_node(&mut self, v: &Value) -> Result<NodeId, gadk_goyaml::Error> {
        let nanos = match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => gadk_platform::go_parse_duration(s).ok().map(|d| d.0),
            _ => None,
        };
        match nanos {
            Some(n) => {
                let text = gadk_platform::go_format_duration(gadk_platform::GoDuration(n));
                self.leaf(&GoValue::String(text))
            }
            None => self.zero(Zero::Duration),
        }
    }

    /// `civil.Date.MarshalText` (`%04d-%02d-%02d`) from genai's `{year, month, day}` JSON.
    fn date_node(&mut self, v: &Value) -> Result<NodeId, gadk_goyaml::Error> {
        let part = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        let text = format!(
            "{:04}-{:02}-{:02}",
            part("year"),
            part("month"),
            part("day")
        );
        self.leaf(&GoValue::String(text))
    }
}

/// yaml.v3's key order for a map with these string keys (its `keyList` sort): the keys
/// encoded as a Go `map[string]int` and read back.
pub(crate) fn go_key_order<'a>(
    keys: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>, gadk_goyaml::Error> {
    let m = GoValue::Map(keys.map(|k| (k.to_owned(), GoValue::Null)).collect());
    let d = gadk_goyaml::to_node(&m, &Options::default())?;
    Ok(d[d.root()]
        .content
        .iter()
        .step_by(2)
        .map(|&k| d[k].value.clone())
        .collect())
}

/// The Go value of gadk JSON `v` at a non-struct position of declared type `go_type`
/// (`*int`, `[]string`, `map[string]any`, `float32`, …): numbers take the declared number
/// type (`any` numbers are `float64`, a `float32` keeps its 32-bit digits).
pub(crate) fn leaf_value(go_type: &str, v: &Value) -> GoValue {
    let t = go_type.trim_start_matches('*');
    if v.is_null() {
        return GoValue::Null;
    }
    if let Some(inner) = t.strip_prefix("[]") {
        return match v {
            Value::Array(items) => {
                GoValue::Seq(items.iter().map(|i| leaf_value(inner, i)).collect())
            }
            _ => GoValue::Seq(Vec::new()),
        };
    }
    let map_inner = if t == "http.Header" {
        Some("[]string")
    } else {
        t.strip_prefix("map[string]")
    };
    if let Some(inner) = map_inner {
        return match v {
            Value::Object(m) => GoValue::Map(
                m.iter()
                    .map(|(k, i)| (k.clone(), leaf_value(inner, i)))
                    .collect(),
            ),
            _ => GoValue::Map(Vec::new()),
        };
    }
    match t {
        "interface {}" => json_to_go_value(v),
        "bool" => GoValue::Bool(v.as_bool().unwrap_or_default()),
        "float64" => GoValue::Float(v.as_f64().unwrap_or_default()),
        "float32" => {
            let f = v.as_f64().unwrap_or_default();
            let text = gadk_platform::go_format_float_g(f, 32);
            GoValue::Float(text.parse().unwrap_or(f))
        }
        "int" | "int8" | "int16" | "int32" | "int64" => match v {
            Value::String(s) => GoValue::Int(s.parse().unwrap_or_default()),
            _ => GoValue::Int(v.as_i64().unwrap_or_default()),
        },
        "uint" | "uint8" | "uint16" | "uint32" | "uint64" => match v {
            Value::String(s) => GoValue::Uint(s.parse().unwrap_or_default()),
            _ => GoValue::Uint(v.as_u64().unwrap_or_default()),
        },
        _ => match v {
            Value::String(s) => GoValue::String(s.clone()),
            other => GoValue::String(other.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Decoding: the inverse fixups.
// ---------------------------------------------------------------------------------------------

/// The recording roots' types, by the YAML key that leads to them.
const ROOT_TYPES: [(&str, &str); 4] = [
    ("llm_request", "model.LLMRequest"),
    ("llm_responses", "model.LLMResponse"),
    ("tool_call", "genai.FunctionCall"),
    ("tool_response", "genai.FunctionResponse"),
];

/// The inverse fixups over the recordings document at `at`, in place, before the serde
/// decode: a Go `[]byte` `!!int` sequence becomes gadk's base64 string, a `civil.Date` text
/// becomes genai's `{year, month, day}` (a text Go's `UnmarshalText` rejects aborts the decode
/// with Go's error, as yaml.v3's `fail` does), the zero `time.Time` becomes gadk's `None`.
/// Returns the yaml.v3 type errors found on the way (`(line, message)`), which yaml.v3 would
/// have collected while decoding those positions.
pub(crate) fn invert(
    doc: &mut Document,
    at: NodeId,
) -> Result<Vec<(usize, String)>, gadk_goyaml::Error> {
    let mut inv = Inverse {
        doc,
        errors: Vec::new(),
    };
    let mut root = at;
    if inv.doc[root].kind == Kind::Document {
        match inv.doc[root].content.first() {
            Some(&c) => root = c,
            None => return Ok(Vec::new()),
        }
    }
    for (_, recs) in inv.pairs(root, &["recordings"]) {
        for rec in inv.items(recs) {
            for (k, v) in inv.pairs(rec, &["llm_recording", "tool_recording"]) {
                let keys: &[&str] = if k == "llm_recording" {
                    &["llm_request", "llm_responses"]
                } else {
                    &["tool_call", "tool_response"]
                };
                for (rk, rv) in inv.pairs(v, keys) {
                    let Some(&(_, ty)) = ROOT_TYPES.iter().find(|(name, _)| *name == rk) else {
                        continue;
                    };
                    if rk == "llm_responses" {
                        for item in inv.items(rv) {
                            inv.walk_struct(ty, item)?;
                        }
                    } else {
                        inv.walk_struct(ty, rv)?;
                    }
                }
            }
        }
    }
    Ok(inv.errors)
}

struct Inverse<'a> {
    doc: &'a mut Document,
    errors: Vec<(usize, String)>,
}

impl Inverse<'_> {
    /// The `(key, value)` pairs of mapping `m` whose scalar key is one of `keys` (all scalar
    /// keys when `keys` is empty), in document order.
    fn pairs(&self, m: NodeId, keys: &[&str]) -> Vec<(String, NodeId)> {
        let n = &self.doc[m];
        if n.kind != Kind::Mapping {
            return Vec::new();
        }
        n.content
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|kv| {
                let k = &self.doc[kv[0]];
                (k.kind == Kind::Scalar && (keys.is_empty() || keys.contains(&k.value.as_str())))
                    .then(|| (k.value.clone(), kv[1]))
            })
            .collect()
    }

    /// The items of sequence `s`.
    fn items(&self, s: NodeId) -> Vec<NodeId> {
        let n = &self.doc[s];
        if n.kind == Kind::Sequence {
            n.content.clone()
        } else {
            Vec::new()
        }
    }

    /// Walks mapping `m` as Go struct `go_type` (keys matched exactly, as yaml.v3 does).
    fn walk_struct(&mut self, go_type: &str, m: NodeId) -> Result<(), gadk_goyaml::Error> {
        let Some(s) = shape(go_type) else {
            return Ok(());
        };
        for (k, v) in self.pairs(m, &[]) {
            let Some(f) = s.fields.iter().find(|f| f.key == k) else {
                continue;
            };
            match f.category {
                Category::Bytes => self.bytes(v),
                Category::TextMarshaler => self.date(v)?,
                Category::Time => self.time(v),
                Category::Ptr | Category::Slice | Category::Map | Category::Struct
                    if !f.elem.is_empty() && f.elem != JSONSCHEMA_TYPE =>
                {
                    self.walk_elem(f.go_type, f.elem, v)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn walk_elem(
        &mut self,
        go_type: &str,
        elem: &str,
        v: NodeId,
    ) -> Result<(), gadk_goyaml::Error> {
        let t = go_type.trim_start_matches('*');
        if let Some(inner) = t.strip_prefix("[]") {
            for item in self.items(v) {
                self.walk_elem(inner, elem, item)?;
            }
            return Ok(());
        }
        if let Some(inner) = t.strip_prefix("map[string]") {
            for (_, item) in self.pairs(v, &[]) {
                self.walk_elem(inner, elem, item)?;
            }
            return Ok(());
        }
        self.walk_struct(elem, v)
    }

    /// Go's `d.terror(n, tag, out)` text for node `v` decoded into Go type `into`.
    fn terror(&mut self, v: NodeId, into: &str) {
        let n = &self.doc[v];
        let tag = self.doc.short_tag(v);
        let value = if tag == "!!seq" || tag == "!!map" {
            String::new()
        } else if n.value.len() > 10 {
            let cut = String::from_utf8_lossy(&n.value.as_bytes()[..7]).into_owned();
            format!(" `{cut}...`")
        } else {
            format!(" `{}`", n.value)
        };
        let line = n.line;
        self.errors.push((
            line,
            format!("line {line}: cannot unmarshal {tag}{value} into {into}"),
        ));
    }

    fn set_null(&mut self, v: NodeId) {
        let n = self.doc.get_mut(v);
        n.kind = Kind::Scalar;
        n.tag = "!!null".to_owned();
        n.style = Style::empty();
        n.value = "null".to_owned();
        n.content.clear();
    }

    fn set_str(&mut self, v: NodeId, s: &str) {
        let n = self.doc.get_mut(v);
        n.kind = Kind::Scalar;
        n.tag = "!!str".to_owned();
        n.style = Style::DOUBLE_QUOTED;
        n.value = s.to_owned();
        n.content.clear();
    }

    /// Go `[]byte`: yaml.v3 decodes a sequence element by element into `uint8` (a failing
    /// element is dropped with a type error, a null one silently) and rejects any non-null
    /// scalar or mapping (`cannot unmarshal !!str `x` into []uint8`).
    fn bytes(&mut self, v: NodeId) {
        match self.doc[v].kind {
            Kind::Sequence => {
                let decoded =
                    gadk_goyaml::from_node_partial::<Vec<u8>>(self.doc, v, &Options::default());
                let bytes = match decoded {
                    Ok((bytes, terr)) => {
                        for e in terr.map(|t| t.errors).unwrap_or_default() {
                            self.errors.push((error_line(&e), e));
                        }
                        bytes
                    }
                    Err(_) => Vec::new(),
                };
                self.set_str(v, &STANDARD.encode(bytes));
            }
            Kind::Scalar if self.doc.short_tag(v) == "!!null" => {}
            Kind::Scalar | Kind::Mapping => {
                self.terror(v, "[]uint8");
                self.set_null(v);
            }
            _ => {}
        }
    }

    /// `civil.Date.UnmarshalText` of a scalar (yaml.v3 hands a TextUnmarshaler the node's
    /// text); its error aborts the whole decode, as yaml.v3's `fail(err)` does.
    fn date(&mut self, v: NodeId) -> Result<(), gadk_goyaml::Error> {
        match self.doc[v].kind {
            Kind::Scalar if self.doc.short_tag(v) == "!!null" => Ok(()),
            Kind::Scalar => {
                let text = if self.doc.short_tag(v) == "!!binary" {
                    let raw = STANDARD
                        .decode(self.doc[v].value.as_bytes())
                        .unwrap_or_default();
                    String::from_utf8_lossy(&raw).into_owned()
                } else {
                    self.doc[v].value.clone()
                };
                let (year, month, day) =
                    parse_civil_date(&text).map_err(gadk_goyaml::Error::Custom)?;
                let mut content = Vec::with_capacity(6);
                for (k, n) in [("year", year), ("month", month), ("day", day)] {
                    content.push(self.doc.push(NodeData {
                        kind: Kind::Scalar,
                        tag: "!!str".to_owned(),
                        value: k.to_owned(),
                        ..NodeData::default()
                    }));
                    content.push(self.doc.push(NodeData {
                        kind: Kind::Scalar,
                        tag: "!!int".to_owned(),
                        value: n.to_string(),
                        ..NodeData::default()
                    }));
                }
                let n = self.doc.get_mut(v);
                n.kind = Kind::Mapping;
                n.tag = "!!map".to_owned();
                n.style = Style::empty();
                n.value.clear();
                n.content = content;
                Ok(())
            }
            Kind::Sequence => {
                self.terror(v, "civil.Date");
                self.set_null(v);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Go's zero `time.Time` is gadk's `None`.
    fn time(&mut self, v: NodeId) {
        if self.doc[v].kind != Kind::Scalar {
            return;
        }
        if let Ok(GoValue::Timestamp(t)) =
            gadk_goyaml::from_node::<GoValue>(self.doc, v, &Options::default())
            && is_go_zero_time(&t)
        {
            self.set_null(v);
        }
    }
}

/// Go `time.Time.IsZero`: January 1, year 1, 00:00:00 UTC.
fn is_go_zero_time(t: &DateTime<FixedOffset>) -> bool {
    t.with_timezone(&Utc).timestamp() == -62_135_596_800 && t.timestamp_subsec_nanos() == 0
}

/// The line of a yaml.v3 type-error message (`line N: …`).
fn error_line(e: &str) -> usize {
    e.strip_prefix("line ")
        .and_then(|r| r.split(':').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Merges the inverse fixups' type errors into the serde decode's (`(line, message)` in
/// document order, ours first on the same line), as yaml.v3's single collection would order
/// them.
pub(crate) fn merge_type_errors(
    mut ours: Vec<(usize, String)>,
    theirs: Option<gadk_goyaml::TypeError>,
) -> Option<gadk_goyaml::TypeError> {
    for e in theirs.map(|t| t.errors).unwrap_or_default() {
        ours.push((error_line(&e), e));
    }
    if ours.is_empty() {
        return None;
    }
    ours.sort_by_key(|(line, _)| *line);
    Some(gadk_goyaml::TypeError {
        errors: ours.into_iter().map(|(_, e)| e).collect(),
    })
}

/// Go `civil.ParseDate(s)` = `time.Parse("2006-01-02", s)`: `(year, month, day)` or Go's
/// `*time.ParseError` text (`parsing time "0000-00-00": month out of range`).
pub(crate) fn parse_civil_date(s: &str) -> Result<(i64, i64, i64), String> {
    // Go: time/format.go parse (layout "2006-01-02": stdLongYear, "-", stdZeroMonth, "-",
    // stdZeroDay)
    const LAYOUT: &str = "2006-01-02";
    let b = s.as_bytes();
    let cannot = |value: &[u8], elem: &str| {
        format!(
            "parsing time {} as {}: cannot parse {} as {}",
            time_quote(s),
            time_quote(LAYOUT),
            time_quote(&String::from_utf8_lossy(value)),
            time_quote(elem)
        )
    };
    // stdLongYear
    if b.len() < 4 || !b[0].is_ascii_digit() {
        return Err(cannot(b, "2006"));
    }
    let (p, mut rest) = b.split_at(4);
    let Some(year) = time_atoi(p) else {
        return Err(cannot(b, "2006"));
    };
    let mut fields = [0i64; 2];
    for (i, elem) in ["01", "02"].iter().enumerate() {
        // The "-" literal prefix.
        match rest.split_first() {
            Some((b'-', r)) => rest = r,
            _ => return Err(cannot(rest, "-")),
        }
        let hold = rest;
        let Some((n, r)) = getnum(rest, true) else {
            return Err(cannot(hold, elem));
        };
        rest = r;
        if i == 0 && !(1..=12).contains(&n) {
            return Err(format!(
                "parsing time {}: month out of range",
                time_quote(s)
            ));
        }
        fields[i] = n;
    }
    if !rest.is_empty() {
        let extra = time_quote(&String::from_utf8_lossy(rest));
        return Err(format!(
            "parsing time {}: extra text: {extra}",
            time_quote(s)
        ));
    }
    let [month, day] = fields;
    if day < 1 || day > days_in(month, year) {
        return Err(format!("parsing time {}: day out of range", time_quote(s)));
    }
    Ok((year, month, day))
}

/// Go time's `getnum(s, fixed)`.
fn getnum(s: &[u8], fixed: bool) -> Option<(i64, &[u8])> {
    let digit = |i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    if !digit(0) {
        return None;
    }
    if !digit(1) {
        if fixed {
            return None;
        }
        return Some((i64::from(s[0] - b'0'), &s[1..]));
    }
    Some((
        i64::from(s[0] - b'0') * 10 + i64::from(s[1] - b'0'),
        &s[2..],
    ))
}

/// Go time's `atoi` of the 4-byte year (its first byte is a digit).
fn time_atoi(p: &[u8]) -> Option<i64> {
    if !p.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(p).ok()?.parse().ok()
}

/// Go time's `daysIn(month, year)`.
fn days_in(month: i64, year: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Go time's `quote`: `"…"` with `"` and `\` escaped and every byte of a non-ASCII or control
/// character as `\xNN`.
fn time_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c as u32 >= 0x80 || c < ' ' {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("\\x{b:02x}"));
            }
        } else {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_parse_as_go() {
        assert_eq!(parse_civil_date("2024-01-02"), Ok((2024, 1, 2)));
        assert_eq!(parse_civil_date("0000-02-29"), Ok((0, 2, 29)));
        let cases = [
            (
                "0000-00-00",
                r#"parsing time "0000-00-00": month out of range"#,
            ),
            (
                "2024-00-00",
                r#"parsing time "2024-00-00": month out of range"#,
            ),
            (
                "2024-13-01",
                r#"parsing time "2024-13-01": month out of range"#,
            ),
            (
                "2023-02-29",
                r#"parsing time "2023-02-29": day out of range"#,
            ),
            (
                "2024-01-00",
                r#"parsing time "2024-01-00": day out of range"#,
            ),
            (
                "2024-1-02",
                r#"parsing time "2024-1-02" as "2006-01-02": cannot parse "1-02" as "01""#,
            ),
            (
                "24-01-02",
                r#"parsing time "24-01-02" as "2006-01-02": cannot parse "24-01-02" as "2006""#,
            ),
            (
                "2024/01/02",
                r#"parsing time "2024/01/02" as "2006-01-02": cannot parse "/01/02" as "-""#,
            ),
            (
                "2024-01-02x",
                r#"parsing time "2024-01-02x": extra text: "x""#,
            ),
            (
                "2a24-01-02",
                r#"parsing time "2a24-01-02" as "2006-01-02": cannot parse "2a24-01-02" as "2006""#,
            ),
            (
                "é024-01-02",
                r#"parsing time "\xc3\xa9024-01-02" as "2006-01-02": cannot parse "\xc3\xa9024-01-02" as "2006""#,
            ),
            (
                "",
                r#"parsing time "" as "2006-01-02": cannot parse "" as "2006""#,
            ),
        ];
        for (input, want) in cases {
            assert_eq!(parse_civil_date(input), Err(want.to_owned()), "{input}");
        }
    }

    #[test]
    fn leaf_values_take_the_declared_number_type() {
        let v = serde_json::json!(0.699999988079071);
        assert_eq!(leaf_value("*float32", &v), GoValue::Float(0.7));
        assert_eq!(
            leaf_value("*int64", &serde_json::json!("12")),
            GoValue::Int(12)
        );
        assert_eq!(
            leaf_value("interface {}", &serde_json::json!(2)),
            GoValue::Float(2.0)
        );
        assert_eq!(
            leaf_value("[]string", &serde_json::json!(["a"])),
            GoValue::Seq(vec![GoValue::String("a".into())])
        );
    }

    #[test]
    fn key_order_is_yaml_v3_s() {
        let keys = ["b", "a10", "a2", "A"];
        assert_eq!(
            go_key_order(keys.into_iter()).unwrap(),
            vec!["A", "a2", "a10", "b"]
        );
    }

    #[test]
    fn shapes_are_sorted_for_lookup() {
        assert!(SHAPES.windows(2).all(|w| w[0].go_type < w[1].go_type));
        assert_eq!(shape("genai.Part").unwrap().fields[7].key, "text");
        assert_eq!(
            fixup_class("genai.Citation", "publicationdate"),
            Some(FixupClass::TextMarshaler)
        );
    }
}
