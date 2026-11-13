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

//! Golden loaders (format: `scripts/gen-go-goyaml/SCENARIO.md`): the probe's parse, differential and serde goldens
//! as Rust values; [`NodeDump`] ⇄ [`Document`] conversion and a comparison of a document
//! against a dump that names the first differing path and field; [`ValueDump`] ⇄ [`GoValue`].

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::DateTime;
use gadk_goyaml::{Document, GoValue, Kind, NodeData, NodeId, Style};
use serde::Deserialize;
use serde::de::Error as _;

/// Golden counts the probe writes (`scripts/check-fixtures.sh` asserts the same).
pub const PARSE_GOLDEN_COUNT: usize = 625;
/// Differential goldens: 32 seeds × 20 cases.
pub const DIFF_GOLDEN_COUNT: usize = 640;
/// Serde goldens over the eight shapes.
pub const SERDE_GOLDEN_COUNT: usize = 77;

/// `crates/gadk-goyaml/testdata`.
pub fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// Decodes a golden's standard-base64 field.
pub fn b64(s: &str) -> Vec<u8> {
    STANDARD
        .decode(s)
        .unwrap_or_else(|e| panic!("invalid base64 in a golden: {e}"))
}

/// Shows bytes as text for failure messages.
pub fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------------------------
// Golden files.
// ---------------------------------------------------------------------------------------------

/// A parse golden (`testdata/golden/parse/<case>.json`) or a differential golden
/// (`testdata/golden/diff/<seed>-<n>.json`, which also carries `source`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseGolden {
    /// The case name (the file stem).
    pub case: String,
    /// The input bytes, base64.
    pub input_b64: String,
    /// Differential goldens only: the dump of the generated Go value the input was marshaled
    /// from.
    #[serde(default)]
    pub source: Option<ValueDump>,
    /// `yaml.Unmarshal(in, &node)`'s document node (`None` on error or for a zero node).
    pub node: Option<NodeDump>,
    /// `yaml.Unmarshal(in, &node)`'s error.
    pub error: Option<String>,
    /// `yaml.Marshal(&node)` (only when `Unmarshal` succeeded).
    pub marshal_b64: Option<String>,
    /// `yaml.Marshal(&node)`'s error.
    pub marshal_error: Option<String>,
    /// An `Encoder` with `SetIndent(2)` encoding the node, then `Close`.
    pub indent2_b64: Option<String>,
    /// The `Decoder` / `Encoder` stream view.
    pub stream: StreamGolden,
    /// `yaml.Unmarshal(in, &v)` with `var v interface{}`.
    pub value: Option<ValueDump>,
    /// That `Unmarshal`'s error.
    pub value_error: Option<String>,
}

impl ParseGolden {
    /// The input bytes.
    pub fn input(&self) -> Vec<u8> {
        b64(&self.input_b64)
    }

    /// The `Marshal(&node)` bytes.
    pub fn marshal(&self) -> Option<Vec<u8>> {
        self.marshal_b64.as_deref().map(b64)
    }

    /// The `SetIndent(2)` encoder bytes.
    pub fn indent2(&self) -> Option<Vec<u8>> {
        self.indent2_b64.as_deref().map(b64)
    }
}

/// A parse golden's `stream`: Go's `Decoder` decoding `yaml.Node`s until `io.EOF`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamGolden {
    /// The decoded document nodes (possibly some before an error).
    pub nodes: Vec<NodeDump>,
    /// The first non-EOF error.
    pub error: Option<String>,
    /// One default `Encoder` encoding every decoded node, then `Close` (`None` if that failed,
    /// e.g. `Close` without any `Encode`).
    pub marshal_b64: Option<String>,
}

impl StreamGolden {
    /// The re-encoded stream bytes.
    pub fn marshal(&self) -> Option<Vec<u8>> {
        self.marshal_b64.as_deref().map(b64)
    }
}

/// A serde golden (`testdata/golden/serde/<shape>-<case>.json`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerdeGolden {
    /// The shape (`scalars`, `nested`, `optionals`, `collections`, `inline`, `node`,
    /// `godefaults`, `recordings`).
    pub shape: String,
    /// The case name (`zero` first for every shape).
    pub case: String,
    /// The input decoded into the Go shape (`None`: the zero value, not decoded).
    pub input_b64: Option<String>,
    /// `yaml.Unmarshal(input, v)`'s error (Go keeps what it filled in).
    pub decode_error: Option<String>,
    /// `yaml.Marshal(v)`.
    pub marshal_b64: Option<String>,
    /// `yaml.Marshal(v)`'s error.
    pub marshal_error: Option<String>,
    /// The node `node.Encode(v)` fills (not a document node; lines and columns are 0).
    pub node: Option<NodeDump>,
}

impl SerdeGolden {
    /// The input bytes (`None` for the zero case).
    pub fn input(&self) -> Option<Vec<u8>> {
        self.input_b64.as_deref().map(b64)
    }

