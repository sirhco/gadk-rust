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

//! Port of yaml.v3 v3.0.1 `apic.go`: parser and emitter construction, input/output handlers,
//! settings, event constructors and the token queue insert. The libyaml C code apic.go keeps
//! commented out (token/document constructors, `yaml_check_utf8`) is not ported.
//!
//! Go's constructors take the event by pointer and overwrite it; the Rust ones take
//! `&mut YamlEvent` and the byte fields by value (Go shares the caller's slices; every yaml.v3
//! caller passes a fresh copy). Go's string read handler reads from a borrowed slice
//! (`YamlParser::input`); Go's string write handler appends to the encoder's `out` through a
//! pointer, which is the emitter-owned `YamlEmitter::output_buffer` here.
// Until Tasks 3–7 call these functions; Task 12 removes this line.
#![allow(dead_code)]

use std::io::{Read, Write};

use crate::yamlh::{
    INITIAL_QUEUE_SIZE, INITIAL_STACK_SIZE, INPUT_BUFFER_SIZE, INPUT_RAW_BUFFER_SIZE,
    OUTPUT_BUFFER_SIZE, OUTPUT_RAW_BUFFER_SIZE, YamlBreak, YamlEmitter, YamlEncoding, YamlEvent,
    YamlEventType, YamlMappingStyle, YamlParser, YamlScalarStyle, YamlSequenceStyle, YamlStyle,
    YamlTagDirective, YamlToken, YamlVersionDirective,
};

/// Insert `token` into the queue at `pos` past the head (`pos < 0` appends).
pub(crate) fn yaml_insert_token(parser: &mut YamlParser<'_>, pos: isize, token: YamlToken) {
    // Go: apic.go yaml_insert_token
    //fmt.Println("yaml_insert_token", "pos:", pos, "typ:", token.typ, "head:", parser.tokens_head, "len:", len(parser.tokens))

    // Check if we can move the queue at the beginning of the buffer.
    if parser.tokens_head > 0 && parser.tokens.len() == parser.tokens.capacity() {
        parser.tokens.drain(..parser.tokens_head);
        parser.tokens_head = 0;
    }
    if pos < 0 {
        parser.tokens.push(token);
        return;
    }
    // Go appends, shifts tokens[head+pos:] right by one and stores the token at head+pos.
    let at = parser.tokens_head + pos.unsigned_abs();
    parser.tokens.insert(at, token);
}

/// Create a new parser object.
pub(crate) fn yaml_parser_initialize(parser: &mut YamlParser<'_>) -> bool {
    // Go: apic.go yaml_parser_initialize
    *parser = YamlParser {
        raw_buffer: Vec::with_capacity(INPUT_RAW_BUFFER_SIZE),
        buffer: Vec::with_capacity(INPUT_BUFFER_SIZE),
        ..YamlParser::default()
    };
    true
}

/// Destroy a parser object.
pub(crate) fn yaml_parser_delete(parser: &mut YamlParser<'_>) {
    // Go: apic.go yaml_parser_delete
    *parser = YamlParser::default();
}

/// String read handler.
pub(crate) fn yaml_string_read_handler(
    parser: &mut YamlParser<'_>,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    // Go: apic.go yaml_string_read_handler
    if parser.input_pos == parser.input.len() {
        return Ok(0); // Go: (0, io.EOF)
    }
    let rest = &parser.input[parser.input_pos..];
    let n = rest.len().min(buffer.len());
    buffer[..n].copy_from_slice(&rest[..n]);
    parser.input_pos += n;
    Ok(n)
}

/// Reader read handler. Rust's `Interrupted` errors are retried (Go's `io.Reader` has none).
pub(crate) fn yaml_reader_read_handler(
    parser: &mut YamlParser<'_>,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    // Go: apic.go yaml_reader_read_handler
    let Some(reader) = parser.input_reader.as_mut() else {
        panic!("read handler called without an input reader");
    };
    loop {
        match reader.read(buffer) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// Set a string input.
pub(crate) fn yaml_parser_set_input_string<'r>(parser: &mut YamlParser<'r>, input: &'r [u8]) {
    // Go: apic.go yaml_parser_set_input_string
    if parser.read_handler.is_some() {
        panic!("must set the input source only once");
    }
    parser.read_handler = Some(yaml_string_read_handler);
    parser.input = input;
    parser.input_pos = 0;
}

/// Set a file input.
pub(crate) fn yaml_parser_set_input_reader<'r>(parser: &mut YamlParser<'r>, r: Box<dyn Read + 'r>) {
    // Go: apic.go yaml_parser_set_input_reader
    if parser.read_handler.is_some() {
        panic!("must set the input source only once");
    }
    parser.read_handler = Some(yaml_reader_read_handler);
    parser.input_reader = Some(r);
}

