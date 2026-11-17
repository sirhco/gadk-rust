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

//! The encoder's event layer: the `Encoder` (emitter setup, `init`, `finish`, `emit`, `must`),
//! the scalar writers shared with `ser.rs` (`stringv`, `boolv`, `intv`, `uintv`, `timev`,
//! `floatv`, `nilv`, `emit_scalar`), the collection brackets (`mappingv`, `slicev`) and the node
//! encoder (`nodev`, `node`). The dispatch over serde values (`marshal`, `mapv`, `structv`) is
//! `ser.rs`; the base-60 float and old-style bool checks live in `resolve.rs` beside the
//! resolver.

use std::io::Write;

use chrono::{DateTime, FixedOffset};

use crate::api::{
    yaml_alias_event_initialize, yaml_document_end_event_initialize,
    yaml_document_start_event_initialize, yaml_emitter_delete, yaml_emitter_initialize,
    yaml_emitter_set_output_string, yaml_emitter_set_output_writer, yaml_emitter_set_unicode,
    yaml_mapping_end_event_initialize, yaml_mapping_start_event_initialize,
    yaml_scalar_event_initialize, yaml_sequence_end_event_initialize,
    yaml_sequence_start_event_initialize, yaml_stream_end_event_initialize,
    yaml_stream_start_event_initialize,
};
use crate::emitter::yaml_emitter_emit;
use crate::error::{Error, MAX_WALK_DEPTH, failf, walk_depth_exceeded};
use crate::node::{Document, Kind, NodeData, NodeId, Style};
use crate::resolve::{
    BINARY_TAG, MAP_TAG, SEQ_TAG, STR_TAG, encode_base64, is_base60_float, is_old_bool, long_tag,
    resolve, short_tag,
};
use crate::yamlh::{
    YamlEmitter, YamlEncoding, YamlEvent, YamlMappingStyle, YamlScalarStyle, YamlSequenceStyle,
};

/// The encoder state. The string output is `emitter.output_buffer`.
pub(crate) struct Encoder<'w> {
    /// The libyaml emitter.
    pub(crate) emitter: YamlEmitter<'w>,
    /// The event being emitted.
    pub(crate) event: YamlEvent,
    /// Emit the next collection in flow style (yaml.v3's `,flow` option), and quote multi-line
    /// strings.
    pub(crate) flow: bool,
    /// Indentation (0 = the default 4; never negative).
    pub(crate) indent: usize,
    /// Whether `init` ran (the STREAM-START event was emitted).
    pub(crate) done_init: bool,
    /// Current collection nesting of the recursive walk (limited by `MAX_WALK_DEPTH`).
    pub(crate) depth: usize,
}

/// An encoder around an initialized emitter (initialized with `yaml_emitter_initialize`).
fn blank_encoder<'w>() -> Encoder<'w> {
    let mut e = Encoder {
        emitter: YamlEmitter::default(),
        event: YamlEvent::default(),
        flow: false,
        indent: 0,
        done_init: false,
        depth: 0,
    };
    yaml_emitter_initialize(&mut e.emitter);
    e
}

/// An encoder with output to `emitter.output_buffer`, unicode allowed.
pub(crate) fn new_encoder() -> Encoder<'static> {
    let mut e = blank_encoder();
    yaml_emitter_set_output_string(&mut e.emitter);
    yaml_emitter_set_unicode(&mut e.emitter, true);
    e
}

/// An encoder with output to `w`, unicode allowed.
pub(crate) fn new_encoder_with_writer<'w>(w: Box<dyn Write + 'w>) -> Encoder<'w> {
    let mut e = blank_encoder();
    yaml_emitter_set_output_writer(&mut e.emitter, w);
    yaml_emitter_set_unicode(&mut e.emitter, true);
    e
}

impl<'w> Encoder<'w> {
    /// Initializes once: default indent 4, STREAM-START.
    pub(crate) fn init(&mut self) -> Result<(), Error> {
        if self.done_init {
            return Ok(());
        }
        if self.indent == 0 {
            self.indent = 4;
        }
        // Values outside 2..=9 become 2 in yaml_emitter_emit_stream_start.
        self.emitter.best_indent = isize::try_from(self.indent).unwrap_or(isize::MAX);
        yaml_stream_start_event_initialize(&mut self.event, YamlEncoding::Utf8);
        self.emit()?;
        self.done_init = true;
        Ok(())
    }

