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

//! The event stream from tokens (the parser state machine, directives, comment events).
//!
//! The parser implements the following grammar:
//!
//! ```text
//! stream               ::= STREAM-START implicit_document? explicit_document* STREAM-END
//! implicit_document    ::= block_node DOCUMENT-END*
//! explicit_document    ::= DIRECTIVE* DOCUMENT-START block_node? DOCUMENT-END*
//! block_node_or_indentless_sequence    ::=
//!                          ALIAS
//!                          | properties (block_content | indentless_block_sequence)?
//!                          | block_content
//!                          | indentless_block_sequence
//! block_node           ::= ALIAS
//!                          | properties block_content?
//!                          | block_content
//! flow_node            ::= ALIAS
//!                          | properties flow_content?
//!                          | flow_content
//! properties           ::= TAG ANCHOR? | ANCHOR TAG?
//! block_content        ::= block_collection | flow_collection | SCALAR
//! flow_content         ::= flow_collection | SCALAR
//! block_collection     ::= block_sequence | block_mapping
//! flow_collection      ::= flow_sequence | flow_mapping
//! block_sequence       ::= BLOCK-SEQUENCE-START (BLOCK-ENTRY block_node?)* BLOCK-END
//! indentless_sequence  ::= (BLOCK-ENTRY block_node?)+
//! block_mapping        ::= BLOCK-MAPPING_START
//!                          ((KEY block_node_or_indentless_sequence?)?
//!                          (VALUE block_node_or_indentless_sequence?)?)*
//!                          BLOCK-END
//! flow_sequence        ::= FLOW-SEQUENCE-START
//!                          (flow_sequence_entry FLOW-ENTRY)*
//!                          flow_sequence_entry?
//!                          FLOW-SEQUENCE-END
//! flow_sequence_entry  ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//! flow_mapping         ::= FLOW-MAPPING-START
//!                          (flow_mapping_entry FLOW-ENTRY)*
//!                          flow_mapping_entry?
//!                          FLOW-MAPPING-END
//! flow_mapping_entry   ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//! ```
//!
//! `peek_token` returns whether a token is available and [`token`] reads the queue head, so the
//! parser state can be mutated while the token's fields (copied out first) are in use. Event
//! and token byte slices are owned `Vec<u8>` copies.

use crate::emitter::default_tag_directives;
use crate::scanner::yaml_parser_fetch_more_tokens;
use crate::yamlh::{
    YamlComment, YamlErrorType, YamlEvent, YamlEventType, YamlMappingStyle, YamlMark, YamlParser,
    YamlParserState, YamlScalarStyle, YamlSequenceStyle, YamlStyle, YamlTagDirective, YamlToken,
    YamlTokenType, YamlVersionDirective,
};

/// The token at the head of the queue (available after `peek_token`). Only valid after
/// `peek_token` returned true.
fn token<'a>(parser: &'a YamlParser<'_>) -> &'a YamlToken {
    &parser.tokens[parser.tokens_head]
}

/// Pops the state stack into `parser.state`.
fn pop_state(parser: &mut YamlParser<'_>) {
    let Some(state) = parser.states.pop() else {
        panic!("parser state stack underflow");
    };
    parser.state = state;
}

/// Pops and returns the top of the mark stack.
fn pop_mark(parser: &mut YamlParser<'_>) -> YamlMark {
    let Some(mark) = parser.marks.pop() else {
        panic!("parser mark stack underflow");
    };
    mark
}

/// Peek the next token in the token queue (false on failure).
fn peek_token(parser: &mut YamlParser<'_>) -> bool {
    if parser.token_available || yaml_parser_fetch_more_tokens(parser) {
        yaml_parser_unfold_comments(parser);
        return true;
    }
    false
}

/// yaml_parser_unfold_comments walks through the comments queue and joins all comments behind
/// the position of the provided token (the queue head) into the respective top-level comment
/// slices in the parser.
fn yaml_parser_unfold_comments(parser: &mut YamlParser<'_>) {
    let (token_typ, token_index) = {
        let token = token(parser);
        (token.typ, token.start_mark.index)
    };
    while parser.comments_head < parser.comments.len()
        && token_index >= parser.comments[parser.comments_head].token_mark.index
    {
        let comment = &mut parser.comments[parser.comments_head];
        if !comment.head.is_empty() {
            if token_typ == YamlTokenType::BlockEnd {
                // No heads on ends, so keep comment.head for a follow up token.
                break;
            }
            if !parser.head_comment.is_empty() {
                parser.head_comment.push(b'\n');
            }
            parser.head_comment.extend_from_slice(&comment.head);
        }
        if !comment.foot.is_empty() {
            if !parser.foot_comment.is_empty() {
                parser.foot_comment.push(b'\n');
            }
            parser.foot_comment.extend_from_slice(&comment.foot);
        }
        if !comment.line.is_empty() {
            if !parser.line_comment.is_empty() {
                parser.line_comment.push(b'\n');
            }
            parser.line_comment.extend_from_slice(&comment.line);
        }
        // Zeroed, never removed: the scanner indexes `parser.comments` from 0.
        *comment = YamlComment::default();
        parser.comments_head += 1;
    }
}

/// Remove the next token from the queue (must be called after peek_token).
fn skip_token(parser: &mut YamlParser<'_>) {
    parser.token_available = false;
    parser.tokens_parsed += 1;
    parser.stream_end_produced = parser.tokens[parser.tokens_head].typ == YamlTokenType::StreamEnd;
    parser.tokens_head += 1;
}

/// Get the next event. Returns false on error (set on `parser`); after STREAM-END or an error
/// it keeps returning true with an empty event.
pub(crate) fn yaml_parser_parse(parser: &mut YamlParser<'_>, event: &mut YamlEvent) -> bool {
    // Erase the event object.
    *event = YamlEvent::default();

    // No events after the end of the stream or error.
    if parser.stream_end_produced
        || parser.error != YamlErrorType::No
        || parser.state == YamlParserState::End
    {
        return true;
    }

    // Generate the next event.
    yaml_parser_state_machine(parser, event)
}

/// Set parser error.
fn yaml_parser_set_parser_error(
    parser: &mut YamlParser<'_>,
    problem: &str,
    problem_mark: YamlMark,
) -> bool {
    parser.error = YamlErrorType::Parser;
    parser.problem = problem.to_string();
    parser.problem_mark = problem_mark;
    false
}

