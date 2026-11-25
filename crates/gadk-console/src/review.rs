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

//! The review queue: the recent traces that have no console score of a given name yet, or
//! that miss a console score for some criterion of a rubric.
//!
//! A queue is defined by URL parameters ([`ReviewQueue`]). Its pending traces come from
//! [`gadk_analytics::review`], with the queue's filters ([`ReviewQueue::analytics`]) and the
//! score names it waits for ([`ReviewQueue::names`]).

use crate::rubrics::Rubrics;
use crate::traces::TimeRange;
use crate::url::percent_encode;

/// The note shown by `/review` and `/review/next` when the trace source is not BigQuery.
pub(crate) const NEEDS_BIGQUERY_REVIEW: &str = "Review needs the BigQuery trace source: start the console with --traces bigquery://<project>.<dataset>.<table>.";

/// The longest score name, in bytes after trimming (the trace page's score-name input is
/// `maxlength="100"`).
const MAX_SCORE_LEN: usize = 100;

/// What a queue's traces are waiting for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    /// A console score with this name, trimmed (1-100 bytes).
    Score(String),
    /// A console score for every criterion of the loaded rubric with this name.
    Rubric(String),
}

impl Target {
    /// The score name or the rubric name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Target::Score(name) | Target::Rubric(name) => name,
        }
    }
}

/// A review queue: the traces of the last `range` with an LLM response that still miss the
/// console scores of `target`, optionally only those that called `prompt` (at `version`) or
/// that carry an app thumbs-down.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReviewQueue {
    /// The score or rubric the queue waits for.
    pub(crate) target: Target,
    /// Only traces whose LLM requests used this prompt.
    pub(crate) prompt: Option<String>,
    /// Only that version of `prompt` (never set without `prompt`).
    pub(crate) version: Option<u32>,
    /// Only traces with an app thumbs-down.
    pub(crate) flagged: bool,
    /// The window, ending now.
    pub(crate) range: TimeRange,
}

impl ReviewQueue {
    /// The queue of the URL parameters `score` or `rubric` (one of the `rubrics`), `prompt`,
    /// `version`, `flagged` and `range` (the first value of each counts and is trimmed; a
    /// blank `score`, `rubric`, `prompt`, `version` or `range` is absent).
    ///
    /// `Ok(None)` when both `score` and `rubric` are absent, before any other parameter is
    /// checked.
    ///
    /// # Errors
    ///
    /// The text of the 400 page: `score and rubric: choose one`, `` rubric: unknown rubric
    /// `<name>` ``, `score: must be at most 100 bytes`, `version: must be a positive integer`,
    /// `version: needs a prompt`, `range: must be 1h, 6h, 24h or 7d`.
    pub(crate) fn from_params(
        params: &[(String, String)],
        rubrics: Option<&Rubrics>,
    ) -> Result<Option<ReviewQueue>, String> {
        let first = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let present = |key: &str| first(key).filter(|v| !v.trim().is_empty());
        let target = match (
            present("score").map(str::trim),
            present("rubric").map(str::trim),
        ) {
            (None, None) => return Ok(None),
            (Some(_), Some(_)) => return Err("score and rubric: choose one".to_owned()),
            (None, Some(rubric)) => {
                if rubrics.and_then(|r| r.get(rubric)).is_none() {
                    return Err(format!("rubric: unknown rubric `{rubric}`"));
                }
                Target::Rubric(rubric.to_owned())
            }
            (Some(score), None) => {
                if score.len() > MAX_SCORE_LEN {
                    return Err(format!("score: must be at most {MAX_SCORE_LEN} bytes"));
                }
                Target::Score(score.to_owned())
            }
        };
        let prompt = present("prompt").map(|p| p.trim().to_owned());
        let version = match present("version") {
            None => None,
            Some(v) => {
                let v = v.trim();
                match v.parse::<u32>() {
                    // `parse` alone would take a leading `+`.
                    Ok(n) if n > 0 && v.bytes().all(|b| b.is_ascii_digit()) => Some(n),
                    _ => return Err("version: must be a positive integer".to_owned()),
                }
            }
        };
        if version.is_some() && prompt.is_none() {
            return Err("version: needs a prompt".to_owned());
        }
        let range = match present("range") {
            None => TimeRange::Days7,
            Some(r) => TimeRange::parse(r.trim())
                .ok_or_else(|| "range: must be 1h, 6h, 24h or 7d".to_owned())?,
        };
        Ok(Some(ReviewQueue {
            target,
            prompt,
            version,
            flagged: first("flagged") == Some("1"),
            range,
        }))
    }

