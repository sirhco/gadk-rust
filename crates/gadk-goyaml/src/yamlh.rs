// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
// Portions derived from libyaml, Copyright (c) 2006-2010 Kirill Simonov.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of
// this software and associated documentation files (the "Software"), to deal in
// the Software without restriction, including without limitation the rights to
// use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is furnished to do
// so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Port of yaml.v3 v3.0.1 `yamlh.go` (libyaml's data types: tokens, events, parser and
//! emitter state) and `yamlprivateh.go` (buffer sizes and character-class helpers).
//!
//! Naming: Go's `yaml_xxx_t` types are `YamlXxx` structs/enums; enum variants are Go's constant
//! names in CamelCase without the `yaml_` prefix and without the prefix/suffix every constant
//! of that type shares (`yaml_STREAM_START_TOKEN` → `YamlTokenType::StreamStart`,
//! `yaml_PARSE_BLOCK_NODE_STATE` → `YamlParserState::BlockNode`, `yaml_NO_ERROR` →
//! `YamlErrorType::No`). Go `[]byte` fields are `Vec<u8>` (nil and empty are not distinguished:
//! every nil check in yaml.v3 v3.0.1 is on a value that is never set empty). Go `int` positions,
//! counts and marks are `usize`; Go `int`s that can be negative (indentation levels, widths,
//! the emitter's line/column) are `isize`.
// Until Tasks 3–7 consume these items; Task 12 narrows this to the items that stay unused
// (yaml.v3 itself never reads the document/node types and several fields).
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Write};

// ---------------------------------------------------------------------------------------------
// yamlh.go
// ---------------------------------------------------------------------------------------------

/// Go: `yaml_version_directive_t` — the version directive data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlVersionDirective {
    /// The major version number.
    pub(crate) major: i8,
    /// The minor version number.
    pub(crate) minor: i8,
}

/// Go: `yaml_tag_directive_t` — the tag directive data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlTagDirective {
    /// The tag handle.
    pub(crate) handle: Vec<u8>,
    /// The tag prefix.
    pub(crate) prefix: Vec<u8>,
}

/// Go: `yaml_encoding_t` — the stream encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlEncoding {
    /// Let the parser choose the encoding.
    #[default]
    Any = 0,
    /// The default UTF-8 encoding.
    Utf8 = 1,
    /// The UTF-16-LE encoding with BOM.
    Utf16le = 2,
    /// The UTF-16-BE encoding with BOM.
    Utf16be = 3,
}

/// Go: `yaml_break_t` — line break types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlBreak {
    /// Let the parser choose the break type.
    #[default]
    Any = 0,
    /// Use CR for line breaks (Mac style).
    Cr = 1,
    /// Use LN for line breaks (Unix style).
    Ln = 2,
    /// Use CR LN for line breaks (DOS style).
    Crln = 3,
}

/// Go: `yaml_error_type_t` — many bad things could happen with the parser and emitter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlErrorType {
    /// No error is produced.
    #[default]
    No = 0,
    /// Cannot allocate or reallocate a block of memory.
    Memory = 1,
    /// Cannot read or decode the input stream.
    Reader = 2,
    /// Cannot scan the input stream.
    Scanner = 3,
    /// Cannot parse the input stream.
    Parser = 4,
    /// Cannot compose a YAML document.
    Composer = 5,
    /// Cannot write to the output stream.
    Writer = 6,
    /// Cannot emit a YAML stream.
    Emitter = 7,
}

/// Go: `yaml_mark_t` — the pointer position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlMark {
    /// The position index.
    pub(crate) index: usize,
    /// The position line.
    pub(crate) line: usize,
    /// The position column.
    pub(crate) column: usize,
}

// Node Styles

/// Go: `yaml_style_t` — the raw style stored in events; read it back through
/// [`YamlEvent::scalar_style`], [`YamlEvent::sequence_style`] or [`YamlEvent::mapping_style`].
pub(crate) type YamlStyle = i8;

/// Go: `yaml_scalar_style_t` — scalar styles (Go's values: `0` then `1 << iota` from 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlScalarStyle {
    /// Let the emitter choose the style.
    #[default]
    Any = 0,
    /// The plain scalar style.
    Plain = 2,
    /// The single-quoted scalar style.
    SingleQuoted = 4,
    /// The double-quoted scalar style.
    DoubleQuoted = 8,
    /// The literal scalar style.
    Literal = 16,
    /// The folded scalar style.
    Folded = 32,
}

impl YamlScalarStyle {
    /// Go's `yaml_scalar_style_t(style)` conversion (values other than the constants map to
    /// `Any`; yaml.v3 never stores any).
    pub(crate) fn from_style(style: YamlStyle) -> YamlScalarStyle {
        match style {
            2 => YamlScalarStyle::Plain,
            4 => YamlScalarStyle::SingleQuoted,
            8 => YamlScalarStyle::DoubleQuoted,
            16 => YamlScalarStyle::Literal,
            32 => YamlScalarStyle::Folded,
            _ => YamlScalarStyle::Any,
        }
    }
}

/// Go: `yaml_sequence_style_t` — sequence styles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlSequenceStyle {
    /// Let the emitter choose the style.
    #[default]
    Any = 0,
    /// The block sequence style.
    Block = 1,
    /// The flow sequence style.
    Flow = 2,
}

impl YamlSequenceStyle {
    /// Go's `yaml_sequence_style_t(style)` conversion.
    pub(crate) fn from_style(style: YamlStyle) -> YamlSequenceStyle {
        match style {
            1 => YamlSequenceStyle::Block,
            2 => YamlSequenceStyle::Flow,
            _ => YamlSequenceStyle::Any,
        }
    }
}

/// Go: `yaml_mapping_style_t` — mapping styles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlMappingStyle {
    /// Let the emitter choose the style.
    #[default]
    Any = 0,
    /// The block mapping style.
    Block = 1,
    /// The flow mapping style.
    Flow = 2,
}

impl YamlMappingStyle {
    /// Go's `yaml_mapping_style_t(style)` conversion.
    pub(crate) fn from_style(style: YamlStyle) -> YamlMappingStyle {
        match style {
            1 => YamlMappingStyle::Block,
            2 => YamlMappingStyle::Flow,
            _ => YamlMappingStyle::Any,
        }
    }
}

// Tokens

