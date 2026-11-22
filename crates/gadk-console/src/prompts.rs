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

//! Prompt-registry views: the prompt list, version history and version diffs.

use chrono::{DateTime, Utc};
use gadk_prompts::{PromptSnapshot, PromptVersion, RolloutSnapshot, Selector};
use similar::{ChangeTag, TextDiff};

/// One line of the prompt list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PromptRow {
    pub name: String,
    pub latest: u32,
    pub labels: Vec<(String, u32)>,
    pub created_at: DateTime<Utc>,
}

/// One row per prompt (sorted by name); labels sorted by label; `created_at` of the latest
/// version. Prompts with no versions are skipped.
pub(crate) fn prompt_rows(s: &PromptSnapshot) -> Vec<PromptRow> {
    s.prompts
        .iter()
        .filter_map(|(name, entry)| {
            let (latest, version) = entry.versions.iter().next_back()?;
            Some(PromptRow {
                name: name.clone(),
                latest: *latest,
                labels: entry
                    .labels
                    .iter()
                    .map(|(l, n)| (l.as_str().to_owned(), *n))
                    .collect(),
                created_at: version.meta.created_at,
            })
        })
        .collect()
}

/// One entry of a prompt's version history.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionRow {
    pub version: u32,
    pub labels: Vec<String>,
    /// `(label, percent)` of each rollout serving this version.
    pub rollouts: Vec<(String, u8)>,
    pub created_at: DateTime<Utc>,
    pub author: String,
    pub message: String,
    pub hash: String,
}

/// A prompt's version history, labels and rollouts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PromptDetail {
    pub name: String,
    pub latest: u32,
    /// Newest first.
    pub versions: Vec<VersionRow>,
    pub labels: Vec<(String, u32)>,
    /// `(label, rollout version, percent)` of each label with a rollout, sorted by label.
    pub rollouts: Vec<(String, u32, u8)>,
}

/// `None` when the prompt is unknown or has no versions. `rollouts` gives each label's
/// rollout.
pub(crate) fn prompt_detail(
    s: &PromptSnapshot,
    rollouts: &RolloutSnapshot,
    name: &str,
) -> Option<PromptDetail> {
    let entry = s.prompts.get(name)?;
    let latest = *entry.versions.keys().next_back()?;
    let labels: Vec<(String, u32)> = entry
        .labels
        .iter()
        .map(|(l, n)| (l.as_str().to_owned(), *n))
        .collect();
    let rollouts: Vec<(String, u32, u8)> = entry
        .labels
        .keys()
        .filter_map(|l| {
            rollouts
                .get(name, l)
                .map(|r| (l.as_str().to_owned(), r.version, r.percent))
        })
        .collect();
    let versions = entry
        .versions
        .iter()
        .rev()
        .map(|(n, v)| VersionRow {
            version: *n,
            labels: labels
                .iter()
                .filter(|(_, target)| target == n)
                .map(|(l, _)| l.clone())
                .collect(),
            rollouts: rollouts
                .iter()
                .filter(|(_, target, _)| target == n)
                .map(|(l, _, p)| (l.clone(), *p))
                .collect(),
            created_at: v.meta.created_at,
            author: v.meta.author.clone(),
            message: v.meta.message.clone(),
            hash: v.meta.hash.clone(),
        })
        .collect();
    Some(PromptDetail {
        name: name.to_owned(),
        latest,
        versions,
        labels,
        rollouts,
    })
}

/// One version's template and config.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionView {
    pub version: u32,
    pub template: String,
    /// `serde_json::to_string_pretty` of the config.
    pub config: Option<String>,
}

fn pretty_config(v: &PromptVersion) -> Option<String> {
    v.config
        .as_ref()
        .map(|c| serde_json::to_string_pretty(c).unwrap_or_default())
}

