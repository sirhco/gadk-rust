// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! gadk-goyaml: a pure-Rust port of `gopkg.in/yaml.v3` v3.0.1 (the version ADK-Go pins), so
//! gadk reads and writes YAML exactly as ADK-Go does: the same node trees, error texts and
//! emitted bytes. This file ports `yaml.go`'s API.
//!
//! - [`unmarshal_node`] / [`marshal_node`]: Go's `yaml.Unmarshal(data, &node)` /
//!   `yaml.Marshal(&node)` over the [`Document`] node arena.
//! - [`unmarshal`] / [`marshal`] / [`from_node`] / [`to_node`]: serde values decoded and encoded
//!   by yaml.v3's reflection rules; struct keys come from [`Options::key_for`].
//! - [`Encoder`] / [`Decoder`]: multi-document streams (`SetIndent`, `Close`).
//! - [`GoValue`]: what yaml.v3 decodes into `interface{}`; [`InlineMap`]: Go's `,inline` map.
//!
//! Internal crate (`publish = false`); the libyaml-derived modules keep libyaml's MIT license.

#![forbid(unsafe_code)]

mod api;
mod de;
mod emitter;
mod encode;
mod error;
mod inline;
mod node;
mod parser;
mod reader;
mod resolve;
mod scanner;
mod ser;
mod sorter;
mod value;
mod writer;
mod yamlh;

#[cfg(test)]
mod parity_tests;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

pub use error::{Error, TypeError};
pub use inline::{INLINE_FIELD, InlineMap};
pub use node::{Document, Kind, NodeData, NodeId, Style};
pub use value::GoValue;

/// How serde names map to YAML keys (Go's struct tags, which serde types do not carry).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Struct names (serde's `name` argument) whose field names are used verbatim (yaml-tagged
    /// in Go).
    pub verbatim_structs: BTreeSet<&'static str>,
    /// (struct name, serde field name) → key, for fields whose Go name differs from the serde
    /// name.
    pub field_keys: BTreeMap<(&'static str, &'static str), &'static str>,
}

impl Options {
    /// The YAML key of field `field` of struct `struct_name`: the `field_keys` override, else the
    /// serde name verbatim if `struct_name` is in `verbatim_structs`, else Go's default (the
    /// field name lowercased, `getStructInfo`'s `strings.ToLower(field.Name)`): the serde name
    /// lowercased with `_` removed.
    pub fn key_for(&self, struct_name: &'static str, field: &'static str) -> Cow<'static, str> {
        if let Some(key) = self.field_keys.get(&(struct_name, field)) {
            return Cow::Borrowed(key);
        }
        if self.verbatim_structs.contains(struct_name) {
            return Cow::Borrowed(field);
        }
        Cow::Owned(gadk_platform::go_to_lower(&field.replace('_', "")))
    }
}

/// Go's `yaml.Unmarshal(data, &node)`: the document node of the first document (the zero node
/// for an empty stream).
pub fn unmarshal_node(data: &[u8]) -> Result<Document, Error> {
    // Go: yaml.go unmarshal (out = *Node: `d.unmarshal` copies the document node)
    let mut p = node::new_parser(data);
    let parsed = p.parse();
    p.destroy();
    Ok(match parsed? {
        Some(n) => p.arena.subtree(n),
        None => Document::default(),
    })
}

/// Go's `yaml.Marshal(&node)` for node `at` of `doc` (the zero node, `Document::default()`,
/// marshals to `null\n` as in Go).
pub fn marshal_node(doc: &Document, at: NodeId) -> Result<Vec<u8>, Error> {
    // Go: yaml.go Marshal
    let mut e = encode::new_encoder();
    let result = e.marshal_doc_node(doc, at).and_then(|()| e.finish());
    let out = std::mem::take(&mut e.emitter.output_buffer);
    e.destroy();
    result.map(|()| out)
}

/// Go's `yaml.Unmarshal(data, &v)` for a serde type.
#[allow(unused_variables)] // Task 8 writes the body and removes this line.
pub fn unmarshal<T: serde::de::DeserializeOwned>(data: &[u8], opts: &Options) -> Result<T, Error> {
    todo!("Task 8")
}

/// Go's `yaml.Marshal(v)` for a serde value.
#[allow(unused_variables)] // Task 9 writes the body and removes this line.
pub fn marshal<T: serde::Serialize + ?Sized>(v: &T, opts: &Options) -> Result<Vec<u8>, Error> {
    todo!("Task 9")
}

/// Go's `node.Encode(v)`: `marshal`, then a `textless` re-parse; returns the document's first
/// content node as its own [`Document`].
#[allow(unused_variables)] // Task 9 writes the body and removes this line.
pub fn to_node<T: serde::Serialize + ?Sized>(v: &T, opts: &Options) -> Result<Document, Error> {
    todo!("Task 9")
}