fn yaml_parser_set_parser_error_context(
    parser: &mut YamlParser<'_>,
    context: &str,
    context_mark: YamlMark,
    problem: &str,
    problem_mark: YamlMark,
) -> bool {
    parser.error = YamlErrorType::Parser;
    parser.context = context.to_string();
    parser.context_mark = context_mark;
    parser.problem = problem.to_string();
    parser.problem_mark = problem_mark;
    false
}

/// State dispatcher.
fn yaml_parser_state_machine(parser: &mut YamlParser<'_>, event: &mut YamlEvent) -> bool {
    match parser.state {
        YamlParserState::StreamStart => yaml_parser_parse_stream_start(parser, event),
        YamlParserState::ImplicitDocumentStart => {
            yaml_parser_parse_document_start(parser, event, true)
        }
        YamlParserState::DocumentStart => yaml_parser_parse_document_start(parser, event, false),
        YamlParserState::DocumentContent => yaml_parser_parse_document_content(parser, event),
        YamlParserState::DocumentEnd => yaml_parser_parse_document_end(parser, event),
        YamlParserState::BlockNode => yaml_parser_parse_node(parser, event, true, false),
        YamlParserState::BlockNodeOrIndentlessSequence => {
            yaml_parser_parse_node(parser, event, true, true)
        }
        YamlParserState::FlowNode => yaml_parser_parse_node(parser, event, false, false),
        YamlParserState::BlockSequenceFirstEntry => {
            yaml_parser_parse_block_sequence_entry(parser, event, true)
        }
        YamlParserState::BlockSequenceEntry => {
            yaml_parser_parse_block_sequence_entry(parser, event, false)
        }
        YamlParserState::IndentlessSequenceEntry => {
            yaml_parser_parse_indentless_sequence_entry(parser, event)
        }
        YamlParserState::BlockMappingFirstKey => {
            yaml_parser_parse_block_mapping_key(parser, event, true)
        }
        YamlParserState::BlockMappingKey => {
            yaml_parser_parse_block_mapping_key(parser, event, false)
        }
        YamlParserState::BlockMappingValue => yaml_parser_parse_block_mapping_value(parser, event),
        YamlParserState::FlowSequenceFirstEntry => {
            yaml_parser_parse_flow_sequence_entry(parser, event, true)
        }
        YamlParserState::FlowSequenceEntry => {
            yaml_parser_parse_flow_sequence_entry(parser, event, false)
        }
        YamlParserState::FlowSequenceEntryMappingKey => {
            yaml_parser_parse_flow_sequence_entry_mapping_key(parser, event)
        }
        YamlParserState::FlowSequenceEntryMappingValue => {
            yaml_parser_parse_flow_sequence_entry_mapping_value(parser, event)
        }
        YamlParserState::FlowSequenceEntryMappingEnd => {
            yaml_parser_parse_flow_sequence_entry_mapping_end(parser, event)
        }
        YamlParserState::FlowMappingFirstKey => {
            yaml_parser_parse_flow_mapping_key(parser, event, true)
        }
        YamlParserState::FlowMappingKey => yaml_parser_parse_flow_mapping_key(parser, event, false),
        YamlParserState::FlowMappingValue => {
            yaml_parser_parse_flow_mapping_value(parser, event, false)
        }
        YamlParserState::FlowMappingEmptyValue => {
            yaml_parser_parse_flow_mapping_value(parser, event, true)
        }
        YamlParserState::End => panic!("invalid parser state"),
    }
}

// Parse the production:
// stream   ::= STREAM-START implicit_document? explicit_document* STREAM-END
//              ************
fn yaml_parser_parse_stream_start(parser: &mut YamlParser<'_>, event: &mut YamlEvent) -> bool {
    if !peek_token(parser) {
        return false;
    }
    let token = token(parser);
    if token.typ != YamlTokenType::StreamStart {
        let mark = token.start_mark;
        return yaml_parser_set_parser_error(parser, "did not find expected <stream-start>", mark);
    }
    *event = YamlEvent {
        typ: YamlEventType::StreamStart,
        start_mark: token.start_mark,
        end_mark: token.end_mark,
        encoding: token.encoding,
        ..YamlEvent::default()
    };
    parser.state = YamlParserState::ImplicitDocumentStart;
    skip_token(parser);
    true
}

// Parse the productions:
// implicit_document    ::= block_node DOCUMENT-END*
//                          *
// explicit_document    ::= DIRECTIVE* DOCUMENT-START block_node? DOCUMENT-END*
//                          *************************
fn yaml_parser_parse_document_start(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    implicit: bool,
) -> bool {
    if !peek_token(parser) {
        return false;
    }

    // Parse extra document end indicators.
    if !implicit {
        while token(parser).typ == YamlTokenType::DocumentEnd {
            skip_token(parser);
            if !peek_token(parser) {
                return false;
            }
        }
    }

    let typ = token(parser).typ;
    if implicit
        && typ != YamlTokenType::VersionDirective
        && typ != YamlTokenType::TagDirective
        && typ != YamlTokenType::DocumentStart
        && typ != YamlTokenType::StreamEnd
    {
        // Parse an implicit document.
        if !yaml_parser_process_directives(parser, None, None) {
            return false;
        }
        parser.states.push(YamlParserState::DocumentEnd);
        parser.state = YamlParserState::BlockNode;

        let mut head_comment = Vec::new();
        if !parser.head_comment.is_empty() {
            // Scan the header comment backwards, and if an empty line is found, break the
            // header so the part before the last empty line goes into the document header,
            // while the bottom of it goes into a follow up event.
            let len = parser.head_comment.len();
            let mut i = len - 1;
            while i > 0 {
                if parser.head_comment[i] == b'\n' {
                    if i == len - 1 {
                        head_comment = parser.head_comment[..i].to_vec();
                        parser.head_comment = parser.head_comment[i + 1..].to_vec();
                        break;
                    } else if parser.head_comment[i - 1] == b'\n' {
                        head_comment = parser.head_comment[..i - 1].to_vec();
                        parser.head_comment = parser.head_comment[i + 1..].to_vec();
                        break;
                    }
                }
                i -= 1;
            }
        }

        let token = token(parser);
        *event = YamlEvent {
            typ: YamlEventType::DocumentStart,
            start_mark: token.start_mark,
            end_mark: token.end_mark,

            head_comment,
            ..YamlEvent::default()
        };
    } else if typ != YamlTokenType::StreamEnd {
        // Parse an explicit document.
        let mut version_directive = None;
        let mut tag_directives = Vec::new();
        let start_mark = token(parser).start_mark;
        if !yaml_parser_process_directives(
            parser,
            Some(&mut version_directive),
            Some(&mut tag_directives),
        ) {
            return false;
        }
        if !peek_token(parser) {
            return false;
        }
        let token = token(parser);
        if token.typ != YamlTokenType::DocumentStart {
            let mark = token.start_mark;
            yaml_parser_set_parser_error(parser, "did not find expected <document start>", mark);
            return false;
        }
        let end_mark = token.end_mark;
        parser.states.push(YamlParserState::DocumentEnd);
        parser.state = YamlParserState::DocumentContent;

        *event = YamlEvent {
            typ: YamlEventType::DocumentStart,
            start_mark,
            end_mark,
            version_directive,
            tag_directives,
            implicit: false,
            ..YamlEvent::default()
        };
        skip_token(parser);
    } else {
        // Parse the stream end.
        let token = token(parser);
        *event = YamlEvent {
            typ: YamlEventType::StreamEnd,
            start_mark: token.start_mark,
            end_mark: token.end_mark,
            ..YamlEvent::default()
        };
        parser.state = YamlParserState::End;
        skip_token(parser);
    }

    true
}

