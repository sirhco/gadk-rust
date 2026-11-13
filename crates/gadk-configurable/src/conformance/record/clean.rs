// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Go `internal/configurable/conformance/recordplugin/record_plugin.go`'s YAML post-processing:
//! `shouldPruneYAMLField`, `snakeCaseFieldMap` (+ its `init()`), `findKeyNode`,
//! `cleanYAMLNode`, `injectSchemaTitles` and `toCamelCaseTitle`, as loops over the
//! gadk-goyaml node arena with Go's in-place mutation order (Go's `*yaml.Node` is a
//! [`NodeId`] into the [`Document`]).

use std::collections::HashMap;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gadk_goyaml::{Document, Kind, NodeData, NodeId, Style};

/// Go `shouldPruneYAMLField`: whether the field `key` with value `value` is dropped.
pub fn should_prune_yaml_field(
    key: &str,
    doc: &Document,
    value: NodeId,
    in_tool_args: bool,
) -> bool {
    // Go: record_plugin.go shouldPruneYAMLField
    if key == "usermessageindex" || key == "user_message_index" {
        return false;
    }
    if !in_tool_args && (key == "property_order" || key == "response_json_schema") {
        return true;
    }
    let value = &doc[value];
    if value.kind == Kind::Scalar {
        if value.tag == "!!null" || value.value == "null" || value.value == "~" {
            return true;
        }
        if !in_tool_args {
            if (value.tag == "!!int" || value.tag.is_empty()) && value.value == "0" {
                return true;
            }
            if (value.tag == "!!bool" || value.tag.is_empty()) && value.value == "false" {
                return true;
            }
            if value.value.is_empty() {
                return true;
            }
        }
    }
    if value.kind == Kind::Sequence && value.content.is_empty() {
        return true;
    }
    if value.kind == Kind::Mapping && value.content.is_empty() {
        if in_tool_args {
            return false;
        }
        return !matches!(key, "google_search" | "code_execution" | "retrieval");
    }
    false
}

/// Go `init`'s `snakeCaseKeys`, in Go's order.
const SNAKE_CASE_KEYS: [&str; 83] = [
    // Top-level and Recording structs
    "user_message_index",
    "agent_name",
    "llm_recording",
    "llm_request",
    "llm_responses",
    "tool_recording",
    "tool_call",
    "tool_response",
    // LLMResponse fields
    "citation_metadata",
    "grounding_metadata",
    "usage_metadata",
    "custom_metadata",
    "logprobs_result",
    "input_transcription",
    "output_transcription",
    "model_version",
    "session_resumption_handle",
    "error_code",
    "error_message",
    "finish_reason",
    "avg_logprobs",
    // GenerateContentConfig fields
    "http_options",
    "system_instruction",
    "candidate_count",
    "max_output_tokens",
    "stop_sequences",
    "response_logprobs",
    "presence_penalty",
    "frequency_penalty",
    "response_mime_type",
    "response_schema",
    "response_json_schema",
    "routing_config",
    "model_selection_config",
    "safety_settings",
    "tool_config",
    "cached_content",
    "response_modalities",
    "media_resolution",
    "speech_config",
    "audio_timestamp",
    "thinking_config",
    "image_config",
    "enable_enhanced_civic_answers",
    "model_armor_config",
    "service_tier",
    // Tool fields
    "google_search",
    "blocking_confidence",
    "function_declarations",
    "parameters_json_schema",
    "additional_properties",
    "property_order",
    // Part / Content fields
    "part_metadata",
    "video_metadata",
    "code_execution_result",
    "executable_code",
    "file_data",
    "function_call",
    "function_call_id",
    "function_response",
    "inline_data",
    "thought_signature",
    // GroundingMetadata fields
    "grounding_chunks",
    "grounding_supports",
    "retrieval_metadata",
    "search_entry_point",
    "web_search_queries",
    // GroundingChunk / Support / Segment / SearchEntryPoint fields
    "grounding_chunk_indices",
    "confidence_score",
    "start_index",
    "end_index",
    "rendered_content",
    "sdk_active_queries",
    // Token / usage details
    "google_maps_widget_context_token",
    "candidates_token_count",
    "prompt_token_count",
    "prompt_tokens_details",
    "token_count",
    "thoughts_token_count",
    "tool_use_prompt_token_count",
    "tool_use_prompt_tokens_details",
    "total_token_count",
    "traffic_type",
];

static SNAKE_CASE_FIELD_MAP: LazyLock<HashMap<String, &'static str>> = LazyLock::new(|| {
    // Go: record_plugin.go init
    let mut m = HashMap::new();
    for key in SNAKE_CASE_KEYS {
        let cleaned = key.replace('_', "");
        m.insert(cleaned, key);
    }

    // Manual edge cases
    m.insert("propertyordering".to_string(), "property_order");
    m
});

