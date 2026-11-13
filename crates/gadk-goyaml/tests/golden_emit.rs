// Copyright 2026 Chris Olson
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

//! Replays the probe's emit goldens: `marshal_b64`, `indent2_b64` and `stream.marshal_b64`
//! from Documents built out of the node dumps (Task 7).

mod support;

use gadk_goyaml::{Document, Encoder, marshal_node};

use support::golden::{
    DIFF_GOLDEN_COUNT, PARSE_GOLDEN_COUNT, ParseGolden, dump_to_document, golden_paths,
    read_parse_golden,
};

/// Shows bytes as text for failure messages.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The node Go's `yaml.Unmarshal(in, &node)` filled: the dumped document node, or the zero
/// node (`node` null without an error: empty, whitespace-only and comment-only inputs).
fn golden_document(g: &ParseGolden) -> Document {
    match &g.node {
        Some(dump) => dump_to_document(dump),
        None => Document::default(),
    }
}

/// Go's probe `encodeAll`: one `Encoder` (indent 0 = Go's default) encoding every document,
/// then `Close`.
fn encode_all(docs: &[Document], indent: usize) -> Result<Vec<u8>, gadk_goyaml::Error> {
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out);
    if indent != 0 {
        enc.set_indent(indent);
    }
    for doc in docs {
        enc.encode_node(doc, doc.root())?;
    }
    enc.close()?;
    Ok(out)
}

/// Checks one golden's three emit views; returns the failures.
fn check_golden(g: &ParseGolden) -> Vec<String> {
    let mut failures = Vec::new();
    if g.error.is_none() {
        let doc = golden_document(g);
        // `yaml.Marshal(&node)`.
        match (
            marshal_node(&doc, doc.root()),
            g.marshal(),
            &g.marshal_error,
        ) {
            (Ok(got), Some(want), None) if got == want => {}
            (Err(e), None, Some(want)) if e.to_string() == *want => {}
            (got, want, want_err) => failures.push(format!(
                "{}: marshal: got {:?}, want {:?} / error {want_err:?}",
                g.case,
                got.map(|b| show(&b)).map_err(|e| e.to_string()),
                want.map(|b| show(&b)),
            )),
        }
        // `Encoder` with `SetIndent(2)`.
        match (encode_all(std::slice::from_ref(&doc), 2), g.indent2()) {
            (Ok(got), Some(want)) if got == want => {}
            (Err(_), None) => {}
            (got, want) => failures.push(format!(
                "{}: indent2: got {:?}, want {:?}",
                g.case,
                got.map(|b| show(&b)).map_err(|e| e.to_string()),
                want.map(|b| show(&b)),
            )),
        }
    }
    // The stream re-encoded by one default `Encoder`.
    let docs: Vec<Document> = g.stream.nodes.iter().map(dump_to_document).collect();
    match (encode_all(&docs, 0), g.stream.marshal()) {
        (Ok(got), Some(want)) if got == want => {}
        // Go's `Close` without any `Encode`.
        (Err(e), None) if !docs.is_empty() || e.to_string() == "yaml: expected STREAM-START" => {}
        (got, want) => failures.push(format!(
            "{}: stream: got {:?}, want {:?}",
            g.case,
            got.map(|b| show(&b)).map_err(|e| e.to_string()),
            want.map(|b| show(&b)),
        )),
    }
    failures
}

/// Replays every golden of `dir`; returns how many were checked.
fn replay(dir: &str) -> usize {
    let paths = golden_paths(dir);
    let mut failures = Vec::new();
    for path in &paths {
        failures.extend(check_golden(&read_parse_golden(path)));
    }
    assert!(
        failures.is_empty(),
        "{} emit failures in {dir}:\n{}",
        failures.len(),
        failures.join("\n")
    );
    paths.len()
}

#[test]
fn parse_goldens_emit_like_go() {
    assert_eq!(replay("parse"), PARSE_GOLDEN_COUNT);
}

#[test]
fn diff_goldens_emit_like_go() {
    assert_eq!(replay("diff"), DIFF_GOLDEN_COUNT);
}

#[test]
fn zero_node_marshals_to_null() {
    // Go: `var n yaml.Node; yaml.Marshal(&n)` (the golden `node` null case).
    let g = support::golden::parse_golden("edge-empty");
    assert!(g.node.is_none() && g.error.is_none());
    let doc = Document::default();
    assert_eq!(marshal_node(&doc, doc.root()).unwrap(), b"null\n");
    assert_eq!(g.marshal().unwrap(), b"null\n");
}

#[test]
fn close_without_encode_fails_like_go() {
    let mut out = Vec::new();
    let enc = Encoder::new(&mut out);
    assert_eq!(
        enc.close().unwrap_err().to_string(),
        "yaml: expected STREAM-START"
    );
    assert!(out.is_empty());
}

#[test]
fn self_containing_anchors_re_emit() {
    // The dumps' alias paths point at an ancestor (a cyclic tree); Marshal emits the alias by
    // name without following it.
    for case in ["edge-anchor-self-map", "edge-anchor-self-seq"] {
        let g = support::golden::parse_golden(case);
        assert!(check_golden(&g).is_empty(), "{case}");
        let doc = golden_document(&g);
        let got = marshal_node(&doc, doc.root()).unwrap();
        assert_eq!(got, g.marshal().unwrap(), "{case}");
    }
}
