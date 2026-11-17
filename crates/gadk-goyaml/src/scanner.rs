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

//! The tokenizer (simple keys, indentation, flow levels, every scalar style, directives, tags,
//! anchors, comment capture).
//!
//! libyaml's scanner opens with a long introduction to the token stream (block collection
//! starts, simple keys, examples); it is not repeated here. There is no `cache` helper (every
//! call site inlines `parser.unread < n && !yaml_parser_update_buffer(...)`) nor a debug
//! `trace` printer. Functions that append to a byte buffer (`read`, `read_line`) take
//! `&mut Vec<u8>`, and out-parameters are `&mut`. Values that can be negative (indentation
//! columns, the token queue position) are `isize`; the comparisons that mix them with `usize`
//! marks cast the mark to `isize` and never subtract below zero in `usize`.

use crate::api::yaml_insert_token;
use crate::reader::yaml_parser_update_buffer;
use crate::yamlh::{
    YamlComment, YamlErrorType, YamlMark, YamlParser, YamlScalarStyle, YamlSimpleKey, YamlToken,
    YamlTokenType, as_digit, as_hex, is_alpha, is_blank, is_blankz, is_bom, is_break, is_breakz,
    is_crlf, is_digit, is_hex, is_space, is_tab, is_z, width,
};

/// Advance the buffer pointer.
fn skip(parser: &mut YamlParser<'_>) {
    if !is_blank(&parser.buffer, parser.buffer_pos) {
        parser.newlines = 0;
    }
    parser.mark.index += 1;
    parser.mark.column += 1;
    parser.unread -= 1;
    parser.buffer_pos += width(parser.buffer[parser.buffer_pos]);
}

fn skip_line(parser: &mut YamlParser<'_>) {
    if is_crlf(&parser.buffer, parser.buffer_pos) {
        parser.mark.index += 2;
        parser.mark.column = 0;
        parser.mark.line += 1;
        parser.unread -= 2;
        parser.buffer_pos += 2;
        parser.newlines += 1;
    } else if is_break(&parser.buffer, parser.buffer_pos) {
        parser.mark.index += 1;
        parser.mark.column = 0;
        parser.mark.line += 1;
        parser.unread -= 1;
        parser.buffer_pos += width(parser.buffer[parser.buffer_pos]);
        parser.newlines += 1;
    }
}

/// Copy a character to a string buffer and advance pointers.
fn read(parser: &mut YamlParser<'_>, s: &mut Vec<u8>) {
    if !is_blank(&parser.buffer, parser.buffer_pos) {
        parser.newlines = 0;
    }
    let w = width(parser.buffer[parser.buffer_pos]);
    if w == 0 {
        panic!("invalid character sequence");
    }
    s.extend_from_slice(&parser.buffer[parser.buffer_pos..parser.buffer_pos + w]);
    parser.buffer_pos += w;
    parser.mark.index += 1;
    parser.mark.column += 1;
    parser.unread -= 1;
}

/// Copy a line break character to a string buffer and advance pointers.
fn read_line(parser: &mut YamlParser<'_>, s: &mut Vec<u8>) {
    let buf = &parser.buffer;
    let pos = parser.buffer_pos;
    if buf[pos] == b'\r' && buf[pos + 1] == b'\n' {
        // CR LF . LF
        s.push(b'\n');
        parser.buffer_pos += 2;
        parser.mark.index += 1;
        parser.unread -= 1;
    } else if buf[pos] == b'\r' || buf[pos] == b'\n' {
        // CR|LF . LF
        s.push(b'\n');
        parser.buffer_pos += 1;
    } else if buf[pos] == b'\xC2' && buf[pos + 1] == b'\x85' {
        // NEL . LF
        s.push(b'\n');
        parser.buffer_pos += 2;
    } else if buf[pos] == b'\xE2'
        && buf[pos + 1] == b'\x80'
        && (buf[pos + 2] == b'\xA8' || buf[pos + 2] == b'\xA9')
    {
        // LS|PS . LS|PS
        s.extend_from_slice(&buf[pos..pos + 3]);
        parser.buffer_pos += 3;
    } else {
        return;
    }
    parser.mark.index += 1;
    parser.mark.column = 0;
    parser.mark.line += 1;
    parser.unread -= 1;
    parser.newlines += 1;
}

/// Get the next token (libyaml's public scanner entry point; yaml.v3's parser pulls tokens
/// through `peek_token`/`skip_token` instead, so only tests call it).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn yaml_parser_scan(parser: &mut YamlParser<'_>, token: &mut YamlToken) -> bool {
    // Erase the token object.
    *token = YamlToken::default(); // erases the token

    // No tokens after STREAM-END or error.
    if parser.stream_end_produced || parser.error != YamlErrorType::No {
        return true;
    }

    // Ensure that the tokens queue contains enough tokens.
    if !parser.token_available && !yaml_parser_fetch_more_tokens(parser) {
        return false;
    }

    // Fetch the next token from the queue.
    *token = parser.tokens[parser.tokens_head].clone();
    parser.tokens_head += 1;
    parser.tokens_parsed += 1;
    parser.token_available = false;

    if token.typ == YamlTokenType::StreamEnd {
        parser.stream_end_produced = true;
    }
    true
}

/// Set the scanner error and return false.
fn yaml_parser_set_scanner_error(
    parser: &mut YamlParser<'_>,
    context: &str,
    context_mark: YamlMark,
    problem: &str,
) -> bool {
    parser.error = YamlErrorType::Scanner;
    parser.context = context.to_string();
    parser.context_mark = context_mark;
    parser.problem = problem.to_string();
    parser.problem_mark = parser.mark;
    false
}

fn yaml_parser_set_scanner_tag_error(
    parser: &mut YamlParser<'_>,
    directive: bool,
    context_mark: YamlMark,
    problem: &str,
) -> bool {
    let context = if directive {
        "while parsing a %TAG directive"
    } else {
        "while parsing a tag"
    };
    yaml_parser_set_scanner_error(parser, context, context_mark, problem)
}

/// Ensure that the tokens queue contains at least one token which can be returned to the
/// parser.
pub(crate) fn yaml_parser_fetch_more_tokens(parser: &mut YamlParser<'_>) -> bool {
    // While we need more tokens to fetch, do it.
    loop {
        // The comment parsing logic requires a lookahead of two tokens so that foot comments
        // may be parsed in time of associating them with the tokens that are parsed before
        // them, and also for line comments to be transformed into head comments in some edge
        // cases.
        if parser.tokens_head + 2 < parser.tokens.len() {
            // If a potential simple key is at the head position, we need to fetch
            // the next token to disambiguate it.
            let Some(&head_tok_idx) = parser.simple_keys_by_tok.get(&parser.tokens_parsed) else {
                break;
            };
            let (valid, ok) = yaml_simple_key_is_valid(parser, head_tok_idx);
            if !ok {
                return false;
            } else if !valid {
                break;
            }
        }
        // Fetch the next token.
        if !yaml_parser_fetch_next_token(parser) {
            return false;
        }
    }

    parser.token_available = true;
    true
}

/// The dispatcher for token fetchers.
fn yaml_parser_fetch_next_token(parser: &mut YamlParser<'_>) -> bool {
    // Ensure that the buffer is initialized.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    // Check if we just started scanning.  Fetch STREAM-START then.
    if !parser.stream_start_produced {
        return yaml_parser_fetch_stream_start(parser);
    }

    let scan_mark = parser.mark;

    // Eat whitespaces and comments until we reach the next token.
    if !yaml_parser_scan_to_next_token(parser) {
        return false;
    }

    // While unrolling indents, transform the head comments of prior indentation levels
    // observed after scan_start into foot comments at the respective indexes.

    // Check the indentation level against the current column.
    let column = parser.mark.column as isize;
    if !yaml_parser_unroll_indent(parser, column, scan_mark) {
        return false;
    }

    // Ensure that the buffer contains at least 4 characters.  4 is the length
    // of the longest indicators ('--- ' and '... ').
    if parser.unread < 4 && !yaml_parser_update_buffer(parser, 4) {
        return false;
    }

    // Is it the end of the stream?
    if is_z(&parser.buffer, parser.buffer_pos) {
        return yaml_parser_fetch_stream_end(parser);
    }

    // Is it a directive?
    if parser.mark.column == 0 && parser.buffer[parser.buffer_pos] == b'%' {
        return yaml_parser_fetch_directive(parser);
    }

    let buf = &parser.buffer;
    let pos = parser.buffer_pos;

    // Is it the document start indicator?
    if parser.mark.column == 0
        && buf[pos] == b'-'
        && buf[pos + 1] == b'-'
        && buf[pos + 2] == b'-'
        && is_blankz(buf, pos + 3)
    {
        return yaml_parser_fetch_document_indicator(parser, YamlTokenType::DocumentStart);
    }

    // Is it the document end indicator?
    if parser.mark.column == 0
        && buf[pos] == b'.'
        && buf[pos + 1] == b'.'
        && buf[pos + 2] == b'.'
        && is_blankz(buf, pos + 3)
    {
        return yaml_parser_fetch_document_indicator(parser, YamlTokenType::DocumentEnd);
    }

    let mut comment_mark = parser.mark;
    if !parser.tokens.is_empty()
        && ((parser.flow_level == 0 && buf[pos] == b':')
            || (parser.flow_level > 0 && buf[pos] == b','))
    {
        // Associate any following comments with the prior token.
        comment_mark = parser.tokens[parser.tokens.len() - 1].start_mark;
    }

    // Each branch below leaves through the code after the block (the common epilogue).
    let ok = 'fetch: {
        // Is it the flow sequence start indicator?
        if parser.buffer[parser.buffer_pos] == b'[' {
            break 'fetch yaml_parser_fetch_flow_collection_start(
                parser,
                YamlTokenType::FlowSequenceStart,
            );
        }

        // Is it the flow mapping start indicator?
        if parser.buffer[parser.buffer_pos] == b'{' {
            break 'fetch yaml_parser_fetch_flow_collection_start(
                parser,
                YamlTokenType::FlowMappingStart,
            );
        }

        // Is it the flow sequence end indicator?
        if parser.buffer[parser.buffer_pos] == b']' {
            break 'fetch yaml_parser_fetch_flow_collection_end(
                parser,
                YamlTokenType::FlowSequenceEnd,
            );
        }

        // Is it the flow mapping end indicator?
        if parser.buffer[parser.buffer_pos] == b'}' {
            break 'fetch yaml_parser_fetch_flow_collection_end(
                parser,
                YamlTokenType::FlowMappingEnd,
            );
        }

        // Is it the flow entry indicator?
        if parser.buffer[parser.buffer_pos] == b',' {
            break 'fetch yaml_parser_fetch_flow_entry(parser);
        }

        // Is it the block entry indicator?
        if parser.buffer[parser.buffer_pos] == b'-'
            && is_blankz(&parser.buffer, parser.buffer_pos + 1)
        {
            break 'fetch yaml_parser_fetch_block_entry(parser);
        }

        // Is it the key indicator?
        if parser.buffer[parser.buffer_pos] == b'?'
            && (parser.flow_level > 0 || is_blankz(&parser.buffer, parser.buffer_pos + 1))
        {
            break 'fetch yaml_parser_fetch_key(parser);
        }

        // Is it the value indicator?
        if parser.buffer[parser.buffer_pos] == b':'
            && (parser.flow_level > 0 || is_blankz(&parser.buffer, parser.buffer_pos + 1))
        {
            break 'fetch yaml_parser_fetch_value(parser);
        }

        // Is it an alias?
        if parser.buffer[parser.buffer_pos] == b'*' {
            break 'fetch yaml_parser_fetch_anchor(parser, YamlTokenType::Alias);
        }

        // Is it an anchor?
        if parser.buffer[parser.buffer_pos] == b'&' {
            break 'fetch yaml_parser_fetch_anchor(parser, YamlTokenType::Anchor);
        }

        // Is it a tag?
        if parser.buffer[parser.buffer_pos] == b'!' {
            break 'fetch yaml_parser_fetch_tag(parser);
        }

        // Is it a literal scalar?
        if parser.buffer[parser.buffer_pos] == b'|' && parser.flow_level == 0 {
            break 'fetch yaml_parser_fetch_block_scalar(parser, true);
        }

        // Is it a folded scalar?
        if parser.buffer[parser.buffer_pos] == b'>' && parser.flow_level == 0 {
            break 'fetch yaml_parser_fetch_block_scalar(parser, false);
        }

        // Is it a single-quoted scalar?
        if parser.buffer[parser.buffer_pos] == b'\'' {
            break 'fetch yaml_parser_fetch_flow_scalar(parser, true);
        }

        // Is it a double-quoted scalar?
        if parser.buffer[parser.buffer_pos] == b'"' {
            break 'fetch yaml_parser_fetch_flow_scalar(parser, false);
        }

        // Is it a plain scalar?
        //
        // A plain scalar may start with any non-blank characters except
        //
        //      '-', '?', ':', ',', '[', ']', '{', '}',
        //      '#', '&', '*', '!', '|', '>', '\'', '\"',
        //      '%', '@', '`'.
        //
        // In the block context (and, for the '-' indicator, in the flow context
        // too), it may also start with the characters
        //
        //      '-', '?', ':'
        //
        // if it is followed by a non-space character.
        //
        // The last rule is more restrictive than the specification requires.
        // TODO: make this logic more reasonable.
        let c = parser.buffer[parser.buffer_pos];
        if !(is_blankz(&parser.buffer, parser.buffer_pos)
            || c == b'-'
            || c == b'?'
            || c == b':'
            || c == b','
            || c == b'['
            || c == b']'
            || c == b'{'
            || c == b'}'
            || c == b'#'
            || c == b'&'
            || c == b'*'
            || c == b'!'
            || c == b'|'
            || c == b'>'
            || c == b'\''
            || c == b'"'
            || c == b'%'
            || c == b'@'
            || c == b'`')
            || (c == b'-' && !is_blank(&parser.buffer, parser.buffer_pos + 1))
            || (parser.flow_level == 0
                && (c == b'?' || c == b':')
                && !is_blankz(&parser.buffer, parser.buffer_pos + 1))
        {
            break 'fetch yaml_parser_fetch_plain_scalar(parser);
        }

        // If we don't determine the token type so far, it is an error.
        let mark = parser.mark;
        yaml_parser_set_scanner_error(
            parser,
            "while scanning for the next token",
            mark,
            "found character that cannot start any token",
        )
    };

    // The common epilogue.
    if !ok {
        return false;
    }
    if !parser.tokens.is_empty()
        && parser.tokens[parser.tokens.len() - 1].typ == YamlTokenType::BlockEntry
    {
        // Sequence indicators alone have no line comments. It becomes
        // a head comment for whatever follows.
        return true;
    }
    yaml_parser_scan_line_comment(parser, comment_mark)
}

