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

//! Replays the probe's `value` / `value_error` goldens: Go's `yaml.Unmarshal(in, &v)` with
//! `var v interface{}` against `gadk_goyaml::unmarshal::<GoValue>`, over every parse
//! and differential golden — including the alias-ratio limit cases (`alias-wWW-lLL`,
//! `limit-*`) and the self-containing anchors (`anchor 'a' value contains itself`).

mod support;

use gadk_goyaml::{GoValue, Options, unmarshal};
use support::golden::{
    DIFF_GOLDEN_COUNT, PARSE_GOLDEN_COUNT, ParseGolden, ValueDump, golden_paths, read_parse_golden,
    value_to_dump,
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

/// Go's decode of `g`'s input into `interface{}` against the golden `value` / `value_error`
/// (a null `value` without an error is Go's nil).
fn check_value(g: &ParseGolden) -> Result<(), String> {
    match (
        unmarshal::<GoValue>(&g.input(), &Options::default()),
        &g.value_error,
    ) {
        (Err(e), Some(want)) if e.to_string() == *want => Ok(()),
        (Err(e), want) => Err(format!("error {:?}, want {want:?}", e.to_string())),
        (Ok(_), Some(want)) => Err(format!("no error, want {want:?}")),
        (Ok(v), None) => {
            let want = g.value.clone().unwrap_or(ValueDump::Null);
            let got = value_to_dump(&v);
            if got == want {
                Ok(())
            } else {
                Err(format!(
                    "value\n got: {}\nwant: {}",
                    got.to_compact_json(),
                    want.to_compact_json()
                ))
            }
        }
    }
}

#[test]
fn golden_value_matches_go() {
    let goldens = all_goldens();
    let mut failures = Vec::new();
    let (mut values, mut errors) = (0, 0);
    for g in &goldens {
        match check_value(g) {
            Ok(()) if g.value_error.is_some() => errors += 1,
            Ok(()) => values += 1,
            Err(e) => failures.push(format!("{}: {e}", g.case)),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} value goldens differ:\n{}",
        failures.len(),
        goldens.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!("golden_value: {values} values, {errors} errors");
    assert_eq!(values + errors, PARSE_GOLDEN_COUNT + DIFF_GOLDEN_COUNT);
}

#[test]
fn golden_value_covers_aliases_and_limits() {
    // The cases the alias accounting must reproduce, by error text.
    let goldens = all_goldens();
    let count = |text: &str| {
        goldens
            .iter()
            .filter(|g| g.value_error.as_deref() == Some(text))
            .count()
    };
    assert_eq!(count("yaml: anchor 'a' value contains itself"), 3);
    assert_eq!(count("yaml: document contains excessive aliasing"), 5);
    // Each limit boundary: accepted just below, rejected just above.
    for (case, accepted) in [
        ("alias-w03-l06", true),
        ("alias-w03-l07", false),
        ("alias-w10-l03", true),
        ("alias-w10-l04", false),
        ("limit-100-aliases", true),
        ("limit-1kb-of-maps", true),
    ] {
        let g = goldens.iter().find(|g| g.case == case).expect(case);
        assert_eq!(g.value.is_some(), accepted, "{case}: Go's outcome");
        check_value(g).unwrap_or_else(|e| panic!("{case}: {e}"));
    }
}