// Parse the productions:
// explicit_document    ::= DIRECTIVE* DOCUMENT-START block_node? DOCUMENT-END*
//                                                    ***********
//
fn yaml_parser_parse_document_content(parser: &mut YamlParser<'_>, event: &mut YamlEvent) -> bool {
    if !peek_token(parser) {
        return false;
    }

    let token = token(parser);
    if token.typ == YamlTokenType::VersionDirective
        || token.typ == YamlTokenType::TagDirective
        || token.typ == YamlTokenType::DocumentStart
        || token.typ == YamlTokenType::DocumentEnd
        || token.typ == YamlTokenType::StreamEnd
    {
        let mark = token.start_mark;
        pop_state(parser);
        return yaml_parser_process_empty_scalar(event, mark);
    }
    yaml_parser_parse_node(parser, event, true, false)
}

// Parse the productions:
// implicit_document    ::= block_node DOCUMENT-END*
//                                     *************
// explicit_document    ::= DIRECTIVE* DOCUMENT-START block_node? DOCUMENT-END*
//
fn yaml_parser_parse_document_end(parser: &mut YamlParser<'_>, event: &mut YamlEvent) -> bool {
    if !peek_token(parser) {
        return false;
    }

    let token = token(parser);
    let start_mark = token.start_mark;
    let mut end_mark = token.start_mark;

    let mut implicit = true;
    if token.typ == YamlTokenType::DocumentEnd {
        end_mark = token.end_mark;
        skip_token(parser);
        implicit = false;
    }

    parser.tag_directives.clear();

    parser.state = YamlParserState::DocumentStart;
    *event = YamlEvent {
        typ: YamlEventType::DocumentEnd,
        start_mark,
        end_mark,
        implicit,
        ..YamlEvent::default()
    };
    yaml_parser_set_event_comments(parser, event);
    if !event.head_comment.is_empty() && event.foot_comment.is_empty() {
        event.foot_comment = std::mem::take(&mut event.head_comment);
    }
    true
}

fn yaml_parser_set_event_comments(parser: &mut YamlParser<'_>, event: &mut YamlEvent) {
    event.head_comment = std::mem::take(&mut parser.head_comment);
    event.line_comment = std::mem::take(&mut parser.line_comment);
    event.foot_comment = std::mem::take(&mut parser.foot_comment);
    parser.tail_comment = Vec::new();
    parser.stem_comment = Vec::new();
}