    /// Emits STREAM-END (flushes the output).
    pub(crate) fn finish(&mut self) -> Result<(), Error> {
        self.emitter.open_ended = false;
        yaml_stream_end_event_initialize(&mut self.event);
        self.emit()
    }

    /// Releases the emitter.
    pub(crate) fn destroy(&mut self) {
        yaml_emitter_delete(&mut self.emitter);
    }

    /// Emits `self.event`.
    pub(crate) fn emit(&mut self) -> Result<(), Error> {
        // This will internally delete the self.event value.
        let ok = yaml_emitter_emit(&mut self.emitter, &mut self.event);
        self.must(ok)
    }

    /// `Ok` when `ok`, else `failf("%s", emitter.problem)` (or
    /// `unknown problem generating YAML content`).
    pub(crate) fn must(&mut self, ok: bool) -> Result<(), Error> {
        if !ok {
            let mut msg = self.emitter.problem.as_str();
            if msg.is_empty() {
                msg = "unknown problem generating YAML content";
            }
            return Err(failf(msg));
        }
        Ok(())
    }

    /// Encodes a node as a document: a `Document` node is encoded as is, any
    /// other node inside an implicit document (`e.init()` included).
    pub(crate) fn marshal_doc_node(&mut self, doc: &Document, at: NodeId) -> Result<(), Error> {
        self.init()?;
        if doc[at].kind == Kind::Document {
            self.nodev(doc, at)
        } else {
            yaml_document_start_event_initialize(&mut self.event, None, Vec::new(), true);
            self.emit()?;
            // A node goes straight to nodev.
            self.nodev(doc, at)?;
            yaml_document_end_event_initialize(&mut self.event, true);
            self.emit()
        }
    }

    /// MAPPING-START (flow if `self.flow`, which it resets),
    /// `f`, MAPPING-END.
    pub(crate) fn mappingv(
        &mut self,
        tag: &str,
        f: impl FnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.mappingv_start(tag)?;
        f(self)?;
        self.mappingv_end()
    }

    /// The MAPPING-START half of [`Encoder::mappingv`] (serde's streaming `serialize_map`).
    pub(crate) fn mappingv_start(&mut self, tag: &str) -> Result<(), Error> {
        let implicit = tag.is_empty();
        let mut style = YamlMappingStyle::Block;
        if self.flow {
            self.flow = false;
            style = YamlMappingStyle::Flow;
        }
        yaml_mapping_start_event_initialize(
            &mut self.event,
            Vec::new(),
            tag.as_bytes().to_vec(),
            implicit,
            style,
        );
        self.emit()
    }

    /// The MAPPING-END half of [`Encoder::mappingv`].
    pub(crate) fn mappingv_end(&mut self) -> Result<(), Error> {
        yaml_mapping_end_event_initialize(&mut self.event);
        self.emit()
    }

    /// The SEQUENCE-START half of a sequence (flow if `self.flow`, which it
    /// resets).
    pub(crate) fn slicev_start(&mut self, tag: &str) -> Result<(), Error> {
        let implicit = tag.is_empty();
        let mut style = YamlSequenceStyle::Block;
        if self.flow {
            self.flow = false;
            style = YamlSequenceStyle::Flow;
        }
        let ok = yaml_sequence_start_event_initialize(
            &mut self.event,
            Vec::new(),
            tag.as_bytes().to_vec(),
            implicit,
            style,
        );
        self.must(ok)?;
        self.emit()
    }

    /// The SEQUENCE-END half of a sequence.
    pub(crate) fn slicev_end(&mut self) -> Result<(), Error> {
        let ok = yaml_sequence_end_event_initialize(&mut self.event);
        self.must(ok)?;
        self.emit()
    }