/// Go: `yaml_token_type_t` — token types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlTokenType {
    /// An empty token.
    #[default]
    No = 0,
    /// A STREAM-START token.
    StreamStart,
    /// A STREAM-END token.
    StreamEnd,
    /// A VERSION-DIRECTIVE token.
    VersionDirective,
    /// A TAG-DIRECTIVE token.
    TagDirective,
    /// A DOCUMENT-START token.
    DocumentStart,
    /// A DOCUMENT-END token.
    DocumentEnd,
    /// A BLOCK-SEQUENCE-START token.
    BlockSequenceStart,
    /// A BLOCK-SEQUENCE-END token.
    BlockMappingStart,
    /// A BLOCK-END token.
    BlockEnd,
    /// A FLOW-SEQUENCE-START token.
    FlowSequenceStart,
    /// A FLOW-SEQUENCE-END token.
    FlowSequenceEnd,
    /// A FLOW-MAPPING-START token.
    FlowMappingStart,
    /// A FLOW-MAPPING-END token.
    FlowMappingEnd,
    /// A BLOCK-ENTRY token.
    BlockEntry,
    /// A FLOW-ENTRY token.
    FlowEntry,
    /// A KEY token.
    Key,
    /// A VALUE token.
    Value,
    /// An ALIAS token.
    Alias,
    /// An ANCHOR token.
    Anchor,
    /// A TAG token.
    Tag,
    /// A SCALAR token.
    Scalar,
}

impl fmt::Display for YamlTokenType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Go: yamlh.go yaml_token_type_t.String
        let s = match self {
            YamlTokenType::No => "yaml_NO_TOKEN",
            YamlTokenType::StreamStart => "yaml_STREAM_START_TOKEN",
            YamlTokenType::StreamEnd => "yaml_STREAM_END_TOKEN",
            YamlTokenType::VersionDirective => "yaml_VERSION_DIRECTIVE_TOKEN",
            YamlTokenType::TagDirective => "yaml_TAG_DIRECTIVE_TOKEN",
            YamlTokenType::DocumentStart => "yaml_DOCUMENT_START_TOKEN",
            YamlTokenType::DocumentEnd => "yaml_DOCUMENT_END_TOKEN",
            YamlTokenType::BlockSequenceStart => "yaml_BLOCK_SEQUENCE_START_TOKEN",
            YamlTokenType::BlockMappingStart => "yaml_BLOCK_MAPPING_START_TOKEN",
            YamlTokenType::BlockEnd => "yaml_BLOCK_END_TOKEN",
            YamlTokenType::FlowSequenceStart => "yaml_FLOW_SEQUENCE_START_TOKEN",
            YamlTokenType::FlowSequenceEnd => "yaml_FLOW_SEQUENCE_END_TOKEN",
            YamlTokenType::FlowMappingStart => "yaml_FLOW_MAPPING_START_TOKEN",
            YamlTokenType::FlowMappingEnd => "yaml_FLOW_MAPPING_END_TOKEN",
            YamlTokenType::BlockEntry => "yaml_BLOCK_ENTRY_TOKEN",
            YamlTokenType::FlowEntry => "yaml_FLOW_ENTRY_TOKEN",
            YamlTokenType::Key => "yaml_KEY_TOKEN",
            YamlTokenType::Value => "yaml_VALUE_TOKEN",
            YamlTokenType::Alias => "yaml_ALIAS_TOKEN",
            YamlTokenType::Anchor => "yaml_ANCHOR_TOKEN",
            YamlTokenType::Tag => "yaml_TAG_TOKEN",
            YamlTokenType::Scalar => "yaml_SCALAR_TOKEN",
        };
        f.write_str(s)
    }
}

/// Go: `yaml_token_t` — the token structure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlToken {
    /// The token type.
    pub(crate) typ: YamlTokenType,
    /// The start of the token.
    pub(crate) start_mark: YamlMark,
    /// The end of the token.
    pub(crate) end_mark: YamlMark,
    /// The stream encoding (for `StreamStart`).
    pub(crate) encoding: YamlEncoding,
    /// The alias/anchor/scalar value or tag/tag directive handle
    /// (for `Alias`, `Anchor`, `Scalar`, `Tag`, `TagDirective`).
    pub(crate) value: Vec<u8>,
    /// The tag suffix (for `Tag`).
    pub(crate) suffix: Vec<u8>,
    /// The tag directive prefix (for `TagDirective`).
    pub(crate) prefix: Vec<u8>,
    /// The scalar style (for `Scalar`).
    pub(crate) style: YamlScalarStyle,
    /// The version directive major (for `VersionDirective`).
    pub(crate) major: i8,
    /// The version directive minor (for `VersionDirective`).
    pub(crate) minor: i8,
}

// Events

/// Go: `yaml_event_type_t` — event types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlEventType {
    /// An empty event.
    #[default]
    No = 0,
    /// A STREAM-START event.
    StreamStart,
    /// A STREAM-END event.
    StreamEnd,
    /// A DOCUMENT-START event.
    DocumentStart,
    /// A DOCUMENT-END event.
    DocumentEnd,
    /// An ALIAS event.
    Alias,
    /// A SCALAR event.
    Scalar,
    /// A SEQUENCE-START event.
    SequenceStart,
    /// A SEQUENCE-END event.
    SequenceEnd,
    /// A MAPPING-START event.
    MappingStart,
    /// A MAPPING-END event.
    MappingEnd,
    /// A TAIL-COMMENT event.
    TailComment,
}

impl fmt::Display for YamlEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Go: yamlh.go yaml_event_type_t.String (eventStrings)
        let s = match self {
            YamlEventType::No => "none",
            YamlEventType::StreamStart => "stream start",
            YamlEventType::StreamEnd => "stream end",
            YamlEventType::DocumentStart => "document start",
            YamlEventType::DocumentEnd => "document end",
            YamlEventType::Alias => "alias",
            YamlEventType::Scalar => "scalar",
            YamlEventType::SequenceStart => "sequence start",
            YamlEventType::SequenceEnd => "sequence end",
            YamlEventType::MappingStart => "mapping start",
            YamlEventType::MappingEnd => "mapping end",
            YamlEventType::TailComment => "tail comment",
        };
        f.write_str(s)
    }
}

/// Go: `yaml_event_t` — the event structure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlEvent {
    /// The event type.
    pub(crate) typ: YamlEventType,
    /// The start of the event.
    pub(crate) start_mark: YamlMark,
    /// The end of the event.
    pub(crate) end_mark: YamlMark,
    /// The document encoding (for `StreamStart`).
    pub(crate) encoding: YamlEncoding,
    /// The version directive (for `DocumentStart`); Go's pointer, `None` = nil.
    pub(crate) version_directive: Option<YamlVersionDirective>,
    /// The list of tag directives (for `DocumentStart`).
    pub(crate) tag_directives: Vec<YamlTagDirective>,
    /// The head comment.
    pub(crate) head_comment: Vec<u8>,
    /// The line comment.
    pub(crate) line_comment: Vec<u8>,
    /// The foot comment.
    pub(crate) foot_comment: Vec<u8>,
    /// The tail comment.
    pub(crate) tail_comment: Vec<u8>,
    /// The anchor (for `Scalar`, `SequenceStart`, `MappingStart`, `Alias`).
    pub(crate) anchor: Vec<u8>,
    /// The tag (for `Scalar`, `SequenceStart`, `MappingStart`).
    pub(crate) tag: Vec<u8>,
    /// The scalar value (for `Scalar`).
    pub(crate) value: Vec<u8>,
    /// Is the document start/end indicator implicit, or the tag optional?
    /// (for `DocumentStart`, `DocumentEnd`, `SequenceStart`, `MappingStart`, `Scalar`).
    pub(crate) implicit: bool,
    /// Is the tag optional for any non-plain style? (for `Scalar`).
    pub(crate) quoted_implicit: bool,
    /// The style (for `Scalar`, `SequenceStart`, `MappingStart`).
    pub(crate) style: YamlStyle,
}