/// Go `snakeCaseFieldMap` after its `init()`: the key with `_` removed → the snake_case key,
/// plus `propertyordering` → `property_order`.
pub fn snake_case_field_map() -> &'static HashMap<String, &'static str> {
    // Go: record_plugin.go init (snakeCaseFieldMap)
    &SNAKE_CASE_FIELD_MAP
}

/// Go `findKeyNode`: the value node of `key` in mapping `node`; `None` when `node` is not a
/// mapping or has no such key.
pub fn find_key_node(doc: &Document, node: NodeId, key: &str) -> Option<NodeId> {
    // Go: record_plugin.go findKeyNode
    let node = &doc[node];
    if node.kind != Kind::Mapping {
        return None;
    }
    let mut i = 0;
    while i < node.content.len() {
        if doc[node.content[i]].value == key {
            return Some(node.content[i + 1]);
        }
        i += 2;
    }
    None
}

/// One pending level of [`clean_yaml_node`]'s walk (Go recurses; see there).
enum Frame {
    /// A document or sequence node: its children are cleaned in order.
    List {
        content: Vec<NodeId>,
        next: usize,
        in_tool_args: bool,
    },
    /// A mapping node: each value is prepared, cleaned, then kept or pruned, in order.
    Mapping {
        node: NodeId,
        content: Vec<NodeId>,
        next: usize,
        in_tool_args: bool,
        new_content: Vec<NodeId>,
        /// The key, value and value context of the pair whose value is being cleaned.
        pending: Option<(NodeId, NodeId, bool)>,
    },
}

/// Go `cleanYAMLNode`: snake_case keys, `system_instruction` → scalar, `thought_signature`
/// int sequence → base64, schema title injection, pruning outside tool args.
pub fn clean_yaml_node(doc: &mut Document, node: NodeId, in_tool_args: bool) {
    // Go: record_plugin.go cleanYAMLNode
    // Divergence (ledger 6c-2): Go recurses; this walks an explicit stack (same visiting and
    // mutation order) so arbitrarily deep input cannot overflow the thread's stack.
    let mut stack = Vec::new();
    enter_node(doc, &mut stack, node, in_tool_args);
    while let Some(frame) = stack.last_mut() {
        match frame {
            Frame::List {
                content,
                next,
                in_tool_args,
            } => {
                let Some(&child) = content.get(*next) else {
                    stack.pop();
                    continue;
                };
                *next += 1;
                let in_tool_args = *in_tool_args;
                enter_node(doc, &mut stack, child, in_tool_args);
            }
            Frame::Mapping {
                node,
                content,
                next,
                in_tool_args,
                new_content,
                pending,
            } => {
                if let Some((k, v, next_in_tool_args)) = pending.take() {
                    // Drop empty, null, or redundant fields outside tool parameters
                    if !should_prune_yaml_field(&doc[k].value, doc, v, next_in_tool_args) {
                        new_content.push(k);
                        new_content.push(v);
                    }
                    continue;
                }
                if *next >= content.len() {
                    let node = *node;
                    let new_content = std::mem::take(new_content);
                    doc.get_mut(node).content = new_content;
                    stack.pop();
                    continue;
                }
                let k = content[*next];
                let v = content[*next + 1];
                *next += 2;
                let next_in_tool_args = prepare_mapping_value(doc, k, v, *in_tool_args);
                *pending = Some((k, v, next_in_tool_args));
                enter_node(doc, &mut stack, v, next_in_tool_args);
            }
        }
    }
}

/// The start of Go's `cleanYAMLNode(node, inToolArgs)`: a document or sequence pushes its
/// children; a mapping first injects the schema titles of a function declaration, then pushes
/// its pairs; scalars and aliases are left alone.
fn enter_node(doc: &mut Document, stack: &mut Vec<Frame>, node: NodeId, in_tool_args: bool) {
    // Go: record_plugin.go cleanYAMLNode (switch node.Kind)
    match doc[node].kind {
        Kind::Document | Kind::Sequence => stack.push(Frame::List {
            content: doc[node].content.clone(),
            next: 0,
            in_tool_args,
        }),
        Kind::Mapping => {
            // If this mapping represents an OpenAPI function declaration, recursively inject
            // parameter schema titles.
            // This ensures the generated schemas exactly match Pydantic reference standards.
            let mut tool_name = String::new();
            if let Some(name_node) = find_key_node(doc, node, "name")
                && doc[name_node].kind == Kind::Scalar
            {
                tool_name = doc[name_node].value.clone();
            }
            let mut param_schema_node = None;
            for key in [
                "parameters",
                "parameters_json_schema",
                "parametersjsonschema",
            ] {
                if let Some(p) = find_key_node(doc, node, key)
                    && doc[p].kind == Kind::Mapping
                {
                    param_schema_node = Some(p);
                    break;
                }
            }
            if !tool_name.is_empty()
                && let Some(param_schema_node) = param_schema_node
            {
                inject_schema_titles(doc, param_schema_node, &tool_name);
            }

            stack.push(Frame::Mapping {
                node,
                content: doc[node].content.clone(),
                next: 0,
                in_tool_args,
                new_content: Vec::new(),
                pending: None,
            });
        }
        _ => {}
    }
}

