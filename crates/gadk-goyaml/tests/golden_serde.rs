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

//! Replays the probe's serde goldens for zero and hand-built values: `marshal_b64` and the
//! `to_node` dump (Task 9). Each golden's Go value is [`ShapeValue::expected`] (the zero value
//! for `zero` cases, else the value Go's `yaml.Unmarshal` left in the shape), built by hand, so
//! no decoding is involved; Task 11 replays the same goldens through decode → encode.

mod support;

use support::golden::{SERDE_GOLDEN_COUNT, SerdeGolden, compare_node, serde_goldens, show};
use support::shapes::{SHAPES, ShapeValue, options};

/// The Go value of a golden: the shape's zero value for `zero`, else the hand-built value.
fn go_value(g: &SerdeGolden) -> ShapeValue {
    if g.case == "zero" {
        let zero = ShapeValue::zero(&g.shape);
        zero.assert_same(&ShapeValue::expected(&g.shape, "zero"), &g.shape);
        zero
    } else {
        ShapeValue::expected(&g.shape, &g.case)
    }
}

/// Checks one golden's `marshal_b64`/`marshal_error` and `node`; returns the failures.
fn check_golden(g: &SerdeGolden) -> Vec<String> {
    let name = format!("{}-{}", g.shape, g.case);
    let v = go_value(g);
    let opts = options();
    let mut failures = Vec::new();
    // `yaml.Marshal(s)`.
    match (v.marshal(&opts), g.marshal(), &g.marshal_error) {
        (Ok(got), Some(want), None) if got == want => {}
        (Err(e), None, Some(want)) if e.to_string() == *want => {}
        (got, want, want_err) => failures.push(format!(
            "{name}: marshal: got {:?}, want {:?} / error {want_err:?}",
            got.map(|b| show(&b)).map_err(|e| e.to_string()),
            want.map(|b| show(&b)),
        )),
    }
    // `node.Encode(s)`.
    match (v.to_node(&opts), &g.node) {
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
fn serde_goldens_marshal_and_to_node() {
    let goldens = serde_goldens();
    assert_eq!(goldens.len(), SERDE_GOLDEN_COUNT);
    let mut failures = Vec::new();
    for g in &goldens {
        assert!(
            SHAPES.contains(&g.shape.as_str()),
            "unknown shape {}",
            g.shape
        );
        failures.extend(check_golden(g));
    }
    assert!(
        failures.is_empty(),
        "{} of {} serde goldens failed:\n{}",
        failures.len(),
        goldens.len(),
        failures.join("\n")
    );
}

#[test]
fn zero_values_of_every_shape() {
    let opts = options();
    for shape in SHAPES {
        let g = serde_goldens()
            .into_iter()
            .find(|g| g.shape == shape && g.case == "zero")
            .unwrap_or_else(|| panic!("{shape}-zero golden"));
        let got = ShapeValue::zero(shape)
            .marshal(&opts)
            .unwrap_or_else(|e| panic!("{shape}: {e}"));
        assert_eq!(
            show(&got),
            show(&g.marshal().unwrap_or_default()),
            "{shape}"
        );
    }
}