impl YamlEvent {
    /// Go: `(*yaml_event_t).scalar_style`.
    pub(crate) fn scalar_style(&self) -> YamlScalarStyle {
        // Go: yamlh.go scalar_style
        YamlScalarStyle::from_style(self.style)
    }

    /// Go: `(*yaml_event_t).sequence_style`.
    pub(crate) fn sequence_style(&self) -> YamlSequenceStyle {
        // Go: yamlh.go sequence_style
        YamlSequenceStyle::from_style(self.style)
    }

    /// Go: `(*yaml_event_t).mapping_style`.
    pub(crate) fn mapping_style(&self) -> YamlMappingStyle {
        // Go: yamlh.go mapping_style
        YamlMappingStyle::from_style(self.style)
    }
}

// Nodes

/// The tag `!!null` with the only possible value: null.
pub(crate) const YAML_NULL_TAG: &str = "tag:yaml.org,2002:null";
/// The tag `!!bool` with the values: true and false.
pub(crate) const YAML_BOOL_TAG: &str = "tag:yaml.org,2002:bool";
/// The tag `!!str` for string values.
pub(crate) const YAML_STR_TAG: &str = "tag:yaml.org,2002:str";
/// The tag `!!int` for integer values.
pub(crate) const YAML_INT_TAG: &str = "tag:yaml.org,2002:int";
/// The tag `!!float` for float values.
pub(crate) const YAML_FLOAT_TAG: &str = "tag:yaml.org,2002:float";
/// The tag `!!timestamp` for date and time values.
pub(crate) const YAML_TIMESTAMP_TAG: &str = "tag:yaml.org,2002:timestamp";
/// The tag `!!seq` is used to denote sequences.
pub(crate) const YAML_SEQ_TAG: &str = "tag:yaml.org,2002:seq";
/// The tag `!!map` is used to denote mapping.
pub(crate) const YAML_MAP_TAG: &str = "tag:yaml.org,2002:map";
/// Not in original libyaml.
pub(crate) const YAML_BINARY_TAG: &str = "tag:yaml.org,2002:binary";
/// Not in original libyaml.
pub(crate) const YAML_MERGE_TAG: &str = "tag:yaml.org,2002:merge";
/// The default scalar tag is `!!str`.
pub(crate) const YAML_DEFAULT_SCALAR_TAG: &str = YAML_STR_TAG;
/// The default sequence tag is `!!seq`.
pub(crate) const YAML_DEFAULT_SEQUENCE_TAG: &str = YAML_SEQ_TAG;
/// The default mapping tag is `!!map`.
pub(crate) const YAML_DEFAULT_MAPPING_TAG: &str = YAML_MAP_TAG;

/// Go: `yaml_node_type_t` — node types (libyaml's document API; unused by yaml.v3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlNodeType {
    /// An empty node.
    #[default]
    No = 0,
    /// A scalar node.
    Scalar,
    /// A sequence node.
    Sequence,
    /// A mapping node.
    Mapping,
}

/// Go: `yaml_node_item_t` — an element of a sequence node.
pub(crate) type YamlNodeItem = isize;

/// Go: `yaml_node_pair_t` — an element of a mapping node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlNodePair {
    /// The key of the element.
    pub(crate) key: isize,
    /// The value of the element.
    pub(crate) value: isize,
}

/// Go: `yaml_node_t.scalar` — the scalar parameters (for `YamlNodeType::Scalar`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlNodeScalar {
    /// The scalar value.
    pub(crate) value: Vec<u8>,
    /// The length of the scalar value.
    pub(crate) length: usize,
    /// The scalar style.
    pub(crate) style: YamlScalarStyle,
}

/// Go: `yaml_node_t.sequence` — the sequence parameters (for `YamlNodeType::Sequence`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlNodeSequence {
    /// The stack of sequence items.
    pub(crate) items_data: Vec<YamlNodeItem>,
    /// The sequence style.
    pub(crate) style: YamlSequenceStyle,
}

/// Go: `yaml_node_t.mapping` — the mapping parameters (for `YamlNodeType::Mapping`). Go's
/// `pairs_start`/`pairs_end`/`pairs_top` pointers are indices into `pairs_data`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlNodeMapping {
    /// The stack of mapping pairs (key, value).
    pub(crate) pairs_data: Vec<YamlNodePair>,
    /// The beginning of the stack.
    pub(crate) pairs_start: usize,
    /// The end of the stack.
    pub(crate) pairs_end: usize,
    /// The top of the stack.
    pub(crate) pairs_top: usize,
    /// The mapping style.
    pub(crate) style: YamlMappingStyle,
}

/// Go: `yaml_node_t` — the node structure (libyaml's document API; unused by yaml.v3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlNode {
    /// The node type.
    pub(crate) typ: YamlNodeType,
    /// The node tag.
    pub(crate) tag: Vec<u8>,
    /// The scalar parameters.
    pub(crate) scalar: YamlNodeScalar,
    /// The sequence parameters.
    pub(crate) sequence: YamlNodeSequence,
    /// The mapping parameters.
    pub(crate) mapping: YamlNodeMapping,
    /// The beginning of the node.
    pub(crate) start_mark: YamlMark,
    /// The end of the node.
    pub(crate) end_mark: YamlMark,
}

/// Go: `yaml_document_t` — the document structure (libyaml's document API; unused by yaml.v3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlDocument {
    /// The document nodes.
    pub(crate) nodes: Vec<YamlNode>,
    /// The version directive.
    pub(crate) version_directive: Option<YamlVersionDirective>,
    /// The list of tag directives.
    pub(crate) tag_directives_data: Vec<YamlTagDirective>,
    /// The beginning of the tag directives list.
    pub(crate) tag_directives_start: usize,
    /// The end of the tag directives list.
    pub(crate) tag_directives_end: usize,
    /// Is the document start indicator implicit?
    pub(crate) start_implicit: isize,
    /// Is the document end indicator implicit?
    pub(crate) end_implicit: isize,
    /// The start of the document.
    pub(crate) start_mark: YamlMark,
    /// The end of the document.
    pub(crate) end_mark: YamlMark,
}

