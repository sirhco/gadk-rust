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

//! The users page: the users of a window with their sessions, turns, tokens, cost, errors and
//! thumbs.

use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE};
use super::layout::{Nav, page};
use super::sessions::{TRUNCATED, millis, totals, user_text};
use crate::prices::{PriceTable, thousands};
use crate::sessions::{USER_CAP, UserPage, UserQuery};
use crate::traces::TimeRange;
use crate::url::percent_encode;

/// The filter form of `/users`: agent and range.
fn filter_form(q: &UserQuery) -> Markup {
    html! {
        form class="filters" method="get" action="/users" {
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

/// `/users`: the filter form, a link to `/sessions`, then one row per user (most recent last
/// activity first), each linked to its sessions except `(none)`.
pub(crate) fn render(q: &UserQuery, p: &UserPage, prices: Option<&PriceTable>) -> Markup {
    let rows: Vec<_> = p
        .users
        .iter()
        .map(|u| (u, totals(&u.tokens, prices)))
        .collect();
    let partial = rows.iter().any(|(_, (_, cost))| cost.partial());
    let body = html! {
        h1 { "Users" }
        (filter_form(q))
        p { a href={ "/sessions?range=" (q.range.as_str()) } { "View by session →" } }
        @if prices.is_none() {
            p class="empty" { (NO_PRICES) }
        }
        @if p.truncated {
            p class="empty" { (TRUNCATED) }
        }
        @if rows.is_empty() {
            p class="empty" { "No users in the last " (q.range.as_str()) "." }
        } @else {
            @if p.more {
                p { "showing the " (USER_CAP) " most recent users" }
            }
            table {
                thead {
                    tr {
                        th { "User" } th { "Sessions" } th { "Turns" } th { "Tokens" }
                        th { "Cost" } th { "Errors" } th { "👍" } th { "👎" } th { "Last seen" }
                    }
                }
                tbody {
                    @for (u, (total, cost)) in &rows {
                        tr {
                            td {
                                @if u.user.is_empty() {
                                    (user_text(&u.user))
                                } @else {
                                    a href={
                                        "/sessions?user=" (percent_encode(&u.user))
                                        "&range=" (q.range.as_str())
                                    } { (u.user) }
                                }
                            }
                            td class="num" { (thousands(u.sessions)) }
                            td class="num" { (thousands(u.turns)) }
                            td class="num" { (thousands(*total)) }
                            td class="num" { (cost.text()) }
                            td class="num" { (thousands(u.errors)) }
                            td class="num" { (thousands(u.ups)) }
                            td class="num" { (thousands(u.downs)) }
                            td class="mono" { (millis(u.last)) }
                        }
                    }
                }
            }
        }
        @if partial {
            p class="empty" { (PARTIAL_NOTE) }
        }
    };
    page("Users", Nav::Sessions, body)
}
