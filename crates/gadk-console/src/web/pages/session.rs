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

//! The session page: one session's turns as a conversation, each with its metrics.

use std::collections::HashMap;

use gadk_eval_monitor::ProductionRun;
use maud::{Markup, html};

use super::cost::{NO_PRICES, PARTIAL_NOTE};
use super::error::banner;
use super::layout::{Nav, page};
use super::millis;
use super::sessions::{totals, user_text};
use crate::prices::{PriceTable, thousands};
use crate::sessions::{SessionRow, TURN_CAP, TurnRow};
use crate::traces::{TimeRange, TraceId};
use crate::url::percent_encode;
use crate::web::sessions::session_link;

/// The conversation text of the turns shown.
#[derive(Debug, Clone)]
pub(crate) enum Conversation {
    /// The runs found, by invocation id.
    Runs(HashMap<String, ProductionRun>),
    /// Why the text could not be read.
    Unavailable(String),
}

/// The 404 page body of a session without activity in the window: the note and links to the
/// last 7 days of the session and to `/sessions`.
pub(crate) fn render_missing(id: &str, range: TimeRange) -> Markup {
    let body = html! {
        h1 { "Session " (id) }
        div class="banner error" {
            p { "No activity for session " (id) " in the last " (range.as_str()) "." }
        }
        p {
            a href=(session_link(id, TimeRange::Days7)) { "Look in the last 7d" }
            " · "
            a href="/sessions" { "All sessions" }
        }
    };
    page("Session", Nav::Sessions, body)
}

/// The user's bubble and the agent's bubble of `run` (texts of several turns joined by a blank
/// line, the run error after the replies), then the incomplete-content note.
fn bubbles(run: &ProductionRun) -> Markup {
    let join = |texts: Vec<&str>| texts.join("\n\n");
    let user = join(
        run.transcript
            .turns
            .iter()
            .map(|t| t.user.as_str())
            .collect(),
    );
    let reply = join(
        run.transcript
            .turns
            .iter()
            .map(|t| t.reply.as_str())
            .collect(),
    );
    html! {
        div class="bubble user" { (user) }
        div class="bubble agent" {
            (reply)
            @if let Some(error) = &run.transcript.error {
                div class="run-error" { (error) }
            }
        }
        @if !run.content_complete {
            div class="chat-note" {
                "content incomplete"
                @if !run.notes.is_empty() {
                    ul {
                        @for note in &run.notes {
                            li { (note) }
                        }
                    }
                }
            }
        }
    }
}

/// The meta line of a turn: start, latency, tokens and cost, tools, errors, scores and the
/// trace link.
fn meta(t: &TurnRow, prices: Option<&PriceTable>) -> Markup {
    let (total, cost) = totals(&t.tokens, prices);
    let latency = t.end.map_or_else(
        || "—".to_owned(),
        |end| {
            format!(
                "{} ms",
                thousands((end - t.start).num_milliseconds().max(0) as u64)
            )
        },
    );
    let scores: Vec<String> = t
        .scores
        .iter()
        .map(|s| format!("{}={}", s.name, s.value))
        .collect();
    html! {
        p class="chat-meta" {
            span class="mono" { (millis(t.start)) }
            " · latency " (latency)
            " · " (thousands(total)) " tokens · " (cost.text())
            @if !t.tools.is_empty() {
                " · tools: " (t.tools.join(", "))
            }
            " · errors: " (thousands(t.errors))
            @if !scores.is_empty() {
                " · scores: " (scores.join(", "))
            }
            @if let Some(id) = t.trace_id.as_deref().and_then(TraceId::parse) {
                " · "
                a href={ "/traces/" (id.as_str()) } { "trace →" }
            }
        }
    }
}

/// `/sessions/{id}`: the header (the totals of `head`, the sessions query's row of the
/// session), then at most [`TURN_CAP`] turns oldest first (`turns` may hold one more, which
/// only marks the cap as hit), each with its bubbles from `text` and its meta line.
pub(crate) fn render(
    head: &SessionRow,
    range: TimeRange,
    turns: &[TurnRow],
    text: &Conversation,
    prices: Option<&PriceTable>,
) -> Markup {
    let (total, cost) = totals(&head.tokens, prices);
    let shown = &turns[..turns.len().min(TURN_CAP)];
    let partial = cost.partial() || shown.iter().any(|t| totals(&t.tokens, prices).1.partial());
    let body = html! {
        h1 { "Session " (head.session_id) }
        p {
            "Activity in the last " (range.as_str()) " · "
            a href={
                "/traces?session=" (percent_encode(&head.session_id)) "&range=" (range.as_str())
            } { "Open in traces list" }
            " · "
            a href={ "/sessions?range=" (range.as_str()) } { "All sessions" }
        }
        dl class="eval-meta" {
            dt { "User" } dd { (user_text(&head.user)) }
            dt { "Agent" } dd { (head.agent) }
            dt { "First activity" } dd class="mono" { (millis(head.first)) }
            dt { "Last activity" } dd class="mono" { (millis(head.last)) }
            dt { "Turns" } dd { (thousands(head.turns)) }
            dt { "Tokens" } dd { (thousands(total)) }
            dt { "Cost" } dd { (cost.text()) }
            dt { "Errors" } dd { (thousands(head.errors)) }
            dt { "👍 / 👎" } dd { (thousands(head.ups)) " / " (thousands(head.downs)) }
        }
        @if prices.is_none() {
            p class="empty" { (NO_PRICES) }
        }
        @if let Conversation::Unavailable(e) = text {
            (banner(&format!("Conversation text unavailable: {e}"), None))
        }
        @if turns.len() > TURN_CAP {
            p class="empty" { "showing the first " (TURN_CAP) " turns" }
        }
        @for t in shown {
            div class="chat-turn" {
                @if let Conversation::Runs(runs) = text {
                    @match runs.get(&t.invocation_id) {
                        Some(run) => { (bubbles(run)) }
                        None => { p class="empty" { "(no content logged)" } }
                    }
                }
                (meta(t, prices))
            }
        }
        @if partial {
            p class="empty" { (PARTIAL_NOTE) }
        }
    };
    page("Session", Nav::Sessions, body)
}