// Parse the productions:
// block_node_or_indentless_sequence    ::=
//                          ALIAS
//                          *****
//                          | properties (block_content | indentless_block_sequence)?
//                            **********  *
//                          | block_content | indentless_block_sequence
//                            *
// block_node           ::= ALIAS
//                          *****
//                          | properties block_content?
//                            ********** *
//                          | block_content
//                            *
// flow_node            ::= ALIAS
//                          *****
//                          | properties flow_content?
//                            ********** *
//                          | flow_content
//                            *
// properties           ::= TAG ANCHOR? | ANCHOR TAG?
//                          *************************
// block_content        ::= block_collection | flow_collection | SCALAR
//                                                               ******
// flow_content         ::= flow_collection | SCALAR
//                                            ******
fn yaml_parser_parse_node(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    block: bool,
    indentless_sequence: bool,
) -> bool {
    if !peek_token(parser) {
        return false;
    }

    let token = token(parser);
    if token.typ == YamlTokenType::Alias {
        *event = YamlEvent {
            typ: YamlEventType::Alias,
            start_mark: token.start_mark,
            end_mark: token.end_mark,
            anchor: token.value.clone(),
            ..YamlEvent::default()
        };
        pop_state(parser);
        yaml_parser_set_event_comments(parser, event);
        skip_token(parser);
        return true;
    }

    let mut start_mark = token.start_mark;
    let mut end_mark = token.start_mark;

    let mut tag_token = false;
    let mut tag_handle = Vec::new();
    let mut tag_suffix = Vec::new();
    let mut anchor = Vec::new();
    let mut tag_mark = YamlMark::default();
    if token.typ == YamlTokenType::Anchor {
        anchor = token.value.clone();
        start_mark = token.start_mark;
        end_mark = token.end_mark;
        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
        let token = self::token(parser);
        if token.typ == YamlTokenType::Tag {
            tag_token = true;
            tag_handle = token.value.clone();
            tag_suffix = token.suffix.clone();
            tag_mark = token.start_mark;
            end_mark = token.end_mark;
            skip_token(parser);
            if !peek_token(parser) {
                return false;
            }
        }
    } else if token.typ == YamlTokenType::Tag {
        tag_token = true;
        tag_handle = token.value.clone();
        tag_suffix = token.suffix.clone();
        start_mark = token.start_mark;
        tag_mark = token.start_mark;
        end_mark = token.end_mark;
        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
        let token = self::token(parser);
        if token.typ == YamlTokenType::Anchor {
            anchor = token.value.clone();
            end_mark = token.end_mark;
            skip_token(parser);
            if !peek_token(parser) {
                return false;
            }
        }
    }

    let mut tag = Vec::new();
    if tag_token {
        if tag_handle.is_empty() {
            tag = std::mem::take(&mut tag_suffix);
        } else {
            for tag_directive in &parser.tag_directives {
                if tag_directive.handle == tag_handle {
                    tag = tag_directive.prefix.clone();
                    tag.extend_from_slice(&tag_suffix);
                    break;
                }
            }
            if tag.is_empty() {
                yaml_parser_set_parser_error_context(
                    parser,
                    "while parsing a node",
                    start_mark,
                    "found undefined tag handle",
                    tag_mark,
                );
                return false;
            }
        }
    }

    let implicit = tag.is_empty();
    let token = self::token(parser);
    if indentless_sequence && token.typ == YamlTokenType::BlockEntry {
        end_mark = token.end_mark;
        parser.state = YamlParserState::IndentlessSequenceEntry;
        *event = YamlEvent {
            typ: YamlEventType::SequenceStart,
            start_mark,
            end_mark,
            anchor,
            tag,
            implicit,
            style: YamlSequenceStyle::Block as YamlStyle,
            ..YamlEvent::default()
        };
        return true;
    }
    if token.typ == YamlTokenType::Scalar {
        let mut plain_implicit = false;
        let mut quoted_implicit = false;
        end_mark = token.end_mark;
        if (tag.is_empty() && token.style == YamlScalarStyle::Plain)
            || (tag.len() == 1 && tag[0] == b'!')
        {
            plain_implicit = true;
        } else if tag.is_empty() {
            quoted_implicit = true;
        }
        *event = YamlEvent {
            typ: YamlEventType::Scalar,
            start_mark,
            end_mark,
            anchor,
            tag,
            value: token.value.clone(),
            implicit: plain_implicit,
            quoted_implicit,
            style: token.style as YamlStyle,
            ..YamlEvent::default()
        };
        pop_state(parser);
        yaml_parser_set_event_comments(parser, event);
        skip_token(parser);
        return true;
    }
    if token.typ == YamlTokenType::FlowSequenceStart {
        // Some of the events below could be merged as they differ only on style.
        end_mark = token.end_mark;
        parser.state = YamlParserState::FlowSequenceFirstEntry;
        *event = YamlEvent {
            typ: YamlEventType::SequenceStart,
            start_mark,
            end_mark,
            anchor,
            tag,
            implicit,
            style: YamlSequenceStyle::Flow as YamlStyle,
            ..YamlEvent::default()
        };
        yaml_parser_set_event_comments(parser, event);
        return true;
    }
    if token.typ == YamlTokenType::FlowMappingStart {
        end_mark = token.end_mark;
        parser.state = YamlParserState::FlowMappingFirstKey;
        *event = YamlEvent {
            typ: YamlEventType::MappingStart,
            start_mark,
            end_mark,
            anchor,
            tag,
            implicit,
            style: YamlMappingStyle::Flow as YamlStyle,
            ..YamlEvent::default()
        };
        yaml_parser_set_event_comments(parser, event);
        return true;
    }
    if block && token.typ == YamlTokenType::BlockSequenceStart {
        end_mark = token.end_mark;
        parser.state = YamlParserState::BlockSequenceFirstEntry;
        *event = YamlEvent {
            typ: YamlEventType::SequenceStart,
            start_mark,
            end_mark,
            anchor,
            tag,
            implicit,
            style: YamlSequenceStyle::Block as YamlStyle,
            ..YamlEvent::default()
        };
        // The stem comment is only ever set non-empty.
        if !parser.stem_comment.is_empty() {
            event.head_comment = std::mem::take(&mut parser.stem_comment);
        }
        return true;
    }
    if block && token.typ == YamlTokenType::BlockMappingStart {
        end_mark = token.end_mark;
        parser.state = YamlParserState::BlockMappingFirstKey;
        *event = YamlEvent {
            typ: YamlEventType::MappingStart,
            start_mark,
            end_mark,
            anchor,
            tag,
            implicit,
            style: YamlMappingStyle::Block as YamlStyle,
            ..YamlEvent::default()
        };
        // The stem comment is only ever set non-empty.
        if !parser.stem_comment.is_empty() {
            event.head_comment = std::mem::take(&mut parser.stem_comment);
        }
        return true;
    }
    if !anchor.is_empty() || !tag.is_empty() {
        pop_state(parser);

        *event = YamlEvent {
            typ: YamlEventType::Scalar,
            start_mark,
            end_mark,
            anchor,
            tag,
            implicit,
            quoted_implicit: false,
            style: YamlScalarStyle::Plain as YamlStyle,
            ..YamlEvent::default()
        };
        return true;
    }

    let context = if block {
        "while parsing a block node"
    } else {
        "while parsing a flow node"
    };
    let problem_mark = token.start_mark;
    yaml_parser_set_parser_error_context(
        parser,
        context,
        start_mark,
        "did not find expected node content",
        problem_mark,
    );
    false
}

// Parse the productions:
// block_sequence ::= BLOCK-SEQUENCE-START (BLOCK-ENTRY block_node?)* BLOCK-END
//                    ********************  *********** *             *********
//
fn yaml_parser_parse_block_sequence_entry(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    first: bool,
) -> bool {
    if first {
        if !peek_token(parser) {
            return false;
        }
        let mark = token(parser).start_mark;
        parser.marks.push(mark);
        skip_token(parser);
    }

    if !peek_token(parser) {
        return false;
    }

    let token = token(parser);
    if token.typ == YamlTokenType::BlockEntry {
        let mark = token.end_mark;
        let prior_head_len = parser.head_comment.len();
        skip_token(parser);
        yaml_parser_split_stem_comment(parser, prior_head_len);
        if !peek_token(parser) {
            return false;
        }
        let typ = self::token(parser).typ;
        if typ != YamlTokenType::BlockEntry && typ != YamlTokenType::BlockEnd {
            parser.states.push(YamlParserState::BlockSequenceEntry);
            return yaml_parser_parse_node(parser, event, true, false);
        } else {
            parser.state = YamlParserState::BlockSequenceEntry;
            return yaml_parser_process_empty_scalar(event, mark);
        }
    }
    if token.typ == YamlTokenType::BlockEnd {
        *event = YamlEvent {
            typ: YamlEventType::SequenceEnd,
            start_mark: token.start_mark,
            end_mark: token.end_mark,
            ..YamlEvent::default()
        };
        pop_state(parser);
        pop_mark(parser);

        skip_token(parser);
        return true;
    }

    let problem_mark = token.start_mark;
    let context_mark = pop_mark(parser);
    yaml_parser_set_parser_error_context(
        parser,
        "while parsing a block collection",
        context_mark,
        "did not find expected '-' indicator",
        problem_mark,
    )
}

