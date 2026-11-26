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

//! The page for one guard report: its header, one row per check, the verdict and the action.

use gadk_eval_monitor::{CheckResult, GuardReport, SideValue, Unit};
use maud::{Markup, html};

use super::eval_report::secs;
use super::layout::{Nav, page};

/// The guard report stored as `name`: the spec, the rollout, the window, the stats (when the
/// spec set `sequential` or `confidence`), the source and when it was made, then the checks
/// table, the verdict, the note (when there is one) and the action, marked `applied` or
/// `dry run`.
pub(crate) fn render(name: &str, r: &GuardReport) -> Markup {
    let body = html! {
        h1 { "guard " (r.spec) }
        dl class="eval-meta" {
            dt { "Rollout" }
            dd {
                (r.prompt) "/" (r.label) " v" (r.baseline) " → v" (r.rollout) " (" (r.percent) "%)"
            }
            dt { "Window" } dd class="mono" { (secs(&r.window.from)) " – " (secs(&r.window.to)) }
            @if let Some(stats) = &r.stats {
                dt { "Stats" } dd { (stats.summary()) }
            }
            dt { "Source" } dd class="mono" { (r.source) }
            dt { "Generated" } dd class="mono" { (secs(&r.generated_at)) }
            dt { "Report" } dd class="mono" { (name) }
        }
        table {
            thead {
                tr {
                    th { "Check" } th { "Baseline" } th { "Rollout" } th { "Harm" }
                    th { "Interval" } th { "Status" }
                }
            }
            tbody {
                @for c in &r.checks { (check_row(c)) }
            }
        }
        p { "verdict: " (r.verdict.as_str()) }
        @if let Some(note) = &r.note {
            p { "note: " (note) }
        }
        p {
            "action: " (r.action_text()) " "
            span class="chip" { @if r.applied { "applied" } @else { "dry run" } }
        }
    };
    page(&format!("Guard {name}"), Nav::Guards, body)
}

fn check_row(c: &CheckResult) -> Markup {
    let status = c.status.as_str();
    html! {
        tr {
            td class="mono" { (c.name) }
            td class="num" { (side(c.unit, &c.baseline)) }
            td class="num" { (side(c.unit, &c.rollout)) }
            td class="num" { (c.unit.format_harm(c.harm)) }
            td class="num" { (interval(c.unit, c.interval)) }
            td {
                span class={ "chip status-" (status) } { (status) }
                @if let Some(reason) = &c.reason { ": " (reason) }
            }
        }
    }
}

/// One side's cell: the value in its unit and ` (n=<n>)`.
fn side(unit: Unit, v: &SideValue) -> String {
    format!("{} (n={})", unit.format_value(v.value), v.n)
}

