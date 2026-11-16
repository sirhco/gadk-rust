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

//! Error banners and error pages (R13).

use axum::response::Response;
use gadk_prompts::StoreError;
use http::StatusCode;
use maud::{Markup, html};

use super::layout::{self, Nav};
use crate::traces::{SourceError, excerpt};

/// <div class="banner error">: text, then (when hint) <p class="hint">hint</p>.
pub(crate) fn banner(text: &str, hint: Option<&str>) -> Markup {
    html! {
        div class="banner error" {
            p { (text) }
            @if let Some(hint) = hint {
                p class="hint" { (hint) }
            }
        }
    }
}

/// Full page with the banner, under the given nav.
pub(crate) fn error_page(status: StatusCode, nav: Nav, text: &str, hint: Option<&str>) -> Response {
    let title = format!(
        "{} {}",
        status.as_str(),
        status.canonical_reason().unwrap_or("Error")
    );
    let body = html! {
        h1 { (title) }
        (banner(text, hint))
    };
    layout::html(status, layout::page(&title, nav, body))
}

/// NotFound → 404 "trace not found"; other variants → 502 with e.to_string() and e.hint().
pub(crate) fn source_error(e: &SourceError) -> Response {
    match e {
        SourceError::NotFound => {
            error_page(StatusCode::NOT_FOUND, Nav::Traces, "trace not found", None)
        }
        _ => error_page(
            StatusCode::BAD_GATEWAY,
            Nav::Traces,
            &e.to_string(),
            e.hint(),
        ),
    }
}

/// NotFound → 404; Invalid → 400; Conflict → 409; others → 502.
pub(crate) fn store_status(e: &StoreError) -> StatusCode {
    match e {
        StoreError::NotFound { .. } => StatusCode::NOT_FOUND,
        StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
        StoreError::Conflict { .. } => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// The text shown for a store failure: `Http` is `http {status}: ` plus an excerpt of at most
/// 1,024 bytes of the upstream body; every other variant is `e.to_string()` cut to an excerpt
/// of at most 1,024 bytes.
pub(crate) fn store_text(e: &StoreError) -> String {
    match e {
        StoreError::Http { status, body } => {
            format!("http {status}: {}", excerpt(body.as_bytes()))
        }
        _ => excerpt(e.to_string().as_bytes()),
    }
}

/// The error page for a store failure: `store_status(e)`, nav Prompts, text
/// [`store_text`].
pub(crate) fn store_error(e: &StoreError) -> Response {
    error_page(store_status(e), Nav::Prompts, &store_text(e), None)
}

#[cfg(test)]
mod tests {
    use gadk_prompts::StoreError;
    use http::StatusCode;
    use http::header::CONTENT_TYPE;

    use super::{banner, source_error, store_error, store_status, store_text};
    use crate::traces::{Service, SourceError};

    async fn body(resp: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn not_found_trace_is_404() {
        let resp = source_error(&SourceError::NotFound);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(resp.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(body(resp).await.contains("trace not found"));
    }

    #[tokio::test]
    async fn upstream_403_is_502_with_the_hint() {
        let e = SourceError::Upstream {
            service: Service::Trace,
            status: 403,
            excerpt: "permission denied".into(),
            reason: None,
        };
        let resp = source_error(&e);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let html = body(resp).await;
        assert!(
            html.contains("Cloud Trace: HTTP 403: permission denied"),
            "{html}"
        );
        assert!(
            html.contains(r#"<p class="hint">needs roles/cloudtrace.user</p>"#),
            "{html}"
        );
        assert!(html.contains(r#"<div class="banner error">"#), "{html}");
    }

    #[tokio::test]
    async fn other_source_errors_are_502() {
        let e = SourceError::Transport {
            service: Service::Logging,
            detail: "connection reset".into(),
        };
        let resp = source_error(&e);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let html = body(resp).await;
        assert!(html.contains("Cloud Logging: connection reset"), "{html}");
        assert!(!html.contains("class=\"hint\""), "{html}");
    }

    #[tokio::test]
    async fn error_text_is_escaped_and_nothing_else_is_echoed() {
        let e = SourceError::Upstream {
            service: Service::Logging,
            status: 500,
            excerpt: "<script>alert(1)</script>".into(),
            reason: None,
        };
        let html = body(source_error(&e)).await;
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );

        let b = banner("a & b", Some("<hint>")).into_string();
        assert_eq!(
            b,
            r#"<div class="banner error"><p>a &amp; b</p><p class="hint">&lt;hint&gt;</p></div>"#
        );
    }

    #[test]
    fn store_status_maps_every_variant() {
        let cases = [
            (
                StoreError::NotFound {
                    name: "greet".into(),
                    selector: String::new(),
                },
                StatusCode::NOT_FOUND,
            ),
            (
                StoreError::Conflict {
                    expected: Some(1),
                    actual: None,
                },
                StatusCode::CONFLICT,
            ),
            (
                StoreError::Invalid("bad label".into()),
                StatusCode::BAD_REQUEST,
            ),
            (StoreError::Io("disk".into()), StatusCode::BAD_GATEWAY),
            (
                StoreError::Http {
                    status: 503,
                    body: "unavailable".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (StoreError::Decode("json".into()), StatusCode::BAD_GATEWAY),
        ];
        for (e, status) in cases {
            assert_eq!(store_status(&e), status, "{e}");
        }
    }

    #[tokio::test]
    async fn store_error_page_carries_the_text_under_prompts() {
        let resp = store_error(&StoreError::Invalid("bad label".into()));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let html = body(resp).await;
        assert!(html.contains("invalid: bad label"), "{html}");
        assert!(
            html.contains(r#"<a class="active" href="/prompts">"#),
            "{html}"
        );
    }

    #[tokio::test]
    async fn store_http_error_carries_only_an_excerpt_of_the_body() {
        let mut upstream = "x".repeat(2000);
        upstream.push_str("END-OF-UPSTREAM-BODY");
        let resp = store_error(&StoreError::Http {
            status: 503,
            body: upstream,
        });
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let html = body(resp).await;
        assert!(!html.contains("END-OF-UPSTREAM-BODY"), "{html}");
        let start = html.find("<p>http 503: ").unwrap() + "<p>http 503: ".len();
        let len = html[start..].find("</p>").unwrap();
        assert!(len <= 1024, "excerpt is {len} bytes");
        assert_eq!(len, 1024);
    }

    #[test]
    fn store_text_is_capped_for_every_variant() {
        assert_eq!(
            store_text(&StoreError::Http {
                status: 503,
                body: "unavailable".into(),
            }),
            "http 503: unavailable"
        );
        let long = store_text(&StoreError::Http {
            status: 500,
            body: format!("{}END", "y".repeat(2000)),
        });
        assert_eq!(long, format!("http 500: {}", "y".repeat(1024)));
        let invalid = StoreError::Invalid("bad label".into());
        assert_eq!(store_text(&invalid), invalid.to_string());
        let io = StoreError::Io("z".repeat(2000));
        let text = store_text(&io);
        assert!(text.len() <= 1024, "io text is {} bytes", text.len());
        assert!(io.to_string().starts_with(&text));
    }
}