/// Returns `(valid, ok)`; `simple_key` is an index into `parser.simple_keys`.
fn yaml_simple_key_is_valid(parser: &mut YamlParser<'_>, simple_key: usize) -> (bool, bool) {
    let key = parser.simple_keys[simple_key];
    if !key.possible {
        return (false, true);
    }

    // The 1.2 specification says:
    //
    //     "If the ? indicator is omitted, parsing needs to see past the
    //     implicit key to recognize it as such. To limit the amount of
    //     lookahead required, the “:” indicator must appear at most 1024
    //     Unicode characters beyond the start of the key. In addition, the key
    //     is restricted to a single line."
    //
    if key.mark.line < parser.mark.line || key.mark.index + 1024 < parser.mark.index {
        // Check if the potential simple key to be removed is required.
        if key.required {
            return (
                false,
                yaml_parser_set_scanner_error(
                    parser,
                    "while scanning a simple key",
                    key.mark,
                    "could not find expected ':'",
                ),
            );
        }
        parser.simple_keys[simple_key].possible = false;
        return (false, true);
    }
    (true, true)
}

/// Check if a simple key may start at the current position and add it if needed.
fn yaml_parser_save_simple_key(parser: &mut YamlParser<'_>) -> bool {
    // A simple key is required at the current position if the scanner is in
    // the block context and the current column coincides with the indentation
    // level.

    let required = parser.flow_level == 0 && parser.indent == parser.mark.column as isize;

    //
    // If the current position may start a simple key, save it.
    //
    if parser.simple_key_allowed {
        let simple_key = YamlSimpleKey {
            possible: true,
            required,
            token_number: parser.tokens_parsed + (parser.tokens.len() - parser.tokens_head),
            mark: parser.mark,
        };

        if !yaml_parser_remove_simple_key(parser) {
            return false;
        }
        let last = parser.simple_keys.len() - 1;
        parser.simple_keys[last] = simple_key;
        parser
            .simple_keys_by_tok
            .insert(simple_key.token_number, last);
    }
    true
}

/// Remove a potential simple key at the current flow level.
fn yaml_parser_remove_simple_key(parser: &mut YamlParser<'_>) -> bool {
    let i = parser.simple_keys.len() - 1;
    if parser.simple_keys[i].possible {
        // If the key is required, it is an error.
        if parser.simple_keys[i].required {
            let mark = parser.simple_keys[i].mark;
            return yaml_parser_set_scanner_error(
                parser,
                "while scanning a simple key",
                mark,
                "could not find expected ':'",
            );
        }
        // Remove the key from the stack.
        parser.simple_keys[i].possible = false;
        let token_number = parser.simple_keys[i].token_number;
        parser.simple_keys_by_tok.remove(&token_number);
    }
    true
}

/// max_flow_level limits the flow_level
const MAX_FLOW_LEVEL: usize = 10000;

/// Increase the flow level and resize the simple key list if needed.
fn yaml_parser_increase_flow_level(parser: &mut YamlParser<'_>) -> bool {
    // Reset the simple key on the next level.
    parser.simple_keys.push(YamlSimpleKey {
        possible: false,
        required: false,
        token_number: parser.tokens_parsed + (parser.tokens.len() - parser.tokens_head),
        mark: parser.mark,
    });

    // Increase the flow level.
    parser.flow_level += 1;
    if parser.flow_level > MAX_FLOW_LEVEL {
        let mark = parser.simple_keys[parser.simple_keys.len() - 1].mark;
        return yaml_parser_set_scanner_error(
            parser,
            "while increasing flow level",
            mark,
            &format!("exceeded max depth of {MAX_FLOW_LEVEL}"),
        );
    }
    true
}

/// Decrease the flow level.
fn yaml_parser_decrease_flow_level(parser: &mut YamlParser<'_>) -> bool {
    if parser.flow_level > 0 {
        parser.flow_level -= 1;
        let last = parser.simple_keys.len() - 1;
        let token_number = parser.simple_keys[last].token_number;
        parser.simple_keys_by_tok.remove(&token_number);
        parser.simple_keys.truncate(last);
    }
    true
}

/// max_indents limits the indents stack size
const MAX_INDENTS: usize = 10000;

/// Push the current indentation level to the stack and set the new level the current column
/// is greater than the indentation level.  In this case, append or insert the specified token
/// into the token queue.
fn yaml_parser_roll_indent(
    parser: &mut YamlParser<'_>,
    column: isize,
    number: isize,
    typ: YamlTokenType,
    mark: YamlMark,
) -> bool {
    // In the flow context, do nothing.
    if parser.flow_level > 0 {
        return true;
    }

    if parser.indent < column {
        // Push the current indentation level to the stack and set the new
        // indentation level.
        parser.indents.push(parser.indent);
        parser.indent = column;
        if parser.indents.len() > MAX_INDENTS {
            let mark = parser.simple_keys[parser.simple_keys.len() - 1].mark;
            return yaml_parser_set_scanner_error(
                parser,
                "while increasing indent level",
                mark,
                &format!("exceeded max depth of {MAX_INDENTS}"),
            );
        }

        // Create a token and insert it into the queue.
        let token = YamlToken {
            typ,
            start_mark: mark,
            end_mark: mark,
            ..YamlToken::default()
        };
        let mut number = number;
        if number > -1 {
            number -= parser.tokens_parsed as isize;
        }
        yaml_insert_token(parser, number, token);
    }
    true
}

/// Pop indentation levels from the indents stack until the current level becomes less or equal
/// to the column.  For each indentation level, append the BLOCK-END token.
fn yaml_parser_unroll_indent(
    parser: &mut YamlParser<'_>,
    column: isize,
    scan_mark: YamlMark,
) -> bool {
    // In the flow context, do nothing.
    if parser.flow_level > 0 {
        return true;
    }

    let mut block_mark = scan_mark;
    // The loop body below needs `indent > column`, i.e. an indentation rolled by an earlier
    // token, so the scan mark is past index 0 whenever the value is used.
    block_mark.index = block_mark.index.wrapping_sub(1);

    // Loop through the indentation levels in the stack.
    while parser.indent > column {
        // Reposition the end token before potential following foot comments of parent
        // blocks. For that, search backwards for recent comments that were at the same indent
        // as the block that is ending now.
        let mut stop_index = block_mark.index;
        for i in (0..parser.comments.len()).rev() {
            let comment = &parser.comments[i];

            if comment.end_mark.index < stop_index {
                // Don't go back beyond the start of the comment/whitespace scan, unless column < 0.
                // If requested indent column is < 0, then the document is over and everything else
                // is a foot anyway.
                break;
            }
            if comment.start_mark.column as isize == parser.indent + 1 {
                // This is a good match. But maybe there's a former comment
                // at that same indent level, so keep searching.
                block_mark = comment.start_mark;
            }

            // While the end of the former comment matches with
            // the start of the following one, we know there's
            // nothing in between and scanning is still safe.
            stop_index = comment.scan_mark.index;
        }

        // Create a token and append it to the queue.
        let token = YamlToken {
            typ: YamlTokenType::BlockEnd,
            start_mark: block_mark,
            end_mark: block_mark,
            ..YamlToken::default()
        };
        yaml_insert_token(parser, -1, token);

        // Pop the indentation level.
        parser.indent = parser.indents[parser.indents.len() - 1];
        parser.indents.truncate(parser.indents.len() - 1);
    }
    true
}