/// Go: `yaml_read_handler_t` — called when the parser needs to read more bytes from the source.
///
/// The handler writes at most `buffer.len()` bytes into `buffer` and returns how many it wrote.
/// Go's `(0, io.EOF)` is `Ok(0)`; any other error is `Err` (reported as `input error: …`).
/// The `buffer` is not part of `parser` (the caller lends the raw buffer out for the call).
pub(crate) type YamlReadHandler = fn(&mut YamlParser<'_>, &mut [u8]) -> std::io::Result<usize>;

/// Go: `yaml_simple_key_t` — information about a potential simple key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlSimpleKey {
    /// Is a simple key possible?
    pub(crate) possible: bool,
    /// Is a simple key required?
    pub(crate) required: bool,
    /// The number of the token.
    pub(crate) token_number: usize,
    /// The position mark.
    pub(crate) mark: YamlMark,
}

/// Go: `yaml_parser_state_t` — the states of the parser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlParserState {
    /// Expect STREAM-START.
    #[default]
    StreamStart = 0,
    /// Expect the beginning of an implicit document.
    ImplicitDocumentStart,
    /// Expect DOCUMENT-START.
    DocumentStart,
    /// Expect the content of a document.
    DocumentContent,
    /// Expect DOCUMENT-END.
    DocumentEnd,
    /// Expect a block node.
    BlockNode,
    /// Expect a block node or indentless sequence.
    BlockNodeOrIndentlessSequence,
    /// Expect a flow node.
    FlowNode,
    /// Expect the first entry of a block sequence.
    BlockSequenceFirstEntry,
    /// Expect an entry of a block sequence.
    BlockSequenceEntry,
    /// Expect an entry of an indentless sequence.
    IndentlessSequenceEntry,
    /// Expect the first key of a block mapping.
    BlockMappingFirstKey,
    /// Expect a block mapping key.
    BlockMappingKey,
    /// Expect a block mapping value.
    BlockMappingValue,
    /// Expect the first entry of a flow sequence.
    FlowSequenceFirstEntry,
    /// Expect an entry of a flow sequence.
    FlowSequenceEntry,
    /// Expect a key of an ordered mapping.
    FlowSequenceEntryMappingKey,
    /// Expect a value of an ordered mapping.
    FlowSequenceEntryMappingValue,
    /// Expect the and of an ordered mapping entry.
    FlowSequenceEntryMappingEnd,
    /// Expect the first key of a flow mapping.
    FlowMappingFirstKey,
    /// Expect a key of a flow mapping.
    FlowMappingKey,
    /// Expect a value of a flow mapping.
    FlowMappingValue,
    /// Expect an empty value of a flow mapping.
    FlowMappingEmptyValue,
    /// Expect nothing.
    End,
}

impl fmt::Display for YamlParserState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Go: yamlh.go yaml_parser_state_t.String
        let s = match self {
            YamlParserState::StreamStart => "yaml_PARSE_STREAM_START_STATE",
            YamlParserState::ImplicitDocumentStart => "yaml_PARSE_IMPLICIT_DOCUMENT_START_STATE",
            YamlParserState::DocumentStart => "yaml_PARSE_DOCUMENT_START_STATE",
            YamlParserState::DocumentContent => "yaml_PARSE_DOCUMENT_CONTENT_STATE",
            YamlParserState::DocumentEnd => "yaml_PARSE_DOCUMENT_END_STATE",
            YamlParserState::BlockNode => "yaml_PARSE_BLOCK_NODE_STATE",
            YamlParserState::BlockNodeOrIndentlessSequence => {
                "yaml_PARSE_BLOCK_NODE_OR_INDENTLESS_SEQUENCE_STATE"
            }
            YamlParserState::FlowNode => "yaml_PARSE_FLOW_NODE_STATE",
            YamlParserState::BlockSequenceFirstEntry => {
                "yaml_PARSE_BLOCK_SEQUENCE_FIRST_ENTRY_STATE"
            }
            YamlParserState::BlockSequenceEntry => "yaml_PARSE_BLOCK_SEQUENCE_ENTRY_STATE",
            YamlParserState::IndentlessSequenceEntry => {
                "yaml_PARSE_INDENTLESS_SEQUENCE_ENTRY_STATE"
            }
            YamlParserState::BlockMappingFirstKey => "yaml_PARSE_BLOCK_MAPPING_FIRST_KEY_STATE",
            YamlParserState::BlockMappingKey => "yaml_PARSE_BLOCK_MAPPING_KEY_STATE",
            YamlParserState::BlockMappingValue => "yaml_PARSE_BLOCK_MAPPING_VALUE_STATE",
            YamlParserState::FlowSequenceFirstEntry => "yaml_PARSE_FLOW_SEQUENCE_FIRST_ENTRY_STATE",
            YamlParserState::FlowSequenceEntry => "yaml_PARSE_FLOW_SEQUENCE_ENTRY_STATE",
            YamlParserState::FlowSequenceEntryMappingKey => {
                "yaml_PARSE_FLOW_SEQUENCE_ENTRY_MAPPING_KEY_STATE"
            }
            YamlParserState::FlowSequenceEntryMappingValue => {
                "yaml_PARSE_FLOW_SEQUENCE_ENTRY_MAPPING_VALUE_STATE"
            }
            YamlParserState::FlowSequenceEntryMappingEnd => {
                "yaml_PARSE_FLOW_SEQUENCE_ENTRY_MAPPING_END_STATE"
            }
            YamlParserState::FlowMappingFirstKey => "yaml_PARSE_FLOW_MAPPING_FIRST_KEY_STATE",
            YamlParserState::FlowMappingKey => "yaml_PARSE_FLOW_MAPPING_KEY_STATE",
            YamlParserState::FlowMappingValue => "yaml_PARSE_FLOW_MAPPING_VALUE_STATE",
            YamlParserState::FlowMappingEmptyValue => "yaml_PARSE_FLOW_MAPPING_EMPTY_VALUE_STATE",
            YamlParserState::End => "yaml_PARSE_END_STATE",
        };
        f.write_str(s)
    }
}

/// Go: `yaml_alias_data_t` — aliases data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlAliasData {
    /// The anchor.
    pub(crate) anchor: Vec<u8>,
    /// The node id.
    pub(crate) index: usize,
    /// The anchor mark.
    pub(crate) mark: YamlMark,
}

