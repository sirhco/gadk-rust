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

use chrono::{DateTime, TimeDelta, Utc};
use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE, token_cells};
use super::evals::{points, trend_labeled};
use super::layout::{Nav, page};
use crate::prices::{CostSum, PriceTable, Usage, cost_of, cost_text, thousands};
use crate::traces::TraceId;
use crate::usage::{BREAKDOWN_CAP, Breakdown, ModelCalls, TopTrace, Trend, TrendCell, UsageRange};

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

/// The note shown in place of the charts when the trend query hit its cap.
const TOO_MANY_GROUPS: &str = "Too many groups to chart for this range.";
/// The note under the chart when a price file is loaded and some calls have no price.
const UNPRICED_CAPTION: &str = "Excludes calls to models without a price.";

/// The chart metric of a group of calls: their cost in USD with a price file (0 when the model
/// has no price), else their input + output + reasoning tokens.
fn metric(m: &ModelCalls, prices: Option<&PriceTable>) -> f64 {
    match prices {
        Some(_) => cost_of(prices, &m.model, &m.usage).unwrap_or(0.0),
        None => m
            .usage
            .input
            .saturating_add(m.usage.output)
            .saturating_add(m.usage.reasoning) as f64,
    }
}

/// The metric per bucket (`count` of them, an empty bucket 0) of each `key`'s calls; cells
/// outside `0..count` are ignored.
fn series<K: Ord>(
    cells: &[TrendCell],
    count: usize,
    prices: Option<&PriceTable>,
    key: impl Fn(&TrendCell) -> K,
) -> BTreeMap<K, Vec<f64>> {
    let mut out: BTreeMap<K, Vec<f64>> = BTreeMap::new();
    for c in cells.iter().filter(|c| c.bucket < count) {
        out.entry(key(c)).or_insert_with(|| vec![0.0; count])[c.bucket] +=
            metric(&c.cell.calls, prices);
    }
    out
}

/// The metric per bucket of all calls.
fn total(cells: &[TrendCell], count: usize, prices: Option<&PriceTable>) -> Vec<f64> {
    let mut out = vec![0.0; count];
    for c in cells.iter().filter(|c| c.bucket < count) {
        out[c.bucket] += metric(&c.cell.calls, prices);
    }
    out
}

/// The `Over time` chart of `values` (one per `bucket_seconds`-wide bucket from `start`, at
/// least two): a 600×120 line scaled to the largest value, labeled with the largest and the
/// latest value and the first and last buckets' start times (UTC).
fn chart(
    values: &[f64],
    bucket_label: &str,
    start: DateTime<Utc>,
    bucket_seconds: i64,
    cost: bool,
) -> Markup {
    debug_assert!(values.len() >= 2, "{} buckets", values.len());
    let n = values.len();
    let max = values.iter().copied().fold(0.0, f64::max);
    let latest = values.last().copied().unwrap_or(0.0);
    let xy: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = 8.0 + 584.0 * i as f64 / (n - 1) as f64;
            let y = if max > 0.0 {
                100.0 - 90.0 * v / max
            } else {
                100.0
            };
            (x, y)
        })
        .collect();
    let value = |v: f64| {
        if cost {
            cost_text(Some(v))
        } else {
            thousands(v.round() as u64)
        }
    };
    let format = if bucket_seconds >= 21_600 {
        "%m-%d"
    } else {
        "%H:%M"
    };
    let last = start + TimeDelta::seconds(bucket_seconds * (n as i64 - 1));
    let label = format!(
        "{} per {bucket_label}",
        if cost { "cost" } else { "tokens" }
    );
    html! {
        svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label=(label) {
            polyline points=(points(&xy)) fill="none" {}
            text x="8" y="12" text-anchor="start" { "max " (value(max)) }
            text x="592" y="12" text-anchor="end" { "latest " (value(latest)) }
            text x="8" y="116" text-anchor="start" { (start.format(format)) }
            text x="592" y="116" text-anchor="end" { (last.format(format)) }
        }
    }
}

/// A row's sparkline: `values` divided by their maximum (all 0 when it is 0).
fn spark(values: &[f64]) -> Markup {
    let max = values.iter().copied().fold(0.0, f64::max);
    let rates: Vec<Option<f64>> = values
        .iter()
        .map(|v| Some(if max > 0.0 { v / max } else { 0.0 }))
        .collect();
    trend_labeled(&rates, "usage trend")
}

/// The range picker.
fn range_form(range: UsageRange) -> Markup {
    html! {
        form class="filters" method="get" action="/usage" {
            label {
                "Range"
                select name="range" {
                    @for r in UsageRange::ALL {
                        option value=(r.as_str()) selected[r == range] { (r.as_str()) }
                    }
                }
            }
            button type="submit" { "Show" }
        }
    }
}

