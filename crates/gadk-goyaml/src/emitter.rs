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

//! Port of yaml.v3 v3.0.1 `emitterc.go`: events to text (the emitter state machine,
//! indentation, line width, scalar analysis and style selection, comments).

use crate::api::yaml_event_delete;
use crate::writer::yaml_emitter_flush;
use crate::yamlh::{
    YamlBreak, YamlEmitter, YamlEmitterState, YamlEncoding, YamlErrorType, YamlEvent,
    YamlEventType, YamlMappingStyle, YamlScalarStyle, YamlSequenceStyle, YamlTagDirective,
    YamlVersionDirective, is_alpha, is_ascii, is_blank, is_blankz, is_bom, is_break, is_printable,
    is_space, width,
};

/// Go: `default_tag_directives` (parserc.go:1170) — the `!` and `!!` handles every document
/// knows. Go declares it in parserc.go and the emitter uses it too; Task 6's parser can share
/// this function.
pub(crate) fn default_tag_directives() -> [YamlTagDirective; 2] {
    // Go: parserc.go default_tag_directives
    [
        YamlTagDirective {
            handle: b"!".to_vec(),
            prefix: b"!".to_vec(),
        },
        YamlTagDirective {
            handle: b"!!".to_vec(),
            prefix: b"tag:yaml.org,2002:".to_vec(),
        },
    ]
}

/// Flush the buffer if needed.
fn flush(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go flush
    if emitter.buffer_pos + 5 >= emitter.buffer.len() {
        return yaml_emitter_flush(emitter);
    }
    true
}

/// Put a character to the output buffer.
fn put(emitter: &mut YamlEmitter<'_>, value: u8) -> bool {
    // Go: emitterc.go put
    if emitter.buffer_pos + 5 >= emitter.buffer.len() && !yaml_emitter_flush(emitter) {
        return false;
    }
    emitter.buffer[emitter.buffer_pos] = value;
    emitter.buffer_pos += 1;
    emitter.column += 1;
    true
}

/// Put a line break to the output buffer.
fn put_break(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go put_break
    if emitter.buffer_pos + 5 >= emitter.buffer.len() && !yaml_emitter_flush(emitter) {
        return false;
    }
    match emitter.line_break {
        YamlBreak::Cr => {
            emitter.buffer[emitter.buffer_pos] = b'\r';
            emitter.buffer_pos += 1;
        }
        YamlBreak::Ln => {
            emitter.buffer[emitter.buffer_pos] = b'\n';
            emitter.buffer_pos += 1;
        }
        YamlBreak::Crln => {
            emitter.buffer[emitter.buffer_pos] = b'\r';
            emitter.buffer[emitter.buffer_pos + 1] = b'\n';
            emitter.buffer_pos += 2;
        }
        YamlBreak::Any => panic!("unknown line break setting"),
    }
    if emitter.column == 0 {
        emitter.space_above = true;
    }
    emitter.column = 0;
    emitter.line += 1;
    // [Go] Do this here and below and drop from everywhere else (see commented lines).
    emitter.indention = true;
    true
}

/// Copy a character from a string into buffer.
fn write(emitter: &mut YamlEmitter<'_>, s: &[u8], i: &mut usize) -> bool {
    // Go: emitterc.go write
    if emitter.buffer_pos + 5 >= emitter.buffer.len() && !yaml_emitter_flush(emitter) {
        return false;
    }
    let p = emitter.buffer_pos;
    let w = width(s[*i]);
    match w {
        1..=4 => emitter.buffer[p..p + w].copy_from_slice(&s[*i..*i + w]),
        _ => panic!("unknown character width"),
    }
    emitter.column += 1;
    emitter.buffer_pos += w;
    *i += w;
    true
}

/// Write a whole string into buffer.
fn write_all(emitter: &mut YamlEmitter<'_>, s: &[u8]) -> bool {
    // Go: emitterc.go write_all
    let mut i = 0;
    while i < s.len() {
        if !write(emitter, s, &mut i) {
            return false;
        }
    }
    true
}

/// Copy a line break character from a string into buffer.
fn write_break(emitter: &mut YamlEmitter<'_>, s: &[u8], i: &mut usize) -> bool {
    // Go: emitterc.go write_break
    if s[*i] == b'\n' {
        if !put_break(emitter) {
            return false;
        }
        *i += 1;
    } else {
        if !write(emitter, s, i) {
            return false;
        }
        if emitter.column == 0 {
            emitter.space_above = true;
        }
        emitter.column = 0;
        emitter.line += 1;
        // [Go] Do this here and above and drop from everywhere else (see commented lines).
        emitter.indention = true;
    }
    true
}

/// Set an emitter error and return false.
fn yaml_emitter_set_emitter_error(emitter: &mut YamlEmitter<'_>, problem: &str) -> bool {
    // Go: emitterc.go yaml_emitter_set_emitter_error
    emitter.error = YamlErrorType::Emitter;
    emitter.problem = problem.to_owned();
    false
}

/// Emit an event. Go appends a copy of `*event` to the emitter's queue; the port may take the
/// event's contents (`std::mem::take`): every caller re-initializes `event` before the next
/// emit. Returns false on error (set on `emitter`).
#[allow(dead_code)] // Until Task 7 calls it; Task 12 removes this line.
pub(crate) fn yaml_emitter_emit(emitter: &mut YamlEmitter<'_>, event: &mut YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit
    emitter.events.push(std::mem::take(event));
    while !yaml_emitter_need_more_events(emitter) {
        // Go works on `&emitter.events[emitter.events_head]`. The port moves the event out of
        // the queue for the analysis and the state machine, leaving its type behind: the type is
        // the only field read through the queue while an event is processed
        // (yaml_emitter_need_more_events, yaml_emitter_check_empty_sequence/_mapping,
        // yaml_emitter_check_simple_key).
        let head = emitter.events_head;
        let typ = emitter.events[head].typ;
        let mut event = std::mem::replace(
            &mut emitter.events[head],
            YamlEvent {
                typ,
                ..YamlEvent::default()
            },
        );
        if !yaml_emitter_analyze_event(emitter, &mut event) {
            emitter.events[head] = event;
            return false;
        }
        if !yaml_emitter_state_machine(emitter, &event) {
            emitter.events[head] = event;
            return false;
        }
        yaml_event_delete(&mut emitter.events[head]);
        emitter.events_head += 1;
    }
    // Go never shrinks the queue (processed events stay as zero values); dropping them once
    // all are processed changes nothing observable, as every queue read is relative to
    // events_head.
    if emitter.events_head == emitter.events.len() {
        emitter.events.clear();
        emitter.events_head = 0;
    }
    true
}

/// Check if we need to accumulate more events before emitting.
///
/// We accumulate extra
///  - 1 event for DOCUMENT-START
///  - 2 events for SEQUENCE-START
///  - 3 events for MAPPING-START
fn yaml_emitter_need_more_events(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_need_more_events
    if emitter.events_head == emitter.events.len() {
        return true;
    }
    let accumulate = match emitter.events[emitter.events_head].typ {
        YamlEventType::DocumentStart => 1,
        YamlEventType::SequenceStart => 2,
        YamlEventType::MappingStart => 3,
        _ => return false,
    };
    if emitter.events.len() - emitter.events_head > accumulate {
        return false;
    }
    let mut level: isize = 0;
    for i in emitter.events_head..emitter.events.len() {
        match emitter.events[i].typ {
            YamlEventType::StreamStart
            | YamlEventType::DocumentStart
            | YamlEventType::SequenceStart
            | YamlEventType::MappingStart => level += 1,
            YamlEventType::StreamEnd
            | YamlEventType::DocumentEnd
            | YamlEventType::SequenceEnd
            | YamlEventType::MappingEnd => level -= 1,
            _ => {}
        }
        if level == 0 {
            return false;
        }
    }
    true
}

/// Append a directive to the directives stack.
fn yaml_emitter_append_tag_directive(
    emitter: &mut YamlEmitter<'_>,
    value: &YamlTagDirective,
    allow_duplicates: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_append_tag_directive
    for i in 0..emitter.tag_directives.len() {
        if value.handle == emitter.tag_directives[i].handle {
            if allow_duplicates {
                return true;
            }
            return yaml_emitter_set_emitter_error(emitter, "duplicate %TAG directive");
        }
    }

    // [Go] Do we actually need to copy this given garbage collection
    // and the lack of deallocating destructors?
    let tag_copy = YamlTagDirective {
        handle: value.handle.clone(),
        prefix: value.prefix.clone(),
    };
    emitter.tag_directives.push(tag_copy);
    true
}

/// Increase the indentation level.
fn yaml_emitter_increase_indent(
    emitter: &mut YamlEmitter<'_>,
    flow: bool,
    indentless: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_increase_indent
    emitter.indents.push(emitter.indent);
    if emitter.indent < 0 {
        if flow {
            emitter.indent = emitter.best_indent;
        } else {
            emitter.indent = 0;
        }
    } else if !indentless {
        // [Go] This was changed so that indentations are more regular.
        if emitter.states[emitter.states.len() - 1] == YamlEmitterState::BlockSequenceItem {
            // The first indent inside a sequence will just skip the "- " indicator.
            emitter.indent += 2;
        } else {
            // Everything else aligns to the chosen indentation.
            emitter.indent = emitter.best_indent
                * ((emitter.indent + emitter.best_indent) / emitter.best_indent);
        }
    }
    true
}

/// Go's inline `emitter.indent = emitter.indents[len(emitter.indents)-1]` +
/// `emitter.indents = emitter.indents[:len(emitter.indents)-1]`.
fn pop_indent(emitter: &mut YamlEmitter<'_>) {
    let Some(indent) = emitter.indents.pop() else {
        panic!("emitter indent stack underflow");
    };
    emitter.indent = indent;
}

/// Go's inline `emitter.state = emitter.states[len(emitter.states)-1]` +
/// `emitter.states = emitter.states[:len(emitter.states)-1]`.
fn pop_state(emitter: &mut YamlEmitter<'_>) {
    let Some(state) = emitter.states.pop() else {
        panic!("emitter state stack underflow");
    };
    emitter.state = state;
}

/// State dispatcher.
fn yaml_emitter_state_machine(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_state_machine
    match emitter.state {
        YamlEmitterState::StreamStart => yaml_emitter_emit_stream_start(emitter, event),
        YamlEmitterState::FirstDocumentStart => {
            yaml_emitter_emit_document_start(emitter, event, true)
        }
        YamlEmitterState::DocumentStart => yaml_emitter_emit_document_start(emitter, event, false),
        YamlEmitterState::DocumentContent => yaml_emitter_emit_document_content(emitter, event),
        YamlEmitterState::DocumentEnd => yaml_emitter_emit_document_end(emitter, event),
        YamlEmitterState::FlowSequenceFirstItem => {
            yaml_emitter_emit_flow_sequence_item(emitter, event, true, false)
        }
        YamlEmitterState::FlowSequenceTrailItem => {
            yaml_emitter_emit_flow_sequence_item(emitter, event, false, true)
        }
        YamlEmitterState::FlowSequenceItem => {
            yaml_emitter_emit_flow_sequence_item(emitter, event, false, false)
        }
        YamlEmitterState::FlowMappingFirstKey => {
            yaml_emitter_emit_flow_mapping_key(emitter, event, true, false)
        }
        YamlEmitterState::FlowMappingTrailKey => {
            yaml_emitter_emit_flow_mapping_key(emitter, event, false, true)
        }
        YamlEmitterState::FlowMappingKey => {
            yaml_emitter_emit_flow_mapping_key(emitter, event, false, false)
        }
        YamlEmitterState::FlowMappingSimpleValue => {
            yaml_emitter_emit_flow_mapping_value(emitter, event, true)
        }
        YamlEmitterState::FlowMappingValue => {
            yaml_emitter_emit_flow_mapping_value(emitter, event, false)
        }
        YamlEmitterState::BlockSequenceFirstItem => {
            yaml_emitter_emit_block_sequence_item(emitter, event, true)
        }
        YamlEmitterState::BlockSequenceItem => {
            yaml_emitter_emit_block_sequence_item(emitter, event, false)
        }
        YamlEmitterState::BlockMappingFirstKey => {
            yaml_emitter_emit_block_mapping_key(emitter, event, true)
        }
        YamlEmitterState::BlockMappingKey => {
            yaml_emitter_emit_block_mapping_key(emitter, event, false)
        }
        YamlEmitterState::BlockMappingSimpleValue => {
            yaml_emitter_emit_block_mapping_value(emitter, event, true)
        }
        YamlEmitterState::BlockMappingValue => {
            yaml_emitter_emit_block_mapping_value(emitter, event, false)
        }
        YamlEmitterState::End => {
            yaml_emitter_set_emitter_error(emitter, "expected nothing after STREAM-END")
        }
    }
}