/// Go: `yaml_parser_t` — the parser structure. All members are internal; manage it with the
/// `yaml_parser_` family of functions (`api.rs`, `reader.rs`, `scanner.rs`, `parser.rs`).
///
/// `'r` is the lifetime of the input: the borrowed string input (`input`, Go's `[]byte` slice)
/// or the boxed reader (`input_reader`, Go's `io.Reader`).
#[derive(Default)]
pub(crate) struct YamlParser<'r> {
    // Error handling
    /// Error type.
    pub(crate) error: YamlErrorType,
    /// Error description.
    pub(crate) problem: String,
    /// The byte about which the problem occurred.
    pub(crate) problem_offset: usize,
    /// The problem value (`-1` when there is none).
    pub(crate) problem_value: isize,
    /// The problem position.
    pub(crate) problem_mark: YamlMark,
    /// The error context.
    pub(crate) context: String,
    /// The error context position.
    pub(crate) context_mark: YamlMark,

    // Reader stuff
    /// Read handler.
    pub(crate) read_handler: Option<YamlReadHandler>,
    /// File input data.
    pub(crate) input_reader: Option<Box<dyn Read + 'r>>,
    /// String input data.
    pub(crate) input: &'r [u8],
    /// The read position in `input`.
    pub(crate) input_pos: usize,
    /// EOF flag.
    pub(crate) eof: bool,
    /// The working buffer.
    pub(crate) buffer: Vec<u8>,
    /// The current position of the buffer.
    pub(crate) buffer_pos: usize,
    /// The number of unread characters in the buffer.
    pub(crate) unread: usize,
    /// The number of line breaks since last non-break/non-blank character.
    pub(crate) newlines: usize,
    /// The raw buffer.
    pub(crate) raw_buffer: Vec<u8>,
    /// The current position of the raw buffer.
    pub(crate) raw_buffer_pos: usize,
    /// The input encoding.
    pub(crate) encoding: YamlEncoding,
    /// The offset of the current position (in bytes).
    pub(crate) offset: usize,
    /// The mark of the current position.
    pub(crate) mark: YamlMark,

    // Comments
    /// The current head comments.
    pub(crate) head_comment: Vec<u8>,
    /// The current line comments.
    pub(crate) line_comment: Vec<u8>,
    /// The current foot comments.
    pub(crate) foot_comment: Vec<u8>,
    /// Foot comment that happens at the end of a block.
    pub(crate) tail_comment: Vec<u8>,
    /// Comment in item preceding a nested structure (list inside list item, etc).
    pub(crate) stem_comment: Vec<u8>,
    /// The folded comments for all parsed tokens.
    pub(crate) comments: Vec<YamlComment>,
    /// The head of the comments queue.
    pub(crate) comments_head: usize,

    // Scanner stuff
    /// Have we started to scan the input stream?
    pub(crate) stream_start_produced: bool,
    /// Have we reached the end of the input stream?
    pub(crate) stream_end_produced: bool,
    /// The number of unclosed '[' and '{' indicators.
    pub(crate) flow_level: usize,
    /// The tokens queue.
    pub(crate) tokens: Vec<YamlToken>,
    /// The head of the tokens queue.
    pub(crate) tokens_head: usize,
    /// The number of tokens fetched from the queue.
    pub(crate) tokens_parsed: usize,
    /// Does the tokens queue contain a token ready for dequeueing.
    pub(crate) token_available: bool,
    /// The current indentation level.
    pub(crate) indent: isize,
    /// The indentation levels stack.
    pub(crate) indents: Vec<isize>,
    /// May a simple key occur at the current position?
    pub(crate) simple_key_allowed: bool,
    /// The stack of simple keys.
    pub(crate) simple_keys: Vec<YamlSimpleKey>,
    /// Possible simple_key indexes indexed by token_number.
    pub(crate) simple_keys_by_tok: HashMap<usize, usize>,

    // Parser stuff
    /// The current parser state.
    pub(crate) state: YamlParserState,
    /// The parser states stack.
    pub(crate) states: Vec<YamlParserState>,
    /// The stack of marks.
    pub(crate) marks: Vec<YamlMark>,
    /// The list of TAG directives.
    pub(crate) tag_directives: Vec<YamlTagDirective>,

    // Dumper stuff
    /// The alias data.
    pub(crate) aliases: Vec<YamlAliasData>,
    /// The currently parsed document.
    pub(crate) document: Option<Box<YamlDocument>>,
}

/// Go: `yaml_comment_t`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlComment {
    /// Position where scanning for comments started.
    pub(crate) scan_mark: YamlMark,
    /// Position after which tokens will be associated with this comment.
    pub(crate) token_mark: YamlMark,
    /// Position of '#' comment mark.
    pub(crate) start_mark: YamlMark,
    /// Position where comment terminated.
    pub(crate) end_mark: YamlMark,
    /// The head comment text.
    pub(crate) head: Vec<u8>,
    /// The line comment text.
    pub(crate) line: Vec<u8>,
    /// The foot comment text.
    pub(crate) foot: Vec<u8>,
}

// Emitter Definitions

/// Go: `yaml_write_handler_t` — called when the emitter needs to flush the accumulated
/// characters to the output. The handler writes all of `buffer`; an `Err` becomes the emitter's
/// `write error: …`. The `buffer` is not part of `emitter` (the caller lends it out for the
/// call).
pub(crate) type YamlWriteHandler = fn(&mut YamlEmitter<'_>, &[u8]) -> std::io::Result<()>;

/// Go: `yaml_emitter_state_t` — the emitter states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum YamlEmitterState {
    /// Expect STREAM-START.
    #[default]
    StreamStart = 0,
    /// Expect the first DOCUMENT-START or STREAM-END.
    FirstDocumentStart,
    /// Expect DOCUMENT-START or STREAM-END.
    DocumentStart,
    /// Expect the content of a document.
    DocumentContent,
    /// Expect DOCUMENT-END.
    DocumentEnd,
    /// Expect the first item of a flow sequence.
    FlowSequenceFirstItem,
    /// Expect the next item of a flow sequence, with the comma already written out.
    FlowSequenceTrailItem,
    /// Expect an item of a flow sequence.
    FlowSequenceItem,
    /// Expect the first key of a flow mapping.
    FlowMappingFirstKey,
    /// Expect the next key of a flow mapping, with the comma already written out.
    FlowMappingTrailKey,
    /// Expect a key of a flow mapping.
    FlowMappingKey,
    /// Expect a value for a simple key of a flow mapping.
    FlowMappingSimpleValue,
    /// Expect a value of a flow mapping.
    FlowMappingValue,
    /// Expect the first item of a block sequence.
    BlockSequenceFirstItem,
    /// Expect an item of a block sequence.
    BlockSequenceItem,
    /// Expect the first key of a block mapping.
    BlockMappingFirstKey,
    /// Expect the key of a block mapping.
    BlockMappingKey,
    /// Expect a value for a simple key of a block mapping.
    BlockMappingSimpleValue,
    /// Expect a value of a block mapping.
    BlockMappingValue,
    /// Expect nothing.
    End,
}

/// Go: `yaml_emitter_t.anchor_data` — anchor analysis.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlAnchorData {
    /// The anchor value.
    pub(crate) anchor: Vec<u8>,
    /// Is it an alias?
    pub(crate) alias: bool,
}

