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

//! Port of yaml.v3 v3.0.1 `example_embedded_test.go`.
//!
//! An example showing how to unmarshal embedded structs from YAML. Go's `StructB` embeds
//! `StructA` with `yaml:",inline"`; in Rust the inlined struct's fields are listed directly in
//! the outer struct, in Go's field order (spec R8), and the `yaml:"a"` / `yaml:"b"` tags are the
//! serde names used verbatim ([`Options::verbatim_structs`]).

use std::fmt::Write as _;

use serde::Deserialize;

use crate::{Options, unmarshal};

/// Go's `StructB` with the inlined `StructA`'s field `A` (`yaml:"a"`) listed first.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StructB {
    a: String,
    b: String,
}

/// Go's `data`.
const DATA: &str = "
a: a string from struct A
b: a string from struct B
";

/// The [`Options`] standing in for Go's `yaml:"a"` / `yaml:"b"` tags.
fn options() -> Options {
    let mut opts = Options::default();
    opts.verbatim_structs.insert("StructB");
    opts
}

/// Go's `// Output:` block.
const OUTPUT: &str = "a string from struct A\na string from struct B\n";

// parity: adapted: Go's embedded `StructA` with `yaml:",inline"` is listed field by field in StructB (spec R8); the example's printed lines are collected and compared with its `// Output:` block
#[test]
fn example_unmarshal_embedded() {
    assert_eq!(
        DATA.as_bytes(),
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/corpus/parse/example-data.yaml"
        ))
        .unwrap()
    );
    let b: StructB = unmarshal(DATA.as_bytes(), &options())
        .unwrap_or_else(|err| panic!("cannot unmarshal data: {err}"));
    let mut out = String::new();
    writeln!(out, "{}", b.a).unwrap();
    writeln!(out, "{}", b.b).unwrap();
    assert_eq!(out, OUTPUT);
}
