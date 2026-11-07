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

//! Parity target for Go tool/skilltoolset/internal/skilltool/tools_test.go (Go
//! `internal/skilltool` is the crate-private `skills::tools` module).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Context, Tool};
use serde_json::{Map, Value, json};

use crate::skills::skill::{
    Frontmatter, MAX_RESOURCE_BYTES, ResourceReader, SkillError, SkillErrorKind, Source,
};
use crate::skills::tools::{list_skills, load_skill, load_skill_resource, skills_to_xml};

/// Go `mockSource` (tools_test.go).
#[derive(Default)]
struct MockSource {
    frontmatters: Vec<Frontmatter>,
    /// skill name → resource path → content.
    resources: HashMap<String, HashMap<String, Vec<u8>>>,
    instructions: HashMap<String, String>,
}

fn skill_not_found() -> SkillError {
    SkillError::new(SkillErrorKind::SkillNotFound, "skill not found")
}

fn resource_not_found() -> SkillError {
    SkillError::new(SkillErrorKind::ResourceNotFound, "resource not found")
}

#[async_trait]
impl Source for MockSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        Ok(self.frontmatters.clone())
    }
    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError> {
        Ok(self
            .resources
            .get(name)
            .map(|r| {
                r.keys()
                    .filter(|p| p.starts_with(subpath))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }
    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        self.frontmatters
            .iter()
            .find(|fm| fm.name == name)
            .cloned()
            .ok_or_else(skill_not_found)
    }
    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        self.instructions
            .get(name)
            .cloned()
            .ok_or_else(skill_not_found)
    }
    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        let res = self
            .resources
            .get(name)
            .and_then(|r| r.get(resource_path))
            .ok_or_else(resource_not_found)?;
        Ok(Box::pin(std::io::Cursor::new(res.clone())))
    }
}

/// Go `createToolContext`: a tool context over an empty invocation context.
fn create_tool_context() -> Context {
    Context::builder().build()
}

async fn run(
    tool: &Arc<dyn Tool>,
    args: Value,
) -> Result<Map<String, Value>, gadk_agent::ToolError> {
    tool.as_runnable()
        .expect("tool does not implement toolinternal.FunctionTool")
        .run(create_tool_context(), args)
        .await
}

