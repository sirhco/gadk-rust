// Copyright 2026 Chris Olson
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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The prompt and reply helpers from outside the crate, and `JudgeRequest` as a map key.

use std::collections::HashMap;

use gadk_eval::{JudgeKind, JudgeOutcome, JudgeRequest, judge_prompt, parse_judge_reply};

fn rubric(text: &str) -> JudgeRequest {
    JudgeRequest::new(
        JudgeKind::Rubric {
            rubric: text.into(),
        },
        "User: hi\nAgent: hello\n\n",
        "hello",
    )
}

#[test]
fn the_helpers_are_at_the_crate_root() {
    let prompt = judge_prompt(&rubric("Polite"));
    assert!(prompt.contains("Rubric:\nPolite"), "{prompt}");
    assert!(
        prompt.contains("Conversation:\nUser: hi\nAgent: hello\n"),
        "{prompt}"
    );
    assert_eq!(
        parse_judge_reply(r#"{"pass": true, "score": 5, "rationale": "kind"}"#),
        JudgeOutcome::Verdict {
            pass: true,
            score: 5,
            rationale: "kind".into(),
        }
    );
}

#[test]
fn requests_are_map_keys() {
    let mut outcomes: HashMap<JudgeRequest, JudgeOutcome> = HashMap::new();
    outcomes.insert(rubric("Polite"), JudgeOutcome::Error("first".into()));
    outcomes.insert(rubric("Polite"), JudgeOutcome::Error("second".into()));
    outcomes.insert(rubric("Brief"), JudgeOutcome::Error("third".into()));
    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        outcomes[&rubric("Polite")],
        JudgeOutcome::Error("second".into())
    );
}