    /// The `Marshal(v)` bytes.
    pub fn marshal(&self) -> Option<Vec<u8>> {
        self.marshal_b64.as_deref().map(b64)
    }
}

/// The `*.json` files of `testdata/golden/<dir>`, sorted by name.
pub fn golden_paths(dir: &str) -> Vec<PathBuf> {
    let dir = testdata_dir().join("golden").join(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path()
        })
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Reads one parse or differential golden file.
pub fn read_parse_golden(path: &Path) -> ParseGolden {
    read_json(path)
}

/// Reads one serde golden file.
pub fn read_serde_golden(path: &Path) -> SerdeGolden {
    read_json(path)
}

/// The parse golden of `case` (`testdata/golden/parse/<case>.json`).
pub fn parse_golden(case: &str) -> ParseGolden {
    read_parse_golden(
        &testdata_dir()
            .join("golden/parse")
            .join(format!("{case}.json")),
    )
}

/// The differential golden of `case` (`<seed>-<n>`).
pub fn diff_golden(case: &str) -> ParseGolden {
    read_parse_golden(
        &testdata_dir()
            .join("golden/diff")
            .join(format!("{case}.json")),
    )
}

/// The serde golden of `shape`-`case`.
pub fn serde_golden(shape: &str, case: &str) -> SerdeGolden {
    read_serde_golden(
        &testdata_dir()
            .join("golden/serde")
            .join(format!("{shape}-{case}.json")),
    )
}

/// Every parse golden, sorted by case name (625).
pub fn parse_goldens() -> Vec<ParseGolden> {
    golden_paths("parse")
        .iter()
        .map(|p| read_parse_golden(p))
        .collect()
}

/// Every differential golden, sorted by file name (640).
pub fn diff_goldens() -> Vec<ParseGolden> {
    golden_paths("diff")
        .iter()
        .map(|p| read_parse_golden(p))
        .collect()
}

/// Every serde golden, sorted by file name (77).
pub fn serde_goldens() -> Vec<SerdeGolden> {
    golden_paths("serde")
        .iter()
        .map(|p| read_serde_golden(p))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Node dumps.
// ---------------------------------------------------------------------------------------------

/// The probe's dump of one `yaml.Node` and its content (SCENARIO.md "Node dump").
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDump {
    /// `document`, `sequence`, `mapping`, `scalar` or `alias` (`zero` only for a Go zero node,
    /// which the probe never dumps).
    pub kind: String,
    /// The set `yaml.Style` flags in the order `tagged, double, single, literal, folded, flow`.
    pub style: Vec<String>,
    /// `Tag`.
    pub tag: String,
    /// `Value`.
    pub value: String,
    /// `Anchor`.
    pub anchor: String,
    /// The content-index path from the dumped root to the first node (pre-order) that is the
    /// alias target.
    pub alias: Option<Vec<usize>>,
    /// `HeadComment`.
    pub head: String,
    /// `LineComment`.
    pub line_comment: String,
    /// `FootComment`.
    pub foot: String,
    /// `Line`.
    pub line: usize,
    /// `Column`.
    pub column: usize,
    /// `Content`.
    pub content: Vec<NodeDump>,
}

/// Parses one node dump from its JSON text.
pub fn node_dump_from_json(json: &str) -> NodeDump {
    serde_json::from_str(json).unwrap_or_else(|e| panic!("node dump JSON: {e}"))
}

/// The dump's name of a [`Kind`] (`zero` for [`Kind::Zero`], which Go's dumper rejects).
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Zero => "zero",
        Kind::Document => "document",
        Kind::Sequence => "sequence",
        Kind::Mapping => "mapping",
        Kind::Scalar => "scalar",
        Kind::Alias => "alias",
    }
}

/// The [`Kind`] of a dump's kind name.
pub fn kind_from_name(name: &str) -> Kind {
    match name {
        "zero" => Kind::Zero,
        "document" => Kind::Document,
        "sequence" => Kind::Sequence,
        "mapping" => Kind::Mapping,
        "scalar" => Kind::Scalar,
        "alias" => Kind::Alias,
        other => panic!("unknown node kind {other:?} in a dump"),
    }
}

/// The dump's style flag names, in dump order.
pub const STYLE_NAMES: [(Style, &str); 6] = [
    (Style::TAGGED, "tagged"),
    (Style::DOUBLE_QUOTED, "double"),
    (Style::SINGLE_QUOTED, "single"),
    (Style::LITERAL, "literal"),
    (Style::FOLDED, "folded"),
    (Style::FLOW, "flow"),
];

/// The dump's style list of `style` (unknown bits are dropped, as Go's dumper drops them).
pub fn style_names(style: Style) -> Vec<String> {
    STYLE_NAMES
        .iter()
        .filter(|(flag, _)| style.intersects(*flag))
        .map(|(_, name)| (*name).to_string())
        .collect()
}

/// The [`Style`] of a dump's style list.
pub fn style_from_names(names: &[String]) -> Style {
    let mut style = Style::empty();
    for name in names {
        let flag = STYLE_NAMES
            .iter()
            .find(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("unknown style {name:?} in a dump"))
            .0;
        style |= flag;
    }
    style
}

