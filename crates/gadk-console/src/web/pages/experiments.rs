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

//! The Experiments list page: one row per stored experiment with each variant's pass rate.

use std::sync::Arc;

use chrono::SecondsFormat;
use gadk_eval_monitor::ExperimentReport;
use maud::{Markup, html};

use super::layout::{Nav, page};
use crate::evals::percent;
use crate::experiments::experiment_href;

/// The Experiments list: `experiments` are `(stored name, experiment)`, newest first. Each row
/// links the experiment name to its page and shows the eval set, when it was generated and each
/// variant's name and per-run pass rate.
pub(crate) fn render(experiments: &[(String, Arc<ExperimentReport>)]) -> Markup {
    let body = html! {
        h1 { "Experiments" }
        @if experiments.is_empty() {
            p class="empty" { "No experiments yet" }
        } @else {
            table {
                thead {
                    tr { th { "Experiment" } th { "Set" } th { "Generated" } th { "Variants (pass rate)" } }
                }
                tbody {
                    @for (name, x) in experiments {
                        tr {
                            td class="mono" { a href=(experiment_href(name)) { (x.name) } }
                            td { (x.set) }
                            td class="mono" {
                                (x.generated_at.to_rfc3339_opts(SecondsFormat::Secs, true))
                            }
                            td {
                                @for s in x.variant_stats() {
                                    span class="variant-rate" {
                                        span class="mono" { (s.name) } " " (percent(s.pass_rate()))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    page("Experiments", Nav::Experiments, body)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;

    use super::render;
    use crate::web::testutil::experiment;

    #[test]
    fn empty_list_says_so_under_the_experiments_nav() {
        let out = render(&[]).into_string();
        assert!(
            out.contains("<title>Experiments · gadk console</title>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<a class="active" href="/experiments">Experiments</a>"#),
            "{out}"
        );
        assert!(out.contains("No experiments yet"), "{out}");
        assert!(!out.contains("<table"), "{out}");
    }

    #[test]
    fn rows_link_the_stored_name_and_show_each_variant_rate() {
        let newer = experiment(
            "exp",
            1_800_003_600,
            &[
                ("v1", &[("c1", &[Outcome::Pass]), ("c2", &[Outcome::Pass])]),
                ("v2", &[("c1", &[Outcome::Pass]), ("c2", &[Outcome::Fail])]),
            ],
        );
        let older = experiment(
            "exp",
            1_800_000_000,
            &[("v1", &[("c1", &[Outcome::Error])])],
        );
        let rows = vec![
            ("exp-20270115T090000Z.json".to_owned(), Arc::new(newer)),
            ("exp-20270115T080000Z.json".to_owned(), Arc::new(older)),
        ];
        let out = render(&rows).into_string();
        let first = out
            .find(r#"<td class="mono"><a href="/experiments/exp-20270115T090000Z.json">exp</a></td><td>set</td><td class="mono">2027-01-15T09:00:00Z</td>"#)
            .expect(&out);
        let second = out
            .find(r#"<td class="mono"><a href="/experiments/exp-20270115T080000Z.json">exp</a></td><td>set</td><td class="mono">2027-01-15T08:00:00Z</td>"#)
            .expect(&out);
        assert!(first < second, "{out}");
        assert!(
            out.contains(r#"<td><span class="variant-rate"><span class="mono">v1</span> 100.0%</span><span class="variant-rate"><span class="mono">v2</span> 50.0%</span></td>"#),
            "{out}"
        );
        // A variant whose runs all errored has no pass rate.
        assert!(
            out.contains(r#"<span class="variant-rate"><span class="mono">v1</span> —</span>"#),
            "{out}"
        );
    }

    #[test]
    fn experiment_text_is_escaped() {
        let mut x = experiment("exp", 1_800_000_000, &[("<b>v</b>", &[])]);
        x.set = "<i>set</i>".to_owned();
        let out = render(&[("exp-20270115T080000Z.json".to_owned(), Arc::new(x))]).into_string();
        assert!(
            !out.contains("<b>v</b>") && !out.contains("<i>set</i>"),
            "{out}"
        );
        assert!(out.contains("&lt;b&gt;v&lt;/b&gt;"), "{out}");
        assert!(out.contains("&lt;i&gt;set&lt;/i&gt;"), "{out}");
    }
}
