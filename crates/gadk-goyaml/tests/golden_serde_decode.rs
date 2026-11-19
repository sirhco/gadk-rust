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

//! Replays the probe's serde goldens on decode: Go's `yaml.Unmarshal(in, &s)` error text
//! (`decode_error`, with Rust type names) against `gadk_goyaml::unmarshal`, and the value Go
//! leaves in `s` (also after a `TypeError`) against `gadk_goyaml::from_node_partial`,
//! compared with `ShapeValue::assert_same`.

mod support;

use gadk_goyaml::unmarshal_node;
use support::golden::{SERDE_GOLDEN_COUNT, SerdeGolden, serde_goldens};
use support::shapes::{ShapeValue, options};

/// The `decode_error` the port must return for `g`. The recorded texts were rewritten once
/// from yaml.v3's Go type names to the Rust names gadk-goyaml prints (`i64`, `String`,
/// `Vec<u8>`, the bare serde struct name, `a sequence`, `a mapping`).
fn expected_error(g: &SerdeGolden) -> Option<String> {
    g.decode_error.clone()
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
fn golden_errors_name_rust_types() {
    for g in &serde_goldens() {
        let case = format!("{}-{}", g.shape, g.case);
        let text = g.decode_error.as_deref().unwrap_or("");
        for go in [
            "main.",
            "yaml.Node",
            "interface {}",
            "[]uint8",
            "int64",
            "string",
        ] {
            assert!(!text.contains(go), "{case}: {text}");
        }
    }
}
