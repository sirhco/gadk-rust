// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! Port of yaml.v3 v3.0.1 `limit_test.go` (Task 10). Its `Benchmark*` functions and the
//! `benchmark` helper are not ported (benchmarks are outside the ledger).

use crate::error::walk_depth_exceeded;
use crate::{GoValue, Options, unmarshal, unmarshal_node};

/// One `limitTests` row.
struct LimitTest {
    name: &'static str,
    data: Vec<u8>,
    /// Go's expected error (empty: none).
    error: &'static str,
    /// The input nests deeper than plan decision D7's 127 levels: parsing accepts it as Go
    /// does, but decoding into a `GoValue` fails with `yaml: exceeded max depth of 127` where
    /// Go succeeds (Divergence, ledger 6c-1).
    d7: bool,
}

/// Go: `limitTests`.
fn limit_tests() -> Vec<LimitTest> {
    let row = |name, data: String, error, d7| LimitTest {
        name,
        data: data.into_bytes(),
        error,
        d7,
    };
    vec![
        row(
            "1000kb of maps with 100 aliases",
            format!(
                "{{a: &a [{{a}}{}], b: &b [*a{}]}}",
                ",{a}".repeat(1000 * 1024 / 4 - 100),
                ",*a".repeat(99)
            ),
            "yaml: document contains excessive aliasing",
            false,
        ),
        row(
            "1000kb of deeply nested slices",
            "[".repeat(1000 * 1024),
            "yaml: exceeded max depth of 10000",
            false,
        ),
        row(
            "1000kb of deeply nested maps",
            format!("x: {}", "{".repeat(1000 * 1024)),
            "yaml: exceeded max depth of 10000",
            false,
        ),
        row(
            "1000kb of deeply nested indents",
            "- ".repeat(1000 * 1024),
            "yaml: exceeded max depth of 10000",
            false,
        ),
        row(
            "1000kb of 1000-indent lines",
            ("- ".repeat(1000) + "\n").repeat(1024 / 2),
            "",
            true,
        ),
        row(
            "1kb of maps",
            format!("a: &a [{{a}}{}]", ",{a}".repeat(1024 / 4 - 1)),
            "",
            false,
        ),
        row(
            "10kb of maps",
            format!("a: &a [{{a}}{}]", ",{a}".repeat(10 * 1024 / 4 - 1)),
            "",
            false,
        ),
        row(
            "100kb of maps",
            format!("a: &a [{{a}}{}]", ",{a}".repeat(100 * 1024 / 4 - 1)),
            "",
            false,
        ),
        row(
            "1000kb of maps",
            format!("a: &a [{{a}}{}]", ",{a}".repeat(1000 * 1024 / 4 - 1)),
            "",
            false,
        ),
        row(
            "1000kb slice nested at max-depth",
            format!(
                "{}1{}{}",
                "[".repeat(10000),
                ",1".repeat(1000 * 1024 / 2 - 20000 - 1),
                "]".repeat(10000)
            ),
            "",
            true,
        ),
        row(
            "1000kb slice nested in maps at max-depth",
            format!(
                "{{a,b:\n{} [1{}]{}",
                " {a,b:".repeat(10000 - 2),
                ",1".repeat(1000 * 1024 / 2 - 6 * 10000 - 1),
                "}".repeat(10000 - 1)
            ),
            "",
            true,
        ),
        row(
            "1000kb of 10000-nested lines",
            format!(
                "- {}{}
",
                "[".repeat(10000),
                "]".repeat(10000)
            )
            .repeat(1000 * 1024 / 20000),
            "",
            true,
        ),
    ]
}

// parity: adapted: rows nesting deeper than 127 levels expect plan decision D7's `yaml: exceeded max depth of 127` (Go decodes them) after checking that the parse alone succeeds; Go's testing.Short() skip is dropped
#[test]
fn test_limits() {
    // Go: limit_test.go TestLimits
    for tc in limit_tests() {
        let result = unmarshal::<GoValue>(&tc.data, &Options::default());
        if tc.d7 {
            assert!(tc.error.is_empty(), "testcase: {}", tc.name);
            // Divergence (ledger 6c-1): D7 — the iterative composer accepts Go's depth, the
            // recursive GoValue decode stops at 127 levels.
            unmarshal_node(&tc.data)
                .unwrap_or_else(|e| panic!("testcase: {}: parse: {e}", tc.name));
            let err = result.expect_err(tc.name);
            assert_eq!(
                err.to_string(),
                walk_depth_exceeded().to_string(),
                "testcase: {}",
                tc.name
            );
        } else if !tc.error.is_empty() {
            let err = result.expect_err(tc.name);
            assert_eq!(err.to_string(), tc.error, "testcase: {}", tc.name);
        } else {
            result.unwrap_or_else(|e| panic!("testcase: {}: {e}", tc.name));
        }
    }
}