#[tokio::test]
async fn test_list_skills() {
    let source = MockSource {
        frontmatters: vec![
            Frontmatter {
                name: "skill1".into(),
                description: "description1".into(),
                ..Default::default()
            },
            Frontmatter {
                name: "skill2".into(),
                description: "description2".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let tool = list_skills(Arc::new(source)).expect("ListSkills failed");
    let result = run(&tool, json!({})).await.expect("tool.Run failed");

    let want = json!({
        "skills": "<available_skills>\n<skill>\n<name>\nskill1\n</name>\n<description>\ndescription1\n</description>\n</skill>\n<skill>\n<name>\nskill2\n</name>\n<description>\ndescription2\n</description>\n</skill>\n</available_skills>",
    });
    assert_eq!(Value::Object(result), want, "result mismatch");
}

#[tokio::test]
async fn test_load_skill() {
    let source = MockSource {
        frontmatters: vec![Frontmatter {
            name: "skill1".into(),
            description: "description1".into(),
            metadata: BTreeMap::from([("key1".to_string(), "value1".to_string())]),
            ..Default::default()
        }],
        instructions: HashMap::from([("skill1".to_string(), "instructions1".to_string())]),
        ..Default::default()
    };
    let tool = load_skill(Arc::new(source)).expect("LoadSkill failed");

    let got = run(&tool, json!({"name": "skill1"}))
        .await
        .expect("LoadSkill tool.Run failed");
    let want = json!({
        "skill_name": "skill1",
        "instructions": "instructions1",
        "frontmatter": {
            "name": "skill1",
            "description": "description1",
            "metadata": {
                "key1": "value1",
            },
        },
    });
    assert_eq!(Value::Object(got), want, "LoadSkill result mismatch");
}

#[tokio::test]
async fn test_load_skill_resource() {
    let source = MockSource {
        resources: HashMap::from([(
            "skill1".to_string(),
            HashMap::from([("assets/data.txt".to_string(), b"content1".to_vec())]),
        )]),
        ..Default::default()
    };
    let tool = load_skill_resource(Arc::new(source)).expect("LoadSkillResource");
    let result = run(
        &tool,
        json!({
            "skill_name": "skill1",
            "resource_path": "assets/data.txt",
        }),
    )
    .await
    .expect("tool.Run failed");
    let want = json!({
        "skill_name": "skill1",
        "path": "assets/data.txt",
        "content": "content1",
    });
    assert_eq!(Value::Object(result), want, "result mismatch");
}

/// Spec risk "Inferred schemas": each declaration equals the JSON Go's `functiontool.New` emits
/// for the same tool (generated from ADK-Go at the pinned commit with `json.Marshal` of
/// `Declaration()`).
#[test]
fn declarations_match_go() {
    let source: Arc<dyn Source> = Arc::new(MockSource::default());
    let cases: [(Arc<dyn Tool>, &str); 3] = [
        (
            list_skills(source.clone()).unwrap(),
            r#"{"description":"Lists all available skills with their names and descriptions.","name":"list_skills","parametersJsonSchema":{"type":"object","additionalProperties":false},"responseJsonSchema":{"type":["null","object"],"properties":{"skills":{"type":"string"}},"required":["skills"],"additionalProperties":false}}"#,
        ),
        (
            load_skill(source.clone()).unwrap(),
            r#"{"description":"Loads the SKILL.md instructions for a given skill.","name":"load_skill","parametersJsonSchema":{"type":"object","properties":{"name":{"type":"string","description":"The name of the skill to load."}},"required":["name"],"additionalProperties":false},"responseJsonSchema":{"type":["null","object"],"properties":{"skill_name":{"type":"string"},"instructions":{"type":"string"},"frontmatter":{"type":["null","object"],"properties":{"name":{"type":"string"},"description":{"type":"string"},"license":{"type":"string"},"compatibility":{"type":"string"},"metadata":{"type":"object","additionalProperties":{"type":"string"}},"allowed-tools":{"type":["null","array"],"items":{"type":"string"}}},"required":["name","description"],"additionalProperties":false}},"additionalProperties":false}}"#,
        ),
        (
            load_skill_resource(source).unwrap(),
            r#"{"description":"Loads a resource file (e.g., from references/ or assets/) associated with the specified skill.","name":"load_skill_resource","parametersJsonSchema":{"type":"object","properties":{"skill_name":{"type":"string","description":"The name of the skill."},"resource_path":{"type":"string","description":"The relative path to the resource (e.g., 'references/my_doc.md', 'assets/template.txt', or 'scripts/setup.sh')."}},"required":["skill_name","resource_path"],"additionalProperties":false},"responseJsonSchema":{"type":["null","object"],"properties":{"skill_name":{"type":"string"},"path":{"type":"string"},"content":{"type":"string"}},"additionalProperties":false}}"#,
        ),
    ];
    for (tool, go_json) in cases {
        let got = serde_json::to_value(tool.declaration().unwrap()).unwrap();
        let want: Value = serde_json::from_str(go_json).unwrap();
        assert_eq!(got, want, "{} declaration differs from Go's", tool.name());
    }
}

/// Go `SkillsToXML`: no skills, and `html.EscapeString` escaping of names and descriptions.
#[test]
fn skills_to_xml_empty_and_escaped() {
    assert_eq!(
        skills_to_xml(&[]),
        "<available_skills>\n</available_skills>"
    );
    let fm = Frontmatter {
        name: "a&b".into(),
        description: "<x> 'y' \"z\"".into(),
        ..Default::default()
    };
    assert_eq!(
        skills_to_xml(&[fm]),
        "<available_skills>\n<skill>\n<name>\na&amp;b\n</name>\n<description>\n&lt;x&gt; &#39;y&#39; &#34;z&#34;\n</description>\n</skill>\n</available_skills>"
    );
}

/// Go's error texts of `loadSkill` and `loadSkillResource`.
#[tokio::test]
async fn skill_tool_error_texts() {
    let source: Arc<dyn Source> = Arc::new(MockSource::default());
    let load = load_skill(source.clone()).unwrap();
    let err = run(&load, json!({"name": ""})).await.unwrap_err();
    assert_eq!(err.to_string(), "skill name is required to load a skill");
    let err = run(&load, json!({"name": "ghost"})).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "load frontmatter for skill \"ghost\": {}",
            skill_not_found()
        )
    );

    let resource = load_skill_resource(source).unwrap();
    let err = run(
        &resource,
        json!({"skill_name": "", "resource_path": "assets/a"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "skill name is required to load a resource");
    let err = run(&resource, json!({"skill_name": "s", "resource_path": ""}))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "resource path is required to load a resource for skill \"s\""
    );
    let err = run(
        &resource,
        json!({"skill_name": "s", "resource_path": "assets/a"}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "load resource 'assets/a' from skill 's': {}",
            resource_not_found()
        )
    );
}

/// Go `maxResourceSize`: exactly 10 MiB loads; one byte more fails with Go's text.
#[tokio::test]
async fn load_skill_resource_size_limit() {
    let source = MockSource {
        resources: HashMap::from([(
            "s".to_string(),
            HashMap::from([
                ("assets/max".to_string(), vec![b'a'; MAX_RESOURCE_BYTES]),
                (
                    "assets/over".to_string(),
                    vec![b'a'; MAX_RESOURCE_BYTES + 1],
                ),
            ]),
        )]),
        ..Default::default()
    };
    let tool = load_skill_resource(Arc::new(source)).unwrap();
    let ok = run(
        &tool,
        json!({"skill_name": "s", "resource_path": "assets/max"}),
    )
    .await
    .unwrap();
    assert_eq!(
        ok["content"].as_str().map(str::len),
        Some(MAX_RESOURCE_BYTES)
    );
    let err = run(
        &tool,
        json!({"skill_name": "s", "resource_path": "assets/over"}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "resource 'assets/over' from skill 's' is too large (limit: 10485760 bytes)"
    );
}
