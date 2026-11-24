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

//! The Evals list page: one card per monitor with its pass-rate trend, then the reports.

use std::sync::Arc;

use chrono::SecondsFormat;
use gadk_eval_monitor::MonitorReport;
use maud::{Markup, html};

use super::layout::{Nav, page};
use crate::evals::{SpecSummary, group, percent, report_href, spec_href};

/// The Evals list: `reports` are `(stored name, report)`, newest first; `spec` is the active
/// `?spec=` filter. One card per monitor, then a table of the reports.
pub(crate) fn render(spec: Option<&str>, reports: &[(String, Arc<MonitorReport>)]) -> Markup {
    let body = html! {
        h1 { "Evals" }
        @if let Some(spec) = spec {
            p class="filter" {
                "Monitor " span class="mono" { (spec) } " · " a href="/evals" { "all monitors" }
            }
        }
        @if reports.is_empty() {
            p class="empty" { "No reports yet" }
        } @else {
            div class="eval-cards" {
                @for s in group(reports) { (card(&s)) }
            }
            table {
                thead {
                    tr { th { "Monitor" } th { "Generated" } th { "Runs" } th { "Pass rate" } th { "Errors" } }
                }
                tbody {
                    @for (name, r) in reports {
                        tr {
                            td { a href=(spec_href(&r.spec)) { (r.spec) } }
                            td class="mono" { a href=(report_href(name)) { (stamp(r)) } }
                            td class="num" { (r.runs.len()) }
                            td class="num" { (percent(r.pass_rate())) }
                            td class="num" { (r.error_count()) }
                        }
                    }
                }
            }
        }
    };
    page("Evals", Nav::Evals, body)
}

fn card(s: &SpecSummary) -> Markup {
    html! {
        div class="eval-card" {
            h2 { a href=(spec_href(&s.spec)) { (s.spec) } }
            p class="rate" { (percent(s.latest.pass_rate())) }
            p class="meta" {
                a href=(report_href(&s.latest_name)) { (stamp(&s.latest)) }
                " · " (s.latest.runs.len()) " runs · " (s.latest.error_count()) " errors"
            }
            (trend(&s.rates))
        }
    }
}