/// Expect STREAM-START.
fn yaml_emitter_emit_stream_start(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_stream_start
    if event.typ != YamlEventType::StreamStart {
        return yaml_emitter_set_emitter_error(emitter, "expected STREAM-START");
    }
    if emitter.encoding == YamlEncoding::Any {
        emitter.encoding = event.encoding;
        if emitter.encoding == YamlEncoding::Any {
            emitter.encoding = YamlEncoding::Utf8;
        }
    }
    if emitter.best_indent < 2 || emitter.best_indent > 9 {
        emitter.best_indent = 2;
    }
    if emitter.best_width >= 0 && emitter.best_width <= emitter.best_indent * 2 {
        emitter.best_width = 80;
    }
    if emitter.best_width < 0 {
        emitter.best_width = (1 << 31) - 1;
    }
    if emitter.line_break == YamlBreak::Any {
        emitter.line_break = YamlBreak::Ln;
    }

    emitter.indent = -1;
    emitter.line = 0;
    emitter.column = 0;
    emitter.whitespace = true;
    emitter.indention = true;
    emitter.space_above = true;
    emitter.foot_indent = -1;

    if emitter.encoding != YamlEncoding::Utf8 && !yaml_emitter_write_bom(emitter) {
        return false;
    }
    emitter.state = YamlEmitterState::FirstDocumentStart;
    true
}

/// Expect DOCUMENT-START or STREAM-END.
fn yaml_emitter_emit_document_start(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    first: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_document_start
    if event.typ == YamlEventType::DocumentStart {
        if let Some(version_directive) = &event.version_directive
            && !yaml_emitter_analyze_version_directive(emitter, version_directive)
        {
            return false;
        }

        for tag_directive in &event.tag_directives {
            if !yaml_emitter_analyze_tag_directive(emitter, tag_directive) {
                return false;
            }
            if !yaml_emitter_append_tag_directive(emitter, tag_directive, false) {
                return false;
            }
        }

        for tag_directive in &default_tag_directives() {
            if !yaml_emitter_append_tag_directive(emitter, tag_directive, true) {
                return false;
            }
        }

        let mut implicit = event.implicit;
        if !first || emitter.canonical {
            implicit = false;
        }

        if emitter.open_ended
            && (event.version_directive.is_some() || !event.tag_directives.is_empty())
        {
            if !yaml_emitter_write_indicator(emitter, b"...", true, false, false) {
                return false;
            }
            if !yaml_emitter_write_indent(emitter) {
                return false;
            }
        }

        if event.version_directive.is_some() {
            implicit = false;
            if !yaml_emitter_write_indicator(emitter, b"%YAML", true, false, false) {
                return false;
            }
            if !yaml_emitter_write_indicator(emitter, b"1.1", true, false, false) {
                return false;
            }
            if !yaml_emitter_write_indent(emitter) {
                return false;
            }
        }

        if !event.tag_directives.is_empty() {
            implicit = false;
            for tag_directive in &event.tag_directives {
                if !yaml_emitter_write_indicator(emitter, b"%TAG", true, false, false) {
                    return false;
                }
                if !yaml_emitter_write_tag_handle(emitter, &tag_directive.handle) {
                    return false;
                }
                if !yaml_emitter_write_tag_content(emitter, &tag_directive.prefix, true) {
                    return false;
                }
                if !yaml_emitter_write_indent(emitter) {
                    return false;
                }
            }
        }

        if yaml_emitter_check_empty_document(emitter) {
            implicit = false;
        }
        if !implicit {
            if !yaml_emitter_write_indent(emitter) {
                return false;
            }
            if !yaml_emitter_write_indicator(emitter, b"---", true, false, false) {
                return false;
            }
            // Go: `if emitter.canonical || true` — always.
            if !yaml_emitter_write_indent(emitter) {
                return false;
            }
        }

        if !emitter.head_comment.is_empty() {
            if !yaml_emitter_process_head_comment(emitter) {
                return false;
            }
            if !put_break(emitter) {
                return false;
            }
        }

        emitter.state = YamlEmitterState::DocumentContent;
        return true;
    }

    if event.typ == YamlEventType::StreamEnd {
        if emitter.open_ended {
            if !yaml_emitter_write_indicator(emitter, b"...", true, false, false) {
                return false;
            }
            if !yaml_emitter_write_indent(emitter) {
                return false;
            }
        }
        if !yaml_emitter_flush(emitter) {
            return false;
        }
        emitter.state = YamlEmitterState::End;
        return true;
    }

    yaml_emitter_set_emitter_error(emitter, "expected DOCUMENT-START or STREAM-END")
}

/// Expect the root node.
fn yaml_emitter_emit_document_content(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_document_content
    emitter.states.push(YamlEmitterState::DocumentEnd);

    if !yaml_emitter_process_head_comment(emitter) {
        return false;
    }
    if !yaml_emitter_emit_node(emitter, event, true, false, false, false) {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }
    if !yaml_emitter_process_foot_comment(emitter) {
        return false;
    }
    true
}

/// Expect DOCUMENT-END.
fn yaml_emitter_emit_document_end(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_document_end
    if event.typ != YamlEventType::DocumentEnd {
        return yaml_emitter_set_emitter_error(emitter, "expected DOCUMENT-END");
    }
    // [Go] Force document foot separation.
    emitter.foot_indent = 0;
    if !yaml_emitter_process_foot_comment(emitter) {
        return false;
    }
    emitter.foot_indent = -1;
    if !yaml_emitter_write_indent(emitter) {
        return false;
    }
    if !event.implicit {
        // [Go] Allocate the slice elsewhere.
        if !yaml_emitter_write_indicator(emitter, b"...", true, false, false) {
            return false;
        }
        if !yaml_emitter_write_indent(emitter) {
            return false;
        }
    }
    if !yaml_emitter_flush(emitter) {
        return false;
    }
    emitter.state = YamlEmitterState::DocumentStart;
    emitter.tag_directives.clear();
    true
}

/// Expect a flow item node.
fn yaml_emitter_emit_flow_sequence_item(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    first: bool,
    trail: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_flow_sequence_item
    if first {
        if !yaml_emitter_write_indicator(emitter, b"[", true, true, false) {
            return false;
        }
        if !yaml_emitter_increase_indent(emitter, true, false) {
            return false;
        }
        emitter.flow_level += 1;
    }

    if event.typ == YamlEventType::SequenceEnd {
        if emitter.canonical
            && !first
            && !trail
            && !yaml_emitter_write_indicator(emitter, b",", false, false, false)
        {
            return false;
        }
        emitter.flow_level -= 1;
        pop_indent(emitter);
        if (emitter.column == 0 || emitter.canonical && !first)
            && !yaml_emitter_write_indent(emitter)
        {
            return false;
        }
        if !yaml_emitter_write_indicator(emitter, b"]", false, false, false) {
            return false;
        }
        if !yaml_emitter_process_line_comment(emitter) {
            return false;
        }
        if !yaml_emitter_process_foot_comment(emitter) {
            return false;
        }
        pop_state(emitter);

        return true;
    }

    if !first && !trail && !yaml_emitter_write_indicator(emitter, b",", false, false, false) {
        return false;
    }

    if !yaml_emitter_process_head_comment(emitter) {
        return false;
    }
    if emitter.column == 0 && !yaml_emitter_write_indent(emitter) {
        return false;
    }

    if (emitter.canonical || emitter.column > emitter.best_width)
        && !yaml_emitter_write_indent(emitter)
    {
        return false;
    }
    if emitter.line_comment.len() + emitter.foot_comment.len() + emitter.tail_comment.len() > 0 {
        emitter.states.push(YamlEmitterState::FlowSequenceTrailItem);
    } else {
        emitter.states.push(YamlEmitterState::FlowSequenceItem);
    }
    if !yaml_emitter_emit_node(emitter, event, false, true, false, false) {
        return false;
    }
    if emitter.line_comment.len() + emitter.foot_comment.len() + emitter.tail_comment.len() > 0
        && !yaml_emitter_write_indicator(emitter, b",", false, false, false)
    {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }
    if !yaml_emitter_process_foot_comment(emitter) {
        return false;
    }
    true
}

/// Expect a flow key node.
fn yaml_emitter_emit_flow_mapping_key(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    first: bool,
    trail: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_flow_mapping_key
    if first {
        if !yaml_emitter_write_indicator(emitter, b"{", true, true, false) {
            return false;
        }
        if !yaml_emitter_increase_indent(emitter, true, false) {
            return false;
        }
        emitter.flow_level += 1;
    }

    if event.typ == YamlEventType::MappingEnd {
        if (emitter.canonical
            || emitter.head_comment.len() + emitter.foot_comment.len() + emitter.tail_comment.len()
                > 0)
            && !first
            && !trail
            && !yaml_emitter_write_indicator(emitter, b",", false, false, false)
        {
            return false;
        }
        if !yaml_emitter_process_head_comment(emitter) {
            return false;
        }
        emitter.flow_level -= 1;
        pop_indent(emitter);
        if emitter.canonical && !first && !yaml_emitter_write_indent(emitter) {
            return false;
        }
        if !yaml_emitter_write_indicator(emitter, b"}", false, false, false) {
            return false;
        }
        if !yaml_emitter_process_line_comment(emitter) {
            return false;
        }
        if !yaml_emitter_process_foot_comment(emitter) {
            return false;
        }
        pop_state(emitter);
        return true;
    }

    if !first && !trail && !yaml_emitter_write_indicator(emitter, b",", false, false, false) {
        return false;
    }

    if !yaml_emitter_process_head_comment(emitter) {
        return false;
    }

    if emitter.column == 0 && !yaml_emitter_write_indent(emitter) {
        return false;
    }

    if (emitter.canonical || emitter.column > emitter.best_width)
        && !yaml_emitter_write_indent(emitter)
    {
        return false;
    }

    if !emitter.canonical && yaml_emitter_check_simple_key(emitter) {
        emitter
            .states
            .push(YamlEmitterState::FlowMappingSimpleValue);
        return yaml_emitter_emit_node(emitter, event, false, false, true, true);
    }
    if !yaml_emitter_write_indicator(emitter, b"?", true, false, false) {
        return false;
    }
    emitter.states.push(YamlEmitterState::FlowMappingValue);
    yaml_emitter_emit_node(emitter, event, false, false, true, false)
}

/// Expect a flow value node.
fn yaml_emitter_emit_flow_mapping_value(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    simple: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_flow_mapping_value
    if simple {
        if !yaml_emitter_write_indicator(emitter, b":", false, false, false) {
            return false;
        }
    } else {
        if (emitter.canonical || emitter.column > emitter.best_width)
            && !yaml_emitter_write_indent(emitter)
        {
            return false;
        }
        if !yaml_emitter_write_indicator(emitter, b":", true, false, false) {
            return false;
        }
    }
    if emitter.line_comment.len() + emitter.foot_comment.len() + emitter.tail_comment.len() > 0 {
        emitter.states.push(YamlEmitterState::FlowMappingTrailKey);
    } else {
        emitter.states.push(YamlEmitterState::FlowMappingKey);
    }
    if !yaml_emitter_emit_node(emitter, event, false, false, true, false) {
        return false;
    }
    if emitter.line_comment.len() + emitter.foot_comment.len() + emitter.tail_comment.len() > 0
        && !yaml_emitter_write_indicator(emitter, b",", false, false, false)
    {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }
    if !yaml_emitter_process_foot_comment(emitter) {
        return false;
    }
    true
}

/// Expect a block item node.
fn yaml_emitter_emit_block_sequence_item(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    first: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_block_sequence_item
    if first && !yaml_emitter_increase_indent(emitter, false, false) {
        return false;
    }
    if event.typ == YamlEventType::SequenceEnd {
        pop_indent(emitter);
        pop_state(emitter);
        return true;
    }
    if !yaml_emitter_process_head_comment(emitter) {
        return false;
    }
    if !yaml_emitter_write_indent(emitter) {
        return false;
    }
    if !yaml_emitter_write_indicator(emitter, b"-", true, false, true) {
        return false;
    }
    emitter.states.push(YamlEmitterState::BlockSequenceItem);
    if !yaml_emitter_emit_node(emitter, event, false, true, false, false) {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }
    if !yaml_emitter_process_foot_comment(emitter) {
        return false;
    }
    true
}

/// Expect a block key node.
fn yaml_emitter_emit_block_mapping_key(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    first: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_block_mapping_key
    if first && !yaml_emitter_increase_indent(emitter, false, false) {
        return false;
    }
    if !yaml_emitter_process_head_comment(emitter) {
        return false;
    }
    if event.typ == YamlEventType::MappingEnd {
        pop_indent(emitter);
        pop_state(emitter);
        return true;
    }
    if !yaml_emitter_write_indent(emitter) {
        return false;
    }
    if !emitter.line_comment.is_empty() {
        // [Go] A line comment was provided for the key. That's unusual as the
        //      scanner associates line comments with the value. Either way,
        //      save the line comment and render it appropriately later.
        emitter.key_line_comment = std::mem::take(&mut emitter.line_comment);
    }
    if yaml_emitter_check_simple_key(emitter) {
        emitter
            .states
            .push(YamlEmitterState::BlockMappingSimpleValue);
        return yaml_emitter_emit_node(emitter, event, false, false, true, true);
    }
    if !yaml_emitter_write_indicator(emitter, b"?", true, false, true) {
        return false;
    }
    emitter.states.push(YamlEmitterState::BlockMappingValue);
    yaml_emitter_emit_node(emitter, event, false, false, true, false)
}

