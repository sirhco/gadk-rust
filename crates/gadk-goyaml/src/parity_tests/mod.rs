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

//! Ports of yaml.v3 v3.0.1's test files (ledger 6c-1): one module per Go
//! file, `yaml_v3_<file>`, holding `fn test_<snake of TestName>` (and
//! `example_unmarshal_embedded`).

mod yaml_v3_decode;
mod yaml_v3_encode;
mod yaml_v3_example_embedded;
mod yaml_v3_limit;
mod yaml_v3_node;

use crate::GoValue;

/// The probe's copy of a Go test input or output (`testdata/corpus/parse/<case>.yaml`).
pub(super) fn corpus(case: &str) -> Vec<u8> {
    let path = format!(
        "{}/testdata/corpus/parse/{case}.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Go's `string` inside an `interface{}`.
pub(super) fn s(v: &str) -> GoValue {
    GoValue::String(v.to_string())
}

/// Go's `int` inside an `interface{}`.
pub(super) fn int(v: i64) -> GoValue {
    GoValue::Int(v)
}

/// Go's `map[string]interface{}` inside an `interface{}`, entries in document order.
pub(super) fn gmap<const N: usize>(entries: [(&str, GoValue); N]) -> GoValue {
    GoValue::Map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

/// Checks that a Go table's rows arrive in Go's order and that every row is either ported or
/// skipped.
#[derive(Default)]
pub(super) struct RowCount {
    next: usize,
    ported: usize,
    skipped: Vec<usize>,
}

impl RowCount {
    pub(super) fn ported(&mut self, i: usize) {
        assert_eq!(i, self.next, "row {i} out of order");
        self.next += 1;
        self.ported += 1;
    }

    pub(super) fn skipped(&mut self, i: usize) {
        assert_eq!(i, self.next, "row {i} out of order");
        self.next += 1;
        self.skipped.push(i);
    }

    pub(super) fn finish(&self, total: usize, skipped: &[usize]) {
        assert_eq!(self.next, total);
        assert_eq!(self.skipped, skipped);
        assert_eq!(self.ported + self.skipped.len(), total);
    }
}