// Parse the productions:
// indentless_sequence  ::= (BLOCK-ENTRY block_node?)+
//                           *********** *
fn yaml_parser_parse_indentless_sequence_entry(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
) -> bool {
    if !peek_token(parser) {
        return false;
    }

    let token = token(parser);
    if token.typ == YamlTokenType::BlockEntry {
        let mark = token.end_mark;
        let prior_head_len = parser.head_comment.len();
        skip_token(parser);
        yaml_parser_split_stem_comment(parser, prior_head_len);
        if !peek_token(parser) {
            return false;
        }
        let typ = self::token(parser).typ;
        if typ != YamlTokenType::BlockEntry
            && typ != YamlTokenType::Key
            && typ != YamlTokenType::Value
            && typ != YamlTokenType::BlockEnd
        {
            parser.states.push(YamlParserState::IndentlessSequenceEntry);
            return yaml_parser_parse_node(parser, event, true, false);
        }
        parser.state = YamlParserState::IndentlessSequenceEntry;
        return yaml_parser_process_empty_scalar(event, mark);
    }
    *event = YamlEvent {
        typ: YamlEventType::SequenceEnd,
        start_mark: token.start_mark,
        end_mark: token.start_mark, // (the start mark, as in libyaml)
        ..YamlEvent::default()
    };
    pop_state(parser);
    true
}

/// Split stem comment from head comment.
///
/// When a sequence or map is found under a sequence entry, the former head comment is assigned
/// to the underlying sequence or map as a whole, not the individual sequence or map entry as
/// would be expected otherwise. To handle this case the previous head comment is moved aside as
/// the stem comment.
fn yaml_parser_split_stem_comment(parser: &mut YamlParser<'_>, stem_len: usize) {
    if stem_len == 0 {
        return;
    }

    if !peek_token(parser) {
        return;
    }
    let typ = token(parser).typ;
    if typ != YamlTokenType::BlockSequenceStart && typ != YamlTokenType::BlockMappingStart {
        return;
    }

    parser.stem_comment = parser.head_comment[..stem_len].to_vec();
    if parser.head_comment.len() == stem_len {
        parser.head_comment = Vec::new();
    } else {
        // Copy suffix to prevent very strange bugs if someone ever appends
        // further bytes to the prefix in the stem_comment slice above.
        parser.head_comment = parser.head_comment[stem_len + 1..].to_vec();
    }
}

// Parse the productions:
// block_mapping        ::= BLOCK-MAPPING_START
//                          *******************
//                          ((KEY block_node_or_indentless_sequence?)?
//                            *** *
//                          (VALUE block_node_or_indentless_sequence?)?)*
//
//                          BLOCK-END
//                          *********
//
fn yaml_parser_parse_block_mapping_key(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    first: bool,
) -> bool {
    if first {
        if !peek_token(parser) {
            return false;
        }
        let mark = token(parser).start_mark;
        parser.marks.push(mark);
        skip_token(parser);
    }

    if !peek_token(parser) {
        return false;
    }

    let token = token(parser);
    // A tail comment was left from the prior mapping value processed. Emit an event as it
    // needs to be processed with that value and not the following key.
    if !parser.tail_comment.is_empty() {
        *event = YamlEvent {
            typ: YamlEventType::TailComment,
            start_mark: token.start_mark,
            end_mark: token.end_mark,
            foot_comment: parser.tail_comment.clone(),
            ..YamlEvent::default()
        };
        parser.tail_comment = Vec::new();
        return true;
    }

    if token.typ == YamlTokenType::Key {
        let mark = token.end_mark;
        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
        let typ = self::token(parser).typ;
        if typ != YamlTokenType::Key
            && typ != YamlTokenType::Value
            && typ != YamlTokenType::BlockEnd
        {
            parser.states.push(YamlParserState::BlockMappingValue);
            return yaml_parser_parse_node(parser, event, true, true);
        } else {
            parser.state = YamlParserState::BlockMappingValue;
            return yaml_parser_process_empty_scalar(event, mark);
        }
    } else if token.typ == YamlTokenType::BlockEnd {
        *event = YamlEvent {
            typ: YamlEventType::MappingEnd,
            start_mark: token.start_mark,
            end_mark: token.end_mark,
            ..YamlEvent::default()
        };
        pop_state(parser);
        pop_mark(parser);
        yaml_parser_set_event_comments(parser, event);
        skip_token(parser);
        return true;
    }

    let problem_mark = token.start_mark;
    let context_mark = pop_mark(parser);
    yaml_parser_set_parser_error_context(
        parser,
        "while parsing a block mapping",
        context_mark,
        "did not find expected key",
        problem_mark,
    )
}

// Parse the productions:
// block_mapping        ::= BLOCK-MAPPING_START
//
//                          ((KEY block_node_or_indentless_sequence?)?
//
//                          (VALUE block_node_or_indentless_sequence?)?)*
//                           ***** *
//                          BLOCK-END
//
//
fn yaml_parser_parse_block_mapping_value(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
) -> bool {
    if !peek_token(parser) {
        return false;
    }
    let token = token(parser);
    if token.typ == YamlTokenType::Value {
        let mark = token.end_mark;
        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
        let typ = self::token(parser).typ;
        if typ != YamlTokenType::Key
            && typ != YamlTokenType::Value
            && typ != YamlTokenType::BlockEnd
        {
            parser.states.push(YamlParserState::BlockMappingKey);
            return yaml_parser_parse_node(parser, event, true, true);
        }
        parser.state = YamlParserState::BlockMappingKey;
        return yaml_parser_process_empty_scalar(event, mark);
    }
    let mark = token.start_mark;
    parser.state = YamlParserState::BlockMappingKey;
    yaml_parser_process_empty_scalar(event, mark)
}

