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

//! Review rubrics: the `--rubrics` file, a YAML list of named rubrics whose criteria are each
//! scored on a small integer scale. A criterion `c` of rubric `R` is stored as the console
//! `number` score `R/c`.

use std::collections::HashSet;
use std::path::Path;

use gadk_analytics::scores::NewScore;
use serde_json::{Map, Value};

use crate::ConsoleError;

/// The most rubrics a file may hold.
const MAX_RUBRICS: usize = 20;
/// The most criteria a rubric may hold.
const MAX_CRITERIA: usize = 10;
/// The longest rubric or criterion name, in bytes.
const MAX_NAME_LEN: usize = 64;
/// The longest score name `R/c`, in bytes (the review queue's limit).
const MAX_SCORE_NAME_LEN: usize = 100;
/// The highest scale value.
const MAX_SCALE: u64 = 100;
/// The widest scale, `max - min` (11 steps).
const MAX_SCALE_SPAN: u64 = 10;
/// The scale of a criterion without one.
const DEFAULT_SCALE: (u32, u32) = (1, 5);

/// The 404 text of `POST /traces/{id}/rubric` without `--rubrics`.
pub(crate) const NO_RUBRICS: &str =
    "no rubrics are loaded (start the console with --rubrics <file>)";

/// One criterion of a rubric: an integer score from `min` to `max`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Criterion {
    /// `[A-Za-z0-9_-]{1,64}`, unique in its rubric.
    pub(crate) name: String,
    /// Shown next to the name on the form.
    pub(crate) description: Option<String>,
    /// The lowest score.
    pub(crate) min: u32,
    /// The highest score (`min < max <= min + 10`).
    pub(crate) max: u32,
}

/// A named set of criteria scored together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rubric {
    /// `[A-Za-z0-9_-]{1,64}`, unique in the file.
    pub(crate) name: String,
    /// Shown above the criteria on the form.
    pub(crate) description: Option<String>,
    /// 1-10 criteria, in file order.
    pub(crate) criteria: Vec<Criterion>,
}

impl Rubric {
    /// The score names of the criteria, `<rubric>/<criterion>`, in file order.
    pub(crate) fn score_names(&self) -> Vec<String> {
        self.criteria
            .iter()
            .map(|c| format!("{}/{}", self.name, c.name))
            .collect()
    }

    /// The `number` scores of a submitted rubric form, one per criterion in file order: score
    /// `<rubric>/<criterion>` with the value of field `c_<criterion>` (its first value,
    /// trimmed) and the shared `comment` (dropped when blank).
    ///
    /// # Errors
    ///
    /// The text of the 400 page for the first failing criterion: `` `<criterion>`: choose a
    /// score `` when the value is missing or blank, `` `<criterion>`: must be an integer from
    /// <min> to <max> `` when it is not an integer on the scale.
    pub(crate) fn scores_from_form(
        &self,
        form: &[(String, String)],
        comment: &str,
    ) -> Result<Vec<NewScore>, String> {
        self.criteria
            .iter()
            .map(|c| {
                let key = format!("c_{}", c.name);
                let value = form
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.trim())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("`{}`: choose a score", c.name))?;
                let n = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| {
                        value.bytes().all(|b| b.is_ascii_digit()) && (c.min..=c.max).contains(n)
                    })
                    .ok_or_else(|| {
                        format!(
                            "`{}`: must be an integer from {} to {}",
                            c.name, c.min, c.max
                        )
                    })?;
                NewScore::from_form(
                    &format!("{}/{}", self.name, c.name),
                    "number",
                    &n.to_string(),
                    comment,
                )
            })
            .collect()
    }
}

/// The rubrics of a `--rubrics` file, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rubrics(pub(crate) Vec<Rubric>);