/// One table of totals, the first column headed `first`, each row with its sparkline (an empty
/// `Trend` cell when `None`).
fn totals_table(
    first: &str,
    rows: impl Iterator<Item = (String, Totals, Option<Markup>)>,
) -> Markup {
    html! {
        table {
            thead {
                tr {
                    th { (first) } th { "LLM calls" } th { "Input" } th { "Cached" }
                    th { "Output" } th { "Reasoning" } th { "Cost" } th { "Trend" }
                }
            }
            tbody {
                @for (label, t, trend) in rows {
                    tr {
                        td { (label) }
                        td class="num" { (thousands(t.calls)) }
                        (token_cells(&t.usage))
                        td class="num" { (t.cost.text()) }
                        td {
                            @if let Some(trend) = trend {
                                (trend)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `/usage` with the Cloud Trace source: the range picker and a note; nothing is queried.
pub(crate) fn render_unavailable(range: UsageRange) -> Markup {
    let body = html! {
        h1 { "Usage" }
        (range_form(range))
        div class="banner info" { p { (NEEDS_BIGQUERY) } }
    };
    page("Usage", Nav::Usage, body)
}

/// `/usage`: totals, the cost (or tokens) per bucket of the window from `start`, then calls,
/// tokens, cost and a sparkline by model, by agent and by prompt version, then the traces with
/// the most tokens. Costs are computed per model, so every group sums its models' costs.
pub(crate) fn render(
    range: UsageRange,
    b: &Breakdown,
    top: &[TopTrace],
    trend: &Trend,
    prices: Option<&PriceTable>,
    start: DateTime<Utc>,
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
    let (bucket_seconds, count, bucket_label) = range.bucket();
    let charted = !trend.truncated;
    let model_series = series(&trend.cells, count, prices, |c| c.cell.calls.model.clone());
    let agent_series = series(&trend.cells, count, prices, |c| c.cell.agent.clone());
    let prompt_series = series(&trend.cells, count, prices, |c| c.cell.prompt.clone());
    let all_series = total(&trend.cells, count, prices);
    // A row's sparkline, all zeros when the trend has no calls for it.
    let row_spark =
        |values: Option<&Vec<f64>>| charted.then(|| spark(values.map_or(&vec![0.0; count], |v| v)));
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
            (totals_table("Calls", std::iter::once((
                "All calls".to_owned(),
                all,
                row_spark(Some(&all_series)),
            ))))
            h2 { "Over time" }
            @if charted {
                (chart(&all_series, bucket_label, start, bucket_seconds, prices.is_some()))
                @if prices.is_some() && all.cost.unpriced {
                    p class="empty" { (UNPRICED_CAPTION) }
                }
            } @else {
                p class="empty" { (TOO_MANY_GROUPS) }
            }
            h2 { "By model" }
            (totals_table("Model", by_model.iter().map(|(k, t)| {
                (k.clone(), *t, row_spark(model_series.get(k)))
            })))
            h2 { "By agent" }
            (totals_table("Agent", by_agent.iter().map(|(k, t)| {
                (
                    if k.is_empty() { "(unknown)".to_owned() } else { k.clone() },
                    *t,
                    row_spark(agent_series.get(k)),
                )
            })))
            h2 { "By prompt" }
            (totals_table("Prompt", by_prompt.iter().map(|(k, t)| {
                (
                    k.clone().unwrap_or_else(|| "(none)".to_owned()),
                    *t,
                    row_spark(prompt_series.get(k)),
                )
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::usage::Cell;

    /// `m` costs $1 per 1,000,000 input tokens and $2 per 1,000,000 output tokens; `free` has no
    /// price.
    const PRICES: &str = "- model: m\n  input: 1.0\n  output: 2.0\n";

    fn cell(
        bucket: usize,
        model: &str,
        agent: &str,
        prompt: Option<&str>,
        usage: Usage,
    ) -> TrendCell {
        TrendCell {
            bucket,
            cell: Cell {
                agent: agent.into(),
                prompt: prompt.map(str::to_owned),
                calls: ModelCalls {
                    model: model.into(),
                    calls: 1,
                    usage,
                },
            },
        }
    }

    fn tokens(input: u64, output: u64, reasoning: u64) -> Usage {
        Usage {
            input,
            cached: 0,
            output,
            reasoning,
        }
    }

    /// Bucket 1 and 3 are empty; bucket 5 is past the 4 buckets.
    fn cells() -> Vec<TrendCell> {
        vec![
            cell(0, "m", "a", Some("p@1"), tokens(1_000_000, 0, 0)),
            cell(2, "m", "", None, tokens(0, 500_000, 0)),
            cell(2, "free", "a", None, tokens(100, 50, 10)),
            cell(5, "m", "a", None, tokens(1_000_000, 0, 0)),
        ]
    }

    #[test]
    fn series_sum_cost_per_bucket_with_prices() {
        let prices = PriceTable::from_yaml(PRICES).unwrap();
        let p = Some(&prices);
        let cells = cells();
        assert_eq!(
            series(&cells, 4, p, |c| c.cell.calls.model.clone()),
            BTreeMap::from([
                ("free".to_owned(), vec![0.0, 0.0, 0.0, 0.0]),
                ("m".to_owned(), vec![1.0, 0.0, 1.0, 0.0]),
            ])
        );
        assert_eq!(
            series(&cells, 4, p, |c| c.cell.agent.clone()),
            BTreeMap::from([
                (String::new(), vec![0.0, 0.0, 1.0, 0.0]),
                ("a".to_owned(), vec![1.0, 0.0, 0.0, 0.0]),
            ])
        );
        assert_eq!(
            series(&cells, 4, p, |c| c.cell.prompt.clone()),
            BTreeMap::from([
                (None, vec![0.0, 0.0, 1.0, 0.0]),
                (Some("p@1".to_owned()), vec![1.0, 0.0, 0.0, 0.0]),
            ])
        );
        assert_eq!(total(&cells, 4, p), vec![1.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn series_sum_tokens_per_bucket_without_prices() {
        let cells = cells();
        assert_eq!(
            series(&cells, 4, None, |c| c.cell.calls.model.clone()),
            BTreeMap::from([
                ("free".to_owned(), vec![0.0, 0.0, 160.0, 0.0]),
                ("m".to_owned(), vec![1_000_000.0, 0.0, 500_000.0, 0.0]),
            ])
        );
        assert_eq!(
            total(&cells, 4, None),
            vec![1_000_000.0, 0.0, 500_160.0, 0.0]
        );
        assert_eq!(total(&[], 3, None), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn chart_draws_tokens_per_hour() {
        let start = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        assert_eq!(
            chart(&[0.0, 2000.0, 1000.0], "hour", start, 3600, false).into_string(),
            r#"<svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label="tokens per hour"><polyline points="8.0,100.0 300.0,10.0 592.0,55.0" fill="none"></polyline><text x="8" y="12" text-anchor="start">max 2,000</text><text x="592" y="12" text-anchor="end">latest 1,000</text><text x="8" y="116" text-anchor="start">12:00</text><text x="592" y="116" text-anchor="end">14:00</text></svg>"#
        );
    }

    #[test]
    fn chart_draws_cost_per_day_with_dates() {
        let start = Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap();
        assert_eq!(
            chart(&[0.5, 0.25], "day", start, 86400, true).into_string(),
            r#"<svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label="cost per day"><polyline points="8.0,10.0 592.0,55.0" fill="none"></polyline><text x="8" y="12" text-anchor="start">max $0.500000</text><text x="592" y="12" text-anchor="end">latest $0.250000</text><text x="8" y="116" text-anchor="start">10-17</text><text x="592" y="116" text-anchor="end">10-18</text></svg>"#
        );
    }

    #[test]
    fn chart_of_zeros_lies_on_the_baseline() {
        let start = Utc.with_ymd_and_hms(2026, 11, 16, 11, 0, 0).unwrap();
        assert_eq!(
            chart(&[0.0, 0.0, 0.0], "5 minutes", start, 300, false).into_string(),
            r#"<svg class="usage-chart" viewBox="0 0 600 120" role="img" aria-label="tokens per 5 minutes"><polyline points="8.0,100.0 300.0,100.0 592.0,100.0" fill="none"></polyline><text x="8" y="12" text-anchor="start">max 0</text><text x="592" y="12" text-anchor="end">latest 0</text><text x="8" y="116" text-anchor="start">11:00</text><text x="592" y="116" text-anchor="end">11:10</text></svg>"#
        );
        // 6-hour buckets are labeled by date.
        assert!(
            chart(&[0.0, 0.0], "6 hours", start, 21600, true)
                .into_string()
                .contains(r#"<text x="8" y="116" text-anchor="start">11-16</text><text x="592" y="116" text-anchor="end">11-16</text>"#)
        );
    }

    #[test]
    fn spark_scales_each_row_to_its_own_maximum() {
        assert_eq!(
            spark(&[2.0, 1.0, 0.0]).into_string(),
            r#"<svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="usage trend"><polyline points="4.0,2.0 60.0,16.0 116.0,30.0" fill="none"></polyline><circle cx="116.0" cy="30.0" r="2"></circle></svg>"#
        );
        assert_eq!(
            spark(&[0.0, 0.0, 0.0]).into_string(),
            r#"<svg class="trend" viewBox="0 0 120 32" width="120" height="32" role="img" aria-label="usage trend"><polyline points="4.0,30.0 60.0,30.0 116.0,30.0" fill="none"></polyline><circle cx="116.0" cy="30.0" r="2"></circle></svg>"#
        );
    }
}
