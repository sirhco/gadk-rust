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

//! The prompt detail page: versions, diff, label and rollout forms.

use chrono::SecondsFormat;
use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE};
use super::error::banner;
use super::layout::{Nav, page};
use crate::evals::percent;
use crate::prices::thousands;
use crate::prompts::{DiffLine, DiffTag, DiffView, PromptDetail, VersionView};
use crate::scores::FeedbackRow;
use crate::traces::TimeRange;
use crate::url::percent_encode;

/// `/prompts/{name}`, with the name percent-encoded so it stays one path segment (prompt
/// names are `[a-z0-9._-]` segments joined by `/`, so only the `/` becomes `%2F`).
pub(crate) fn href(name: &str) -> String {
    format!("/prompts/{}", percent_encode(name))
}

fn csrf_field(csrf: &str) -> Markup {
    html! { input type="hidden" name="csrf" value=(csrf); }
}

fn version_options(d: &PromptDetail, selected: u32) -> Markup {
    html! {
        @for v in &d.versions {
            option value=(v.version) selected[v.version == selected] { "v" (v.version) }
        }
    }
}

fn diff_table(lines: &[DiffLine]) -> Markup {
    html! {
        @if lines.is_empty() {
            p class="empty" { "Empty in both versions." }
        } @else {
            table class="diff" {
                tbody {
                    @for l in lines {
                        @let (class, sign) = match l.tag {
                            DiffTag::Equal => ("diff-equal", " "),
                            DiffTag::Delete => ("diff-delete", "-"),
                            DiffTag::Insert => ("diff-insert", "+"),
                        };
                        tr class=(class) {
                            td class="ln" { @if let Some(n) = l.old_no { (n) } }
                            td class="ln" { @if let Some(n) = l.new_no { (n) } }
                            td { (sign) }
                            td { (l.text) }
                        }
                    }
                }
            }
        }
    }
}

/// The note shown when the trace source is not BigQuery.
pub(crate) const NEEDS_BIGQUERY_METRICS: &str = "Production metrics need the BigQuery trace source: start the console with --traces bigquery://<project>.<dataset>.<table>.";

/// The note shown when a metrics query hit its row cap.
pub(crate) const METRICS_TRUNCATED: &str =
    "Some versions or costs are left out: a query reached its row limit.";
/// The note shown when the invocation-to-version mapping hit its row cap.
pub(crate) const PASS_RATES_TRUNCATED: &str =
    "Eval pass rates leave out some runs: the version mapping reached its row limit.";

/// The note shown when the feedback query hit its row cap.
pub(crate) const FEEDBACK_TRUNCATED: &str =
    "Some scores are left out: the feedback query reached its row limit.";

/// The Feedback table of the production-metrics section.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FeedbackBody {
    /// The query failed: the error text and its hint.
    Failed(String, Option<&'static str>),
    /// One row per version, score name, source and data type.
    Rows {
        /// The rows, newest version first.
        rows: Vec<FeedbackRow>,
        /// The query hit its row cap.
        truncated: bool,
    },
}

/// The production-metrics section of a prompt page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProductionMetrics {
    /// The range shown.
    pub(crate) range: TimeRange,
    /// What the section shows.
    pub(crate) body: MetricsBody,
}

/// The content of the production-metrics section.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MetricsBody {
    /// The trace source is not BigQuery: a note, no query.
    NeedsBigQuery,
    /// A query failed: the error text and its hint.
    Failed(String, Option<&'static str>),
    /// One row per version seen in the range.
    Rows {
        /// The rows, newest version first.
        rows: Vec<MetricsRow>,
        /// Whether the console has a price file.
        priced: bool,
        /// The eval pass-rate column.
        pass: PassColumn,
        /// A metrics query hit its row cap: some versions or costs are left out.
        truncated: bool,
        /// The scores per version.
        feedback: FeedbackBody,
    },
}