/// Expect a block value node.
fn yaml_emitter_emit_block_mapping_value(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    simple: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_block_mapping_value
    if simple {
        if !yaml_emitter_write_indicator(emitter, b":", false, false, false) {
            return false;
        }
    } else {
        if !yaml_emitter_write_indent(emitter) {
            return false;
        }
        if !yaml_emitter_write_indicator(emitter, b":", true, false, true) {
            return false;
        }
    }
    if !emitter.key_line_comment.is_empty() {
        // [Go] Line comments are generally associated with the value, but when there's
        //      no value on the same line as a mapping key they end up attached to the
        //      key itself.
        if event.typ == YamlEventType::Scalar {
            if emitter.line_comment.is_empty() {
                // A scalar is coming and it has no line comments by itself yet,
                // so just let it handle the line comment as usual. If it has a
                // line comment, we can't have both so the one from the key is lost.
                emitter.line_comment = std::mem::take(&mut emitter.key_line_comment);
            }
        } else if event.sequence_style() != YamlSequenceStyle::Flow
            && (event.typ == YamlEventType::MappingStart
                || event.typ == YamlEventType::SequenceStart)
        {
            // Go compares the event's style as a sequence style even for a MAPPING-START
            // (emitterc.go:828); the flow values coincide (2).
            // An indented block follows, so write the comment right now.
            std::mem::swap(&mut emitter.line_comment, &mut emitter.key_line_comment);
            if !yaml_emitter_process_line_comment(emitter) {
                return false;
            }
            std::mem::swap(&mut emitter.line_comment, &mut emitter.key_line_comment);
        }
    }
    emitter.states.push(YamlEmitterState::BlockMappingKey);
    if !yaml_emitter_emit_node(emitter, event, false, false, true, false) {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }
    if !yaml_emitter_process_foot_comment(emitter) {
        return false;
    }
    true
}

// Unused in yaml.v3 v3.0.1 as well (declared at emitterc.go:850, never called).
#[allow(dead_code)]
fn yaml_emitter_silent_nil_event(emitter: &YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_silent_nil_event
    event.typ == YamlEventType::Scalar
        && event.implicit
        && !emitter.canonical
        && emitter.scalar_data.value.is_empty()
}

/// Expect a node.
fn yaml_emitter_emit_node(
    emitter: &mut YamlEmitter<'_>,
    event: &YamlEvent,
    root: bool,
    sequence: bool,
    mapping: bool,
    simple_key: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_emit_node
    emitter.root_context = root;
    emitter.sequence_context = sequence;
    emitter.mapping_context = mapping;
    emitter.simple_key_context = simple_key;

    match event.typ {
        YamlEventType::Alias => yaml_emitter_emit_alias(emitter, event),
        YamlEventType::Scalar => yaml_emitter_emit_scalar(emitter, event),
        YamlEventType::SequenceStart => yaml_emitter_emit_sequence_start(emitter, event),
        YamlEventType::MappingStart => yaml_emitter_emit_mapping_start(emitter, event),
        _ => yaml_emitter_set_emitter_error(
            emitter,
            &format!(
                "expected SCALAR, SEQUENCE-START, MAPPING-START, or ALIAS, but got {}",
                event.typ
            ),
        ),
    }
}

/// Expect ALIAS.
fn yaml_emitter_emit_alias(emitter: &mut YamlEmitter<'_>, _event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_alias
    if !yaml_emitter_process_anchor(emitter) {
        return false;
    }
    pop_state(emitter);
    true
}

/// Expect SCALAR.
fn yaml_emitter_emit_scalar(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_scalar
    if !yaml_emitter_select_scalar_style(emitter, event) {
        return false;
    }
    if !yaml_emitter_process_anchor(emitter) {
        return false;
    }
    if !yaml_emitter_process_tag(emitter) {
        return false;
    }
    if !yaml_emitter_increase_indent(emitter, true, false) {
        return false;
    }
    if !yaml_emitter_process_scalar(emitter) {
        return false;
    }
    pop_indent(emitter);
    pop_state(emitter);
    true
}

/// Expect SEQUENCE-START.
fn yaml_emitter_emit_sequence_start(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_sequence_start
    if !yaml_emitter_process_anchor(emitter) {
        return false;
    }
    if !yaml_emitter_process_tag(emitter) {
        return false;
    }
    if emitter.flow_level > 0
        || emitter.canonical
        || event.sequence_style() == YamlSequenceStyle::Flow
        || yaml_emitter_check_empty_sequence(emitter)
    {
        emitter.state = YamlEmitterState::FlowSequenceFirstItem;
    } else {
        emitter.state = YamlEmitterState::BlockSequenceFirstItem;
    }
    true
}

/// Expect MAPPING-START.
fn yaml_emitter_emit_mapping_start(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_emit_mapping_start
    if !yaml_emitter_process_anchor(emitter) {
        return false;
    }
    if !yaml_emitter_process_tag(emitter) {
        return false;
    }
    if emitter.flow_level > 0
        || emitter.canonical
        || event.mapping_style() == YamlMappingStyle::Flow
        || yaml_emitter_check_empty_mapping(emitter)
    {
        emitter.state = YamlEmitterState::FlowMappingFirstKey;
    } else {
        emitter.state = YamlEmitterState::BlockMappingFirstKey;
    }
    true
}

/// Check if the document content is an empty scalar.
fn yaml_emitter_check_empty_document(_emitter: &YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_check_empty_document
    false // [Go] Huh?
}

/// Check if the next events represent an empty sequence.
fn yaml_emitter_check_empty_sequence(emitter: &YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_check_empty_sequence
    if emitter.events.len() - emitter.events_head < 2 {
        return false;
    }
    emitter.events[emitter.events_head].typ == YamlEventType::SequenceStart
        && emitter.events[emitter.events_head + 1].typ == YamlEventType::SequenceEnd
}

/// Check if the next events represent an empty mapping.
fn yaml_emitter_check_empty_mapping(emitter: &YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_check_empty_mapping
    if emitter.events.len() - emitter.events_head < 2 {
        return false;
    }
    emitter.events[emitter.events_head].typ == YamlEventType::MappingStart
        && emitter.events[emitter.events_head + 1].typ == YamlEventType::MappingEnd
}

/// Check if the next node can be expressed as a simple key.
fn yaml_emitter_check_simple_key(emitter: &YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_check_simple_key
    let mut length = 0;
    match emitter.events[emitter.events_head].typ {
        YamlEventType::Alias => {
            length += emitter.anchor_data.anchor.len();
        }
        YamlEventType::Scalar => {
            if emitter.scalar_data.multiline {
                return false;
            }
            length += emitter.anchor_data.anchor.len()
                + emitter.tag_data.handle.len()
                + emitter.tag_data.suffix.len()
                + emitter.scalar_data.value.len();
        }
        YamlEventType::SequenceStart => {
            if !yaml_emitter_check_empty_sequence(emitter) {
                return false;
            }
            length += emitter.anchor_data.anchor.len()
                + emitter.tag_data.handle.len()
                + emitter.tag_data.suffix.len();
        }
        YamlEventType::MappingStart => {
            if !yaml_emitter_check_empty_mapping(emitter) {
                return false;
            }
            length += emitter.anchor_data.anchor.len()
                + emitter.tag_data.handle.len()
                + emitter.tag_data.suffix.len();
        }
        _ => return false,
    }
    length <= 128
}

/// Determine an acceptable scalar style.
fn yaml_emitter_select_scalar_style(emitter: &mut YamlEmitter<'_>, event: &YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_select_scalar_style
    let no_tag = emitter.tag_data.handle.is_empty() && emitter.tag_data.suffix.is_empty();
    if no_tag && !event.implicit && !event.quoted_implicit {
        return yaml_emitter_set_emitter_error(
            emitter,
            "neither tag nor implicit flags are specified",
        );
    }

    let mut style = event.scalar_style();
    if style == YamlScalarStyle::Any {
        style = YamlScalarStyle::Plain;
    }
    if emitter.canonical {
        style = YamlScalarStyle::DoubleQuoted;
    }
    if emitter.simple_key_context && emitter.scalar_data.multiline {
        style = YamlScalarStyle::DoubleQuoted;
    }

    if style == YamlScalarStyle::Plain {
        if emitter.flow_level > 0 && !emitter.scalar_data.flow_plain_allowed
            || emitter.flow_level == 0 && !emitter.scalar_data.block_plain_allowed
        {
            style = YamlScalarStyle::SingleQuoted;
        }
        if emitter.scalar_data.value.is_empty()
            && (emitter.flow_level > 0 || emitter.simple_key_context)
        {
            style = YamlScalarStyle::SingleQuoted;
        }
        if no_tag && !event.implicit {
            style = YamlScalarStyle::SingleQuoted;
        }
    }
    if style == YamlScalarStyle::SingleQuoted && !emitter.scalar_data.single_quoted_allowed {
        style = YamlScalarStyle::DoubleQuoted;
    }
    if (style == YamlScalarStyle::Literal || style == YamlScalarStyle::Folded)
        && (!emitter.scalar_data.block_allowed
            || emitter.flow_level > 0
            || emitter.simple_key_context)
    {
        style = YamlScalarStyle::DoubleQuoted;
    }

    if no_tag && !event.quoted_implicit && style != YamlScalarStyle::Plain {
        emitter.tag_data.handle = b"!".to_vec();
    }
    emitter.scalar_data.style = style;
    true
}

/// Write an anchor.
fn yaml_emitter_process_anchor(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_process_anchor
    // Go tests `anchor == nil`; the anchor is only ever set non-empty (analyze_anchor).
    if emitter.anchor_data.anchor.is_empty() {
        return true;
    }
    let c: &[u8] = if emitter.anchor_data.alias {
        b"*"
    } else {
        b"&"
    };
    if !yaml_emitter_write_indicator(emitter, c, true, false, false) {
        return false;
    }
    let anchor = std::mem::take(&mut emitter.anchor_data.anchor);
    let ok = yaml_emitter_write_anchor(emitter, &anchor);
    emitter.anchor_data.anchor = anchor;
    ok
}

/// Write a tag.
fn yaml_emitter_process_tag(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_process_tag
    if emitter.tag_data.handle.is_empty() && emitter.tag_data.suffix.is_empty() {
        return true;
    }
    let handle = emitter.tag_data.handle.clone();
    let suffix = emitter.tag_data.suffix.clone();
    if !handle.is_empty() {
        if !yaml_emitter_write_tag_handle(emitter, &handle) {
            return false;
        }
        if !suffix.is_empty() && !yaml_emitter_write_tag_content(emitter, &suffix, false) {
            return false;
        }
    } else {
        // [Go] Allocate these slices elsewhere.
        if !yaml_emitter_write_indicator(emitter, b"!<", true, false, false) {
            return false;
        }
        if !yaml_emitter_write_tag_content(emitter, &suffix, false) {
            return false;
        }
        if !yaml_emitter_write_indicator(emitter, b">", false, false, false) {
            return false;
        }
    }
    true
}

/// Write a scalar.
fn yaml_emitter_process_scalar(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_process_scalar
    let value = std::mem::take(&mut emitter.scalar_data.value);
    let allow_breaks = !emitter.simple_key_context;
    let ok = match emitter.scalar_data.style {
        YamlScalarStyle::Plain => yaml_emitter_write_plain_scalar(emitter, &value, allow_breaks),
        YamlScalarStyle::SingleQuoted => {
            yaml_emitter_write_single_quoted_scalar(emitter, &value, allow_breaks)
        }
        YamlScalarStyle::DoubleQuoted => {
            yaml_emitter_write_double_quoted_scalar(emitter, &value, allow_breaks)
        }
        YamlScalarStyle::Literal => yaml_emitter_write_literal_scalar(emitter, &value),
        YamlScalarStyle::Folded => yaml_emitter_write_folded_scalar(emitter, &value),
        YamlScalarStyle::Any => panic!("unknown scalar style"),
    };
    emitter.scalar_data.value = value;
    ok
}

/// Write a head comment.
fn yaml_emitter_process_head_comment(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_process_head_comment
    if !emitter.tail_comment.is_empty() {
        if !yaml_emitter_write_indent(emitter) {
            return false;
        }
        let tail_comment = std::mem::take(&mut emitter.tail_comment);
        if !yaml_emitter_write_comment(emitter, &tail_comment) {
            emitter.tail_comment = tail_comment;
            return false;
        }
        // Go: emitter.tail_comment = emitter.tail_comment[:0] (taken above).
        emitter.foot_indent = emitter.indent;
        if emitter.foot_indent < 0 {
            emitter.foot_indent = 0;
        }
    }

    if emitter.head_comment.is_empty() {
        return true;
    }
    if !yaml_emitter_write_indent(emitter) {
        return false;
    }
    let head_comment = std::mem::take(&mut emitter.head_comment);
    if !yaml_emitter_write_comment(emitter, &head_comment) {
        emitter.head_comment = head_comment;
        return false;
    }
    // Go: emitter.head_comment = emitter.head_comment[:0] (taken above).
    true
}

/// Write an line comment.
fn yaml_emitter_process_line_comment(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_process_line_comment
    if emitter.line_comment.is_empty() {
        return true;
    }
    if !emitter.whitespace && !put(emitter, b' ') {
        return false;
    }
    let line_comment = std::mem::take(&mut emitter.line_comment);
    if !yaml_emitter_write_comment(emitter, &line_comment) {
        emitter.line_comment = line_comment;
        return false;
    }
    // Go: emitter.line_comment = emitter.line_comment[:0] (taken above).
    true
}

/// Write a foot comment.
fn yaml_emitter_process_foot_comment(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_process_foot_comment
    if emitter.foot_comment.is_empty() {
        return true;
    }
    if !yaml_emitter_write_indent(emitter) {
        return false;
    }
    let foot_comment = std::mem::take(&mut emitter.foot_comment);
    if !yaml_emitter_write_comment(emitter, &foot_comment) {
        emitter.foot_comment = foot_comment;
        return false;
    }
    // Go: emitter.foot_comment = emitter.foot_comment[:0] (taken above).
    emitter.foot_indent = emitter.indent;
    if emitter.foot_indent < 0 {
        emitter.foot_indent = 0;
    }
    true
}

