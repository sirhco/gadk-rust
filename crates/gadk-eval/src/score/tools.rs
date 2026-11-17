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

//! The `tools` scorer: which tools the agent called, optionally in order and with which args.

use crate::result::{ScoreResult, ToolCall};
use crate::spec::ToolExpect;

/// Whether `call` satisfies `want`: same name, and `want.args` (if any) is a subset of its args.
fn matches(call: &ToolCall, want: &ToolExpect) -> bool {
    call.name == want.name
        && want
            .args
            .as_ref()
            .is_none_or(|a| a.iter().all(|(k, v)| call.args.get(k) == Some(v)))
}

fn describe(want: &ToolExpect) -> String {
    match &want.args {
        None => want.name.clone(),
        Some(a) => format!("{}({})", want.name, serde_json::Value::Object(a.clone())),
    }
}

fn names(calls: &[&ToolCall]) -> String {
    let n: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
    format!("[{}]", n.join(", "))
}

fn wanted(want: &[ToolExpect]) -> String {
    let n: Vec<String> = want.iter().map(describe).collect();
    format!("[{}]", n.join(", "))
}

/// Grades `calls` (every call of every turn, in order) against `want`.
///
/// - `exact`: the calls are exactly `want`, in order.
/// - `ordered`: `want` occurs in `calls` as a subsequence (other calls may sit between).
/// - otherwise: every `want` entry is matched by a distinct call, in any order.
pub(crate) fn tools(
    calls: &[&ToolCall],
    want: &[ToolExpect],
    ordered: bool,
    exact: bool,
) -> ScoreResult {
    let got = names(calls);
    if exact {
        let pass = calls.len() == want.len() && calls.iter().zip(want).all(|(c, w)| matches(c, w));
        let rationale = if pass {
            format!("called exactly {got}")
        } else {
            format!("expected exactly {}, called {got}", wanted(want))
        };
        return ScoreResult::check("tools", pass, rationale);
    }
    if ordered {
        let mut next = 0;
        for c in calls {
            if want.get(next).is_some_and(|w| matches(c, w)) {
                next += 1;
            }
        }
        return match want.get(next) {
            None => ScoreResult::check(
                "tools",
                true,
                format!("called {} in order; called {got}", wanted(want)),
            ),
            Some(w) => ScoreResult::check(
                "tools",
                false,
                format!(
                    "missing {} in order {}; called {got}",
                    describe(w),
                    wanted(want)
                ),
            ),
        };
    }
    // Unordered: match the most specific expectations (with args) first, each to a distinct call.
    let mut order: Vec<&ToolExpect> = want.iter().collect();
    order.sort_by_key(|w| w.args.is_none());
    let mut used = vec![false; calls.len()];
    for w in order {
        match (0..calls.len()).find(|&i| !used[i] && matches(calls[i], w)) {
            Some(i) => used[i] = true,
            None => {
                return ScoreResult::check(
                    "tools",
                    false,
                    format!("missing call {}; called {got}", describe(w)),
                );
            }
        }
    }
    ScoreResult::check(
        "tools",
        true,
        format!("called {}; called {got}", wanted(want)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Outcome;
    use serde_json::{Value, json};

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
        }
    }

    fn want(name: &str, args: Option<Value>) -> ToolExpect {
        ToolExpect {
            name: name.into(),
            args: args.and_then(|a| a.as_object().cloned()),
        }
    }

    fn run(calls: &[ToolCall], w: &[ToolExpect], ordered: bool, exact: bool) -> ScoreResult {
        let refs: Vec<&ToolCall> = calls.iter().collect();
        tools(&refs, w, ordered, exact)
    }

    #[test]
    fn unordered_allows_extra_calls_and_any_order() {
        let calls = [
            call("b", json!({})),
            call("x", json!({})),
            call("a", json!({})),
        ];
        let r = run(&calls, &[want("a", None), want("b", None)], false, false);
        assert_eq!(r.outcome, Outcome::Pass, "{}", r.rationale);
    }

    #[test]
    fn unordered_is_a_multiset() {
        let calls = [call("a", json!({}))];
        let r = run(&calls, &[want("a", None), want("a", None)], false, false);
        assert_eq!(r.outcome, Outcome::Fail);
        assert_eq!(r.rationale, "missing call a; called [a]");
    }

    #[test]
    fn unordered_matches_specific_expectations_first() {
        let calls = [call("a", json!({"k": 1})), call("a", json!({"k": 2}))];
        let w = [want("a", None), want("a", Some(json!({"k": 1})))];
        assert_eq!(run(&calls, &w, false, false).outcome, Outcome::Pass);
    }

    #[test]
    fn ordered_is_a_subsequence() {
        let calls = [
            call("a", json!({})),
            call("x", json!({})),
            call("b", json!({})),
        ];
        assert_eq!(
            run(&calls, &[want("a", None), want("b", None)], true, false).outcome,
            Outcome::Pass
        );
        let r = run(&calls, &[want("b", None), want("a", None)], true, false);
        assert_eq!(r.outcome, Outcome::Fail);
        assert_eq!(r.rationale, "missing a in order [b, a]; called [a, x, b]");
    }

    #[test]
    fn exact_requires_the_whole_sequence() {
        let calls = [call("a", json!({})), call("b", json!({}))];
        assert_eq!(
            run(&calls, &[want("a", None), want("b", None)], false, true).outcome,
            Outcome::Pass
        );
        assert_eq!(
            run(&calls, &[want("a", None)], false, true).outcome,
            Outcome::Fail
        );
        assert_eq!(
            run(&calls, &[want("b", None), want("a", None)], false, true).outcome,
            Outcome::Fail
        );
    }

    #[test]
    fn args_are_a_subset_with_equal_values() {
        let calls = [call("lookup", json!({"order": "123", "verbose": true}))];
        assert_eq!(
            run(
                &calls,
                &[want("lookup", Some(json!({"order": "123"})))],
                false,
                false
            )
            .outcome,
            Outcome::Pass
        );
        let r = run(
            &calls,
            &[want("lookup", Some(json!({"order": 123})))],
            false,
            false,
        );
        assert_eq!(r.outcome, Outcome::Fail);
        assert_eq!(
            r.rationale,
            "missing call lookup({\"order\":123}); called [lookup]"
        );
    }

    #[test]
    fn no_calls_fails() {
        let r = run(&[], &[want("a", None)], false, false);
        assert_eq!(
            (r.outcome, r.rationale.as_str()),
            (Outcome::Fail, "missing call a; called []")
        );
    }
}
