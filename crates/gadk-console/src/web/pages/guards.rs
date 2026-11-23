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

//! The Guards list page: one card per guarded rollout with its latest decision and its
//! applied-step timeline, then the reports.

use std::sync::Arc;

use gadk_eval_monitor::{Action, GuardReport};
use maud::{Markup, html};

use super::eval_report::secs;
use super::layout::{Nav, page};
use super::prompt;
use crate::guards::guard_href;

/// One guarded rollout: the reports that share a prompt, label, baseline and rollout version.
pub(crate) struct RolloutSummary {
    /// The prompt name.
    pub prompt: String,
    /// The guarded label.
    pub label: String,
    /// The label's version.
    pub baseline: u32,
    /// The rollout version.
    pub rollout: u32,
    /// The newest report's stored name.
    pub latest_name: String,
    /// The newest report.
    pub latest: Arc<GuardReport>,
    /// How many reports the rollout has.
    pub n: usize,
    /// How many of them applied their action.
    pub k: usize,
    /// The [`timeline`] of the rollout's reports.
    pub timeline: String,
}

/// One summary per rollout from `reports` (`(stored name, report)`, newest first), keyed by
/// `(prompt, label, baseline, rollout)` and ordered by each rollout's newest report, newest
/// first.
pub(crate) fn group(reports: &[(String, Arc<GuardReport>)]) -> Vec<RolloutSummary> {
    // Each group's reports, newest first, in the order the groups are first seen.
    let mut groups: Vec<Vec<&(String, Arc<GuardReport>)>> = Vec::new();
    for entry in reports {
        let r = &entry.1;
        let same = |g: &&mut Vec<&(String, Arc<GuardReport>)>| {
            let x = &g[0].1;
            x.prompt == r.prompt
                && x.label == r.label
                && x.baseline == r.baseline
                && x.rollout == r.rollout
        };
        match groups.iter_mut().find(same) {
            Some(g) => g.push(entry),
            None => groups.push(vec![entry]),
        }
    }
    let mut out: Vec<RolloutSummary> = groups
        .into_iter()
        .map(|g| {
            let (latest_name, latest) = g[0].clone();
            let oldest_first: Vec<&GuardReport> = g.iter().rev().map(|(_, r)| r.as_ref()).collect();
            RolloutSummary {
                prompt: latest.prompt.clone(),
                label: latest.label.clone(),
                baseline: latest.baseline,
                rollout: latest.rollout,
                n: g.len(),
                k: g.iter().filter(|(_, r)| r.applied).count(),
                timeline: timeline(&oldest_first),
                latest_name,
                latest,
            }
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.latest.generated_at));
    out
}

/// The oldest report's `{percent}%`, then ` → {step}` per applied report, oldest first:
/// `ramped {q}%`, `promoted v{rollout}` or `aborted`. Empty without reports.
pub(crate) fn timeline(oldest_first: &[&GuardReport]) -> String {
    let Some(first) = oldest_first.first() else {
        return String::new();
    };
    let mut out = format!("{}%", first.percent);
    for r in oldest_first.iter().filter(|r| r.applied) {
        match r.action {
            Action::Ramp(q) => out.push_str(&format!(" → ramped {q}%")),
            Action::Promote => out.push_str(&format!(" → promoted v{}", r.rollout)),
            Action::Abort => out.push_str(" → aborted"),
            _ => {}
        }
    }
    out
}