/// Set the source encoding.
pub(crate) fn yaml_parser_set_encoding(parser: &mut YamlParser<'_>, encoding: YamlEncoding) {
    // Go: apic.go yaml_parser_set_encoding
    if parser.encoding != YamlEncoding::Any {
        panic!("must set the encoding only once");
    }
    parser.encoding = encoding;
}

/// Create a new emitter object.
pub(crate) fn yaml_emitter_initialize(emitter: &mut YamlEmitter<'_>) {
    // Go: apic.go yaml_emitter_initialize
    *emitter = YamlEmitter {
        buffer: vec![0; OUTPUT_BUFFER_SIZE],
        raw_buffer: Vec::with_capacity(OUTPUT_RAW_BUFFER_SIZE),
        states: Vec::with_capacity(INITIAL_STACK_SIZE),
        events: Vec::with_capacity(INITIAL_QUEUE_SIZE),
        best_width: -1,
        ..YamlEmitter::default()
    };
}

/// Destroy an emitter object.
pub(crate) fn yaml_emitter_delete(emitter: &mut YamlEmitter<'_>) {
    // Go: apic.go yaml_emitter_delete
    *emitter = YamlEmitter::default();
}

/// String write handler: appends to `emitter.output_buffer` (Go: `*emitter.output_buffer`).
pub(crate) fn yaml_string_write_handler(
    emitter: &mut YamlEmitter<'_>,
    buffer: &[u8],
) -> std::io::Result<()> {
    // Go: apic.go yaml_string_write_handler
    emitter.output_buffer.extend_from_slice(buffer);
    Ok(())
}

/// Uses `emitter.output_writer` to write the emitted text.
pub(crate) fn yaml_writer_write_handler(
    emitter: &mut YamlEmitter<'_>,
    buffer: &[u8],
) -> std::io::Result<()> {
    // Go: apic.go yaml_writer_write_handler
    let Some(writer) = emitter.output_writer.as_mut() else {
        panic!("write handler called without an output writer");
    };
    writer.write_all(buffer)
}

/// Set a string output: the emitted text accumulates in `emitter.output_buffer`.
pub(crate) fn yaml_emitter_set_output_string(emitter: &mut YamlEmitter<'_>) {
    // Go: apic.go yaml_emitter_set_output_string
    if emitter.write_handler.is_some() {
        panic!("must set the output target only once");
    }
    emitter.write_handler = Some(yaml_string_write_handler);
    emitter.output_buffer = Vec::new();
}

/// Set a file output.
pub(crate) fn yaml_emitter_set_output_writer<'w>(
    emitter: &mut YamlEmitter<'w>,
    w: Box<dyn Write + 'w>,
) {
    // Go: apic.go yaml_emitter_set_output_writer
    if emitter.write_handler.is_some() {
        panic!("must set the output target only once");
    }
    emitter.write_handler = Some(yaml_writer_write_handler);
    emitter.output_writer = Some(w);
}

/// Set the output encoding.
pub(crate) fn yaml_emitter_set_encoding(emitter: &mut YamlEmitter<'_>, encoding: YamlEncoding) {
    // Go: apic.go yaml_emitter_set_encoding
    if emitter.encoding != YamlEncoding::Any {
        panic!("must set the output encoding only once");
    }
    emitter.encoding = encoding;
}

/// Set the canonical output style.
pub(crate) fn yaml_emitter_set_canonical(emitter: &mut YamlEmitter<'_>, canonical: bool) {
    // Go: apic.go yaml_emitter_set_canonical
    emitter.canonical = canonical;
}