impl Rubrics {
    /// The rubric named `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&Rubric> {
        self.0.iter().find(|r| r.name == name)
    }

    /// The rubrics in file order.
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, Rubric> {
        self.0.iter()
    }

    /// Reads and parses the rubric file at `path`.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Rubrics`] with the read error or the [`from_yaml`](Self::from_yaml)
    /// detail.
    pub(crate) fn load(path: &Path) -> Result<Rubrics, ConsoleError> {
        let err = |detail: String| ConsoleError::Rubrics {
            path: path.to_path_buf(),
            detail,
        };
        let text = std::fs::read_to_string(path).map_err(|e| err(e.to_string()))?;
        Self::from_yaml(&text).map_err(err)
    }

    /// Parses rubric-file text: a list of 1-20 `{name, description?, criteria}` maps, each
    /// criterion a `{name, description?, scale?}` map. Indices in the messages count from 1.
    ///
    /// # Errors
    ///
    /// The detail of [`ConsoleError::Rubrics`], for example `invalid YAML: …`, `the file must
    /// be a list of 1 to 20 rubrics` or `` rubrics[2]: duplicate criterion name `tone` ``.
    pub(crate) fn from_yaml(text: &str) -> Result<Rubrics, String> {
        let doc: Value = serde_saphyr::from_str(text).map_err(|e| format!("invalid YAML: {e}"))?;
        let items = match doc.as_array() {
            Some(items) if (1..=MAX_RUBRICS).contains(&items.len()) => items,
            _ => return Err("the file must be a list of 1 to 20 rubrics".to_owned()),
        };
        let mut rubrics: Vec<Rubric> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let rubric = parse_rubric(item, &format!("rubrics[{}]", i + 1))?;
            if rubrics.iter().any(|r| r.name == rubric.name) {
                return Err(format!("duplicate rubric name `{}`", rubric.name));
            }
            rubrics.push(rubric);
        }
        Ok(Rubrics(rubrics))
    }
}

/// Whether `s` is 1-64 of `[A-Za-z0-9_-]`.
fn is_name(s: &str) -> bool {
    (1..=MAX_NAME_LEN).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `Err` naming the first key of `m` not in `allowed`, prefixed with `at`.
fn check_keys(m: &Map<String, Value>, at: &str, allowed: &[&str]) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{at}: unknown key `{k}`")),
        None => Ok(()),
    }
}

/// The `name` of map `m` when it matches `[A-Za-z0-9_-]{1,64}`.
fn name_of(m: &Map<String, Value>, at: &str) -> Result<String, String> {
    match m.get("name").and_then(Value::as_str) {
        Some(name) if is_name(name) => Ok(name.to_owned()),
        _ => Err(format!("{at}.name must match [A-Za-z0-9_-]{{1,64}}")),
    }
}

/// The optional `description` of map `m`: absent, or a non-blank string.
fn description_of(m: &Map<String, Value>, at: &str) -> Result<Option<String>, String> {
    match m.get("description") {
        None => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!("{at}.description must be a non-empty string")),
    }
}

/// Rubric `item`, at `at` (`rubrics[i]`).
fn parse_rubric(item: &Value, at: &str) -> Result<Rubric, String> {
    let Some(m) = item.as_object() else {
        return Err(format!("{at}.name must match [A-Za-z0-9_-]{{1,64}}"));
    };
    check_keys(m, at, &["name", "description", "criteria"])?;
    let name = name_of(m, at)?;
    let description = description_of(m, at)?;
    let items = match m.get("criteria").and_then(Value::as_array) {
        Some(items) if (1..=MAX_CRITERIA).contains(&items.len()) => items,
        _ => {
            return Err(format!("{at}.criteria must be a list of 1 to 10 criteria"));
        }
    };
    let mut seen = HashSet::new();
    let mut criteria = Vec::with_capacity(items.len());
    for (j, item) in items.iter().enumerate() {
        let c = parse_criterion(item, &format!("{at}.criteria[{}]", j + 1), &name)?;
        if !seen.insert(c.name.clone()) {
            return Err(format!("{at}: duplicate criterion name `{}`", c.name));
        }
        criteria.push(c);
    }
    Ok(Rubric {
        name,
        description,
        criteria,
    })
}