/// Check if a %YAML directive is valid.
fn yaml_emitter_analyze_version_directive(
    emitter: &mut YamlEmitter<'_>,
    version_directive: &YamlVersionDirective,
) -> bool {
    // Go: emitterc.go yaml_emitter_analyze_version_directive
    if version_directive.major != 1 || version_directive.minor != 1 {
        return yaml_emitter_set_emitter_error(emitter, "incompatible %YAML directive");
    }
    true
}

/// Check if a %TAG directive is valid.
fn yaml_emitter_analyze_tag_directive(
    emitter: &mut YamlEmitter<'_>,
    tag_directive: &YamlTagDirective,
) -> bool {
    // Go: emitterc.go yaml_emitter_analyze_tag_directive
    let handle = &tag_directive.handle;
    let prefix = &tag_directive.prefix;
    if handle.is_empty() {
        return yaml_emitter_set_emitter_error(emitter, "tag handle must not be empty");
    }
    if handle[0] != b'!' {
        return yaml_emitter_set_emitter_error(emitter, "tag handle must start with '!'");
    }
    if handle[handle.len() - 1] != b'!' {
        return yaml_emitter_set_emitter_error(emitter, "tag handle must end with '!'");
    }
    let mut i = 1;
    while i < handle.len() - 1 {
        if !is_alpha(handle, i) {
            return yaml_emitter_set_emitter_error(
                emitter,
                "tag handle must contain alphanumerical characters only",
            );
        }
        i += width(handle[i]);
    }
    if prefix.is_empty() {
        return yaml_emitter_set_emitter_error(emitter, "tag prefix must not be empty");
    }
    true
}

/// Check if an anchor is valid.
fn yaml_emitter_analyze_anchor(emitter: &mut YamlEmitter<'_>, anchor: &[u8], alias: bool) -> bool {
    // Go: emitterc.go yaml_emitter_analyze_anchor
    if anchor.is_empty() {
        let problem = if alias {
            "alias value must not be empty"
        } else {
            "anchor value must not be empty"
        };
        return yaml_emitter_set_emitter_error(emitter, problem);
    }
    let mut i = 0;
    while i < anchor.len() {
        if !is_alpha(anchor, i) {
            let problem = if alias {
                "alias value must contain alphanumerical characters only"
            } else {
                "anchor value must contain alphanumerical characters only"
            };
            return yaml_emitter_set_emitter_error(emitter, problem);
        }
        i += width(anchor[i]);
    }
    emitter.anchor_data.anchor = anchor.to_vec();
    emitter.anchor_data.alias = alias;
    true
}

/// Check if a tag is valid.
fn yaml_emitter_analyze_tag(emitter: &mut YamlEmitter<'_>, tag: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_analyze_tag
    if tag.is_empty() {
        return yaml_emitter_set_emitter_error(emitter, "tag value must not be empty");
    }
    for i in 0..emitter.tag_directives.len() {
        let tag_directive = &emitter.tag_directives[i];
        if tag.starts_with(&tag_directive.prefix) {
            let handle = tag_directive.handle.clone();
            let suffix = tag[tag_directive.prefix.len()..].to_vec();
            emitter.tag_data.handle = handle;
            emitter.tag_data.suffix = suffix;
            return true;
        }
    }
    emitter.tag_data.suffix = tag.to_vec();
    true
}

/// Check if a scalar is valid.
fn yaml_emitter_analyze_scalar(emitter: &mut YamlEmitter<'_>, value: Vec<u8>) -> bool {
    // Go: emitterc.go yaml_emitter_analyze_scalar
    let mut block_indicators = false;
    let mut flow_indicators = false;
    let mut line_breaks = false;
    let mut special_characters = false;
    let mut tab_characters = false;

    let mut leading_space = false;
    let mut leading_break = false;
    let mut trailing_space = false;
    let mut trailing_break = false;
    let mut break_space = false;
    let mut space_break = false;

    let mut previous_space = false;
    let mut previous_break = false;

    emitter.scalar_data.value = value;
    let value = &emitter.scalar_data.value;

    if value.is_empty() {
        emitter.scalar_data.multiline = false;
        emitter.scalar_data.flow_plain_allowed = false;
        emitter.scalar_data.block_plain_allowed = true;
        emitter.scalar_data.single_quoted_allowed = true;
        emitter.scalar_data.block_allowed = false;
        return true;
    }

    if value.len() >= 3
        && ((value[0] == b'-' && value[1] == b'-' && value[2] == b'-')
            || (value[0] == b'.' && value[1] == b'.' && value[2] == b'.'))
    {
        block_indicators = true;
        flow_indicators = true;
    }

    let mut preceded_by_whitespace = true;
    let mut i = 0;
    while i < value.len() {
        let w = width(value[i]);
        let followed_by_whitespace = i + w >= value.len() || is_blank(value, i + w);

        if i == 0 {
            match value[i] {
                b'#' | b',' | b'[' | b']' | b'{' | b'}' | b'&' | b'*' | b'!' | b'|' | b'>'
                | b'\'' | b'"' | b'%' | b'@' | b'`' => {
                    flow_indicators = true;
                    block_indicators = true;
                }
                b'?' | b':' => {
                    flow_indicators = true;
                    if followed_by_whitespace {
                        block_indicators = true;
                    }
                }
                b'-' if followed_by_whitespace => {
                    flow_indicators = true;
                    block_indicators = true;
                }
                _ => {}
            }
        } else {
            match value[i] {
                b',' | b'?' | b'[' | b']' | b'{' | b'}' => {
                    flow_indicators = true;
                }
                b':' => {
                    flow_indicators = true;
                    if followed_by_whitespace {
                        block_indicators = true;
                    }
                }
                b'#' if preceded_by_whitespace => {
                    flow_indicators = true;
                    block_indicators = true;
                }
                _ => {}
            }
        }

        if value[i] == b'\t' {
            tab_characters = true;
        } else if !is_printable(value, i) || !is_ascii(value, i) && !emitter.unicode {
            special_characters = true;
        }
        if is_space(value, i) {
            if i == 0 {
                leading_space = true;
            }
            if i + width(value[i]) == value.len() {
                trailing_space = true;
            }
            if previous_break {
                break_space = true;
            }
            previous_space = true;
            previous_break = false;
        } else if is_break(value, i) {
            line_breaks = true;
            if i == 0 {
                leading_break = true;
            }
            if i + width(value[i]) == value.len() {
                trailing_break = true;
            }
            if previous_space {
                space_break = true;
            }
            previous_space = false;
            previous_break = true;
        } else {
            previous_space = false;
            previous_break = false;
        }

        // [Go]: Why 'z'? Couldn't be the end of the string as that's the loop condition.
        preceded_by_whitespace = is_blankz(value, i);
        i += w;
    }

    emitter.scalar_data.multiline = line_breaks;
    emitter.scalar_data.flow_plain_allowed = true;
    emitter.scalar_data.block_plain_allowed = true;
    emitter.scalar_data.single_quoted_allowed = true;
    emitter.scalar_data.block_allowed = true;

    if leading_space || leading_break || trailing_space || trailing_break {
        emitter.scalar_data.flow_plain_allowed = false;
        emitter.scalar_data.block_plain_allowed = false;
    }
    if trailing_space {
        emitter.scalar_data.block_allowed = false;
    }
    if break_space {
        emitter.scalar_data.flow_plain_allowed = false;
        emitter.scalar_data.block_plain_allowed = false;
        emitter.scalar_data.single_quoted_allowed = false;
    }
    if space_break || tab_characters || special_characters {
        emitter.scalar_data.flow_plain_allowed = false;
        emitter.scalar_data.block_plain_allowed = false;
        emitter.scalar_data.single_quoted_allowed = false;
    }
    if space_break || special_characters {
        emitter.scalar_data.block_allowed = false;
    }
    if line_breaks {
        emitter.scalar_data.flow_plain_allowed = false;
        emitter.scalar_data.block_plain_allowed = false;
    }
    if flow_indicators {
        emitter.scalar_data.flow_plain_allowed = false;
    }
    if block_indicators {
        emitter.scalar_data.block_plain_allowed = false;
    }
    true
}

/// Check if the event data is valid. Go aliases the event's comment, anchor and value slices
/// into the emitter; the port moves the comments and the scalar value out of the (owned,
/// about-to-be-deleted) event — nothing reads them from the event afterwards.
fn yaml_emitter_analyze_event(emitter: &mut YamlEmitter<'_>, event: &mut YamlEvent) -> bool {
    // Go: emitterc.go yaml_emitter_analyze_event
    emitter.anchor_data.anchor = Vec::new();
    emitter.tag_data.handle = Vec::new();
    emitter.tag_data.suffix = Vec::new();
    emitter.scalar_data.value = Vec::new();

    if !event.head_comment.is_empty() {
        emitter.head_comment = std::mem::take(&mut event.head_comment);
    }
    if !event.line_comment.is_empty() {
        emitter.line_comment = std::mem::take(&mut event.line_comment);
    }
    if !event.foot_comment.is_empty() {
        emitter.foot_comment = std::mem::take(&mut event.foot_comment);
    }
    if !event.tail_comment.is_empty() {
        emitter.tail_comment = std::mem::take(&mut event.tail_comment);
    }

    match event.typ {
        YamlEventType::Alias => {
            if !yaml_emitter_analyze_anchor(emitter, &event.anchor, true) {
                return false;
            }
        }
        YamlEventType::Scalar => {
            if !event.anchor.is_empty()
                && !yaml_emitter_analyze_anchor(emitter, &event.anchor, false)
            {
                return false;
            }
            if !event.tag.is_empty()
                && (emitter.canonical || (!event.implicit && !event.quoted_implicit))
                && !yaml_emitter_analyze_tag(emitter, &event.tag)
            {
                return false;
            }
            if !yaml_emitter_analyze_scalar(emitter, std::mem::take(&mut event.value)) {
                return false;
            }
        }
        YamlEventType::SequenceStart | YamlEventType::MappingStart => {
            if !event.anchor.is_empty()
                && !yaml_emitter_analyze_anchor(emitter, &event.anchor, false)
            {
                return false;
            }
            if !event.tag.is_empty()
                && (emitter.canonical || !event.implicit)
                && !yaml_emitter_analyze_tag(emitter, &event.tag)
            {
                return false;
            }
        }
        _ => {}
    }
    true
}

/// Write the BOM character.
fn yaml_emitter_write_bom(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_write_bom
    if !flush(emitter) {
        return false;
    }
    let pos = emitter.buffer_pos;
    emitter.buffer[pos] = 0xEF;
    emitter.buffer[pos + 1] = 0xBB;
    emitter.buffer[pos + 2] = 0xBF;
    emitter.buffer_pos += 3;
    true
}

fn yaml_emitter_write_indent(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: emitterc.go yaml_emitter_write_indent
    let mut indent = emitter.indent;
    if indent < 0 {
        indent = 0;
    }
    if (!emitter.indention
        || emitter.column > indent
        || (emitter.column == indent && !emitter.whitespace))
        && !put_break(emitter)
    {
        return false;
    }
    if emitter.foot_indent == indent && !put_break(emitter) {
        return false;
    }
    while emitter.column < indent {
        if !put(emitter, b' ') {
            return false;
        }
    }
    emitter.whitespace = true;
    //emitter.indention = true
    emitter.space_above = false;
    emitter.foot_indent = -1;
    true
}

fn yaml_emitter_write_indicator(
    emitter: &mut YamlEmitter<'_>,
    indicator: &[u8],
    need_whitespace: bool,
    is_whitespace: bool,
    is_indention: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_write_indicator
    if need_whitespace && !emitter.whitespace && !put(emitter, b' ') {
        return false;
    }
    if !write_all(emitter, indicator) {
        return false;
    }
    emitter.whitespace = is_whitespace;
    emitter.indention = emitter.indention && is_indention;
    emitter.open_ended = false;
    true
}

fn yaml_emitter_write_anchor(emitter: &mut YamlEmitter<'_>, value: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_write_anchor
    if !write_all(emitter, value) {
        return false;
    }
    emitter.whitespace = false;
    emitter.indention = false;
    true
}

fn yaml_emitter_write_tag_handle(emitter: &mut YamlEmitter<'_>, value: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_write_tag_handle
    if !emitter.whitespace && !put(emitter, b' ') {
        return false;
    }
    if !write_all(emitter, value) {
        return false;
    }
    emitter.whitespace = false;
    emitter.indention = false;
    true
}

fn yaml_emitter_write_tag_content(
    emitter: &mut YamlEmitter<'_>,
    value: &[u8],
    need_whitespace: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_write_tag_content
    if need_whitespace && !emitter.whitespace && !put(emitter, b' ') {
        return false;
    }
    let mut i = 0;
    while i < value.len() {
        let must_write = match value[i] {
            b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b',' | b'_' | b'.'
            | b'~' | b'*' | b'\'' | b'(' | b')' | b'[' | b']' => true,
            _ => is_alpha(value, i),
        };
        if must_write {
            if !write(emitter, value, &mut i) {
                return false;
            }
        } else {
            let w = width(value[i]);
            for _ in 0..w {
                let octet = value[i];
                i += 1;
                if !put(emitter, b'%') {
                    return false;
                }

                let mut c = octet >> 4;
                if c < 10 {
                    c += b'0';
                } else {
                    c += b'A' - 10;
                }
                if !put(emitter, c) {
                    return false;
                }

                c = octet & 0x0f;
                if c < 10 {
                    c += b'0';
                } else {
                    c += b'A' - 10;
                }
                if !put(emitter, c) {
                    return false;
                }
            }
        }
    }
    emitter.whitespace = false;
    emitter.indention = false;
    true
}