/// Initialize the scanner and produce the STREAM-START token.
fn yaml_parser_fetch_stream_start(parser: &mut YamlParser<'_>) -> bool {
    // Set the initial indentation.
    parser.indent = -1;

    // Initialize the simple key stack.
    parser.simple_keys.push(YamlSimpleKey::default());

    parser.simple_keys_by_tok = std::collections::HashMap::new();

    // A simple key is allowed at the beginning of the stream.
    parser.simple_key_allowed = true;

    // We have started.
    parser.stream_start_produced = true;

    // Create the STREAM-START token and append it to the queue.
    let token = YamlToken {
        typ: YamlTokenType::StreamStart,
        start_mark: parser.mark,
        end_mark: parser.mark,
        encoding: parser.encoding,
        ..YamlToken::default()
    };
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the STREAM-END token and shut down the scanner.
fn yaml_parser_fetch_stream_end(parser: &mut YamlParser<'_>) -> bool {
    // Force new line.
    if parser.mark.column != 0 {
        parser.mark.column = 0;
        parser.mark.line += 1;
    }

    // Reset the indentation level.
    let mark = parser.mark;
    if !yaml_parser_unroll_indent(parser, -1, mark) {
        return false;
    }

    // Reset simple keys.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    parser.simple_key_allowed = false;

    // Create the STREAM-END token and append it to the queue.
    let token = YamlToken {
        typ: YamlTokenType::StreamEnd,
        start_mark: parser.mark,
        end_mark: parser.mark,
        ..YamlToken::default()
    };
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce a VERSION-DIRECTIVE or TAG-DIRECTIVE token.
fn yaml_parser_fetch_directive(parser: &mut YamlParser<'_>) -> bool {
    // Reset the indentation level.
    let mark = parser.mark;
    if !yaml_parser_unroll_indent(parser, -1, mark) {
        return false;
    }

    // Reset simple keys.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    parser.simple_key_allowed = false;

    // Create the YAML-DIRECTIVE or TAG-DIRECTIVE token.
    let mut token = YamlToken::default();
    if !yaml_parser_scan_directive(parser, &mut token) {
        return false;
    }
    // Append the token to the queue.
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the DOCUMENT-START or DOCUMENT-END token.
fn yaml_parser_fetch_document_indicator(parser: &mut YamlParser<'_>, typ: YamlTokenType) -> bool {
    // Reset the indentation level.
    let mark = parser.mark;
    if !yaml_parser_unroll_indent(parser, -1, mark) {
        return false;
    }

    // Reset simple keys.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    parser.simple_key_allowed = false;

    // Consume the token.
    let start_mark = parser.mark;

    skip(parser);
    skip(parser);
    skip(parser);

    let end_mark = parser.mark;

    // Create the DOCUMENT-START or DOCUMENT-END token.
    let token = YamlToken {
        typ,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    // Append the token to the queue.
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the FLOW-SEQUENCE-START or FLOW-MAPPING-START token.
fn yaml_parser_fetch_flow_collection_start(
    parser: &mut YamlParser<'_>,
    typ: YamlTokenType,
) -> bool {
    // The indicators '[' and '{' may start a simple key.
    if !yaml_parser_save_simple_key(parser) {
        return false;
    }

    // Increase the flow level.
    if !yaml_parser_increase_flow_level(parser) {
        return false;
    }

    // A simple key may follow the indicators '[' and '{'.
    parser.simple_key_allowed = true;

    // Consume the token.
    let start_mark = parser.mark;
    skip(parser);
    let end_mark = parser.mark;

    // Create the FLOW-SEQUENCE-START of FLOW-MAPPING-START token.
    let token = YamlToken {
        typ,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    // Append the token to the queue.
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the FLOW-SEQUENCE-END or FLOW-MAPPING-END token.
fn yaml_parser_fetch_flow_collection_end(parser: &mut YamlParser<'_>, typ: YamlTokenType) -> bool {
    // Reset any potential simple key on the current flow level.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    // Decrease the flow level.
    if !yaml_parser_decrease_flow_level(parser) {
        return false;
    }

    // No simple keys after the indicators ']' and '}'.
    parser.simple_key_allowed = false;

    // Consume the token.

    let start_mark = parser.mark;
    skip(parser);
    let end_mark = parser.mark;

    // Create the FLOW-SEQUENCE-END of FLOW-MAPPING-END token.
    let token = YamlToken {
        typ,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    // Append the token to the queue.
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the FLOW-ENTRY token.
fn yaml_parser_fetch_flow_entry(parser: &mut YamlParser<'_>) -> bool {
    // Reset any potential simple keys on the current flow level.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    // Simple keys are allowed after ','.
    parser.simple_key_allowed = true;

    // Consume the token.
    let start_mark = parser.mark;
    skip(parser);
    let end_mark = parser.mark;

    // Create the FLOW-ENTRY token and append it to the queue.
    let token = YamlToken {
        typ: YamlTokenType::FlowEntry,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the BLOCK-ENTRY token.
fn yaml_parser_fetch_block_entry(parser: &mut YamlParser<'_>) -> bool {
    // Check if the scanner is in the block context.
    if parser.flow_level == 0 {
        // Check if we are allowed to start a new entry.
        if !parser.simple_key_allowed {
            let mark = parser.mark;
            return yaml_parser_set_scanner_error(
                parser,
                "",
                mark,
                "block sequence entries are not allowed in this context",
            );
        }
        // Add the BLOCK-SEQUENCE-START token if needed.
        let mark = parser.mark;
        if !yaml_parser_roll_indent(
            parser,
            mark.column as isize,
            -1,
            YamlTokenType::BlockSequenceStart,
            mark,
        ) {
            return false;
        }
    } else {
        // It is an error for the '-' indicator to occur in the flow context,
        // but we let the Parser detect and report about it because the Parser
        // is able to point to the context.
    }

    // Reset any potential simple keys on the current flow level.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    // Simple keys are allowed after '-'.
    parser.simple_key_allowed = true;

    // Consume the token.
    let start_mark = parser.mark;
    skip(parser);
    let end_mark = parser.mark;

    // Create the BLOCK-ENTRY token and append it to the queue.
    let token = YamlToken {
        typ: YamlTokenType::BlockEntry,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the KEY token.
fn yaml_parser_fetch_key(parser: &mut YamlParser<'_>) -> bool {
    // In the block context, additional checks are required.
    if parser.flow_level == 0 {
        // Check if we are allowed to start a new key (not nessesary simple).
        if !parser.simple_key_allowed {
            let mark = parser.mark;
            return yaml_parser_set_scanner_error(
                parser,
                "",
                mark,
                "mapping keys are not allowed in this context",
            );
        }
        // Add the BLOCK-MAPPING-START token if needed.
        let mark = parser.mark;
        if !yaml_parser_roll_indent(
            parser,
            mark.column as isize,
            -1,
            YamlTokenType::BlockMappingStart,
            mark,
        ) {
            return false;
        }
    }

    // Reset any potential simple keys on the current flow level.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    // Simple keys are allowed after '?' in the block context.
    parser.simple_key_allowed = parser.flow_level == 0;

    // Consume the token.
    let start_mark = parser.mark;
    skip(parser);
    let end_mark = parser.mark;

    // Create the KEY token and append it to the queue.
    let token = YamlToken {
        typ: YamlTokenType::Key,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the VALUE token.
fn yaml_parser_fetch_value(parser: &mut YamlParser<'_>) -> bool {
    // The simple key is the last entry of `parser.simple_keys`.
    let simple_key = parser.simple_keys.len() - 1;

    // Have we found a simple key?
    let (valid, ok) = yaml_simple_key_is_valid(parser, simple_key);
    if !ok {
        return false;
    } else if valid {
        let key = parser.simple_keys[simple_key];

        // Create the KEY token and insert it into the queue.
        let token = YamlToken {
            typ: YamlTokenType::Key,
            start_mark: key.mark,
            end_mark: key.mark,
            ..YamlToken::default()
        };
        yaml_insert_token(
            parser,
            key.token_number as isize - parser.tokens_parsed as isize,
            token,
        );

        // In the block context, we may need to add the BLOCK-MAPPING-START token.
        if !yaml_parser_roll_indent(
            parser,
            key.mark.column as isize,
            key.token_number as isize,
            YamlTokenType::BlockMappingStart,
            key.mark,
        ) {
            return false;
        }

        // Remove the simple key.
        parser.simple_keys[simple_key].possible = false;
        parser.simple_keys_by_tok.remove(&key.token_number);

        // A simple key cannot follow another simple key.
        parser.simple_key_allowed = false;
    } else {
        // The ':' indicator follows a complex key.

        // In the block context, extra checks are required.
        if parser.flow_level == 0 {
            // Check if we are allowed to start a complex value.
            if !parser.simple_key_allowed {
                let mark = parser.mark;
                return yaml_parser_set_scanner_error(
                    parser,
                    "",
                    mark,
                    "mapping values are not allowed in this context",
                );
            }

            // Add the BLOCK-MAPPING-START token if needed.
            let mark = parser.mark;
            if !yaml_parser_roll_indent(
                parser,
                mark.column as isize,
                -1,
                YamlTokenType::BlockMappingStart,
                mark,
            ) {
                return false;
            }
        }

        // Simple keys after ':' are allowed in the block context.
        parser.simple_key_allowed = parser.flow_level == 0;
    }

    // Consume the token.
    let start_mark = parser.mark;
    skip(parser);
    let end_mark = parser.mark;

    // Create the VALUE token and append it to the queue.
    let token = YamlToken {
        typ: YamlTokenType::Value,
        start_mark,
        end_mark,
        ..YamlToken::default()
    };
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the ALIAS or ANCHOR token.
fn yaml_parser_fetch_anchor(parser: &mut YamlParser<'_>, typ: YamlTokenType) -> bool {
    // An anchor or an alias could be a simple key.
    if !yaml_parser_save_simple_key(parser) {
        return false;
    }

    // A simple key cannot follow an anchor or an alias.
    parser.simple_key_allowed = false;

    // Create the ALIAS or ANCHOR token and append it to the queue.
    let mut token = YamlToken::default();
    if !yaml_parser_scan_anchor(parser, &mut token, typ) {
        return false;
    }
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the TAG token.
fn yaml_parser_fetch_tag(parser: &mut YamlParser<'_>) -> bool {
    // A tag could be a simple key.
    if !yaml_parser_save_simple_key(parser) {
        return false;
    }

    // A simple key cannot follow a tag.
    parser.simple_key_allowed = false;

    // Create the TAG token and append it to the queue.
    let mut token = YamlToken::default();
    if !yaml_parser_scan_tag(parser, &mut token) {
        return false;
    }
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the SCALAR(...,literal) or SCALAR(...,folded) tokens.
fn yaml_parser_fetch_block_scalar(parser: &mut YamlParser<'_>, literal: bool) -> bool {
    // Remove any potential simple keys.
    if !yaml_parser_remove_simple_key(parser) {
        return false;
    }

    // A simple key may follow a block scalar.
    parser.simple_key_allowed = true;

    // Create the SCALAR token and append it to the queue.
    let mut token = YamlToken::default();
    if !yaml_parser_scan_block_scalar(parser, &mut token, literal) {
        return false;
    }
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the SCALAR(...,single-quoted) or SCALAR(...,double-quoted) tokens.
fn yaml_parser_fetch_flow_scalar(parser: &mut YamlParser<'_>, single: bool) -> bool {
    // A plain scalar could be a simple key.
    if !yaml_parser_save_simple_key(parser) {
        return false;
    }

    // A simple key cannot follow a flow scalar.
    parser.simple_key_allowed = false;

    // Create the SCALAR token and append it to the queue.
    let mut token = YamlToken::default();
    if !yaml_parser_scan_flow_scalar(parser, &mut token, single) {
        return false;
    }
    yaml_insert_token(parser, -1, token);
    true
}

/// Produce the SCALAR(...,plain) token.
fn yaml_parser_fetch_plain_scalar(parser: &mut YamlParser<'_>) -> bool {
    // A plain scalar could be a simple key.
    if !yaml_parser_save_simple_key(parser) {
        return false;
    }

    // A simple key cannot follow a flow scalar.
    parser.simple_key_allowed = false;

    // Create the SCALAR token and append it to the queue.
    let mut token = YamlToken::default();
    if !yaml_parser_scan_plain_scalar(parser, &mut token) {
        return false;
    }
    yaml_insert_token(parser, -1, token);
    true
}

/// Eat whitespaces and comments until the next token is found.
fn yaml_parser_scan_to_next_token(parser: &mut YamlParser<'_>) -> bool {
    let scan_mark = parser.mark;

    // Until the next token is not found.
    loop {
        // Allow the BOM mark to start a line.
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
        if parser.mark.column == 0 && is_bom(&parser.buffer, parser.buffer_pos) {
            skip(parser);
        }

        // Eat whitespaces.
        // Tabs are allowed:
        //  - in the flow context
        //  - in the block context, but not at the beginning of the line or
        //  after '-', '?', or ':' (complex value).
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }

        while parser.buffer[parser.buffer_pos] == b' '
            || ((parser.flow_level > 0 || !parser.simple_key_allowed)
                && parser.buffer[parser.buffer_pos] == b'\t')
        {
            skip(parser);
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }

        // Check if we just had a line comment under a sequence entry that
        // looks more like a header to the following content. Similar to this:
        //
        // - # The comment
        //   - Some data
        //
        // If so, transform the line comment to a head comment and reposition.
        if !parser.comments.is_empty() && parser.tokens.len() > 1 {
            let token_a = parser.tokens[parser.tokens.len() - 2].typ;
            let token_b = parser.tokens[parser.tokens.len() - 1].typ;
            let comment = parser.comments.len() - 1;
            if token_a == YamlTokenType::BlockSequenceStart
                && token_b == YamlTokenType::BlockEntry
                && !parser.comments[comment].line.is_empty()
                && !is_break(&parser.buffer, parser.buffer_pos)
            {
                // If it was in the prior line, reposition so it becomes a
                // header of the follow up token. Otherwise, keep it in place
                // so it becomes a header of the former.
                let mark = parser.mark;
                let comment = &mut parser.comments[comment];
                comment.head = std::mem::take(&mut comment.line);
                // The comment started on the line before the current mark.
                if comment.start_mark.line + 1 == mark.line {
                    comment.token_mark = mark;
                }
            }
        }

        // Eat a comment until a line break.
        if parser.buffer[parser.buffer_pos] == b'#' && !yaml_parser_scan_comments(parser, scan_mark)
        {
            return false;
        }

        // If it is a line break, eat it.
        if is_break(&parser.buffer, parser.buffer_pos) {
            if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                return false;
            }
            skip_line(parser);

            // In the block context, a new line may start a simple key.
            if parser.flow_level == 0 {
                parser.simple_key_allowed = true;
            }
        } else {
            break; // We have found a token.
        }
    }

    true
}

/// Scan a YAML-DIRECTIVE or TAG-DIRECTIVE token.
///
/// Scope:
/// ```text
///      %YAML    1.1    # a comment \n
///      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
///      %TAG    !yaml!  tag:yaml.org,2002:  \n
///      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
/// ```
fn yaml_parser_scan_directive(parser: &mut YamlParser<'_>, token: &mut YamlToken) -> bool {
    // Eat '%'.
    let start_mark = parser.mark;
    skip(parser);

    // Scan the directive name.
    let mut name = Vec::new();
    if !yaml_parser_scan_directive_name(parser, start_mark, &mut name) {
        return false;
    }

    // Is it a YAML directive?
    if name == b"YAML" {
        // Scan the VERSION directive value.
        let mut major = 0i8;
        let mut minor = 0i8;
        if !yaml_parser_scan_version_directive_value(parser, start_mark, &mut major, &mut minor) {
            return false;
        }
        let end_mark = parser.mark;

        // Create a VERSION-DIRECTIVE token.
        *token = YamlToken {
            typ: YamlTokenType::VersionDirective,
            start_mark,
            end_mark,
            major,
            minor,
            ..YamlToken::default()
        };

        // Is it a TAG directive?
    } else if name == b"TAG" {
        // Scan the TAG directive value.
        let mut handle = Vec::new();
        let mut prefix = Vec::new();
        if !yaml_parser_scan_tag_directive_value(parser, start_mark, &mut handle, &mut prefix) {
            return false;
        }
        let end_mark = parser.mark;

        // Create a TAG-DIRECTIVE token.
        *token = YamlToken {
            typ: YamlTokenType::TagDirective,
            start_mark,
            end_mark,
            value: handle,
            prefix,
            ..YamlToken::default()
        };

        // Unknown directive.
    } else {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a directive",
            start_mark,
            "found unknown directive name",
        );
        return false;
    }

    // Eat the rest of the line including any comments.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    while is_blank(&parser.buffer, parser.buffer_pos) {
        skip(parser);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    if parser.buffer[parser.buffer_pos] == b'#' {
        // Discard this inline comment for the time being.
        //if !yaml_parser_scan_line_comment(parser, start_mark) {
        //	return false
        //}
        while !is_breakz(&parser.buffer, parser.buffer_pos) {
            skip(parser);
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }
    }

    // Check if we are at the end of the line.
    if !is_breakz(&parser.buffer, parser.buffer_pos) {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a directive",
            start_mark,
            "did not find expected comment or line break",
        );
        return false;
    }

    // Eat a line break.
    if is_break(&parser.buffer, parser.buffer_pos) {
        if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
            return false;
        }
        skip_line(parser);
    }

    true
}

/// Scan the directive name.
///
/// Scope:
/// ```text
///      %YAML   1.1     # a comment \n
///       ^^^^
///      %TAG    !yaml!  tag:yaml.org,2002:  \n
///       ^^^
/// ```
fn yaml_parser_scan_directive_name(
    parser: &mut YamlParser<'_>,
    start_mark: YamlMark,
    name: &mut Vec<u8>,
) -> bool {
    // Consume the directive name.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    let mut s = Vec::new();
    while is_alpha(&parser.buffer, parser.buffer_pos) {
        read(parser, &mut s);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    // Check if the name is empty.
    if s.is_empty() {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a directive",
            start_mark,
            "could not find expected directive name",
        );
        return false;
    }

    // Check for an blank character after the name.
    if !is_blankz(&parser.buffer, parser.buffer_pos) {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a directive",
            start_mark,
            "found unexpected non-alphabetical character",
        );
        return false;
    }
    *name = s;
    true
}

/// Scan the value of VERSION-DIRECTIVE.
///
/// Scope:
/// ```text
///      %YAML   1.1     # a comment \n
///           ^^^^^^
/// ```
fn yaml_parser_scan_version_directive_value(
    parser: &mut YamlParser<'_>,
    start_mark: YamlMark,
    major: &mut i8,
    minor: &mut i8,
) -> bool {
    // Eat whitespaces.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    while is_blank(&parser.buffer, parser.buffer_pos) {
        skip(parser);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    // Consume the major version number.
    if !yaml_parser_scan_version_directive_number(parser, start_mark, major) {
        return false;
    }

    // Eat '.'.
    if parser.buffer[parser.buffer_pos] != b'.' {
        return yaml_parser_set_scanner_error(
            parser,
            "while scanning a %YAML directive",
            start_mark,
            "did not find expected digit or '.' character",
        );
    }

    skip(parser);

    // Consume the minor version number.
    if !yaml_parser_scan_version_directive_number(parser, start_mark, minor) {
        return false;
    }
    true
}

const MAX_NUMBER_LENGTH: i8 = 2;

/// Scan the version number of VERSION-DIRECTIVE.
///
/// Scope:
/// ```text
///      %YAML   1.1     # a comment \n
///              ^
///      %YAML   1.1     # a comment \n
///                ^
/// ```
fn yaml_parser_scan_version_directive_number(
    parser: &mut YamlParser<'_>,
    start_mark: YamlMark,
    number: &mut i8,
) -> bool {
    // Repeat while the next character is digit.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    let mut value: i8 = 0;
    let mut length: i8 = 0;
    while is_digit(&parser.buffer, parser.buffer_pos) {
        // Check if the number is too long.
        length += 1;
        if length > MAX_NUMBER_LENGTH {
            return yaml_parser_set_scanner_error(
                parser,
                "while scanning a %YAML directive",
                start_mark,
                "found extremely long version number",
            );
        }
        value = value * 10 + as_digit(&parser.buffer, parser.buffer_pos) as i8;
        skip(parser);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    // Check if the number was present.
    if length == 0 {
        return yaml_parser_set_scanner_error(
            parser,
            "while scanning a %YAML directive",
            start_mark,
            "did not find expected version number",
        );
    }
    *number = value;
    true
}

/// Scan the value of a TAG-DIRECTIVE token.
///
/// Scope:
/// ```text
///      %TAG    !yaml!  tag:yaml.org,2002:  \n
///          ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
/// ```
fn yaml_parser_scan_tag_directive_value(
    parser: &mut YamlParser<'_>,
    start_mark: YamlMark,
    handle: &mut Vec<u8>,
    prefix: &mut Vec<u8>,
) -> bool {
    let mut handle_value = Vec::new();
    let mut prefix_value = Vec::new();

    // Eat whitespaces.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    while is_blank(&parser.buffer, parser.buffer_pos) {
        skip(parser);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    // Scan a handle.
    if !yaml_parser_scan_tag_handle(parser, true, start_mark, &mut handle_value) {
        return false;
    }

    // Expect a whitespace.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    if !is_blank(&parser.buffer, parser.buffer_pos) {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a %TAG directive",
            start_mark,
            "did not find expected whitespace",
        );
        return false;
    }

    // Eat whitespaces.
    while is_blank(&parser.buffer, parser.buffer_pos) {
        skip(parser);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    // Scan a prefix.
    if !yaml_parser_scan_tag_uri(parser, true, &[], start_mark, &mut prefix_value) {
        return false;
    }

    // Expect a whitespace or line break.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    if !is_blankz(&parser.buffer, parser.buffer_pos) {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a %TAG directive",
            start_mark,
            "did not find expected whitespace or line break",
        );
        return false;
    }

    *handle = handle_value;
    *prefix = prefix_value;
    true
}

fn yaml_parser_scan_anchor(
    parser: &mut YamlParser<'_>,
    token: &mut YamlToken,
    typ: YamlTokenType,
) -> bool {
    let mut s = Vec::new();

    // Eat the indicator character.
    let start_mark = parser.mark;
    skip(parser);

    // Consume the value.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    while is_alpha(&parser.buffer, parser.buffer_pos) {
        read(parser, &mut s);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    let end_mark = parser.mark;

    /*
     * Check if length of the anchor is greater than 0 and it is followed by
     * a whitespace character or one of the indicators:
     *
     *      '?', ':', ',', ']', '}', '%', '@', '`'.
     */

    let c = parser.buffer[parser.buffer_pos];
    if s.is_empty()
        || !(is_blankz(&parser.buffer, parser.buffer_pos)
            || c == b'?'
            || c == b':'
            || c == b','
            || c == b']'
            || c == b'}'
            || c == b'%'
            || c == b'@'
            || c == b'`')
    {
        let context = if typ == YamlTokenType::Anchor {
            "while scanning an anchor"
        } else {
            "while scanning an alias"
        };
        yaml_parser_set_scanner_error(
            parser,
            context,
            start_mark,
            "did not find expected alphabetic or numeric character",
        );
        return false;
    }

    // Create a token.
    *token = YamlToken {
        typ,
        start_mark,
        end_mark,
        value: s,
        ..YamlToken::default()
    };

    true
}

/*
 * Scan a TAG token.
 */

fn yaml_parser_scan_tag(parser: &mut YamlParser<'_>, token: &mut YamlToken) -> bool {
    let mut handle = Vec::new();
    let mut suffix = Vec::new();

    let start_mark = parser.mark;

    // Check if the tag is in the canonical form.
    if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
        return false;
    }

    if parser.buffer[parser.buffer_pos + 1] == b'<' {
        // Keep the handle as ''

        // Eat '!<'
        skip(parser);
        skip(parser);

        // Consume the tag value.
        if !yaml_parser_scan_tag_uri(parser, false, &[], start_mark, &mut suffix) {
            return false;
        }

        // Check for '>' and eat it.
        if parser.buffer[parser.buffer_pos] != b'>' {
            yaml_parser_set_scanner_error(
                parser,
                "while scanning a tag",
                start_mark,
                "did not find the expected '>'",
            );
            return false;
        }

        skip(parser);
    } else {
        // The tag has either the '!suffix' or the '!handle!suffix' form.

        // First, try to scan a handle.
        if !yaml_parser_scan_tag_handle(parser, false, start_mark, &mut handle) {
            return false;
        }

        // Check if it is, indeed, handle.
        if handle[0] == b'!' && handle.len() > 1 && handle[handle.len() - 1] == b'!' {
            // Scan the suffix now.
            if !yaml_parser_scan_tag_uri(parser, false, &[], start_mark, &mut suffix) {
                return false;
            }
        } else {
            // It wasn't a handle after all.  Scan the rest of the tag.
            if !yaml_parser_scan_tag_uri(parser, false, &handle, start_mark, &mut suffix) {
                return false;
            }

            // Set the handle to '!'.
            handle = vec![b'!'];

            // A special case: the '!' tag.  Set the handle to '' and the
            // suffix to '!'.
            if suffix.is_empty() {
                std::mem::swap(&mut handle, &mut suffix);
            }
        }
    }

    // Check the character which ends the tag.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    if !is_blankz(&parser.buffer, parser.buffer_pos) {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a tag",
            start_mark,
            "did not find expected whitespace or line break",
        );
        return false;
    }

    let end_mark = parser.mark;

    // Create a token.
    *token = YamlToken {
        typ: YamlTokenType::Tag,
        start_mark,
        end_mark,
        value: handle,
        suffix,
        ..YamlToken::default()
    };
    true
}

/// Scan a tag handle.
fn yaml_parser_scan_tag_handle(
    parser: &mut YamlParser<'_>,
    directive: bool,
    start_mark: YamlMark,
    handle: &mut Vec<u8>,
) -> bool {
    // Check the initial '!' character.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    if parser.buffer[parser.buffer_pos] != b'!' {
        yaml_parser_set_scanner_tag_error(
            parser,
            directive,
            start_mark,
            "did not find expected '!'",
        );
        return false;
    }

    let mut s = Vec::new();

    // Copy the '!' character.
    read(parser, &mut s);

    // Copy all subsequent alphabetical and numerical characters.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    while is_alpha(&parser.buffer, parser.buffer_pos) {
        read(parser, &mut s);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }

    // Check if the trailing character is '!' and copy it.
    if parser.buffer[parser.buffer_pos] == b'!' {
        read(parser, &mut s);
    } else {
        // It's either the '!' tag or not really a tag handle.  If it's a %TAG
        // directive, it's an error.  If it's a tag token, it must be a part of URI.
        if directive && s != b"!" {
            yaml_parser_set_scanner_tag_error(
                parser,
                directive,
                start_mark,
                "did not find expected '!'",
            );
            return false;
        }
    }

    *handle = s;
    true
}

/// Scan a tag.
fn yaml_parser_scan_tag_uri(
    parser: &mut YamlParser<'_>,
    directive: bool,
    head: &[u8],
    start_mark: YamlMark,
    uri: &mut Vec<u8>,
) -> bool {
    //size_t length = head ? strlen((char *)head) : 0
    let mut s = Vec::new();
    let mut has_tag = !head.is_empty();

    // Copy the head if needed.
    //
    // Note that we don't copy the leading '!' character.
    if head.len() > 1 {
        s.extend_from_slice(&head[1..]);
    }

    // Scan the tag.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    // The set of characters that may appear in URI is as follows:
    //
    //      '0'-'9', 'A'-'Z', 'a'-'z', '_', '-', ';', '/', '?', ':', '@', '&',
    //      '=', '+', '$', ',', '.', '!', '~', '*', '\'', '(', ')', '[', ']',
    //      '%'.
    // TODO: convert this into more reasonable logic.
    while is_alpha(&parser.buffer, parser.buffer_pos)
        || matches!(
            parser.buffer[parser.buffer_pos],
            b';' | b'/'
                | b'?'
                | b':'
                | b'@'
                | b'&'
                | b'='
                | b'+'
                | b'$'
                | b','
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')'
                | b'['
                | b']'
                | b'%'
        )
    {
        // Check if it is a URI-escape sequence.
        if parser.buffer[parser.buffer_pos] == b'%' {
            if !yaml_parser_scan_uri_escapes(parser, directive, start_mark, &mut s) {
                return false;
            }
        } else {
            read(parser, &mut s);
        }
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
        has_tag = true;
    }

    if !has_tag {
        yaml_parser_set_scanner_tag_error(
            parser,
            directive,
            start_mark,
            "did not find expected tag URI",
        );
        return false;
    }
    *uri = s;
    true
}

/// Decode an URI-escape sequence corresponding to a single UTF-8 character.
fn yaml_parser_scan_uri_escapes(
    parser: &mut YamlParser<'_>,
    directive: bool,
    start_mark: YamlMark,
    s: &mut Vec<u8>,
) -> bool {
    // Decode the required number of characters.
    let mut w: usize = 1024;
    while w > 0 {
        // Check for a URI-escaped octet.
        if parser.unread < 3 && !yaml_parser_update_buffer(parser, 3) {
            return false;
        }

        if !(parser.buffer[parser.buffer_pos] == b'%'
            && is_hex(&parser.buffer, parser.buffer_pos + 1)
            && is_hex(&parser.buffer, parser.buffer_pos + 2))
        {
            return yaml_parser_set_scanner_tag_error(
                parser,
                directive,
                start_mark,
                "did not find URI escaped octet",
            );
        }

        // Get the octet.
        let octet = ((as_hex(&parser.buffer, parser.buffer_pos + 1) << 4)
            + as_hex(&parser.buffer, parser.buffer_pos + 2)) as u8;

        // If it is the leading octet, determine the length of the UTF-8 sequence.
        if w == 1024 {
            w = width(octet);
            if w == 0 {
                return yaml_parser_set_scanner_tag_error(
                    parser,
                    directive,
                    start_mark,
                    "found an incorrect leading UTF-8 octet",
                );
            }
        } else {
            // Check if the trailing octet is correct.
            if octet & 0xC0 != 0x80 {
                return yaml_parser_set_scanner_tag_error(
                    parser,
                    directive,
                    start_mark,
                    "found an incorrect trailing UTF-8 octet",
                );
            }
        }

        // Copy the octet and move the pointers.
        s.push(octet);
        skip(parser);
        skip(parser);
        skip(parser);
        w -= 1;
    }
    true
}

/// Scan a block scalar.
fn yaml_parser_scan_block_scalar(
    parser: &mut YamlParser<'_>,
    token: &mut YamlToken,
    literal: bool,
) -> bool {
    // Eat the indicator '|' or '>'.
    let start_mark = parser.mark;
    skip(parser);

    // Scan the additional block scalar indicators.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }

    // Check for a chomping indicator.
    let mut chomping: isize = 0;
    let mut increment: isize = 0;
    if parser.buffer[parser.buffer_pos] == b'+' || parser.buffer[parser.buffer_pos] == b'-' {
        // Set the chomping method and eat the indicator.
        if parser.buffer[parser.buffer_pos] == b'+' {
            chomping = 1;
        } else {
            chomping = -1;
        }
        skip(parser);

        // Check for an indentation indicator.
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
        if is_digit(&parser.buffer, parser.buffer_pos) {
            // Check that the indentation is greater than 0.
            if parser.buffer[parser.buffer_pos] == b'0' {
                yaml_parser_set_scanner_error(
                    parser,
                    "while scanning a block scalar",
                    start_mark,
                    "found an indentation indicator equal to 0",
                );
                return false;
            }

            // Get the indentation level and eat the indicator.
            increment = as_digit(&parser.buffer, parser.buffer_pos);
            skip(parser);
        }
    } else if is_digit(&parser.buffer, parser.buffer_pos) {
        // Do the same as above, but in the opposite order.

        if parser.buffer[parser.buffer_pos] == b'0' {
            yaml_parser_set_scanner_error(
                parser,
                "while scanning a block scalar",
                start_mark,
                "found an indentation indicator equal to 0",
            );
            return false;
        }
        increment = as_digit(&parser.buffer, parser.buffer_pos);
        skip(parser);

        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
        if parser.buffer[parser.buffer_pos] == b'+' || parser.buffer[parser.buffer_pos] == b'-' {
            if parser.buffer[parser.buffer_pos] == b'+' {
                chomping = 1;
            } else {
                chomping = -1;
            }
            skip(parser);
        }
    }

    // Eat whitespaces and comments to the end of the line.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    while is_blank(&parser.buffer, parser.buffer_pos) {
        skip(parser);
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
    }
    if parser.buffer[parser.buffer_pos] == b'#' {
        if !yaml_parser_scan_line_comment(parser, start_mark) {
            return false;
        }
        while !is_breakz(&parser.buffer, parser.buffer_pos) {
            skip(parser);
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }
    }

    // Check if we are at the end of the line.
    if !is_breakz(&parser.buffer, parser.buffer_pos) {
        yaml_parser_set_scanner_error(
            parser,
            "while scanning a block scalar",
            start_mark,
            "did not find expected comment or line break",
        );
        return false;
    }

    // Eat a line break.
    if is_break(&parser.buffer, parser.buffer_pos) {
        if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
            return false;
        }
        skip_line(parser);
    }

    let mut end_mark = parser.mark;

    // Set the indentation level if it was specified.
    let mut indent: isize = 0;
    if increment > 0 {
        if parser.indent >= 0 {
            indent = parser.indent + increment;
        } else {
            indent = increment;
        }
    }

    // Scan the leading line breaks and determine the indentation level if needed.
    let mut s = Vec::new();
    let mut leading_break = Vec::new();
    let mut trailing_breaks = Vec::new();
    if !yaml_parser_scan_block_scalar_breaks(
        parser,
        &mut indent,
        &mut trailing_breaks,
        start_mark,
        &mut end_mark,
    ) {
        return false;
    }

    // Scan the block scalar content.
    if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
        return false;
    }
    let mut leading_blank = false;
    let mut trailing_blank;
    while parser.mark.column as isize == indent && !is_z(&parser.buffer, parser.buffer_pos) {
        // We are at the beginning of a non-empty line.

        // Is it a trailing whitespace?
        trailing_blank = is_blank(&parser.buffer, parser.buffer_pos);

        // Check if we need to fold the leading line break.
        if !literal
            && !leading_blank
            && !trailing_blank
            && !leading_break.is_empty()
            && leading_break[0] == b'\n'
        {
            // Do we need to join the lines by space?
            if trailing_breaks.is_empty() {
                s.push(b' ');
            }
        } else {
            s.extend_from_slice(&leading_break);
        }
        leading_break.clear();

        // Append the remaining line breaks.
        s.extend_from_slice(&trailing_breaks);
        trailing_breaks.clear();

        // Is it a leading whitespace?
        leading_blank = is_blank(&parser.buffer, parser.buffer_pos);

        // Consume the current line.
        while !is_breakz(&parser.buffer, parser.buffer_pos) {
            read(parser, &mut s);
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }

        // Consume the line break.
        if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
            return false;
        }

        read_line(parser, &mut leading_break);

        // Eat the following indentation spaces and line breaks.
        if !yaml_parser_scan_block_scalar_breaks(
            parser,
            &mut indent,
            &mut trailing_breaks,
            start_mark,
            &mut end_mark,
        ) {
            return false;
        }
    }

    // Chomp the tail.
    if chomping != -1 {
        s.extend_from_slice(&leading_break);
    }
    if chomping == 1 {
        s.extend_from_slice(&trailing_breaks);
    }

    // Create a token.
    *token = YamlToken {
        typ: YamlTokenType::Scalar,
        start_mark,
        end_mark,
        value: s,
        style: YamlScalarStyle::Literal,
        ..YamlToken::default()
    };
    if !literal {
        token.style = YamlScalarStyle::Folded;
    }
    true
}

/// Scan indentation spaces and line breaks for a block scalar.  Determine the indentation level
/// if needed.
fn yaml_parser_scan_block_scalar_breaks(
    parser: &mut YamlParser<'_>,
    indent: &mut isize,
    breaks: &mut Vec<u8>,
    start_mark: YamlMark,
    end_mark: &mut YamlMark,
) -> bool {
    *end_mark = parser.mark;

    // Eat the indentation spaces and line breaks.
    let mut max_indent: isize = 0;
    loop {
        // Eat the indentation spaces.
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }
        while (*indent == 0 || (parser.mark.column as isize) < *indent)
            && is_space(&parser.buffer, parser.buffer_pos)
        {
            skip(parser);
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }
        if parser.mark.column as isize > max_indent {
            max_indent = parser.mark.column as isize;
        }

        // Check for a tab character messing the indentation.
        if (*indent == 0 || (parser.mark.column as isize) < *indent)
            && is_tab(&parser.buffer, parser.buffer_pos)
        {
            return yaml_parser_set_scanner_error(
                parser,
                "while scanning a block scalar",
                start_mark,
                "found a tab character where an indentation space is expected",
            );
        }

        // Have we found a non-empty line?
        if !is_break(&parser.buffer, parser.buffer_pos) {
            break;
        }

        // Consume the line break.
        if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
            return false;
        }
        // TODO: should really be returning breaks instead.
        read_line(parser, breaks);
        *end_mark = parser.mark;
    }

    // Determine the indentation level if needed.
    if *indent == 0 {
        *indent = max_indent;
        if *indent < parser.indent + 1 {
            *indent = parser.indent + 1;
        }
        if *indent < 1 {
            *indent = 1;
        }
    }
    true
}

