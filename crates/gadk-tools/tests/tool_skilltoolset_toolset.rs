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

//! Parity target for Go tool/skilltoolset/toolset_test.go.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Context, Toolset};
use gadk_model::LlmRequest;
use gadk_tools::skills::skill::{Frontmatter, ResourceReader, SkillError, SkillErrorKind, Source};
use gadk_tools::skills::{SkillToolset, SkillToolsetConfig};

/// Go `mockSource`: embeds a nil `skill.Source`, so only `ListFrontmatters` is implemented; the
/// other methods (which Go would nil-panic on) fail.
#[derive(Default)]
struct MockSource {
    frontmatters: Vec<Frontmatter>,
}

fn unimplemented_method() -> SkillError {
    SkillError::new(SkillErrorKind::Other, "mockSource: method not implemented")
}

#[async_trait]
impl Source for MockSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        Ok(self.frontmatters.clone())
    }
    async fn list_resources(&self, _name: &str, _subpath: &str) -> Result<Vec<String>, SkillError> {
        Err(unimplemented_method())
    }
    async fn load_frontmatter(&self, _name: &str) -> Result<Frontmatter, SkillError> {
        Err(unimplemented_method())
    }
    async fn load_instructions(&self, _name: &str) -> Result<String, SkillError> {
        Err(unimplemented_method())
    }
    async fn load_resource(
        &self,
        _name: &str,
        _resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        Err(unimplemented_method())
    }
}

fn toolset(source: MockSource) -> Arc<SkillToolset> {
    SkillToolset::new(SkillToolsetConfig {
        source: Arc::new(source),
        name: None,
        system_instruction: None,
    })
}

// parity: adapted: Go passes a nil agent.Context; Rust passes a default Context::builder() context
#[tokio::test]
async fn test_process_request() {
    let ts = toolset(MockSource {
        frontmatters: vec![
            Frontmatter {
                name: "skill0".into(),
                description: "description0".into(),
                ..Default::default()
            },
            Frontmatter {
                name: "skill1".into(),
                description: "description1".into(),
                compatibility: "...".into(),
                metadata: BTreeMap::from([
                    ("flag1".to_string(), "key1".to_string()),
                    ("flag2".to_string(), "key2".to_string()),
                ]),
                ..Default::default()
            },
        ],
    });
    let mut req = LlmRequest::default();

    ts.process_request(&Context::builder().build(), &mut req)
        .await
        .expect("ProcessRequest failed");
    let config = req
        .config
        .as_ref()
        .expect("ProcessRequest: req.Config is nil");
    let si = config
        .system_instruction
        .as_ref()
        .expect("ProcessRequest: req.Config.SystemInstruction is nil");
    assert_eq!(
        si.parts.len(),
        1,
        "ProcessRequest: got {} parts, expected 1",
        si.parts.len()
    );
    let got_text = &si.parts[0].text;
    for want in [
        "SKILL.md",
        "skills",
        "<available_skills>",
        "skill0",
        "description1",
    ] {
        assert!(
            got_text.contains(want),
            "ProcessRequest: got {got_text:?}, want to contain {want:?}"
        );
    }
}

// parity: adapted: Go passes a nil agent.Context; Rust passes a default Context::builder() context
#[tokio::test]
async fn test_process_request_no_skills() {
    let ts = toolset(MockSource::default());
    let mut req = LlmRequest::default();

    ts.process_request(&Context::builder().build(), &mut req)
        .await
        .expect("ProcessRequest failed");
    if let Some(si) = req
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
    {
        assert!(
            si.parts.is_empty(),
            "ProcessRequest: got {} parts, expected 0",
            si.parts.len()
        );
    }
}

// parity: skipped TestNew_MissingSource: SkillToolsetConfig.source is an Arc<dyn Source>, so a missing source cannot be expressed and SkillToolset::new cannot fail (spec R11)

// parity: adapted: Go passes a nil ReadonlyContext to Tools; Rust passes a default context's readonly view
#[tokio::test]
async fn test_tools() {
    let ts = toolset(MockSource::default());
    let ctx = Context::builder().build();

    let tools = ts.tools(ctx.as_readonly()).await.expect("Tools failed");
    let mut got: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    got.sort_unstable();
    assert_eq!(got, ["list_skills", "load_skill", "load_skill_resource"]);
}

/// Spec R11: `tools()` returns the same three instances on every call.
#[tokio::test]
async fn tools_are_the_same_instances_on_every_call() {
    let ts = toolset(MockSource::default());
    let ctx = Context::builder().build();
    let first = ts.tools(ctx.as_readonly()).await.unwrap();
    let second = ts.tools(ctx.as_readonly()).await.unwrap();
    assert_eq!(first.len(), 3);
    for (a, b) in first.iter().zip(&second) {
        assert!(Arc::ptr_eq(a, b), "{} was rebuilt", a.name());
    }
}

/// Go `New`'s defaulting: an unset or empty name is `SkillToolset`; a custom system instruction
/// replaces the default one.
#[tokio::test]
async fn name_and_system_instruction_defaults() {
    assert_eq!(toolset(MockSource::default()).name(), "SkillToolset");
    let empty = SkillToolset::new(SkillToolsetConfig {
        source: Arc::new(MockSource {
            frontmatters: vec![Frontmatter {
                name: "a".into(),
                description: "b".into(),
                ..Default::default()
            }],
        }),
        name: Some(String::new()),
        system_instruction: Some(String::new()),
    });
    assert_eq!(empty.name(), "SkillToolset");
    // An empty system instruction falls back to the default one (which is private to the crate;
    // its exact text is pinned by the lib test `default_instruction_is_go_text`).
    let mut req = LlmRequest::default();
    empty
        .process_request(&Context::builder().build(), &mut req)
        .await
        .unwrap();
    let text = &req.config.unwrap().system_instruction.unwrap().parts[0].text;
    assert!(
        text.starts_with("You can use specialized 'skills' to help you with complex tasks."),
        "empty system_instruction did not default: {text:?}"
    );
    assert!(
        text.ends_with(
            "Do NOT use other tools to access these files.\n\n\n<available_skills>\n<skill>\n<name>\na\n</name>\n<description>\nb\n</description>\n</skill>\n</available_skills>"
        ),
        "default instruction and skill list not joined as Go does: {text:?}"
    );

    let custom = SkillToolset::new(SkillToolsetConfig {
        source: Arc::new(MockSource {
            frontmatters: vec![Frontmatter {
                name: "a".into(),
                description: "b".into(),
                ..Default::default()
            }],
        }),
        name: Some("my_skills".into()),
        system_instruction: Some("Use skills.".into()),
    });
    assert_eq!(custom.name(), "my_skills");
    let mut req = LlmRequest::default();
    custom
        .process_request(&Context::builder().build(), &mut req)
        .await
        .unwrap();
    let text = &req.config.unwrap().system_instruction.unwrap().parts[0].text;
    assert_eq!(
        text,
        "Use skills.\n\n<available_skills>\n<skill>\n<name>\na\n</name>\n<description>\nb\n</description>\n</skill>\n</available_skills>"
    );
}
