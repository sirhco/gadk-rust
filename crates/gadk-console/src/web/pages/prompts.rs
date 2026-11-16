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

//! The prompt list page.

use chrono::SecondsFormat;
use maud::{Markup, html};

use super::layout::{Nav, page};
use super::prompt::href;
use crate::prompts::PromptRow;

/// The prompt list page: name link, latest version, label chips and the latest version's
/// creation time.
pub(crate) fn render(rows: &[PromptRow]) -> Markup {
    let body = html! {
        h1 { "Prompts" }
        @if rows.is_empty() {
            p class="empty" { "The store has no prompts yet." }
        } @else {
            table {
                thead {
                    tr { th { "Name" } th { "Latest" } th { "Labels" } th { "Created" } }
                }
                tbody {
                    @for r in rows {
                        tr {
                            td { a href=(href(&r.name)) { (r.name) } }
                            td class="num" { "v" (r.latest) }
                            td {
                                @for (label, version) in &r.labels {
                                    span class="chip" { (label) " → v" (version) }
                                }
                            }
                            td class="mono" {
                                (r.created_at.to_rfc3339_opts(SecondsFormat::Secs, true))
                            }
                        }
                    }
                }
            }
        }
    };
    page("Prompts", Nav::Prompts, body)
}