/// Scan a quoted scalar.
fn yaml_parser_scan_flow_scalar(
    parser: &mut YamlParser<'_>,
    token: &mut YamlToken,
    single: bool,
) -> bool {
    // Eat the left quote.
    let start_mark = parser.mark;
    skip(parser);

    // Consume the content of the quoted scalar.
    let mut s = Vec::new();
    let mut leading_break = Vec::new();
    let mut trailing_breaks = Vec::new();
    let mut whitespaces = Vec::new();
    loop {
        // Check that there are no document indicators at the beginning of the line.
        if parser.unread < 4 && !yaml_parser_update_buffer(parser, 4) {
            return false;
        }

        if parser.mark.column == 0
            && ((parser.buffer[parser.buffer_pos] == b'-'
                && parser.buffer[parser.buffer_pos + 1] == b'-'
                && parser.buffer[parser.buffer_pos + 2] == b'-')
                || (parser.buffer[parser.buffer_pos] == b'.'
                    && parser.buffer[parser.buffer_pos + 1] == b'.'
                    && parser.buffer[parser.buffer_pos + 2] == b'.'))
            && is_blankz(&parser.buffer, parser.buffer_pos + 3)
        {
            yaml_parser_set_scanner_error(
                parser,
                "while scanning a quoted scalar",
                start_mark,
                "found unexpected document indicator",
            );
            return false;
        }

        // Check for EOF.
        if is_z(&parser.buffer, parser.buffer_pos) {
            yaml_parser_set_scanner_error(
                parser,
                "while scanning a quoted scalar",
                start_mark,
                "found unexpected end of stream",
            );
            return false;
        }

        // Consume non-blank characters.
        let mut leading_blanks = false;
        while !is_blankz(&parser.buffer, parser.buffer_pos) {
            if single
                && parser.buffer[parser.buffer_pos] == b'\''
                && parser.buffer[parser.buffer_pos + 1] == b'\''
            {
                // Is is an escaped single quote.
                s.push(b'\'');
                skip(parser);
                skip(parser);
            } else if single && parser.buffer[parser.buffer_pos] == b'\'' {
                // It is a right single quote.
                break;
            } else if !single && parser.buffer[parser.buffer_pos] == b'"' {
                // It is a right double quote.
                break;
            } else if !single
                && parser.buffer[parser.buffer_pos] == b'\\'
                && is_break(&parser.buffer, parser.buffer_pos + 1)
            {
                // It is an escaped line break.
                if parser.unread < 3 && !yaml_parser_update_buffer(parser, 3) {
                    return false;
                }
                skip(parser);
                skip_line(parser);
                leading_blanks = true;
                break;
            } else if !single && parser.buffer[parser.buffer_pos] == b'\\' {
                // It is an escape sequence.
                let mut code_length: usize = 0;

                // Check the escape character.
                match parser.buffer[parser.buffer_pos + 1] {
                    b'0' => s.push(0),
                    b'a' => s.push(b'\x07'),
                    b'b' => s.push(b'\x08'),
                    b't' | b'\t' => s.push(b'\x09'),
                    b'n' => s.push(b'\x0A'),
                    b'v' => s.push(b'\x0B'),
                    b'f' => s.push(b'\x0C'),
                    b'r' => s.push(b'\x0D'),
                    b'e' => s.push(b'\x1B'),
                    b' ' => s.push(b'\x20'),
                    b'"' => s.push(b'"'),
                    b'\'' => s.push(b'\''),
                    b'\\' => s.push(b'\\'),
                    // NEL (#x85)
                    b'N' => s.extend_from_slice(b"\xC2\x85"),
                    // #xA0
                    b'_' => s.extend_from_slice(b"\xC2\xA0"),
                    // LS (#x2028)
                    b'L' => s.extend_from_slice(b"\xE2\x80\xA8"),
                    // PS (#x2029)
                    b'P' => s.extend_from_slice(b"\xE2\x80\xA9"),
                    b'x' => code_length = 2,
                    b'u' => code_length = 4,
                    b'U' => code_length = 8,
                    _ => {
                        yaml_parser_set_scanner_error(
                            parser,
                            "while parsing a quoted scalar",
                            start_mark,
                            "found unknown escape character",
                        );
                        return false;
                    }
                }

                skip(parser);
                skip(parser);

                // Consume an arbitrary escape code.
                if code_length > 0 {
                    let mut value: isize = 0;

                    // Scan the character value.
                    if parser.unread < code_length
                        && !yaml_parser_update_buffer(parser, code_length)
                    {
                        return false;
                    }
                    for k in 0..code_length {
                        if !is_hex(&parser.buffer, parser.buffer_pos + k) {
                            yaml_parser_set_scanner_error(
                                parser,
                                "while parsing a quoted scalar",
                                start_mark,
                                "did not find expected hexdecimal number",
                            );
                            return false;
                        }
                        value = (value << 4) + as_hex(&parser.buffer, parser.buffer_pos + k);
                    }

                    // Check the value and write the character.
                    if (0xD800..=0xDFFF).contains(&value) || value > 0x10FFFF {
                        yaml_parser_set_scanner_error(
                            parser,
                            "while parsing a quoted scalar",
                            start_mark,
                            "found invalid Unicode character escape code",
                        );
                        return false;
                    }
                    if value <= 0x7F {
                        s.push(value as u8);
                    } else if value <= 0x7FF {
                        s.push((0xC0 + (value >> 6)) as u8);
                        s.push((0x80 + (value & 0x3F)) as u8);
                    } else if value <= 0xFFFF {
                        s.push((0xE0 + (value >> 12)) as u8);
                        s.push((0x80 + ((value >> 6) & 0x3F)) as u8);
                        s.push((0x80 + (value & 0x3F)) as u8);
                    } else {
                        s.push((0xF0 + (value >> 18)) as u8);
                        s.push((0x80 + ((value >> 12) & 0x3F)) as u8);
                        s.push((0x80 + ((value >> 6) & 0x3F)) as u8);
                        s.push((0x80 + (value & 0x3F)) as u8);
                    }

                    // Advance the pointer.
                    for _ in 0..code_length {
                        skip(parser);
                    }
                }
            } else {
                // It is a non-escaped non-blank character.
                read(parser, &mut s);
            }
            if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                return false;
            }
        }

        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }

        // Check if we are at the end of the scalar.
        if single {
            if parser.buffer[parser.buffer_pos] == b'\'' {
                break;
            }
        } else if parser.buffer[parser.buffer_pos] == b'"' {
            break;
        }

        // Consume blank characters.
        while is_blank(&parser.buffer, parser.buffer_pos)
            || is_break(&parser.buffer, parser.buffer_pos)
        {
            if is_blank(&parser.buffer, parser.buffer_pos) {
                // Consume a space or a tab character.
                if !leading_blanks {
                    read(parser, &mut whitespaces);
                } else {
                    skip(parser);
                }
            } else {
                if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                    return false;
                }

                // Check if it is a first line break.
                if !leading_blanks {
                    whitespaces.clear();
                    read_line(parser, &mut leading_break);
                    leading_blanks = true;
                } else {
                    read_line(parser, &mut trailing_breaks);
                }
            }
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }

        // Join the whitespaces or fold line breaks.
        if leading_blanks {
            // Do we need to fold line breaks?
            if !leading_break.is_empty() && leading_break[0] == b'\n' {
                if trailing_breaks.is_empty() {
                    s.push(b' ');
                } else {
                    s.extend_from_slice(&trailing_breaks);
                }
            } else {
                s.extend_from_slice(&leading_break);
                s.extend_from_slice(&trailing_breaks);
            }
            trailing_breaks.clear();
            leading_break.clear();
        } else {
            s.extend_from_slice(&whitespaces);
            whitespaces.clear();
        }
    }

    // Eat the right quote.
    skip(parser);
    let end_mark = parser.mark;

    // Create a token.
    *token = YamlToken {
        typ: YamlTokenType::Scalar,
        start_mark,
        end_mark,
        value: s,
        style: YamlScalarStyle::SingleQuoted,
        ..YamlToken::default()
    };
    if !single {
        token.style = YamlScalarStyle::DoubleQuoted;
    }
    true
}