fn yaml_emitter_write_plain_scalar(
    emitter: &mut YamlEmitter<'_>,
    value: &[u8],
    allow_breaks: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_write_plain_scalar
    if !value.is_empty() && !emitter.whitespace && !put(emitter, b' ') {
        return false;
    }

    let mut spaces = false;
    let mut breaks = false;
    let mut i = 0;
    while i < value.len() {
        if is_space(value, i) {
            if allow_breaks
                && !spaces
                && emitter.column > emitter.best_width
                && !is_space(value, i + 1)
            {
                if !yaml_emitter_write_indent(emitter) {
                    return false;
                }
                i += width(value[i]);
            } else if !write(emitter, value, &mut i) {
                return false;
            }
            spaces = true;
        } else if is_break(value, i) {
            if !breaks && value[i] == b'\n' && !put_break(emitter) {
                return false;
            }
            if !write_break(emitter, value, &mut i) {
                return false;
            }
            //emitter.indention = true
            breaks = true;
        } else {
            if breaks && !yaml_emitter_write_indent(emitter) {
                return false;
            }
            if !write(emitter, value, &mut i) {
                return false;
            }
            emitter.indention = false;
            spaces = false;
            breaks = false;
        }
    }

    if !value.is_empty() {
        emitter.whitespace = false;
    }
    emitter.indention = false;
    if emitter.root_context {
        emitter.open_ended = true;
    }

    true
}

fn yaml_emitter_write_single_quoted_scalar(
    emitter: &mut YamlEmitter<'_>,
    value: &[u8],
    allow_breaks: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_write_single_quoted_scalar
    if !yaml_emitter_write_indicator(emitter, b"'", true, false, false) {
        return false;
    }

    let mut spaces = false;
    let mut breaks = false;
    let mut i = 0;
    while i < value.len() {
        if is_space(value, i) {
            if allow_breaks
                && !spaces
                && emitter.column > emitter.best_width
                && i > 0
                && i < value.len() - 1
                && !is_space(value, i + 1)
            {
                if !yaml_emitter_write_indent(emitter) {
                    return false;
                }
                i += width(value[i]);
            } else if !write(emitter, value, &mut i) {
                return false;
            }
            spaces = true;
        } else if is_break(value, i) {
            if !breaks && value[i] == b'\n' && !put_break(emitter) {
                return false;
            }
            if !write_break(emitter, value, &mut i) {
                return false;
            }
            //emitter.indention = true
            breaks = true;
        } else {
            if breaks && !yaml_emitter_write_indent(emitter) {
                return false;
            }
            if value[i] == b'\'' && !put(emitter, b'\'') {
                return false;
            }
            if !write(emitter, value, &mut i) {
                return false;
            }
            emitter.indention = false;
            spaces = false;
            breaks = false;
        }
    }
    if !yaml_emitter_write_indicator(emitter, b"'", false, false, false) {
        return false;
    }
    emitter.whitespace = false;
    emitter.indention = false;
    true
}

fn yaml_emitter_write_double_quoted_scalar(
    emitter: &mut YamlEmitter<'_>,
    value: &[u8],
    allow_breaks: bool,
) -> bool {
    // Go: emitterc.go yaml_emitter_write_double_quoted_scalar
    let mut spaces = false;
    if !yaml_emitter_write_indicator(emitter, b"\"", true, false, false) {
        return false;
    }

    let mut i = 0;
    while i < value.len() {
        if !is_printable(value, i)
            || (!emitter.unicode && !is_ascii(value, i))
            || is_bom(value, i)
            || is_break(value, i)
            || value[i] == b'"'
            || value[i] == b'\\'
        {
            let mut octet = value[i];

            let (mut w, mut v): (usize, u32) = if octet & 0x80 == 0x00 {
                (1, u32::from(octet & 0x7F))
            } else if octet & 0xE0 == 0xC0 {
                (2, u32::from(octet & 0x1F))
            } else if octet & 0xF0 == 0xE0 {
                (3, u32::from(octet & 0x0F))
            } else if octet & 0xF8 == 0xF0 {
                (4, u32::from(octet & 0x07))
            } else {
                (0, 0)
            };
            for k in 1..w {
                octet = value[i + k];
                v = (v << 6) + (u32::from(octet) & 0x3F);
            }
            i += w;

            if !put(emitter, b'\\') {
                return false;
            }

            let ok = match v {
                0x00 => put(emitter, b'0'),
                0x07 => put(emitter, b'a'),
                0x08 => put(emitter, b'b'),
                0x09 => put(emitter, b't'),
                0x0A => put(emitter, b'n'),
                0x0b => put(emitter, b'v'),
                0x0c => put(emitter, b'f'),
                0x0d => put(emitter, b'r'),
                0x1b => put(emitter, b'e'),
                0x22 => put(emitter, b'"'),
                0x5c => put(emitter, b'\\'),
                0x85 => put(emitter, b'N'),
                0xA0 => put(emitter, b'_'),
                0x2028 => put(emitter, b'L'),
                0x2029 => put(emitter, b'P'),
                _ => {
                    let mut ok;
                    if v <= 0xFF {
                        ok = put(emitter, b'x');
                        w = 2;
                    } else if v <= 0xFFFF {
                        ok = put(emitter, b'u');
                        w = 4;
                    } else {
                        ok = put(emitter, b'U');
                        w = 8;
                    }
                    let mut k = (w as isize - 1) * 4;
                    while ok && k >= 0 {
                        let digit = ((v >> k) & 0x0F) as u8;
                        if digit < 10 {
                            ok = put(emitter, digit + b'0');
                        } else {
                            ok = put(emitter, digit + b'A' - 10);
                        }
                        k -= 4;
                    }
                    ok
                }
            };
            if !ok {
                return false;
            }
            spaces = false;
        } else if is_space(value, i) {
            if allow_breaks
                && !spaces
                && emitter.column > emitter.best_width
                && i > 0
                && i < value.len() - 1
            {
                if !yaml_emitter_write_indent(emitter) {
                    return false;
                }
                if is_space(value, i + 1) && !put(emitter, b'\\') {
                    return false;
                }
                i += width(value[i]);
            } else if !write(emitter, value, &mut i) {
                return false;
            }
            spaces = true;
        } else {
            if !write(emitter, value, &mut i) {
                return false;
            }
            spaces = false;
        }
    }
    if !yaml_emitter_write_indicator(emitter, b"\"", false, false, false) {
        return false;
    }
    emitter.whitespace = false;
    emitter.indention = false;
    true
}

fn yaml_emitter_write_block_scalar_hints(emitter: &mut YamlEmitter<'_>, value: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_write_block_scalar_hints
    if is_space(value, 0) || is_break(value, 0) {
        let indent_hint = [b'0' + emitter.best_indent as u8];
        if !yaml_emitter_write_indicator(emitter, &indent_hint, false, false, false) {
            return false;
        }
    }

    emitter.open_ended = false;

    let mut chomp_hint = [0u8; 1];
    if value.is_empty() {
        chomp_hint[0] = b'-';
    } else {
        let mut i = value.len() - 1;
        while value[i] & 0xC0 == 0x80 {
            i -= 1;
        }
        if !is_break(value, i) {
            chomp_hint[0] = b'-';
        } else if i == 0 {
            chomp_hint[0] = b'+';
            emitter.open_ended = true;
        } else {
            i -= 1;
            while value[i] & 0xC0 == 0x80 {
                i -= 1;
            }
            if is_break(value, i) {
                chomp_hint[0] = b'+';
                emitter.open_ended = true;
            }
        }
    }
    if chomp_hint[0] != 0
        && !yaml_emitter_write_indicator(emitter, &chomp_hint, false, false, false)
    {
        return false;
    }
    true
}

fn yaml_emitter_write_literal_scalar(emitter: &mut YamlEmitter<'_>, value: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_write_literal_scalar
    if !yaml_emitter_write_indicator(emitter, b"|", true, false, false) {
        return false;
    }
    if !yaml_emitter_write_block_scalar_hints(emitter, value) {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }
    //emitter.indention = true
    emitter.whitespace = true;
    let mut breaks = true;
    let mut i = 0;
    while i < value.len() {
        if is_break(value, i) {
            if !write_break(emitter, value, &mut i) {
                return false;
            }
            //emitter.indention = true
            breaks = true;
        } else {
            if breaks && !yaml_emitter_write_indent(emitter) {
                return false;
            }
            if !write(emitter, value, &mut i) {
                return false;
            }
            emitter.indention = false;
            breaks = false;
        }
    }

    true
}

fn yaml_emitter_write_folded_scalar(emitter: &mut YamlEmitter<'_>, value: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_write_folded_scalar
    if !yaml_emitter_write_indicator(emitter, b">", true, false, false) {
        return false;
    }
    if !yaml_emitter_write_block_scalar_hints(emitter, value) {
        return false;
    }
    if !yaml_emitter_process_line_comment(emitter) {
        return false;
    }

    //emitter.indention = true
    emitter.whitespace = true;

    let mut breaks = true;
    let mut leading_spaces = true;
    let mut i = 0;
    while i < value.len() {
        if is_break(value, i) {
            if !breaks && !leading_spaces && value[i] == b'\n' {
                // Go scans from 0 here, not from i (emitterc.go:1946); kept as in Go.
                let mut k = 0;
                while is_break(value, k) {
                    k += width(value[k]);
                }
                if !is_blankz(value, k) && !put_break(emitter) {
                    return false;
                }
            }
            if !write_break(emitter, value, &mut i) {
                return false;
            }
            //emitter.indention = true
            breaks = true;
        } else {
            if breaks {
                if !yaml_emitter_write_indent(emitter) {
                    return false;
                }
                leading_spaces = is_blank(value, i);
            }
            if !breaks
                && is_space(value, i)
                && !is_space(value, i + 1)
                && emitter.column > emitter.best_width
            {
                if !yaml_emitter_write_indent(emitter) {
                    return false;
                }
                i += width(value[i]);
            } else if !write(emitter, value, &mut i) {
                return false;
            }
            emitter.indention = false;
            breaks = false;
        }
    }
    true
}

fn yaml_emitter_write_comment(emitter: &mut YamlEmitter<'_>, comment: &[u8]) -> bool {
    // Go: emitterc.go yaml_emitter_write_comment
    let mut breaks = false;
    let mut pound = false;
    let mut i = 0;
    while i < comment.len() {
        if is_break(comment, i) {
            if !write_break(emitter, comment, &mut i) {
                return false;
            }
            //emitter.indention = true
            breaks = true;
            pound = false;
        } else {
            if breaks && !yaml_emitter_write_indent(emitter) {
                return false;
            }
            if !pound {
                if comment[i] != b'#' && (!put(emitter, b'#') || !put(emitter, b' ')) {
                    return false;
                }
                pound = true;
            }
            if !write(emitter, comment, &mut i) {
                return false;
            }
            emitter.indention = false;
            breaks = false;
        }
    }
    if !breaks && !put_break(emitter) {
        return false;
    }

    emitter.whitespace = true;
    //emitter.indention = true
    true
}

#[cfg(test)]
mod tests {
    //! Event sequences → exact bytes. Expected bytes come from yaml.v3 v3.0.1: the
    //! `golden_*` tests read the probe goldens (testdata/golden), the rest were produced by
    //! driving yaml.v3's own `yaml_emitter_emit` with the same event sequences (best_indent 4
    //! and unicode on, as `yaml.Marshal` sets them, unless a case configures otherwise).
    use super::*;
    use crate::api::{
        yaml_alias_event_initialize, yaml_document_end_event_initialize,
        yaml_document_start_event_initialize, yaml_emitter_initialize, yaml_emitter_set_break,
        yaml_emitter_set_canonical, yaml_emitter_set_output_string, yaml_emitter_set_output_writer,
        yaml_emitter_set_unicode, yaml_emitter_set_width, yaml_mapping_end_event_initialize,
        yaml_mapping_start_event_initialize, yaml_scalar_event_initialize,
        yaml_sequence_end_event_initialize, yaml_sequence_start_event_initialize,
        yaml_stream_end_event_initialize, yaml_stream_start_event_initialize,
    };
    use base64::Engine as _;