/// A [`Document`] holding the dumped tree: one fresh node per dumped node (pre-order, root
/// first) and each `alias` path resolved to the node at that path (which may be an ancestor,
/// as for `edge-anchor-self-map`).
pub fn dump_to_document(dump: &NodeDump) -> Document {
    let mut doc = Document::new(node_data(dump));
    let root = doc.root();
    let mut paths: HashMap<Vec<usize>, NodeId> = HashMap::new();
    let mut path = Vec::new();
    build_content(&mut doc, root, dump, &mut path, &mut paths);
    resolve_aliases(&mut doc, root, dump, &paths);
    doc
}

/// The fields of one dumped node, without content or alias.
fn node_data(dump: &NodeDump) -> NodeData {
    NodeData {
        kind: kind_from_name(&dump.kind),
        style: style_from_names(&dump.style),
        tag: dump.tag.clone(),
        value: dump.value.clone(),
        anchor: dump.anchor.clone(),
        alias: None,
        content: Vec::new(),
        head_comment: dump.head.clone(),
        line_comment: dump.line_comment.clone(),
        foot_comment: dump.foot.clone(),
        line: dump.line,
        column: dump.column,
    }
}

fn build_content(
    doc: &mut Document,
    id: NodeId,
    dump: &NodeDump,
    path: &mut Vec<usize>,
    paths: &mut HashMap<Vec<usize>, NodeId>,
) {
    paths.insert(path.clone(), id);
    for (i, child) in dump.content.iter().enumerate() {
        let child_id = doc.push(node_data(child));
        doc[id].content.push(child_id);
        path.push(i);
        build_content(doc, child_id, child, path, paths);
        path.pop();
    }
}

fn resolve_aliases(
    doc: &mut Document,
    id: NodeId,
    dump: &NodeDump,
    paths: &HashMap<Vec<usize>, NodeId>,
) {
    if let Some(target) = &dump.alias {
        let target_id = *paths
            .get(target)
            .unwrap_or_else(|| panic!("alias path {target:?} is not in the dumped tree"));
        doc[id].alias = Some(target_id);
    }
    let content = doc[id].content.clone();
    for (child_id, child) in content.into_iter().zip(&dump.content) {
        resolve_aliases(doc, child_id, child, paths);
    }
}

/// Go's `dumpNode` of node `at` of `doc`: its fields and content, with `alias` as the
/// content-index path from `at` to the first node (pre-order) that is the alias target.
///
/// Panics (like the probe) when an alias target is not reachable from `at` through content.
pub fn document_to_dump(doc: &Document, at: NodeId) -> NodeDump {
    let mut paths: HashMap<NodeId, Vec<usize>> = HashMap::new();
    let mut path = Vec::new();
    index_paths(doc, at, &mut path, &mut paths);
    dump_node(doc, at, &paths)
}

fn index_paths(
    doc: &Document,
    id: NodeId,
    path: &mut Vec<usize>,
    paths: &mut HashMap<NodeId, Vec<usize>>,
) {
    if paths.contains_key(&id) {
        // Go keeps the first path; a node's content was indexed on its first visit (this
        // also stops at content cycles, which Go would recurse on forever).
        return;
    }
    paths.insert(id, path.clone());
    for (i, &child) in doc[id].content.iter().enumerate() {
        path.push(i);
        index_paths(doc, child, path, paths);
        path.pop();
    }
}

fn dump_node(doc: &Document, id: NodeId, paths: &HashMap<NodeId, Vec<usize>>) -> NodeDump {
    let n = &doc[id];
    let alias = n.alias.map(|target| {
        paths
            .get(&target)
            .cloned()
            .unwrap_or_else(|| panic!("alias target outside the dumped tree"))
    });
    NodeDump {
        kind: kind_name(n.kind).to_string(),
        style: style_names(n.style),
        tag: n.tag.clone(),
        value: n.value.clone(),
        anchor: n.anchor.clone(),
        alias,
        head: n.head_comment.clone(),
        line_comment: n.line_comment.clone(),
        foot: n.foot_comment.clone(),
        line: n.line,
        column: n.column,
        content: n
            .content
            .iter()
            .map(|&c| dump_node(doc, c, paths))
            .collect(),
    }
}

/// The first difference between two dumps, as `<path>.<field>: got <got>, want <want>`
/// (`path` is `$` for the root, then `.content[i]` per level; fields are checked in dump
/// order, then the content length, then each child).
pub fn diff_dumps(got: &NodeDump, want: &NodeDump) -> Option<String> {
    let mut path = String::from("$");
    diff_at(got, want, &mut path)
}

