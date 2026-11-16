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

//! The prompt detail page: versions, diff and label forms.

use chrono::SecondsFormat;
use maud::{Markup, html};

use super::layout::{Nav, page};
use crate::prompts::{DiffLine, DiffTag, DiffView, PromptDetail, VersionView};
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

/// The prompt detail page: version history, diff picker, the selected version, the diff,
/// and the label forms. `error` (an `error::banner`) is shown under the heading.
pub(crate) fn render(
    d: &PromptDetail,
    view: Option<&VersionView>,
    diff: Option<&DiffView>,
    csrf: &str,
    error: Option<Markup>,
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
                        td { @for l in &v.labels { span class="chip" { (l) } } }
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
                tbody {
                    @for (label, current) in &d.labels {
                        tr {
                            td { span class="chip" { (label) } }
                            td class="num" { "v" (current) }
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
