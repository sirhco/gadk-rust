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

//! The sessions page: the sessions of a window with their turns, tokens, cost, errors and
//! thumbs.

use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE};
use super::layout::{Nav, page};
use super::millis;
use crate::prices::{CostSum, PriceTable, Usage, cost_of, thousands};
use crate::sessions::{SessionPage, SessionQuery};
use crate::traces::TimeRange;
use crate::web::sessions::{next_link, session_link};

/// The note shown when a query hit its row cap.
pub(crate) const TRUNCATED: &str =
    "results truncated: the query hit its 5,000-row cap, so some rows or models are left out.";

/// Input + output + reasoning tokens over `tokens`, and their cost (each model priced on its
/// own).
pub(crate) fn totals(tokens: &[(String, Usage)], prices: Option<&PriceTable>) -> (u64, CostSum) {
    let mut total = 0u64;
    let mut cost = CostSum::default();
    for (model, usage) in tokens {
        total = total
            .saturating_add(usage.input)
            .saturating_add(usage.output)
            .saturating_add(usage.reasoning);
        cost.add(cost_of(prices, model, usage));
    }
    (total, cost)
}

/// `user`, or `(none)` when empty.
pub(crate) fn user_text(user: &str) -> &str {
    if user.is_empty() { "(none)" } else { user }
}

/// The filter form of `/sessions`: user, agent and range.
fn filter_form(q: &SessionQuery) -> Markup {
    html! {
        form class="filters" method="get" action="/sessions" {
            label {
                "User"
                input type="text" name="user" maxlength="128" value=[q.user.as_deref()];
            }
            label {
                "Agent"
                input type="text" name="agent" maxlength="128" value=[q.agent.as_deref()];
            }
            label {
                "Range"
                select name="range" {
                    @for r in TimeRange::ALL {
                        option value=(r.as_str()) selected[r == q.range] { (r.as_str()) }
                    }
                }
            }
            button type="submit" { "Show" }
        }
    }
}

/// `/sessions`: the filter form, a link to `/users`, then one row per session (newest last
/// activity first) and a `Next →` link when more sessions follow.
pub(crate) fn render(q: &SessionQuery, p: &SessionPage, prices: Option<&PriceTable>) -> Markup {
    let rows: Vec<_> = p
        .sessions
        .iter()
        .map(|s| (s, totals(&s.tokens, prices)))
        .collect();
    let partial = rows.iter().any(|(_, (_, cost))| cost.partial());
    let body = html! {
        h1 { "Sessions" }
        (filter_form(q))
        p { a href={ "/users?range=" (q.range.as_str()) } { "View by user →" } }
        @if prices.is_none() {
            p class="empty" { (NO_PRICES) }
        }
        @if p.truncated {
            p class="empty" { (TRUNCATED) }
        }
        @if rows.is_empty() {
            p class="empty" { "No sessions in the last " (q.range.as_str()) "." }
        } @else {
            table {
                thead {
                    tr {
                        th { "Session" } th { "User" } th { "Agent" } th { "First activity" }
                        th { "Last activity" } th { "Turns" } th { "Tokens" } th { "Cost" }
                        th { "Errors" } th { "👍" } th { "👎" }
                    }
                }
                tbody {
                    @for (s, (total, cost)) in &rows {
                        tr {
                            td class="mono" {
                                a href=(session_link(&s.session_id, q.range)) { (s.session_id) }
                            }
                            td { (user_text(&s.user)) }
                            td { (s.agent) }
                            td class="mono" { (millis(s.first)) }
                            td class="mono" { (millis(s.last)) }
                            td class="num" { (thousands(s.turns)) }
                            td class="num" { (thousands(*total)) }
                            td class="num" { (cost.text()) }
                            td class="num" { (thousands(s.errors)) }
                            td class="num" { (thousands(s.ups)) }
                            td class="num" { (thousands(s.downs)) }
                        }
                    }
                }
            }
            @if p.has_next {
                @if let Some((last, _)) = rows.last() {
                    p { a href=(next_link(q, &last.session_id, last.last)) { "Next →" } }
                }
            }
        }
        @if partial {
            p class="empty" { (PARTIAL_NOTE) }
        }
    };
    page("Sessions", Nav::Sessions, body)
}
