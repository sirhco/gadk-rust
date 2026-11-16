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

//! The trace list page.

use std::fmt::Write as _;

use chrono::SecondsFormat;
use maud::{Markup, html};

use super::layout::{Nav, page};
use crate::traces::{TimeRange, TracePage, TraceQuery};
use crate::url::percent_encode;

/// The trace list: filter form, one row per trace, and the next-page link.
pub(crate) fn render(q: &TraceQuery, list: &TracePage) -> Markup {
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    let body = html! {
        h1 { "Traces" }
        form class="filters" method="get" action="/traces" {
            label {
                "Range"
                select name="range" {
                    @for r in TimeRange::ALL {
                        option value=(r.as_str()) selected[r == q.range] { (r.as_str()) }
                    }
                }
            }
            label { "Root span name" input type="text" name="name" value=(text(&q.name)); }
            label { "Agent" input type="text" name="agent" value=(text(&q.agent)); }
            label { "Session" input type="text" name="session" value=(text(&q.session)); }
            button type="submit" { "Filter" }
        }
        @if list.traces.is_empty() {
            p class="empty" { "No traces in this range" }
        } @else {
            table {
                thead {
                    tr { th { "Root span" } th { "Started" } th { "Duration" } th { "Agent" } th { "Session" } }
                }
                tbody {
                    @for t in &list.traces {
                        tr {
                            td { a href={ "/traces/" (t.trace_id.as_str()) } { (t.root_name) } }
                            td class="mono" { (t.start.to_rfc3339_opts(SecondsFormat::Secs, true)) }
                            td class="num" { (format!("{:.1} ms", t.duration_ms)) }
                            td { (text(&t.agent)) }
                            td class="mono" { (text(&t.session)) }
                        }
                    }
                }
            }
        }
        @if let Some(token) = &list.next {
            p class="pager" { a href=(next_href(q, token)) { "Next page" } }
        }
    };
    page("Traces", Nav::Traces, body)
}

/// `/traces?range=…[&name=…][&agent=…][&session=…]&page={token}`: the current filters plus
/// the next page token, each value percent-encoded.
fn next_href(q: &TraceQuery, token: &str) -> String {
    let mut href = format!("/traces?range={}", q.range.as_str());
    let filters = [
        ("name", &q.name),
        ("agent", &q.agent),
        ("session", &q.session),
    ];
    for (key, value) in filters {
        if let Some(value) = value {
            let _ = write!(href, "&{key}={}", percent_encode(value));
        }
    }
    let _ = write!(href, "&page={}", percent_encode(token));
    href
}