/// Go: `yaml_emitter_t.tag_data` — tag analysis.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlTagData {
    /// The tag handle.
    pub(crate) handle: Vec<u8>,
    /// The tag suffix.
    pub(crate) suffix: Vec<u8>,
}

/// Go: `yaml_emitter_t.scalar_data` — scalar analysis.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlScalarData {
    /// The scalar value.
    pub(crate) value: Vec<u8>,
    /// Does the scalar contain line breaks?
    pub(crate) multiline: bool,
    /// Can the scalar be expessed in the flow plain style?
    pub(crate) flow_plain_allowed: bool,
    /// Can the scalar be expressed in the block plain style?
    pub(crate) block_plain_allowed: bool,
    /// Can the scalar be expressed in the single quoted style?
    pub(crate) single_quoted_allowed: bool,
    /// Can the scalar be expressed in the literal or folded styles?
    pub(crate) block_allowed: bool,
    /// The output style.
    pub(crate) style: YamlScalarStyle,
}

/// Go: `yaml_emitter_t.anchors` — the information associated with the document nodes
/// (libyaml's dumper; unused by yaml.v3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct YamlEmitterAnchors {
    /// The number of references.
    pub(crate) references: isize,
    /// The anchor id.
    pub(crate) anchor: isize,
    /// If the node has been emitted?
    pub(crate) serialized: bool,
}

/// Go: `yaml_emitter_t` — the emitter structure. All members are internal; manage it with the
/// `yaml_emitter_` family of functions (`api.rs`, `emitter.rs`, `writer.rs`).
///
/// Go's string output (`output_buffer *[]byte`, pointing at the encoder's `out`) is the owned
/// `output_buffer` here; `'w` is the lifetime of the boxed writer (`output_writer`).
#[derive(Default)]
pub(crate) struct YamlEmitter<'w> {
    // Error handling
    /// Error type.
    pub(crate) error: YamlErrorType,
    /// Error description.
    pub(crate) problem: String,

    // Writer stuff
    /// Write handler.
    pub(crate) write_handler: Option<YamlWriteHandler>,
    /// String output data.
    pub(crate) output_buffer: Vec<u8>,
    /// File output data.
    pub(crate) output_writer: Option<Box<dyn Write + 'w>>,
    /// The working buffer.
    pub(crate) buffer: Vec<u8>,
    /// The current position of the buffer.
    pub(crate) buffer_pos: usize,
    /// The raw buffer.
    pub(crate) raw_buffer: Vec<u8>,
    /// The current position of the raw buffer.
    pub(crate) raw_buffer_pos: usize,
    /// The stream encoding.
    pub(crate) encoding: YamlEncoding,

    // Emitter stuff
    /// If the output is in the canonical style?
    pub(crate) canonical: bool,
    /// The number of indentation spaces.
    pub(crate) best_indent: isize,
    /// The preferred width of the output lines.
    pub(crate) best_width: isize,
    /// Allow unescaped non-ASCII characters?
    pub(crate) unicode: bool,
    /// The preferred line break.
    pub(crate) line_break: YamlBreak,
    /// The current emitter state.
    pub(crate) state: YamlEmitterState,
    /// The stack of states.
    pub(crate) states: Vec<YamlEmitterState>,
    /// The event queue.
    pub(crate) events: Vec<YamlEvent>,
    /// The head of the event queue.
    pub(crate) events_head: usize,
    /// The stack of indentation levels.
    pub(crate) indents: Vec<isize>,
    /// The list of tag directives.
    pub(crate) tag_directives: Vec<YamlTagDirective>,
    /// The current indentation level.
    pub(crate) indent: isize,
    /// The current flow level.
    pub(crate) flow_level: usize,
    /// Is it the document root context?
    pub(crate) root_context: bool,
    /// Is it a sequence context?
    pub(crate) sequence_context: bool,
    /// Is it a mapping context?
    pub(crate) mapping_context: bool,
    /// Is it a simple mapping key context?
    pub(crate) simple_key_context: bool,
    /// The current line.
    pub(crate) line: isize,
    /// The current column.
    pub(crate) column: isize,
    /// If the last character was a whitespace?
    pub(crate) whitespace: bool,
    /// If the last character was an indentation character (' ', '-', '?', ':')?
    pub(crate) indention: bool,
    /// If an explicit document end is required?
    pub(crate) open_ended: bool,
    /// Is there's an empty line above?
    pub(crate) space_above: bool,
    /// The indent used to write the foot comment above, or -1 if none.
    pub(crate) foot_indent: isize,
    /// Anchor analysis.
    pub(crate) anchor_data: YamlAnchorData,
    /// Tag analysis.
    pub(crate) tag_data: YamlTagData,
    /// Scalar analysis.
    pub(crate) scalar_data: YamlScalarData,

    // Comments
    /// The pending head comment.
    pub(crate) head_comment: Vec<u8>,
    /// The pending line comment.
    pub(crate) line_comment: Vec<u8>,
    /// The pending foot comment.
    pub(crate) foot_comment: Vec<u8>,
    /// The pending tail comment.
    pub(crate) tail_comment: Vec<u8>,
    /// The line comment of the current key.
    pub(crate) key_line_comment: Vec<u8>,

    // Dumper stuff
    /// If the stream was already opened?
    pub(crate) opened: bool,
    /// If the stream was already closed?
    pub(crate) closed: bool,
    /// The information associated with the document nodes.
    pub(crate) anchors: Option<YamlEmitterAnchors>,
    /// The last assigned anchor id.
    pub(crate) last_anchor_id: isize,
    /// The currently emitted document.
    pub(crate) document: Option<Box<YamlDocument>>,
}

// ---------------------------------------------------------------------------------------------
// yamlprivateh.go
// ---------------------------------------------------------------------------------------------

/// The size of the input raw buffer.
pub(crate) const INPUT_RAW_BUFFER_SIZE: usize = 512;

/// The size of the input buffer. It should be possible to decode the whole raw buffer.
pub(crate) const INPUT_BUFFER_SIZE: usize = INPUT_RAW_BUFFER_SIZE * 3;

/// The size of the output buffer.
pub(crate) const OUTPUT_BUFFER_SIZE: usize = 128;

/// The size of the output raw buffer. It should be possible to encode the whole output buffer.
pub(crate) const OUTPUT_RAW_BUFFER_SIZE: usize = OUTPUT_BUFFER_SIZE * 2 + 2;

/// The size of other stacks and queues.
pub(crate) const INITIAL_STACK_SIZE: usize = 16;
/// The initial queue size.
pub(crate) const INITIAL_QUEUE_SIZE: usize = 16;
/// The initial string size.
pub(crate) const INITIAL_STRING_SIZE: usize = 16;