/// Set the indentation increment (values outside 2..=9 become 2).
pub(crate) fn yaml_emitter_set_indent(emitter: &mut YamlEmitter<'_>, indent: isize) {
    // Go: apic.go yaml_emitter_set_indent
    let indent = if !(2..=9).contains(&indent) {
        2
    } else {
        indent
    };
    emitter.best_indent = indent;
}

/// Set the preferred line width (negative values become -1, "unlimited").
pub(crate) fn yaml_emitter_set_width(emitter: &mut YamlEmitter<'_>, width: isize) {
    // Go: apic.go yaml_emitter_set_width
    let width = if width < 0 { -1 } else { width };
    emitter.best_width = width;
}

/// Set if unescaped non-ASCII characters are allowed.
pub(crate) fn yaml_emitter_set_unicode(emitter: &mut YamlEmitter<'_>, unicode: bool) {
    // Go: apic.go yaml_emitter_set_unicode
    emitter.unicode = unicode;
}

/// Set the preferred line break character.
pub(crate) fn yaml_emitter_set_break(emitter: &mut YamlEmitter<'_>, line_break: YamlBreak) {
    // Go: apic.go yaml_emitter_set_break
    emitter.line_break = line_break;
}

/// Create STREAM-START.
pub(crate) fn yaml_stream_start_event_initialize(event: &mut YamlEvent, encoding: YamlEncoding) {
    // Go: apic.go yaml_stream_start_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::StreamStart,
        encoding,
        ..YamlEvent::default()
    };
}

/// Create STREAM-END.
pub(crate) fn yaml_stream_end_event_initialize(event: &mut YamlEvent) {
    // Go: apic.go yaml_stream_end_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::StreamEnd,
        ..YamlEvent::default()
    };
}

/// Create DOCUMENT-START.
pub(crate) fn yaml_document_start_event_initialize(
    event: &mut YamlEvent,
    version_directive: Option<YamlVersionDirective>,
    tag_directives: Vec<YamlTagDirective>,
    implicit: bool,
) {
    // Go: apic.go yaml_document_start_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::DocumentStart,
        version_directive,
        tag_directives,
        implicit,
        ..YamlEvent::default()
    };
}

/// Create DOCUMENT-END.
pub(crate) fn yaml_document_end_event_initialize(event: &mut YamlEvent, implicit: bool) {
    // Go: apic.go yaml_document_end_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::DocumentEnd,
        implicit,
        ..YamlEvent::default()
    };
}

/// Create ALIAS.
pub(crate) fn yaml_alias_event_initialize(event: &mut YamlEvent, anchor: Vec<u8>) -> bool {
    // Go: apic.go yaml_alias_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::Alias,
        anchor,
        ..YamlEvent::default()
    };
    true
}

/// Create SCALAR.
#[allow(clippy::too_many_arguments)] // Go's signature.
pub(crate) fn yaml_scalar_event_initialize(
    event: &mut YamlEvent,
    anchor: Vec<u8>,
    tag: Vec<u8>,
    value: Vec<u8>,
    plain_implicit: bool,
    quoted_implicit: bool,
    style: YamlScalarStyle,
) -> bool {
    // Go: apic.go yaml_scalar_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::Scalar,
        anchor,
        tag,
        value,
        implicit: plain_implicit,
        quoted_implicit,
        style: style as YamlStyle,
        ..YamlEvent::default()
    };
    true
}

/// Create SEQUENCE-START.
pub(crate) fn yaml_sequence_start_event_initialize(
    event: &mut YamlEvent,
    anchor: Vec<u8>,
    tag: Vec<u8>,
    implicit: bool,
    style: YamlSequenceStyle,
) -> bool {
    // Go: apic.go yaml_sequence_start_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::SequenceStart,
        anchor,
        tag,
        implicit,
        style: style as YamlStyle,
        ..YamlEvent::default()
    };
    true
}

/// Create SEQUENCE-END.
pub(crate) fn yaml_sequence_end_event_initialize(event: &mut YamlEvent) -> bool {
    // Go: apic.go yaml_sequence_end_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::SequenceEnd,
        ..YamlEvent::default()
    };
    true
}