    fn ss_enc(encoding: YamlEncoding) -> YamlEvent {
        let mut e = YamlEvent::default();
        yaml_stream_start_event_initialize(&mut e, encoding);
        e
    }
    fn ss() -> YamlEvent {
        ss_enc(YamlEncoding::Utf8)
    }
    fn se() -> YamlEvent {
        let mut e = YamlEvent::default();
        yaml_stream_end_event_initialize(&mut e);
        e
    }
    fn ds_with(
        version: Option<(i8, i8)>,
        tags: Vec<YamlTagDirective>,
        implicit: bool,
    ) -> YamlEvent {
        let mut e = YamlEvent::default();
        let vd = version.map(|(major, minor)| YamlVersionDirective { major, minor });
        yaml_document_start_event_initialize(&mut e, vd, tags, implicit);
        e
    }
    fn ds() -> YamlEvent {
        ds_with(None, Vec::new(), true)
    }
    fn dsx() -> YamlEvent {
        ds_with(None, Vec::new(), false)
    }
    fn td(handle: &str, prefix: &str) -> YamlTagDirective {
        YamlTagDirective {
            handle: handle.as_bytes().to_vec(),
            prefix: prefix.as_bytes().to_vec(),
        }
    }
    fn de() -> YamlEvent {
        let mut e = YamlEvent::default();
        yaml_document_end_event_initialize(&mut e, true);
        e
    }
    fn dex() -> YamlEvent {
        let mut e = YamlEvent::default();
        yaml_document_end_event_initialize(&mut e, false);
        e
    }
    fn sc_imp(v: &str, plain: bool, quoted: bool, style: YamlScalarStyle) -> YamlEvent {
        let mut e = YamlEvent::default();
        assert!(yaml_scalar_event_initialize(
            &mut e,
            Vec::new(),
            Vec::new(),
            v.as_bytes().to_vec(),
            plain,
            quoted,
            style
        ));
        e
    }
    fn pl(v: &str) -> YamlEvent {
        sc_imp(v, true, true, YamlScalarStyle::Plain)
    }
    fn sq(v: &str) -> YamlEvent {
        sc_imp(v, true, true, YamlScalarStyle::SingleQuoted)
    }
    fn dq(v: &str) -> YamlEvent {
        sc_imp(v, true, true, YamlScalarStyle::DoubleQuoted)
    }
    fn lit(v: &str) -> YamlEvent {
        sc_imp(v, true, true, YamlScalarStyle::Literal)
    }
    fn fold(v: &str) -> YamlEvent {
        sc_imp(v, true, true, YamlScalarStyle::Folded)
    }
    fn sct(v: &str, tag: &str, style: YamlScalarStyle) -> YamlEvent {
        let mut e = YamlEvent::default();
        assert!(yaml_scalar_event_initialize(
            &mut e,
            Vec::new(),
            tag.as_bytes().to_vec(),
            v.as_bytes().to_vec(),
            false,
            false,
            style
        ));
        e
    }
    fn seqs(flow: bool, tag: &str) -> YamlEvent {
        let mut e = YamlEvent::default();
        let style = if flow {
            YamlSequenceStyle::Flow
        } else {
            YamlSequenceStyle::Block
        };
        assert!(yaml_sequence_start_event_initialize(
            &mut e,
            Vec::new(),
            tag.as_bytes().to_vec(),
            tag.is_empty(),
            style
        ));
        e
    }
    fn seq() -> YamlEvent {
        seqs(false, "")
    }
    fn fseq() -> YamlEvent {
        seqs(true, "")
    }
    fn seq_end() -> YamlEvent {
        let mut e = YamlEvent::default();
        assert!(yaml_sequence_end_event_initialize(&mut e));
        e
    }
    fn maps(flow: bool, tag: &str) -> YamlEvent {
        let mut e = YamlEvent::default();
        let style = if flow {
            YamlMappingStyle::Flow
        } else {
            YamlMappingStyle::Block
        };
        yaml_mapping_start_event_initialize(
            &mut e,
            Vec::new(),
            tag.as_bytes().to_vec(),
            tag.is_empty(),
            style,
        );
        e
    }
    fn mp() -> YamlEvent {
        maps(false, "")
    }
    fn fmp() -> YamlEvent {
        maps(true, "")
    }
    fn map_end() -> YamlEvent {
        let mut e = YamlEvent::default();
        yaml_mapping_end_event_initialize(&mut e);
        e
    }
    fn al(anchor: &str) -> YamlEvent {
        let mut e = YamlEvent::default();
        assert!(yaml_alias_event_initialize(
            &mut e,
            anchor.as_bytes().to_vec()
        ));
        e
    }
    fn anc(mut e: YamlEvent, anchor: &str) -> YamlEvent {
        e.anchor = anchor.as_bytes().to_vec();
        e
    }
    fn hc(mut e: YamlEvent, c: &str) -> YamlEvent {
        e.head_comment = c.as_bytes().to_vec();
        e
    }
    fn lc(mut e: YamlEvent, c: &str) -> YamlEvent {
        e.line_comment = c.as_bytes().to_vec();
        e
    }
    fn fc(mut e: YamlEvent, c: &str) -> YamlEvent {
        e.foot_comment = c.as_bytes().to_vec();
        e
    }
    fn tc(mut e: YamlEvent, c: &str) -> YamlEvent {
        e.tail_comment = c.as_bytes().to_vec();
        e
    }
    /// STREAM-START, implicit DOCUMENT-START, `events`, implicit DOCUMENT-END, STREAM-END.
    fn doc(events: Vec<YamlEvent>) -> Vec<YamlEvent> {
        let mut out = vec![ss(), ds()];
        out.extend(events);
        out.push(de());
        out.push(se());
        out
    }