/// Check if the character at the specified position is an alphabetical character, a digit,
/// '_', or '-'.
pub(crate) fn is_alpha(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_alpha
    b[i].is_ascii_digit()
        || b[i].is_ascii_uppercase()
        || b[i].is_ascii_lowercase()
        || b[i] == b'_'
        || b[i] == b'-'
}

/// Check if the character at the specified position is a digit.
pub(crate) fn is_digit(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_digit
    b[i].is_ascii_digit()
}

/// Get the value of a digit.
pub(crate) fn as_digit(b: &[u8], i: usize) -> isize {
    // Go: yamlprivateh.go as_digit
    isize::from(b[i]) - isize::from(b'0')
}

/// Check if the character at the specified position is a hex-digit.
pub(crate) fn is_hex(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_hex
    b[i].is_ascii_digit() || (b'A'..=b'F').contains(&b[i]) || (b'a'..=b'f').contains(&b[i])
}

/// Get the value of a hex-digit.
pub(crate) fn as_hex(b: &[u8], i: usize) -> isize {
    // Go: yamlprivateh.go as_hex
    let bi = b[i];
    if (b'A'..=b'F').contains(&bi) {
        return isize::from(bi) - isize::from(b'A') + 10;
    }
    if (b'a'..=b'f').contains(&bi) {
        return isize::from(bi) - isize::from(b'a') + 10;
    }
    isize::from(bi) - isize::from(b'0')
}

/// Check if the character is ASCII.
pub(crate) fn is_ascii(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_ascii
    b[i] <= 0x7F
}

/// Check if the character at the start of the buffer can be printed unescaped.
pub(crate) fn is_printable(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_printable
    (b[i] == 0x0A) // . == #x0A
        || (b[i] >= 0x20 && b[i] <= 0x7E) // #x20 <= . <= #x7E
        || (b[i] == 0xC2 && b[i + 1] >= 0xA0) // #0xA0 <= . <= #xD7FF
        || (b[i] > 0xC2 && b[i] < 0xED)
        || (b[i] == 0xED && b[i + 1] < 0xA0)
        || (b[i] == 0xEE)
        || (b[i] == 0xEF // #xE000 <= . <= #xFFFD
            && !(b[i + 1] == 0xBB && b[i + 2] == 0xBF) // && . != #xFEFF
            && !(b[i + 1] == 0xBF && (b[i + 2] == 0xBE || b[i + 2] == 0xBF)))
}

/// Check if the character at the specified position is NUL.
pub(crate) fn is_z(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_z
    b[i] == 0x00
}

/// Check if the beginning of the buffer is a BOM. As in Go, the position is ignored: the
/// check is always on the first three bytes of `b`.
pub(crate) fn is_bom(b: &[u8], _i: usize) -> bool {
    // Go: yamlprivateh.go is_bom
    b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF
}

/// Check if the character at the specified position is space.
pub(crate) fn is_space(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_space
    b[i] == b' '
}

/// Check if the character at the specified position is tab.
pub(crate) fn is_tab(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_tab
    b[i] == b'\t'
}

/// Check if the character at the specified position is blank (space or tab).
pub(crate) fn is_blank(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_blank
    b[i] == b' ' || b[i] == b'\t'
}

/// Check if the character at the specified position is a line break.
pub(crate) fn is_break(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_break
    b[i] == b'\r' // CR (#xD)
        || b[i] == b'\n' // LF (#xA)
        || b[i] == 0xC2 && b[i + 1] == 0x85 // NEL (#x85)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA8 // LS (#x2028)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA9 // PS (#x2029)
}

/// Check if the character at the specified position is CR LF.
pub(crate) fn is_crlf(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_crlf
    b[i] == b'\r' && b[i + 1] == b'\n'
}

/// Check if the character is a line break or NUL.
pub(crate) fn is_breakz(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_breakz
    // is_break:
    b[i] == b'\r' // CR (#xD)
        || b[i] == b'\n' // LF (#xA)
        || b[i] == 0xC2 && b[i + 1] == 0x85 // NEL (#x85)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA8 // LS (#x2028)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA9 // PS (#x2029)
        // is_z:
        || b[i] == 0
}

/// Check if the character is a line break, space, or NUL.
pub(crate) fn is_spacez(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_spacez
    // is_space:
    b[i] == b' '
        // is_breakz:
        || b[i] == b'\r' // CR (#xD)
        || b[i] == b'\n' // LF (#xA)
        || b[i] == 0xC2 && b[i + 1] == 0x85 // NEL (#x85)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA8 // LS (#x2028)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA9 // PS (#x2029)
        || b[i] == 0
}

/// Check if the character is a line break, space, tab, or NUL.
pub(crate) fn is_blankz(b: &[u8], i: usize) -> bool {
    // Go: yamlprivateh.go is_blankz
    // is_blank:
    b[i] == b' ' || b[i] == b'\t'
        // is_breakz:
        || b[i] == b'\r' // CR (#xD)
        || b[i] == b'\n' // LF (#xA)
        || b[i] == 0xC2 && b[i + 1] == 0x85 // NEL (#x85)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA8 // LS (#x2028)
        || b[i] == 0xE2 && b[i + 1] == 0x80 && b[i + 2] == 0xA9 // PS (#x2029)
        || b[i] == 0
}