/// One version's production metrics.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MetricsRow {
    pub(crate) version: u32,
    pub(crate) calls: u64,
    pub(crate) mean_input: f64,
    pub(crate) mean_output: f64,
    /// Mean cost per priced call; `None` when no call had a price.
    pub(crate) mean_cost: Option<f64>,
    /// Some calls had no price (the mean covers the priced ones).
    pub(crate) cost_partial: bool,
    pub(crate) p50_ms: Option<f64>,
    pub(crate) p95_ms: Option<f64>,
    /// `(passed, runs)` of the monitored runs mapped to this version.
    pub(crate) pass: Option<(u64, u64)>,
}

/// The eval pass-rate column.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PassColumn {
    /// No report store (`--reports`): no column.
    Off,
    /// Reading the reports or mapping the runs failed: a banner, no column.
    Failed(String),
    /// The column; `capped` when only the newest 5,000 invocations were mapped.
    On {
        /// More than 5,000 invocations were in range.
        capped: bool,
        /// The invocation-to-version mapping hit its row cap: some runs are left out.
        truncated: bool,
    },
}

/// `$x` per call (six decimals), `*` when it leaves out unpriced calls, `—` without a price.
fn mean_cost_text(r: &MetricsRow) -> String {
    match r.mean_cost {
        None => "—".to_owned(),
        Some(c) if r.cost_partial => format!("${c:.6}*"),
        Some(c) => format!("${c:.6}"),
    }
}

fn latency_text(ms: Option<f64>) -> String {
    ms.map_or_else(|| "—".to_owned(), |v| format!("{v:.0} ms"))
}

/// `NN.N% (passed/runs)`, or `—` when no run mapped to the version.
fn pass_text(pass: Option<(u64, u64)>) -> String {
    match pass {
        Some((passed, runs)) if runs > 0 => format!(
            "{} ({passed}/{runs})",
            percent(Some(passed as f64 / runs as f64))
        ),
        _ => "—".to_owned(),
    }
}

/// A feedback row's summary: `NN.N% positive` (`bool`), `mean N.NN` (`number`), `most common:
/// <value>` (`category`), else `—`.
fn feedback_result(r: &FeedbackRow) -> String {
    match r.data_type.as_str() {
        "bool" if r.count > 0 => format!(
            "{} positive",
            percent(Some(r.positive as f64 / r.count as f64))
        ),
        "number" => r
            .mean
            .map_or_else(|| "—".to_owned(), |m| format!("mean {m:.2}")),
        "category" => r
            .mode
            .as_ref()
            .map_or_else(|| "—".to_owned(), |m| format!("most common: {m}")),
        _ => "—".to_owned(),
    }
}

/// The Feedback table: per version, score name, source and data type, the count and summary.
fn feedback_table(base: &str, range: TimeRange, f: &FeedbackBody) -> Markup {
    html! {
        h3 { "Feedback" }
        @match f {
            FeedbackBody::Failed(text, hint) => {
                (banner(&format!("Feedback could not be loaded: {text}"), *hint))
            }
            FeedbackBody::Rows { rows, truncated } => {
                @if rows.is_empty() {
                    p class="empty" {
                        "No scores for calls of this prompt in the last " (range.as_str()) "."
                    }
                } @else {
                    table {
                        thead {
                            tr {
                                th { "Version" } th { "Score" } th { "Source" } th { "Type" }
                                th { "Count" } th { "Result" }
                            }
                        }
                        tbody {
                            @for r in rows {
                                tr {
                                    td { a href={ (base) "?v=" (r.version) } { "v" (r.version) } }
                                    td { (r.name) }
                                    td { (r.source) }
                                    td { (r.data_type) }
                                    td class="num" { (thousands(r.count)) }
                                    td { (feedback_result(r)) }
                                }
                            }
                        }
                    }
                }
                @if *truncated {
                    p class="empty" { (FEEDBACK_TRUNCATED) }
                }
            }
        }
    }
}

