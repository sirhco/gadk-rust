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

//! Replays the seeded differential corpus (`testdata/golden/diff/`, 32 seeds x 20 cases):
//! parse and emit (Task 11, spec R12).
//!
//! Each golden holds a value the probe generated with Go's `math/rand` (`source`), Go's
//! `yaml.Marshal` of it (the input), the node Go parses back (`node` / `error`), Go's
//! re-emission of that node (`marshal_b64`, `indent2_b64`), the `Decoder`/`Encoder` stream
//! view and the `interface{}` decode. This target replays all of them end to end: the port's
//! `marshal` of the generated value, `unmarshal_node` of Go's bytes, `marshal_node` / `Encoder`
//! of the port's own parse, the stream, and `unmarshal::<GoValue>`.

mod support;

use gadk_goyaml::{
    Decoder, Document, Encoder, Error, GoValue, Options, marshal, marshal_node, unmarshal,
    unmarshal_node,
};
use support::golden::{
    DIFF_GOLDEN_COUNT, ParseGolden, ValueDump, compare_node, diff_goldens, value_from_dump,
    value_to_dump,
};

/// The seed list (`testdata/corpus/differential.json`) and cases per seed (16 scalars + 4
/// trees).
const SEEDS: usize = 32;
const CASES_PER_SEED: usize = 20;

/// Shows bytes as text for failure messages.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Compares bytes (or an error) with the golden's, naming `what` on mismatch.
fn check_bytes(
    what: &str,
    got: Result<Vec<u8>, Error>,
    want: Option<Vec<u8>>,
) -> Result<(), String> {
    match (got, want) {
        (Ok(got), Some(want)) if got == want => Ok(()),
        (got, want) => Err(format!(
            "{what}: got {:?}, want {:?}",
            got.map(|b| show(&b)).map_err(|e| e.to_string()),
            want.map(|b| show(&b)),
        )),
    }
}

/// One `Encoder` (indent 0 = Go's default) encoding every document, then `Close`.
fn encode_all(docs: &[Document], indent: usize) -> Result<Vec<u8>, Error> {
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

/// Emit: the port's `marshal` of the generated value must be Go's `Marshal` bytes (the input).
fn check_source(g: &ParseGolden) -> Result<(), String> {
    let source = g.source.as_ref().ok_or("no source value")?;
    let value: GoValue = value_from_dump(source);
    check_bytes(
        "marshal(source)",
        marshal(&value, &Options::default()),
        Some(g.input()),
    )
}

/// Parse: `unmarshal_node` of Go's bytes against `node` / `error`; then emit the port's own
/// node again (`marshal_b64`, `indent2_b64`). Returns whether the input parsed.
fn check_parse_and_reemit(g: &ParseGolden) -> Result<bool, String> {
    let doc = match (unmarshal_node(&g.input()), &g.error) {
        (Err(e), Some(want)) if e.to_string() == *want => return Ok(false),
        (Err(e), want) => return Err(format!("parse error {:?}, want {want:?}", e.to_string())),
        (Ok(_), Some(want)) => return Err(format!("parsed, want error {want:?}")),
        (Ok(doc), None) => doc,
    };
    let want = g.node.as_ref().ok_or("golden has neither node nor error")?;
    compare_node(&doc, doc.root(), want).map_err(|d| format!("node {d}"))?;
    check_bytes("marshal_node", marshal_node(&doc, doc.root()), g.marshal())?;
    check_bytes(
        "indent2",
        encode_all(std::slice::from_ref(&doc), 2),
        g.indent2(),
    )?;
    Ok(true)
}

/// The `Decoder` stream (nodes, first error) and its re-encoding by one default `Encoder`.
fn check_stream(g: &ParseGolden) -> Result<(), String> {
    let input = g.input();
    let mut dec = Decoder::new(input.as_slice());
    let mut docs = Vec::new();
    let mut err = None;
    loop {
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
    match g.stream.marshal() {
        Some(want) => check_bytes("stream re-encode", encode_all(&docs, 0), Some(want)),
        // Go's `Close` without any `Encode`.
        None if docs.is_empty() => Ok(()),
        None => Err("golden has stream nodes but no stream bytes".into()),
    }
}

/// `unmarshal::<GoValue>` against `value` / `value_error`.
fn check_value(g: &ParseGolden) -> Result<(), String> {
    match (
        unmarshal::<GoValue>(&g.input(), &Options::default()),
        &g.value_error,
    ) {
        (Err(e), Some(want)) if e.to_string() == *want => Ok(()),
        (Err(e), want) => Err(format!("value error {:?}, want {want:?}", e.to_string())),
        (Ok(_), Some(want)) => Err(format!("value decoded, want error {want:?}")),
        (Ok(v), None) => {
            let want = g.value.clone().unwrap_or(ValueDump::Null);
            let got = value_to_dump(&v);
            if got == want {
                Ok(())
            } else {
                Err(format!(
                    "value got {}, want {}",
                    got.to_compact_json(),
                    want.to_compact_json()
                ))
            }
        }
    }
}

#[test]
fn differential_replays_every_case() {
    let goldens = diff_goldens();
    assert_eq!(goldens.len(), DIFF_GOLDEN_COUNT);
    assert_eq!(goldens.len(), SEEDS * CASES_PER_SEED);
    assert_eq!(goldens.len(), 640);

    let mut failures = Vec::new();
    let (mut parsed, mut parse_errors) = (0, 0);
    for g in &goldens {
        match check_parse_and_reemit(g) {
            Ok(true) => parsed += 1,
            Ok(false) => parse_errors += 1,
            Err(e) => failures.push(format!("{}: {e}", g.case)),
        }
        for check in [check_source(g), check_stream(g), check_value(g)] {
            if let Err(e) = check {
                failures.push(format!("{}: {e}", g.case));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} differential failures over {} cases:\n{}",
        failures.len(),
        goldens.len(),
        failures.join("\n")
    );
    println!(
        "differential: {} cases, {parsed} parsed, {parse_errors} parse errors",
        goldens.len()
    );
    assert_eq!(parsed + parse_errors, 640);
    // yaml.v3 cannot re-parse five of its own outputs (literal blocks with an indentation
    // indicator for text starting with a space; see the plan notes): the port reproduces that.
    assert_eq!(parse_errors, 5);
}

#[test]
fn differential_cases_cover_every_seed() {
    let goldens = diff_goldens();
    for seed in 1..=SEEDS {
        for n in 0..CASES_PER_SEED {
            let case = format!("{seed}-{n}");
            let g = goldens
                .iter()
                .find(|g| g.case == case)
                .unwrap_or_else(|| panic!("no differential golden {case}"));
            // Cases 0-15 are scalar strings, 16-19 trees.
            let source = g
                .source
                .as_ref()
                .unwrap_or_else(|| panic!("{case}: no source"));
            assert_eq!(
                matches!(source, ValueDump::String(_)),
                n < 16,
                "{case}: {}",
                source.to_compact_json()
            );
        }
    }
}