    type Cfg = fn(&mut YamlEmitter<'_>);

    fn no_cfg(_: &mut YamlEmitter<'_>) {}

    /// Emit `events` into a string-output emitter set up like yaml.v3's `newEncoder` +
    /// `init` (unicode on, best_indent 4), then `cfg`. Returns the output and the problem of
    /// the first failing emit ("" if none).
    fn run(cfg: Cfg, events: Vec<YamlEvent>) -> (Vec<u8>, String) {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_string(&mut e);
        yaml_emitter_set_unicode(&mut e, true);
        e.best_indent = 4;
        cfg(&mut e);
        let mut problem = String::new();
        for mut event in events {
            if !yaml_emitter_emit(&mut e, &mut event) {
                assert_eq!(e.error, YamlErrorType::Emitter);
                problem = e.problem.clone();
                break;
            }
        }
        (e.output_buffer, problem)
    }

    #[track_caller]
    fn check(name: &str, cfg: Cfg, events: Vec<YamlEvent>, want: &[u8]) {
        let (got, problem) = run(cfg, events);
        assert_eq!(problem, "", "{name}: unexpected emitter error");
        assert_eq!(
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(want),
            "{name}"
        );
        assert_eq!(got, want, "{name}");
    }

    #[track_caller]
    fn check_err(name: &str, events: Vec<YamlEvent>, want: &[u8], want_problem: &str) {
        let (got, problem) = run(no_cfg, events);
        assert_eq!(problem, want_problem, "{name}");
        assert_eq!(got, want, "{name}");
    }

    fn rep(s: &str, n: usize) -> String {
        s.repeat(n)
    }

    #[test]
    fn block_and_flow_collections() {
        check(
            "block_mapping",
            no_cfg,
            doc(vec![mp(), pl("a"), pl("1"), map_end()]),
            b"a: 1\n",
        );
        let nested = || {
            doc(vec![
                mp(),
                pl("a"),
                mp(),
                pl("b"),
                pl("c"),
                map_end(),
                pl("d"),
                seq(),
                pl("e"),
                pl("f"),
                seq_end(),
                map_end(),
            ])
        };
        check(
            "nested_indent4",
            no_cfg,
            nested(),
            b"a:\n    b: c\nd:\n    - e\n    - f\n",
        );
        check(
            "nested_indent2",
            |e| e.best_indent = 2,
            nested(),
            b"a:\n  b: c\nd:\n  - e\n  - f\n",
        );
        check(
            "seq_of_maps_and_seqs",
            no_cfg,
            doc(vec![
                seq(),
                mp(),
                pl("a"),
                pl("1"),
                pl("b"),
                pl("2"),
                map_end(),
                seq(),
                pl("x"),
                pl("y"),
                seq_end(),
                seq_end(),
            ]),
            b"- a: 1\n  b: 2\n- - x\n  - y\n",
        );
        check(
            "flow_collections",
            no_cfg,
            doc(vec![
                mp(),
                pl("a"),
                fseq(),
                pl("1"),
                pl("2"),
                seq_end(),
                pl("b"),
                fmp(),
                pl("c"),
                pl("d"),
                pl("e"),
                pl(""),
                map_end(),
                pl("e"),
                seq(),
                seq_end(),
                pl("f"),
                mp(),
                map_end(),
                map_end(),
            ]),
            b"a: [1, 2]\nb: {c: d, e: ''}\ne: []\nf: {}\n",
        );
        check(
            "complex_key",
            no_cfg,
            doc(vec![
                mp(),
                mp(),
                pl("a"),
                pl("b"),
                map_end(),
                pl("c"),
                seq(),
                pl("x"),
                seq_end(),
                pl("d"),
                map_end(),
            ]),
            b"?   a: b\n: c\n?   - x\n: d\n",
        );
        // A key longer than 128 bytes is not a simple key.
        check(
            "flow_complex_key",
            no_cfg,
            doc(vec![
                fmp(),
                fseq(),
                pl("a"),
                seq_end(),
                pl("b"),
                pl(&rep("k", 130)),
                pl("c"),
                map_end(),
            ]),
            b"{? [a] : b, ? kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk : c}\n",
        );
    }

    #[test]
    fn scalar_styles() {
        check(
            "scalar_styles",
            no_cfg,
            doc(vec![
                mp(),
                pl("plain"),
                pl(""),
                pl("single"),
                sq("it's"),
                pl("double"),
                dq("x\ty\x01é\"\\"),
                pl("lit_clip"),
                lit("l1\nl2\n"),
                pl("lit_strip"),
                lit("l1\nl2"),
                pl("lit_keep"),
                lit("l1\n\n"),
                pl("fold"),
                fold("f1 f2\nf3\n"),
                map_end(),
            ]),
            b"plain:\nsingle: 'it''s'\ndouble: \"x\\ty\\x01\xc3\xa9\\\"\\\\\"\nlit_clip: |\n    l1\n    l2\nlit_strip: |-\n    l1\n    l2\nlit_keep: |+\n    l1\n\nfold: >\n    f1 f2\n\n    f3\n\n",
        );
        check(
            "plain_needs_quotes",
            no_cfg,
            doc(vec![
                mp(),
                pl("k1"),
                pl("- a"),
                pl("k2"),
                pl("a: b"),
                pl("k3"),
                pl("#x"),
                pl("k4"),
                pl("x #y"),
                pl("k5"),
                pl("---"),
                pl("k6"),
                pl("a\tb"),
                pl("k7"),
                pl(" lead"),
                pl("k8"),
                pl("trail "),
                pl("k9"),
                pl("a\nb"),
                pl("k10"),
                pl("x: "),
                pl("k11"),
                pl("a,b"),
                pl(""),
                pl("empty key"),
                map_end(),
            ]),
            b"k1: '- a'\nk2: 'a: b'\nk3: '#x'\nk4: 'x #y'\nk5: '---'\nk6: \"a\\tb\"\nk7: ' lead'\nk8: 'trail '\nk9: 'a\n\n    b'\nk10: 'x: '\nk11: a,b\n'': empty key\n",
        );
        // A multi-line key is not a simple key: `? ` form.
        check(
            "multiline_key",
            no_cfg,
            doc(vec![
                mp(),
                pl("a\nb"),
                pl("v"),
                lit("c\nd"),
                pl("w"),
                map_end(),
            ]),
            b"? 'a\n\n    b'\n: v\n? |-\n    c\n    d\n: w\n",
        );
        check(
            "plain_breaks",
            no_cfg,
            doc(vec![
                seq(),
                sct("a\nb\n\nc", "tag:yaml.org,2002:str", YamlScalarStyle::Plain),
                sct(
                    "x\n y",
                    "tag:yaml.org,2002:str",
                    YamlScalarStyle::SingleQuoted,
                ),
                seq_end(),
            ]),
            b"- !!str 'a\n\n  b\n\n\n  c'\n- !!str \"x\\n y\"\n",
        );
        check(
            "single_quoted_breaks",
            no_cfg,
            doc(vec![seq(), sq("a\nb\n\nc"), sq("o'k"), seq_end()]),
            b"- 'a\n\n  b\n\n\n  c'\n- 'o''k'\n",
        );
        // Empty block scalars are not block-allowed: double-quoted.
        check(
            "folded_variants",
            no_cfg,
            doc(vec![
                seq(),
                fold("a\n\nb\n"),
                fold("a\n  b\nc"),
                fold("x\n"),
                fold(""),
                lit(""),
                seq_end(),
            ]),
            b"- >\n  a\n\n\n  b\n\n- >-\n  a\n\n    b\n  c\n- >\n  x\n\n- \"\"\n- \"\"\n",
        );
    }

    #[test]
    fn block_scalar_indentation_hint() {
        // Text starting with a space (or a break) gets the indentation indicator
        // (best_indent); a leading tab does not (is_space checks ' ' only).
        check(
            "block_hint_indent4",
            no_cfg,
            doc(vec![
                mp(),
                pl("a"),
                lit(" x\ny"),
                pl("b"),
                lit("\tz\n"),
                pl("c"),
                fold(" w"),
                map_end(),
            ]),
            b"a: |4-\n     x\n    y\nb: |\n    \tz\nc: >4-\n     w\n",
        );
        check(
            "block_hint_indent2",
            |e| e.best_indent = 2,
            doc(vec![mp(), pl("a"), lit(" x\ny"), map_end()]),
            b"a: |2-\n   x\n  y\n",
        );
        check(
            "block_hint_root",
            no_cfg,
            doc(vec![lit("  root\n")]),
            b"|4\n      root\n",
        );
        check(
            "indent_9",
            |e| e.best_indent = 9,
            doc(vec![
                mp(),
                pl("a"),
                mp(),
                pl("b"),
                lit(" c"),
                map_end(),
                map_end(),
            ]),
            b"a:\n         b: |9-\n                   c\n",
        );
    }

    #[test]
    fn comments() {
        check(
            "comments_block",
            no_cfg,
            vec![
                ss(),
                hc(ds(), "# doc head"),
                mp(),
                hc(pl("a"), "# key head"),
                lc(pl("1"), "# line"),
                pl("b"),
                pl("2"),
                map_end(),
                fc(de(), "# doc foot"),
                se(),
            ],
            b"# doc head\n\n# key head\na: 1 # line\nb: 2\n\n# doc foot\n",
        );
        check(
            "comments_tail_and_foot",
            no_cfg,
            doc(vec![
                mp(),
                pl("a"),
                mp(),
                pl("x"),
                pl("1"),
                map_end(),
                tc(pl("b"), "# foot of a"),
                pl("2"),
                fc(tc(map_end(), "# foot of b"), "# map foot"),
            ]),
            b"a:\n    x: 1\n# foot of a\n\nb: 2\n# foot of b\n\n# map foot\n",
        );
        check(
            "comments_seq",
            no_cfg,
            doc(vec![
                seq(),
                hc(pl("a"), "# item head"),
                lc(pl("b"), "# b line"),
                fc(pl("c"), "# c foot"),
                lc(fc(seq_end(), "# seq foot"), "# seq line"),
            ]),
            b"# item head\n- a\n- b # b line\n- c\n# c foot\n\n# seq foot\n",
        );
        // A comment line without `#` gets "# "; breaks inside a comment re-indent.
        check(
            "comments_multiline",
            no_cfg,
            doc(vec![hc(lc(pl("v"), "line one"), "# h1\n\n# h2")]),
            b"# h1\n\n# h2\nv # line one\n...\n",
        );
        check(
            "comments_flow_seq",
            no_cfg,
            doc(vec![
                mp(),
                pl("k"),
                fseq(),
                lc(pl("a"), "# a"),
                pl("b"),
                seq_end(),
                map_end(),
            ]),
            b"k: [a, # a\n    b]\n",
        );
        check(
            "comments_flow_map",
            no_cfg,
            doc(vec![
                mp(),
                pl("k"),
                fmp(),
                pl("a"),
                lc(pl("1"), "# one"),
                pl("b"),
                pl("2"),
                map_end(),
                map_end(),
            ]),
            b"k: {a: 1, # one\n    b: 2}\n",
        );
        check(
            "comments_flow_map_end_head",
            no_cfg,
            doc(vec![fmp(), pl("a"), pl("1"), hc(map_end(), "# end head")]),
            b"{a: 1,\n    # end head\n}\n",
        );
        check(
            "alias_comments",
            no_cfg,
            doc(vec![
                seq(),
                anc(pl("1"), "x"),
                hc(lc(fc(al("x"), "# af"), "# al"), "# ah"),
                seq_end(),
            ]),
            b"- &x 1\n# ah\n- *x # al\n# af\n",
        );
    }

    #[test]
    fn key_line_comments() {
        check(
            "key_line_comment_scalar",
            no_cfg,
            doc(vec![mp(), lc(pl("k"), "# kc"), pl("v"), map_end()]),
            b"k: v # kc\n",
        );
        // The value's own line comment wins; the key's is lost (as in Go).
        check(
            "key_line_comment_lost",
            no_cfg,
            doc(vec![
                mp(),
                lc(pl("k"), "# kc"),
                lc(pl("v"), "# vc"),
                map_end(),
            ]),
            b"k: v # vc\n",
        );
        check(
            "key_line_comment_block",
            no_cfg,
            doc(vec![
                mp(),
                lc(pl("k"), "# kc"),
                mp(),
                pl("a"),
                pl("b"),
                map_end(),
                lc(pl("s"), "# sc"),
                seq(),
                pl("x"),
                seq_end(),
                map_end(),
            ]),
            b"k: # kc\n    a: b\ns: # sc\n    - x\n",
        );
        // Flow values drop the key's line comment (emitterc.go:828).
        check(
            "key_line_comment_flow_seq",
            no_cfg,
            doc(vec![
                mp(),
                lc(pl("k"), "# kc"),
                fseq(),
                pl("a"),
                seq_end(),
                map_end(),
            ]),
            b"k: [a]\n",
        );
        check(
            "key_line_comment_flow_map",
            no_cfg,
            doc(vec![
                mp(),
                lc(pl("k"), "# kc"),
                fmp(),
                pl("a"),
                pl("b"),
                map_end(),
                map_end(),
            ]),
            b"k: {a: b}\n",
        );
    }

    #[test]
    fn documents_and_open_ended() {
        check(
            "multi_doc",
            no_cfg,
            vec![
                ss(),
                ds(),
                pl("a"),
                de(),
                ds(),
                mp(),
                pl("b"),
                pl("c"),
                map_end(),
                dex(),
                ds(),
                pl("d"),
                de(),
                se(),
            ],
            b"a\n---\nb: c\n...\n---\nd\n...\n",
        );
        check(
            "explicit_doc_start",
            no_cfg,
            vec![ss(), dsx(), pl("a"), de(), se()],
            b"---\na\n...\n",
        );
        // A plain root scalar leaves the stream open-ended: STREAM-END writes "...".
        // (yaml.v3's encoder.finish clears open_ended first; the emitter alone does not.)
        check(
            "root_plain_open_ended",
            no_cfg,
            vec![ss(), ds(), pl("a"), de(), se()],
            b"a\n...\n",
        );
        check(
            "root_empty_plain",
            no_cfg,
            vec![ss(), ds(), pl(""), de(), se()],
            b"\n...\n",
        );
        check(
            "root_literal_keep",
            no_cfg,
            vec![ss(), ds(), lit("a\n\n"), de(), se()],
            b"|+\n    a\n\n",
        );
        check(
            "directives",
            no_cfg,
            vec![
                ss(),
                ds_with(Some((1, 1)), vec![td("!e!", "tag:example.com,2000:")], true),
                sct("x", "tag:example.com,2000:foo", YamlScalarStyle::Plain),
                de(),
                ds_with(None, vec![td("!f!", "tag:f.org,1:")], true),
                sct("y", "tag:example.com,2000:foo", YamlScalarStyle::Plain),
                de(),
                se(),
            ],
            b"%YAML 1.1\n%TAG !e! tag:example.com,2000:\n---\n!e!foo x\n...\n%TAG !f! tag:f.org,1:\n---\n!<tag:example.com,2000:foo> y\n...\n",
        );
        check(
            "open_ended_then_directive",
            no_cfg,
            vec![
                ss(),
                ds(),
                pl("a"),
                de(),
                ds_with(Some((1, 1)), Vec::new(), true),
                pl("b"),
                de(),
                se(),
            ],
            b"a\n...\n%YAML 1.1\n---\nb\n...\n",
        );
    }

    #[test]
    fn anchors_aliases_and_tags() {
        check(
            "anchors_aliases",
            no_cfg,
            doc(vec![
                mp(),
                pl("a"),
                anc(pl("1"), "x"),
                pl("b"),
                al("x"),
                pl("c"),
                anc(seq(), "s"),
                pl("1"),
                seq_end(),
                pl("d"),
                anc(mp(), "m"),
                pl("k"),
                pl("v"),
                map_end(),
                pl("e"),
                anc(fseq(), "f"),
                seq_end(),
                al("m"),
                pl("alias key"),
                map_end(),
            ]),
            b"a: &x 1\nb: *x\nc: &s\n    - 1\nd: &m\n    k: v\ne: &f []\n*m: alias key\n",
        );
        check(
            "tags",
            no_cfg,
            doc(vec![
                mp(),
                pl("a"),
                sct("1", "tag:yaml.org,2002:str", YamlScalarStyle::Plain),
                pl("b"),
                sct("x", "!foo", YamlScalarStyle::Plain),
                pl("c"),
                sct("v", "tag:example.com,2000:x", YamlScalarStyle::Plain),
                pl("d"),
                sct("w", "!a b{é}", YamlScalarStyle::Plain),
                pl("e"),
                seqs(false, "!bar"),
                pl("1"),
                seq_end(),
                pl("f"),
                maps(true, "tag:yaml.org,2002:map"),
                map_end(),
                pl("g"),
                sct("q", "tag:yaml.org,2002:str", YamlScalarStyle::DoubleQuoted),
                pl("h"),
                sc_imp("p", false, true, YamlScalarStyle::Plain),
                pl("i"),
                sc_imp("p", true, false, YamlScalarStyle::DoubleQuoted),
                map_end(),
            ]),
            b"a: !!str 1\nb: !foo x\nc: !<tag:example.com,2000:x> v\nd: !a%20b%7B%C3%A9%7D w\ne: !bar\n    - 1\nf: !!map {}\ng: !!str \"q\"\nh: 'p'\ni: ! \"p\"\n",
        );
    }

    #[test]
    fn width_and_indent_settings() {
        check(
            "width_20",
            |e| yaml_emitter_set_width(e, 20),
            doc(vec![
                mp(),
                pl("p"),
                pl("aaa bbb ccc ddd eee fff ggg hhh"),
                pl("s"),
                sq("aaa bbb ccc ddd eee fff ggg hhh"),
                pl("d"),
                dq("aaa bbb ccc ddd  eee fff ggg hhh"),
                pl("f"),
                fold("aaa bbb ccc ddd eee fff ggg hhh\nnext line here and more\n"),
                pl("q"),
                fseq(),
                pl("one"),
                pl("two"),
                pl("three"),
                pl("four"),
                pl("five"),
                pl("six"),
                seq_end(),
                pl("m"),
                fmp(),
                pl("one"),
                pl("1"),
                pl("two"),
                pl("2"),
                pl("three"),
                pl("3"),
                pl("four"),
                pl("4"),
                map_end(),
                map_end(),
            ]),
            b"p: aaa bbb ccc ddd eee\n    fff ggg hhh\ns: 'aaa bbb ccc ddd eee\n    fff ggg hhh'\nd: \"aaa bbb ccc ddd  eee\n    fff ggg hhh\"\nf: >\n    aaa bbb ccc ddd eee\n    fff ggg hhh\n\n    next line here and\n    more\n\nq: [one, two, three, four,\n    five, six]\nm: {one: 1, two: 2, three: 3,\n    four: 4}\n",
        );
        // best_width -1 (yaml_emitter_initialize) → 1<<31 - 1: never wraps.
        check(
            "width_unlimited",
            no_cfg,
            doc(vec![
                mp(),
                pl("p"),
                pl(&(rep("word ", 40) + "end")),
                map_end(),
            ]),
            b"p: word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word end\n",
        );
        // A width in 0..=2*indent becomes 80.
        check(
            "width_small_becomes_80",
            |e| {
                e.best_indent = 2;
                yaml_emitter_set_width(e, 3);
            },
            doc(vec![seq(), pl(&(rep("word ", 30) + "end")), seq_end()]),
            b"- word word word word word word word word word word word word word word word word\n  word word word word word word word word word word word word word word end\n",
        );
        check(
            "double_quoted_wrap_spaces",
            |e| yaml_emitter_set_width(e, 10),
            doc(vec![dq("aaaaaaaaaaa  bbbbbbbbbbbb cc")]),
            b"\"aaaaaaaaaaa\n    \\ bbbbbbbbbbbb\n    cc\"\n",
        );
        // best_indent outside 2..=9 falls back to 2.
        let nested = || {
            doc(vec![
                mp(),
                pl("a"),
                mp(),
                pl("b"),
                pl("c"),
                map_end(),
                map_end(),
            ])
        };
        check(
            "indent_1_falls_back",
            |e| e.best_indent = 1,
            nested(),
            b"a:\n  b: c\n",
        );
        check(
            "indent_10_falls_back",
            |e| e.best_indent = 10,
            nested(),
            b"a:\n  b: c\n",
        );
    }

    #[test]
    fn canonical_breaks_encoding_and_unicode() {
        check(
            "canonical",
            |e| yaml_emitter_set_canonical(e, true),
            vec![
                ss(),
                ds(),
                mp(),
                sct("a", "tag:yaml.org,2002:str", YamlScalarStyle::Plain),
                seqs(false, "tag:yaml.org,2002:seq"),
                pl("1"),
                pl("2"),
                seq_end(),
                pl("b"),
                mp(),
                map_end(),
                map_end(),
                de(),
                se(),
            ],
            b"---\n{\n    ? !!str \"a\"\n    : !!seq [\n        \"1\",\n        \"2\",\n    ],\n    ? \"b\"\n    : {},\n}\n",
        );
        check(
            "crlf_breaks",
            |e| yaml_emitter_set_break(e, YamlBreak::Crln),
            doc(vec![
                mp(),
                pl("a"),
                lit("x\ny\n"),
                pl("b"),
                seq(),
                pl("c"),
                seq_end(),
                map_end(),
            ]),
            b"a: |\r\n    x\r\n    y\r\nb:\r\n    - c\r\n",
        );
        check(
            "cr_breaks",
            |e| yaml_emitter_set_break(e, YamlBreak::Cr),
            doc(vec![mp(), pl("a"), pl("b"), map_end()]),
            b"a: b\r",
        );
        // A non-UTF-8 stream encoding only adds the BOM (yaml.v3 does not transcode).
        check(
            "utf16_bom",
            no_cfg,
            vec![ss_enc(YamlEncoding::Utf16le), ds(), pl("a"), de(), se()],
            b"\xef\xbb\xbfa\n...\n",
        );
        check(
            "unicode_off",
            |e| yaml_emitter_set_unicode(e, false),
            doc(vec![
                seq(),
                pl("é"),
                pl("a\u{2028}b"),
                pl("😀"),
                pl("\u{85}\u{a0}\x1b\x07\x08\x0b\x0c\r"),
                seq_end(),
            ]),
            b"- \"\\xE9\"\n- \"a\\Lb\"\n- \"\\U0001F600\"\n- \"\\N\\_\\e\\a\\b\\v\\f\\r\"\n",
        );
        check(
            "unicode_on_specials",
            no_cfg,
            doc(vec![
                seq(),
                pl("é😀"),
                pl("\u{feff}bom"),
                pl("a\u{2028}b"),
                pl("\x7f"),
                seq_end(),
            ]),
            b"- \"\xc3\xa9\\U0001F600\"\n- \"\\uFEFF\\x62\\x6F\\x6D\"\n- 'a\xe2\x80\xa8  b'\n- \"\\x7F\"\n",
        );
    }

    #[test]
    fn output_longer_than_the_buffer() {
        check(
            "long_scalar_flush",
            no_cfg,
            doc(vec![
                mp(),
                pl("k"),
                pl(&rep("abcdefghij", 30)),
                pl("u"),
                pl(&rep("é€😀", 40)),
                map_end(),
            ]),
            b"k: abcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghij\nu: \"\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\xc3\xa9\xe2\x82\xac\\U0001F600\"\n",
        );
        let mut events = vec![seq()];
        for i in 0..40 {
            events.push(pl(&format!("item-{i:02}")));
        }
        events.push(seq_end());
        check("long_seq_flush", no_cfg, doc(events), b"- item-00\n- item-01\n- item-02\n- item-03\n- item-04\n- item-05\n- item-06\n- item-07\n- item-08\n- item-09\n- item-10\n- item-11\n- item-12\n- item-13\n- item-14\n- item-15\n- item-16\n- item-17\n- item-18\n- item-19\n- item-20\n- item-21\n- item-22\n- item-23\n- item-24\n- item-25\n- item-26\n- item-27\n- item-28\n- item-29\n- item-30\n- item-31\n- item-32\n- item-33\n- item-34\n- item-35\n- item-36\n- item-37\n- item-38\n- item-39\n");
    }

    #[test]
    fn emitter_errors() {
        check_err(
            "err_expected_stream_start",
            vec![ds(), pl("a")],
            b"",
            "expected STREAM-START",
        );
        check_err(
            "err_nothing_after_stream_end",
            vec![ss(), se(), ss()],
            b"",
            "expected nothing after STREAM-END",
        );
        check_err(
            "err_expected_doc_start",
            vec![ss(), pl("a")],
            b"",
            "expected DOCUMENT-START or STREAM-END",
        );
        check_err(
            "err_expected_doc_end",
            vec![ss(), ds(), pl("a"), pl("b")],
            b"",
            "expected DOCUMENT-END",
        );
        check_err(
            "err_expected_node",
            vec![ss(), ds(), de()],
            b"",
            "expected SCALAR, SEQUENCE-START, MAPPING-START, or ALIAS, but got document end",
        );
        check_err(
            "err_anchor_alnum",
            vec![ss(), ds(), anc(pl("a"), "a b")],
            b"",
            "anchor value must contain alphanumerical characters only",
        );
        check_err(
            "err_alias_empty",
            vec![ss(), ds(), al("")],
            b"",
            "alias value must not be empty",
        );
        check_err(
            "err_alias_alnum",
            vec![ss(), ds(), al("x.y")],
            b"",
            "alias value must contain alphanumerical characters only",
        );
        check_err(
            "err_seq_anchor_alnum",
            vec![ss(), ds(), anc(seq(), "!"), seq_end(), de()],
            b"",
            "anchor value must contain alphanumerical characters only",
        );
        check_err(
            "err_no_tag_no_implicit",
            vec![
                ss(),
                ds(),
                sc_imp("a", false, false, YamlScalarStyle::Plain),
            ],
            b"",
            "neither tag nor implicit flags are specified",
        );
        check_err(
            "err_version",
            vec![ss(), ds_with(Some((1, 2)), Vec::new(), true), pl("a")],
            b"",
            "incompatible %YAML directive",
        );
        let tag_err = |handle: &str, prefix: &str, problem: &str| {
            check_err(
                handle,
                vec![ss(), ds_with(None, vec![td(handle, prefix)], true), pl("a")],
                b"",
                problem,
            );
        };
        tag_err("", "p", "tag handle must not be empty");
        tag_err("x!", "p", "tag handle must start with '!'");
        tag_err("!x", "p", "tag handle must end with '!'");
        tag_err(
            "!a.b!",
            "p",
            "tag handle must contain alphanumerical characters only",
        );
        tag_err("!a!", "", "tag prefix must not be empty");
        check_err(
            "err_tag_duplicate",
            vec![
                ss(),
                ds_with(None, vec![td("!a!", "p"), td("!a!", "q")], true),
                pl("a"),
            ],
            b"",
            "duplicate %TAG directive",
        );
        // Output flushed at DOCUMENT-END stays written when a later event fails.
        check_err(
            "err_partial_output",
            vec![
                ss(),
                ds(),
                mp(),
                pl("a"),
                pl("1"),
                pl("b"),
                pl("2"),
                map_end(),
                de(),
                ds_with(Some((1, 0)), Vec::new(), true),
                pl("x"),
            ],
            b"a: 1\nb: 2\n",
            "incompatible %YAML directive",
        );
    }

    #[test]
    fn analyze_tag_rejects_empty_tag() {
        // Unreachable through yaml_emitter_emit (empty tags are never analyzed) but ported.
        let mut e = YamlEmitter::default();
        assert!(!yaml_emitter_analyze_tag(&mut e, b""));
        assert_eq!(e.problem, "tag value must not be empty");
    }

    #[test]
    fn analyze_scalar_flags() {
        let analyze = |v: &str| {
            let mut e = YamlEmitter {
                unicode: true,
                ..YamlEmitter::default()
            };
            assert!(yaml_emitter_analyze_scalar(&mut e, v.as_bytes().to_vec()));
            let d = &e.scalar_data;
            (
                d.multiline,
                d.flow_plain_allowed,
                d.block_plain_allowed,
                d.single_quoted_allowed,
                d.block_allowed,
            )
        };
        assert_eq!(analyze(""), (false, false, true, true, false));
        assert_eq!(analyze("abc"), (false, true, true, true, true));
        assert_eq!(analyze("a,b"), (false, false, true, true, true));
        assert_eq!(analyze("- a"), (false, false, false, true, true));
        assert_eq!(analyze("-a"), (false, true, true, true, true));
        assert_eq!(analyze("a #b"), (false, false, false, true, true));
        assert_eq!(analyze("a#b"), (false, true, true, true, true));
        assert_eq!(analyze("..."), (false, false, false, true, true));
        assert_eq!(analyze(" a"), (false, false, false, true, true));
        assert_eq!(analyze("a "), (false, false, false, true, false));
        assert_eq!(analyze("a\nb"), (true, false, false, true, true));
        assert_eq!(analyze("a \nb"), (true, false, false, false, false));
        assert_eq!(analyze("a\n b"), (true, false, false, false, true));
        assert_eq!(analyze("a\tb"), (false, false, false, false, true));
        assert_eq!(analyze("a\x01b"), (false, false, false, false, false));
    }

    #[test]
    fn queue_accumulates_and_drains() {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_string(&mut e);
        for mut event in [ss(), ds(), mp()] {
            assert!(yaml_emitter_emit(&mut e, &mut event));
        }
        // MAPPING-START waits for 3 more events (DOCUMENT-START was released by it).
        assert_eq!(e.events.len() - e.events_head, 1);
        for mut event in [pl("a"), pl("b")] {
            assert!(yaml_emitter_emit(&mut e, &mut event));
        }
        assert_eq!(e.events.len() - e.events_head, 3);
        let mut event = map_end();
        assert!(yaml_emitter_emit(&mut e, &mut event));
        // The caller's event was taken.
        assert_eq!(event, YamlEvent::default());
        assert_eq!(e.events.len(), 0);
        assert_eq!(e.events_head, 0);
    }

    #[test]
    fn writer_output_and_write_error() {
        let mut out = Vec::new();
        {
            let mut e = YamlEmitter::default();
            yaml_emitter_initialize(&mut e);
            yaml_emitter_set_output_writer(&mut e, Box::new(&mut out));
            e.best_indent = 4;
            for mut event in doc(vec![mp(), pl("a"), pl("1"), map_end()]) {
                assert!(yaml_emitter_emit(&mut e, &mut event));
            }
        }
        assert_eq!(out, b"a: 1\n");

        struct Failing;
        impl std::io::Write for Failing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_writer(&mut e, Box::new(Failing));
        let mut ok = true;
        for mut event in doc(vec![pl("a")]) {
            ok = yaml_emitter_emit(&mut e, &mut event);
            if !ok {
                break;
            }
        }
        assert!(!ok);
        assert_eq!(e.error, YamlErrorType::Writer);
        assert_eq!(e.problem, "write error: disk full");
    }

    /// Golden bytes: `marshal_b64` (indent 4) and `indent2_b64` (`SetIndent(2)`) of a parse
    /// golden, or `input_b64` (yaml.Marshal of the generated value) of a diff golden.
    fn golden(path: &str, key: &str) -> Vec<u8> {
        let full = format!("{}/testdata/golden/{path}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&full).expect(&full);
        let json: serde_json::Value = serde_json::from_str(&text).expect(&full);
        let b64 = json[key].as_str().expect(key);
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect(key)
    }

    #[track_caller]
    fn check_golden(path: &str, events: impl Fn() -> Vec<YamlEvent>) {
        check(path, no_cfg, events(), &golden(path, "marshal_b64"));
        check(
            path,
            |e| e.best_indent = 2,
            events(),
            &golden(path, "indent2_b64"),
        );
    }

    #[test]
    fn golden_block_scalar_hints() {
        // scalar: |4 # Comment — leading break, indent hint, line comment on the indicator.
        check_golden("parse/decode-unmarshalTests-059.json", || {
            doc(vec![
                mp(),
                pl("scalar"),
                lc(lit("\nliteral\n\n\ttext\n"), "# Comment"),
                map_end(),
            ])
        });
        check_golden("parse/decode-unmarshalTests-060.json", || {
            doc(vec![
                mp(),
                pl("scalar"),
                lc(
                    fold("\nfolded line\nnext line\n * one\n * two\n\nlast line\n"),
                    "# Comment",
                ),
                map_end(),
            ])
        });
        check_golden("parse/edge-multiline-literal.json", || {
            doc(vec![
                mp(),
                pl("a"),
                lit("one\ntwo\n"),
                pl("b"),
                lit("one\ntwo"),
                pl("c"),
                lit("one\ntwo\n\n"),
                pl("d"),
                lit("  indented\nbase\n"),
                map_end(),
            ])
        });
        // |4- for text starting with a space (indent2: |2-).
        check_golden("diff/5-7.json", || doc(vec![lit(" 1>0,\n2'X")]));
        // Diff inputs are yaml.Marshal output (indent 4): a leading break gets |4-; a leading
        // tab gets no hint; inside a sequence the hint still says 4 while the text sits at 2
        // (yaml.v3 cannot re-parse this one).
        check(
            "diff/17-6.json",
            no_cfg,
            doc(vec![lit("\n.")]),
            &golden("diff/17-6.json", "input_b64"),
        );
        check(
            "diff/17-1.json",
            no_cfg,
            doc(vec![lit("\t99Kef14\n3!")]),
            &golden("diff/17-1.json", "input_b64"),
        );
        check(
            "diff/23-19.json",
            no_cfg,
            doc(vec![seq(), lit("\n.x:3"), seq_end()]),
            &golden("diff/23-19.json", "input_b64"),
        );
    }

    #[test]
    fn golden_comments() {
        check_golden("parse/edge-comment-flow.json", || {
            doc(vec![
                mp(),
                pl("a"),
                fseq(),
                lc(pl("1"), "# one"),
                pl("2"),
                lc(seq_end(), "# after"),
                pl("b"),
                fmp(),
                pl("x"),
                lc(pl("1"), "# x"),
                pl("y"),
                pl("2"),
                map_end(),
                map_end(),
            ])
        });
        // encode.go moves a key's foot comment to the next key's tail (here the mapping
        // end's).
        check_golden("parse/edge-comment-foot.json", || {
            doc(vec![
                mp(),
                pl("a"),
                mp(),
                pl("b"),
                pl("1"),
                tc(map_end(), "# foot of b"),
                hc(pl("c"), "# foot of a"),
                pl("2"),
                map_end(),
            ])
        });
        check_golden("parse/edge-comment-line.json", || {
            doc(vec![
                mp(),
                pl("a"),
                lc(pl("1"), "# line a"),
                lc(pl("b"), "# line b key"),
                mp(),
                pl("c"),
                lc(pl("2"), "# line c"),
                map_end(),
                map_end(),
            ])
        });
        check_golden("parse/edge-comment-head-doc.json", || {
            vec![
                ss(),
                hc(ds(), "# head of document"),
                mp(),
                hc(pl("a"), "# head of a"),
                pl("1"),
                map_end(),
                de(),
                se(),
            ]
        });
    }
}