    /// Writes a string: `s` is any bytes; `!!binary` base64 when not UTF-8, else plain / literal /
    /// double-quoted by yaml.v3's rules. `tag` is a short tag or empty.
    pub(crate) fn stringv(&mut self, tag: &str, s: &[u8]) -> Result<(), Error> {
        let mut tag = tag;
        let text: String;
        let mut can_use_plain = true;
        match std::str::from_utf8(s) {
            Err(_) => {
                if tag == BINARY_TAG {
                    return Err(failf(
                        "explicitly tagged !!binary data must be base64-encoded",
                    ));
                }
                if !tag.is_empty() {
                    return Err(failf(format_args!(
                        "cannot marshal invalid UTF-8 data as {}",
                        short_tag(tag)
                    )));
                }
                // It can't be encoded directly as YAML so use a binary tag
                // and encode it as base64.
                tag = BINARY_TAG;
                text = encode_base64(s);
            }
            Ok(st) => {
                if tag.is_empty() {
                    // Check to see if it would resolve to a specific
                    // tag when encoded unquoted. If it doesn't,
                    // there's no need to quote it.
                    let (rtag, _) = resolve("", st)?;
                    can_use_plain = rtag == STR_TAG && !(is_base60_float(st) || is_old_bool(st));
                }
                text = st.to_owned();
            }
        }
        // Note: it's possible for user code to emit invalid YAML
        // if they explicitly specify a tag and a string containing
        // text that's incompatible with that tag.
        let style = if text.contains('\n') {
            if self.flow {
                YamlScalarStyle::DoubleQuoted
            } else {
                YamlScalarStyle::Literal
            }
        } else if can_use_plain {
            YamlScalarStyle::Plain
        } else {
            YamlScalarStyle::DoubleQuoted
        };
        self.emit_scalar(text.as_bytes(), b"", tag, style, b"", b"", b"", b"")
    }

    /// Writes a bool.
    pub(crate) fn boolv(&mut self, tag: &str, b: bool) -> Result<(), Error> {
        let s: &[u8] = if b { b"true" } else { b"false" };
        self.emit_scalar(s, b"", tag, YamlScalarStyle::Plain, b"", b"", b"", b"")
    }

    /// Writes a signed integer.
    pub(crate) fn intv(&mut self, tag: &str, i: i64) -> Result<(), Error> {
        let s = i.to_string();
        self.emit_scalar(
            s.as_bytes(),
            b"",
            tag,
            YamlScalarStyle::Plain,
            b"",
            b"",
            b"",
            b"",
        )
    }

    /// Writes an unsigned integer.
    pub(crate) fn uintv(&mut self, tag: &str, u: u64) -> Result<(), Error> {
        let s = u.to_string();
        self.emit_scalar(
            s.as_bytes(),
            b"",
            tag,
            YamlScalarStyle::Plain,
            b"",
            b"",
            b"",
            b"",
        )
    }

    /// Writes a timestamp as RFC3339Nano text.
    pub(crate) fn timev(&mut self, tag: &str, t: &DateTime<FixedOffset>) -> Result<(), Error> {
        let s = gadk_platform::go_format_rfc3339_nano(t);
        self.emit_scalar(
            s.as_bytes(),
            b"",
            tag,
            YamlScalarStyle::Plain,
            b"",
            b"",
            b"",
            b"",
        )
    }

    /// Writes a float: the shortest representation that round-trips at `precision`, formatted
    /// like `%g`, with `.inf`/`-.inf`/`.nan`; `precision` is 32 for
    /// `float32` (value passed widened) or 64.
    pub(crate) fn floatv(&mut self, tag: &str, f: f64, precision: u32) -> Result<(), Error> {
        // Issue #352: When formatting, use the precision of the underlying value
        let mut s = gadk_platform::go_format_float_g(f, precision);
        match s.as_str() {
            "+Inf" => s = ".inf".to_owned(),
            "-Inf" => s = "-.inf".to_owned(),
            "NaN" => s = ".nan".to_owned(),
            _ => {}
        }
        self.emit_scalar(
            s.as_bytes(),
            b"",
            tag,
            YamlScalarStyle::Plain,
            b"",
            b"",
            b"",
            b"",
        )
    }

    /// Writes a plain `null`.
    pub(crate) fn nilv(&mut self) -> Result<(), Error> {
        self.emit_scalar(b"null", b"", "", YamlScalarStyle::Plain, b"", b"", b"", b"")
    }

    /// Emits a SCALAR event.
    #[allow(clippy::too_many_arguments)] // One argument per scalar event field.
    pub(crate) fn emit_scalar(
        &mut self,
        value: &[u8],
        anchor: &[u8],
        tag: &str,
        style: YamlScalarStyle,
        head: &[u8],
        line: &[u8],
        foot: &[u8],
        tail: &[u8],
    ) -> Result<(), Error> {
        // TODO: kill this function; replace all initialize calls by their underlying struct
        // literals.
        let implicit = tag.is_empty();
        let tag = if implicit {
            String::new()
        } else {
            long_tag(tag)
        };
        let ok = yaml_scalar_event_initialize(
            &mut self.event,
            anchor.to_vec(),
            tag.into_bytes(),
            value.to_vec(),
            implicit,
            implicit,
            style,
        );
        self.must(ok)?;
        self.event.head_comment = head.to_vec();
        self.event.line_comment = line.to_vec();
        self.event.foot_comment = foot.to_vec();
        self.event.tail_comment = tail.to_vec();
        self.emit()
    }

