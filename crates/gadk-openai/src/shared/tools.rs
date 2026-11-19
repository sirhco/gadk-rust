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

//! Tool validation both APIs share.

use gadk_genai::Tool;

use crate::error::OpenAiError;
use crate::shared::config::reject_unsupported_fields;

/// The only [`Tool`] field this crate sends. Every
/// other field is refused, so a field genai adds cannot be dropped silently.
const SUPPORTED_FUNCTION_TOOL_FIELDS: &[&str] = &["function_declarations"];

/// Rejects a missing tool (`None`: `openai: tool <idx> is missing`), one
/// carrying any field besides `FunctionDeclarations` (`openai: non-function tools are not
/// supported (tool <idx>): ` wrapping `UnsupportedConfigField: <Field>`, the first set field
/// in declaration order), and one declaring no functions (`openai: tool <idx> does not
/// declare any functions`). The missing-tool and no-functions errors are plain.
pub(crate) fn ensure_function_tool_only(
    idx: usize,
    tool: Option<&Tool>,
) -> Result<(), OpenAiError> {
    let Some(tool) = tool else {
        return Err(OpenAiError::plain(format!("openai: tool {idx} is missing")));
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

/// Every `Tool` field as `(field name, set)`, in declaration order, by exhaustive
/// destructuring, so a field genai adds fails to compile
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
        ("retrieval", retrieval.is_some()),
        ("computer_use", computer_use.is_some()),
        ("file_search", file_search.is_some()),
        ("google_search", google_search.is_some()),
        ("google_maps", google_maps.is_some()),
        ("code_execution", code_execution.is_some()),
        ("enterprise_web_search", enterprise_web_search.is_some()),
        ("function_declarations", !function_declarations.is_empty()),
        ("google_search_retrieval", google_search_retrieval.is_some()),
        ("parallel_ai_search", parallel_ai_search.is_some()),
        ("url_context", url_context.is_some()),
        ("mcp_servers", !mcp_servers.is_empty()),
        ("exa_ai_search", exa_ai_search.is_some()),
    ]
}
