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

//! Replays the probe's serde goldens on decode (Task 8): Go's `yaml.Unmarshal(in, &s)` error
//! text (`decode_error`) against `gadk_goyaml::unmarshal`, and the value Go leaves in `s`
//! (also after a `TypeError`) against `gadk_goyaml::from_node_partial`, compared with
//! `ShapeValue::assert_same`.

mod support;

use gadk_goyaml::unmarshal_node;
use support::golden::{SERDE_GOLDEN_COUNT, SerdeGolden, serde_goldens};
use support::shapes::{ShapeValue, options};

/// The serde cases whose Go error text names a type serde cannot name: Go prints the
/// package-qualified struct (`main.Inner`) where serde gives the bare struct name, and the
/// element type of a slice (`[]yaml.Node`) where serde exposes none (`[]interface {}`).
/// Ledgered divergence (6c-1): "TypeError type names". Each entry is (`shape-case`, Go's
/// text, the port's text); the rest of the message is unchanged.
const TYPE_NAME_ADAPTATIONS: [(&str, &str, &str); 5] = [
    ("nested-inner-not-map", "main.Inner", "Inner"),
    ("nested-list-mismatch", "main.Inner", "Inner"),
    ("scalars-not-a-map", "main.Scalars", "Scalars"),
    ("scalars-scalar-root", "main.Scalars", "Scalars"),
    ("node-edges-not-seq", "[]yaml.Node", "[]interface {}"),
];

/// The `decode_error` the port must return for `g`: Go's text with the case's adaptation.
fn expected_error(g: &SerdeGolden) -> Option<String> {
    let name = format!("{}-{}", g.shape, g.case);
    let want = g.decode_error.clone()?;
    match TYPE_NAME_ADAPTATIONS
        .iter()
        .find(|(case, _, _)| *case == name)
    {
        Some((_, go, port)) => {
            assert!(want.contains(go), "{name}: Go's text {want:?} lacks {go:?}");
            Some(want.replace(go, port))
        }
        None => Some(want),
    }
}

/// `unmarshal` against the golden `decode_error`, then the decoded value against Go's.
fn check_decode(g: &SerdeGolden) -> Result<(), String> {
    let name = format!("{}-{}", g.shape, g.case);
    let opts = options();
    let Some(input) = g.input() else {
        // The zero case: nothing is decoded.
        ShapeValue::zero(&g.shape).assert_same(&ShapeValue::expected(&g.shape, &g.case), &name);
        return Ok(());
    };
    let got = ShapeValue::unmarshal(&g.shape, &input, &opts).map_err(|e| e.to_string());
    let want = expected_error(g);
    match (&got, &want) {
        (Err(e), Some(w)) if e == w => {}
        (Ok(_), None) => {}
        _ => return Err(format!("error {:?}, want {want:?}", got.err())),
    }
    // Go keeps what it decoded alongside a TypeError; a syntax error leaves the zero value.
    let value = match unmarshal_node(&input) {
        Ok(doc) => {
            let (v, terr) = ShapeValue::from_node_partial(&g.shape, &doc, doc.root(), &opts)
                .map_err(|e| format!("from_node_partial: {e}"))?;
            if terr.map(|t| t.to_string())
                != want.filter(|w| w.starts_with("yaml: unmarshal errors:"))
            {
                return Err("from_node_partial's TypeError differs from unmarshal's".into());
            }
            v
        }
        Err(_) => ShapeValue::zero(&g.shape),
    };
    let expected = ShapeValue::expected(&g.shape, &g.case);
    if value.canonical() != expected.canonical() {
        return Err(format!(
            "value\n got: {}\nwant: {}",
            value.canonical(),
            expected.canonical()
        ));
    }
    Ok(())
}

#[test]
fn golden_serde_decode_matches_go() {
    let goldens = serde_goldens();
    assert_eq!(goldens.len(), SERDE_GOLDEN_COUNT);
    let failures: Vec<String> = goldens
        .iter()
        .filter_map(|g| {
            check_decode(g)
                .err()
                .map(|e| format!("{}-{}: {e}", g.shape, g.case))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} serde goldens differ on decode:\n{}",
        failures.len(),
        goldens.len(),
        failures.join("\n")
    );
    let errors = goldens.iter().filter(|g| g.decode_error.is_some()).count();
    println!(
        "golden_serde_decode: {} cases ({errors} with errors)",
        goldens.len()
    );
}

#[test]
fn type_name_adaptations_name_real_cases() {
    let goldens = serde_goldens();
    for (case, go, _) in TYPE_NAME_ADAPTATIONS {
        let g = goldens
            .iter()
            .find(|g| format!("{}-{}", g.shape, g.case) == case)
            .unwrap_or_else(|| panic!("no serde golden {case}"));
        assert!(
            g.decode_error.as_deref().is_some_and(|e| e.contains(go)),
            "{case}"
        );
    }
    // No other golden names a Go type serde cannot reproduce.
    for g in &goldens {
        let case = format!("{}-{}", g.shape, g.case);
        if TYPE_NAME_ADAPTATIONS.iter().any(|(c, _, _)| *c == case) {
            continue;
        }
        let text = g.decode_error.as_deref().unwrap_or("");
        assert!(
            !text.contains("main.") && !text.contains("yaml.Node"),
            "{case}: {text}"
        );
    }
}
