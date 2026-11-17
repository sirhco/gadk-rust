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

//! gadk-goyaml: a pure-Rust YAML library that reproduces `gopkg.in/yaml.v3` v3.0.1 byte for
//! byte, so gadk reads and writes YAML exactly as ADK does: the same node trees, error texts
//! and emitted bytes. This file is the public API.
//!
//! - [`unmarshal_node`] / [`marshal_node`]: parse to and emit from the [`Document`] node
//!   arena.
//! - [`unmarshal`] / [`marshal`] / [`from_node`] / [`to_node`]: serde values decoded and encoded
//!   by yaml.v3's reflection rules; struct keys come from [`Options::key_for`] (a struct
//!   variant's under its variant name). Untagged, internally tagged and adjacently tagged enums
//!   decode through serde's `deserialize_any` (raw YAML keys, no `key_for`), so their struct
//!   variants round-trip only with field names that already are default keys (lowercase, no
//!   `_`) or with the variant in [`Options::verbatim_structs`]; prefer externally tagged enums.
//! - [`Encoder`] / [`Decoder`]: multi-document streams (`SetIndent`, `Close`).
//! - [`GoValue`]: a dynamically typed YAML value; [`InlineMap`]: a catch-all map for the keys no
//!   other field matched.
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

/// How serde names map to YAML keys (yaml.v3 reads struct tags, which serde types do not carry).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Struct names (serde's `name` argument) whose field names are used verbatim (the
    /// equivalent of an explicit yaml tag).
    pub verbatim_structs: BTreeSet<&'static str>,
    /// (struct name, serde field name) → key, for fields whose YAML key differs from the default
    /// derived from the serde name.
    pub field_keys: BTreeMap<(&'static str, &'static str), &'static str>,
}

impl Options {
    /// The YAML key of field `field` of struct `struct_name`: the `field_keys` override, else the
    /// serde name verbatim if `struct_name` is in `verbatim_structs`, else the default: the serde
    /// name lowercased with `_` removed.
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

/// Parses `data` to a node tree: the document node of the first document (the zero node
/// for an empty stream).
pub fn unmarshal_node(data: &[u8]) -> Result<Document, Error> {
    let mut p = node::new_parser(data);
    let parsed = p.parse();
    p.destroy();
    Ok(match parsed? {
        Some(n) => p.arena.subtree(n),
        None => Document::default(),
    })
}

/// Emits node `at` of `doc` as YAML (the zero node, `Document::default()`, marshals to
/// `null\n`).
pub fn marshal_node(doc: &Document, at: NodeId) -> Result<Vec<u8>, Error> {
    let mut e = encode::new_encoder();
    let result = e.marshal_doc_node(doc, at).and_then(|()| e.finish());
    let out = std::mem::take(&mut e.emitter.output_buffer);
    e.destroy();
    result.map(|()| out)
}

/// Decodes the first document of `data` into a serde type by yaml.v3's rules (an empty stream
/// decodes the zero node, i.e. as null). Type errors are collected and returned together as
/// [`Error::Type`] (yaml.v3 also returns the partly decoded value; see [`from_node_partial`]).
pub fn unmarshal<T: serde::de::DeserializeOwned>(data: &[u8], opts: &Options) -> Result<T, Error> {
    let mut p = node::new_parser(data);
    let parsed = p.parse();
    p.destroy();
    match parsed? {
        Some(n) => de::from_node(&p.arena, n, opts),
        // An empty stream: serde builds the value from the zero node (null).
        None => {
            let zero = Document::default();
            de::from_node(&zero, zero.root(), opts)
        }
    }
}

/// Encodes a serde value as one YAML document.
pub fn marshal<T: serde::Serialize + ?Sized>(v: &T, opts: &Options) -> Result<Vec<u8>, Error> {
    let mut e = encode::new_encoder();
    let result = ser::marshal_doc(&mut e, v, opts).and_then(|()| e.finish());
    let out = std::mem::take(&mut e.emitter.output_buffer);
    e.destroy();
    result.map(|()| out)
}

/// Encodes a serde value as a node tree: `marshal`, then a `textless` re-parse; returns the
/// document's first content node as its own [`Document`].
pub fn to_node<T: serde::Serialize + ?Sized>(v: &T, opts: &Options) -> Result<Document, Error> {
    let out = marshal(v, opts)?;
    let mut p = node::new_parser(&out);
    p.textless = true;
    let parsed = p.parse();
    p.destroy();
    // `marshal` always writes one document, so the parse yields a document node with one
    // child, whose first content node is the result.
    Ok(parsed?
        .and_then(|doc| p.arena[doc].content.first().copied())
        .map(|first| p.arena.subtree(first))
        .unwrap_or_default())
}

/// Decodes node `at` of `doc` into a serde type: [`Error::Type`] when type errors were
/// collected.
pub fn from_node<T: serde::de::DeserializeOwned>(
    doc: &Document,
    at: NodeId,
    opts: &Options,
) -> Result<T, Error> {
    de::from_node(doc, at, opts)
}

/// Like [`from_node`], but keeps the partly decoded value next to its [`TypeError`]
/// (`T::default()` when the type error left serde no value, e.g. for an enum).
pub fn from_node_partial<T: serde::de::DeserializeOwned + Default>(
    doc: &Document,
    at: NodeId,
    opts: &Options,
) -> Result<(T, Option<TypeError>), Error> {
    de::from_node_partial(doc, at, opts)
}

/// Writes YAML documents to a stream, separated by `---`.
pub struct Encoder<'w> {
    encoder: encode::Encoder<'w>,
}