// Parse the productions:
// flow_sequence        ::= FLOW-SEQUENCE-START
//                          *******************
//                          (flow_sequence_entry FLOW-ENTRY)*
//                           *                   **********
//                          flow_sequence_entry?
//                          *
//                          FLOW-SEQUENCE-END
//                          *****************
// flow_sequence_entry  ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//                          *
//
fn yaml_parser_parse_flow_sequence_entry(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    first: bool,
) -> bool {
    if first {
        if !peek_token(parser) {
            return false;
        }
        let mark = token(parser).start_mark;
        parser.marks.push(mark);
        skip_token(parser);
    }
    if !peek_token(parser) {
        return false;
    }
    if token(parser).typ != YamlTokenType::FlowSequenceEnd {
        if !first {
            let token = token(parser);
            if token.typ == YamlTokenType::FlowEntry {
                skip_token(parser);
                if !peek_token(parser) {
                    return false;
                }
            } else {
                let problem_mark = token.start_mark;
                let context_mark = pop_mark(parser);
                return yaml_parser_set_parser_error_context(
                    parser,
                    "while parsing a flow sequence",
                    context_mark,
                    "did not find expected ',' or ']'",
                    problem_mark,
                );
            }
        }

        let token = token(parser);
        if token.typ == YamlTokenType::Key {
            *event = YamlEvent {
                typ: YamlEventType::MappingStart,
                start_mark: token.start_mark,
                end_mark: token.end_mark,
                implicit: true,
                style: YamlMappingStyle::Flow as YamlStyle,
                ..YamlEvent::default()
            };
            parser.state = YamlParserState::FlowSequenceEntryMappingKey;
            skip_token(parser);
            return true;
        } else if token.typ != YamlTokenType::FlowSequenceEnd {
            parser.states.push(YamlParserState::FlowSequenceEntry);
            return yaml_parser_parse_node(parser, event, false, false);
        }
    }

    let token = token(parser);
    *event = YamlEvent {
        typ: YamlEventType::SequenceEnd,
        start_mark: token.start_mark,
        end_mark: token.end_mark,
        ..YamlEvent::default()
    };
    pop_state(parser);
    pop_mark(parser);
    yaml_parser_set_event_comments(parser, event);

    skip_token(parser);
    true
}

//
// Parse the productions:
// flow_sequence_entry  ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//                                      *** *
//
fn yaml_parser_parse_flow_sequence_entry_mapping_key(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
) -> bool {
    if !peek_token(parser) {
        return false;
    }
    let token = token(parser);
    if token.typ != YamlTokenType::Value
        && token.typ != YamlTokenType::FlowEntry
        && token.typ != YamlTokenType::FlowSequenceEnd
    {
        parser
            .states
            .push(YamlParserState::FlowSequenceEntryMappingValue);
        return yaml_parser_parse_node(parser, event, false, false);
    }
    let mark = token.end_mark;
    skip_token(parser);
    parser.state = YamlParserState::FlowSequenceEntryMappingValue;
    yaml_parser_process_empty_scalar(event, mark)
}

// Parse the productions:
// flow_sequence_entry  ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//                                                      ***** *
//
fn yaml_parser_parse_flow_sequence_entry_mapping_value(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
) -> bool {
    if !peek_token(parser) {
        return false;
    }
    let token = token(parser);
    // The empty scalar after the `if` below is marked at this (VALUE or other) token, not at
    // the token peeked inside it.
    let mark = token.start_mark;
    if token.typ == YamlTokenType::Value {
        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
        let typ = self::token(parser).typ;
        if typ != YamlTokenType::FlowEntry && typ != YamlTokenType::FlowSequenceEnd {
            parser
                .states
                .push(YamlParserState::FlowSequenceEntryMappingEnd);
            return yaml_parser_parse_node(parser, event, false, false);
        }
    }
    parser.state = YamlParserState::FlowSequenceEntryMappingEnd;
    yaml_parser_process_empty_scalar(event, mark)
}

// Parse the productions:
// flow_sequence_entry  ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//                                                                      *
//
fn yaml_parser_parse_flow_sequence_entry_mapping_end(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
) -> bool {
    if !peek_token(parser) {
        return false;
    }
    let token = token(parser);
    *event = YamlEvent {
        typ: YamlEventType::MappingEnd,
        start_mark: token.start_mark,
        end_mark: token.start_mark, // (the start mark, as in libyaml)
        ..YamlEvent::default()
    };
    parser.state = YamlParserState::FlowSequenceEntry;
    true
}

// Parse the productions:
// flow_mapping         ::= FLOW-MAPPING-START
//                          ******************
//                          (flow_mapping_entry FLOW-ENTRY)*
//                           *                  **********
//                          flow_mapping_entry?
//                          ******************
//                          FLOW-MAPPING-END
//                          ****************
// flow_mapping_entry   ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//                          *           *** *
//
fn yaml_parser_parse_flow_mapping_key(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    first: bool,
) -> bool {
    if first {
        // The result is not checked here: the FLOW-MAPPING-START token is still the
        // available queue head (yaml_parser_parse_node peeked it without skipping).
        peek_token(parser);
        let mark = token(parser).start_mark;
        parser.marks.push(mark);
        skip_token(parser);
    }

    if !peek_token(parser) {
        return false;
    }

    if token(parser).typ != YamlTokenType::FlowMappingEnd {
        if !first {
            let token = token(parser);
            if token.typ == YamlTokenType::FlowEntry {
                skip_token(parser);
                if !peek_token(parser) {
                    return false;
                }
            } else {
                let problem_mark = token.start_mark;
                let context_mark = pop_mark(parser);
                return yaml_parser_set_parser_error_context(
                    parser,
                    "while parsing a flow mapping",
                    context_mark,
                    "did not find expected ',' or '}'",
                    problem_mark,
                );
            }
        }

        let typ = token(parser).typ;
        if typ == YamlTokenType::Key {
            skip_token(parser);
            if !peek_token(parser) {
                return false;
            }
            let token = token(parser);
            if token.typ != YamlTokenType::Value
                && token.typ != YamlTokenType::FlowEntry
                && token.typ != YamlTokenType::FlowMappingEnd
            {
                parser.states.push(YamlParserState::FlowMappingValue);
                return yaml_parser_parse_node(parser, event, false, false);
            } else {
                let mark = token.start_mark;
                parser.state = YamlParserState::FlowMappingValue;
                return yaml_parser_process_empty_scalar(event, mark);
            }
        } else if typ != YamlTokenType::FlowMappingEnd {
            parser.states.push(YamlParserState::FlowMappingEmptyValue);
            return yaml_parser_parse_node(parser, event, false, false);
        }
    }

    let token = token(parser);
    *event = YamlEvent {
        typ: YamlEventType::MappingEnd,
        start_mark: token.start_mark,
        end_mark: token.end_mark,
        ..YamlEvent::default()
    };
    pop_state(parser);
    pop_mark(parser);
    yaml_parser_set_event_comments(parser, event);
    skip_token(parser);
    true
}

// Parse the productions:
// flow_mapping_entry   ::= flow_node | KEY flow_node? (VALUE flow_node?)?
//                                   *                  ***** *
//
fn yaml_parser_parse_flow_mapping_value(
    parser: &mut YamlParser<'_>,
    event: &mut YamlEvent,
    empty: bool,
) -> bool {
    if !peek_token(parser) {
        return false;
    }
    if empty {
        let mark = token(parser).start_mark;
        parser.state = YamlParserState::FlowMappingKey;
        return yaml_parser_process_empty_scalar(event, mark);
    }
    if token(parser).typ == YamlTokenType::Value {
        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
        let typ = token(parser).typ;
        if typ != YamlTokenType::FlowEntry && typ != YamlTokenType::FlowMappingEnd {
            parser.states.push(YamlParserState::FlowMappingKey);
            return yaml_parser_parse_node(parser, event, false, false);
        }
    }
    let mark = token(parser).start_mark;
    parser.state = YamlParserState::FlowMappingKey;
    yaml_parser_process_empty_scalar(event, mark)
}