fn stamp(r: &MonitorReport) -> String {
    r.generated_at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The pass-rate trend as inline SVG (oldest rate first; `None` is a gap): x = 4 + 112·i/(n−1)
/// (116 for a single rate), y = 30 − 28·rate; a `polyline` per run of two or more known rates,
/// a `circle` on each isolated known rate and on the newest known rate.
pub(crate) fn trend(rates: &[Option<f64>]) -> Markup {
    trend_labeled(rates, "pass rate trend")
}

/// [`trend`] with `label` as the SVG's `aria-label`.
pub(crate) fn trend_labeled(rates: &[Option<f64>], label: &str) -> Markup {
    let n = rates.len();
    let x = |i: usize| {
        if n <= 1 {
            116.0
        } else {
            4.0 + 112.0 * i as f64 / (n - 1) as f64
        }
    };
    let y = |r: f64| 30.0 - 28.0 * r;
    let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();
    for (i, rate) in rates.iter().enumerate() {
        match rate {
            Some(r) => current.push((x(i), y(*r))),
            None if !current.is_empty() => segments.push(std::mem::take(&mut current)),
            None => {}
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    let mut dots: Vec<(f64, f64)> = segments
        .iter()
        .filter(|s| s.len() == 1)
        .map(|s| s[0])
        .collect();
    let newest = rates
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, r)| r.map(|r| (x(i), y(r))));
    if let Some(p) = newest
        && !dots.contains(&p)
    {
        dots.push(p);
    }
    html! {
        svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label=(label) {
            @for s in segments.iter().filter(|s| s.len() > 1) {
                polyline points=(points(s)) fill="none" {}
            }
            @for (cx, cy) in &dots {
                circle cx=(format!("{cx:.1}")) cy=(format!("{cy:.1}")) r="2" {}
            }
        }
    }
}

/// `x,y` pairs (one decimal each) joined by single spaces, for a `polyline`'s `points`.
pub(crate) fn points(segment: &[(f64, f64)]) -> String {
    segment
        .iter()
        .map(|(x, y)| format!("{x:.1},{y:.1}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;

    use super::{render, trend, trend_labeled};
    use crate::web::testutil::report;

    fn named(
        spec: &str,
        secs: i64,
        runs: &[&[Outcome]],
        name: &str,
    ) -> (String, Arc<gadk_eval_monitor::MonitorReport>) {
        (name.to_owned(), Arc::new(report(spec, secs, runs)))
    }

    #[test]
    fn empty_list_says_so_under_the_evals_nav() {
        let out = render(None, &[]).into_string();
        assert!(out.contains("<title>Evals · gadk console</title>"), "{out}");
        assert!(
            out.contains(r#"<a class="active" href="/evals">Evals</a>"#),
            "{out}"
        );
        assert!(out.contains("No reports yet"), "{out}");
        assert!(!out.contains("<table"), "{out}");
    }

    #[test]
    fn cards_per_spec_then_a_table_newest_first() {
        let reports = vec![
            named(
                "a",
                1_800_007_200,
                &[&[Outcome::Pass], &[Outcome::Fail]],
                "a-20270115T100000Z.json",
            ),
            named(
                "b",
                1_800_003_600,
                &[&[Outcome::Error]],
                "b-20270115T090000Z.json",
            ),
            named(
                "a",
                1_800_000_000,
                &[&[Outcome::Pass]],
                "a-20270115T080000Z.json",
            ),
        ];
        let out = render(None, &reports).into_string();
        // One card per spec, newest spec first, with the newest report's numbers.
        let card_a = out
            .find(r#"<div class="eval-card"><h2><a href="/evals?spec=a">a</a></h2>"#)
            .expect(&out);
        let card_b = out
            .find(r#"<div class="eval-card"><h2><a href="/evals?spec=b">b</a></h2>"#)
            .expect(&out);
        assert!(card_a < card_b, "{out}");
        assert!(out.contains(r#"<p class="rate">50.0%</p>"#), "{out}");
        assert!(out.contains(r#"<p class="rate">0.0%</p>"#), "{out}");
        assert!(out.contains(r#"<a href="/evals/a-20270115T100000Z.json">2027-01-15T10:00:00Z</a> · 2 runs · 0 errors"#), "{out}");
        assert!(out.contains(" · 1 runs · 1 errors"), "{out}");
        // The table lists every report in the given order.
        let rows: Vec<usize> = [
            "a-20270115T100000Z.json",
            "b-20270115T090000Z.json",
            "a-20270115T080000Z.json",
        ]
        .iter()
        .map(|n| {
            out.rfind(&format!(r#"<td class="mono"><a href="/evals/{n}">"#))
                .expect(n)
        })
        .collect();
        assert!(rows[0] < rows[1] && rows[1] < rows[2], "{out}");
        assert!(
            out.contains(
                r#"<td class="num">2</td><td class="num">50.0%</td><td class="num">0</td>"#
            ),
            "{out}"
        );
        assert!(!out.contains(r#"class="filter""#), "{out}");
    }

    #[test]
    fn a_spec_filter_shows_a_way_back() {
        let reports = vec![named(
            "a",
            1_800_000_000,
            &[&[Outcome::Pass]],
            "a-20270115T080000Z.json",
        )];
        let out = render(Some("a"), &reports).into_string();
        assert!(out.contains(r#"<p class="filter">Monitor <span class="mono">a</span> · <a href="/evals">all monitors</a></p>"#), "{out}");
    }

    #[test]
    fn report_text_is_escaped() {
        let reports = vec![named(
            "<b>x</b>",
            1_800_000_000,
            &[],
            "x-20270115T080000Z.json",
        )];
        let out = render(Some("<i>"), &reports).into_string();
        assert!(!out.contains("<b>x</b>") && !out.contains("<i>"), "{out}");
        assert!(out.contains("&lt;b&gt;x&lt;/b&gt;"), "{out}");
        assert!(out.contains("/evals?spec=%3Cb%3Ex%3C%2Fb%3E"), "{out}");
    }

    #[test]
    fn trend_draws_lines_gaps_and_dots() {
        let out = trend(&[Some(1.0), None, Some(0.5), Some(0.0)]).into_string();
        assert!(out.starts_with(r#"<svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="pass rate trend">"#), "{out}");
        assert_eq!(out.matches("<polyline").count(), 1, "{out}");
        assert!(
            out.contains(r#"<polyline points="78.7,16.0 116.0,30.0" fill="none"></polyline>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<circle cx="4.0" cy="2.0" r="2"></circle>"#),
            "isolated point: {out}"
        );
        assert!(
            out.contains(r#"<circle cx="116.0" cy="30.0" r="2"></circle>"#),
            "newest point: {out}"
        );
        assert_eq!(out.matches("<circle").count(), 2, "{out}");

        let one = trend(&[Some(0.5)]).into_string();
        assert!(!one.contains("<polyline"), "{one}");
        assert_eq!(one.matches("<circle").count(), 1, "{one}");
        assert!(
            one.contains(r#"<circle cx="116.0" cy="16.0" r="2"></circle>"#),
            "{one}"
        );

        let none = trend(&[None, None]).into_string();
        assert!(
            !none.contains("<polyline") && !none.contains("<circle"),
            "{none}"
        );
    }

    #[test]
    fn trend_labeled_names_the_svg() {
        let rates = [Some(1.0), None, Some(0.5), Some(0.0)];
        let out = trend_labeled(&rates, "usage trend").into_string();
        assert!(out.starts_with(r#"<svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="usage trend">"#), "{out}");
        assert_eq!(
            trend(&rates).into_string(),
            trend_labeled(&rates, "pass rate trend").into_string()
        );
    }
}
