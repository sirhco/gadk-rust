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

//! Parity target for Go tool/toolutils/toolutils_test.go.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::any::Any;
use std::sync::Arc;

use gadk_agent::{Tool, pack_tool};
use gadk_genai::FunctionDeclaration;
use gadk_model::LlmRequest;

/// Port of Go fakeTool: a name and an optional declaration.
struct FakeTool {
    name: String,
    decl: Option<FunctionDeclaration>,
}
impl Tool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        ""
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        self.decl.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn fake(name: &str, with_decl: bool) -> Arc<dyn Tool> {
    Arc::new(FakeTool {
        name: name.into(),
        decl: with_decl.then(|| FunctionDeclaration {
            name: name.into(),
            ..Default::default()
        }),
    })
}

#[test]
fn test_pack_tool_single_tool() {
    let mut req = LlmRequest::default();
    pack_tool(&mut req, &fake("first", true)).unwrap();
    assert!(
        req.tools.contains_key("first"),
        "req.tools missing \"first\""
    );
    let cfg = req.config.as_ref().unwrap();
    assert_eq!(cfg.tools.len(), 1);
    assert_eq!(cfg.tools[0].function_declarations.len(), 1);
}

#[test]
fn test_pack_tool_second_tool_shares_same_genai_tool() {
    let mut req = LlmRequest::default();
    pack_tool(&mut req, &fake("first", true)).unwrap();
    pack_tool(&mut req, &fake("second", true)).unwrap();
    assert!(req.tools.contains_key("second"));
    let cfg = req.config.as_ref().unwrap();
    assert_eq!(cfg.tools.len(), 1, "second tool should append, not create");
    assert_eq!(cfg.tools[0].function_declarations.len(), 2);
}

#[test]
fn test_pack_tool_duplicate_name() {
    let mut req = LlmRequest::default();
    let t = fake("dup", true);
    pack_tool(&mut req, &t).unwrap();
    let err = pack_tool(&mut req, &t).expect_err("expected error for duplicate tool name");
    assert_eq!(err.to_string(), "duplicate tool: \"dup\"");
}

#[test]
fn test_pack_tool_nil_declaration() {
    let mut req = LlmRequest::default();
    pack_tool(&mut req, &fake("nodecl", false)).unwrap();
    assert!(
        req.tools.contains_key("nodecl"),
        "req.tools missing \"nodecl\""
    );
    let cfg = req.config.as_ref().expect("req.config should be created");
    assert!(
        cfg.tools.is_empty(),
        "config.tools should be empty, got {}",
        cfg.tools.len()
    );
}

// Go utils.AppendInstructions (internal/utils/utils.go:138) has no Go test; these pin its
// branches now that it is public as gadk_agent::append_instructions.

#[test]
fn append_instructions_empty_input_changes_nothing() {
    let mut req = LlmRequest::default();
    gadk_agent::append_instructions(&mut req, &[]);
    assert!(req.config.is_none(), "Go returns before creating Config");
}

#[test]
fn append_instructions_creates_a_user_system_instruction() {
    let mut req = LlmRequest::default();
    gadk_agent::append_instructions(&mut req, &["first", "second"]);
    let si = req.config.unwrap().system_instruction.unwrap();
    assert_eq!(si.role, "user");
    assert_eq!(si.parts.len(), 1);
    assert_eq!(si.parts[0].text, "first\n\nsecond");
}

#[test]
fn append_instructions_extends_a_non_empty_last_part() {
    let mut req = LlmRequest {
        config: Some(gadk_genai::GenerateContentConfig {
            system_instruction: Some(gadk_genai::Content {
                role: "system".into(),
                parts: vec![
                    gadk_genai::Part::from_text("one"),
                    gadk_genai::Part::from_text("two"),
                ],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    gadk_agent::append_instructions(&mut req, &["three"]);
    let si = req.config.unwrap().system_instruction.unwrap();
    assert_eq!(si.role, "system", "the existing role is kept");
    let texts: Vec<_> = si.parts.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(texts, ["one", "two\n\nthree"]);
}

#[test]
fn append_instructions_adds_a_part_after_an_empty_or_missing_last_text() {
    for parts in [vec![], vec![gadk_genai::Part::default()]] {
        let n = parts.len();
        let mut req = LlmRequest {
            config: Some(gadk_genai::GenerateContentConfig {
                system_instruction: Some(gadk_genai::Content {
                    role: "user".into(),
                    parts,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        gadk_agent::append_instructions(&mut req, &["x", "y"]);
        let si = req.config.unwrap().system_instruction.unwrap();
        assert_eq!(si.parts.len(), n + 1, "a new part is appended");
        assert_eq!(si.parts[n].text, "x\n\ny");
    }
}
