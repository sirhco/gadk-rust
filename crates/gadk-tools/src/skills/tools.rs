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

//! The `list_skills`, `load_skill` and `load_skill_resource` tools, and [`skills_to_xml`].
//!
//! Each tool is a [`FunctionTool`] whose input and output schemas are written out as JSON literals,
//! the schemas ADK infers for the same arguments and results (schemars would infer different ones).

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::{Context, Tool, ToolError, ToolOriginKind, go_quote};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::skill::{Frontmatter, MAX_RESOURCE_BYTES, Source};
use crate::go_fmt::{go_lossy_utf8, html_escape_string, wrap_err};

pub(crate) const LIST_SKILLS_NAME: &str = "list_skills";
pub(crate) const LIST_SKILLS_DESCRIPTION: &str =
    "Lists all available skills with their names and descriptions.";
pub(crate) const LOAD_SKILL_NAME: &str = "load_skill";
pub(crate) const LOAD_SKILL_DESCRIPTION: &str =
    "Loads the SKILL.md instructions for a given skill.";
pub(crate) const LOAD_SKILL_RESOURCE_NAME: &str = "load_skill_resource";
pub(crate) const LOAD_SKILL_RESOURCE_DESCRIPTION: &str = "Loads a resource file (e.g., from references/ or assets/) associated with the specified skill.";

// ---------------------------------------------------------------------------------------------
// list_skills
// ---------------------------------------------------------------------------------------------

/// The arguments of `list_skills`: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(crate) struct ListSkillsArgs {}

/// The result of `list_skills`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ListSkillsResult {
    #[serde(rename = "result")]
    pub(crate) skills_xml: String,
}

/// The input schema of `list_skills`.
pub(crate) fn list_skills_input_schema() -> Value {
    json!({"type": "object", "additionalProperties": false})
}

/// The output schema of `list_skills`.
pub(crate) fn list_skills_output_schema() -> Value {
    json!({
        "type": ["null", "object"],
        "properties": {"result": {"type": "string"}},
        "required": ["result"],
        "additionalProperties": false
    })
}

/// Builds the `list_skills` tool.
pub(crate) fn list_skills(source: Arc<dyn Source>) -> Result<Arc<dyn Tool>, ToolError> {
    let cfg = FunctionToolConfig {
        input_schema: Some(list_skills_input_schema()),
        output_schema: Some(list_skills_output_schema()),
        // Skill tools are classified as `SKILL`.
        origin: ToolOriginKind::Skill,
        ..FunctionToolConfig::new(LIST_SKILLS_NAME, LIST_SKILLS_DESCRIPTION)
    };
    FunctionTool::new(cfg, move |_ctx: Context, _args: ListSkillsArgs| {
        let source = source.clone();
        async move {
            // The listing error is returned unwrapped.
            let skills = source
                .list_frontmatters()
                .await
                .map_err(|e| ToolError::Execution(Box::new(e)))?;
            Ok(ListSkillsResult {
                skills_xml: skills_to_xml(&skills),
            })
        }
    })
}

/// The skills as XML: names and descriptions HTML-escaped (`&`, `'`, `<`, `>` and `"`); no trailing
/// newline.
pub(crate) fn skills_to_xml(frontmatters: &[Frontmatter]) -> String {
    let mut sb = String::from("<available_skills>\n");
    for fm in frontmatters {
        sb.push_str("<skill>\n");
        sb.push_str("<name>\n");
        sb.push_str(&html_escape_string(&fm.name));
        sb.push_str("\n</name>\n");
        sb.push_str("<description>\n");
        sb.push_str(&html_escape_string(&fm.description));
        sb.push_str("\n</description>\n");
        sb.push_str("</skill>\n");
    }
    sb.push_str("</available_skills>");
    sb
}

// ---------------------------------------------------------------------------------------------
// load_skill
// ---------------------------------------------------------------------------------------------

/// The arguments of `load_skill`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct LoadSkillArgs {
    /// The name of the skill to load.
    pub(crate) name: String,
}

/// A skill's frontmatter as `load_skill` returns it (JSON key `allowed-tools`; empty optional
/// fields omitted).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct FrontmatterJson {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    license: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    compatibility: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
    #[serde(rename = "allowed-tools", skip_serializing_if = "Vec::is_empty")]
    allowed_tools: Vec<String>,
}

impl From<Frontmatter> for FrontmatterJson {
    fn from(fm: Frontmatter) -> Self {
        Self {
            name: fm.name,
            description: fm.description,
            license: fm.license,
            compatibility: fm.compatibility,
            metadata: fm.metadata,
            allowed_tools: fm.allowed_tools,
        }
    }
}

/// The result of `load_skill`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct LoadSkillResult {
    #[serde(skip_serializing_if = "String::is_empty")]
    skill_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    frontmatter: Option<FrontmatterJson>,
}

/// The input schema of `load_skill`.
pub(crate) fn load_skill_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "The name of the skill to load."}
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