/// Generate an empty scalar event.
fn yaml_parser_process_empty_scalar(event: &mut YamlEvent, mark: YamlMark) -> bool {
    *event = YamlEvent {
        typ: YamlEventType::Scalar,
        start_mark: mark,
        end_mark: mark,
        value: Vec::new(), // Empty
        implicit: true,
        style: YamlScalarStyle::Plain as YamlStyle,
        ..YamlEvent::default()
    };
    true
}

/// Parse directives.
fn yaml_parser_process_directives(
    parser: &mut YamlParser<'_>,
    version_directive_ref: Option<&mut Option<YamlVersionDirective>>,
    tag_directives_ref: Option<&mut Vec<YamlTagDirective>>,
) -> bool {
    let mut version_directive: Option<YamlVersionDirective> = None;
    let mut tag_directives: Vec<YamlTagDirective> = Vec::new();

    if !peek_token(parser) {
        return false;
    }

    loop {
        let token = token(parser);
        if token.typ != YamlTokenType::VersionDirective && token.typ != YamlTokenType::TagDirective
        {
            break;
        }
        if token.typ == YamlTokenType::VersionDirective {
            if version_directive.is_some() {
                let mark = token.start_mark;
                yaml_parser_set_parser_error(parser, "found duplicate %YAML directive", mark);
                return false;
            }
            if token.major != 1 || token.minor != 1 {
                let mark = token.start_mark;
                yaml_parser_set_parser_error(parser, "found incompatible YAML document", mark);
                return false;
            }
            version_directive = Some(YamlVersionDirective {
                major: token.major,
                minor: token.minor,
            });
        } else if token.typ == YamlTokenType::TagDirective {
            let value = YamlTagDirective {
                handle: token.value.clone(),
                prefix: token.prefix.clone(),
            };
            let mark = token.start_mark;
            if !yaml_parser_append_tag_directive(parser, value.clone(), false, mark) {
                return false;
            }
            tag_directives.push(value);
        }

        skip_token(parser);
        if !peek_token(parser) {
            return false;
        }
    }

    let mark = token(parser).start_mark;
    for tag_directive in default_tag_directives() {
        if !yaml_parser_append_tag_directive(parser, tag_directive, true, mark) {
            return false;
        }
    }

    if let Some(version_directive_ref) = version_directive_ref {
        *version_directive_ref = version_directive;
    }
    if let Some(tag_directives_ref) = tag_directives_ref {
        *tag_directives_ref = tag_directives;
    }
    true
}

/// Append a tag directive to the directives stack.
fn yaml_parser_append_tag_directive(
    parser: &mut YamlParser<'_>,
    value: YamlTagDirective,
    allow_duplicates: bool,
    mark: YamlMark,
) -> bool {
    if parser
        .tag_directives
        .iter()
        .any(|tag_directive| value.handle == tag_directive.handle)
    {
        if allow_duplicates {
            return true;
        }
        return yaml_parser_set_parser_error(parser, "found duplicate %TAG directive", mark);
    }

    // (`value` is already an owned copy here, so no further copy is needed.)
    parser.tag_directives.push(value);
    true
}

#[cfg(test)]
mod tests {
    use super::yaml_parser_parse;
    use crate::reader::tests::{reader_parser, string_parser};
    use crate::yamlh::{YamlErrorType, YamlEvent, YamlEventType, YamlParser};

    /// One event as compact text: type, then the fields that are set
    /// (`a=` anchor, `t=` tag, `v=` value, `s=` raw style, `i`/`q` implicit/quoted_implicit,
    /// `h=`/`l=`/`f=` comments, `@line:col` start mark).
    fn show(e: &YamlEvent) -> String {
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let mut s = format!("{}@{}:{}", e.typ, e.start_mark.line, e.start_mark.column);
        if !e.anchor.is_empty() {
            s += &format!(" a={}", text(&e.anchor));
        }
        if !e.tag.is_empty() {
            s += &format!(" t={}", text(&e.tag));
        }
        if e.typ == YamlEventType::Scalar {
            s += &format!(" v={:?}", text(&e.value));
        }
        if e.style != 0 {
            s += &format!(" s={}", e.style);
        }
        if e.implicit {
            s += " i";
        }
        if e.quoted_implicit {
            s += " q";
        }
        if let Some(v) = e.version_directive {
            s += &format!(" %YAML {}.{}", v.major, v.minor);
        }
        for d in &e.tag_directives {
            s += &format!(" %TAG {} {}", text(&d.handle), text(&d.prefix));
        }
        for (k, c) in [
            ("h", &e.head_comment),
            ("l", &e.line_comment),
            ("f", &e.foot_comment),
        ] {
            if !c.is_empty() {
                s += &format!(" {k}={:?}", text(c));
            }
        }
        s
    }

