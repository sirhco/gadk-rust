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

//! Skills: folders of instructions and resources that extend an agent's capabilities, and the
//! [`SkillToolset`] that lets an agent discover and use them (port of Go `tool/skilltoolset`).
//!
//! [`skill`] holds the frontmatter parser and the skill sources; this module holds the toolset.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! use std::sync::Arc;
//! use gadk_tools::skills::{SkillToolset, SkillToolsetConfig, skill};
//!
//! let fs = Arc::new(skill::DirFs::new("./skills"));
//! let source: Arc<dyn skill::Source> = Arc::new(skill::FileSystemSource::new(fs));
//! let (source, _reload) = skill::with_complete_preload(source).await?;
//! let toolset = SkillToolset::new(SkillToolsetConfig { source, name: None, system_instruction: None });
//! # let _ = toolset;
//! # Ok(())
//! # }
//! ```

pub mod skill;
pub(crate) mod tools;

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Context, ReadonlyContext, Tool, ToolError, Toolset};
use gadk_model::LlmRequest;

/// Go `defaultName`.
const DEFAULT_NAME: &str = "SkillToolset";

/// Go `defaultSkillSystemInstruction` (tool/skilltoolset/toolset.go:34-47), byte-for-byte,
/// including the trailing newline.
pub(crate) const DEFAULT_SKILL_SYSTEM_INSTRUCTION: &str = "You can use specialized 'skills' to help you with complex tasks. You MUST use the skill tools to interact with these skills.

Skills are folders of instructions and resources that extend your capabilities for specialized tasks. Each skill folder contains:
- **SKILL.md** (required): The main instruction file with skill metadata and detailed markdown instructions.
- **references/** (Optional): Additional documentation or examples for skill usage.
- **assets/** (Optional): Templates, scripts or other resources used by the skill.
- **scripts/** (Optional): Executable scripts that can be run via bash.

This is very important:

1. If a skill seems relevant to the current user query, you MUST use the `load_skill` tool with `name=\"<SKILL_NAME>\"` to read its full instructions before proceeding.
2. Once you have read the instructions, follow them exactly as documented before replying to the user. For example, If the instruction lists multiple steps, please make sure you complete all of them in order.
3. The `load_skill_resource` tool is for viewing files within a skill's directory (e.g., `references/*`, `assets/*`, `scripts/*`). Do NOT use other tools to access these files.
";

/// Port of Go `skilltoolset.Config`.
#[derive(Clone)]
pub struct SkillToolsetConfig {
    /// Where the skills come from. Required by the type, so Go's "skill source must be provided"
    /// check has no Rust counterpart.
    pub source: Arc<dyn skill::Source>,
    /// The toolset name. `None` or an empty string means `"SkillToolset"`.
    pub name: Option<String>,
    /// The system instruction appended before the skill list. `None` or an empty string means
    /// the default instruction.
    pub system_instruction: Option<String>,
}

/// Port of Go `skilltoolset.SkillToolset`: the `list_skills`, `load_skill` and
/// `load_skill_resource` tools over one [`skill::Source`], plus a request processor that tells
/// the model which skills exist.
pub struct SkillToolset {
    name: String,
    /// The three tools, or the construction error Go's `New` would have returned (it cannot occur
    /// with the fixed schemas; [`SkillToolset::new`] cannot fail, so `tools()` reports it).
    tools: Result<Vec<Arc<dyn Tool>>, String>,
    source: Arc<dyn skill::Source>,
    system_instruction: String,
}

/// Go's `cfg.X != ""` defaulting.
fn non_empty_or(v: Option<String>, default: &str) -> String {
    v.filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl SkillToolset {
    /// Port of Go `skilltoolset.New`. The three tools are built once; [`Toolset::tools`] returns
    /// the same instances on every call.
    pub fn new(cfg: SkillToolsetConfig) -> Arc<SkillToolset> {
        let tools = build_tools(&cfg.source);
        Arc::new(SkillToolset {
            name: non_empty_or(cfg.name, DEFAULT_NAME),
            tools,
            source: cfg.source,
            system_instruction: non_empty_or(
                cfg.system_instruction,
                DEFAULT_SKILL_SYSTEM_INSTRUCTION,
            ),
        })
    }
}

/// Go `New`'s three constructor calls, with its error wrapping texts.
fn build_tools(source: &Arc<dyn skill::Source>) -> Result<Vec<Arc<dyn Tool>>, String> {
    let list =
        tools::list_skills(source.clone()).map_err(|e| format!("create list skills tool: {e}"))?;
    let load =
        tools::load_skill(source.clone()).map_err(|e| format!("create load skill tool: {e}"))?;
    let load_resource = tools::load_skill_resource(source.clone())
        .map_err(|e| format!("create load skill resource tool: {e}"))?;
    Ok(vec![list, load, load_resource])
}

#[async_trait]
impl Toolset for SkillToolset {
    fn name(&self) -> &str {
        &self.name
    }

    /// Port of Go `SkillToolset.Tools`: always `[list_skills, load_skill, load_skill_resource]`.
    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        self.tools.clone().map_err(ToolError::InvalidArgument)
    }

    /// Port of Go `SkillToolset.ProcessRequest` (toolset.go:110-120): with at least one skill,
    /// appends the system instruction and the skill list as one instruction (joined by
    /// `"\n\n"`). A listing error is returned unwrapped, as in Go.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        let skills = self
            .source
            .list_frontmatters()
            .await
            .map_err(|e| ToolError::Execution(Box::new(e)))?;
        if skills.is_empty() {
            return Ok(());
        }
        let xml = tools::skills_to_xml(&skills);
        append_instructions(req, &[&self.system_instruction, &xml]);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