/// Criterion `item` of rubric `rubric`, at `at` (`rubrics[i].criteria[j]`).
fn parse_criterion(item: &Value, at: &str, rubric: &str) -> Result<Criterion, String> {
    let Some(m) = item.as_object() else {
        return Err(format!("{at}.name must match [A-Za-z0-9_-]{{1,64}}"));
    };
    check_keys(m, at, &["name", "description", "scale"])?;
    let name = name_of(m, at)?;
    let score_name = format!("{rubric}/{name}");
    if score_name.len() > MAX_SCORE_NAME_LEN {
        return Err(format!(
            "{at}: score name `{score_name}` is longer than 100 bytes"
        ));
    }
    let description = description_of(m, at)?;
    let (min, max) = match m.get("scale") {
        None => DEFAULT_SCALE,
        Some(scale) => scale_of(scale).ok_or_else(|| {
            format!("{at}.scale must be [min, max] with 0 <= min < max <= 100 and at most 11 steps")
        })?,
    };
    Ok(Criterion {
        name,
        description,
        min,
        max,
    })
}

/// `[min, max]` with `0 <= min < max <= 100` and `max - min <= 10`.
fn scale_of(v: &Value) -> Option<(u32, u32)> {
    let [min, max] = v.as_array()?.as_slice() else {
        return None;
    };
    let (min, max) = (min.as_u64()?, max.as_u64()?);
    if min < max && max <= MAX_SCALE && max - min <= MAX_SCALE_SPAN {
        Some((u32::try_from(min).ok()?, u32::try_from(max).ok()?))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const VALID: &str = "\
- name: support
  description: Support answers
  criteria:
    - name: accuracy
      description: Is the answer right?
    - name: tone
      scale: [0, 3]
- name: safety
  criteria:
    - name: harmless
      scale: [90, 100]
";

    fn err(text: &str) -> String {
        Rubrics::from_yaml(text).unwrap_err()
    }

    #[test]
    fn a_valid_file_parses_in_order_with_the_default_scale() {
        let rubrics = Rubrics::from_yaml(VALID).unwrap();
        assert_eq!(
            rubrics,
            Rubrics(vec![
                Rubric {
                    name: "support".into(),
                    description: Some("Support answers".into()),
                    criteria: vec![
                        Criterion {
                            name: "accuracy".into(),
                            description: Some("Is the answer right?".into()),
                            min: 1,
                            max: 5,
                        },
                        Criterion {
                            name: "tone".into(),
                            description: None,
                            min: 0,
                            max: 3,
                        },
                    ],
                },
                Rubric {
                    name: "safety".into(),
                    description: None,
                    criteria: vec![Criterion {
                        name: "harmless".into(),
                        description: None,
                        min: 90,
                        max: 100,
                    }],
                },
            ])
        );
        assert_eq!(
            rubrics.get("support").unwrap().score_names(),
            vec!["support/accuracy".to_owned(), "support/tone".to_owned()]
        );
        assert_eq!(rubrics.get("safety").unwrap().criteria.len(), 1);
        assert_eq!(rubrics.get("Support"), None);
        assert_eq!(
            rubrics.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["support", "safety"]
        );
    }

    #[test]
    fn limits_are_inclusive() {
        let criteria: String = (0..10)
            .map(|j| format!("    - name: c{j}\n      scale: [0, 10]\n"))
            .collect();
        let rubrics: String = (0..20)
            .map(|i| format!("- name: r{i}\n  criteria:\n{criteria}"))
            .collect();
        let parsed = Rubrics::from_yaml(&rubrics).unwrap();
        assert_eq!(parsed.0.len(), 20);
        assert_eq!(parsed.0[19].criteria.len(), 10);
        let long = format!(
            "- name: {}\n  criteria:\n    - name: {}\n",
            "r".repeat(64),
            "c".repeat(35)
        );
        let parsed = Rubrics::from_yaml(&long).unwrap();
        assert_eq!(parsed.0[0].score_names()[0].len(), 100);
        let names = "- name: A-z_09\n  criteria:\n    - name: Z_-9\n      scale: [99, 100]\n";
        assert!(Rubrics::from_yaml(names).is_ok());
    }

    #[test]
    fn the_file_must_be_a_list_of_1_to_20_rubrics() {
        let twenty_one: String = (0..21)
            .map(|i| format!("- name: r{i}\n  criteria:\n    - name: c\n"))
            .collect();
        for text in [
            "[]",
            "name: support\n",
            "\"text\"",
            "7",
            twenty_one.as_str(),
        ] {
            assert_eq!(
                err(text),
                "the file must be a list of 1 to 20 rubrics",
                "{text}"
            );
        }
        assert!(err("- name: [unclosed\n").starts_with("invalid YAML: "));
    }

    #[test]
    fn invalid_rubrics_name_the_rubric_and_the_rule() {
        let ok = "  criteria:\n    - name: c\n";
        for (text, want) in [
            (
                format!("- name: a\n  weight: 2\n{ok}"),
                "rubrics[1]: unknown key `weight`",
            ),
            (
                format!("- name: ''\n{ok}"),
                "rubrics[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                format!("- name: a/b\n{ok}"),
                "rubrics[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                format!("- name: 7\n{ok}"),
                "rubrics[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                format!("- name: {}\n{ok}", "r".repeat(65)),
                "rubrics[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (ok.to_owned(), "the file must be a list of 1 to 20 rubrics"),
            (
                format!("- {ok}"),
                "rubrics[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                "- just text\n".to_owned(),
                "rubrics[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                format!("- name: a\n  description: ' '\n{ok}"),
                "rubrics[1].description must be a non-empty string",
            ),
            (
                format!("- name: a\n  description: 3\n{ok}"),
                "rubrics[1].description must be a non-empty string",
            ),
            (
                "- name: a\n".to_owned(),
                "rubrics[1].criteria must be a list of 1 to 10 criteria",
            ),
            (
                "- name: a\n  criteria: []\n".to_owned(),
                "rubrics[1].criteria must be a list of 1 to 10 criteria",
            ),
            (
                format!(
                    "- name: a\n  criteria:\n{}",
                    (0..11)
                        .map(|j| format!("    - name: c{j}\n"))
                        .collect::<String>()
                ),
                "rubrics[1].criteria must be a list of 1 to 10 criteria",
            ),
            (
                format!("- name: a\n{ok}- name: a\n{ok}"),
                "duplicate rubric name `a`",
            ),
        ] {
            assert_eq!(err(&text), want, "{text}");
        }
    }

    #[test]
    fn invalid_criteria_name_the_rubric_the_criterion_and_the_rule() {
        let head = "- name: a\n  criteria:\n    - name: ok\n- name: b\n  criteria:\n";
        for (criterion, want) in [
            (
                "    - name: c\n      weight: 1\n".to_owned(),
                "rubrics[2].criteria[1]: unknown key `weight`",
            ),
            (
                "    - name: c/d\n".to_owned(),
                "rubrics[2].criteria[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                "    - description: no name\n".to_owned(),
                "rubrics[2].criteria[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                "    - just text\n".to_owned(),
                "rubrics[2].criteria[1].name must match [A-Za-z0-9_-]{1,64}",
            ),
            (
                "    - name: c\n      description: ''\n".to_owned(),
                "rubrics[2].criteria[1].description must be a non-empty string",
            ),
            (
                "    - name: c\n    - name: c\n".to_owned(),
                "rubrics[2]: duplicate criterion name `c`",
            ),
            (
                "    - name: c\n    - name: d\n      weight: 1\n".to_owned(),
                "rubrics[2].criteria[2]: unknown key `weight`",
            ),
        ] {
            let text = format!("{head}{criterion}");
            assert_eq!(err(&text), want, "{text}");
        }
        let scale_err = "rubrics[2].criteria[1].scale must be [min, max] with 0 <= min < max <= 100 and at most 11 steps";
        for scale in [
            "[3, 3]",
            "[4, 2]",
            "[0, 11]",
            "[95, 101]",
            "[-1, 3]",
            "[1, 2, 3]",
            "[1]",
            "[1.5, 3]",
            "['1', '5']",
            "5",
            "{min: 1, max: 5}",
        ] {
            let text = format!("{head}    - name: c\n      scale: {scale}\n");
            assert_eq!(err(&text), scale_err, "{scale}");
        }
        let text = format!(
            "- name: {}\n  criteria:\n    - name: {}\n",
            "r".repeat(64),
            "c".repeat(36)
        );
        assert_eq!(
            err(&text),
            format!(
                "rubrics[1].criteria[1]: score name `{}/{}` is longer than 100 bytes",
                "r".repeat(64),
                "c".repeat(36)
            )
        );
    }

    fn form(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_complete_form_is_one_number_score_per_criterion_in_file_order() {
        let rubrics = Rubrics::from_yaml(VALID).unwrap();
        let support = rubrics.get("support").unwrap();
        let scores = support
            .scores_from_form(
                &form(&[
                    ("c_tone", " 0 "),
                    ("c_accuracy", "4"),
                    ("c_accuracy", "1"),
                    ("c_other", "x"),
                ]),
                "clear and kind",
            )
            .unwrap();
        assert_eq!(
            scores.iter().map(NewScore::content).collect::<Vec<_>>(),
            vec![
                serde_json::json!({"name": "support/accuracy", "value": 4.0, "data_type": "number", "comment": "clear and kind"}),
                serde_json::json!({"name": "support/tone", "value": 0.0, "data_type": "number", "comment": "clear and kind"}),
            ]
        );
        let scores = support
            .scores_from_form(&form(&[("c_accuracy", "5"), ("c_tone", "3")]), "  ")
            .unwrap();
        assert_eq!(
            scores[0].content(),
            serde_json::json!({"name": "support/accuracy", "value": 5.0, "data_type": "number"})
        );
        assert_eq!(
            scores[0].content().to_string(),
            r#"{"data_type":"number","name":"support/accuracy","value":5.0}"#
        );
    }

    #[test]
    fn the_first_failing_criterion_in_file_order_is_reported() {
        let rubrics = Rubrics::from_yaml(VALID).unwrap();
        let support = rubrics.get("support").unwrap();
        let err = |pairs: &[(&str, &str)]| support.scores_from_form(&form(pairs), "").unwrap_err();
        assert_eq!(err(&[]), "`accuracy`: choose a score");
        assert_eq!(err(&[("c_accuracy", " ")]), "`accuracy`: choose a score");
        assert_eq!(err(&[("c_tone", "9")]), "`accuracy`: choose a score");
        assert_eq!(err(&[("c_accuracy", "3")]), "`tone`: choose a score");
        for value in ["0", "6", "3.0", "+3", "-1", "x", "99999999999"] {
            assert_eq!(
                err(&[("c_accuracy", value), ("c_tone", "1")]),
                "`accuracy`: must be an integer from 1 to 5",
                "{value}"
            );
        }
        assert_eq!(
            err(&[("c_accuracy", "2"), ("c_tone", "4")]),
            "`tone`: must be an integer from 0 to 3"
        );
    }

    #[test]
    fn load_errors_name_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.yaml");
        let text = Rubrics::load(&missing).unwrap_err().to_string();
        assert!(
            text.starts_with(&format!("rubric file {}: ", missing.display())),
            "{text}"
        );
        let bad = dir.path().join("rubrics.yaml");
        std::fs::write(&bad, "[]").unwrap();
        let e = Rubrics::load(&bad).unwrap_err();
        assert!(matches!(e, ConsoleError::Rubrics { .. }), "{e}");
        assert_eq!(
            e.to_string(),
            format!(
                "rubric file {}: the file must be a list of 1 to 20 rubrics",
                bad.display()
            )
        );
        std::fs::write(&bad, VALID).unwrap();
        assert_eq!(
            Rubrics::load(&bad).unwrap(),
            Rubrics::from_yaml(VALID).unwrap()
        );
    }
}
