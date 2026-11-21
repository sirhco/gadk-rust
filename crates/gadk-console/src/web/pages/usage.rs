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

//! The usage page: model calls, tokens and cost over a time range.

use std::collections::BTreeMap;

use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE, token_cells};
use super::layout::{Nav, page};
use crate::prices::{CostSum, PriceTable, Usage, cost_of, thousands};
use crate::traces::{TimeRange, TraceId};
use crate::usage::{BREAKDOWN_CAP, Breakdown, ModelCalls, TopTrace};

/// The note shown when the trace source is not BigQuery.
pub(crate) const NEEDS_BIGQUERY: &str = "Usage needs the BigQuery trace source: start the console with --traces bigquery://<project>.<dataset>.<table>.";

/// Calls, tokens and cost summed over groups of model calls.
#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    calls: u64,
    usage: Usage,
    cost: CostSum,
}

impl Totals {
    fn add(&mut self, m: &ModelCalls, prices: Option<&PriceTable>) {
        self.calls += m.calls;
        self.usage += m.usage;
        self.cost.add(cost_of(prices, &m.model, &m.usage));
    }
}

/// The range picker.
fn range_form(range: TimeRange) -> Markup {
    html! {
        form class="filters" method="get" action="/usage" {
            label {
                "Range"
                select name="range" {
                    @for r in TimeRange::ALL {
                        option value=(r.as_str()) selected[r == range] { (r.as_str()) }
                    }
                }
            }
            button type="submit" { "Show" }
        }
    }
}

/// One table of totals, the first column headed `first`.
fn totals_table(first: &str, rows: impl Iterator<Item = (String, Totals)>) -> Markup {
    html! {
        table {
            thead {
                tr {
                    th { (first) } th { "LLM calls" } th { "Input" } th { "Cached" }
                    th { "Output" } th { "Reasoning" } th { "Cost" }
                }
            }
            tbody {
                @for (label, t) in rows {
                    tr {
                        td { (label) }
                        td class="num" { (thousands(t.calls)) }
                        (token_cells(&t.usage))
                        td class="num" { (t.cost.text()) }
                    }
                }
            }
        }
    }
}

/// `/usage` with the Cloud Trace source: the range picker and a note; nothing is queried.
pub(crate) fn render_unavailable(range: TimeRange) -> Markup {
    let body = html! {
        h1 { "Usage" }
        (range_form(range))
        div class="banner info" { p { (NEEDS_BIGQUERY) } }
    };
    page("Usage", Nav::Usage, body)
}

/// `/usage`: totals, then calls, tokens and cost by model, by agent and by prompt version, then
/// the traces with the most tokens. Costs are computed per model, so every group sums its
/// models' costs.
pub(crate) fn render(
    range: TimeRange,
    b: &Breakdown,
    top: &[TopTrace],
    prices: Option<&PriceTable>,
) -> Markup {
    let mut all = Totals::default();
    let mut by_model: BTreeMap<String, Totals> = BTreeMap::new();
    let mut by_agent: BTreeMap<String, Totals> = BTreeMap::new();
    let mut by_prompt: BTreeMap<Option<String>, Totals> = BTreeMap::new();
    for c in &b.cells {
        all.add(&c.calls, prices);
        by_model
            .entry(c.calls.model.clone())
            .or_default()
            .add(&c.calls, prices);
        by_agent
            .entry(c.agent.clone())
            .or_default()
            .add(&c.calls, prices);
        by_prompt
            .entry(c.prompt.clone())
            .or_default()
            .add(&c.calls, prices);
    }
    let traces: Vec<(&TopTrace, Totals)> = top
        .iter()
        .map(|t| {
            let mut totals = Totals::default();
            for m in &t.models {
                totals.add(m, prices);
            }
            (t, totals)
        })
        .collect();
    let partial = all.cost.partial() || traces.iter().any(|(_, t)| t.cost.partial());
    let body = html! {
        h1 { "Usage" }
        (range_form(range))
        @if prices.is_none() {
            p class="empty" { (NO_PRICES) }
        }
        @if b.cells.is_empty() {
            p class="empty" { "No model calls in the last " (range.as_str()) "." }
        } @else {
            h2 { "Totals" }
            (totals_table("Calls", std::iter::once(("All calls".to_owned(), all))))
            h2 { "By model" }
            (totals_table("Model", by_model.iter().map(|(k, t)| (k.clone(), *t))))
            h2 { "By agent" }
            (totals_table("Agent", by_agent.iter().map(|(k, t)| {
                (if k.is_empty() { "(unknown)".to_owned() } else { k.clone() }, *t)
            })))
            h2 { "By prompt" }
            (totals_table("Prompt", by_prompt.iter().map(|(k, t)| {
                (k.clone().unwrap_or_else(|| "(none)".to_owned()), *t)
            })))
            h2 { "Top traces" }
            table {
                thead {
                    tr { th { "Trace" } th { "Total tokens" } th { "LLM calls" } th { "Cost" } }
                }
                tbody {
                    @for (t, totals) in &traces {
                        tr {
                            td class="mono" {
                                @match TraceId::parse(&t.trace_id) {
                                    Some(id) => {
                                        a href={ "/traces/" (id.as_str()) } { (id.as_str()) }
                                    }
                                    None => { (t.trace_id) }
                                }
                            }
                            td class="num" { (thousands(t.total)) }
                            td class="num" { (thousands(totals.calls)) }
                            td class="num" { (totals.cost.text()) }
                        }
                    }
                }
            }
            @if b.truncated {
                p class="empty" {
                    "Showing the first " (thousands(BREAKDOWN_CAP as u64))
                    " model × agent × prompt groups; the totals cover only those."
                }
            }
        }
        @if partial {
            p class="empty" { (PARTIAL_NOTE) }
        }
    };
    page("Usage", Nav::Usage, body)
}