/// Scan a plain scalar.
fn yaml_parser_scan_plain_scalar(parser: &mut YamlParser<'_>, token: &mut YamlToken) -> bool {
    let mut s = Vec::new();
    let mut leading_break = Vec::new();
    let mut trailing_breaks = Vec::new();
    let mut whitespaces = Vec::new();
    let mut leading_blanks = false;
    let indent = parser.indent + 1;

    let start_mark = parser.mark;
    let mut end_mark = parser.mark;

    // Consume the content of the plain scalar.
    loop {
        // Check for a document indicator.
        if parser.unread < 4 && !yaml_parser_update_buffer(parser, 4) {
            return false;
        }
        if parser.mark.column == 0
            && ((parser.buffer[parser.buffer_pos] == b'-'
                && parser.buffer[parser.buffer_pos + 1] == b'-'
                && parser.buffer[parser.buffer_pos + 2] == b'-')
                || (parser.buffer[parser.buffer_pos] == b'.'
                    && parser.buffer[parser.buffer_pos + 1] == b'.'
                    && parser.buffer[parser.buffer_pos + 2] == b'.'))
            && is_blankz(&parser.buffer, parser.buffer_pos + 3)
        {
            break;
        }

        // Check for a comment.
        if parser.buffer[parser.buffer_pos] == b'#' {
            break;
        }

        // Consume non-blank characters.
        while !is_blankz(&parser.buffer, parser.buffer_pos) {
            // Check for indicators that may end a plain scalar.
            let c = parser.buffer[parser.buffer_pos];
            if (c == b':' && is_blankz(&parser.buffer, parser.buffer_pos + 1))
                || (parser.flow_level > 0
                    && (c == b',' || c == b'?' || c == b'[' || c == b']' || c == b'{' || c == b'}'))
            {
                break;
            }

            // Check if we need to join whitespaces and breaks.
            if leading_blanks || !whitespaces.is_empty() {
                if leading_blanks {
                    // Do we need to fold line breaks?
                    if leading_break[0] == b'\n' {
                        if trailing_breaks.is_empty() {
                            s.push(b' ');
                        } else {
                            s.extend_from_slice(&trailing_breaks);
                        }
                    } else {
                        s.extend_from_slice(&leading_break);
                        s.extend_from_slice(&trailing_breaks);
                    }
                    trailing_breaks.clear();
                    leading_break.clear();
                    leading_blanks = false;
                } else {
                    s.extend_from_slice(&whitespaces);
                    whitespaces.clear();
                }
            }

            // Copy the character.
            read(parser, &mut s);

            end_mark = parser.mark;
            if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                return false;
            }
        }

        // Is it the end?
        if !(is_blank(&parser.buffer, parser.buffer_pos)
            || is_break(&parser.buffer, parser.buffer_pos))
        {
            break;
        }

        // Consume blank characters.
        if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
            return false;
        }

        while is_blank(&parser.buffer, parser.buffer_pos)
            || is_break(&parser.buffer, parser.buffer_pos)
        {
            if is_blank(&parser.buffer, parser.buffer_pos) {
                // Check for tab characters that abuse indentation.
                if leading_blanks
                    && (parser.mark.column as isize) < indent
                    && is_tab(&parser.buffer, parser.buffer_pos)
                {
                    yaml_parser_set_scanner_error(
                        parser,
                        "while scanning a plain scalar",
                        start_mark,
                        "found a tab character that violates indentation",
                    );
                    return false;
                }

                // Consume a space or a tab character.
                if !leading_blanks {
                    read(parser, &mut whitespaces);
                } else {
                    skip(parser);
                }
            } else {
                if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                    return false;
                }

                // Check if it is a first line break.
                if !leading_blanks {
                    whitespaces.clear();
                    read_line(parser, &mut leading_break);
                    leading_blanks = true;
                } else {
                    read_line(parser, &mut trailing_breaks);
                }
            }
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
        }

        // Check indentation level.
        if parser.flow_level == 0 && (parser.mark.column as isize) < indent {
            break;
        }
    }

    // Create a token.
    *token = YamlToken {
        typ: YamlTokenType::Scalar,
        start_mark,
        end_mark,
        value: s,
        style: YamlScalarStyle::Plain,
        ..YamlToken::default()
    };

    // Note that we change the 'simple_key_allowed' flag.
    if leading_blanks {
        parser.simple_key_allowed = true;
    }
    true
}

