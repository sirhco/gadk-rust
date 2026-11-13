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

//! Replays the probe's serde goldens as decode → encode round trips (Task 11).
//!
//! For every serde golden with an input, the probe ran `yaml.Unmarshal(in, &s)` into the Go
//! shape, then `yaml.Marshal(s)` (`marshal_b64`) and `node.Encode(s)` (`node`) on whatever
//! `Unmarshal` left in `s` — also after a `*TypeError`, and the zero value after a syntax
//! error. This target does the same through the port: `unmarshal_node` + `from_node_partial`
//! (Go's partial value next to its `TypeError`; R7), then `marshal` and `to_node`, so no
//! hand-built value is involved (`golden_serde` covers those).

mod support;

use gadk_goyaml::unmarshal_node;
use support::golden::{SERDE_GOLDEN_COUNT, SerdeGolden, compare_node, serde_goldens};
use support::shapes::{SHAPES, ShapeValue, options};

/// Serde goldens with an input (all but the eight `zero` cases, one per shape).
const ROUND_TRIP_COUNT: usize = 69;

/// Shows bytes as text for failure messages.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// What Go's shape holds after `yaml.Unmarshal(input, &s)`: the partly filled value next to a
/// `TypeError`, or the zero value when the input does not parse. Returns the value and whether
/// decoding reported an error.
fn decode(g: &SerdeGolden, input: &[u8]) -> Result<(ShapeValue, bool), String> {
    let opts = options();
    match unmarshal_node(input) {
        Ok(doc) => {
            let (value, terr) = ShapeValue::from_node_partial(&g.shape, &doc, doc.root(), &opts)
                .map_err(|e| format!("from_node_partial: {e}"))?;
            Ok((value, terr.is_some()))
        }
        Err(_) => Ok((ShapeValue::zero(&g.shape), true)),
    }
}

/// Decodes one golden's input and re-encodes it; returns the failures.
fn check_round_trip(g: &SerdeGolden, input: &[u8]) -> Vec<String> {
    let name = format!("{}-{}", g.shape, g.case);
    let (value, decode_failed) = match decode(g, input) {
        Ok(decoded) => decoded,
        Err(e) => return vec![format!("{name}: {e}")],
    };
    let mut failures = Vec::new();
    if decode_failed != g.decode_error.is_some() {
        failures.push(format!(
            "{name}: decode error reported: {decode_failed}, Go's: {:?}",
            g.decode_error
        ));
    }
    let opts = options();
    // `yaml.Marshal(s)`.
    match (value.marshal(&opts), g.marshal(), &g.marshal_error) {
        (Ok(got), Some(want), None) if got == want => {}
        (Err(e), None, Some(want)) if e.to_string() == *want => {}
        (got, want, want_err) => failures.push(format!(
            "{name}: marshal: got {:?}, want {:?} / error {want_err:?}",
            got.map(|b| show(&b)).map_err(|e| e.to_string()),
            want.map(|b| show(&b)),
        )),
    }
    // `node.Encode(s)`.
    match (value.to_node(&opts), &g.node) {
        (Ok(doc), Some(want)) => {
            if let Err(diff) = compare_node(&doc, doc.root(), want) {
                failures.push(format!("{name}: to_node: node mismatch at {diff}"));
            }
        }
        (Err(e), None) if g.marshal_error.as_deref() == Some(&e.to_string()) => {}
        (got, want) => failures.push(format!(
            "{name}: to_node: got {:?}, want {want:?}",
            got.map_err(|e| e.to_string()).map(|_| "a node"),
        )),
    }
    failures
}

#[test]
fn serde_goldens_round_trip_like_go() {
    let goldens = serde_goldens();
    assert_eq!(goldens.len(), SERDE_GOLDEN_COUNT);
    let mut failures = Vec::new();
    let (mut round_trips, mut zero, mut with_errors) = (0, 0, 0);
    for g in &goldens {
        assert!(
            SHAPES.contains(&g.shape.as_str()),
            "unknown shape {}",
            g.shape
        );
        let Some(input) = g.input() else {
            // The zero case: nothing to decode (`golden_serde` encodes the zero value).
            assert_eq!(g.case, "zero", "{}-{}: no input", g.shape, g.case);
            zero += 1;
            continue;
        };
        round_trips += 1;
        if g.decode_error.is_some() {
            with_errors += 1;
        }
        failures.extend(check_round_trip(g, &input));
    }
    assert!(
        failures.is_empty(),
        "{} round-trip failures over {round_trips} serde goldens:\n{}",
        failures.len(),
        failures.join("\n")
    );
    println!(
        "golden_serde_roundtrip: {round_trips} round trips ({with_errors} after a decode error), \
         {zero} zero cases skipped"
    );
    assert_eq!(round_trips, ROUND_TRIP_COUNT);
    assert_eq!(zero, SHAPES.len());
    assert_eq!(round_trips + zero, SERDE_GOLDEN_COUNT);
}