fn diff_at(got: &NodeDump, want: &NodeDump, path: &mut String) -> Option<String> {
    macro_rules! field {
        ($name:literal, $f:ident) => {
            if got.$f != want.$f {
                return Some(format!(
                    "{path}.{}: got {:?}, want {:?}",
                    $name, got.$f, want.$f
                ));
            }
        };
    }
    field!("kind", kind);
    field!("style", style);
    field!("tag", tag);
    field!("value", value);
    field!("anchor", anchor);
    field!("alias", alias);
    field!("head", head);
    field!("line_comment", line_comment);
    field!("foot", foot);
    field!("line", line);
    field!("column", column);
    if got.content.len() != want.content.len() {
        return Some(format!(
            "{path}.content: got {} nodes, want {}",
            got.content.len(),
            want.content.len()
        ));
    }
    for (i, (g, w)) in got.content.iter().zip(&want.content).enumerate() {
        let len = path.len();
        let _ = write!(path, ".content[{i}]");
        if let Some(diff) = diff_at(g, w, path) {
            return Some(diff);
        }
        path.truncate(len);
    }
    None
}

/// Compares node `at` of `doc` with `want`: `Err` names the first differing path and field
/// (see [`diff_dumps`]).
pub fn compare_node(doc: &Document, at: NodeId, want: &NodeDump) -> Result<(), String> {
    match diff_dumps(&document_to_dump(doc, at), want) {
        None => Ok(()),
        Some(diff) => Err(diff),
    }
}

/// Asserts that node `at` of `doc` matches `want`; the panic names `context` (e.g. the golden
/// case) and the first differing path and field.
#[track_caller]
pub fn assert_node_eq(doc: &Document, at: NodeId, want: &NodeDump, context: &str) {
    if let Err(diff) = compare_node(doc, at, want) {
        panic!("{context}: node mismatch at {diff}");
    }
}

// ---------------------------------------------------------------------------------------------
// GoValue dumps.
// ---------------------------------------------------------------------------------------------

/// The probe's dump of a Go `interface{}` value (SCENARIO.md "Value dump"). Numbers,
/// timestamps and bytes keep their golden text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueDump {
    /// `{"t":"null"}`.
    Null,
    /// `{"t":"bool","v":b}`.
    Bool(bool),
    /// `{"t":"int","v":"<decimal>"}`.
    Int(String),
    /// `{"t":"uint","v":"<decimal>"}`.
    Uint(String),
    /// `{"t":"float","v":"<FormatFloat(v,'g',-1,64)>"}`.
    Float(String),
    /// `{"t":"string","v":s}`.
    String(String),
    /// `{"t":"bytes","b64":…}` (a Go string that is not valid UTF-8).
    Bytes(String),
    /// `{"t":"timestamp","v":"<RFC3339Nano>"}`.
    Timestamp(String),
    /// `{"t":"seq","v":[…]}`.
    Seq(Vec<ValueDump>),
    /// `{"t":"map","v":[[key, dump]…]}` (sorted by key).
    Map(Vec<(String, ValueDump)>),
    /// `{"t":"mapany","v":[[keydump, dump]…]}` (sorted by the key dump's compact JSON).
    MapAny(Vec<(ValueDump, ValueDump)>),
}

impl<'de> Deserialize<'de> for ValueDump {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let json = serde_json::Value::deserialize(deserializer)?;
        ValueDump::from_json(&json).map_err(D::Error::custom)
    }
}

