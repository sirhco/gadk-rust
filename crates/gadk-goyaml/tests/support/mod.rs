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

//! Shared test support for the golden replays (plan Task 5): golden loaders (Appendix
//! C.golden), NodeDump / ValueDump conversions and comparisons (`golden`), and the serde
//! shape mirrors with their `Options` and expected values (`shapes`).
//!
//! Every `tests/*.rs` target includes this module (`mod support;`) and uses a different part
//! of it. Its own self-tests run once, in the `golden_parse` target ([`runs_self_tests`]).
#![allow(dead_code)] // Each test target uses a different part of the support module.

pub mod golden;
pub mod shapes;

/// Whether this test target runs the support module's self-tests: only `golden_parse`, so the
/// golden-wide checks run once rather than in all seven targets that include `support`.
#[cfg(test)]
pub fn runs_self_tests() -> bool {
    env!("CARGO_CRATE_NAME") == "golden_parse"
}