/// `[<low>, <high>]` as harms, or `—` without an interval.
fn interval(unit: Unit, i: Option<(f64, f64)>) -> String {
    match i {
        Some((lo, hi)) => format!(
            "[{}, {}]",
            unit.format_harm(Some(lo)),
            unit.format_harm(Some(hi))
        ),
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use gadk_eval_monitor::{Action, GuardStats};

    use super::render;
    use crate::web::testutil::guard;

    const NAME: &str = "g-20270115T080000Z.json";

    #[test]
    fn the_header_shows_the_spec_rollout_window_source_and_time() {
        let out = render(
            NAME,
            &guard("g", 1_800_000_000, "support", 50, Action::Abort, true),
        )
        .into_string();
        assert!(
            out.contains("<title>Guard g-20270115T080000Z.json · gadk console</title>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<a class="active" href="/guards">Guards</a>"#),
            "{out}"
        );
        assert!(out.contains("<h1>guard g</h1>"), "{out}");
        assert!(
            out.contains("<dt>Rollout</dt><dd>support/production v1 → v2 (50%)</dd>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<dt>Window</dt><dd class="mono">2027-01-15T07:00:00Z – 2027-01-15T08:00:00Z</dd>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<dt>Source</dt><dd class="mono">bigquery:p.d.t</dd>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<dt>Generated</dt><dd class="mono">2027-01-15T08:00:00Z</dd>"#),
            "{out}"
        );
    }

    #[test]
    fn checks_show_values_harm_interval_and_a_status_badge() {
        let out = render(
            NAME,
            &guard("g", 1_800_000_000, "support", 50, Action::Abort, true),
        )
        .into_string();
        assert!(
            out.contains("<th>Check</th><th>Baseline</th><th>Rollout</th><th>Harm</th><th>Interval</th><th>Status</th>"),
            "{out}"
        );
        assert!(
            out.contains(concat!(
                r#"<tr><td class="mono">error_rate</td><td class="num">0.00% (n=5)</td>"#,
                r#"<td class="num">100.00% (n=6)</td><td class="num">+100.00pt</td>"#,
                r#"<td class="num">[+50.00pt, +100.00pt]</td>"#,
                r#"<td><span class="chip status-worse">worse</span>: rollout errors</td></tr>"#
            )),
            "{out}"
        );
        assert!(
            out.contains(concat!(
                r#"<tr><td class="mono">latency_p95</td><td class="num">1200 ms (n=5)</td>"#,
                r#"<td class="num">— (n=0)</td><td class="num">—</td><td class="num">—</td>"#,
                r#"<td><span class="chip status-insufficient">insufficient</span>: too few calls</td></tr>"#
            )),
            "{out}"
        );
        assert!(out.contains("<p>verdict: abort</p>"), "{out}");
        assert!(!out.contains("note: "), "{out}");
        assert!(
            out.contains(r#"<p>action: aborted <span class="chip">applied</span></p>"#),
            "{out}"
        );
    }

    #[test]
    fn a_status_without_a_reason_and_a_note_and_a_dry_run() {
        let mut r = guard("g", 1_800_000_000, "support", 50, Action::Ramp(75), false);
        r.checks[0].reason = None;
        r.note = Some("step at 50% is 3h old (< min_step_age 1d)".to_owned());
        let out = render(NAME, &r).into_string();
        assert!(
            out.contains(r#"<td><span class="chip status-worse">worse</span></td></tr>"#),
            "{out}"
        );
        assert!(
            out.contains(
                "<p>verdict: advance</p><p>note: step at 50% is 3h old (&lt; min_step_age 1d)</p>"
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<p>action: would ramp to 75% <span class="chip">dry run</span></p>"#),
            "{out}"
        );
    }

    #[test]
    fn a_stats_row_shows_only_with_stats() {
        let mut r = guard("g", 1_800_000_000, "support", 50, Action::Abort, true);
        let out = render(NAME, &r).into_string();
        assert!(!out.contains("<dt>Stats</dt>"), "{out}");
        r.stats = Some(GuardStats::new(true, 0.95, 2, TimeDelta::days(30)));
        let out = render(NAME, &r).into_string();
        assert!(
            out.contains(concat!(
                r#"<dt>Window</dt><dd class="mono">2027-01-15T07:00:00Z – 2027-01-15T08:00:00Z</dd>"#,
                r#"<dt>Stats</dt><dd>sequential, 95% across 2 interval checks</dd>"#,
                r#"<dt>Source</dt>"#
            )),
            "{out}"
        );
    }

    #[test]
    fn report_text_is_escaped() {
        let mut r = guard("g", 1_800_000_000, "<i>p</i>", 50, Action::Abort, false);
        r.spec = "<b>x</b>".to_owned();
        r.label = "<u>l</u>".to_owned();
        r.source = "<s>src</s>".to_owned();
        r.checks[0].name = "<em>c</em>".to_owned();
        r.checks[0].reason = Some("<q>why</q>".to_owned());
        r.note = Some("<code>n</code>".to_owned());
        let out = render("<a>-20270115T080000Z.json", &r).into_string();
        for raw in [
            "<b>x", "<i>p", "<u>l", "<s>src", "<em>c", "<q>why", "<code>n", "<a>-",
        ] {
            assert!(!out.contains(raw), "{raw}: {out}");
        }
        for escaped in [
            "&lt;b&gt;x&lt;/b&gt;",
            "&lt;i&gt;p&lt;/i&gt;/&lt;u&gt;l&lt;/u&gt;",
            "&lt;s&gt;src&lt;/s&gt;",
            "&lt;em&gt;c&lt;/em&gt;",
            "&lt;q&gt;why&lt;/q&gt;",
            "&lt;code&gt;n&lt;/code&gt;",
            "&lt;a&gt;-20270115T080000Z.json",
        ] {
            assert!(out.contains(escaped), "{escaped}: {out}");
        }
    }
}
