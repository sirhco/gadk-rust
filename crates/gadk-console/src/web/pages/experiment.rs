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

//! The page for one experiment: the variants side by side, then the cases they disagree on.

use chrono::SecondsFormat;
use gadk_analytics::cost::CostSum;
use gadk_eval::{CaseRun, Outcome};
use gadk_eval_monitor::{CaseVerdicts, ExperimentReport, VariantStats, Verdict};
use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE};
use super::eval_report::outcome_chip;
use super::layout::{Nav, page};
use crate::evals::percent;
use crate::experiments::experiment_href;
use crate::prices::{PriceTable, runs_cost, thousands};

/// The experiment stored as `name`: its header, a comparison table with one column per variant
/// (the first is the baseline; a Model row when any variant records a model), then the cases
/// where the variants disagree (every case when `all`), each verdict opening the variant's runs
/// of that case.
pub(crate) fn render(
    name: &str,
    x: &ExperimentReport,
    all: bool,
    prices: Option<&PriceTable>,
) -> Markup {
    let stats = x.variant_stats();
    let costs: Vec<CostSum> = (0..stats.len())
        .map(|v| runs_cost(prices, x.metrics.get(v).map_or(&[][..], Vec::as_slice)))
        .collect();
    let base = stats.first().and_then(VariantStats::pass_rate);
    let cases: Vec<CaseVerdicts> = x
        .case_verdicts()
        .into_iter()
        .filter(|c| all || c.disagree())
        .collect();
    let href = experiment_href(name);
    let body = html! {
        h1 { "Experiment " span class="mono" { (name) } }
        dl class="eval-meta" {
            dt { "Experiment" } dd { (x.name) }
            dt { "Set" } dd { (x.set) }
            dt { "Generated" } dd class="mono" {
                (x.generated_at.to_rfc3339_opts(SecondsFormat::Secs, true))
            }
        }
        h2 { "Variants" }
        table class="exp-compare" {
            thead {
                tr {
                    th {}
                    @for (i, s) in stats.iter().enumerate() {
                        th { (s.name) @if i == 0 { " (baseline)" } }
                    }
                }
            }
            tbody {
                tr {
                    th { "Prompt" }
                    @for i in 0..stats.len() {
                        td class="mono" {
                            (x.variants.get(i).and_then(|v| v.prompt.as_deref()).unwrap_or("—"))
                        }
                    }
                }
                @if x.variants.iter().any(|v| v.model.is_some()) {
                    tr {
                        th { "Model" }
                        @for i in 0..stats.len() {
                            td class="mono" {
                                (x.variants.get(i).and_then(|v| v.model.as_deref()).unwrap_or("—"))
                            }
                        }
                    }
                }
                (row("Runs", stats.iter().map(|s| s.runs.to_string())))
                (row("Pass rate", stats.iter().map(|s| percent(s.pass_rate()))))
                (row("vs baseline", stats.iter().enumerate().map(|(i, s)| {
                    if i == 0 { "—".to_owned() } else { delta(s.pass_rate(), base) }
                })))
                (row("Mean score", stats.iter().map(|s| {
                    s.mean_score.map_or_else(|| "—".to_owned(), |m| format!("{m:.2}"))
                })))
                (row("Errors", stats.iter().map(|s| s.errors.to_string())))
                (row("p50", stats.iter().map(|s| ms(s.p50_ms))))
                (row("p95", stats.iter().map(|s| ms(s.p95_ms))))
                (row("Input tokens", stats.iter().map(|s| thousands(s.tokens.input))))
                (row("Output tokens", stats.iter().map(|s| thousands(s.tokens.output))))
                (row("Cached tokens", stats.iter().map(|s| thousands(s.tokens.cached))))
                (row("Reasoning tokens", stats.iter().map(|s| thousands(s.tokens.reasoning))))
                (row("Cost", costs.iter().map(CostSum::text)))
            }
        }
        @if prices.is_none() {
            p class="empty" { (NO_PRICES) }
        }
        @if costs.iter().any(CostSum::partial) {
            p class="empty" { (PARTIAL_NOTE) }
        }
        h2 { "Cases" }
        @if all {
            p class="filter" { "All cases · " a href=(href) { "only disagreements" } }
        } @else {
            p class="filter" {
                "Cases where the variants disagree · "
                a href={ (href) "?cases=all" } { "all cases" }
            }
        }
        @if cases.is_empty() {
            p class="empty" {
                @if all { "No cases" } @else { "The variants agree on every case" }
            }
        } @else {
            table class="exp-cases" {
                thead {
                    tr {
                        th { "Case" }
                        @for s in &stats { th { (s.name) } }
                    }
                }
                tbody {
                    @for c in &cases {
                        tr {
                            td class="mono" { (c.case) }
                            @for (v, verdict) in c.verdicts.iter().enumerate() {
                                td { (verdict_cell(x, v, &c.case, *verdict)) }
                            }
                        }
                    }
                }
            }
        }
    };
    page(&format!("Experiment {name}"), Nav::Experiments, body)
}