/// Go's `node.Decode(&v)` for node `at` of `doc`.
#[allow(unused_variables)] // Task 8 writes the body and removes this line.
pub fn from_node<T: serde::de::DeserializeOwned>(
    doc: &Document,
    at: NodeId,
    opts: &Options,
) -> Result<T, Error> {
    todo!("Task 8")
}

/// Go's `node.Decode(&v)` keeping Go's partly decoded value next to its `*TypeError`.
#[allow(unused_variables)] // Task 8 writes the body and removes this line.
pub fn from_node_partial<T: serde::de::DeserializeOwned + Default>(
    doc: &Document,
    at: NodeId,
    opts: &Options,
) -> Result<(T, Option<TypeError>), Error> {
    todo!("Task 8")
}

/// Go's `yaml.Encoder`: writes YAML documents to a stream, separated by `---`.
pub struct Encoder<'w> {
    encoder: encode::Encoder<'w>,
}

impl<'w> Encoder<'w> {
    /// Go's `yaml.NewEncoder(w)`. Pass `&mut writer` to keep using the writer after
    /// [`Encoder::close`].
    pub fn new<W: std::io::Write + 'w>(w: W) -> Self {
        // Go: yaml.go NewEncoder
        Encoder {
            encoder: encode::new_encoder_with_writer(Box::new(w)),
        }
    }

    /// Go's `Encoder.SetIndent`. Go's negative-indent panic is unrepresentable; 0 means the
    /// default 4 (`encode.go:60-63`) and values outside 2..=9 fall back to 2 in the emitter
    /// (`apic.go` `yaml_emitter_set_indent`), as in Go.
    pub fn set_indent(&mut self, spaces: usize) {
        // Go: yaml.go SetIndent
        self.encoder.indent = spaces;
    }

    /// Go's `Encoder.Encode(v)` for a serde value.
    #[allow(unused_variables)] // Task 9 writes the body and removes this line.
    pub fn encode<T: serde::Serialize + ?Sized>(
        &mut self,
        v: &T,
        opts: &Options,
    ) -> Result<(), Error> {
        todo!("Task 9")
    }

    /// Go's `Encoder.Encode(&node)` for node `at` of `doc`.
    pub fn encode_node(&mut self, doc: &Document, at: NodeId) -> Result<(), Error> {
        // Go: yaml.go Encode
        self.encoder.marshal_doc_node(doc, at)
    }

    /// Go's `Encoder.Close`: writes any remaining data (no `...` terminator). Closing an
    /// encoder that encoded nothing fails with Go's `yaml: expected STREAM-START`.
    pub fn close(mut self) -> Result<(), Error> {
        // Go: yaml.go Close
        self.encoder.finish()
    }
}

/// Go's `yaml.Decoder`: reads the documents of a stream one by one.
pub struct Decoder<'r> {
    parser: node::Parser<'r>,
}

impl<'r> Decoder<'r> {
    /// Go's `yaml.NewDecoder(r)`. The decoder buffers and may read beyond the documents
    /// requested.
    pub fn new<R: std::io::Read + 'r>(r: R) -> Self {
        // Go: yaml.go NewDecoder
        Decoder {
            parser: node::new_parser_from_reader(r),
        }
    }

    /// Go's `Decoder.Decode(&node)`: the next document node; `None` is Go's `io.EOF`.
    pub fn decode_node(&mut self) -> Result<Option<Document>, Error> {
        // Go: yaml.go (*Decoder).Decode (out = *Node: `d.unmarshal` copies the document node)
        Ok(self.parser.parse()?.map(|n| self.parser.arena.subtree(n)))
    }

    /// Go's `Decoder.Decode(&v)` for a serde type; `None` is Go's `io.EOF`.
    #[allow(unused_variables)] // Task 8 writes the body and removes this line.
    pub fn decode<T: serde::de::DeserializeOwned>(
        &mut self,
        opts: &Options,
    ) -> Result<Option<T>, Error> {
        todo!("Task 8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_for_follows_go_default_naming() {
        let opts = Options::default();
        assert_eq!(opts.key_for("Agent", "name"), "name");
        assert_eq!(
            opts.key_for("Agent", "user_message_index"),
            "usermessageindex"
        );
        assert_eq!(opts.key_for("Part", "mediaResolution"), "mediaresolution");
        assert_eq!(opts.key_for("T", "ÀB_c"), "àbc");
    }

    #[test]
    fn key_for_verbatim_and_overrides() {
        let mut opts = Options::default();
        opts.verbatim_structs.insert("Config");
        opts.field_keys
            .insert(("Config", "sub_agents"), "subAgents");
        opts.field_keys.insert(("Other", "a_b"), "x");
        assert_eq!(opts.key_for("Config", "agent_class"), "agent_class");
        assert_eq!(opts.key_for("Config", "sub_agents"), "subAgents");
        assert_eq!(opts.key_for("Other", "a_b"), "x");
        assert_eq!(opts.key_for("Other", "c_d"), "cd");
        assert!(matches!(opts.key_for("Config", "x"), Cow::Borrowed(_)));
    }
}