/// The body of Go `cleanYAMLNode`'s mapping loop before it recurses into the value: map the key
/// to snake_case, collapse `system_instruction`, coalesce `thought_signature`; returns the
/// value's tool-args context.
fn prepare_mapping_value(doc: &mut Document, k: NodeId, v: NodeId, in_tool_args: bool) -> bool {
    // Go: record_plugin.go cleanYAMLNode (mapping loop)
    // Translate lowercase serialized struct fields back to canonical snake_case
    if let Some(mapped_key) = snake_case_field_map().get(&doc[k].value) {
        doc.get_mut(k).value = (*mapped_key).to_string();
    }

    // Set insideToolArgs context flag so we avoid pruning user data and parameter payloads
    let key = doc[k].value.as_str();
    let next_in_tool_args = in_tool_args
        || key == "args"
        || key == "response"
        || key == "tool_call"
        || key == "tool_response";

    // Extract structured system instructions (Content with Parts) into simple string scalars
    if key == "system_instruction"
        && doc[v].kind == Kind::Mapping
        && let Some(parts) = find_key_node(doc, v, "parts")
        && doc[parts].kind == Kind::Sequence
        && let Some(&part) = doc[parts].content.first()
        && let Some(text) = find_key_node(doc, part, "text")
        && doc[text].kind == Kind::Scalar
    {
        let (style, value) = (doc[text].style, doc[text].value.clone());
        let v = doc.get_mut(v);
        v.kind = Kind::Scalar;
        v.tag = "!!str".to_string();
        v.style = style;
        v.value = value;
        v.content = Vec::new();
    }

    // Coalesce raw integer sequences representing byte signatures back to standard Base64
    // string representation
    if doc[k].value == "thought_signature"
        && doc[v].kind == Kind::Sequence
        && !doc[v].content.is_empty()
    {
        let mut bytes = Vec::new();
        for &child in &doc[v].content {
            let child = &doc[child];
            if child.kind == Kind::Scalar
                && (child.tag == "!!int" || child.tag.is_empty())
                && let Some(b) = go_sscan_int(&child.value)
            {
                // Go `byte(b)`: the low 8 bits.
                bytes.push(b as u8);
            }
        }
        let v = doc.get_mut(v);
        v.kind = Kind::Scalar;
        v.style = Style::DOUBLE_QUOTED;
        v.tag = "!!str".to_string();
        v.value = STANDARD.encode(&bytes);
        v.content = Vec::new();
    }

    next_in_tool_args
}

/// Go `fmt.Sscan(s, &b)` with `b int` (64-bit): `Some(b)` when it returns a nil error. The
/// operand is the first space-separated token (leading Unicode spaces and newlines skipped,
/// trailing input ignored): an optional sign, then decimal digits, or `0b`/`0o`/`0x`/`0`
/// prefixed digits, `_` allowed, parsed by `strconv.ParseInt(tok, 0, 64)`.
fn go_sscan_int(s: &str) -> Option<i64> {
    // Go: fmt/scan.go (*ss).scanInt (verb 'v') + scanBasePrefix + scanNumber
    let mut chars = s.chars().peekable();
    // SkipSpace (nlIsSpace: `\r\n` and `\n` are spaces too).
    while let Some(&c) = chars.peek() {
        if !go_fmt_is_space(c) {
            break;
        }
        chars.next();
    }
    // notEOF
    chars.peek()?;
    let mut tok = String::new();
    if let Some(&c) = chars.peek()
        && (c == '+' || c == '-')
    {
        tok.push(c);
        chars.next();
    }
    let (digits, have_digits): (&str, bool) = if chars.peek() == Some(&'0') {
        tok.push('0');
        chars.next();
        match chars.peek() {
            Some(&c @ ('b' | 'B')) => {
                tok.push(c);
                chars.next();
                ("01_", true)
            }
            Some(&c @ ('o' | 'O')) => {
                tok.push(c);
                chars.next();
                ("01234567_", true)
            }
            Some(&c @ ('x' | 'X')) => {
                tok.push(c);
                chars.next();
                ("0123456789aAbBcCdDeEfF_", true)
            }
            _ => ("01234567_", true),
        }
    } else {
        ("0123456789_", false)
    };
    if !have_digits && !chars.peek().is_some_and(|c| digits.contains(*c)) {
        // "expected integer" (or unexpected EOF)
        return None;
    }
    while let Some(&c) = chars.peek() {
        if !digits.contains(c) {
            break;
        }
        tok.push(c);
        chars.next();
    }
    go_parse_int_base0(&tok)
}

