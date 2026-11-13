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

//! Port of ADK-Go `internal/configurable/conformance/replayplugin/replay_plugin_internal_test.go` (Task 6).

use pretty_assertions::assert_eq;

use crate::conformance::replay::matching::normalize_description;

#[test]
fn test_normalize_description() {
    let tests: [(&str, &str, &str); 9] = [
        (
            "Simple single line",
            "Simple description",
            "Simple description",
        ),
        (
            "Single line with leading and trailing spaces",
            "   Simple description   ",
            "Simple description",
        ),
        (
            "Multiple lines with indentation",
            "  Line 1  \n  Line 2  ",
            "Line 1\nLine 2",
        ),
        (
            "Empty lines at start and end",
            "\n\nLine 1\nLine 2\n\n",
            "Line 1\nLine 2",
        ),
        (
            "Whitespace-only lines at start and end",
            "   \n  \nLine 1\nLine 2\n  \n   ",
            "Line 1\nLine 2",
        ),
        (
            "Empty lines in the middle are preserved",
            "Line 1\n\nLine 2",
            "Line 1\n\nLine 2",
        ),
        ("Empty input", "", ""),
        ("Whitespace-only input", "    \n   \n  ", ""),
        (
            "Complex formatting with tabs and newlines",
            "\t\n  Line 1 \t \n\tLine 2\n  \t\n",
            "Line 1\nLine 2",
        ),
    ];
    for (name, input, expected) in tests {
        let actual = normalize_description(input);
        assert_eq!(actual, expected, "{name}: normalizeDescription({input:?})");
    }
}
