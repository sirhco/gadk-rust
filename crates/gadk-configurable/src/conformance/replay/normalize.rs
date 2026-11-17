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

//! `normalize_yaml_node` with [`TO_IGNORE`] / [`TO_IGNORE_BUT_RECURSE`], over the gadk-goyaml
//! node arena.

use gadk_goyaml::{Document, Kind, NodeData, NodeId};

/// Free-form keys whose values are neither renamed nor recursed into.
pub const TO_IGNORE: [&str; 4] = ["thought_signature", "http_options", "args", "response"];

/// Snake_case structural keys kept as they are, values recursed into.
pub const TO_IGNORE_BUT_RECURSE: [&str; 8] = [
    "user_message_index",
    "agent_name",
    "llm_recording",
    "llm_request",
    "llm_responses",
    "tool_recording",
    "tool_call",
    "tool_response",
];

/// A scalar `system_instruction` becomes a `user` content with one
/// text part, a mapping `llm_response` becomes the one-element `llm_responses` sequence, and
/// every other key loses its underscores (outside [`TO_IGNORE`] / [`TO_IGNORE_BUT_RECURSE`]).
///
/// The walk uses an explicit stack of node ids (a node's pairs are rewritten, then its values
/// are visited first to last), so a document of any depth is normalized without growing the
/// call stack. Alias nodes are not followed.
pub fn normalize_yaml_node(doc: &mut Document, node: NodeId) {
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        match doc[id].kind {
            Kind::Document | Kind::Sequence => {
                stack.extend(doc[id].content.iter().rev().copied());
            }
            Kind::Mapping => {
                let content = doc[id].content.clone();
                let mut recurse = Vec::new();
                // A mapping with an odd number of children (only a hand-built node) has a
                // dangling key, which is left alone.
                for &[key, value] in content.as_chunks::<2>().0 {
                    // 1. Fix known type mismatches and singular/plural fields
                    match doc[key].value.as_str() {
                        "systeminstruction" | "system_instruction" => {
                            if doc[value].kind == Kind::Scalar {
                                system_instruction_to_content(doc, value);
                            }
                        }
                        "llmresponse" | "llm_response" => {
                            if doc[value].kind == Kind::Mapping {
                                let orig_copy = doc[value].clone();
                                let copy = doc.push(orig_copy);
                                let v = doc.get_mut(value);
                                v.kind = Kind::Sequence;
                                v.tag = "!!seq".to_owned();
                                v.value = String::new();
                                v.content = vec![copy];
                            }
                            doc.get_mut(key).value = "llm_responses".to_owned();
                        }
                        _ => {}
                    }

                    let key_value = doc[key].value.as_str();
                    // 2. Skip processing/recursion for ignored freeform keys (like tool args or
                    //    response payload)
                    if TO_IGNORE.contains(&key_value) {
                        continue;
                    }
                    // 3. For standard metadata fields, preserve snake_case but recurse into value
                    if TO_IGNORE_BUT_RECURSE.contains(&key_value) {
                        recurse.push(value);
                        continue;
                    }
                    // 4. Strip underscores to match the camelCase struct field names
                    //    case-insensitively
                    let stripped = key_value.replace('_', "");
                    doc.get_mut(key).value = stripped;
                    // 5. Recurse to process nested fields
                    recurse.push(value);
                }
                stack.extend(recurse.into_iter().rev());
            }
            Kind::Scalar | Kind::Alias | Kind::Zero => {}
        }
    }
}

