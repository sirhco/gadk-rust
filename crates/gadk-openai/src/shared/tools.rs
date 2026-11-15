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

//! Tool validation both APIs share (port of ADK-Go `internal/shared/tools.go`).

use gadk_genai::Tool;

use crate::error::OpenAiError;
use crate::shared::config::reject_unsupported_fields;

/// Go `supportedFunctionToolFields`: the only `genai.Tool` field this package sends. Every
/// other field is refused, so a field genai adds cannot be dropped silently.
const SUPPORTED_FUNCTION_TOOL_FIELDS: &[&str] = &["FunctionDeclarations"];

/// Go `EnsureFunctionToolOnly`: rejects a nil tool (`None`: `openai: tool <idx> is nil`), one
/// carrying any field besides `FunctionDeclarations` (`openai: non-function tools are not
/// supported (tool <idx>): ` wrapping `ErrUnsupportedConfigField: <Field>`, the first set field
/// in genai's declaration order), and one declaring no functions (`openai: tool <idx> does not
/// declare any functions`). The nil and no-functions errors are plain.
pub(crate) fn ensure_function_tool_only(
    idx: usize,
    tool: Option<&Tool>,
) -> Result<(), OpenAiError> {
    let Some(tool) = tool else {
        return Err(OpenAiError::plain(format!("openai: tool {idx} is nil")));
    };
    reject_unsupported_fields("", &tool_fields(tool), SUPPORTED_FUNCTION_TOOL_FIELDS).map_err(
        |e| {
            e.prefixed(format!(
                "openai: non-function tools are not supported (tool {idx}): "
            ))
        },
    )?;
    if tool.function_declarations.is_empty() {
        return Err(OpenAiError::plain(format!(
            "openai: tool {idx} does not declare any functions"
        )));
    }
    Ok(())
}

/// Every `Tool` field as `(Go field name, set)`, in genai's declaration order, by exhaustive
/// destructuring (Go walks the struct by reflection), so a field genai adds fails to compile
/// here until it is listed.
fn tool_fields(tool: &Tool) -> [(&'static str, bool); 13] {
    let Tool {
        retrieval,
        computer_use,
        file_search,
        google_search,
        google_maps,
        code_execution,
        enterprise_web_search,
        function_declarations,
        google_search_retrieval,
        parallel_ai_search,
        url_context,
        mcp_servers,
        exa_ai_search,
    } = tool;
    [
        ("Retrieval", retrieval.is_some()),
        ("ComputerUse", computer_use.is_some()),
        ("FileSearch", file_search.is_some()),
        ("GoogleSearch", google_search.is_some()),
        ("GoogleMaps", google_maps.is_some()),
        ("CodeExecution", code_execution.is_some()),
        ("EnterpriseWebSearch", enterprise_web_search.is_some()),
        ("FunctionDeclarations", !function_declarations.is_empty()),
        ("GoogleSearchRetrieval", google_search_retrieval.is_some()),
        ("ParallelAISearch", parallel_ai_search.is_some()),
        ("URLContext", url_context.is_some()),
        ("MCPServers", !mcp_servers.is_empty()),
        ("ExaAISearch", exa_ai_search.is_some()),
    ]
}