impl ValueDump {
    /// Parses a value dump from its JSON value (every object must have exactly the keys its
    /// `t` calls for).
    pub fn from_json(json: &serde_json::Value) -> Result<ValueDump, String> {
        let obj = json
            .as_object()
            .ok_or_else(|| format!("value dump is not an object: {json}"))?;
        let t = obj
            .get("t")
            .and_then(|t| t.as_str())
            .ok_or_else(|| format!("value dump without \"t\": {json}"))?;
        let want_keys: &[&str] = match t {
            "null" => &["t"],
            "bytes" => &["t", "b64"],
            _ => &["t", "v"],
        };
        if obj.len() != want_keys.len() || want_keys.iter().any(|k| !obj.contains_key(*k)) {
            return Err(format!("value dump keys for {t:?}: {json}"));
        }
        let text = |key: &str| -> Result<String, String> {
            obj[key]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("value dump {t:?} wants a string {key:?}: {json}"))
        };
        let array = |key: &str| -> Result<&Vec<serde_json::Value>, String> {
            obj[key]
                .as_array()
                .ok_or_else(|| format!("value dump {t:?} wants an array: {json}"))
        };
        let pair = |entry: &serde_json::Value| -> Result<(serde_json::Value, ValueDump), String> {
            match entry.as_array().map(Vec::as_slice) {
                Some([k, v]) => Ok((k.clone(), ValueDump::from_json(v)?)),
                _ => Err(format!("map entry is not a pair: {entry}")),
            }
        };
        Ok(match t {
            "null" => ValueDump::Null,
            "bool" => ValueDump::Bool(
                obj["v"]
                    .as_bool()
                    .ok_or_else(|| format!("bool dump: {json}"))?,
            ),
            "int" => ValueDump::Int(text("v")?),
            "uint" => ValueDump::Uint(text("v")?),
            "float" => ValueDump::Float(text("v")?),
            "string" => ValueDump::String(text("v")?),
            "bytes" => ValueDump::Bytes(text("b64")?),
            "timestamp" => ValueDump::Timestamp(text("v")?),
            "seq" => ValueDump::Seq(
                array("v")?
                    .iter()
                    .map(ValueDump::from_json)
                    .collect::<Result<_, _>>()?,
            ),
            "map" => ValueDump::Map(
                array("v")?
                    .iter()
                    .map(|e| {
                        let (k, v) = pair(e)?;
                        let k = k
                            .as_str()
                            .ok_or_else(|| format!("map key is not a string: {e}"))?;
                        Ok((k.to_string(), v))
                    })
                    .collect::<Result<_, String>>()?,
            ),
            "mapany" => ValueDump::MapAny(
                array("v")?
                    .iter()
                    .map(|e| {
                        let (k, v) = pair(e)?;
                        Ok((ValueDump::from_json(&k)?, v))
                    })
                    .collect::<Result<_, String>>()?,
            ),
            other => return Err(format!("unknown value dump type {other:?}")),
        })
    }

    /// The compact JSON text Go's `json.Encoder` (`SetEscapeHTML(false)`) writes for this
    /// dump: the probe sorts `mapany` entries by this text.
    pub fn to_compact_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        let tv = |out: &mut String, t: &str| {
            out.push_str("{\"t\":");
            go_json_string(out, t);
        };
        match self {
            ValueDump::Null => {
                tv(out, "null");
                out.push('}');
            }
            ValueDump::Bool(b) => {
                tv(out, "bool");
                let _ = write!(out, ",\"v\":{b}}}");
            }
            ValueDump::Int(s)
            | ValueDump::Uint(s)
            | ValueDump::Float(s)
            | ValueDump::String(s)
            | ValueDump::Timestamp(s) => {
                let t = match self {
                    ValueDump::Int(_) => "int",
                    ValueDump::Uint(_) => "uint",
                    ValueDump::Float(_) => "float",
                    ValueDump::String(_) => "string",
                    _ => "timestamp",
                };
                tv(out, t);
                out.push_str(",\"v\":");
                go_json_string(out, s);
                out.push('}');
            }
            ValueDump::Bytes(b) => {
                tv(out, "bytes");
                out.push_str(",\"b64\":");
                go_json_string(out, b);
                out.push('}');
            }
            ValueDump::Seq(items) => {
                tv(out, "seq");
                out.push_str(",\"v\":[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push_str("]}");
            }
            ValueDump::Map(entries) => {
                tv(out, "map");
                out.push_str(",\"v\":[");
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    go_json_string(out, k);
                    out.push(',');
                    v.write_json(out);
                    out.push(']');
                }
                out.push_str("]}");
            }
            ValueDump::MapAny(entries) => {
                tv(out, "mapany");
                out.push_str(",\"v\":[");
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    k.write_json(out);
                    out.push(',');
                    v.write_json(out);
                    out.push(']');
                }
                out.push_str("]}");
            }
        }
    }
}

/// Go's `encoding/json` string encoding with `SetEscapeHTML(false)`: `"` and `\` escaped,
/// `\b \f \n \r \t`, other control characters as `\u00xx`, U+2028 and U+2029 as ` ` /
/// ` `, everything else verbatim.
pub fn go_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The [`GoValue`] a value dump describes (maps keep the dump's order).
pub fn value_from_dump(dump: &ValueDump) -> GoValue {
    match dump {
        ValueDump::Null => GoValue::Null,
        ValueDump::Bool(b) => GoValue::Bool(*b),
        ValueDump::Int(s) => GoValue::Int(s.parse().unwrap_or_else(|e| panic!("int {s:?}: {e}"))),
        ValueDump::Uint(s) => {
            GoValue::Uint(s.parse().unwrap_or_else(|e| panic!("uint {s:?}: {e}")))
        }
        ValueDump::Float(s) => GoValue::Float(parse_go_float(s)),
        ValueDump::String(s) => GoValue::String(s.clone()),
        ValueDump::Bytes(b) => GoValue::Bytes(b64(b)),
        ValueDump::Timestamp(s) => GoValue::Timestamp(
            DateTime::parse_from_rfc3339(s).unwrap_or_else(|e| panic!("timestamp {s:?}: {e}")),
        ),
        ValueDump::Seq(items) => GoValue::Seq(items.iter().map(value_from_dump).collect()),
        ValueDump::Map(entries) => GoValue::Map(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), value_from_dump(v)))
                .collect(),
        ),
        ValueDump::MapAny(entries) => GoValue::MapAny(
            entries
                .iter()
                .map(|(k, v)| (value_from_dump(k), value_from_dump(v)))
                .collect(),
        ),
    }
}

/// A float in Go's `strconv.FormatFloat(v, 'g', -1, 64)` text (`+Inf`, `-Inf`, `NaN`, `1e+21`).
pub fn parse_go_float(s: &str) -> f64 {
    match s {
        "+Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => s
            .parse()
            .unwrap_or_else(|e| panic!("float {s:?} in a value dump: {e}")),
    }
}