/// fmt's `isSpace` (scan.go `space` table) for the runes `SkipSpace` skips.
fn go_fmt_is_space(c: char) -> bool {
    // Go: fmt/scan.go space, isSpace
    matches!(
        c as u32,
        0x0009..=0x000d
            | 0x0020
            | 0x0085
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028..=0x2029
            | 0x202f
            | 0x205f
            | 0x3000
    )
}

/// Go `strconv.ParseInt(s, 0, 64)`: `None` on a syntax or range error.
fn go_parse_int_base0(s: &str) -> Option<i64> {
    // Go: strconv/atoi.go ParseInt + ParseUint (base 0) + underscoreOK
    let (neg, unsigned) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    let b = unsigned.as_bytes();
    if b.is_empty() {
        return None;
    }
    let (base, rest): (u64, &[u8]) = if b[0] == b'0' {
        match b.get(1).map(u8::to_ascii_lowercase) {
            Some(b'b') if b.len() >= 3 => (2, &b[2..]),
            Some(b'o') if b.len() >= 3 => (8, &b[2..]),
            Some(b'x') if b.len() >= 3 => (16, &b[2..]),
            _ => (8, &b[1..]),
        }
    } else {
        (10, b)
    };
    let mut n: u64 = 0;
    let mut underscores = false;
    for &c in rest {
        if c == b'_' {
            underscores = true;
            continue;
        }
        let d = match c {
            b'0'..=b'9' => u64::from(c - b'0'),
            b'a'..=b'z' | b'A'..=b'Z' => u64::from(c.to_ascii_lowercase() - b'a') + 10,
            _ => return None,
        };
        if d >= base {
            return None;
        }
        n = n.checked_mul(base)?.checked_add(d)?;
    }
    if underscores && !go_underscore_ok(unsigned.as_bytes()) {
        return None;
    }
    const CUTOFF: u64 = 1 << 63;
    if !neg && n >= CUTOFF || neg && n > CUTOFF {
        return None;
    }
    // n <= 2^63 here; -(2^63) is i64::MIN.
    Some(if neg {
        0i64.wrapping_sub_unsigned(n)
    } else {
        n as i64
    })
}

/// Go `strconv.underscoreOK`: whether `_` only separates digits (base prefix counts as one).
fn go_underscore_ok(s: &[u8]) -> bool {
    // Go: strconv/atoi.go underscoreOK
    let mut saw = b'^';
    let mut i = 0;
    let s = match s.first() {
        Some(b'-' | b'+') => &s[1..],
        _ => s,
    };
    let mut hex = false;
    if s.len() >= 2 && s[0] == b'0' && matches!(s[1].to_ascii_lowercase(), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = s[1].eq_ignore_ascii_case(&b'x');
    }
    while i < s.len() {
        let c = s[i];
        i += 1;
        if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&c.to_ascii_lowercase()) {
            saw = b'0';
            continue;
        }
        if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
            continue;
        }
        if saw == b'_' {
            return false;
        }
        saw = b'!';
    }
    saw != b'_'
}

/// A `!!str` scalar, double-quoted when `quoted` (Go's `&yaml.Node{Kind: ScalarNode, Tag:
/// "!!str", …}` literals).
fn str_node(value: String, quoted: bool) -> NodeData {
    NodeData {
        kind: Kind::Scalar,
        tag: "!!str".to_string(),
        value,
        style: if quoted {
            Style::DOUBLE_QUOTED
        } else {
            Style::empty()
        },
        ..NodeData::default()
    }
}

/// Fill an existing `title` node that is empty or null in place (Go's `titleNode.Kind = …`
/// block).
fn fill_empty_title(doc: &mut Document, title_node: NodeId, title: String) {
    let t = doc.get_mut(title_node);
    if t.value.is_empty() || t.tag == "!!null" || t.value == "null" {
        t.kind = Kind::Scalar;
        t.tag = "!!str".to_string();
        t.style = Style::DOUBLE_QUOTED;
        t.value = title;
        t.content = Vec::new();
    }
}

/// Prepend a `title: "<title>"` pair to mapping `node` (Go's
/// `append([]*yaml.Node{titleKey, titleValue}, node.Content...)`).
fn prepend_title(doc: &mut Document, node: NodeId, title: String) {
    let title_key = doc.push(str_node("title".to_string(), false));
    let title_value = doc.push(str_node(title, true));
    let content = &mut doc.get_mut(node).content;
    content.splice(0..0, [title_key, title_value]);
}