/// The production-metrics section: range picker, then the note, the error banner or the table.
fn metrics_section(base: &str, m: &ProductionMetrics) -> Markup {
    html! {
        h2 { "Production metrics" }
        form class="filters" method="get" action=(base) {
            label {
                "Range"
                select name="range" {
                    @for r in TimeRange::ALL {
                        option value=(r.as_str()) selected[r == m.range] { (r.as_str()) }
                    }
                }
            }
            button type="submit" { "Show" }
        }
        @match &m.body {
            MetricsBody::NeedsBigQuery => {
                div class="banner info" { p { (NEEDS_BIGQUERY_METRICS) } }
            }
            MetricsBody::Failed(text, hint) => {
                (banner(text, *hint))
            }
            MetricsBody::Rows { rows, priced, pass, truncated, feedback } => {
                @let pass_on = matches!(pass, PassColumn::On { .. });
                @if rows.is_empty() {
                    p class="empty" {
                        "No model calls used this prompt in the last " (m.range.as_str()) "."
                    }
                } @else {
                    @if !priced {
                        p class="empty" { (NO_PRICES) }
                    }
                    table {
                        thead {
                            tr {
                                th { "Version" } th { "LLM calls" } th { "Mean input" }
                                th { "Mean output" } th { "Mean cost" } th { "p50 latency" }
                                th { "p95 latency" }
                                @if pass_on { th { "Eval pass rate" } }
                            }
                        }
                        tbody {
                            @for r in rows {
                                tr {
                                    td { a href={ (base) "?v=" (r.version) } { "v" (r.version) } }
                                    td class="num" { (thousands(r.calls)) }
                                    td class="num" { (format!("{:.1}", r.mean_input)) }
                                    td class="num" { (format!("{:.1}", r.mean_output)) }
                                    td class="num" { (mean_cost_text(r)) }
                                    td class="num" { (latency_text(r.p50_ms)) }
                                    td class="num" { (latency_text(r.p95_ms)) }
                                    @if pass_on { td class="num" { (pass_text(r.pass)) } }
                                }
                            }
                        }
                    }
                    @if rows.iter().any(|r| r.cost_partial) {
                        p class="empty" { (PARTIAL_NOTE) }
                    }
                }
                @if *truncated {
                    p class="empty" { (METRICS_TRUNCATED) }
                }
                @match pass {
                    PassColumn::Failed(text) => {
                        (banner(&format!("Eval pass rates could not be loaded: {text}"), None))
                    }
                    PassColumn::On { capped, truncated } => {
                        @if *capped {
                            p class="empty" {
                                "Eval pass rates use the newest 5,000 monitored invocations in the range."
                            }
                        }
                        @if *truncated {
                            p class="empty" { (PASS_RATES_TRUNCATED) }
                        }
                    }
                    PassColumn::Off => {}
                }
                (feedback_table(base, m.range, feedback))
            }
        }
    }
}

