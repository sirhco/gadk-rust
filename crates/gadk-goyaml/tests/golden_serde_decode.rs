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
use support::golden::{
    SERDE_GOLDEN_COUNT, SerdeGolden, diff_goldens, parse_goldens, serde_goldens,
};
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

/// Whether an error text names a Go type: a type-error target (the text after ` into `) or an
/// `invalid map key` rendering in Go syntax.
fn names_go_type(text: &str) -> bool {
    let go_target = |t: &str| {
        let t = t.trim_start();
        let digits = |rest: &str| rest.chars().all(|c| c.is_ascii_digit());
        let word: String = t
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        ["[]", "map[", "main.", "interface", "*"]
            .iter()
            .any(|p| t.starts_with(p))
            || word
                .strip_prefix("uint")
                .or_else(|| word.strip_prefix("int"))
                .is_some_and(digits)
            || word
                .strip_prefix("float")
                .is_some_and(|r| !r.is_empty() && digits(r))
            || word == "string"
    };
    text.split(" into ").skip(1).any(go_target)
        || text.split("invalid map key: ").skip(1).any(go_target)
        || text.contains("interface {}")
}

#[test]
fn golden_errors_name_rust_types() {
    for g in &serde_goldens() {
        let text = g.decode_error.as_deref().unwrap_or("");
        assert!(!names_go_type(text), "{}-{}: {text}", g.shape, g.case);
    }
    for g in parse_goldens().iter().chain(&diff_goldens()) {
        let text = g.value_error.as_deref().unwrap_or("");
        assert!(!names_go_type(text), "{text}");
    }
    // The check itself.
    for go in [
        "line 1: cannot unmarshal !!str `x` into int64",
        "line 1: cannot unmarshal !!str `x` into int",
        "line 1: cannot unmarshal !!str `x` into float64",
        "line 1: cannot unmarshal !!seq into string",
        "line 1: cannot unmarshal !!str `x` into []uint8",
        "line 1: cannot unmarshal !!str `x` into main.Inner",
        "line 1: cannot unmarshal !!str `x` into map[string]interface {}",
        "yaml: invalid map key: []interface {}{\"a\"}",
    ] {
        assert!(names_go_type(go), "{go}");
    }
    for rust in [
        "line 1: cannot unmarshal !!str `x` into i64",
        "line 1: cannot unmarshal !!seq into String",
        "line 1: cannot unmarshal !!str `x` into Vec<u8>",
        "line 1: cannot unmarshal !!str `x` into a sequence",
        "line 1: cannot unmarshal !!str `x` into Inner",
        "yaml: invalid map key: [\"a\", \"b\"]",
        "yaml: invalid map key: {\".\": null}",
    ] {
        assert!(!names_go_type(rust), "{rust}");
    }
}