/// Go `injectSchemaTitles`: `<tool>Params` on the schema, `Title Case` on each property.
pub fn inject_schema_titles(doc: &mut Document, schema_node: NodeId, tool_name: &str) {
    // Go: record_plugin.go injectSchemaTitles
    if doc[schema_node].kind != Kind::Mapping {
        return;
    }

    // Check or inject schema top-level title
    let title = format!("{tool_name}Params");
    match find_key_node(doc, schema_node, "title") {
        Some(title_node) => fill_empty_title(doc, title_node, title),
        None => prepend_title(doc, schema_node, title),
    }

    // Recursively check and inject titles for individual properties
    if let Some(properties_node) = find_key_node(doc, schema_node, "properties")
        && doc[properties_node].kind == Kind::Mapping
    {
        let content = doc[properties_node].content.clone();
        for pair in content.chunks(2) {
            let (prop_name_node, prop_def_node) = (pair[0], pair[1]);
            if doc[prop_def_node].kind == Kind::Mapping {
                let title = to_camel_case_title(&doc[prop_name_node].value);
                match find_key_node(doc, prop_def_node, "title") {
                    Some(prop_title) => fill_empty_title(doc, prop_title, title),
                    None => prepend_title(doc, prop_def_node, title),
                }
            }
        }
    }
}

/// Go `toCamelCaseTitle`: `phone_number` → `Phone Number` (byte-wise `ToUpper` of each part's
/// first byte, as Go's `strings.ToUpper(part[:1])`).
pub fn to_camel_case_title(s: &str) -> String {
    // Go: record_plugin.go toCamelCaseTitle
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    for (i, part) in s.split('_').enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        let bytes = part.as_bytes();
        if let Some((&first, rest)) = bytes.split_first() {
            if first.is_ascii() {
                out.push(first.to_ascii_uppercase());
            } else {
                // Go's `part[:1]` is a lone UTF-8 lead byte, which `strings.ToUpper` replaces
                // with U+FFFD.
                out.extend_from_slice("\u{FFFD}".as_bytes());
            }
            out.extend_from_slice(rest);
        }
    }
    // Divergence (ledger 6c-2): Go's result keeps the orphaned continuation bytes of a
    // non-ASCII first character (invalid UTF-8, so yaml.Marshal then fails and the record
    // file is not written); a Rust String replaces each with U+FFFD.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use gadk_goyaml::{marshal_node, unmarshal_node};
    use pretty_assertions::assert_eq;

    use super::*;

    /// `yaml.Unmarshal` → `cleanYAMLNode(&node, false)` → `yaml.Marshal`.
    fn clean(input: &str) -> String {
        let mut doc = unmarshal_node(input.as_bytes()).expect("parse");
        let root = doc.root();
        clean_yaml_node(&mut doc, root, false);
        String::from_utf8(marshal_node(&doc, root).expect("marshal")).expect("UTF-8")
    }

    fn scalar(doc: &mut Document, tag: &str, value: &str) -> NodeId {
        doc.push(NodeData {
            kind: Kind::Scalar,
            tag: tag.to_string(),
            value: value.to_string(),
            ..NodeData::default()
        })
    }

    fn collection(doc: &mut Document, kind: Kind, content: Vec<NodeId>) -> NodeId {
        doc.push(NodeData {
            kind,
            content,
            ..NodeData::default()
        })
    }

    // The expected outputs below are Go's: the same input through the vendored
    // `cleanYAMLNode` (scripts/gen-go-configurable) and yaml.v3.
    const PRUNE_IN: &str = r#"a: null
b: ~
c: "null"
d: !!str ~
e: 0
f: "0"
g: !!int 0
h: !!float 0
i: false
j: "false"
k: !!bool false
l: ""
m: []
n: {}
google_search: {}
code_execution: {}
retrieval: {}
codeexecution: {}
user_message_index: 0
usermessageindex: null
property_order: [a]
propertyordering: [a]
response_json_schema: {type: object}
responsejsonschema: {type: object}
o: 0.0
p: x
q: [null]
r: {s: null}
args:
  a: null
  e: 0
  i: false
  l: ""
  m: []
  n: {}
  property_order: [a]
  response_json_schema: {type: object}
  nested: {deep: {}}
tool_call: {}
response: ""
"#;
    const PRUNE_OUT: &str = r#"f: "0"
h: !!float 0
j: "false"
google_search: {}
code_execution: {}
retrieval: {}
user_message_index: 0
user_message_index: null
o: 0.0
p: x
q: [null]
args:
    e: 0
    i: false
    l: ""
    n: {}
    property_order: [a]
    response_json_schema: {type: object}
    nested: {deep: {}}
tool_call: {}
response: ""
"#;

    const SIG_IN: &str = r#"thought_signature:
  - 72
  - "105"
  - [1]
  - {a: 1}
  - 0x41
  - 0o17
  - 1_0
  - 300
  - -1
  - 08
  - 1e3
  - 18446744073709551615
  - !!int 12abc
  - !!int " 7"
  - !custom 9
  - null
"#;
    const SIG_OUT: &str = r#"thought_signature: "SEEPCiz/DAc="
"#;

    const SIG_EMPTY_IN: &str = r#"parts:
  - thought_signature:
      - "a"
      - 1.5
    text: x
args:
  thought_signature:
    - "a"
  thoughtsignature:
    - [2]
"#;
    const SIG_EMPTY_OUT: &str = r#"parts:
    - text: x
args:
    thought_signature: ""
    thought_signature: ""