    /// The score names a trace needs to leave the queue: the score, or `<rubric>/<criterion>`
    /// for each criterion of the rubric in `rubrics` (none when it is not there).
    pub(crate) fn names(&self, rubrics: Option<&Rubrics>) -> Vec<String> {
        match &self.target {
            Target::Score(score) => vec![score.clone()],
            Target::Rubric(name) => rubrics
                .and_then(|r| r.get(name))
                .map(|r| r.score_names())
                .unwrap_or_default(),
        }
    }

    /// The queue's filters, for the gadk-analytics review queries.
    pub(crate) fn analytics(&self) -> gadk_analytics::review::ReviewQueue {
        let mut q = gadk_analytics::review::ReviewQueue::new(self.range).with_flagged(self.flagged);
        if let Some(prompt) = &self.prompt {
            q = q.with_prompt(prompt.clone());
        }
        if let Some(version) = self.version {
            q = q.with_version(version);
        }
        q
    }

    /// The queue as URL parameters, in the order `score` (or `rubric`), `prompt`, `version`,
    /// `flagged`, `range`, each value percent-encoded. Absent filters, `flagged` off and the
    /// default range (7d) are left out.
    pub(crate) fn query_string(&self) -> String {
        let mut out = match &self.target {
            Target::Score(score) => format!("score={}", percent_encode(score)),
            Target::Rubric(rubric) => format!("rubric={}", percent_encode(rubric)),
        };
        if let Some(prompt) = &self.prompt {
            out.push_str(&format!("&prompt={}", percent_encode(prompt)));
        }
        if let Some(version) = self.version {
            out.push_str(&format!("&version={version}"));
        }
        if self.flagged {
            out.push_str("&flagged=1");
        }
        if self.range != TimeRange::Days7 {
            out.push_str(&format!("&range={}", self.range.as_str()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::traces::BigQueryTable;
    use gadk_analytics::AnalyticsSource;
    use gadk_analytics::review::Cursor;
    use gadk_analytics::testutil::{FakeHttp, param_value, pending_reply};

    const TRACE: &str = "0123456789abcdef0123456789abcdef";

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// The pairs of a query string, percent-decoded.
    fn decode(qs: &str) -> Vec<(String, String)> {
        let unescape = |s: &str| {
            let b = s.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        };
        qs.split('&')
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap();
                (unescape(k), unescape(v))
            })
            .collect()
    }

    fn queue(pairs: &[(&str, &str)]) -> ReviewQueue {
        ReviewQueue::from_params(&params(pairs), None)
            .unwrap()
            .unwrap()
    }

    fn source(fake: &Arc<FakeHttp>) -> AnalyticsSource {
        AnalyticsSource::new(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
    }

    #[test]
    fn a_queue_takes_its_filters_and_defaults_to_seven_days() {
        assert_eq!(
            queue(&[("score", "  quality ")]),
            ReviewQueue {
                target: Target::Score("quality".into()),
                prompt: None,
                version: None,
                flagged: false,
                range: TimeRange::Days7,
            }
        );
        assert_eq!(
            queue(&[
                ("range", "24h"),
                ("flagged", "1"),
                ("version", "3"),
                ("prompt", "triage"),
                ("score", "quality"),
                ("score", "ignored"),
            ]),
            ReviewQueue {
                target: Target::Score("quality".into()),
                prompt: Some("triage".into()),
                version: Some(3),
                flagged: true,
                range: TimeRange::Hours24,
            }
        );
        // Blank optional values are absent; any `flagged` other than 1 is off.
        assert_eq!(
            queue(&[
                ("score", "q"),
                ("prompt", " "),
                ("version", ""),
                ("range", ""),
                ("flagged", "yes"),
            ]),
            queue(&[("score", "q")])
        );
        assert_eq!(
            queue(&[("score", &"x".repeat(100))]).target.name().len(),
            100
        );
    }

    #[test]
    fn padded_prompt_and_range_are_trimmed() {
        let q = queue(&[("score", "q"), ("prompt", " triage "), ("range", " 7d ")]);
        assert_eq!(q.prompt.as_deref(), Some("triage"));
        assert_eq!(q.range, TimeRange::Days7);
        let q = queue(&[("score", "q"), ("range", "\t24h ")]);
        assert_eq!(q.range, TimeRange::Hours24);
        assert_eq!(queue(&[("score", "q"), ("prompt", "   ")]).prompt, None);
    }

    #[test]
    fn no_score_is_no_queue_before_anything_else_is_checked() {
        for pairs in [
            &[][..],
            &[("score", "")][..],
            &[("score", "  "), ("version", "x"), ("range", "1y")][..],
        ] {
            assert_eq!(
                ReviewQueue::from_params(&params(pairs), None),
                Ok(None),
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn invalid_values_are_errors() {
        let err =
            |pairs: &[(&str, &str)]| ReviewQueue::from_params(&params(pairs), None).unwrap_err();
        assert_eq!(
            err(&[("score", "q"), ("version", "2")]),
            "version: needs a prompt"
        );
        for v in ["0", "x", "-1", "1.5"] {
            assert_eq!(
                err(&[("score", "q"), ("prompt", "p"), ("version", v)]),
                "version: must be a positive integer",
                "{v}"
            );
        }
        assert_eq!(
            err(&[("score", "q"), ("range", "30d")]),
            "range: must be 1h, 6h, 24h or 7d"
        );
        for len in [101, 129] {
            assert_eq!(
                err(&[("score", &"x".repeat(len))]),
                "score: must be at most 100 bytes",
                "{len}"
            );
        }
    }

    #[test]
    fn a_version_is_ascii_digits_only() {
        let version = |v: &str| {
            ReviewQueue::from_params(
                &params(&[("score", "q"), ("prompt", "p"), ("version", v)]),
                None,
            )
            .map(|q| q.unwrap().version)
        };
        for v in ["+3", " +3 "] {
            assert_eq!(
                version(v),
                Err("version: must be a positive integer".to_owned()),
                "{v}"
            );
        }
        assert_eq!(version("3"), Ok(Some(3)));
        assert_eq!(version(" 3 "), Ok(Some(3)));
    }

    #[test]
    fn the_analytics_queue_carries_the_filters() {
        let q = queue(&[
            ("score", "quality"),
            ("prompt", "triage"),
            ("version", "3"),
            ("flagged", "1"),
            ("range", "24h"),
        ]);
        assert_eq!(
            q.analytics(),
            gadk_analytics::review::ReviewQueue::new(TimeRange::Hours24)
                .with_prompt("triage")
                .with_version(3)
                .with_flagged(true)
        );
        assert_eq!(
            queue(&[("score", "quality")]).analytics(),
            gadk_analytics::review::ReviewQueue::new(TimeRange::Days7)
        );
    }

    #[test]
    fn the_query_string_is_ordered_encoded_and_round_trips() {
        assert_eq!(queue(&[("score", "q")]).query_string(), "score=q");
        let q = queue(&[
            ("range", "1h"),
            ("flagged", "1"),
            ("version", "12"),
            ("prompt", "team/greet"),
            ("score", "a&b#c dé"),
        ]);
        let qs = q.query_string();
        assert_eq!(
            qs,
            "score=a%26b%23c%20d%C3%A9&prompt=team%2Fgreet&version=12&flagged=1&range=1h"
        );
        assert_eq!(ReviewQueue::from_params(&decode(&qs), None), Ok(Some(q)));
    }

    fn rubrics() -> Arc<Rubrics> {
        crate::web::testutil::test_rubrics()
    }

    fn rubric_queue(pairs: &[(&str, &str)]) -> ReviewQueue {
        ReviewQueue::from_params(&params(pairs), Some(&rubrics()))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_rubric_queue_takes_a_loaded_rubric_and_the_same_filters() {
        assert_eq!(
            rubric_queue(&[
                ("rubric", " support "),
                ("score", " "),
                ("prompt", "triage"),
                ("version", "2"),
                ("flagged", "1"),
                ("range", "6h"),
            ]),
            ReviewQueue {
                target: Target::Rubric("support".into()),
                prompt: Some("triage".into()),
                version: Some(2),
                flagged: true,
                range: TimeRange::Hours6,
            }
        );
        // A blank rubric is absent: the score queue is unchanged with rubrics loaded.
        assert_eq!(
            rubric_queue(&[("score", "quality"), ("rubric", "")]),
            queue(&[("score", "quality")])
        );
        assert_eq!(
            ReviewQueue::from_params(&params(&[("rubric", " ")]), Some(&rubrics())),
            Ok(None)
        );
    }

    #[test]
    fn rubric_errors_come_before_the_other_parameters() {
        let err = |pairs: &[(&str, &str)], rubrics: Option<&Rubrics>| {
            ReviewQueue::from_params(&params(pairs), rubrics).unwrap_err()
        };
        let loaded = rubrics();
        for r in [Some(&*loaded), None] {
            assert_eq!(
                err(
                    &[("score", "q"), ("rubric", "support"), ("version", "x")],
                    r
                ),
                "score and rubric: choose one"
            );
            assert_eq!(
                err(&[("score", &"x".repeat(101)), ("rubric", "nope")], r),
                "score and rubric: choose one"
            );
            assert_eq!(
                err(&[("rubric", "nope"), ("version", "x"), ("range", "1y")], r),
                "rubric: unknown rubric `nope`"
            );
        }
        assert_eq!(
            err(&[("rubric", "support")], None),
            "rubric: unknown rubric `support`",
            "no rubric is known without --rubrics"
        );
        assert_eq!(
            err(&[("rubric", "Support")], Some(&loaded)),
            "rubric: unknown rubric `Support`"
        );
        assert_eq!(
            err(&[("rubric", "support"), ("version", "2")], Some(&loaded)),
            "version: needs a prompt"
        );
    }

    #[test]
    fn a_rubric_queue_string_starts_with_the_rubric_and_round_trips() {
        let q = rubric_queue(&[("range", "1h"), ("prompt", "a b"), ("rubric", "safety")]);
        let qs = q.query_string();
        assert_eq!(qs, "rubric=safety&prompt=a%20b&range=1h");
        assert_eq!(
            ReviewQueue::from_params(&decode(&qs), Some(&rubrics())),
            Ok(Some(q))
        );
    }

    #[test]
    fn names_are_the_score_or_the_rubrics_criteria() {
        let loaded = rubrics();
        assert_eq!(
            queue(&[("score", "quality")]).names(Some(&loaded)),
            vec!["quality".to_owned()]
        );
        assert_eq!(
            queue(&[("score", "quality")]).names(None),
            vec!["quality".to_owned()]
        );
        let q = rubric_queue(&[("rubric", "support")]);
        assert_eq!(
            q.names(Some(&loaded)),
            vec!["support/accuracy".to_owned(), "support/tone".to_owned()]
        );
        assert_eq!(q.names(None), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_rubric_queue_sends_its_criterion_names() {
        let fake = FakeHttp::new(vec![pending_reply(vec![]), pending_reply(vec![])]);
        let src = source(&fake);
        let loaded = rubrics();
        let q = rubric_queue(&[("rubric", "support")]);
        assert!(
            src.pending(&q.analytics(), &q.names(Some(&loaded)), end())
                .await
                .unwrap()
                .is_empty()
        );
        let after = Cursor::parse("2026-11-23T10:00:00Z", TRACE).unwrap();
        assert_eq!(
            src.next_pending(&q.analytics(), &q.names(Some(&loaded)), end(), &after)
                .await
                .unwrap(),
            None
        );
        for i in 0..2 {
            let p = param_value(&fake.body(i), "names");
            assert_eq!(
                p["parameterType"],
                json!({ "type": "ARRAY", "arrayType": { "type": "STRING" } })
            );
            assert_eq!(
                p["parameterValue"],
                json!({ "arrayValues": [
                    { "value": "support/accuracy" },
                    { "value": "support/tone" },
                ] })
            );
        }
        let body = fake.body(0);
        let names: Vec<&str> = body["queryParameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"score"), "{names:?}");
    }
}
