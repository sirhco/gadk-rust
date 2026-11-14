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

//! Go test helpers of `bigquery_agent_analytics_plugin_test.go` (and the `fakeTool` of
//! `arrow_test_helper_test.go`) that [`part_1`](super::part_1), [`part_2`](super::part_2) and
//! [`part_3`](super::part_3) share (owner: Task 10). The helpers shared by every 7b-3a port are
//! in [`support`](super::support).

use std::sync::Arc;

use gadk_agent::{AgentError, Tool};
use gadk_genai::{Content, FunctionDeclaration};
use serde_json::Value;

use super::support::{check_json, lookup};
use crate::redaction::COLUMN_CONTENT_PARTS;
use crate::row::{Row, RowValue};

/// Go `textContent(role, text)`.
pub(super) fn text_content(role: &str, text: &str) -> Content {
    Content::from_text(text, role)
}

/// Go `callbackErrors`: the errors returned by plugin callbacks.
#[derive(Default)]
pub(super) struct CallbackErrors(Vec<String>);

impl CallbackErrors {
    /// Go `errs.add(res, err)`.
    pub(super) fn add<T>(&mut self, r: Result<T, AgentError>) {
        if let Err(e) = r {
            self.0.push(e.to_string());
        }
    }

    /// Go `errs.check(t)`.
    #[track_caller]
    pub(super) fn check(&self) {
        assert!(self.0.is_empty(), "callback error: {}", self.0.join("\n"));
    }
}

/// Go `fakeTool` / `declaredTool` (`arrow_test_helper_test.go`): a tool for callback tests.
#[derive(Default)]
pub(super) struct FakeTool {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) long_running: bool,
    /// The description panics (a tool whose metadata is unavailable).
    pub(super) panic_on_description: bool,
    /// Go `declaredTool.Declaration()` (a `fakeTool` has none).
    pub(super) decl: Option<FunctionDeclaration>,
}

impl Tool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        assert!(!self.panic_on_description, "tool description unavailable");
        &self.description
    }
    fn is_long_running(&self) -> bool {
        self.long_running
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        self.decl.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `&fakeTool{name: name}`.
pub(super) fn fake_tool(name: &str) -> Arc<dyn Tool> {
    Arc::new(FakeTool {
        name: name.into(),
        ..FakeTool::default()
    })
}

/// Go `checkJSONAt(t, obj, want, path...)`.
#[track_caller]
pub(super) fn check_json_at(obj: &Value, want: &str, path: &[&str]) {
    let name = path.join(".");
    let got = lookup(obj, path).unwrap_or_else(|| panic!("{name} is missing; got {obj}"));
    check_json(&name, got, want).unwrap_or_else(|e| panic!("{e}"));
}

/// The `content_parts` of `row` (Go `row["content_parts"].([]map[string]any)`), empty when
/// absent.
pub(super) fn parts_of(row: &Row) -> Vec<Row> {
    match row.get(COLUMN_CONTENT_PARTS) {
        Some(RowValue::Parts(parts)) => parts.clone(),
        _ => Vec::new(),
    }
}
