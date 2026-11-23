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

//! The review page: the queue's filter form and its pending traces.

use chrono::{DateTime, Utc};
use maud::{Markup, html};

use super::layout::{Nav, page};
use crate::review::{PendingTrace, REVIEW_CAP, ReviewQueue};
use crate::traces::TimeRange;
use crate::web::review::review_link;

/// The review page: the filter form (keeping the queue's values), then, for a queue, the
/// heading, the count line, the "Start reviewing" link and at most [`REVIEW_CAP`] pending
/// traces (`pending` may hold one more, which only marks the cap as hit), or the empty state
/// (`done`: the reviewer just finished the queue).
pub(crate) fn render(
    queue: Option<&ReviewQueue>,
    pending: Option<&[PendingTrace]>,
    done: bool,
) -> Markup {
    let title = match queue {
        Some(q) => format!("Review: {}", q.score),
        None => "Review".to_owned(),
    };
    let body = html! {
        h1 { (title) }
        (filter_form(queue))
        @match (queue, pending) {
            (Some(q), Some(rows)) => {
                @if rows.is_empty() {
                    p class="empty" {
                        @if done { "Queue empty — all caught up" } @else { "Queue empty" }
                    }
                } @else {
                    p {
                        @if rows.len() > REVIEW_CAP {
                            (REVIEW_CAP) "+ pending (showing " (REVIEW_CAP) ")"
                        } @else {
                            (rows.len()) " pending"
                        }
                    }
                    p { a href=(review_link(q, &rows[0].trace_id, rows[0].first)) { "Start reviewing" } }
                    (pending_table(q, &rows[..rows.len().min(REVIEW_CAP)]))
                }
            }
            _ => {
                p class="empty" {
                    "Enter a score name to list the recent traces that have no console score with that name yet."
                }
            }
        }
    };
    page(&title, Nav::Review, body)
}

/// The GET form that defines a queue; its fields keep the queue's values (the range defaults
/// to 7d).
fn filter_form(queue: Option<&ReviewQueue>) -> Markup {
    let range = queue.map_or(TimeRange::Days7, |q| q.range);
    html! {
        form class="filters" method="get" action="/review" {
            label {
                "Score name"
                input type="text" name="score" value=[queue.map(|q| q.score.as_str())]
                    required maxlength="100";
            }
            label {
                "Prompt"
                input type="text" name="prompt" value=[queue.and_then(|q| q.prompt.as_deref())];
            }
            label {
                "Version"
                input type="number" name="version" min="1" value=[queue.and_then(|q| q.version)];
            }
            label {
                input type="checkbox" name="flagged" value="1"
                    checked[queue.is_some_and(|q| q.flagged)];
                " Thumbs-down only"
            }
            label {
                "Range"
                select name="range" {
                    @for r in TimeRange::ALL {
                        option value=(r.as_str()) selected[r == range] { (r.as_str()) }
                    }
                }
            }
            button type="submit" { "Show queue" }
        }
    }
}

/// One row per pending trace: its first LLM response, its link in review mode, its prompt
/// versions and its app thumbs-down count.
fn pending_table(q: &ReviewQueue, rows: &[PendingTrace]) -> Markup {
    html! {
        table {
            thead {
                tr { th { "Time" } th { "Trace" } th { "Prompt versions" } th { "Thumbs down" } }
            }
            tbody {
                @for t in rows {
                    tr {
                        td class="mono" { (millis(t.first)) }
                        td class="mono" {
                            a href=(review_link(q, &t.trace_id, t.first)) { (t.trace_id) }
                        }
                        td { (t.prompts.join(", ")) }
                        td class="num" { (t.thumbs_down) }
                    }
                }
            }
        }
    }
}

/// `%Y-%m-%dT%H:%M:%S%.3fZ`, as the scores table shows times.
fn millis(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