/// The probe's dump of `v`: `Map` entries sorted by key, `MapAny` entries sorted by the key
/// dump's compact JSON, floats as `FormatFloat(v, 'g', -1, 64)`, timestamps as RFC3339Nano.
pub fn value_to_dump(v: &GoValue) -> ValueDump {
    match v {
        GoValue::Null => ValueDump::Null,
        GoValue::Bool(b) => ValueDump::Bool(*b),
        GoValue::Int(i) => ValueDump::Int(i.to_string()),
        GoValue::Uint(u) => ValueDump::Uint(u.to_string()),
        GoValue::Float(f) => ValueDump::Float(gadk_platform::go_format_float_g(*f, 64)),
        GoValue::String(s) => ValueDump::String(s.clone()),
        GoValue::Bytes(b) => ValueDump::Bytes(STANDARD.encode(b)),
        GoValue::Timestamp(t) => ValueDump::Timestamp(gadk_platform::go_format_rfc3339_nano(t)),
        GoValue::Seq(items) => ValueDump::Seq(items.iter().map(value_to_dump).collect()),
        GoValue::Map(entries) => {
            let mut out: Vec<(String, ValueDump)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), value_to_dump(v)))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            ValueDump::Map(out)
        }
        GoValue::MapAny(entries) => {
            let mut out: Vec<(String, (ValueDump, ValueDump))> = entries
                .iter()
                .map(|(k, v)| {
                    let k = value_to_dump(k);
                    (k.to_compact_json(), (k, value_to_dump(v)))
                })
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            ValueDump::MapAny(out.into_iter().map(|(_, e)| e).collect())
        }
    }
}

/// `v` with every `Map` sorted by key and every `MapAny` sorted as the probe sorts it, so two
/// values that differ only in map order compare equal (Go maps have no order).
pub fn normalize_go_value(v: &GoValue) -> GoValue {
    match v {
        GoValue::Seq(items) => GoValue::Seq(items.iter().map(normalize_go_value).collect()),
        GoValue::Map(entries) => {
            let mut out: Vec<(String, GoValue)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), normalize_go_value(v)))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            GoValue::Map(out)
        }
        GoValue::MapAny(entries) => {
            let mut out: Vec<(String, (GoValue, GoValue))> = entries
                .iter()
                .map(|(k, v)| {
                    (
                        value_to_dump(k).to_compact_json(),
                        (normalize_go_value(k), normalize_go_value(v)),
                    )
                })
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            GoValue::MapAny(out.into_iter().map(|(_, e)| e).collect())
        }
        other => other.clone(),
    }
}