"#;

    const SYSTEM_INSTRUCTION_IN: &str = r#"- config:
    system_instruction:
      parts:
        - text: 'single quoted'
        - text: second
      role: user
- config:
    systeminstruction:
      parts:
        - inline_data: {mime_type: x}
      role: user
- config:
    system_instruction:
      parts:
        - text: ""
      role: user
- config:
    system_instruction:
      parts:
        - text: [a]
      role: user
- config:
    system_instruction:
      parts: []
      role: user
- config:
    system_instruction:
      parts:
        - text: false
"#;
    const SYSTEM_INSTRUCTION_OUT: &str = r#"- config:
    system_instruction: 'single quoted'
- config:
    system_instruction:
        parts:
            - inline_data: {mime_type: x}
        role: user
- {}
- config:
    system_instruction:
        parts:
            - text: [a]
        role: user
- config:
    system_instruction:
        role: user
- config:
    system_instruction: "false"
"#;

    const TITLES_IN: &str = r#"- name: tool_a
  parameters:
    title: null
    properties:
      first_name:
        title: ~
        type: string
      last_name:
        title: ""
        type: string
      kept:
        title: Custom
      missing:
        type: object
        properties:
          inner_prop:
            type: string
      scalar_prop: x
- name: tool_b
  parameters: "not a mapping"
  parameters_json_schema:
    title: Kept Title
    properties:
      x: {}
      y: true
- name: tool_c
  parametersjsonschema:
    type: object
- name: ""
  parameters:
    properties:
      a: {type: string}
- name: [x]
  parameters:
    type: object
- parameters:
    type: object
- args:
    name: inner_tool
    parameters:
      properties:
        p_q: {}
"#;
    const TITLES_OUT: &str = r#"- name: tool_a
  parameters:
    title: "tool_aParams"
    properties:
        first_name:
            title: "First Name"
            type: string
        last_name:
            title: "Last Name"
            type: string
        kept:
            title: Custom
        missing:
            title: "Missing"
            type: object
            properties:
                inner_prop:
                    type: string
        scalar_prop: x
- name: tool_b
  parameters: "not a mapping"
  parameters_json_schema:
    title: Kept Title
    properties:
        x: {title: "X"}
        y: true
- name: tool_c
  parameters_json_schema:
    title: "tool_cParams"
    type: object
- parameters:
    properties:
        a: {type: string}
- name: [x]
  parameters:
    type: object
- parameters:
    type: object
- args:
    name: inner_tool
    parameters:
        title: "inner_toolParams"
        properties:
            p_q: {title: "P Q"}