/// Create MAPPING-START.
pub(crate) fn yaml_mapping_start_event_initialize(
    event: &mut YamlEvent,
    anchor: Vec<u8>,
    tag: Vec<u8>,
    implicit: bool,
    style: YamlMappingStyle,
) {
    // Go: apic.go yaml_mapping_start_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::MappingStart,
        anchor,
        tag,
        implicit,
        style: style as YamlStyle,
        ..YamlEvent::default()
    };
}

/// Create MAPPING-END.
pub(crate) fn yaml_mapping_end_event_initialize(event: &mut YamlEvent) {
    // Go: apic.go yaml_mapping_end_event_initialize
    *event = YamlEvent {
        typ: YamlEventType::MappingEnd,
        ..YamlEvent::default()
    };
}

/// Destroy an event object.
pub(crate) fn yaml_event_delete(event: &mut YamlEvent) {
    // Go: apic.go yaml_event_delete
    *event = YamlEvent::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yamlh::{YamlErrorType, YamlTokenType};

    fn tok(typ: YamlTokenType, v: &str) -> YamlToken {
        YamlToken {
            typ,
            value: v.as_bytes().to_vec(),
            ..YamlToken::default()
        }
    }

    fn values(p: &YamlParser<'_>) -> Vec<String> {
        p.tokens[p.tokens_head..]
            .iter()
            .map(|t| String::from_utf8_lossy(&t.value).into_owned())
            .collect()
    }

    #[test]
    fn insert_token_appends_and_inserts_past_head() {
        let mut p = YamlParser::default();
        yaml_insert_token(&mut p, -1, tok(YamlTokenType::Scalar, "a"));
        yaml_insert_token(&mut p, -1, tok(YamlTokenType::Scalar, "c"));
        yaml_insert_token(&mut p, 1, tok(YamlTokenType::Key, "b"));
        assert_eq!(values(&p), ["a", "b", "c"]);
        p.tokens_head = 1;
        yaml_insert_token(&mut p, 0, tok(YamlTokenType::Key, "x"));
        assert_eq!(values(&p), ["x", "b", "c"]);
        // Appending at pos == len past the head is an append.
        yaml_insert_token(&mut p, 3, tok(YamlTokenType::Value, "z"));
        assert_eq!(values(&p), ["x", "b", "c", "z"]);
    }

    #[test]
    fn insert_token_compacts_a_full_queue() {
        let mut p = YamlParser {
            tokens: Vec::with_capacity(2),
            ..YamlParser::default()
        };
        yaml_insert_token(&mut p, -1, tok(YamlTokenType::Scalar, "a"));
        yaml_insert_token(&mut p, -1, tok(YamlTokenType::Scalar, "b"));
        let cap = p.tokens.capacity();
        // Fill to capacity so the next insert must move the queue to the front.
        while p.tokens.len() < cap {
            yaml_insert_token(&mut p, -1, tok(YamlTokenType::Scalar, "f"));
        }
        p.tokens_head = 1;
        yaml_insert_token(&mut p, 0, tok(YamlTokenType::Key, "k"));
        assert_eq!(p.tokens_head, 0);
        assert_eq!(p.tokens[0].value, b"k");
        assert_eq!(p.tokens[1].value, b"b");
        assert_eq!(p.tokens.len(), cap);
        // A fully consumed full queue is emptied before appending.
        let mut q = YamlParser {
            tokens: Vec::with_capacity(1),
            ..YamlParser::default()
        };
        yaml_insert_token(&mut q, -1, tok(YamlTokenType::Scalar, "a"));
        while q.tokens.len() < q.tokens.capacity() {
            yaml_insert_token(&mut q, -1, tok(YamlTokenType::Scalar, "a"));
        }
        q.tokens_head = q.tokens.len();
        yaml_insert_token(&mut q, -1, tok(YamlTokenType::Scalar, "n"));
        assert_eq!(q.tokens_head, 0);
        assert_eq!(values(&q), ["n"]);
    }

    #[test]
    fn parser_initialize_and_string_input() {
        let mut p = YamlParser {
            error: YamlErrorType::Scanner,
            ..YamlParser::default()
        };
        assert!(yaml_parser_initialize(&mut p));
        assert_eq!(p.error, YamlErrorType::No);
        assert!(p.raw_buffer.is_empty() && p.raw_buffer.capacity() >= INPUT_RAW_BUFFER_SIZE);
        assert!(p.buffer.capacity() >= INPUT_BUFFER_SIZE);
        let input = b"hello world";
        yaml_parser_set_input_string(&mut p, input);
        let handler = p.read_handler.expect("handler set");
        let mut buf = [0u8; 5];
        assert_eq!(handler(&mut p, &mut buf).ok(), Some(5));
        assert_eq!(&buf, b"hello");
        let mut big = [0u8; 16];
        assert_eq!(handler(&mut p, &mut big).ok(), Some(6));
        assert_eq!(&big[..6], b" world");
        assert_eq!(handler(&mut p, &mut big).ok(), Some(0)); // io.EOF
        yaml_parser_delete(&mut p);
        assert!(p.read_handler.is_none() && p.input.is_empty());
    }

    #[test]
    #[should_panic(expected = "must set the input source only once")]
    fn parser_input_set_twice_panics() {
        let mut p = YamlParser::default();
        yaml_parser_set_input_string(&mut p, b"a");
        yaml_parser_set_input_reader(&mut p, Box::new(&b"b"[..]));
    }

    #[test]
    fn reader_input() {
        let mut p = YamlParser::default();
        let data = b"abc".to_vec();
        yaml_parser_set_input_reader(&mut p, Box::new(std::io::Cursor::new(data)));
        let handler = p.read_handler.expect("handler set");
        let mut buf = [0u8; 8];
        assert_eq!(handler(&mut p, &mut buf).ok(), Some(3));
        assert_eq!(&buf[..3], b"abc");
        assert_eq!(handler(&mut p, &mut buf).ok(), Some(0));
    }

    #[test]
    fn reader_errors_pass_through() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
        }
        let mut p = YamlParser::default();
        yaml_parser_set_input_reader(&mut p, Box::new(Failing));
        let handler = p.read_handler.expect("handler set");
        let err = handler(&mut p, &mut [0u8; 4]).err().map(|e| e.to_string());
        assert_eq!(err.as_deref(), Some("boom"));
    }

    #[test]
    #[should_panic(expected = "must set the encoding only once")]
    fn parser_encoding_once() {
        let mut p = YamlParser::default();
        yaml_parser_set_encoding(&mut p, YamlEncoding::Utf16le);
        assert_eq!(p.encoding, YamlEncoding::Utf16le);
        yaml_parser_set_encoding(&mut p, YamlEncoding::Utf8);
    }

    #[test]
    fn emitter_initialize_and_settings() {
        let mut e = YamlEmitter {
            canonical: true,
            ..YamlEmitter::default()
        };
        yaml_emitter_initialize(&mut e);
        assert!(!e.canonical);
        assert_eq!(e.best_width, -1);
        assert_eq!(e.buffer.len(), OUTPUT_BUFFER_SIZE);
        assert!(e.raw_buffer.is_empty() && e.raw_buffer.capacity() >= OUTPUT_RAW_BUFFER_SIZE);
        assert!(e.states.capacity() >= INITIAL_STACK_SIZE);
        assert!(e.events.capacity() >= INITIAL_QUEUE_SIZE);
        for (given, want) in [(1, 2), (2, 2), (4, 4), (9, 9), (10, 2), (-3, 2)] {
            yaml_emitter_set_indent(&mut e, given);
            assert_eq!(e.best_indent, want, "indent {given}");
        }
        for (given, want) in [(-5, -1), (-1, -1), (0, 0), (80, 80)] {
            yaml_emitter_set_width(&mut e, given);
            assert_eq!(e.best_width, want, "width {given}");
        }
        yaml_emitter_set_canonical(&mut e, true);
        yaml_emitter_set_unicode(&mut e, true);
        yaml_emitter_set_break(&mut e, YamlBreak::Crln);
        assert!(e.canonical && e.unicode && e.line_break == YamlBreak::Crln);
        yaml_emitter_set_encoding(&mut e, YamlEncoding::Utf8);
        assert_eq!(e.encoding, YamlEncoding::Utf8);
        yaml_emitter_delete(&mut e);
        assert_eq!(e.best_width, 0);
        assert!(e.buffer.is_empty());
    }

    #[test]
    fn emitter_string_output() {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_string(&mut e);
        let handler = e.write_handler.expect("handler set");
        assert!(handler(&mut e, b"a: ").is_ok());
        assert!(handler(&mut e, b"1\n").is_ok());
        assert_eq!(e.output_buffer, b"a: 1\n");
    }

    #[test]
    fn emitter_writer_output() {
        let mut out = Vec::new();
        {
            let mut e = YamlEmitter::default();
            yaml_emitter_set_output_writer(&mut e, Box::new(&mut out));
            let handler = e.write_handler.expect("handler set");
            assert!(handler(&mut e, b"x\n").is_ok());
        }
        assert_eq!(out, b"x\n");
    }

    #[test]
    #[should_panic(expected = "must set the output target only once")]
    fn emitter_output_set_twice_panics() {
        let mut e = YamlEmitter::default();
        yaml_emitter_set_output_string(&mut e);
        yaml_emitter_set_output_string(&mut e);
    }

    #[test]
    fn event_constructors() {
        let mut ev = YamlEvent {
            value: b"stale".to_vec(),
            ..YamlEvent::default()
        };
        yaml_stream_start_event_initialize(&mut ev, YamlEncoding::Utf8);
        assert_eq!(
            ev,
            YamlEvent {
                typ: YamlEventType::StreamStart,
                encoding: YamlEncoding::Utf8,
                ..YamlEvent::default()
            }
        );
        yaml_stream_end_event_initialize(&mut ev);
        assert_eq!(ev.typ, YamlEventType::StreamEnd);

        let vd = YamlVersionDirective { major: 1, minor: 1 };
        let td = vec![YamlTagDirective {
            handle: b"!e!".to_vec(),
            prefix: b"tag:e,".to_vec(),
        }];
        yaml_document_start_event_initialize(&mut ev, Some(vd), td.clone(), true);
        assert_eq!(ev.typ, YamlEventType::DocumentStart);
        assert_eq!(ev.version_directive, Some(vd));
        assert_eq!(ev.tag_directives, td);
        assert!(ev.implicit);
        yaml_document_end_event_initialize(&mut ev, false);
        assert_eq!((ev.typ, ev.implicit), (YamlEventType::DocumentEnd, false));
        assert!(ev.tag_directives.is_empty());

        assert!(yaml_alias_event_initialize(&mut ev, b"a".to_vec()));
        assert_eq!(
            (ev.typ, ev.anchor.as_slice()),
            (YamlEventType::Alias, &b"a"[..])
        );

        assert!(yaml_scalar_event_initialize(
            &mut ev,
            b"anc".to_vec(),
            b"tag:yaml.org,2002:str".to_vec(),
            b"v".to_vec(),
            false,
            true,
            YamlScalarStyle::DoubleQuoted,
        ));
        assert_eq!(ev.typ, YamlEventType::Scalar);
        assert_eq!(ev.anchor, b"anc");
        assert_eq!(ev.tag, b"tag:yaml.org,2002:str");
        assert_eq!(ev.value, b"v");
        assert!(!ev.implicit && ev.quoted_implicit);
        assert_eq!(ev.scalar_style(), YamlScalarStyle::DoubleQuoted);

        assert!(yaml_sequence_start_event_initialize(
            &mut ev,
            Vec::new(),
            Vec::new(),
            true,
            YamlSequenceStyle::Flow
        ));
        assert_eq!(ev.typ, YamlEventType::SequenceStart);
        assert_eq!(ev.sequence_style(), YamlSequenceStyle::Flow);
        assert!(ev.value.is_empty());
        assert!(yaml_sequence_end_event_initialize(&mut ev));
        assert_eq!(ev.typ, YamlEventType::SequenceEnd);

        yaml_mapping_start_event_initialize(
            &mut ev,
            b"m".to_vec(),
            Vec::new(),
            false,
            YamlMappingStyle::Block,
        );
        assert_eq!(ev.typ, YamlEventType::MappingStart);
        assert_eq!(ev.mapping_style(), YamlMappingStyle::Block);
        assert_eq!(ev.anchor, b"m");
        yaml_mapping_end_event_initialize(&mut ev);
        assert_eq!(ev.typ, YamlEventType::MappingEnd);

        yaml_event_delete(&mut ev);
        assert_eq!(ev, YamlEvent::default());
    }
}