impl<'w> Encoder<'w> {
    /// An encoder writing to `w`. Pass `&mut writer` to keep using the writer after
    /// [`Encoder::close`].
    pub fn new<W: std::io::Write + 'w>(w: W) -> Self {
        Encoder {
            encoder: encode::new_encoder_with_writer(Box::new(w)),
        }
    }

    /// Sets the indent width. 0 means the default 4 and values outside 2..=9 fall back to 2 in
    /// the emitter; a negative indent is unrepresentable.
    pub fn set_indent(&mut self, spaces: usize) {
        self.encoder.indent = spaces;
    }

    /// Encodes a serde value as the next document.
    pub fn encode<T: serde::Serialize + ?Sized>(
        &mut self,
        v: &T,
        opts: &Options,
    ) -> Result<(), Error> {
        ser::marshal_doc(&mut self.encoder, v, opts)
    }

    /// Encodes node `at` of `doc` as the next document.
    pub fn encode_node(&mut self, doc: &Document, at: NodeId) -> Result<(), Error> {
        self.encoder.marshal_doc_node(doc, at)
    }

    /// Writes any remaining data (no `...` terminator). Closing an encoder that encoded nothing
    /// fails with `yaml: expected STREAM-START`.
    pub fn close(mut self) -> Result<(), Error> {
        self.encoder.finish()
    }
}

/// Reads the documents of a stream one by one.
pub struct Decoder<'r> {
    parser: node::Parser<'r>,
}

impl<'r> Decoder<'r> {
    /// A decoder reading from `r`. The decoder buffers and may read beyond the documents
    /// requested.
    pub fn new<R: std::io::Read + 'r>(r: R) -> Self {
        Decoder {
            parser: node::new_parser_from_reader(r),
        }
    }

    /// The next document node; `None` at the end of the stream.
    pub fn decode_node(&mut self) -> Result<Option<Document>, Error> {
        Ok(self.parser.parse()?.map(|n| self.parser.arena.subtree(n)))
    }

    /// The next document decoded into a serde type; `None` at the end of the stream. Each
    /// call decodes with fresh decoder state; anchors persist across the stream's documents.
    pub fn decode<T: serde::de::DeserializeOwned>(
        &mut self,
        opts: &Options,
    ) -> Result<Option<T>, Error> {
        match self.parser.parse()? {
            Some(n) => de::from_node(&self.parser.arena, n, opts).map(Some),
            None => Ok(None),
        }
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
