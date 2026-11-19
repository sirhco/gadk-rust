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

use gadk_genai::{
    FileSearch, FunctionDeclaration, GoogleSearch, MCPServer, Tool, ToolExaAISearch,
    ToolParallelAISearch,
};

use crate::error::ErrorKind;
use crate::shared::tools::ensure_function_tool_only;

// Go refuses every `genai.Tool` field but `FunctionDeclarations` (275e190e, #1983), so the
// cases name the fields genai 1.77 added, alone and beside functions, and expect the wrapped
// `ErrUnsupportedConfigField`.
#[test]
fn test_ensure_function_tool_only() {
    let fn_decls = || {
        vec![FunctionDeclaration {
            name: "fn1".to_string(),
            ..FunctionDeclaration::default()
        }]
    };
    let mcp = || {
        vec![MCPServer {
            name: "my-mcp".to_string(),
            ..MCPServer::default()
        }]
    };
    // (name, tool, want error text, want ErrUnsupportedConfigField)
    let tests: Vec<(&str, Option<Tool>, &str, bool)> = vec![
        ("nil tool", None, "tool 0 is missing", false),
        (
            "GoogleSearch alone",
            Some(Tool {
                google_search: Some(GoogleSearch::default()),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "FileSearch alone",
            Some(Tool {
                file_search: Some(FileSearch::default()),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "ParallelAISearch alone",
            Some(Tool {
                parallel_ai_search: Some(ToolParallelAISearch::default()),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "MCPServers alone",
            Some(Tool {
                mcp_servers: mcp(),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "ExaAISearch alone",
            Some(Tool {
                exa_ai_search: Some(ToolExaAISearch::default()),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "MCPServers beside FunctionDeclarations",
            Some(Tool {
                function_declarations: fn_decls(),
                mcp_servers: mcp(),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "FileSearch beside FunctionDeclarations",
            Some(Tool {
                function_declarations: fn_decls(),
                file_search: Some(FileSearch::default()),
                ..Tool::default()
            }),
            "non-function tools",
            true,
        ),
        (
            "no functions",
            Some(Tool::default()),
            "does not declare any functions",
            false,
        ),
        (
            "valid",
            Some(Tool {
                function_declarations: fn_decls(),
                ..Tool::default()
            }),
            "",
            false,
        ),
    ];
    for (name, tool, want_err, want_is) in tests {
        let result = ensure_function_tool_only(0, tool.as_ref());
        if want_err.is_empty() {
            assert!(result.is_ok(), "{name}: expected no error, got {result:?}");
            continue;
        }
        let err = match result {
            Err(err) if err.to_string().contains(want_err) => err,
            other => panic!("{name}: expected error containing {want_err:?}, got: {other:?}"),
        };
        if want_is {
            assert!(
                err.is(ErrorKind::UnsupportedConfigField),
                "{name}: errors.Is({err}, ErrUnsupportedConfigField) = false"
            );
        }
    }
}