/// Rewrites a scalar `system_instruction` value in place into
/// `{role: user, parts: [{text: <value>}]}` (the node keeps its other fields).
fn system_instruction_to_content(doc: &mut Document, value: NodeId) {
    let val = std::mem::take(&mut doc.get_mut(value).value);
    let scalar = |tag: &str, v: &str| NodeData {
        kind: Kind::Scalar,
        tag: tag.to_owned(),
        value: v.to_owned(),
        ..NodeData::default()
    };
    let role_key = doc.push(scalar("", "role"));
    let role = doc.push(scalar("!!str", "user"));
    let parts_key = doc.push(scalar("", "parts"));
    let text_key = doc.push(scalar("", "text"));
    let text = doc.push(scalar("!!str", &val));
    let part = doc.push(NodeData {
        kind: Kind::Mapping,
        tag: "!!map".to_owned(),
        content: vec![text_key, text],
        ..NodeData::default()
    });
    let parts = doc.push(NodeData {
        kind: Kind::Sequence,
        tag: "!!seq".to_owned(),
        content: vec![part],
        ..NodeData::default()
    });
    let v = doc.get_mut(value);
    v.kind = Kind::Mapping;
    v.tag = "!!map".to_owned();
    v.value = String::new();
    v.content = vec![role_key, role, parts_key, parts];
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use gadk_goyaml::{Style, marshal_node, unmarshal_node};
    use pretty_assertions::assert_eq;

    use super::*;

    fn normalized(input: &str) -> (Document, String) {
        let mut doc = unmarshal_node(input.as_bytes()).unwrap();
        let root = doc.root();
        normalize_yaml_node(&mut doc, root);
        let out = String::from_utf8(marshal_node(&doc, doc.root()).unwrap()).unwrap();
        (doc, out)
    }

    /// The mapping node at the document's top.
    fn top(doc: &Document) -> NodeId {
        doc[doc.root()].content[0]
    }

    #[test]
    fn system_instruction_scalar_becomes_go_shaped_content() {
        let (doc, out) = normalized("system_instruction: \"Be brief.\"\n");
        assert_eq!(
            out,
            "systeminstruction:\n    role: user\n    parts:\n        - text: Be brief.\n"
        );
        let top = top(&doc);
        let si = doc[top].content[1];
        // The rewritten node keeps the scalar's style; Go's new nodes carry Go's tags.
        assert_eq!(doc[si].kind, Kind::Mapping);
        assert_eq!(doc[si].tag, "!!map");
        assert_eq!(doc[si].style, Style::DOUBLE_QUOTED);
        let c = &doc[si].content;
        assert_eq!(
            [&doc[c[0]].tag, &doc[c[1]].tag, &doc[c[2]].tag],
            ["", "!!str", ""]
        );
        assert_eq!(doc[c[3]].tag, "!!seq");
        let part = doc[c[3]].content[0];
        assert_eq!(doc[part].tag, "!!map");
        let p = &doc[part].content;
        assert_eq!(
            (doc[p[0]].tag.as_str(), doc[p[0]].value.as_str()),
            ("", "text")
        );
        assert_eq!(
            (doc[p[1]].tag.as_str(), doc[p[1]].value.as_str()),
            ("!!str", "Be brief.")
        );
    }

    #[test]
    fn system_instruction_mapping_is_only_renamed_and_recursed() {
        let (_, out) = normalized(
            "system_instruction:\n  role: user\n  parts:\n    - inline_data: {mime_type: a}\n",
        );
        assert_eq!(
            out,
            "systeminstruction:\n    role: user\n    parts:\n        - inlinedata: {mimetype: a}\n"
        );
    }

    #[test]
    fn llm_response_mapping_is_wrapped_and_its_copy_recursed() {
        let (doc, out) = normalized("llm_response: &a\n  finish_reason: STOP\n");
        assert_eq!(
            out,
            "llm_responses: &a\n    - &a\n      finishreason: STOP\n"
        );
        let seq = doc[top(&doc)].content[1];
        let copy = doc[seq].content[0];
        assert_eq!(
            (doc[seq].kind, doc[seq].tag.as_str()),
            (Kind::Sequence, "!!seq")
        );
        assert_eq!(
            (doc[copy].kind, doc[copy].anchor.as_str()),
            (Kind::Mapping, "a")
        );
    }

    #[test]
    fn llm_response_key_is_renamed_whatever_its_value() {
        let (_, out) = normalized("llmresponse: x\nllm_response: [a_b: 1]\n");
        assert_eq!(out, "llm_responses: x\nllm_responses: [{ab: 1}]\n");
    }

    #[test]
    fn ignored_values_are_not_visited() {
        let (_, out) = normalized(
            "contents:\n  - parts:\n      - function_call:\n          args: {a_b: {c_d: 1}}\n        function_response:\n          response: {e_f: 2}\n        thought_signature: {g_h: 3}\n",
        );
        assert_eq!(
            out,
            "contents:\n    - parts:\n        - functioncall:\n            args: {a_b: {c_d: 1}}\n          functionresponse:\n            response: {e_f: 2}\n          thought_signature: {g_h: 3}\n"
        );
    }

    #[test]
    fn recurse_only_keys_keep_their_underscores() {
        let (_, out) = normalized(
            "recordings:\n  - user_message_index: 0\n    agent_name: a\n    tool_recording:\n      tool_call: {will_continue: true, args: {x_y: 1}}\n      tool_response: {will_continue: false}\n",
        );
        assert_eq!(
            out,
            "recordings:\n    - user_message_index: 0\n      agent_name: a\n      tool_recording:\n        tool_call: {willcontinue: true, args: {x_y: 1}}\n        tool_response: {willcontinue: false}\n"
        );
    }

    #[test]
    fn aliases_are_not_followed_and_scalars_ignored() {
        let mut doc = unmarshal_node(b"a_b: &x {c_d: 1}\ne_f: *x\n").unwrap();
        let root = doc.root();
        normalize_yaml_node(&mut doc, root);
        let out = String::from_utf8(marshal_node(&doc, doc.root()).unwrap()).unwrap();
        assert_eq!(out, "ab: &x {cd: 1}\nef: *x\n");
        let mut scalar = unmarshal_node(b"a_b\n").unwrap();
        let root = scalar.root();
        normalize_yaml_node(&mut scalar, root);
        assert_eq!(marshal_node(&scalar, scalar.root()).unwrap(), b"a_b\n");
    }

    #[test]
    fn deep_documents_do_not_recurse() {
        let depth = 100_000;
        let mut doc = Document::new(NodeData {
            kind: Kind::Mapping,
            ..NodeData::default()
        });
        let mut cur = doc.root();
        for _ in 0..depth {
            let k = doc.push(NodeData {
                kind: Kind::Scalar,
                value: "a_b".to_owned(),
                ..NodeData::default()
            });
            let v = doc.push(NodeData {
                kind: Kind::Mapping,
                ..NodeData::default()
            });
            doc.get_mut(cur).content = vec![k, v];
            cur = v;
        }
        let root = doc.root();
        normalize_yaml_node(&mut doc, root);
        let mut cur = doc.root();
        let mut n = 0;
        while let [k, v] = doc[cur].content[..] {
            assert_eq!(doc[k].value, "ab");
            cur = v;
            n += 1;
        }
        assert_eq!(n, depth);
    }
}