/// The Guards list: `reports` are `(stored name, report)`, newest first; `filter` is the active
/// `?prompt=` filter. One card per rollout, then a table of the reports.
pub(crate) fn render(filter: Option<&str>, reports: &[(String, Arc<GuardReport>)]) -> Markup {
    let title = match filter {
        Some(p) => format!("Guards: {p}"),
        None => "Guards".to_owned(),
    };
    let body = html! {
        h1 { (title) }
        @if filter.is_some() {
            p class="filter" { a href="/guards" { "all guards" } }
        }
        @if reports.is_empty() {
            p class="empty" { "No guard runs yet" }
        } @else {
            div class="eval-cards" {
                @for s in group(reports) { (card(&s)) }
            }
            table {
                thead {
                    tr {
                        th { "Generated" } th { "Rollout" } th { "Versions" } th { "Percent" }
                        th { "Verdict" } th { "Action" } th { "Applied" } th { "Spec" }
                    }
                }
                tbody {
                    @for (name, r) in reports {
                        tr {
                            td class="mono" { a href=(guard_href(name)) { (secs(&r.generated_at)) } }
                            td { (r.prompt) "/" (r.label) }
                            td { "v" (r.baseline) " → v" (r.rollout) }
                            td class="num" { (r.percent) "%" }
                            td { (r.verdict.as_str()) }
                            td { (r.action_text()) }
                            td { @if r.applied { "applied" } @else { "dry run" } }
                            td { (r.spec) }
                        }
                    }
                }
            }
        }
    };
    page(&title, Nav::Guards, body)
}

