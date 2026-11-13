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

//! `go_format_float_g` against Go's `strconv.FormatFloat(f, 'g', -1, 32|64)`: replays
//! `testdata/go_format_float_g.json`, written by `scripts/gen-go-goyaml` (spec R9).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_platform::go_format_float_g;
use serde_json::Value;

const TABLE: &str = include_str!("testdata/go_format_float_g.json");

/// The f64 a table row's IEEE bits stand for (32-bit rows widened, as R9 passes them).
fn row_value(bits: u64, x: &str) -> f64 {
    let hex = x.strip_prefix("0x").expect("0x prefix");
    match bits {
        32 => f64::from(f32::from_bits(
            u32::from_str_radix(hex, 16).expect("u32 hex"),
        )),
        64 => f64::from_bits(u64::from_str_radix(hex, 16).expect("u64 hex")),
        other => panic!("bits {other}"),
    }
}

#[test]
fn go_format_float_g_matches_go_table() {
    let rows: Vec<Value> = serde_json::from_str(TABLE).expect("table is JSON");
    assert_eq!(
        rows.len(),
        3729,
        "row count (check-fixtures.sh counts it too)"
    );
    let mut failures = Vec::new();
    for row in &rows {
        let bits = row["bits"].as_u64().expect("bits");
        let x = row["x"].as_str().expect("x");
        let want = row["out"].as_str().expect("out");
        let got = go_format_float_g(row_value(bits, x), u32::try_from(bits).expect("bits"));
        if got != want {
            failures.push(format!("bits={bits} x={x}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} rows differ:\n{}",
        failures.len(),
        rows.len(),
        failures.join("\n")
    );
}

/// The cases spec R9 names, spelled out so a table regeneration cannot drop them.
#[test]
fn go_format_float_g_named_cases() {
    for (f, bits, want) in [
        (0.0, 64, "0"),
        (-0.0, 64, "-0"),
        (-0.0, 32, "-0"),
        (f64::INFINITY, 64, "+Inf"),
        (f64::NEG_INFINITY, 32, "-Inf"),
        (f64::NAN, 64, "NaN"),
        (f64::NAN, 32, "NaN"),
        (1e-5, 64, "1e-05"),
        (1e-4, 64, "0.0001"),
        (1e5, 64, "100000"),
        (1e6, 64, "1e+06"),
        (123_456.0, 64, "123456"),
        (1_234_567.0, 64, "1.234567e+06"),
        (1e21, 64, "1e+21"),
        (0.1, 64, "0.1"),
        (f64::from(0.1_f32), 32, "0.1"),
        (f64::from(0.1_f32), 64, "0.10000000149011612"),
        (f64::from(0.7_f32), 32, "0.7"),
        (f64::from(f32::MAX), 32, "3.4028235e+38"),
        (f64::from(f32::from_bits(1)), 32, "1e-45"),
        // Finite float64s that round to ±Inf as float32 (past f32::MAX + half an ulp).
        (1e300, 32, "+Inf"),
        (-1e300, 32, "-Inf"),
        (3.402_823_6e38, 32, "+Inf"),
        (-3.402_823_6e38, 32, "-Inf"),
        // Above f32::MAX but below the halfway point: rounds down to f32::MAX.
        (3.402_823_5e38, 32, "3.4028235e+38"),
        (f64::from_bits(1), 64, "5e-324"),
        (2.225_073_858_507_201e-308, 64, "2.225073858507201e-308"),
        // An exact decimal tie (…520.625): Go keeps the even digit.
        (141_016_749_166_520.0 + 0.625, 64, "1.4101674916652062e+14"),
        // Powers of two, where the nearest decimal at the shortest length does not round-trip.
        (2f64.powi(-1017), 64, "7.120236347223045e-307"),
        (f64::from(2f32.powi(-96)), 32, "1.2621775e-29"),
    ] {
        assert_eq!(
            go_format_float_g(f, bits),
            want,
            "FormatFloat({f:e}, 'g', -1, {bits})"
        );
    }
}