    /// Encodes node `at` of `doc`.
    pub(crate) fn nodev(&mut self, doc: &Document, at: NodeId) -> Result<(), Error> {
        self.node(doc, &doc[at], "")
    }

    /// Encodes `node` with the comment `tail`. `node` is passed by value so mapping keys can be
    /// encoded from a copy with the foot comment cleared; its `content` and
    /// `alias` refer to `doc`.
    pub(crate) fn node(
        &mut self,
        doc: &Document,
        node: &NodeData,
        tail: &str,
    ) -> Result<(), Error> {
        // Collections nested deeper than MAX_WALK_DEPTH fail instead of
        // overflowing the stack.
        let nests = matches!(node.kind, Kind::Sequence | Kind::Mapping);
        if nests {
            if self.depth >= MAX_WALK_DEPTH {
                return Err(walk_depth_exceeded());
            }
            self.depth += 1;
        }
        let result = self.node_unguarded(doc, node, tail);
        if nests {
            self.depth -= 1;
        }
        result
    }

    /// The body of [`Encoder::node`], which wraps it with the depth guard.
    fn node_unguarded(&mut self, doc: &Document, node: &NodeData, tail: &str) -> Result<(), Error> {
        // Zero nodes behave as nil.
        if node.kind == Kind::Zero && node.is_zero() {
            return self.nilv();
        }

        // If the tag was not explicitly requested, and dropping it won't change the
        // implicit tag of the value, don't include it in the presentation.
        let mut tag: &str = &node.tag;
        let stag = short_tag(tag);
        let mut force_quoting = false;
        if !tag.is_empty() && !node.style.intersects(Style::TAGGED) {
            if node.kind == Kind::Scalar {
                if stag == STR_TAG
                    && node.style.intersects(
                        Style::SINGLE_QUOTED
                            | Style::DOUBLE_QUOTED
                            | Style::LITERAL
                            | Style::FOLDED,
                    )
                {
                    tag = "";
                } else {
                    // An empty tag never fails to resolve.
                    let (rtag, _) = resolve("", &node.value)?;
                    if rtag == stag {
                        tag = "";
                    } else if stag == STR_TAG {
                        tag = "";
                        force_quoting = true;
                    }
                }
            } else {
                let rtag = match node.kind {
                    Kind::Mapping => MAP_TAG,
                    Kind::Sequence => SEQ_TAG,
                    _ => "",
                };
                if rtag == stag {
                    tag = "";
                }
            }
        }

        match node.kind {
            Kind::Document => {
                yaml_document_start_event_initialize(&mut self.event, None, Vec::new(), true);
                self.event.head_comment = node.head_comment.as_bytes().to_vec();
                self.emit()?;
                for &child in &node.content {
                    self.node(doc, &doc[child], "")?;
                }
                yaml_document_end_event_initialize(&mut self.event, true);
                self.event.foot_comment = node.foot_comment.as_bytes().to_vec();
                self.emit()?;
            }

            Kind::Sequence => {
                let mut style = YamlSequenceStyle::Block;
                if node.style.intersects(Style::FLOW) {
                    style = YamlSequenceStyle::Flow;
                }
                let ok = yaml_sequence_start_event_initialize(
                    &mut self.event,
                    node.anchor.as_bytes().to_vec(),
                    long_tag(tag).into_bytes(),
                    tag.is_empty(),
                    style,
                );
                self.must(ok)?;
                self.event.head_comment = node.head_comment.as_bytes().to_vec();
                self.emit()?;
                for &child in &node.content {
                    self.node(doc, &doc[child], "")?;
                }
                let ok = yaml_sequence_end_event_initialize(&mut self.event);
                self.must(ok)?;
                self.event.line_comment = node.line_comment.as_bytes().to_vec();
                self.event.foot_comment = node.foot_comment.as_bytes().to_vec();
                self.emit()?;
            }

            Kind::Mapping => {
                let mut style = YamlMappingStyle::Block;
                if node.style.intersects(Style::FLOW) {
                    style = YamlMappingStyle::Flow;
                }
                yaml_mapping_start_event_initialize(
                    &mut self.event,
                    node.anchor.as_bytes().to_vec(),
                    long_tag(tag).into_bytes(),
                    tag.is_empty(),
                    style,
                );
                self.event.tail_comment = tail.as_bytes().to_vec();
                self.event.head_comment = node.head_comment.as_bytes().to_vec();
                self.emit()?;

                // The tail logic below moves the foot comment of prior keys to the following key,
                // since the value for each key may be a nested structure and the foot needs to be
                // processed only the entirety of the value is streamed. The last tail is processed
                // with the mapping end event.
                let mut tail = String::new();
                let mut i = 0;
                while i + 1 < node.content.len() {
                    let k = &doc[node.content[i]];
                    let foot = k.foot_comment.clone();
                    if foot.is_empty() {
                        self.node(doc, k, &tail)?;
                    } else {
                        let mut kopy = k.clone();
                        kopy.foot_comment = String::new();
                        self.node(doc, &kopy, &tail)?;
                    }
                    tail = foot;

                    let v = &doc[node.content[i + 1]];
                    self.node(doc, v, "")?;
                    i += 2;
                }

                yaml_mapping_end_event_initialize(&mut self.event);
                self.event.tail_comment = tail.into_bytes();
                self.event.line_comment = node.line_comment.as_bytes().to_vec();
                self.event.foot_comment = node.foot_comment.as_bytes().to_vec();
                self.emit()?;
            }

            Kind::Alias => {
                yaml_alias_event_initialize(&mut self.event, node.value.as_bytes().to_vec());
                self.event.head_comment = node.head_comment.as_bytes().to_vec();
                self.event.line_comment = node.line_comment.as_bytes().to_vec();
                self.event.foot_comment = node.foot_comment.as_bytes().to_vec();
                self.emit()?;
            }

            Kind::Scalar => {
                // There is no invalid-UTF-8 branch (an error for `!!binary` / other explicit
                // tags, else `!!binary` plus base64): `NodeData::value` is a Rust `String`,
                // always valid UTF-8.
                let value = &node.value;

                let style = if node.style.intersects(Style::DOUBLE_QUOTED) {
                    YamlScalarStyle::DoubleQuoted
                } else if node.style.intersects(Style::SINGLE_QUOTED) {
                    YamlScalarStyle::SingleQuoted
                } else if node.style.intersects(Style::LITERAL) {
                    YamlScalarStyle::Literal
                } else if node.style.intersects(Style::FOLDED) {
                    YamlScalarStyle::Folded
                } else if value.contains('\n') {
                    YamlScalarStyle::Literal
                } else if force_quoting {
                    YamlScalarStyle::DoubleQuoted
                } else {
                    YamlScalarStyle::Plain
                };

                self.emit_scalar(
                    value.as_bytes(),
                    node.anchor.as_bytes(),
                    tag,
                    style,
                    node.head_comment.as_bytes(),
                    node.line_comment.as_bytes(),
                    node.foot_comment.as_bytes(),
                    tail.as_bytes(),
                )?;
            }

            Kind::Zero => {
                return Err(failf(format_args!(
                    "cannot encode node with unknown kind {}",
                    node.kind.bits()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go's `yaml.Marshal` for an encoder driven by `f` inside one implicit document.
    fn marshal_with(f: impl FnOnce(&mut Encoder<'static>) -> Result<(), Error>) -> String {
        try_marshal_with(f).unwrap()
    }

    fn try_marshal_with(
        f: impl FnOnce(&mut Encoder<'static>) -> Result<(), Error>,
    ) -> Result<String, Error> {
        let mut e = new_encoder();
        e.init()?;
        yaml_document_start_event_initialize(&mut e.event, None, Vec::new(), true);
        e.emit()?;
        f(&mut e)?;
        yaml_document_end_event_initialize(&mut e.event, true);
        e.emit()?;
        e.finish()?;
        let out = std::mem::take(&mut e.emitter.output_buffer);
        e.destroy();
        Ok(String::from_utf8(out).unwrap())
    }

    /// Go's `yaml.Marshal(&node)` for node `at` of `doc`.
    fn marshal_node(doc: &Document, at: NodeId) -> Result<String, Error> {
        let mut e = new_encoder();
        e.marshal_doc_node(doc, at)?;
        e.finish()?;
        Ok(String::from_utf8(std::mem::take(&mut e.emitter.output_buffer)).unwrap())
    }

    fn scalar(tag: &str, value: &str, style: Style) -> NodeData {
        NodeData {
            kind: Kind::Scalar,
            tag: tag.to_owned(),
            value: value.to_owned(),
            style,
            ..NodeData::default()
        }
    }

    #[test]
    fn stringv_styles_follow_go() {
        // Plain when the text resolves to !!str and is no YAML 1.1 bool / base-60 float.
        assert_eq!(marshal_with(|e| e.stringv("", b"hello")), "hello\n");
        // Resolves to another tag: double-quoted.
        assert_eq!(marshal_with(|e| e.stringv("", b"true")), "\"true\"\n");
        assert_eq!(marshal_with(|e| e.stringv("", b"12")), "\"12\"\n");
        assert_eq!(marshal_with(|e| e.stringv("", b"")), "\"\"\n");
        // isOldBool / isBase60Float.
        assert_eq!(marshal_with(|e| e.stringv("", b"yes")), "\"yes\"\n");
        assert_eq!(marshal_with(|e| e.stringv("", b"1:20")), "\"1:20\"\n");
        // A newline: literal, or double-quoted in flow.
        assert_eq!(
            marshal_with(|e| e.stringv("", b"a\nb")),
            "|-\n    a\n    b\n"
        );
        assert_eq!(
            marshal_with(|e| {
                e.flow = true;
                e.stringv("", b"a\nb")
            }),
            "\"a\\nb\"\n"
        );
        // An explicit tag is kept and skips the resolve check.
        assert_eq!(marshal_with(|e| e.stringv("!!str", b"12")), "!!str 12\n");
    }

    #[test]
    fn stringv_invalid_utf8() {
        assert_eq!(
            marshal_with(|e| e.stringv("", b"\xff\xfe")),
            "!!binary //4=\n"
        );
        assert_eq!(
            try_marshal_with(|e| e.stringv("!!binary", b"\xff"))
                .unwrap_err()
                .to_string(),
            "yaml: explicitly tagged !!binary data must be base64-encoded"
        );
        assert_eq!(
            try_marshal_with(|e| e.stringv("!!str", b"\xff"))
                .unwrap_err()
                .to_string(),
            "yaml: cannot marshal invalid UTF-8 data as !!str"
        );
    }

    #[test]
    fn scalar_writers() {
        assert_eq!(marshal_with(|e| e.boolv("", true)), "true\n");
        assert_eq!(marshal_with(|e| e.boolv("", false)), "false\n");
        assert_eq!(marshal_with(|e| e.intv("", -42)), "-42\n");
        assert_eq!(
            marshal_with(|e| e.uintv("", u64::MAX)),
            "18446744073709551615\n"
        );
        assert_eq!(marshal_with(|e| e.floatv("", 1.5, 64)), "1.5\n");
        assert_eq!(marshal_with(|e| e.floatv("", 1e21, 64)), "1e+21\n");
        assert_eq!(
            marshal_with(|e| e.floatv("", f64::from(0.1f32), 32)),
            "0.1\n"
        );
        assert_eq!(marshal_with(|e| e.floatv("", f64::INFINITY, 64)), ".inf\n");
        assert_eq!(
            marshal_with(|e| e.floatv("", f64::NEG_INFINITY, 64)),
            "-.inf\n"
        );
        assert_eq!(marshal_with(|e| e.floatv("", f64::NAN, 64)), ".nan\n");
        assert_eq!(marshal_with(|e| e.nilv()), "null\n");
        let t = DateTime::parse_from_rfc3339("2001-12-14T21:59:43.1-05:00").unwrap();
        assert_eq!(
            marshal_with(|e| e.timev("", &t)),
            "2001-12-14T21:59:43.1-05:00\n"
        );
        // A tag is written in its short form.
        assert_eq!(marshal_with(|e| e.intv("!!str", 1)), "!!str 1\n");
    }

    #[test]
    fn collection_brackets() {
        let out = marshal_with(|e| {
            e.mappingv("", |e| {
                e.stringv("", b"a")?;
                e.slicev_start("")?;
                e.intv("", 1)?;
                e.intv("", 2)?;
                e.slicev_end()?;
                e.stringv("", b"b")?;
                e.flow = true;
                e.mappingv_start("")?;
                e.stringv("", b"c")?;
                e.nilv()?;
                e.mappingv_end()
            })
        });
        assert_eq!(out, "a:\n    - 1\n    - 2\nb: {c: null}\n");
        // `flow` applies to the next collection only.
        let out = marshal_with(|e| {
            e.flow = true;
            e.slicev_start("")?;
            assert!(!e.flow);
            e.intv("", 1)?;
            e.slicev_end()
        });
        assert_eq!(out, "[1]\n");
    }

    #[test]
    fn indent_default_and_set() {
        let doc = {
            let mut d = Document::new(NodeData {
                kind: Kind::Mapping,
                ..NodeData::default()
            });
            let k = d.push(scalar("", "a", Style::empty()));
            let v = d.push(NodeData {
                kind: Kind::Sequence,
                ..NodeData::default()
            });
            let one = d.push(scalar("", "1", Style::empty()));
            d[v].content.push(one);
            let root = d.root();
            d[root].content = vec![k, v];
            d
        };
        assert_eq!(marshal_node(&doc, doc.root()).unwrap(), "a:\n    - 1\n");
        for (indent, want) in [
            (2, "a:\n  - 1\n"),
            (3, "a:\n   - 1\n"),
            // Outside 2..=9: the emitter falls back to 2.
            (1, "a:\n  - 1\n"),
            (10, "a:\n  - 1\n"),
        ] {
            let mut e = new_encoder();
            e.indent = indent;
            e.marshal_doc_node(&doc, doc.root()).unwrap();
            e.finish().unwrap();
            assert_eq!(
                String::from_utf8(std::mem::take(&mut e.emitter.output_buffer)).unwrap(),
                want,
                "indent {indent}"
            );
        }
    }

    #[test]
    fn zero_node_is_null() {
        let doc = Document::default();
        assert_eq!(marshal_node(&doc, doc.root()).unwrap(), "null\n");
    }

    #[test]
    fn unknown_kind_fails() {
        let doc = Document::new(NodeData {
            value: "x".to_owned(),
            ..NodeData::default()
        });
        assert_eq!(
            marshal_node(&doc, doc.root()).unwrap_err().to_string(),
            "yaml: cannot encode node with unknown kind 0"
        );
    }

    #[test]
    fn scalar_tags_dropped_or_kept_like_go() {
        for (tag, value, style, want) in [
            // Resolves to the same tag: dropped.
            ("!!int", "1", Style::empty(), "1\n"),
            ("tag:yaml.org,2002:int", "1", Style::empty(), "1\n"),
            // !!str that would resolve otherwise: dropped, forced to double quotes.
            ("!!str", "1", Style::empty(), "\"1\"\n"),
            // !!str with a quoting style: dropped.
            ("!!str", "1", Style::SINGLE_QUOTED, "'1'\n"),
            // A tag the text does not resolve to: kept.
            ("!!int", "a", Style::empty(), "!!int a\n"),
            ("!foo", "a", Style::empty(), "!foo a\n"),
            // TaggedStyle keeps the tag.
            ("!!int", "1", Style::TAGGED, "!!int 1\n"),
            // Multi-line text: literal.
            ("", "a\nb", Style::empty(), "|-\n    a\n    b\n"),
            ("", "a", Style::FOLDED, ">-\n    a\n"),
        ] {
            let doc = Document::new(scalar(tag, value, style));
            assert_eq!(
                marshal_node(&doc, doc.root()).unwrap(),
                want,
                "{tag} {value:?}"
            );
        }
    }

    #[test]
    fn collection_tags_and_aliases() {
        let mut doc = Document::new(NodeData {
            kind: Kind::Sequence,
            tag: "!!seq".to_owned(),
            ..NodeData::default()
        });
        let m = doc.push(NodeData {
            kind: Kind::Mapping,
            tag: "!!map".to_owned(),
            anchor: "a".to_owned(),
            style: Style::FLOW,
            ..NodeData::default()
        });
        let k = doc.push(scalar("", "k", Style::empty()));
        let v = doc.push(scalar("", "v", Style::empty()));
        doc[m].content = vec![k, v];
        let al = doc.push(NodeData {
            kind: Kind::Alias,
            value: "a".to_owned(),
            alias: Some(m),
            ..NodeData::default()
        });
        let s = doc.push(NodeData {
            kind: Kind::Sequence,
            tag: "!custom".to_owned(),
            ..NodeData::default()
        });
        let root = doc.root();
        doc[root].content = vec![m, al, s];
        assert_eq!(
            marshal_node(&doc, doc.root()).unwrap(),
            "- &a {k: v}\n- *a\n- !custom []\n"
        );
    }

    #[test]
    fn key_foot_comment_moves_to_the_next_key() {
        // Go: the foot comment of a key is emitted as the tail of the following key.
        let mut doc = Document::new(NodeData {
            kind: Kind::Mapping,
            ..NodeData::default()
        });
        let k1 = doc.push(NodeData {
            foot_comment: "# foot".to_owned(),
            ..scalar("", "a", Style::empty())
        });
        let v1 = doc.push(scalar("", "1", Style::empty()));
        let k2 = doc.push(scalar("", "b", Style::empty()));
        let v2 = doc.push(scalar("", "2", Style::empty()));
        let root = doc.root();
        doc[root].content = vec![k1, v1, k2, v2];
        assert_eq!(
            marshal_node(&doc, doc.root()).unwrap(),
            "a: 1\n# foot\n\nb: 2\n"
        );
        // The source node keeps its foot comment.
        assert_eq!(doc[k1].foot_comment, "# foot");
    }

    #[test]
    fn document_node_and_comments() {
        let mut doc = Document::new(NodeData {
            kind: Kind::Document,
            head_comment: "# head".to_owned(),
            foot_comment: "# foot".to_owned(),
            ..NodeData::default()
        });
        let s = doc.push(NodeData {
            line_comment: "# line".to_owned(),
            ..scalar("", "x", Style::empty())
        });
        let root = doc.root();
        doc[root].content = vec![s];
        assert_eq!(
            marshal_node(&doc, doc.root()).unwrap(),
            "# head\n\nx # line\n\n# foot\n"
        );
    }

    #[test]
    fn must_reports_problem_or_unknown() {
        let mut e = new_encoder();
        assert!(e.must(true).is_ok());
        assert_eq!(
            e.must(false).unwrap_err().to_string(),
            "yaml: unknown problem generating YAML content"
        );
        e.emitter.problem = "expected SCALAR".to_owned();
        assert_eq!(
            e.must(false).unwrap_err().to_string(),
            "yaml: expected SCALAR"
        );
    }

    #[test]
    fn finish_without_init_fails_like_go() {
        let mut e = new_encoder();
        assert_eq!(
            e.finish().unwrap_err().to_string(),
            "yaml: expected STREAM-START"
        );
    }

    #[test]
    fn writer_output_and_multiple_documents() {
        let mut out = Vec::new();
        {
            let mut e = new_encoder_with_writer(Box::new(&mut out));
            let doc = Document::new(scalar("", "a", Style::empty()));
            e.marshal_doc_node(&doc, doc.root()).unwrap();
            e.marshal_doc_node(&doc, doc.root()).unwrap();
            e.finish().unwrap();
        }
        assert_eq!(String::from_utf8(out).unwrap(), "a\n---\na\n");
    }

    #[test]
    fn root_plain_scalar_has_no_open_ended_marker() {
        // finish clears open_ended, so no `...` follows a root plain scalar.
        let doc = Document::new(scalar("", "plain", Style::empty()));
        assert_eq!(marshal_node(&doc, doc.root()).unwrap(), "plain\n");
    }

    #[test]
    fn walk_depth_is_capped_at_127_nested_collections() {
        // Plan decision D7: 127 nested flow sequences encode, 128 fail (Go has no limit).
        let ok = format!("{}{}", "[".repeat(127), "]".repeat(127));
        let doc = crate::unmarshal_node(ok.as_bytes()).unwrap();
        assert_eq!(marshal_node(&doc, doc.root()).unwrap(), format!("{ok}\n"));

        let deep = format!("{}{}", "[".repeat(128), "]".repeat(128));
        let doc = crate::unmarshal_node(deep.as_bytes()).unwrap();
        let err = marshal_node(&doc, doc.root()).unwrap_err();
        assert_eq!(err.to_string(), "yaml: exceeded max depth of 127");

        let deep_block = (0..128).fold(String::from("x\n"), |inner, _| {
            let indented: String = inner.lines().map(|l| format!("  {l}\n")).collect();
            format!("k:\n{indented}")
        });
        let doc = crate::unmarshal_node(deep_block.as_bytes()).unwrap();
        let err = marshal_node(&doc, doc.root()).unwrap_err();
        assert_eq!(err.to_string(), "yaml: exceeded max depth of 127");
    }
}