fn card(s: &RolloutSummary) -> Markup {
    let r = &s.latest;
    html! {
        div class="eval-card guard-card" {
            h2 {
                a href=(prompt::href(&s.prompt)) {
                    (s.prompt) "/" (s.label) " v" (s.baseline) " → v" (s.rollout)
                }
            }
            p class="rate" { (r.verdict.as_str()) }
            p class="meta" {
                a href=(guard_href(&s.latest_name)) { (secs(&r.generated_at)) }
                " · " (r.percent) "% · " (r.action_text())
            }
            p class="meta" {
                (s.n) @if s.n == 1 { " report, " } @else { " reports, " } (s.k) " applied"
            }
            p class="meta" { (s.timeline) }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval_monitor::{Action, GuardReport};

    use super::{group, render, timeline};
    use crate::guards::guard_href;
    use crate::web::pages::prompt;
    use crate::web::testutil::guard;

    /// `(stored name, report)` with the name `{spec}-{timestamp}.json` the store would give.
    fn entry(r: GuardReport) -> (String, Arc<GuardReport>) {
        let name = format!(
            "{}-{}.json",
            r.spec,
            r.generated_at.format("%Y%m%dT%H%M%SZ")
        );
        (name, Arc::new(r))
    }

    /// Newest first: `b` (one dry run) then `team/a`'s abort, ramp and dry run.
    fn two_rollouts() -> Vec<(String, Arc<GuardReport>)> {
        vec![
            entry(guard("gb", 1_800_010_800, "b", 10, Action::Ramp(25), false)),
            entry(guard(
                "ga",
                1_800_007_200,
                "team/a",
                75,
                Action::Abort,
                true,
            )),
            entry(guard(
                "ga",
                1_800_003_600,
                "team/a",
                50,
                Action::Ramp(75),
                true,
            )),
            entry(guard(
                "ga",
                1_800_000_000,
                "team/a",
                50,
                Action::Ramp(75),
                false,
            )),
        ]
    }

    #[test]
    fn empty_list_says_so_under_the_guards_nav() {
        let out = render(None, &[]).into_string();
        assert!(
            out.contains("<title>Guards · gadk console</title>"),
            "{out}"
        );
        assert!(out.contains("<h1>Guards</h1>"), "{out}");
        assert!(
            out.contains(r#"<a class="active" href="/guards">Guards</a>"#),
            "{out}"
        );
        assert!(out.contains("No guard runs yet"), "{out}");
        assert!(!out.contains("<table"), "{out}");
        assert!(!out.contains("all guards"), "{out}");
    }

    #[test]
    fn a_filter_names_the_prompt_and_links_back() {
        let out = render(Some("a"), &[]).into_string();
        assert!(
            out.contains("<title>Guards: a · gadk console</title>"),
            "{out}"
        );
        assert!(out.contains("<h1>Guards: a</h1>"), "{out}");
        assert!(
            out.contains(r#"<p class="filter"><a href="/guards">all guards</a></p>"#),
            "{out}"
        );
        assert!(out.contains("No guard runs yet"), "{out}");
    }

    #[test]
    fn reports_group_into_rollouts_newest_first() {
        let groups = group(&two_rollouts());
        assert_eq!(groups.len(), 2);
        let b = &groups[0];
        assert_eq!((b.prompt.as_str(), b.label.as_str()), ("b", "production"));
        assert_eq!((b.baseline, b.rollout, b.n, b.k), (1, 2, 1, 0));
        assert_eq!(b.latest_name, "gb-20270115T110000Z.json");
        assert_eq!(b.timeline, "10%");
        let a = &groups[1];
        assert_eq!(a.prompt, "team/a");
        assert_eq!((a.n, a.k), (3, 2));
        assert_eq!(a.latest_name, "ga-20270115T100000Z.json");
        assert_eq!(a.latest.action, Action::Abort);
        assert_eq!(a.timeline, "50% → ramped 75% → aborted");
    }

    #[test]
    fn a_different_label_or_version_is_another_rollout() {
        let mut other_label = guard("g", 1_800_003_600, "p", 10, Action::Ramp(25), false);
        other_label.label = "staging".to_owned();
        let mut other_rollout = guard("g", 1_800_007_200, "p", 10, Action::Ramp(25), false);
        other_rollout.rollout = 3;
        let reports = vec![
            entry(other_rollout),
            entry(other_label),
            entry(guard("g", 1_800_000_000, "p", 10, Action::Ramp(25), false)),
        ];
        let groups = group(&reports);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|s| s.n == 1));
    }

    #[test]
    fn cards_are_ordered_by_their_newest_report() {
        // Listed out of time order: the card order still follows generated_at.
        let reports = vec![
            entry(guard("ga", 1_800_000_000, "a", 10, Action::Ramp(25), false)),
            entry(guard("gb", 1_800_003_600, "b", 10, Action::Ramp(25), false)),
        ];
        let groups = group(&reports);
        assert_eq!(groups[0].prompt, "b");
        assert_eq!(groups[1].prompt, "a");
    }

    #[test]
    fn a_promote_ends_the_timeline_with_the_rollout_version() {
        let ramp = guard("g", 1_800_000_000, "p", 25, Action::Ramp(50), true);
        let wait = guard("g", 1_800_003_600, "p", 50, Action::None, false);
        let promote = guard("g", 1_800_007_200, "p", 50, Action::Promote, true);
        assert_eq!(
            timeline(&[&ramp, &wait, &promote]),
            "25% → ramped 50% → promoted v2"
        );
        assert_eq!(timeline(&[&wait]), "50%");
        assert_eq!(timeline(&[]), "");
    }

    #[test]
    fn cards_show_the_rollout_its_latest_decision_and_counts() {
        let out = render(None, &two_rollouts()).into_string();
        let b = out
            .find(&format!(
                r#"<div class="eval-card guard-card"><h2><a href="{}">b/production v1 → v2</a></h2>"#,
                prompt::href("b")
            ))
            .expect(&out);
        let a = out
            .find(&format!(
                r#"<div class="eval-card guard-card"><h2><a href="{}">team/a/production v1 → v2</a></h2>"#,
                prompt::href("team/a")
            ))
            .expect(&out);
        assert!(b < a, "{out}");
        assert!(out.contains(r#"<a href="/prompts/team%2Fa">"#), "{out}");
        assert!(
            out.contains(concat!(
                r#"<p class="rate">abort</p><p class="meta">"#,
                r#"<a href="/guards/ga-20270115T100000Z.json">2027-01-15T10:00:00Z</a>"#,
                r#" · 75% · aborted</p><p class="meta">3 reports, 2 applied</p>"#,
                r#"<p class="meta">50% → ramped 75% → aborted</p></div>"#
            )),
            "{out}"
        );
        assert!(
            out.contains(concat!(
                r#"<p class="rate">advance</p><p class="meta">"#,
                r#"<a href="/guards/gb-20270115T110000Z.json">2027-01-15T11:00:00Z</a>"#,
                r#" · 10% · would ramp to 25%</p><p class="meta">1 report, 0 applied</p>"#,
                r#"<p class="meta">10%</p></div>"#
            )),
            "{out}"
        );
    }

    #[test]
    fn the_table_lists_every_report_newest_first() {
        let reports = two_rollouts();
        let out = render(None, &reports).into_string();
        assert!(
            out.contains("<th>Generated</th><th>Rollout</th><th>Versions</th><th>Percent</th><th>Verdict</th><th>Action</th><th>Applied</th><th>Spec</th>"),
            "{out}"
        );
        let rows: Vec<usize> = [
            concat!(
                r#"<tr><td class="mono"><a href="/guards/gb-20270115T110000Z.json">2027-01-15T11:00:00Z</a></td>"#,
                r#"<td>b/production</td><td>v1 → v2</td><td class="num">10%</td><td>advance</td>"#,
                r#"<td>would ramp to 25%</td><td>dry run</td><td>gb</td></tr>"#
            ),
            concat!(
                r#"<tr><td class="mono"><a href="/guards/ga-20270115T100000Z.json">2027-01-15T10:00:00Z</a></td>"#,
                r#"<td>team/a/production</td><td>v1 → v2</td><td class="num">75%</td><td>abort</td>"#,
                r#"<td>aborted</td><td>applied</td><td>ga</td></tr>"#
            ),
            concat!(
                r#"<tr><td class="mono"><a href="/guards/ga-20270115T090000Z.json">2027-01-15T09:00:00Z</a></td>"#,
                r#"<td>team/a/production</td><td>v1 → v2</td><td class="num">50%</td><td>advance</td>"#,
                r#"<td>ramped to 75%</td><td>applied</td><td>ga</td></tr>"#
            ),
            concat!(
                r#"<tr><td class="mono"><a href="/guards/ga-20270115T080000Z.json">2027-01-15T08:00:00Z</a></td>"#,
                r#"<td>team/a/production</td><td>v1 → v2</td><td class="num">50%</td><td>advance</td>"#,
                r#"<td>would ramp to 75%</td><td>dry run</td><td>ga</td></tr>"#
            ),
        ]
        .iter()
        .map(|row| out.find(row).expect(row))
        .collect();
        assert!(rows.windows(2).all(|w| w[0] < w[1]), "{out}");
        for (name, _) in &reports {
            assert!(
                out.contains(&format!(r#"href="{}""#, guard_href(name))),
                "{name}"
            );
        }
    }

    #[test]
    fn report_text_is_escaped_and_hrefs_encoded() {
        let mut r = guard("g", 1_800_000_000, "a <i>b</i>", 10, Action::Abort, false);
        r.spec = "<b>x</b>".to_owned();
        r.label = "<u>l</u>".to_owned();
        let reports = vec![("a b<c>.json".to_owned(), Arc::new(r))];
        let out = render(Some("<s>f</s>"), &reports).into_string();
        for raw in ["<b>x", "<i>b", "<u>l", "<s>f", "<c>"] {
            assert!(!out.contains(raw), "{raw}: {out}");
        }
        assert!(out.contains("&lt;b&gt;"), "{out}");
        assert!(out.contains("Guards: &lt;s&gt;f&lt;/s&gt;"), "{out}");
        assert!(
            out.contains(r#"<a href="/prompts/a%20%3Ci%3Eb%3C%2Fi%3E">"#),
            "{out}"
        );
        assert_eq!(
            out.matches(r#"href="/guards/a%20b%3Cc%3E.json""#).count(),
            2,
            "card and table: {out}"
        );
    }
}
