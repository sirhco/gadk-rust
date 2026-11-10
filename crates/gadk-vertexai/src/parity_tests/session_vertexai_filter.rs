// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Ports of ADK-Go `session/vertexai/filter_test.go`.

use crate::session::client::quote_filter_literal;

#[test]
fn test_quote_filter_literal() {
    let tests = [
        ("plain value", "alice", r#""alice""#),
        // A double quote in the value must not break out of the literal and append an OR
        // predicate that would return every user's sessions.
        (
            "quote injection is neutralized",
            r#"attacker" OR userId!=""#,
            r#""attacker\" OR userId!=\"""#,
        ),
        ("backslash only", r"\", r#""\\""#),
        ("empty string", "", r#""""#),
    ];
    for (name, input, want) in tests {
        assert_eq!(
            quote_filter_literal(input),
            want,
            "{name}: quote_filter_literal({input:?})"
        );
    }
}
