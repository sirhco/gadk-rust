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

//! Port of ADK-Go `model/openaimodel/internal/shared/tools_test.go`.

use gadk_genai::{FunctionDeclaration, GoogleSearch, Tool};

use crate::shared::tools::ensure_function_tool_only;

#[test]
fn test_ensure_function_tool_only() {
    let tests: Vec<(&str, Option<Tool>, &str)> = vec![
        ("nil tool", None, "tool 0 is nil"),
        (
            "non-function tool",
            Some(Tool {
                google_search: Some(GoogleSearch::default()),
                ..Tool::default()
            }),
            "non-function tools",
        ),
        (
            "no functions",
            Some(Tool::default()),
            "does not declare any functions",
        ),
        (
            "valid",
            Some(Tool {
                function_declarations: vec![FunctionDeclaration {
                    name: "fn1".to_string(),
                    ..FunctionDeclaration::default()
                }],
                ..Tool::default()
            }),
            "",
        ),
    ];
    for (name, tool, want_err) in tests {
        let result = ensure_function_tool_only(0, tool.as_ref());
        if want_err.is_empty() {
            assert!(result.is_ok(), "{name}: expected no error, got {result:?}");
        } else {
            match result {
                Err(err) if err.to_string().contains(want_err) => {}
                other => panic!("{name}: expected error containing {want_err:?}, got: {other:?}"),
            }
        }
    }
}