"#;

    #[test]
    fn should_prune_yaml_field_rules() {
        // (key, tag, value, in_tool_args, pruned)
        let scalars = [
            ("user_message_index", "!!null", "null", false, false),
            ("usermessageindex", "!!int", "0", false, false),
            ("property_order", "!!str", "x", false, true),
            ("property_order", "!!str", "x", true, false),
            ("response_json_schema", "!!str", "x", false, true),
            ("response_json_schema", "!!str", "x", true, false),
            ("k", "!!null", "", false, true),
            ("k", "!!null", "", true, true),
            ("k", "!!str", "null", true, true),
            ("k", "!!str", "~", true, true),
            ("k", "!!int", "0", false, true),
            ("k", "", "0", false, true),
            ("k", "!!str", "0", false, false),
            ("k", "!!float", "0", false, false),
            ("k", "!!int", "0", true, false),
            ("k", "!!bool", "false", false, true),
            ("k", "", "false", false, true),
            ("k", "!!str", "false", false, false),
            ("k", "!!bool", "false", true, false),
            ("k", "!!str", "", false, true),
            ("k", "!!str", "", true, false),
            ("k", "!!str", "x", false, false),
        ];
        for (key, tag, value, in_tool_args, want) in scalars {
            let mut doc = Document::default();
            let v = scalar(&mut doc, tag, value);
            assert_eq!(
                should_prune_yaml_field(key, &doc, v, in_tool_args),
                want,
                "{key} {tag} {value:?} in_tool_args={in_tool_args}"
            );
        }

        // (key, kind, in_tool_args, pruned) for empty collections
        let empties = [
            ("k", Kind::Sequence, false, true),
            ("k", Kind::Sequence, true, true),
            ("user_message_index", Kind::Sequence, false, false),
            ("k", Kind::Mapping, false, true),
            ("k", Kind::Mapping, true, false),
            ("google_search", Kind::Mapping, false, false),
            ("code_execution", Kind::Mapping, false, false),
            ("retrieval", Kind::Mapping, false, false),
            ("googlesearch", Kind::Mapping, false, true),
        ];
        for (key, kind, in_tool_args, want) in empties {
            let mut doc = Document::default();
            let v = collection(&mut doc, kind, Vec::new());
            assert_eq!(
                should_prune_yaml_field(key, &doc, v, in_tool_args),
                want,
                "{key} {kind:?} in_tool_args={in_tool_args}"
            );
        }

        // Non-empty collections and other kinds are kept.
        let mut doc = Document::default();
        let item = scalar(&mut doc, "!!null", "null");
        let seq = collection(&mut doc, Kind::Sequence, vec![item]);
        assert!(!should_prune_yaml_field("k", &doc, seq, false));
        let alias = doc.push(NodeData {
            kind: Kind::Alias,
            ..NodeData::default()
        });
        assert!(!should_prune_yaml_field("k", &doc, alias, false));
    }

    #[test]
    fn clean_prune_rules_match_go() {
        assert_eq!(clean(PRUNE_IN), PRUNE_OUT);
    }

    #[test]
    fn snake_case_field_map_matches_go_init() {
        let m = snake_case_field_map();
        assert_eq!(m.len(), SNAKE_CASE_KEYS.len() + 1);
        for key in SNAKE_CASE_KEYS {
            assert_eq!(m[&key.replace('_', "")], key);
        }
        assert_eq!(m["propertyordering"], "property_order");
        assert_eq!(m["propertyorder"], "property_order");
        assert_eq!(m["functiondeclarations"], "function_declarations");
        assert_eq!(m["thoughtsignature"], "thought_signature");
        assert_eq!(m.get("codeexecution"), None);
        assert_eq!(m.get("startoffset"), None);
    }

    #[test]
    fn find_key_node_finds_first_value_of_mappings_only() {
        let doc = unmarshal_node(b"a: 1\nb: 2\nb: 3\n").expect("parse");
        let mapping = doc[doc.root()].content[0];
        let b = find_key_node(&doc, mapping, "b").expect("b");
        assert_eq!(doc[b].value, "2");
        assert_eq!(find_key_node(&doc, mapping, "c"), None);
        // Keys match by value only; values never match.
        assert_eq!(find_key_node(&doc, mapping, "1"), None);
        // Not a mapping: the document node and a scalar.
        assert_eq!(find_key_node(&doc, doc.root(), "a"), None);
        assert_eq!(find_key_node(&doc, b, "a"), None);
    }

    #[test]
    fn thought_signature_sequence_converts_int_children_like_go_sscan() {
        // Non-int children (`!!str`, sequences, mappings, `!!float`, custom tags, null) and
        // `!!int` values Sscan rejects (> i64) are skipped; 300 → 44, -1 → 255.
        assert_eq!(clean(SIG_IN), SIG_OUT);
        // No int child: "" (pruned outside tool args, kept as `""` inside).
        assert_eq!(clean(SIG_EMPTY_IN), SIG_EMPTY_OUT);
    }

    #[test]
    fn thought_signature_node_becomes_double_quoted_str_scalar() {
        let mut doc = Document::default();
        let key = scalar(&mut doc, "!!str", "thoughtsignature");
        let a = scalar(&mut doc, "", "72");
        let b = scalar(&mut doc, "!!int", "105");
        let sig = collection(&mut doc, Kind::Sequence, vec![a, b]);
        let root = collection(&mut doc, Kind::Mapping, vec![key, sig]);
        clean_yaml_node(&mut doc, root, false);
        assert_eq!(doc[root].content, vec![key, sig]);
        assert_eq!(doc[key].value, "thought_signature");
        assert_eq!(
            doc[sig],
            NodeData {
                kind: Kind::Scalar,
                style: Style::DOUBLE_QUOTED,
                tag: "!!str".to_string(),
                value: "SGk=".to_string(),
                ..NodeData::default()
            }
        );
    }

    #[test]
    fn go_sscan_int_matches_go() {
        // Go: `var b int; _, err := fmt.Sscan(s, &b)` (values captured from Go 1.26).
        let cases: [(&str, Option<i64>); 37] = [
            ("72", Some(72)),
            (" 7", Some(7)),
            ("\n\t7", Some(7)),
            ("\u{a0} 9", Some(9)),
            ("+5", Some(5)),
            ("-1", Some(-1)),
            ("300", Some(300)),
            ("12abc", Some(12)),
            ("12.5", Some(12)),
            ("0x41", Some(65)),
            ("0X41", Some(65)),
            ("0o17", Some(15)),
            ("0b101", Some(5)),
            ("017", Some(15)),
            ("08", Some(0)),
            ("0", Some(0)),
            ("1_000", Some(1000)),
            ("1__0", None),
            ("_1", None),
            ("1_", None),
            ("0x", None),
            ("0b2", None),
            ("-0x10", Some(-16)),
            ("9223372036854775807", Some(i64::MAX)),
            ("9223372036854775808", None),
            ("-9223372036854775808", Some(i64::MIN)),
            ("-9223372036854775809", None),
            ("", None),
            ("  ", None),
            ("abc", None),
            ("- 5", None),
            ("0_7", Some(7)),
            ("0x_1f", Some(31)),
            ("1 2", Some(1)),
            ("\u{3000} 3", Some(3)),
            ("\r\n4", Some(4)),
            ("--1", None),
        ];
        for (s, want) in cases {
            assert_eq!(go_sscan_int(s), want, "{s:?}");
        }
    }

    #[test]
    fn system_instruction_collapses_like_go() {
        // First part's scalar text (style kept) → `!!str` scalar; no text / non-scalar text /
        // empty parts keep the mapping; `""` collapses and is pruned.
        assert_eq!(clean(SYSTEM_INSTRUCTION_IN), SYSTEM_INSTRUCTION_OUT);
    }

    #[test]
    fn schema_titles_inject_like_go() {
        // Null/`~`/`""` titles are filled in place, missing titles are prepended, non-empty
        // titles kept; only first-level properties get titles; a non-mapping `parameters`
        // falls through to `parameters_json_schema`; no name (or a non-scalar one) → nothing.
        assert_eq!(clean(TITLES_IN), TITLES_OUT);
    }

    #[test]
    fn inject_schema_titles_fills_and_prepends_nodes() {
        let mut doc = unmarshal_node(b"title: null\nproperties:\n  a_b: {}\n  c: {title: ''}\n")
            .expect("parse");
        let schema = doc[doc.root()].content[0];
        let before = doc[schema].content.clone();
        inject_schema_titles(&mut doc, schema, "t");
        // The top title is filled in place (same node, nothing prepended).
        assert_eq!(doc[schema].content, before);
        let title = find_key_node(&doc, schema, "title").expect("title");
        assert_eq!(doc[title].tag, "!!str");
        assert_eq!(doc[title].style, Style::DOUBLE_QUOTED);
        assert_eq!(doc[title].value, "tParams");

        let props = find_key_node(&doc, schema, "properties").expect("properties");
        let a_b = doc[props].content[1];
        let [key, value] = doc[a_b].content[..] else {
            panic!("a_b: {:?}", doc[a_b].content)
        };
        assert_eq!(doc[key], str_node("title".to_string(), false));
        assert_eq!(doc[value], str_node("A B".to_string(), true));
        let c = doc[props].content[3];
        assert_eq!(doc[c].content.len(), 2);
        let c_title = doc[c].content[1];
        assert_eq!(doc[c_title].value, "C");
        assert_eq!(doc[c_title].style, Style::DOUBLE_QUOTED);

        // Not a mapping: unchanged.
        let mut doc = unmarshal_node(b"[a]\n").expect("parse");
        let seq = doc[doc.root()].content[0];
        let before = doc.clone();
        inject_schema_titles(&mut doc, seq, "t");
        assert_eq!(doc, before);
    }

    #[test]
    fn to_camel_case_title_matches_go() {
        let cases = [
            ("phone_number", "Phone Number"),
            ("email", "Email"),
            ("", ""),
            ("_a__b_", " A  B "),
            ("a", "A"),
            ("already Title", "Already Title"),
            ("ABC_def", "ABC Def"),
            // Go: "�\xa9clair X" / "X �\xbc" (invalid UTF-8; ledger 6c-2).
            ("éclair_x", "\u{FFFD}\u{FFFD}clair X"),
            ("x_ü", "X \u{FFFD}\u{FFFD}"),
        ];
        for (s, want) in cases {
            assert_eq!(to_camel_case_title(s), want, "{s:?}");
        }
    }

    #[test]
    fn deep_nesting_is_cleaned_without_recursion() {
        // 100 000 nested `a:` mappings ending in null: every level becomes empty and is pruned,
        // as in Go (whose stack grows); a recursive walk would overflow the test thread.
        let mut doc = Document::default();
        let mut inner = scalar(&mut doc, "!!null", "null");
        for _ in 0..100_000 {
            let key = scalar(&mut doc, "!!str", "a");
            inner = collection(&mut doc, Kind::Mapping, vec![key, inner]);
        }
        clean_yaml_node(&mut doc, inner, false);
        assert!(doc[inner].content.is_empty());
    }

    #[test]
    fn go_goldens_are_fixed_points() {
        // Go's cleanYAMLNode is idempotent on every probe golden (checked with the vendored
        // copy): unmarshal → clean → marshal reproduces each file byte for byte.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/recordings");
        let mut files = 0;
        for case in std::fs::read_dir(dir).expect("fixtures") {
            for entry in std::fs::read_dir(case.expect("case").path()).expect("case dir") {
                let path = entry.expect("entry").path();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if !name.starts_with("generated-recordings") {
                    continue;
                }
                let data = std::fs::read(&path).expect("read");
                let mut doc = unmarshal_node(&data).expect("parse");
                let root = doc.root();
                clean_yaml_node(&mut doc, root, false);
                let out = marshal_node(&doc, root).expect("marshal");
                assert_eq!(
                    String::from_utf8_lossy(&out),
                    String::from_utf8_lossy(&data),
                    "{}",
                    path.display()
                );
                files += 1;
            }
        }
        assert_eq!(files, 44);
    }
}