/// One comparison row: the label, then one `td class="num"` per variant.
fn row(label: &str, cells: impl Iterator<Item = String>) -> Markup {
    html! {
        tr {
            th { (label) }
            @for c in cells { td class="num" { (c) } }
        }
    }
}

/// The difference between a pass rate and the baseline's in percentage points, one decimal:
/// `+5.0 pp`, `−12.5 pp` (U+2212 minus), `0.0 pp`; `—` when either rate is missing.
pub(crate) fn delta(rate: Option<f64>, base: Option<f64>) -> String {
    let (Some(rate), Some(base)) = (rate, base) else {
        return "—".to_owned();
    };
    let d = (rate - base) * 100.0;
    let text = format!("{:.1}", d.abs());
    if text == "0.0" {
        "0.0 pp".to_owned()
    } else if d > 0.0 {
        format!("+{text} pp")
    } else {
        format!("\u{2212}{text} pp")
    }
}

/// `1,250 ms`, or `—` when there is no duration.
fn ms(v: Option<u64>) -> String {
    v.map_or_else(|| "—".to_owned(), |n| format!("{} ms", thousands(n)))
}

/// A verdict chip that opens variant `v`'s runs of `case`: each run's last reply, run error and
/// score rationales.
fn verdict_cell(x: &ExperimentReport, v: usize, case: &str, verdict: Verdict) -> Markup {
    let (class, label) = outcome_chip(match verdict {
        Verdict::Pass => Outcome::Pass,
        Verdict::Fail => Outcome::Fail,
        _ => Outcome::Error,
    });
    let runs: Vec<&CaseRun> = x
        .report
        .variants
        .get(v)
        .map(|r| r.runs.iter().filter(|r| r.case == case).collect())
        .unwrap_or_default();
    html! {
        details class="exp-verdict" {
            summary { span class=(class) { (label) } }
            div class="eval-run-body" {
                @for r in runs {
                    p class="turn-head" { "Run " (r.run) }
                    @if let Some(err) = &r.transcript.error {
                        div class="banner error" { p { "Run error: " (err) } }
                    }
                    p class="who" { "Last reply" }
                    pre { (r.transcript.reply(None)) }
                    @if !r.scores.is_empty() {
                        ul class="rationales" {
                            @for s in &r.scores {
                                li {
                                    span class="mono" { (s.kind) } " "
                                    (outcome(s.outcome))
                                    @if let Some(n) = s.score { " (" (n) ")" }
                                    ": " (s.rationale)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn outcome(o: Outcome) -> &'static str {
    outcome_chip(o).1
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gadk_eval::{Outcome, RunMetrics, TokenCounts};
    use gadk_eval_monitor::ExperimentReport;

    use super::{delta, render};
    use crate::prices::PriceTable;
    use crate::web::pages::cost::PARTIAL_NOTE;
    use crate::web::testutil::{EXP_MODEL, experiment};

    const NAME: &str = "exp-20270115T080000Z.json";
    /// `NO_PRICES` as the page shows it (HTML-escaped).
    const NO_PRICES: &str =
        "No price file: start the console with --prices &lt;file&gt; to see costs.";
    const PRICES: &str =
        "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n";

    /// `v1` and `v3` pass `c1` and `c2`; `v2` passes `c1` and fails `c2`.
    fn three() -> ExperimentReport {
        experiment(
            "exp",
            1_800_000_000,
            &[
                ("v1", &[("c1", &[Outcome::Pass]), ("c2", &[Outcome::Pass])]),
                ("v2", &[("c1", &[Outcome::Pass]), ("c2", &[Outcome::Fail])]),
                ("v3", &[("c1", &[Outcome::Pass]), ("c2", &[Outcome::Pass])]),
            ],
        )
    }

    fn prices() -> PriceTable {
        PriceTable::from_yaml(PRICES).unwrap()
    }

    #[test]
    fn deltas_are_percentage_points_against_the_baseline() {
        assert_eq!(delta(Some(0.55), Some(0.5)), "+5.0 pp");
        assert_eq!(delta(Some(0.375), Some(0.5)), "\u{2212}12.5 pp");
        assert_eq!(delta(Some(0.5), Some(0.5)), "0.0 pp");
        assert_eq!(delta(Some(0.5), Some(0.50001)), "0.0 pp");
        assert_eq!(delta(None, Some(0.5)), "—");
        assert_eq!(delta(Some(0.5), None), "—");
    }

    #[test]
    fn the_comparison_table_has_a_column_per_variant_baseline_first() {
        let out = render(NAME, &three(), false, Some(&prices())).into_string();
        assert!(
            out.contains("<title>Experiment exp-20270115T080000Z.json · gadk console</title>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<a class="active" href="/experiments">Experiments</a>"#),
            "{out}"
        );
        for want in [
            r#"<dt>Experiment</dt><dd>exp</dd><dt>Set</dt><dd>set</dd><dt>Generated</dt><dd class="mono">2027-01-15T08:00:00Z</dd>"#,
            "<thead><tr><th></th><th>v1 (baseline)</th><th>v2</th><th>v3</th></tr></thead>",
            r#"<tr><th>Prompt</th><td class="mono">—</td><td class="mono">—</td><td class="mono">—</td></tr>"#,
            r#"<tr><th>Runs</th><td class="num">2</td><td class="num">2</td><td class="num">2</td></tr>"#,
            r#"<tr><th>Pass rate</th><td class="num">100.0%</td><td class="num">50.0%</td><td class="num">100.0%</td></tr>"#,
            "<tr><th>vs baseline</th><td class=\"num\">—</td><td class=\"num\">\u{2212}50.0 pp</td><td class=\"num\">0.0 pp</td></tr>",
            r#"<tr><th>Mean score</th><td class="num">—</td><td class="num">—</td><td class="num">—</td></tr>"#,
            r#"<tr><th>Errors</th><td class="num">0</td><td class="num">0</td><td class="num">0</td></tr>"#,
            r#"<tr><th>p50</th><td class="num">100 ms</td><td class="num">100 ms</td><td class="num">100 ms</td></tr>"#,
            r#"<tr><th>p95</th><td class="num">200 ms</td><td class="num">200 ms</td><td class="num">200 ms</td></tr>"#,
            r#"<tr><th>Input tokens</th><td class="num">2,000</td><td class="num">2,000</td><td class="num">2,000</td></tr>"#,
            r#"<tr><th>Output tokens</th><td class="num">600</td><td class="num">600</td><td class="num">600</td></tr>"#,
            r#"<tr><th>Cached tokens</th><td class="num">400</td><td class="num">400</td><td class="num">400</td></tr>"#,
            r#"<tr><th>Reasoning tokens</th><td class="num">100</td><td class="num">100</td><td class="num">100</td></tr>"#,
            // Per run: 800 × 0.30 + 200 × 0.075 + 300 × 2.50 + 50 × 2.50 = 1,130 per million.
            r#"<tr><th>Cost</th><td class="num">$0.002260</td><td class="num">$0.002260</td><td class="num">$0.002260</td></tr>"#,
        ] {
            assert!(out.contains(want), "{want}\n{out}");
        }
        assert!(!out.contains(NO_PRICES), "{out}");
        assert!(!out.contains(PARTIAL_NOTE), "{out}");
    }

    #[test]
    fn a_model_row_shows_when_any_variant_has_a_model() {
        let out = render(NAME, &three(), false, None).into_string();
        assert!(!out.contains("<th>Model</th>"), "{out}");

        let mut x = three();
        x.variants[0].model = Some("gemini-3.8-flash".to_owned());
        x.variants[2].model = Some("gemini-4-hydrogen".to_owned());
        let out = render(NAME, &x, false, None).into_string();
        assert!(
            out.contains(concat!(
                r#"<tr><th>Prompt</th><td class="mono">—</td><td class="mono">—</td><td class="mono">—</td></tr>"#,
                r#"<tr><th>Model</th><td class="mono">gemini-3.8-flash</td><td class="mono">—</td><td class="mono">gemini-4-hydrogen</td></tr>"#,
                r#"<tr><th>Runs</th>"#,
            )),
            "{out}"
        );

        x.variants[1].model = Some("<i>m</i>".to_owned());
        let out = render(NAME, &x, false, None).into_string();
        assert!(!out.contains("<i>m</i>"), "{out}");
        assert!(
            out.contains(r#"<td class="mono">&lt;i&gt;m&lt;/i&gt;</td>"#),
            "{out}"
        );
    }

    #[test]
    fn prompts_scores_errors_and_durations_come_from_the_runs() {
        let mut x = three();
        x.variants[1].prompt = Some("v2".to_owned());
        for (r, n) in x.report.variants[0].runs.iter_mut().zip([4, 5]) {
            r.scores[0].kind = "llm_judge".to_owned();
            r.scores[0].score = Some(n);
        }
        x.report.variants[1].runs[1].transcript.error = Some("boom".to_owned());
        x.report.variants[1].runs[1].scores[0].outcome = Outcome::Error;
        x.metrics[2][1].duration_ms = 1_250;
        let out = render(NAME, &x, false, None).into_string();
        for want in [
            r#"<tr><th>Prompt</th><td class="mono">—</td><td class="mono">v2</td><td class="mono">—</td></tr>"#,
            // v2: one pass, one error → 100% of graded runs.
            r#"<tr><th>Pass rate</th><td class="num">100.0%</td><td class="num">100.0%</td><td class="num">100.0%</td></tr>"#,
            r#"<tr><th>Mean score</th><td class="num">4.50</td><td class="num">—</td><td class="num">—</td></tr>"#,
            r#"<tr><th>Errors</th><td class="num">0</td><td class="num">1</td><td class="num">0</td></tr>"#,
            // v2's errored run is left out of its percentiles.
            r#"<tr><th>p50</th><td class="num">100 ms</td><td class="num">100 ms</td><td class="num">100 ms</td></tr>"#,
            r#"<tr><th>p95</th><td class="num">200 ms</td><td class="num">100 ms</td><td class="num">1,250 ms</td></tr>"#,
        ] {
            assert!(out.contains(want), "{want}\n{out}");
        }
    }

    #[test]
    fn costs_need_prices_and_mark_partial_sums() {
        let out = render(NAME, &three(), false, None).into_string();
        assert!(
            out.contains(r#"<tr><th>Cost</th><td class="num">—</td><td class="num">—</td><td class="num">—</td></tr>"#),
            "{out}"
        );
        assert!(out.contains(NO_PRICES), "{out}");
        assert!(!out.contains(PARTIAL_NOTE), "{out}");

        let mut x = three();
        // v2's second run also called an unpriced model; v3 called only unpriced models.
        x.metrics[1][1]
            .models
            .insert("other-model".to_owned(), TokenCounts::new(10, 0, 10, 0));
        for m in &mut x.metrics[2] {
            *m = RunMetrics::new(
                m.duration_ms,
                BTreeMap::from([("other-model".to_owned(), TokenCounts::new(10, 0, 10, 0))]),
            );
        }
        let out = render(NAME, &x, false, Some(&prices())).into_string();
        assert!(
            out.contains(r#"<tr><th>Cost</th><td class="num">$0.002260</td><td class="num">$0.002260*</td><td class="num">—</td></tr>"#),
            "{out}"
        );
        assert!(out.contains(PARTIAL_NOTE), "{out}");
        assert!(!out.contains(NO_PRICES), "{out}");
        assert!(x.metrics[0][0].models.contains_key(EXP_MODEL));
    }

    #[test]
    fn only_disagreements_by_default_and_every_case_on_request() {
        let x = three();
        let out = render(NAME, &x, false, None).into_string();
        assert!(
            out.contains(r#"<p class="filter">Cases where the variants disagree · <a href="/experiments/exp-20270115T080000Z.json?cases=all">all cases</a></p>"#),
            "{out}"
        );
        assert!(
            out.contains("<thead><tr><th>Case</th><th>v1</th><th>v2</th><th>v3</th></tr></thead>"),
            "{out}"
        );
        assert!(out.contains(r#"<td class="mono">c2</td>"#), "{out}");
        assert!(!out.contains(r#"<td class="mono">c1</td>"#), "{out}");
        assert!(
            out.contains(r#"<summary><span class="chip chip-fail">fail</span></summary>"#),
            "{out}"
        );

        let out = render(NAME, &x, true, None).into_string();
        assert!(
            out.contains(r#"<p class="filter">All cases · <a href="/experiments/exp-20270115T080000Z.json">only disagreements</a></p>"#),
            "{out}"
        );
        let c1 = out.find(r#"<td class="mono">c1</td>"#).expect(&out);
        let c2 = out.find(r#"<td class="mono">c2</td>"#).expect(&out);
        assert!(c1 < c2, "set order: {out}");
    }

    #[test]
    fn when_the_variants_agree_the_default_view_says_so() {
        let x = experiment(
            "exp",
            1_800_000_000,
            &[
                ("v1", &[("c1", &[Outcome::Pass])]),
                ("v2", &[("c1", &[Outcome::Pass])]),
            ],
        );
        let out = render(NAME, &x, false, None).into_string();
        assert!(out.contains("The variants agree on every case"), "{out}");
        assert!(!out.contains("exp-cases"), "{out}");
        let out = render(NAME, &x, true, None).into_string();
        assert!(out.contains(r#"<td class="mono">c1</td>"#), "{out}");
    }

    #[test]
    fn each_verdict_opens_the_runs_with_replies_errors_and_rationales() {
        let mut x = three();
        let run = &mut x.report.variants[1].runs[1];
        run.transcript.error = Some("agent timed out".to_owned());
        run.scores[0].outcome = Outcome::Error;
        run.scores[0].rationale = "no reply to check".to_owned();
        let judge = &mut x.report.variants[0].runs[1].scores[0];
        judge.kind = "llm_judge".to_owned();
        judge.score = Some(5);
        judge.rationale = "spot on".to_owned();
        let out = render(NAME, &x, false, None).into_string();
        assert!(
            out.contains(r#"<details class="exp-verdict"><summary><span class="chip chip-pass">pass</span></summary><div class="eval-run-body"><p class="turn-head">Run 1</p><p class="who">Last reply</p><pre>v1 reply to c2</pre><ul class="rationales"><li><span class="mono">llm_judge</span> pass (5): spot on</li></ul></div></details>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<summary><span class="chip chip-error">error</span></summary><div class="eval-run-body"><p class="turn-head">Run 1</p><div class="banner error"><p>Run error: agent timed out</p></div><p class="who">Last reply</p><pre>v2 reply to c2</pre><ul class="rationales"><li><span class="mono">contains</span> error: no reply to check</li></ul></div>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<summary><span class="chip chip-pass">pass</span></summary><div class="eval-run-body"><p class="turn-head">Run 1</p><p class="who">Last reply</p><pre>v3 reply to c2</pre><ul class="rationales"><li><span class="mono">contains</span> pass: r0</li></ul></div>"#),
            "{out}"
        );
    }

    #[test]
    fn experiment_text_is_escaped() {
        let mut x = three();
        x.name = "<i>x</i>".to_owned();
        x.report.variants[0].runs[1].transcript.turns[0].reply =
            "<script>alert(1)</script>".to_owned();
        x.report.variants[1].runs[1].scores[0].rationale = "<b>no</b>".to_owned();
        let out = render(NAME, &x, false, None).into_string();
        for raw in ["<i>x</i>", "<script>alert", "<b>no</b>"] {
            assert!(!out.contains(raw), "{raw}\n{out}");
        }
        assert!(
            out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{out}"
        );
    }
}
