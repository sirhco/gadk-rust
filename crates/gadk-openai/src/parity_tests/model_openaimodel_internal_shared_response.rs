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

//! Port of ADK-Go `model/openaimodel/internal/shared/response_test.go`.

use gadk_genai::{Content, FunctionCall, Part, ROLE_MODEL};
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::shared::response::completed_content_supersedes;

fn call(id: &str, city: &str) -> Part {
    let mut args = serde_json::Map::new();
    args.insert("city".to_string(), json!(city));
    Part {
        function_call: Some(FunctionCall {
            id: id.to_string(),
            name: "get_weather".to_string(),
            args,
            ..FunctionCall::default()
        }),
        ..Part::default()
    }
}

fn text(t: &str) -> Part {
    Part {
        text: t.to_string(),
        ..Part::default()
    }
}

fn thought(t: &str) -> Part {
    Part {
        thought: true,
        ..text(t)
    }
}

#[test]
fn test_completed_content_supersedes() {
    let tests: Vec<(&str, Vec<Part>, Vec<Part>, bool)> = vec![
        (
            "reasoning containing the answer is not visible output",
            vec![text("hello")],
            vec![thought("hello is the answer")],
            false,
        ),
        (
            "reasoning alone does not justify replacement",
            vec![thought("Checking")],
            vec![thought("Checked")],
            false,
        ),
        (
            "completed text can precede and follow streamed refusal",
            vec![text("I cannot help.")],
            vec![text("Here is "), text("I cannot help."), text(" Sorry.")],
            true,
        ),
        (
            "shorter text does not replace the answer",
            vec![text("hello")],
            vec![text("hel")],
            false,
        ),
        (
            "same name with different arguments is a different call",
            vec![call("call_1", "SF")],
            vec![call("call_1", "NY")],
            false,
        ),
        (
            "different call IDs remain distinct",
            vec![call("call_1", "SF")],
            vec![call("call_2", "SF")],
            false,
        ),
        (
            "one completed call cannot replace two streamed calls",
            vec![call("call_1", "SF"), call("call_1", "SF")],
            vec![call("call_1", "SF")],
            false,
        ),
        (
            "matching calls allow recovery of completed text",
            vec![call("call_1", "SF")],
            vec![call("call_1", "SF"), text("Checking the weather.")],
            true,
        ),
        (
            "reordered calls can match one to one",
            vec![call("call_1", "SF"), call("call_2", "NY")],
            vec![call("call_2", "NY"), call("call_1", "SF")],
            true,
        ),
        (
            "completed function call replaces reasoning alone",
            vec![thought("Checking")],
            vec![call("call_1", "SF")],
            true,
        ),
    ];
    for (name, aggregate, completed, want) in tests {
        let aggregate = Content {
            role: ROLE_MODEL.to_string(),
            parts: aggregate,
        };
        let completed = Content {
            role: ROLE_MODEL.to_string(),
            parts: completed,
        };
        assert_eq!(
            completed_content_supersedes(Some(&aggregate), Some(&completed)),
            want,
            "{name}"
        );
    }
}
