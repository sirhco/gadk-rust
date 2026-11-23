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

//! The page shell: document head, navigation and the HTML response helper.

use axum::response::{IntoResponse, Response};
use http::StatusCode;
use http::header::CONTENT_TYPE;
use maud::{DOCTYPE, Markup, html};

/// The active navigation entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nav {
    Traces,
    Prompts,
    Evals,
    Experiments,
    Guards,
    Review,
    Sessions,
    Usage,
}

/// Full HTML document: <!DOCTYPE html>, <title>{title} · gadk console</title>, <link rel=stylesheet
/// href="/assets/console.css">, <script src="/assets/waterfall.js" defer>, a header nav with links
/// /traces, /prompts, /evals, /experiments, /guards, /review, /sessions and /usage (the active one
/// has class "active"), then `body` in <main>.
pub(crate) fn page(title: &str, nav: Nav, body: Markup) -> Markup {
    let link = |href: &str, label: &str, this: Nav| {
        html! {
            @if nav == this {
                a class="active" href=(href) { (label) }
            } @else {
                a href=(href) { (label) }
            }
        }
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · gadk console" }
                link rel="stylesheet" href="/assets/console.css";
                script src="/assets/waterfall.js" defer {}
            }
            body {
                header class="top" {
                    span class="brand" { "gadk console" }
                    nav {
                        (link("/traces", "Traces", Nav::Traces))
                        (link("/prompts", "Prompts", Nav::Prompts))
                        (link("/evals", "Evals", Nav::Evals))
                        (link("/experiments", "Experiments", Nav::Experiments))
                        (link("/guards", "Guards", Nav::Guards))
                        (link("/review", "Review", Nav::Review))
                        (link("/sessions", "Sessions", Nav::Sessions))
                        (link("/usage", "Usage", Nav::Usage))
                    }
                }
                main { (body) }
            }
        }
    }
}

/// HTML response with status.
pub(crate) fn html(status: StatusCode, markup: Markup) -> Response {
    (
        status,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        markup.into_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use maud::html;

    use super::{Nav, page};

    #[test]
    fn page_has_the_shell_and_marks_the_active_nav() {
        let out = page("Traces", Nav::Traces, html! { p { "hi <b>" } }).into_string();
        assert!(out.starts_with("<!DOCTYPE html>"), "{out}");
        assert!(
            out.contains("<title>Traces · gadk console</title>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<link rel="stylesheet" href="/assets/console.css">"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<script src="/assets/waterfall.js" defer></script>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<a class="active" href="/traces">Traces</a><a href="/prompts">Prompts</a><a href="/evals">Evals</a><a href="/experiments">Experiments</a><a href="/guards">Guards</a><a href="/review">Review</a><a href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );
        assert!(out.contains("<main><p>hi &lt;b&gt;</p></main>"), "{out}");

        let out = page("P", Nav::Prompts, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a class="active" href="/prompts">Prompts</a><a href="/evals">Evals</a><a href="/experiments">Experiments</a><a href="/guards">Guards</a><a href="/review">Review</a><a href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );

        let out = page("E", Nav::Evals, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a href="/prompts">Prompts</a><a class="active" href="/evals">Evals</a><a href="/experiments">Experiments</a><a href="/guards">Guards</a><a href="/review">Review</a><a href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );

        let out = page("X", Nav::Experiments, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a href="/prompts">Prompts</a><a href="/evals">Evals</a><a class="active" href="/experiments">Experiments</a><a href="/guards">Guards</a><a href="/review">Review</a><a href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );

        let out = page("G", Nav::Guards, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a href="/prompts">Prompts</a><a href="/evals">Evals</a><a href="/experiments">Experiments</a><a class="active" href="/guards">Guards</a><a href="/review">Review</a><a href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );

        let out = page("R", Nav::Review, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a href="/prompts">Prompts</a><a href="/evals">Evals</a><a href="/experiments">Experiments</a><a href="/guards">Guards</a><a class="active" href="/review">Review</a><a href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );

        let out = page("S", Nav::Sessions, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a href="/prompts">Prompts</a><a href="/evals">Evals</a><a href="/experiments">Experiments</a><a href="/guards">Guards</a><a href="/review">Review</a><a class="active" href="/sessions">Sessions</a><a href="/usage">Usage</a>"#),
            "{out}"
        );

        let out = page("U", Nav::Usage, html! {}).into_string();
        assert!(
            out.contains(r#"<a href="/traces">Traces</a><a href="/prompts">Prompts</a><a href="/evals">Evals</a><a href="/experiments">Experiments</a><a href="/guards">Guards</a><a href="/review">Review</a><a href="/sessions">Sessions</a><a class="active" href="/usage">Usage</a>"#),
            "{out}"
        );
    }
}