/// Asserts that `got` is the value `want` dumps to (compared as dumps, so NaN equals NaN and
/// map order does not matter); the panic names `context`.
#[track_caller]
pub fn assert_value_eq(got: &GoValue, want: &ValueDump, context: &str) {
    let got = value_to_dump(got);
    if got != *want {
        panic!(
            "{context}: value mismatch\n got: {}\nwant: {}",
            got.to_compact_json(),
            want.to_compact_json()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::runs_self_tests;

    #[test]
    fn support_golden_counts_and_names() {
        if !runs_self_tests() {
            return;
        }
        assert_eq!(golden_paths("parse").len(), PARSE_GOLDEN_COUNT);
        assert_eq!(golden_paths("diff").len(), DIFF_GOLDEN_COUNT);
        assert_eq!(golden_paths("serde").len(), SERDE_GOLDEN_COUNT);
        assert_eq!(
            PARSE_GOLDEN_COUNT + DIFF_GOLDEN_COUNT + SERDE_GOLDEN_COUNT,
            1342
        );
        let corpus = testdata_dir().join("corpus/parse");
        let corpus_files = std::fs::read_dir(&corpus).expect("corpus dir").count();
        assert_eq!(corpus_files, PARSE_GOLDEN_COUNT);
    }

    /// Every parse and differential golden loads; its case is its file name; a parse
    /// golden's input is its corpus file and it has no `source`, a differential golden has
    /// one; every node dump (node, stream nodes) survives NodeDump → Document → NodeDump and
    /// compares equal to itself; every value dump (value, source) survives
    /// ValueDump → GoValue → ValueDump.
    #[test]
    fn support_parse_and_diff_goldens_round_trip() {
        if !runs_self_tests() {
            return;
        }
        let mut nodes = 0;
        let mut values = 0;
        for (dir, is_diff) in [("parse", false), ("diff", true)] {
            for path in golden_paths(dir) {
                let g = read_parse_golden(&path);
                let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
                assert_eq!(g.case, stem, "{}", path.display());
                assert_eq!(g.source.is_some(), is_diff, "{}: source", g.case);
                if !is_diff {
                    let corpus = testdata_dir()
                        .join("corpus/parse")
                        .join(format!("{}.yaml", g.case));
                    let want = std::fs::read(&corpus).expect("corpus file");
                    assert_eq!(g.input(), want, "{}: input vs corpus", g.case);
                }
                for (what, dump) in g
                    .node
                    .iter()
                    .map(|d| ("node", d))
                    .chain(g.stream.nodes.iter().map(|d| ("stream node", d)))
                {
                    let doc = dump_to_document(dump);
                    assert_eq!(
                        &document_to_dump(&doc, doc.root()),
                        dump,
                        "{}: {what}",
                        g.case
                    );
                    assert_node_eq(&doc, doc.root(), dump, &g.case);
                    nodes += 1;
                }
                for dump in g.value.iter().chain(g.source.iter()) {
                    assert_eq!(&value_to_dump(&value_from_dump(dump)), dump, "{}", g.case);
                    assert_value_eq(&value_from_dump(dump), dump, &g.case);
                    values += 1;
                }
                // Byte fields decode.
                let _ = (g.marshal(), g.indent2(), g.stream.marshal());
            }
        }
        // 2367 node dumps (node + stream nodes) and 1795 value dumps (value + source).
        assert_eq!((nodes, values), (2367, 1795));
    }

    #[test]
    fn support_serde_goldens_round_trip() {
        if !runs_self_tests() {
            return;
        }
        for path in golden_paths("serde") {
            let g = read_serde_golden(&path);
            let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
            assert_eq!(format!("{}-{}", g.shape, g.case), stem);
            assert_eq!(g.input_b64.is_none(), g.case == "zero", "{stem}");
            let dump = g.node.as_ref().expect("every serde golden has a node");
            let doc = dump_to_document(dump);
            assert_eq!(&document_to_dump(&doc, doc.root()), dump, "{stem}");
            let _ = (g.input(), g.marshal());
        }
        let g = serde_golden("scalars", "full");
        assert_eq!(g.decode_error, None);
        assert!(g.marshal().is_some_and(|m| m.starts_with(b"b: true\n")));
    }

    #[test]
    fn support_lookup_by_case() {
        if !runs_self_tests() {
            return;
        }
        let g = parse_golden("edge-anchor-self-map");
        assert_eq!(g.input(), b"a: &a\n  b: *a\n");
        assert_eq!(
            g.value_error.as_deref(),
            Some("yaml: anchor 'a' value contains itself")
        );
        let g = diff_golden("17-1");
        assert!(g.error.is_some() && g.source.is_some());
    }

    /// An alias may point at an ancestor (edge-anchor-self-map: `a: &a\n  b: *a\n`).
    #[test]
    fn support_alias_to_ancestor() {
        if !runs_self_tests() {
            return;
        }
        let dump = node_dump_from_json(
            r#"{"kind":"mapping","style":[],"tag":"!!map","value":"","anchor":"a","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":3,"content":[
            {"kind":"scalar","style":[],"tag":"!!str","value":"b","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":2,"column":3,"content":[]},
            {"kind":"alias","style":[],"tag":"","value":"a","anchor":"","alias":[],"head":"","line_comment":"","foot":"","line":2,"column":6,"content":[]}]}"#,
        );
        let doc = dump_to_document(&dump);
        let root = doc.root();
        let alias = doc[root].content[1];
        assert_eq!(doc[alias].kind, Kind::Alias);
        assert_eq!(doc[alias].alias, Some(root));
        assert_eq!(document_to_dump(&doc, root), dump);
    }

    #[test]
    fn support_diff_names_path_and_field() {
        if !runs_self_tests() {
            return;
        }
        let want = node_dump_from_json(
            r##"{"kind":"sequence","style":["flow"],"tag":"!!seq","value":"","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":1,"column":1,"content":[
            {"kind":"scalar","style":[],"tag":"!!int","value":"1","anchor":"","alias":null,"head":"","line_comment":"","foot":"","line":1,"column":2,"content":[]},
            {"kind":"scalar","style":["double"],"tag":"!!str","value":"x","anchor":"","alias":null,"head":"","line_comment":"# c","foot":"","line":1,"column":5,"content":[]}]}"##,
        );
        let mut doc = dump_to_document(&want);
        assert_eq!(compare_node(&doc, doc.root(), &want), Ok(()));
        let second = doc[doc.root()].content[1];
        doc[second].column = 6;
        assert_eq!(
            compare_node(&doc, doc.root(), &want),
            Err("$.content[1].column: got 6, want 5".to_string())
        );
        doc[second].style = Style::SINGLE_QUOTED;
        assert_eq!(
            compare_node(&doc, doc.root(), &want),
            Err(r#"$.content[1].style: got ["single"], want ["double"]"#.to_string())
        );
        let root = doc.root();
        doc[root].content.pop();
        assert_eq!(
            compare_node(&doc, root, &want),
            Err("$.content: got 1 nodes, want 2".to_string())
        );
        let result = std::panic::catch_unwind(|| assert_node_eq(&doc, root, &want, "case-x"));
        let msg = *result
            .expect_err("mismatch panics")
            .downcast::<String>()
            .expect("text");
        assert_eq!(
            msg,
            "case-x: node mismatch at $.content: got 1 nodes, want 2"
        );
    }

    #[test]
    fn support_kind_and_style_names() {
        if !runs_self_tests() {
            return;
        }
        for kind in [
            Kind::Zero,
            Kind::Document,
            Kind::Sequence,
            Kind::Mapping,
            Kind::Scalar,
            Kind::Alias,
        ] {
            assert_eq!(kind_from_name(kind_name(kind)), kind);
        }
        let all = Style::TAGGED
            | Style::DOUBLE_QUOTED
            | Style::SINGLE_QUOTED
            | Style::LITERAL
            | Style::FOLDED
            | Style::FLOW;
        let names = style_names(all);
        assert_eq!(
            names,
            ["tagged", "double", "single", "literal", "folded", "flow"]
        );
        assert_eq!(style_from_names(&names), all);
        assert!(style_names(Style::from_bits(64)).is_empty());
        // The zero node round-trips (kind "zero", which the probe never writes).
        let doc = Document::default();
        let dump = document_to_dump(&doc, doc.root());
        assert_eq!(dump.kind, "zero");
        assert_eq!(dump_to_document(&dump), doc);
    }

    #[test]
    fn support_value_dumps() {
        if !runs_self_tests() {
            return;
        }
        let json = r#"{"t":"mapany","v":[[{"t":"int","v":"1"},{"t":"seq","v":[{"t":"null"},{"t":"bool","v":true},{"t":"uint","v":"18446744073709551615"},{"t":"float","v":"-Inf"},{"t":"bytes","b64":"gIE="},{"t":"timestamp","v":"2001-12-14T21:59:43.1-05:00"}]}],[{"t":"string","v":"k"},{"t":"map","v":[["a",{"t":"float","v":"1e+21"}]]}]]}"#;
        let dump = ValueDump::from_json(&serde_json::from_str(json).expect("json")).expect("dump");
        assert_eq!(dump.to_compact_json(), json);
        let v = value_from_dump(&dump);
        let GoValue::MapAny(entries) = &v else {
            panic!("{v:?}")
        };
        assert_eq!(entries[0].0, GoValue::Int(1));
        let GoValue::Seq(items) = &entries[0].1 else {
            panic!("{v:?}")
        };
        assert_eq!(items[2], GoValue::Uint(u64::MAX));
        assert_eq!(items[3], GoValue::Float(f64::NEG_INFINITY));
        assert_eq!(items[4], GoValue::Bytes(vec![0x80, 0x81]));
        assert_eq!(value_to_dump(&v), dump);
        // Encoding sorts: Map by key, MapAny by the key dump's JSON ("int" < "string").
        let unsorted = GoValue::MapAny(vec![
            (GoValue::String("k".into()), GoValue::Null),
            (GoValue::Int(2), GoValue::Null),
        ]);
        let ValueDump::MapAny(sorted) = value_to_dump(&unsorted) else {
            panic!()
        };
        assert_eq!(sorted[0].0, ValueDump::Int("2".into()));
        assert_eq!(
            normalize_go_value(&GoValue::Map(vec![
                ("b".into(), GoValue::Null),
                ("a".into(), GoValue::Seq(vec![]))
            ])),
            GoValue::Map(vec![
                ("a".into(), GoValue::Seq(vec![])),
                ("b".into(), GoValue::Null)
            ])
        );
        assert!(parse_go_float("NaN").is_nan());
        assert_eq!(parse_go_float("-0").to_bits(), (-0.0f64).to_bits());
        for bad in [
            r#"{"t":"int"}"#,
            r#"{"t":"null","v":1}"#,
            r#"{"t":"map","v":[[1,{"t":"null"}]]}"#,
            r#"{"t":"nope","v":1}"#,
            r#"[1]"#,
        ] {
            let json = serde_json::from_str(bad).expect("json");
            assert!(ValueDump::from_json(&json).is_err(), "{bad}");
        }
        let result = std::panic::catch_unwind(|| {
            assert_value_eq(&GoValue::Int(1), &ValueDump::Int("2".into()), "case-y")
        });
        let msg = *result
            .expect_err("mismatch panics")
            .downcast::<String>()
            .expect("text");
        assert_eq!(
            msg,
            "case-y: value mismatch\n got: {\"t\":\"int\",\"v\":\"1\"}\nwant: {\"t\":\"int\",\"v\":\"2\"}"
        );
    }

    #[test]
    fn support_go_json_string() {
        if !runs_self_tests() {
            return;
        }
        let mut out = String::new();
        go_json_string(
            &mut out,
            "a\"\\\u{8}\u{c}\n\r\t\u{1}\u{7f}<>&\u{2028}\u{2029}é",
        );
        assert_eq!(
            out,
            "\"a\\\"\\\\\\b\\f\\n\\r\\t\\u0001\u{7f}<>&\\u2028\\u2029é\""
        );
    }
}