/// The prompt detail page: version history, diff picker, the selected version, the diff,
/// and the label and rollout forms, and, when given, the production metrics after the diff
/// picker.
/// `error` (an `error::banner`) is shown under the heading.
pub(crate) fn render(
    d: &PromptDetail,
    view: Option<&VersionView>,
    diff: Option<&DiffView>,
    csrf: &str,
    error: Option<Markup>,
    metrics: Option<&ProductionMetrics>,
) -> Markup {
    let base = href(&d.name);
    let labels_action = format!("{base}/labels");
    // The picker defaults to the previous version against the latest.
    let (pick_a, pick_b) = diff.map_or_else(
        || {
            let older = d.versions.get(1).map_or(d.latest, |v| v.version);
            (older, d.latest)
        },
        |x| (x.a, x.b),
    );
    let body = html! {
        h1 { (d.name) }
        @if let Some(error) = error { (error) }

        h2 { "Versions" }
        table {
            thead {
                tr {
                    th { "Version" } th { "Labels" } th { "Created" } th { "Author" }
                    th { "Message" } th { "Hash" }
                }
            }
            tbody {
                @for v in &d.versions {
                    tr {
                        td { a href={ (base) "?v=" (v.version) } { "v" (v.version) } }
                        td {
                            @for l in &v.labels { span class="chip" { (l) } }
                            @for (l, p) in &v.rollouts { span class="chip" { (l) " " (p) "%" } }
                        }
                        td class="mono" {
                            (v.created_at.to_rfc3339_opts(SecondsFormat::Secs, true))
                        }
                        td { (v.author) }
                        td { (v.message) }
                        td class="mono" { (v.hash.get(..12).unwrap_or(&v.hash)) }
                    }
                }
            }
        }

        form method="get" action=(base) class="inline" {
            "Compare "
            select name="a" { (version_options(d, pick_a)) }
            " with "
            select name="b" { (version_options(d, pick_b)) }
            " "
            button type="submit" { "Diff" }
        }
        @if let Some(m) = metrics {
            (metrics_section(&base, m))
        }

        @if let Some(v) = view {
            h2 { "Version " (v.version) }
            h3 { "Template" }
            pre class="template" { (v.template) }
            h3 { "Config" }
            @if let Some(config) = &v.config {
                pre class="config" { (config) }
            } @else {
                p class="empty" { "No config." }
            }
        }

        @if let Some(x) = diff {
            h2 { "Diff v" (x.a) " → v" (x.b) }
            h3 { "Template" }
            (diff_table(&x.template))
            h3 { "Config" }
            (diff_table(&x.config))
        }

        h2 { "Labels" }
        @if d.labels.is_empty() {
            p class="empty" { "No labels." }
        } @else {
            table {
                thead {
                    tr { th { "Label" } th { "Version" } th { "Rollout" } th colspan="3" {} }
                }
                tbody {
                    @for (label, current) in &d.labels {
                        @let rollout = d.rollouts.iter().find(|(l, _, _)| l == label);
                        @let rollout_action = format!("{labels_action}/{label}/rollout");
                        tr {
                            td { span class="chip" { (label) } }
                            td class="num" { "v" (current) }
                            td {
                                @if let Some((_, v, p)) = rollout {
                                    "v" (v) " at " (p) "%"
                                } @else {
                                    "—"
                                }
                            }
                            td {
                                form method="post" action=(labels_action)
                                    data-confirm={ "Move label " (label) "?" } class="inline" {
                                    (csrf_field(csrf))
                                    input type="hidden" name="label" value=(label);
                                    input type="hidden" name="expected" value=(current);
                                    select name="version" { (version_options(d, *current)) }
                                    " "
                                    button type="submit" { "Move" }
                                }
                            }
                            td {
                                form method="post" action={ (labels_action) "/" (label) "/delete" }
                                    data-confirm={ "Remove label " (label) "?" } class="inline" {
                                    (csrf_field(csrf))
                                    button type="submit" { "Remove" }
                                }
                            }
                            td {
                                @if let Some((_, v, p)) = rollout {
                                    form method="post" action=(rollout_action)
                                        data-confirm={ "Change the rollout of " (label) "?" }
                                        class="inline" {
                                        (csrf_field(csrf))
                                        input type="hidden" name="version" value=(v);
                                        input type="number" name="percent" value=(p) min="1"
                                            max="99" required;
                                        "% "
                                        button type="submit" { "Set percent" }
                                    }
                                    " "
                                    form method="post" action={ (rollout_action) "/promote" }
                                        data-confirm={ "Promote v" (v) " to " (label) "?" }
                                        class="inline" {
                                        (csrf_field(csrf))
                                        input type="hidden" name="version" value=(v);
                                        input type="hidden" name="expected" value=(current);
                                        button type="submit" { "Promote" }
                                    }
                                    " "
                                    form method="post" action={ (rollout_action) "/delete" }
                                        data-confirm={ "Abort the rollout of " (label) "?" }
                                        class="inline" {
                                        (csrf_field(csrf))
                                        button type="submit" { "Abort" }
                                    }
                                } @else {
                                    form method="post" action=(rollout_action)
                                        data-confirm={ "Start a rollout on " (label) "?" }
                                        class="inline" {
                                        (csrf_field(csrf))
                                        select name="version" { (version_options(d, d.latest)) }
                                        " to "
                                        input type="number" name="percent" placeholder="percent"
                                            min="1" max="99" required;
                                        "% "
                                        button type="submit" { "Start rollout" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        h3 { "New label" }
        form method="post" action=(labels_action) data-confirm="Set this label?" class="inline" {
            (csrf_field(csrf))
            input type="text" name="label" placeholder="label" required
                pattern="[a-z0-9][a-z0-9\\-]{0,62}" maxlength="63";
            input type="hidden" name="expected" value="";
            " "
            select name="version" { (version_options(d, d.latest)) }
            " "
            button type="submit" { "Set label" }
        }
    };
    page(&d.name, Nav::Prompts, body)
}
