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
//! Checks the `gadk` facade exposes the eval crate under the `eval` feature.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(feature = "eval")]

#[test]
fn facade_exposes_eval() {
    let set = gadk::eval::EvalSet::from_yaml(
        "name: s\ncases:\n  - id: a\n    turns: [hi]\n    expect: [contains: hello]\n",
    )
    .unwrap();
    assert_eq!(set.cases[0].expect[0].kind(), "contains");
    assert_eq!(gadk::eval::Variant::new("v").name(), "v");
}