fn yaml_parser_scan_line_comment(parser: &mut YamlParser<'_>, token_mark: YamlMark) -> bool {
    if parser.newlines > 0 {
        return true;
    }

    let mut start_mark = YamlMark::default();
    let mut text = Vec::new();

    for peek in 0..512 {
        if parser.unread < peek + 1 && !yaml_parser_update_buffer(parser, peek + 1) {
            break;
        }
        if is_blank(&parser.buffer, parser.buffer_pos + peek) {
            continue;
        }
        if parser.buffer[parser.buffer_pos + peek] == b'#' {
            let seen = parser.mark.index + peek;
            loop {
                if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                    return false;
                }
                if is_breakz(&parser.buffer, parser.buffer_pos) {
                    if parser.mark.index >= seen {
                        break;
                    }
                    if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                        return false;
                    }
                    skip_line(parser);
                } else if parser.mark.index >= seen {
                    if text.is_empty() {
                        start_mark = parser.mark;
                    }
                    read(parser, &mut text);
                } else {
                    skip(parser);
                }
            }
        }
        break;
    }
    if !text.is_empty() {
        parser.comments.push(YamlComment {
            token_mark,
            start_mark,
            line: text,
            ..YamlComment::default()
        });
    }
    true
}

fn yaml_parser_scan_comments(parser: &mut YamlParser<'_>, scan_mark: YamlMark) -> bool {
    let mut scan_mark = scan_mark;
    let mut token = &parser.tokens[parser.tokens.len() - 1];

    if token.typ == YamlTokenType::FlowEntry && parser.tokens.len() > 1 {
        token = &parser.tokens[parser.tokens.len() - 2];
    }
    let token_typ = token.typ;

    let mut token_mark = token.start_mark;
    let mut start_mark = YamlMark::default();
    let mut next_indent = parser.indent;
    if next_indent < 0 {
        next_indent = 0;
    }

    let mut recent_empty = false;
    let mut first_empty = parser.newlines <= 1;

    let mut line = parser.mark.line;
    let mut column = parser.mark.column;

    let mut text: Vec<u8> = Vec::new();

    // The foot line is the place where a comment must start to
    // still be considered as a foot of the prior content.
    // If there's some content in the currently parsed line, then
    // the foot is the line below it.
    let mut foot_line: isize = -1;
    if scan_mark.line > 0 {
        foot_line = parser.mark.line as isize - parser.newlines as isize + 1;
        if parser.newlines == 0 && parser.mark.column > 1 {
            foot_line += 1;
        }
    }

    // Every `continue` below advances `peek` by one first.
    let mut peek: usize = 0;
    while peek < 512 {
        if parser.unread < peek + 1 && !yaml_parser_update_buffer(parser, peek + 1) {
            break;
        }
        column += 1;
        if is_blank(&parser.buffer, parser.buffer_pos + peek) {
            peek += 1;
            continue;
        }
        let c = parser.buffer[parser.buffer_pos + peek];
        let close_flow = parser.flow_level > 0 && (c == b']' || c == b'}');
        if close_flow || is_breakz(&parser.buffer, parser.buffer_pos + peek) {
            // Got line break or terminator.
            if close_flow || !recent_empty {
                if close_flow
                    || (first_empty
                        && ((start_mark.line as isize == foot_line
                            && token_typ != YamlTokenType::Value)
                            || (start_mark.column as isize - 1) < next_indent))
                {
                    // This is the first empty line and there were no empty lines before,
                    // so this initial part of the comment is a foot of the prior token
                    // instead of being a head for the following one. Split it up.
                    // Alternatively, this might also be the last comment inside a flow
                    // scope, so it must be a footer.
                    if !text.is_empty() {
                        if (start_mark.column as isize - 1) < next_indent {
                            // If dedented it's unrelated to the prior token.
                            token_mark = start_mark;
                        }
                        parser.comments.push(YamlComment {
                            scan_mark,
                            token_mark,
                            start_mark,
                            end_mark: YamlMark {
                                index: parser.mark.index + peek,
                                line,
                                column,
                            },
                            foot: std::mem::take(&mut text),
                            ..YamlComment::default()
                        });
                        scan_mark = YamlMark {
                            index: parser.mark.index + peek,
                            line,
                            column,
                        };
                        token_mark = scan_mark;
                    }
                } else if !text.is_empty() && parser.buffer[parser.buffer_pos + peek] != 0 {
                    text.push(b'\n');
                }
            }
            if !is_break(&parser.buffer, parser.buffer_pos + peek) {
                break;
            }
            first_empty = false;
            recent_empty = true;
            column = 0;
            line += 1;
            peek += 1;
            continue;
        }

        if !text.is_empty()
            && (close_flow || ((column as isize - 1) < next_indent && column != start_mark.column))
        {
            // The comment at the different indentation is a foot of the
            // preceding data rather than a head of the upcoming one.
            parser.comments.push(YamlComment {
                scan_mark,
                token_mark,
                start_mark,
                end_mark: YamlMark {
                    index: parser.mark.index + peek,
                    line,
                    column,
                },
                foot: std::mem::take(&mut text),
                ..YamlComment::default()
            });
            scan_mark = YamlMark {
                index: parser.mark.index + peek,
                line,
                column,
            };
            token_mark = scan_mark;
        }

        if parser.buffer[parser.buffer_pos + peek] != b'#' {
            break;
        }

        if text.is_empty() {
            start_mark = YamlMark {
                index: parser.mark.index + peek,
                line,
                column,
            };
        } else {
            text.push(b'\n');
        }

        recent_empty = false;

        // Consume until after the consumed comment line.
        let seen = parser.mark.index + peek;
        loop {
            if parser.unread < 1 && !yaml_parser_update_buffer(parser, 1) {
                return false;
            }
            if is_breakz(&parser.buffer, parser.buffer_pos) {
                if parser.mark.index >= seen {
                    break;
                }
                if parser.unread < 2 && !yaml_parser_update_buffer(parser, 2) {
                    return false;
                }
                skip_line(parser);
            } else if parser.mark.index >= seen {
                read(parser, &mut text);
            } else {
                skip(parser);
            }
        }

        peek = 0;
        column = 0;
        line = parser.mark.line;
        next_indent = parser.indent;
        if next_indent < 0 {
            next_indent = 0;
        }
        peek += 1;
    }

    if !text.is_empty() {
        parser.comments.push(YamlComment {
            scan_mark,
            token_mark: start_mark,
            start_mark,
            end_mark: YamlMark {
                index: parser.mark.index + peek - 1,
                line,
                column,
            },
            head: text,
            ..YamlComment::default()
        });
    }
    true
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::yaml_parser_scan;
    use crate::reader::tests::{reader_parser, string_parser};
    use crate::yamlh::{
        YamlComment, YamlEncoding, YamlErrorType, YamlMark, YamlParser, YamlToken, YamlTokenType,
    };

    /// Scan until STREAM-END or the first failure; returns the tokens and whether the scan
    /// reached STREAM-END. Like decode.go's `peek` (yaml.v3 issue #666), a scan that returns
    /// true with `parser.error` set is a failure.
    fn scan_tokens(parser: &mut YamlParser<'_>) -> (Vec<YamlToken>, bool) {
        let mut tokens = Vec::new();
        loop {
            let mut token = YamlToken::default();
            if !yaml_parser_scan(parser, &mut token) || parser.error != YamlErrorType::No {
                return (tokens, false);
            }
            let end = token.typ == YamlTokenType::StreamEnd;
            tokens.push(token);
            if end {
                return (tokens, true);
            }
        }
    }

    fn lossy(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    fn mark(m: YamlMark) -> String {
        format!("{}:{}", m.line, m.column)
    }

    /// `NAME(payload) line:col-line:col`, NAME being Go's token name without `yaml_`/`_TOKEN`.
    fn render(token: &YamlToken) -> String {
        let name = token.typ.to_string();
        let name = name.trim_start_matches("yaml_").trim_end_matches("_TOKEN");
        let head = match token.typ {
            YamlTokenType::StreamStart => format!("{name}({:?})", token.encoding),
            YamlTokenType::VersionDirective => {
                format!("{name}({}.{})", token.major, token.minor)
            }
            YamlTokenType::TagDirective => {
                format!(
                    "{name}({:?}, {:?})",
                    lossy(&token.value),
                    lossy(&token.prefix)
                )
            }
            YamlTokenType::Alias | YamlTokenType::Anchor => {
                format!("{name}({:?})", lossy(&token.value))
            }
            YamlTokenType::Tag => {
                format!(
                    "{name}({:?}, {:?})",
                    lossy(&token.value),
                    lossy(&token.suffix)
                )
            }
            YamlTokenType::Scalar => {
                format!("{name}({:?}, {:?})", token.style, lossy(&token.value))
            }
            _ => name.to_string(),
        };
        format!("{head} {}-{}", mark(token.start_mark), mark(token.end_mark))
    }

    fn scan_rendered(input: &[u8]) -> Vec<String> {
        let mut parser = string_parser(input);
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(ok, "scan failed: {}", parser.problem);
        tokens.iter().map(render).collect()
    }

    fn full_mark(m: YamlMark) -> String {
        format!("{},{},{}", m.index, m.line, m.column)
    }

    /// `kind(text) scan= token= start= end=` with marks as `index,line,column`.
    fn render_comment(c: &YamlComment) -> String {
        let (kind, text) = if !c.head.is_empty() {
            ("head", &c.head)
        } else if !c.line.is_empty() {
            ("line", &c.line)
        } else {
            ("foot", &c.foot)
        };
        format!(
            "{kind}({:?}) scan={} token={} start={} end={}",
            lossy(text),
            full_mark(c.scan_mark),
            full_mark(c.token_mark),
            full_mark(c.start_mark),
            full_mark(c.end_mark)
        )
    }

    /// The scanner/reader error state after scanning `input` to the first failure:
    /// `type|context|context_mark|problem|problem_mark`.
    fn scan_error(input: &[u8]) -> String {
        let mut parser = string_parser(input);
        let (_, ok) = scan_tokens(&mut parser);
        assert!(!ok, "scan of {:?} succeeded", lossy(input));
        format!(
            "{:?}|{}|{}|{}|{}",
            parser.error,
            parser.context,
            full_mark(parser.context_mark),
            parser.problem,
            full_mark(parser.problem_mark)
        )
    }

    /// decode.go `(*parser).fail`'s message (the composer's is `node::Parser::fail`).
    fn decode_fail_text(parser: &YamlParser<'_>) -> String {
        let mut line = 0;
        if parser.context_mark.line != 0 {
            line = parser.context_mark.line;
            // Scanner errors don't iterate line before returning error
            if parser.error == YamlErrorType::Scanner {
                line += 1;
            }
        } else if parser.problem_mark.line != 0 {
            line = parser.problem_mark.line;
            // Scanner errors don't iterate line before returning error
            if parser.error == YamlErrorType::Scanner {
                line += 1;
            }
        }
        let place = if line != 0 {
            format!("line {line}: ")
        } else {
            String::new()
        };
        let msg = if parser.problem.is_empty() {
            "unknown problem parsing YAML content"
        } else {
            parser.problem.as_str()
        };
        format!("yaml: {place}{msg}")
    }

    #[test]
    fn block_mapping_with_nested_sequence() {
        assert_eq!(
            scan_rendered(b"a: 1\nb:\n  - x\n  - y\nc: z\n"),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "BLOCK_MAPPING_START 0:0-0:0",
                "KEY 0:0-0:0",
                "SCALAR(Plain, \"a\") 0:0-0:1",
                "VALUE 0:1-0:2",
                "SCALAR(Plain, \"1\") 0:3-0:4",
                "KEY 1:0-1:0",
                "SCALAR(Plain, \"b\") 1:0-1:1",
                "VALUE 1:1-1:2",
                "BLOCK_SEQUENCE_START 2:2-2:2",
                "BLOCK_ENTRY 2:2-2:3",
                "SCALAR(Plain, \"x\") 2:4-2:5",
                "BLOCK_ENTRY 3:2-3:3",
                "SCALAR(Plain, \"y\") 3:4-3:5",
                "BLOCK_END 4:0-4:0",
                "KEY 4:0-4:0",
                "SCALAR(Plain, \"c\") 4:0-4:1",
                "VALUE 4:1-4:2",
                "SCALAR(Plain, \"z\") 4:3-4:4",
                "BLOCK_END 5:0-5:0",
                "STREAM_END 5:0-5:0",
            ]
        );
    }

    #[test]
    fn indentless_sequence_has_no_block_sequence_start() {
        // scannerc.go's own example: BLOCK-SEQUENCE-START is NOT produced.
        assert_eq!(
            scan_rendered(b"key:\n- item 1\n- item 2\n"),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "BLOCK_MAPPING_START 0:0-0:0",
                "KEY 0:0-0:0",
                "SCALAR(Plain, \"key\") 0:0-0:3",
                "VALUE 0:3-0:4",
                "BLOCK_ENTRY 1:0-1:1",
                "SCALAR(Plain, \"item 1\") 1:2-1:8",
                "BLOCK_ENTRY 2:0-2:1",
                "SCALAR(Plain, \"item 2\") 2:2-2:8",
                "BLOCK_END 3:0-3:0",
                "STREAM_END 3:0-3:0",
            ]
        );
    }

    #[test]
    fn flow_collections_and_complex_keys() {
        assert_eq!(
            scan_rendered(b"{a: [b, 'c'], ? d : e, f}\n"),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "FLOW_MAPPING_START 0:0-0:1",
                "KEY 0:1-0:1",
                "SCALAR(Plain, \"a\") 0:1-0:2",
                "VALUE 0:2-0:3",
                "FLOW_SEQUENCE_START 0:4-0:5",
                "SCALAR(Plain, \"b\") 0:5-0:6",
                "FLOW_ENTRY 0:6-0:7",
                "SCALAR(SingleQuoted, \"c\") 0:8-0:11",
                "FLOW_SEQUENCE_END 0:11-0:12",
                "FLOW_ENTRY 0:12-0:13",
                "KEY 0:14-0:15",
                "SCALAR(Plain, \"d\") 0:16-0:17",
                "VALUE 0:18-0:19",
                "SCALAR(Plain, \"e\") 0:20-0:21",
                "FLOW_ENTRY 0:21-0:22",
                "SCALAR(Plain, \"f\") 0:23-0:24",
                "FLOW_MAPPING_END 0:24-0:25",
                "STREAM_END 1:0-1:0",
            ]
        );
    }

    #[test]
    fn every_scalar_style() {
        let input = b"- plain\n  folded\n\n  text\n- 'it''s\n  here'\n- \"t\\tx\\u00e9\\\n  y\"\n- |+\n  lit\n\n- >2-\n   more\n  less\n";
        assert_eq!(
            scan_rendered(input),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "BLOCK_SEQUENCE_START 0:0-0:0",
                "BLOCK_ENTRY 0:0-0:1",
                "SCALAR(Plain, \"plain folded\\ntext\") 0:2-3:6",
                "BLOCK_ENTRY 4:0-4:1",
                "SCALAR(SingleQuoted, \"it's here\") 4:2-5:7",
                "BLOCK_ENTRY 6:0-6:1",
                "SCALAR(DoubleQuoted, \"t\\txéy\") 6:2-7:4",
                "BLOCK_ENTRY 8:0-8:1",
                "SCALAR(Literal, \"lit\\n\\n\") 8:2-11:0",
                "BLOCK_ENTRY 11:0-11:1",
                "SCALAR(Folded, \" more\\nless\") 11:2-14:0",
                "BLOCK_END 14:0-14:0",
                "STREAM_END 14:0-14:0",
            ]
        );
    }

    #[test]
    fn double_quoted_escapes() {
        let mut parser =
            string_parser(b"\"\\0\\a\\b\\t\\\t\\n\\v\\f\\r\\e\\ \\\"\\'\\\\\\N\\_\\L\\P\\x41\\u00e9\\U0001D11E\"\n");
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(ok);
        assert_eq!(
            tokens[1].value,
            b"\x00\x07\x08\t\t\n\x0b\x0c\r\x1b \"'\\\xc2\x85\xc2\xa0\xe2\x80\xa8\xe2\x80\xa9A\xc3\xa9\xf0\x9d\x84\x9e"
        );
    }

    #[test]
    fn directives_tags_anchors_and_aliases() {
        assert_eq!(
            scan_rendered(b"%YAML 1.1\n%TAG !e! tag:e.com,2000:\n--- !e!foo &a x\n...\n--- *a\n"),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "VERSION_DIRECTIVE(1.1) 0:0-0:9",
                "TAG_DIRECTIVE(\"!e!\", \"tag:e.com,2000:\") 1:0-1:24",
                "DOCUMENT_START 2:0-2:3",
                "TAG(\"!e!\", \"foo\") 2:4-2:10",
                "ANCHOR(\"a\") 2:11-2:13",
                "SCALAR(Plain, \"x\") 2:14-2:15",
                "DOCUMENT_END 3:0-3:3",
                "DOCUMENT_START 4:0-4:3",
                "ALIAS(\"a\") 4:4-4:6",
                "STREAM_END 5:0-5:0",
            ]
        );
    }

    #[test]
    fn tag_forms() {
        assert_eq!(
            scan_rendered(
                b"- !<tag:yaml.org,2002:str> v\n- ! w\n- !!int 3\n- !local x\n- !e%21x y\n"
            ),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "BLOCK_SEQUENCE_START 0:0-0:0",
                "BLOCK_ENTRY 0:0-0:1",
                "TAG(\"\", \"tag:yaml.org,2002:str\") 0:2-0:26",
                "SCALAR(Plain, \"v\") 0:27-0:28",
                "BLOCK_ENTRY 1:0-1:1",
                "TAG(\"\", \"!\") 1:2-1:3",
                "SCALAR(Plain, \"w\") 1:4-1:5",
                "BLOCK_ENTRY 2:0-2:1",
                "TAG(\"!!\", \"int\") 2:2-2:7",
                "SCALAR(Plain, \"3\") 2:8-2:9",
                "BLOCK_ENTRY 3:0-3:1",
                "TAG(\"!\", \"local\") 3:2-3:8",
                "SCALAR(Plain, \"x\") 3:9-3:10",
                "BLOCK_ENTRY 4:0-4:1",
                "TAG(\"!\", \"e!x\") 4:2-4:8",
                "SCALAR(Plain, \"y\") 4:9-4:10",
                "BLOCK_END 5:0-5:0",
                "STREAM_END 5:0-5:0",
            ]
        );
    }

    #[test]
    fn head_line_and_foot_comments() {
        let mut parser = string_parser(b"# head\na: 1 # line\n# foot\n\n# next\nb: 2\n");
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(ok);
        assert_eq!(tokens.len(), 12);
        let comments: Vec<String> = parser.comments.iter().map(render_comment).collect();
        assert_eq!(
            comments,
            [
                "head(\"# head\") scan=0,0,0 token=0,0,1 start=0,0,1 end=6,0,1",
                "line(\"# line\") scan=0,0,0 token=10,1,3 start=12,1,5 end=0,0,0",
                "foot(\"# foot\") scan=18,1,11 token=10,1,3 start=19,2,1 end=26,2,1",
                "head(\"# next\") scan=26,2,1 token=27,3,1 start=27,3,1 end=33,4,1",
            ]
        );
    }

    #[test]
    fn line_comment_after_sequence_indicator_becomes_head() {
        // scan_to_next_token's "- # The comment" repositioning.
        let mut parser = string_parser(b"- # The comment\n  - Some data\n");
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(ok);
        assert_eq!(
            tokens.iter().map(render).collect::<Vec<_>>(),
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "BLOCK_SEQUENCE_START 0:0-0:0",
                "BLOCK_ENTRY 0:0-0:1",
                "BLOCK_SEQUENCE_START 1:2-1:2",
                "BLOCK_ENTRY 1:2-1:3",
                "SCALAR(Plain, \"Some data\") 1:4-1:13",
                "BLOCK_END 2:0-2:0",
                "BLOCK_END 2:0-2:0",
                "STREAM_END 2:0-2:0",
            ]
        );
        let comments: Vec<String> = parser.comments.iter().map(render_comment).collect();
        assert_eq!(
            comments,
            ["head(\"# The comment\") scan=1,0,1 token=2,0,3 start=2,0,3 end=17,0,3"]
        );
    }

    #[test]
    fn flow_comments_and_closing_foot() {
        let mut parser = string_parser(b"[a, # c1\n b # c2\n # c3\n]\n");
        let (_, ok) = scan_tokens(&mut parser);
        assert!(ok);
        let comments: Vec<String> = parser.comments.iter().map(render_comment).collect();
        assert_eq!(
            comments,
            [
                "line(\"# c1\") scan=0,0,0 token=1,0,1 start=4,0,4 end=0,0,0",
                "line(\"# c2\") scan=0,0,0 token=10,1,1 start=12,1,3 end=0,0,0",
                "foot(\"# c3\") scan=16,1,7 token=10,1,1 start=18,2,2 end=23,2,1",
            ]
        );
    }

    #[test]
    fn utf16_and_bom_inputs_scan_like_utf8() {
        let text = "k: é𝄞\n";
        let utf8 = scan_rendered(text.as_bytes());
        assert_eq!(
            utf8,
            [
                "STREAM_START(Utf8) 0:0-0:0",
                "BLOCK_MAPPING_START 0:0-0:0",
                "KEY 0:0-0:0",
                "SCALAR(Plain, \"k\") 0:0-0:1",
                "VALUE 0:1-0:2",
                "SCALAR(Plain, \"é𝄞\") 0:3-0:5",
                "BLOCK_END 1:0-1:0",
                "STREAM_END 1:0-1:0",
            ]
        );
        let mut utf16le = b"\xff\xfe".to_vec();
        let mut utf16be = b"\xfe\xff".to_vec();
        for unit in text.encode_utf16() {
            utf16le.extend_from_slice(&unit.to_le_bytes());
            utf16be.extend_from_slice(&unit.to_be_bytes());
        }
        let mut utf8_bom = b"\xef\xbb\xbf".to_vec();
        utf8_bom.extend_from_slice(text.as_bytes());
        for (input, start) in [
            (utf16le, "STREAM_START(Utf16le) 0:0-0:0"),
            (utf16be, "STREAM_START(Utf16be) 0:0-0:0"),
            (utf8_bom, "STREAM_START(Utf8) 0:0-0:0"),
        ] {
            let got = scan_rendered(&input);
            assert_eq!(got[0], start);
            assert_eq!(got[1..], utf8[1..]);
        }
    }

    #[test]
    fn reader_input_scans_like_string_input() {
        let input = b"a: 1\nb:\n  - x\n  - y\nc: z\n";
        let mut parser = reader_parser(input, false);
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(ok);
        assert_eq!(
            tokens.iter().map(render).collect::<Vec<_>>(),
            scan_rendered(input)
        );
    }

    #[test]
    fn reader_failure_is_an_input_error() {
        let mut parser = reader_parser(b"a: 1\nb:\n  - x\n  - y\nc: z\n", true);
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(!ok);
        assert_eq!(tokens.len(), 16);
        assert_eq!(parser.error, YamlErrorType::Reader);
        assert_eq!(parser.problem, "input error: boom");
        assert_eq!(parser.problem_offset, 25);
        assert_eq!(parser.problem_value, -1);
    }

    #[test]
    fn swallowed_reader_error_keeps_scan_returning_true() {
        // yaml.v3 issue #666 (decode_test.go unmarshalErrorTests): the reader error happens
        // while scan_line_comment peeks ahead, which breaks out of its loop and returns true.
        let mut parser = string_parser(b"0: [:!00 \xef");
        let mut kinds = Vec::new();
        for _ in 0..8 {
            let mut token = YamlToken::default();
            assert!(yaml_parser_scan(&mut parser, &mut token));
            kinds.push(token.typ);
        }
        assert_eq!(
            kinds,
            [
                YamlTokenType::StreamStart,
                YamlTokenType::BlockMappingStart,
                YamlTokenType::Key,
                YamlTokenType::Scalar,
                YamlTokenType::Value,
                YamlTokenType::FlowSequenceStart,
                YamlTokenType::No,
                YamlTokenType::No,
            ]
        );
        assert_eq!(parser.error, YamlErrorType::Reader);
        assert_eq!(parser.problem, "incomplete UTF-8 octet sequence");
        assert_eq!(parser.problem_offset, 9);
        assert_eq!(parser.problem_value, -1);
    }

    #[test]
    fn stream_start_reports_the_encoding_and_is_produced_once() {
        let mut parser = string_parser(b"");
        let (tokens, ok) = scan_tokens(&mut parser);
        assert!(ok);
        assert_eq!(
            tokens.iter().map(render).collect::<Vec<_>>(),
            ["STREAM_START(Utf8) 0:0-0:0", "STREAM_END 0:0-0:0"]
        );
        assert_eq!(tokens[0].encoding, YamlEncoding::Utf8);
        // After STREAM-END the scanner keeps returning true with an empty token.
        let mut token = YamlToken::default();
        assert!(yaml_parser_scan(&mut parser, &mut token));
        assert_eq!(token.typ, YamlTokenType::No);
    }

    #[test]
    fn simple_key_limits() {
        // A simple key longer than 1024 characters is no key: its ':' is a complex value.
        let mut long_key = vec![b'a'; 1100];
        long_key.extend_from_slice(b": b\n");
        assert_eq!(
            scan_error(&long_key),
            "Scanner||1100,0,1100|mapping values are not allowed in this context|1100,0,1100"
        );
        // In a flow mapping the same ':' is a plain VALUE.
        let mut flow = b"{".to_vec();
        flow.extend_from_slice(&[b'a'; 1100]);
        flow.extend_from_slice(b": b}\n");
        let rendered = scan_rendered(&flow);
        assert_eq!(
            rendered[2],
            "SCALAR(Plain, \"".to_string() + &"a".repeat(1100) + "\") 0:1-0:1101"
        );
        assert_eq!(rendered[3], "VALUE 0:1101-0:1102");
    }

    #[test]
    fn flow_depth_limit() {
        let ok = [b"[".repeat(10000), b"]".repeat(10000)].concat();
        let mut parser = string_parser(&ok);
        assert!(scan_tokens(&mut parser).1);
        let deep = [b"[".repeat(10001), b"]".repeat(10001)].concat();
        assert_eq!(
            scan_error(&deep),
            "Scanner|while increasing flow level|10000,0,10000|exceeded max depth of 10000|10000,0,10000"
        );
    }

    #[test]
    fn scanner_error_texts() {
        // (input, Go's error state: type|context|context_mark|problem|problem_mark)
        let cases: &[(&[u8], &str)] = &[
            (
                b"*a$\n",
                "Scanner|while scanning an alias|0,0,0|did not find expected alphabetic or numeric character|2,0,2",
            ),
            (
                b"& a\n",
                "Scanner|while scanning an anchor|0,0,0|did not find expected alphabetic or numeric character|1,0,1",
            ),
            (
                b"a: |0\n  x\n",
                "Scanner|while scanning a block scalar|3,0,3|found an indentation indicator equal to 0|4,0,4",
            ),
            (
                b"a: |\n\tx\n",
                "Scanner|while scanning a block scalar|3,0,3|found a tab character where an indentation space is expected|5,1,0",
            ),
            (
                b"a: | x\n  y\n",
                "Scanner|while scanning a block scalar|3,0,3|did not find expected comment or line break|5,0,5",
            ),
            (
                b"@foo\n",
                "Scanner|while scanning for the next token|0,0,0|found character that cannot start any token|0,0,0",
            ),
            (
                b"%FOO bar\n---\na\n",
                "Scanner|while scanning a directive|0,0,0|found unknown directive name|4,0,4",
            ),
            (
                b"%YA$ML\n",
                "Scanner|while scanning a directive|0,0,0|found unexpected non-alphabetical character|3,0,3",
            ),
            (
                b"% x\n",
                "Scanner|while scanning a directive|0,0,0|could not find expected directive name|1,0,1",
            ),
            (
                b"\"\\q\"\n",
                "Scanner|while parsing a quoted scalar|0,0,0|found unknown escape character|1,0,1",
            ),
            (
                b"\"\\xZZ\"\n",
                "Scanner|while parsing a quoted scalar|0,0,0|did not find expected hexdecimal number|3,0,3",
            ),
            (
                b"\"\\ud800\"\n",
                "Scanner|while parsing a quoted scalar|0,0,0|found invalid Unicode character escape code|3,0,3",
            ),
            (
                b"\"\\U00110000\"\n",
                "Scanner|while parsing a quoted scalar|0,0,0|found invalid Unicode character escape code|3,0,3",
            ),
            (
                b"a: b: c\n",
                "Scanner||4,0,4|mapping values are not allowed in this context|4,0,4",
            ),
            (
                b"a: b\n\tc\n",
                "Scanner|while scanning a plain scalar|3,0,3|found a tab character that violates indentation|5,1,0",
            ),
            (
                b"\"a\n---\n\"\n",
                "Scanner|while scanning a quoted scalar|0,0,0|found unexpected document indicator|3,1,0",
            ),
            (
                b"\"abc",
                "Scanner|while scanning a quoted scalar|0,0,0|found unexpected end of stream|4,0,4",
            ),
            (
                b"%TAG !a x\n",
                "Scanner|while parsing a %TAG directive|0,0,0|did not find expected '!'|7,0,7",
            ),
            (
                b"%TAG !a!b\n",
                "Scanner|while scanning a %TAG directive|0,0,0|did not find expected whitespace|8,0,8",
            ),
            (
                b"%TAG !a! x y\n",
                "Scanner|while scanning a directive|0,0,0|did not find expected comment or line break|11,0,11",
            ),
            (
                b"!foo%ZZ a\n",
                "Scanner|while parsing a tag|0,0,0|did not find URI escaped octet|4,0,4",
            ),
            (
                b"!foo%80 a\n",
                "Scanner|while parsing a tag|0,0,0|found an incorrect leading UTF-8 octet|4,0,4",
            ),
            (
                b"!foo%C3%41 a\n",
                "Scanner|while parsing a tag|0,0,0|found an incorrect trailing UTF-8 octet|7,0,7",
            ),
            (
                b"!!\n",
                "Scanner|while parsing a tag|0,0,0|did not find expected tag URI|2,0,2",
            ),
            (
                b"!<tag:x a\n",
                "Scanner|while scanning a tag|0,0,0|did not find the expected '>'|7,0,7",
            ),
            (
                b"%YAML 123.1\n---\n",
                "Scanner|while scanning a %YAML directive|0,0,0|found extremely long version number|8,0,8",
            ),
            (
                b"%YAML 1,1\n---\n",
                "Scanner|while scanning a %YAML directive|0,0,0|did not find expected digit or '.' character|7,0,7",
            ),
            (
                b"%YAML .1\n---\n",
                "Scanner|while scanning a %YAML directive|0,0,0|did not find expected version number|6,0,6",
            ),
            (
                "a: x\u{85}y\u{2028}z\u{2029}w\r\nv\rq\n".as_bytes(),
                "Scanner|while scanning a simple key|5,1,0|could not find expected ':'|7,2,0",
            ),
            (
                b"a: \x01\n",
                "Reader||0,0,0|control characters are not allowed|0,0,0",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(scan_error(input), *want, "input {:?}", lossy(input));
        }
    }

    /// Every parse/diff golden whose input fails in the scanner (or reader) records exactly that
    /// error, formatted as decode.go's `fail` does (`tests/golden_parse.rs` checks the rest).
    #[test]
    fn golden_scanner_errors() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/golden");
        let mut checked = 0;
        for dir in ["parse", "diff"] {
            let mut paths: Vec<_> = std::fs::read_dir(root.join(dir))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            paths.sort();
            for path in paths {
                let golden: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                let input = base64::engine::general_purpose::STANDARD
                    .decode(golden["input_b64"].as_str().unwrap())
                    .unwrap();
                let mut parser = string_parser(&input);
                if scan_tokens(&mut parser).1 {
                    continue;
                }
                assert_eq!(
                    Some(decode_fail_text(&parser).as_str()),
                    golden["error"].as_str(),
                    "{}",
                    path.display()
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 48);
    }
}
