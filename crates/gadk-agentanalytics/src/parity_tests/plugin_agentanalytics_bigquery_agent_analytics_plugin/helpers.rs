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
//! [`part_3`](super::part_3) share. The helpers shared by every 7b-3a port are
//! in [`support`](super::support).

use gadk_agent::AgentError;

pub(super) use super::support::{FakeTool, check_json_at, fake_tool, text_content};
use crate::redaction::COLUMN_CONTENT_PARTS;
use crate::row::{Row, RowValue};

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

/// The `content_parts` of `row` (Go `row["content_parts"].([]map[string]any)`), empty when
/// absent.
pub(super) fn parts_of(row: &Row) -> Vec<Row> {
    match row.get(COLUMN_CONTENT_PARTS) {
        Some(RowValue::Parts(parts)) => parts.clone(),
        _ => Vec::new(),
    }
}
