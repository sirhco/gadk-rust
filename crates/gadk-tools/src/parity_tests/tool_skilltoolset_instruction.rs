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

//! Parity target for Go tool/skilltoolset/instruction_test.go (package-internal: reads the
//! unexported default instruction).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;

use crate::skills::DEFAULT_SKILL_SYSTEM_INSTRUCTION;
use crate::skills::skill::{Frontmatter, ResourceReader, SkillError, SkillErrorKind, Source};
use crate::skills::tools::load_skill;

/// A source with no skills; only the tool's declaration is inspected.
struct NoSkills;

fn not_found() -> SkillError {
    SkillError::new(SkillErrorKind::SkillNotFound, "skill not found")
}

#[async_trait]
impl Source for NoSkills {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        Ok(Vec::new())
    }
    async fn list_resources(&self, _name: &str, _subpath: &str) -> Result<Vec<String>, SkillError> {
        Err(not_found())
    }
    async fn load_frontmatter(&self, _name: &str) -> Result<Frontmatter, SkillError> {
        Err(not_found())
    }
    async fn load_instructions(&self, _name: &str) -> Result<String, SkillError> {
        Err(not_found())
    }
    async fn load_resource(
        &self,
        _name: &str,
        _resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        Err(not_found())
    }
}

// parity: adapted: Go reads LoadSkillArgs.Name's json tag by reflection; Rust reads the parameter name from the load_skill declaration's input schema, which is what the model sees
#[test]
fn test_default_instruction_matches_load_skill_schema() {
    let tool = load_skill(Arc::new(NoSkills)).unwrap();
    let decl = tool.declaration().expect("load_skill has no declaration");
    let schema = decl
        .parameters_json_schema
        .expect("load_skill has no parameters schema");
    let props = schema["properties"]
        .as_object()
        .expect("LoadSkillArgs has no properties");
    assert_eq!(props.len(), 1, "LoadSkillArgs should have one field");
    let param_name = props
        .keys()
        .next()
        .expect("LoadSkillArgs has no Name field");
    assert!(!param_name.is_empty(), "LoadSkillArgs.Name has no json tag");

    assert!(
        DEFAULT_SKILL_SYSTEM_INSTRUCTION
            .contains(&format!("`load_skill` tool with `{param_name}=")),
        "default instruction should document load_skill with {param_name:?}=, got:\n{DEFAULT_SKILL_SYSTEM_INSTRUCTION}"
    );
    assert!(
        !DEFAULT_SKILL_SYSTEM_INSTRUCTION.contains("`load_skill` tool with `skill_name="),
        "default instruction still documents skill_name= for load_skill, which is not a valid load_skill parameter"
    );
}

/// The default instruction is Go's text byte-for-byte (1183 bytes, trailing newline included),
/// rebuilt here line by line from tool/skilltoolset/toolset.go:34-47.
#[test]
fn default_instruction_is_go_text() {
    let lines = [
        "You can use specialized 'skills' to help you with complex tasks. You MUST use the skill tools to interact with these skills.",
        "",
        "Skills are folders of instructions and resources that extend your capabilities for specialized tasks. Each skill folder contains:",
        "- **SKILL.md** (required): The main instruction file with skill metadata and detailed markdown instructions.",
        "- **references/** (Optional): Additional documentation or examples for skill usage.",
        "- **assets/** (Optional): Templates, scripts or other resources used by the skill.",
        "- **scripts/** (Optional): Executable scripts that can be run via bash.",
        "",
        "This is very important:",
        "",
        "1. If a skill seems relevant to the current user query, you MUST use the `load_skill` tool with `name=\"<SKILL_NAME>\"` to read its full instructions before proceeding.",
        "2. Once you have read the instructions, follow them exactly as documented before replying to the user. For example, If the instruction lists multiple steps, please make sure you complete all of them in order.",
        "3. The `load_skill_resource` tool is for viewing files within a skill's directory (e.g., `references/*`, `assets/*`, `scripts/*`). Do NOT use other tools to access these files.",
    ];
    let want = lines.join("\n") + "\n";
    assert_eq!(DEFAULT_SKILL_SYSTEM_INSTRUCTION, want);
    assert_eq!(DEFAULT_SKILL_SYSTEM_INSTRUCTION.len(), 1183);
}