pub(crate) fn version_view(s: &PromptSnapshot, name: &str, version: u32) -> Option<VersionView> {
    let v = s.resolve(name, &Selector::Version(version)).ok()?;
    Some(VersionView {
        version,
        template: v.template.clone(),
        config: pretty_config(&v),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffTag {
    Equal,
    Delete,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub tag: DiffTag,
    /// 1-based line number in the old text; `None` for an insert.
    pub old_no: Option<usize>,
    /// 1-based line number in the new text; `None` for a delete.
    pub new_no: Option<usize>,
    /// The line without its trailing newline.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DiffView {
    pub a: u32,
    pub b: u32,
    pub template: Vec<DiffLine>,
    pub config: Vec<DiffLine>,
}

fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|c| DiffLine {
            tag: match c.tag() {
                ChangeTag::Equal => DiffTag::Equal,
                ChangeTag::Delete => DiffTag::Delete,
                ChangeTag::Insert => DiffTag::Insert,
            },
            old_no: c.old_index().map(|i| i + 1),
            new_no: c.new_index().map(|i| i + 1),
            text: c.value().trim_end_matches(['\r', '\n']).to_owned(),
        })
        .collect()
}

/// `similar::TextDiff::from_lines` on the templates and on the pretty configs (`""` when
/// `None`); 1-based line numbers. `None` when the prompt or either version is unknown.
pub(crate) fn diff_view(s: &PromptSnapshot, name: &str, a: u32, b: u32) -> Option<DiffView> {
    let va = s.resolve(name, &Selector::Version(a)).ok()?;
    let vb = s.resolve(name, &Selector::Version(b)).ok()?;
    Some(DiffView {
        a,
        b,
        template: diff_lines(&va.template, &vb.template),
        config: diff_lines(
            &pretty_config(&va).unwrap_or_default(),
            &pretty_config(&vb).unwrap_or_default(),
        ),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use chrono::TimeZone;
    use gadk_prompts::{Label, PromptEntry, PromptVersion, Rollout, VersionMeta};
    use serde_json::json;

    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 12, 0, 0).unwrap()
    }

    fn version(
        name: &str,
        n: u32,
        template: &str,
        config: Option<serde_json::Value>,
    ) -> Arc<PromptVersion> {
        Arc::new(PromptVersion {
            name: name.into(),
            version: n,
            template: template.into(),
            config,
            meta: VersionMeta {
                created_at: at(n),
                author: format!("author{n}"),
                message: format!("message{n}"),
                hash: format!("hash{n}"),
            },
        })
    }

    fn entry(versions: Vec<Arc<PromptVersion>>, labels: &[(&str, u32)]) -> PromptEntry {
        PromptEntry {
            versions: versions.into_iter().map(|v| (v.version, v)).collect(),
            labels: labels
                .iter()
                .map(|(l, n)| (Label::new(l).unwrap(), *n))
                .collect(),
        }
    }

    /// `zeta` has v1..v3 and labels `staging`->2, `production`->3; `alpha` has v1 and no
    /// labels; `empty` has no versions.
    fn snapshot() -> PromptSnapshot {
        let mut s = PromptSnapshot::default();
        s.prompts.insert(
            "zeta".into(),
            entry(
                vec![
                    version("zeta", 1, "a\nb\nc\n", None),
                    version("zeta", 2, "a\nB\nc\nd\n", Some(json!({"temperature": 0.5}))),
                    version(
                        "zeta",
                        3,
                        "final",
                        Some(json!({"temperature": 0.9, "n": 2})),
                    ),
                ],
                &[("staging", 2), ("production", 3)],
            ),
        );
        s.prompts.insert(
            "alpha".into(),
            entry(vec![version("alpha", 1, "hi", None)], &[]),
        );
        s.prompts.insert("empty".into(), entry(vec![], &[]));
        s
    }

    #[test]
    fn rows_sorted_with_latest_labels_and_created_at() {
        let rows = prompt_rows(&snapshot());
        assert_eq!(
            rows,
            vec![
                PromptRow {
                    name: "alpha".into(),
                    latest: 1,
                    labels: vec![],
                    created_at: at(1),
                },
                PromptRow {
                    name: "zeta".into(),
                    latest: 3,
                    labels: vec![("production".into(), 3), ("staging".into(), 2)],
                    created_at: at(3),
                },
            ]
        );
    }

    #[test]
    fn rows_skip_prompts_without_versions() {
        let rows = prompt_rows(&snapshot());
        assert!(rows.iter().all(|r| r.name != "empty"));
        assert!(prompt_rows(&PromptSnapshot::default()).is_empty());
    }

    #[test]
    fn detail_lists_versions_newest_first_with_their_labels() {
        let d = prompt_detail(&snapshot(), &RolloutSnapshot::default(), "zeta").unwrap();
        assert_eq!(d.name, "zeta");
        assert_eq!(d.latest, 3);
        assert_eq!(
            d.labels,
            vec![("production".to_string(), 3), ("staging".to_string(), 2)]
        );
        let order: Vec<u32> = d.versions.iter().map(|v| v.version).collect();
        assert_eq!(order, vec![3, 2, 1]);
        assert_eq!(
            d.versions[0],
            VersionRow {
                version: 3,
                labels: vec!["production".into()],
                rollouts: vec![],
                created_at: at(3),
                author: "author3".into(),
                message: "message3".into(),
                hash: "hash3".into(),
            }
        );
        assert_eq!(d.versions[1].labels, vec!["staging".to_string()]);
        assert!(d.versions[2].labels.is_empty());
        assert!(d.rollouts.is_empty());
    }

    #[test]
    fn detail_lists_rollouts_and_marks_their_versions() {
        let mut rollouts = RolloutSnapshot::default();
        rollouts.insert("zeta", Label::production(), Rollout::new(1, 10).unwrap());
        rollouts.insert("zeta", Label::staging(), Rollout::new(1, 50).unwrap());
        // A rollout of another prompt is ignored.
        rollouts.insert("alpha", Label::production(), Rollout::new(1, 20).unwrap());
        let d = prompt_detail(&snapshot(), &rollouts, "zeta").unwrap();
        assert_eq!(
            d.rollouts,
            vec![
                ("production".to_string(), 1, 10),
                ("staging".to_string(), 1, 50)
            ]
        );
        assert_eq!(
            d.versions[2].rollouts,
            vec![("production".to_string(), 10), ("staging".to_string(), 50)]
        );
        assert!(d.versions[0].rollouts.is_empty());
        assert!(d.versions[1].rollouts.is_empty());
    }

    #[test]
    fn detail_unknown_name_and_empty_prompt_are_none() {
        let none = RolloutSnapshot::default();
        assert_eq!(prompt_detail(&snapshot(), &none, "nope"), None);
        assert_eq!(prompt_detail(&snapshot(), &none, "empty"), None);
    }

    #[test]
    fn version_view_template_and_pretty_config() {
        let v = version_view(&snapshot(), "zeta", 2).unwrap();
        assert_eq!(v.version, 2);
        assert_eq!(v.template, "a\nB\nc\nd\n");
        assert_eq!(v.config.as_deref(), Some("{\n  \"temperature\": 0.5\n}"));
    }

    #[test]
    fn version_view_config_none_and_unknown() {
        let v = version_view(&snapshot(), "zeta", 1).unwrap();
        assert_eq!(v.config, None);
        assert_eq!(version_view(&snapshot(), "zeta", 9), None);
        assert_eq!(version_view(&snapshot(), "nope", 1), None);
    }

    fn line(tag: DiffTag, old: Option<usize>, new: Option<usize>, text: &str) -> DiffLine {
        DiffLine {
            tag,
            old_no: old,
            new_no: new,
            text: text.into(),
        }
    }

    #[test]
    fn diff_template_tags_and_line_numbers() {
        // v1 "a\nb\nc\n" -> v2 "a\nB\nc\nd\n"
        let d = diff_view(&snapshot(), "zeta", 1, 2).unwrap();
        assert_eq!((d.a, d.b), (1, 2));
        assert_eq!(
            d.template,
            vec![
                line(DiffTag::Equal, Some(1), Some(1), "a"),
                line(DiffTag::Delete, Some(2), None, "b"),
                line(DiffTag::Insert, None, Some(2), "B"),
                line(DiffTag::Equal, Some(3), Some(3), "c"),
                line(DiffTag::Insert, None, Some(4), "d"),
            ]
        );
    }

    #[test]
    fn diff_config_with_one_side_none() {
        // v1 has no config (""), v2 has {"temperature": 0.5}
        let d = diff_view(&snapshot(), "zeta", 1, 2).unwrap();
        assert_eq!(
            d.config,
            vec![
                line(DiffTag::Insert, None, Some(1), "{"),
                line(DiffTag::Insert, None, Some(2), "  \"temperature\": 0.5"),
                line(DiffTag::Insert, None, Some(3), "}"),
            ]
        );
        let back = diff_view(&snapshot(), "zeta", 2, 1).unwrap();
        assert_eq!(
            back.config,
            vec![
                line(DiffTag::Delete, Some(1), None, "{"),
                line(DiffTag::Delete, Some(2), None, "  \"temperature\": 0.5"),
                line(DiffTag::Delete, Some(3), None, "}"),
            ]
        );
    }

    #[test]
    fn diff_unknown_name_or_either_version_is_none() {
        let s = snapshot();
        assert_eq!(diff_view(&s, "nope", 1, 2), None);
        assert_eq!(diff_view(&s, "zeta", 9, 2), None);
        assert_eq!(diff_view(&s, "zeta", 1, 9), None);
    }
}
