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

//! Parity port of Go internal/llminternal/clone_test.go.

use gadk_genai::{Content, GenerateContentConfig, Part};
use pretty_assertions::assert_eq;

use crate::llm::processors::basic::clone_generate_content_config;

// parity: adapted: Go clones an arbitrary struct via reflection; Rust Clone is per-type, so the deep-copy guarantee is checked on GenerateContentConfig (the only production use)
#[test]
fn test_clone() {
    let original = GenerateContentConfig {
        system_instruction: Some(Content {
            role: "user".into(),
            parts: vec![Part {
                text: "a".into(),
                ..Default::default()
            }],
        }),
        stop_sequences: vec!["x".into()],
        ..Default::default()
    };
    let mut cloned = clone_generate_content_config(Some(&original)).unwrap();
    assert_eq!(cloned, original);
    cloned.system_instruction.as_mut().unwrap().parts[0].text = "changed".into();
    cloned.stop_sequences[0] = "y".into();
    assert_eq!(
        original.system_instruction.as_ref().unwrap().parts[0].text,
        "a"
    );
    assert_eq!(original.stop_sequences[0], "x");
}

// parity: adapted: Go nil pointer maps to Option::None
#[test]
fn test_clone_nil() {
    assert_eq!(clone_generate_content_config(None), None);
}

// parity: skipped TestCloneUnexported: Rust Clone has no reflection over unexported fields, so the Go panic path does not exist
