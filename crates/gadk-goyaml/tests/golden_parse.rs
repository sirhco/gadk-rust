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

//! Replays the probe's parse goldens (`testdata/golden/parse/`, `testdata/golden/diff/`):
//! node trees, error texts and stream nodes (Task 6).

mod support;

use gadk_goyaml::{Decoder, Document, Kind, unmarshal_node};
use support::golden::{
    DIFF_GOLDEN_COUNT, PARSE_GOLDEN_COUNT, ParseGolden, compare_node, golden_paths,
    read_parse_golden,
};

/// Every parse and differential golden, in file-name order.
fn all_goldens() -> Vec<ParseGolden> {
    let mut goldens = Vec::new();
    for dir in ["parse", "diff"] {
        goldens.extend(golden_paths(dir).iter().map(|p| read_parse_golden(p)));
    }
    assert_eq!(goldens.len(), PARSE_GOLDEN_COUNT + DIFF_GOLDEN_COUNT);
    goldens
}

/// Go's `yaml.Unmarshal(in, &node)` against the golden `node` / `error` (a null `node` without
/// an error is Go's zero `Node`). Returns the mismatch, if any.
fn check_unmarshal(g: &ParseGolden) -> Result<(), String> {
    match (unmarshal_node(&g.input()), &g.error) {
        (Err(e), Some(want)) if e.to_string() == *want => Ok(()),
        (Err(e), want) => Err(format!("error {:?}, want {want:?}", e.to_string())),
        (Ok(_), Some(want)) => Err(format!("no error, want {want:?}")),
        (Ok(doc), None) => match &g.node {
            Some(want) => compare_node(&doc, doc.root(), want).map_err(|d| format!("node {d}")),
            None if doc == Document::default() => Ok(()),
            None => Err(format!("want the zero node, got {doc:?}")),
        },
    }
}

/// Go's `Decoder.Decode(&node)` until `io.EOF` or the first error (at most 1000 documents,
/// as the probe) over `reader`, against the golden `stream`.
fn check_stream<R: std::io::Read>(g: &ParseGolden, reader: R) -> Result<(), String> {
    let mut dec = Decoder::new(reader);
    let mut docs = Vec::new();
    let mut err = None;
    while docs.len() < 1000 {
        match dec.decode_node() {
            Ok(Some(doc)) => docs.push(doc),
            Ok(None) => break,
            Err(e) => {
                err = Some(e.to_string());
                break;
            }
        }
    }
    if err != g.stream.error {
        return Err(format!("stream error {err:?}, want {:?}", g.stream.error));
    }
    if docs.len() != g.stream.nodes.len() {
        return Err(format!(
            "{} stream documents, want {}",
            docs.len(),
            g.stream.nodes.len()
        ));
    }
    for (i, (doc, want)) in docs.iter().zip(&g.stream.nodes).enumerate() {
        compare_node(doc, doc.root(), want).map_err(|d| format!("stream node {i}: {d}"))?;
    }
    Ok(())
}

/// A reader handing out one byte per `read`, so every refill of the parser's raw buffer is a
/// separate read (the probe reads through a `bytes.Reader`).
struct OneByte<'a>(&'a [u8]);

impl std::io::Read for OneByte<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.split_first() {
            Some((&b, rest)) if !buf.is_empty() => {
                buf[0] = b;
                self.0 = rest;
                Ok(1)
            }
            _ => Ok(0),
        }
    }
}

fn report(what: &str, failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} of {} goldens fail ({what}):\n{}",
        failures.len(),
        PARSE_GOLDEN_COUNT + DIFF_GOLDEN_COUNT,
        failures.join("\n")
    );
}

#[test]
fn unmarshal_node_matches_every_golden() {
    let failures: Vec<String> = all_goldens()
        .iter()
        .filter_map(|g| check_unmarshal(g).err().map(|e| format!("{}: {e}", g.case)))
        .collect();
    report("unmarshal_node", &failures);
}

#[test]
fn decoder_stream_matches_every_golden() {
    let failures: Vec<String> = all_goldens()
        .iter()
        .filter_map(|g| {
            let input = g.input();
            check_stream(g, input.as_slice())
                .err()
                .map(|e| format!("{}: {e}", g.case))
        })
        .collect();
    report("Decoder", &failures);
}

#[test]
fn decoder_stream_is_independent_of_read_sizes() {
    let failures: Vec<String> = all_goldens()
        .iter()
        .filter_map(|g| {
            let input = g.input();
            check_stream(g, OneByte(&input))
                .err()
                .map(|e| format!("{}: {e}", g.case))
        })
        .collect();
    report("Decoder, one byte per read", &failures);
}

#[test]
fn golden_case_counts() {
    let goldens = all_goldens();
    let errors = goldens.iter().filter(|g| g.error.is_some()).count();
    let nodes = goldens.iter().filter(|g| g.node.is_some()).count();
    let zero = goldens.len() - errors - nodes;
    let streams: usize = goldens.iter().map(|g| g.stream.nodes.len()).sum();
    let stream_errors = goldens.iter().filter(|g| g.stream.error.is_some()).count();
    // Every golden is either a node, an error or Go's zero node; print the split so a probe
    // rerun that changes it is visible in the test log.
    println!(
        "{} goldens: {nodes} nodes, {errors} errors, {zero} zero nodes; \
         {streams} stream documents, {stream_errors} stream errors",
        goldens.len()
    );
    assert_eq!(
        nodes + errors + zero,
        PARSE_GOLDEN_COUNT + DIFF_GOLDEN_COUNT
    );
}

#[test]
fn decoder_aliases_into_earlier_documents_and_repeats_eof() {
    // Go's Decoder keeps one parser, so anchors persist across documents; the returned
    // document carries the alias target along (Go: a pointer into the earlier tree).
    let mut dec = Decoder::new(&b"a: &x 1\n---\nb: *x\n"[..]);
    let first = dec.decode_node().unwrap().unwrap();
    let second = dec.decode_node().unwrap().unwrap();
    let map = second[second.root()].content[0];
    let alias = second[map].content[1];
    assert_eq!(second[alias].kind, Kind::Alias);
    let target = second[alias].alias.expect("alias target");
    assert_eq!(
        (
            second[target].anchor.as_str(),
            second[target].value.as_str()
        ),
        ("x", "1")
    );
    assert_eq!(first[first[first.root()].content[0]].content.len(), 2);
    assert!(dec.decode_node().unwrap().is_none());
    assert!(dec.decode_node().unwrap().is_none());
}

#[test]
fn unmarshal_node_reads_only_the_first_document() {
    let doc = unmarshal_node(b"a\n--- b\n").unwrap();
    let root = &doc[doc.root()];
    assert_eq!(root.kind, Kind::Document);
    assert_eq!(doc[root.content[0]].value, "a");
    assert_eq!(unmarshal_node(b"").unwrap(), Document::default());
}
