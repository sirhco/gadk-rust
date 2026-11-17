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

//! Deterministic checks on a reply: `contains`, `not_contains`, `regex` and `equals`.

use regex::Regex;

use crate::result::{ScoreResult, excerpt};

/// The longest reply excerpt quoted in a rationale.
const QUOTE_MAX: usize = 200;

/// `contains` (`negate == false`) or `not_contains` (`negate == true`) of `needle` in `reply`.
pub(crate) fn contains(reply: &str, needle: &str, ignore_case: bool, negate: bool) -> ScoreResult {
    let found = if ignore_case {
        reply.to_lowercase().contains(&needle.to_lowercase())
    } else {
        reply.contains(needle)
    };
    let kind = if negate { "not_contains" } else { "contains" };
    let rationale = if found {
        format!("reply contains {needle:?}")
    } else {
        format!("reply does not contain {needle:?}")
    };
    ScoreResult::check(kind, found != negate, rationale)
}

/// `regex`: `pattern` matches somewhere in `reply`.
pub(crate) fn regex(reply: &str, pattern: &Regex) -> ScoreResult {
    let found = pattern.is_match(reply);
    let rationale = if found {
        format!("reply matches /{pattern}/")
    } else {
        format!("reply does not match /{pattern}/")
    };
    ScoreResult::check("regex", found, rationale)
}

/// `equals`: `reply` equals `expected`, both trimmed.
pub(crate) fn equals(reply: &str, expected: &str) -> ScoreResult {
    let reply = reply.trim();
    if reply == expected.trim() {
        ScoreResult::check("equals", true, "reply equals the expected text")
    } else {
        ScoreResult::check(
            "equals",
            false,
            format!("reply differs: got {:?}", excerpt(reply, QUOTE_MAX)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Outcome;

    #[test]
    fn contains_and_not_contains() {
        assert_eq!(
            contains("Within 30 days", "30 days", false, false).outcome,
            Outcome::Pass
        );
        assert_eq!(
            contains("Within 30 days", "30 DAYS", false, false).outcome,
            Outcome::Fail
        );
        assert_eq!(
            contains("Within 30 days", "30 DAYS", true, false).outcome,
            Outcome::Pass
        );
        let r = contains("we guarantee it", "guarantee", false, true);
        assert_eq!(
            (r.kind.as_str(), r.outcome),
            ("not_contains", Outcome::Fail)
        );
        assert_eq!(r.rationale, "reply contains \"guarantee\"");
        assert_eq!(
            contains("fine", "guarantee", false, true).outcome,
            Outcome::Pass
        );
        assert_eq!(contains("x", "y", false, false).score, None);
    }

    #[test]
    fn regex_matches_anywhere() {
        let re = Regex::new("(?i)store credit").unwrap();
        assert_eq!(
            regex("We can offer Store Credit.", &re).outcome,
            Outcome::Pass
        );
        let r = regex("No.", &re);
        assert_eq!(r.outcome, Outcome::Fail);
        assert_eq!(r.rationale, "reply does not match /(?i)store credit/");
    }

    #[test]
    fn equals_trims_and_quotes_a_bounded_excerpt() {
        assert_eq!(equals("  OK\n", "OK").outcome, Outcome::Pass);
        let long = "é".repeat(300);
        let r = equals(&long, "OK");
        assert_eq!(r.outcome, Outcome::Fail);
        assert!(r.rationale.len() <= QUOTE_MAX + 30, "{}", r.rationale.len());
    }
}