/// The output schema of `load_skill`.
pub(crate) fn load_skill_output_schema() -> Value {
    json!({
        "type": ["null", "object"],
        "properties": {
            "skill_name": {"type": "string"},
            "instructions": {"type": "string"},
            "frontmatter": {
                "type": ["null", "object"],
                "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "license": {"type": "string"},
                    "compatibility": {"type": "string"},
                    "metadata": {"type": "object", "additionalProperties": {"type": "string"}},
                    "allowed-tools": {"type": ["null", "array"], "items": {"type": "string"}}
                },
                "required": ["name", "description"],
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

/// Builds the `load_skill` tool.
pub(crate) fn load_skill(source: Arc<dyn Source>) -> Result<Arc<dyn Tool>, ToolError> {
    let cfg = FunctionToolConfig {
        input_schema: Some(load_skill_input_schema()),
        output_schema: Some(load_skill_output_schema()),
        // Skill tools are classified as `SKILL`.
        origin: ToolOriginKind::Skill,
        ..FunctionToolConfig::new(LOAD_SKILL_NAME, LOAD_SKILL_DESCRIPTION)
    };
    FunctionTool::new(cfg, move |_ctx: Context, args: LoadSkillArgs| {
        let source = source.clone();
        async move { run_load_skill(&*source, args).await }
    })
}

/// Runs `load_skill`: loads the skill's frontmatter and instructions.
async fn run_load_skill(
    source: &dyn Source,
    args: LoadSkillArgs,
) -> Result<LoadSkillResult, ToolError> {
    if args.name.is_empty() {
        return Err(ToolError::InvalidArgument(
            "skill name is required to load a skill".to_string(),
        ));
    }
    let frontmatter = source.load_frontmatter(&args.name).await.map_err(|e| {
        wrap_err(
            format!("load frontmatter for skill {}", go_quote(&args.name)),
            e,
        )
    })?;
    let instructions = source.load_instructions(&args.name).await.map_err(|e| {
        wrap_err(
            format!("load instructions for skill {}", go_quote(&args.name)),
            e,
        )
    })?;
    Ok(LoadSkillResult {
        skill_name: args.name,
        instructions,
        frontmatter: Some(frontmatter.into()),
    })
}

// ---------------------------------------------------------------------------------------------
// load_skill_resource
// ---------------------------------------------------------------------------------------------

/// The arguments of `load_skill_resource`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct LoadSkillResourceArgs {
    /// The name of the skill.
    pub(crate) skill_name: String,
    /// The relative path to the resource.
    pub(crate) resource_path: String,
}

/// The result of `load_skill_resource` (every empty field omitted).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct LoadSkillResourceResult {
    #[serde(skip_serializing_if = "String::is_empty")]
    skill_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    content: String,
}

/// The input schema of `load_skill_resource`.
pub(crate) fn load_skill_resource_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "skill_name": {"type": "string", "description": "The name of the skill."},
            "resource_path": {
                "type": "string",
                "description": "The relative path to the resource (e.g., 'references/my_doc.md', 'assets/template.txt', or 'scripts/setup.sh')."
            }
        },
        "required": ["skill_name", "resource_path"],
        "additionalProperties": false
    })
}

/// The output schema of `load_skill_resource`.
pub(crate) fn load_skill_resource_output_schema() -> Value {
    json!({
        "type": ["null", "object"],
        "properties": {
            "skill_name": {"type": "string"},
            "path": {"type": "string"},
            "content": {"type": "string"}
        },
        "additionalProperties": false
    })
}

/// Builds the `load_skill_resource` tool.
pub(crate) fn load_skill_resource(source: Arc<dyn Source>) -> Result<Arc<dyn Tool>, ToolError> {
    let cfg = FunctionToolConfig {
        input_schema: Some(load_skill_resource_input_schema()),
        output_schema: Some(load_skill_resource_output_schema()),
        // Skill tools are classified as `SKILL`.
        origin: ToolOriginKind::Skill,
        ..FunctionToolConfig::new(LOAD_SKILL_RESOURCE_NAME, LOAD_SKILL_RESOURCE_DESCRIPTION)
    };
    FunctionTool::new(cfg, move |_ctx: Context, args: LoadSkillResourceArgs| {
        let source = source.clone();
        async move { run_load_skill_resource(&*source, args).await }
    })
}

/// Runs `load_skill_resource`: reads at most 10 MiB + 1 bytes and rejects anything over 10 MiB. The
/// content is returned as text with each invalid UTF-8 byte replaced by one U+FFFD
/// ([`go_lossy_utf8`]).
async fn run_load_skill_resource(
    source: &dyn Source,
    args: LoadSkillResourceArgs,
) -> Result<LoadSkillResourceResult, ToolError> {
    let (name, path) = (&args.skill_name, &args.resource_path);
    if name.is_empty() {
        return Err(ToolError::InvalidArgument(
            "skill name is required to load a resource".to_string(),
        ));
    }
    if path.is_empty() {
        return Err(ToolError::InvalidArgument(format!(
            "resource path is required to load a resource for skill {}",
            go_quote(name)
        )));
    }
    let reader = source
        .load_resource(name, path)
        .await
        .map_err(|e| wrap_err(format!("load resource '{path}' from skill '{name}'"), e))?;
    let mut content = Vec::new();
    reader
        .take(MAX_RESOURCE_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .await
        .map_err(|e| wrap_err(format!("read resource '{path}' from skill '{name}'"), e))?;
    if content.len() > MAX_RESOURCE_BYTES {
        return Err(ToolError::Execution(
            format!(
                "resource '{path}' from skill '{name}' is too large (limit: {MAX_RESOURCE_BYTES} bytes)"
            )
            .into(),
        ));
    }
    Ok(LoadSkillResourceResult {
        skill_name: args.skill_name.clone(),
        path: args.resource_path.clone(),
        content: go_lossy_utf8(&content),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod origin_tests {
    use super::*;
    use crate::skills::skill::{FileSystemSource, MemFs};

    #[test]
    fn skill_tools_are_skill_tools() {
        let source: Arc<dyn Source> = Arc::new(FileSystemSource::new(Arc::new(MemFs::default())));
        for t in [
            list_skills(source.clone()).unwrap(),
            load_skill(source.clone()).unwrap(),
            load_skill_resource(source).unwrap(),
        ] {
            assert_eq!(t.origin_kind(), ToolOriginKind::Skill, "{}", t.name());
            assert!(t.origin_agent().is_none());
        }
    }
}
