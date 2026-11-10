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

//! `pack_tool`, `tool_from_request` (port of Go `tool/toolutils/toolutils.go`),
//! `append_instructions` (port of Go `internal/utils/utils.go` `AppendInstructions`) and the tool
//! confirmation helpers (port of Go `tool/toolconfirmation/tool_confirmation.go`).

use std::any::Any;
use std::sync::Arc;

use gadk_genai::{
    Content, FunctionCall, GenerateContentConfig, Part, ROLE_USER, Tool as GenaiTool,
};
use gadk_model::LlmRequest;
use gadk_platform::go_type_name;
use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::go_fmt::go_quote;
use crate::tool::Tool;

/// Port of Go `toolutils.PackTool`: register `tool` under its name and append its declaration to
/// the first genai Tool that already has function declarations, else to a new one. Called by the
/// flow's toolPreprocess (`base_flow.go`) for every tool that has a declaration, after all
/// request processors.
///
/// The tool is stored as an `Arc<dyn Tool>` boxed in an `Arc<dyn Any>`; read it back with
/// [`tool_from_request`].
pub fn pack_tool(req: &mut LlmRequest, tool: &Arc<dyn Tool>) -> Result<(), ToolError> {
    let name = tool.name().to_string();
    if req.tools.contains_key(&name) {
        return Err(ToolError::InvalidArgument(format!(
            "duplicate tool: {}",
            go_quote(&name)
        )));
    }
    req.tools
        .insert(name, Arc::new(tool.clone()) as Arc<dyn Any + Send + Sync>);
    // Go creates Config even for a tool without a declaration.
    let config = req
        .config
        .get_or_insert_with(GenerateContentConfig::default);
    let Some(decl) = tool.declaration() else {
        return Ok(());
    };
    match config
        .tools
        .iter_mut()
        .find(|t| !t.function_declarations.is_empty())
    {
        Some(t) => t.function_declarations.push(decl),
        None => config.tools.push(GenaiTool {
            function_declarations: vec![decl],
            ..Default::default()
        }),
    }
    Ok(())
}

/// Recover a tool packed by [`pack_tool`] (or the transfer processor's `appendTools` port).
pub fn tool_from_request(req: &LlmRequest, name: &str) -> Option<Arc<dyn Tool>> {
    req.tools
        .get(name)?
        .downcast_ref::<Arc<dyn Tool>>()
        .cloned()
}

/// Port of Go `utils.AppendInstructions` (`internal/utils/utils.go:138`): appends `instructions`,
/// joined with `"\n\n"`, to the request's system instruction. The text goes onto the last
/// system-instruction part when that part's text is non-empty (separated by `"\n\n"`), otherwise
/// into a new part; a missing system instruction becomes a new user-role content. An empty
/// `instructions` slice changes nothing (not even `config`).
pub fn append_instructions(req: &mut LlmRequest, instructions: &[&str]) {
    if instructions.is_empty() {
        return;
    }
    let inst = instructions.join("\n\n");
    let cfg = req
        .config
        .get_or_insert_with(GenerateContentConfig::default);
    let Some(si) = cfg.system_instruction.as_mut() else {
        cfg.system_instruction = Some(Content::from_text(inst, ROLE_USER));
        return;
    };
    if let Some(last) = si.parts.last_mut()
        && !last.text.is_empty()
    {
        last.text.push_str("\n\n");
        last.text.push_str(&inst);
        return;
    }
    si.parts.push(Part::from_text(inst));
}

/// Go `toolconfirmation.FunctionCallName`.
pub const REQUEST_CONFIRMATION_FUNCTION_CALL_NAME: &str = "adk_request_confirmation";

/// Port of Go `toolconfirmation.OriginalCallFrom` (error texts verbatim). An empty `args` map
/// counts as Go's nil functionCall/Args (one check, one message in Go).
///
/// Go also accepts a `*genai.FunctionCall` value stored directly in `Args`; JSON args can never
/// hold one, so only the map path is ported.
pub fn original_call_from(fc: &FunctionCall) -> Result<FunctionCall, ToolError> {
    const KEY: &str = "originalFunctionCall";
    if fc.args.is_empty() {
        return Err(ToolError::InvalidArgument(
            "functionCall or its arguments cannot be nil".into(),
        ));
    }
    let Some(val) = fc.args.get(KEY) else {
        return Err(ToolError::InvalidArgument(format!(
            "required argument {} is missing from call with ID {}",
            go_quote(KEY),
            fc.id
        )));
    };
    let Value::Object(raw) = val else {
        return Err(ToolError::InvalidArgument(format!(
            "argument {} has invalid type: expected JSON object (map[string]any) or *genai.FunctionCall, got {}",
            go_quote(KEY),
            go_type_name(val)
        )));
    };
    // Go converters.FromMapStructure: mapstructure matches field names case-insensitively
    // ("ID" → id).
    const FIELDS: [&str; 5] = ["id", "args", "name", "partialArgs", "willContinue"];
    let normalized: Map<String, Value> = raw
        .iter()
        .filter_map(|(k, v)| {
            FIELDS
                .iter()
                .find(|f| f.eq_ignore_ascii_case(k))
                .map(|f| ((*f).to_string(), v.clone()))
        })
        .collect();
    serde_json::from_value(Value::Object(normalized)).map_err(|e| {
        ToolError::InvalidArgument(format!(
            "failed to decode {} structure for call ID {}: {e}",
            go_quote(KEY),
            fc.id
        ))
    })
}