    /// Parse every event up to STREAM-END; `Err` holds the parser's error fields.
    fn parse_all(parser: &mut YamlParser<'_>) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        loop {
            let mut event = YamlEvent::default();
            if !yaml_parser_parse(parser, &mut event) {
                return Err(format!(
                    "{:?} {:?}@{}:{} {:?}@{}:{} after {out:?}",
                    parser.error,
                    parser.context,
                    parser.context_mark.line,
                    parser.context_mark.column,
                    parser.problem,
                    parser.problem_mark.line,
                    parser.problem_mark.column,
                ));
            }
            let end = event.typ == YamlEventType::StreamEnd;
            out.push(show(&event));
            if end {
                return Ok(out);
            }
        }
    }

    fn events(input: &str) -> Result<Vec<String>, String> {
        parse_all(&mut string_parser(input.as_bytes()))
    }

    #[test]
    fn block_mapping() {
        assert_eq!(
            events("a: 1\n").unwrap(),
            [
                "stream start@0:0",
                "document start@0:0",
                "mapping start@0:0 s=1 i",
                "scalar@0:0 v=\"a\" s=2 i",
                "scalar@0:3 v=\"1\" s=2 i",
                "mapping end@1:0",
                "document end@1:0 i",
                "stream end@1:0",
            ]
        );
    }

    #[test]
    fn explicit_document_directives_properties_and_flow() {
        assert_eq!(
            events(
                "%YAML 1.1\n%TAG !e! tag:example.com,2000:\n--- !e!x &a [b, 'c', {d: }, e: f]\n...\n"
            )
            .unwrap(),
            [
                "stream start@0:0",
                "document start@0:0 %YAML 1.1 %TAG !e! tag:example.com,2000:",
                "sequence start@2:4 a=a t=tag:example.com,2000:x s=2",
                "scalar@2:13 v=\"b\" s=2 i",
                "scalar@2:16 v=\"c\" s=4 q",
                "mapping start@2:21 s=2 i",
                "scalar@2:22 v=\"d\" s=2 i",
                "scalar@2:25 v=\"\" s=2 i",
                "mapping end@2:25",
                // A single-pair mapping inside a flow sequence; its end is marked at the
                // start of the next token (Go: "Shouldn't this be end_mark?").
                "mapping start@2:28 s=2 i",
                "scalar@2:28 v=\"e\" s=2 i",
                "scalar@2:31 v=\"f\" s=2 i",
                "mapping end@2:32",
                "sequence end@2:32",
                "document end@3:0",
                "stream end@4:0",
            ]
        );
    }

    #[test]
    fn flow_mapping_entries_without_values() {
        assert_eq!(
            events("{a, ? b, c: }\n").unwrap(),
            [
                "stream start@0:0",
                "document start@0:0",
                "mapping start@0:0 s=2 i",
                "scalar@0:1 v=\"a\" s=2 i",
                "scalar@0:2 v=\"\" s=2 i",
                "scalar@0:6 v=\"b\" s=2 i",
                "scalar@0:7 v=\"\" s=2 i",
                "scalar@0:9 v=\"c\" s=2 i",
                "scalar@0:12 v=\"\" s=2 i",
                "mapping end@0:12",
                "document end@1:0 i",
                "stream end@1:0",
            ]
        );
    }

    #[test]
    fn indentless_sequence_and_empty_properties_node() {
        assert_eq!(
            events("a:\n- b\n- c\n").unwrap(),
            [
                "stream start@0:0",
                "document start@0:0",
                "mapping start@0:0 s=1 i",
                "scalar@0:0 v=\"a\" s=2 i",
                "sequence start@1:0 s=1 i",
                "scalar@1:2 v=\"b\" s=2 i",
                "scalar@2:2 v=\"c\" s=2 i",
                "sequence end@3:0",
                "mapping end@3:0",
                "document end@3:0 i",
                "stream end@3:0",
            ]
        );
        // Properties without content: an empty plain scalar, neither implicit flag set
        // (a tag is present), no comments attached.
        assert_eq!(
            events("&x !!str\n").unwrap(),
            [
                "stream start@0:0",
                "document start@0:0",
                "scalar@0:0 a=x t=tag:yaml.org,2002:str v=\"\" s=2",
                "document end@1:0 i",
                "stream end@1:0",
            ]
        );
    }

    #[test]
    fn comments_document_head_split_and_stem() {
        // The head comment block before the first node is split at its last empty line: the
        // part above goes to the document, the rest to the first node.
        assert_eq!(
            events("# head\n\n# doc\nk: v # line\n# foot\n").unwrap(),
            [
                "stream start@0:0",
                "document start@3:0 h=\"# head\"",
                "mapping start@3:0 s=1 i",
                "scalar@3:0 v=\"k\" s=2 i h=\"# doc\"",
                "scalar@3:3 v=\"v\" s=2 i l=\"# line\" f=\"# foot\"",
                "mapping end@4:1",
                "document end@5:0 i",
                "stream end@5:0",
            ]
        );
        // A head comment above a sequence entry holding a block collection becomes the stem
        // comment, i.e. the collection's head.
        for (input, start) in [
            (
                "- a\n# stem\n- - b\n",
                "sequence start@2:2 s=1 i h=\"# stem\"",
            ),
            (
                "- a\n# stem\n- k: v\n",
                "mapping start@2:2 s=1 i h=\"# stem\"",
            ),
        ] {
            let got = events(input).unwrap();
            assert_eq!(got[4], start, "{input:?}");
            assert!(
                got.iter().filter(|e| e.contains("# stem")).count() == 1,
                "{got:?}"
            );
        }
    }

    #[test]
    fn parser_errors_and_marks() {
        let cases: [(&[u8], &str); 5] = [
            (
                b"%YAML 1.2\n--- a\n",
                "Parser \"\"@0:0 \"found incompatible YAML document\"@0:0",
            ),
            (
                b"%YAML 1.1\n%YAML 1.1\n--- a\n",
                "Parser \"\"@0:0 \"found duplicate %YAML directive\"@1:0",
            ),
            (
                b"%TAG !a! x\n%TAG !a! y\n--- a\n",
                "Parser \"\"@0:0 \"found duplicate %TAG directive\"@1:0",
            ),
            (
                b"!b!x a\n",
                "Parser \"while parsing a node\"@0:0 \"found undefined tag handle\"@0:0",
            ),
            (
                b"[a: , : b]\n",
                "Parser \"while parsing a flow node\"@0:6 \"did not find expected node content\"@0:6",
            ),
        ];
        for (input, want) in cases {
            let err = parse_all(&mut string_parser(input)).unwrap_err();
            assert!(
                err.starts_with(want),
                "{:?}: {err}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn no_events_after_stream_end_or_error() {
        let mut parser = string_parser(b"a\n");
        parse_all(&mut parser).unwrap();
        for _ in 0..2 {
            let mut event = YamlEvent {
                typ: YamlEventType::Scalar,
                ..YamlEvent::default()
            };
            assert!(yaml_parser_parse(&mut parser, &mut event));
            assert_eq!(event, YamlEvent::default());
        }

        let mut parser = string_parser(b"[a\n");
        assert!(parse_all(&mut parser).is_err());
        assert_ne!(parser.error, YamlErrorType::No);
        let mut event = YamlEvent::default();
        assert!(yaml_parser_parse(&mut parser, &mut event));
        assert_eq!(event.typ, YamlEventType::No);
    }

    #[test]
    fn reader_input_gives_the_same_events() {
        for input in [
            "a: 1\n",
            "# head\n\n# doc\nk: v # line\n# foot\n",
            "- a\n# stem\n- k: [x, {y: z}]\n...\n--- b\n",
        ] {
            let from_reader = parse_all(&mut reader_parser(input.as_bytes(), false));
            assert_eq!(from_reader, events(input), "{input:?}");
        }
    }
}