/// Determine the width of the character (0 for a byte that cannot start a UTF-8 sequence).
pub(crate) fn width(b: u8) -> usize {
    // Go: yamlprivateh.go width
    // Don't replace these by a switch without first
    // confirming that it is being inlined.
    if b & 0x80 == 0x00 {
        return 1;
    }
    if b & 0xE0 == 0xC0 {
        return 2;
    }
    if b & 0xF0 == 0xE0 {
        return 3;
    }
    if b & 0xF8 == 0xF0 {
        return 4;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_values_match_go() {
        // Go: yaml_PLAIN_SCALAR_STYLE = 1 << iota with iota = 1, and so on.
        assert_eq!(YamlScalarStyle::Plain as i8, 2);
        assert_eq!(YamlScalarStyle::Folded as i8, 32);
        for s in [
            YamlScalarStyle::Any,
            YamlScalarStyle::Plain,
            YamlScalarStyle::SingleQuoted,
            YamlScalarStyle::DoubleQuoted,
            YamlScalarStyle::Literal,
            YamlScalarStyle::Folded,
        ] {
            assert_eq!(YamlScalarStyle::from_style(s as YamlStyle), s);
        }
        let ev = YamlEvent {
            style: YamlMappingStyle::Flow as YamlStyle,
            ..Default::default()
        };
        assert_eq!(ev.mapping_style(), YamlMappingStyle::Flow);
        // Go compares a mapping event's style as a sequence style (emitterc.go:828): same numbers.
        assert_eq!(ev.sequence_style(), YamlSequenceStyle::Flow);
        assert_eq!(YamlSequenceStyle::from_style(1), YamlSequenceStyle::Block);
        assert_eq!(YamlSequenceStyle::from_style(9), YamlSequenceStyle::Any);
        assert_eq!(YamlMappingStyle::from_style(1), YamlMappingStyle::Block);
        assert_eq!(YamlScalarStyle::from_style(3), YamlScalarStyle::Any);
    }

    #[test]
    fn strings_match_go() {
        assert_eq!(YamlEventType::No.to_string(), "none");
        assert_eq!(YamlEventType::DocumentStart.to_string(), "document start");
        assert_eq!(YamlEventType::TailComment.to_string(), "tail comment");
        assert_eq!(
            YamlTokenType::BlockMappingStart.to_string(),
            "yaml_BLOCK_MAPPING_START_TOKEN"
        );
        assert_eq!(YamlTokenType::No.to_string(), "yaml_NO_TOKEN");
        assert_eq!(
            YamlParserState::FlowSequenceEntryMappingEnd.to_string(),
            "yaml_PARSE_FLOW_SEQUENCE_ENTRY_MAPPING_END_STATE"
        );
        assert_eq!(YamlParserState::End.to_string(), "yaml_PARSE_END_STATE");
        assert_eq!(YamlTokenType::Scalar as i32, 21);
        assert_eq!(YamlEventType::TailComment as i8, 11);
        assert_eq!(YamlParserState::End as i32, 23);
        assert_eq!(YamlEmitterState::End as i32, 19);
    }

    #[test]
    fn tags_and_sizes_match_go() {
        assert_eq!(YAML_DEFAULT_SCALAR_TAG, "tag:yaml.org,2002:str");
        assert_eq!(YAML_DEFAULT_SEQUENCE_TAG, "tag:yaml.org,2002:seq");
        assert_eq!(YAML_DEFAULT_MAPPING_TAG, "tag:yaml.org,2002:map");
        assert_eq!(YAML_MERGE_TAG, "tag:yaml.org,2002:merge");
        assert_eq!(INPUT_BUFFER_SIZE, 1536);
        assert_eq!(OUTPUT_RAW_BUFFER_SIZE, 258);
    }

    #[test]
    fn char_classes() {
        let b = b"aZ9_-!";
        assert!((0..5).all(|i| is_alpha(b, i)));
        assert!(!is_alpha(b, 5));
        assert!(is_digit(b"7", 0) && !is_digit(b"a", 0));
        assert_eq!(as_digit(b"7", 0), 7);
        assert!(is_hex(b"f", 0) && is_hex(b"A", 0) && is_hex(b"0", 0) && !is_hex(b"g", 0));
        assert_eq!(as_hex(b"f", 0), 15);
        assert_eq!(as_hex(b"B", 0), 11);
        assert_eq!(as_hex(b"9", 0), 9);
        assert!(is_ascii(b"\x7f", 0) && !is_ascii(b"\x80", 0));
        assert!(is_z(b"\0", 0) && !is_z(b" ", 0));
        assert!(is_space(b" ", 0) && !is_space(b"\t", 0));
        assert!(is_tab(b"\t", 0) && !is_tab(b" ", 0));
        assert!(is_blank(b" ", 0) && is_blank(b"\t", 0) && !is_blank(b"\n", 0));
        assert!(is_crlf(b"\r\n", 0) && !is_crlf(b"\n\r", 0));
    }

    #[test]
    fn breaks_and_blanks() {
        for brk in [
            &b"\r"[..],
            b"\n",
            "\u{85}".as_bytes(),
            "\u{2028}".as_bytes(),
            "\u{2029}".as_bytes(),
        ] {
            let mut v = brk.to_vec();
            v.extend_from_slice(b"\0\0");
            assert!(is_break(&v, 0), "{brk:?}");
            assert!(is_breakz(&v, 0) && is_spacez(&v, 0) && is_blankz(&v, 0));
        }
        let nul = b"\0\0\0";
        assert!(!is_break(nul, 0) && is_breakz(nul, 0) && is_spacez(nul, 0) && is_blankz(nul, 0));
        let sp = b"  \0";
        assert!(!is_breakz(sp, 0) && is_spacez(sp, 0) && is_blankz(sp, 0));
        let tab = b"\t\0\0";
        assert!(!is_spacez(tab, 0) && is_blankz(tab, 0));
        // U+2027 is not a break.
        let v = "\u{2027}".as_bytes();
        assert!(!is_break(v, 0) && !is_blankz(v, 0));
        // Short-circuiting keeps one-byte ASCII lookups in bounds, as in Go.
        assert!(!is_break(b"a", 0) && !is_blankz(b"a", 0));
    }

    #[test]
    fn printable() {
        assert!(is_printable(b"\n", 0) && is_printable(b" ", 0) && is_printable(b"~", 0));
        assert!(!is_printable(b"\t", 0) && !is_printable(b"\x7f", 0));
        assert!(is_printable("\u{a0}".as_bytes(), 0));
        assert!(!is_printable("\u{85}".as_bytes(), 0));
        assert!(is_printable("é".as_bytes(), 0));
        assert!(is_printable("\u{d7ff}".as_bytes(), 0));
        assert!(is_printable("\u{e000}".as_bytes(), 0));
        assert!(is_printable("\u{fffd}".as_bytes(), 0));
        assert!(!is_printable("\u{feff}".as_bytes(), 0));
        assert!(!is_printable("\u{fffe}".as_bytes(), 0));
        assert!(!is_printable("\u{ffff}".as_bytes(), 0));
        // Four-byte sequences start with 0xF0..: not printable for libyaml.
        assert!(!is_printable("\u{1f600}".as_bytes(), 0));
    }

    #[test]
    fn bom_ignores_position() {
        let b = b"\xef\xbb\xbfx";
        assert!(is_bom(b, 0));
        assert!(is_bom(b, 3));
        assert!(!is_bom(b"x\xef\xbb\xbf", 1));
    }

    #[test]
    fn widths() {
        assert_eq!(width(b'a'), 1);
        assert_eq!(width(0xC3), 2);
        assert_eq!(width(0xE2), 3);
        assert_eq!(width(0xF0), 4);
        assert_eq!(width(0x80), 0);
        assert_eq!(width(0xF8), 0);
    }

    #[test]
    fn defaults_are_go_zero_values() {
        let p = YamlParser::default();
        assert_eq!(p.error, YamlErrorType::No);
        assert_eq!(p.state, YamlParserState::StreamStart);
        assert!(p.read_handler.is_none() && p.input.is_empty());
        let e = YamlEmitter::default();
        assert_eq!(e.state, YamlEmitterState::StreamStart);
        assert_eq!(e.best_width, 0);
        assert_eq!(YamlEvent::default().typ, YamlEventType::No);
        assert_eq!(YamlToken::default().typ, YamlTokenType::No);
    }
}
