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

//! Prompt routes: `/prompts`, `/prompts/{name}` and the label writes.

use axum::Form;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use gadk_prompts::{Label, PromptSnapshot, StoreError};
use http::StatusCode;
use maud::{Markup, html};
use serde::Deserialize;

use super::AppState;
use super::guard::check_csrf;
use super::pages::error::{banner, error_page, store_error, store_status, store_text};
use super::pages::layout::{self, Nav};
use super::pages::{prompt, prompts};
use crate::prompts::{diff_view, prompt_detail, prompt_rows, version_view};

const NO_STORE: &str = "No prompt store configured (start with --store)";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/prompts", get(list))
        .route("/prompts/{name}", get(detail))
        .route("/prompts/{name}/labels", post(set_label))
        .route("/prompts/{name}/labels/{label}/delete", post(delete_label))
}

fn no_store_404() -> Response {
    error_page(StatusCode::NOT_FOUND, Nav::Prompts, NO_STORE, None)
}

async fn list(State(state): State<AppState>) -> Response {
    let Some(store) = &state.store else {
        let body = html! {
            h1 { "Prompts" }
            p class="empty" { (NO_STORE) }
        };
        return layout::html(StatusCode::OK, layout::page("Prompts", Nav::Prompts, body));
    };
    match store.load_all().await {
        Ok(snap) => layout::html(StatusCode::OK, prompts::render(&prompt_rows(&snap))),
        Err(e) => store_error(&e),
    }
}

/// `?v=`, `?a=` and `?b=`; empty values count as absent.
#[derive(Debug, Default, Deserialize)]
struct DetailQuery {
    v: Option<String>,
    a: Option<String>,
    b: Option<String>,
}

fn bad_request(text: &str) -> Response {
    error_page(StatusCode::BAD_REQUEST, Nav::Prompts, text, None)
}

/// An absent or empty parameter is `Ok(None)`; anything but a u32 is a 400 page.
#[allow(clippy::result_large_err)] // the Err is the response the handler returns as is
fn version_param(key: &str, raw: Option<&str>) -> Result<Option<u32>, Response> {
    match raw.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => s
            .parse()
            .map(Some)
            .map_err(|_| bad_request(&format!("{key}: not a version number"))),
    }
}

/// What the detail page shows besides the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    /// The template and config of one version (`None`: the latest).
    Version(Option<u32>),
    /// The diff of `a` against `b`.
    Diff(u32, u32),
}

async fn detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DetailQuery>,
) -> Response {
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let show = match parse_show(&q) {
        Ok(show) => show,
        Err(resp) => return resp,
    };
    match store.load_all().await {
        Ok(snap) => detail_page(&state, &snap, &name, show, StatusCode::OK, None),
        Err(e) => store_error(&e),
    }
}

#[allow(clippy::result_large_err)] // the Err is the response the handler returns as is
fn parse_show(q: &DetailQuery) -> Result<Show, Response> {
    let v = version_param("v", q.v.as_deref())?;
    let a = version_param("a", q.a.as_deref())?;
    let b = version_param("b", q.b.as_deref())?;
    match (a, b) {
        (Some(a), Some(b)) => Ok(Show::Diff(a, b)),
        (None, None) => Ok(Show::Version(v)),
        _ => Err(bad_request("a diff needs both a and b")),
    }
}

/// Renders the detail page with `status`; an unknown prompt or version is a 404 page.
fn detail_page(
    state: &AppState,
    snap: &PromptSnapshot,
    name: &str,
    show: Show,
    status: StatusCode,
    error: Option<Markup>,
) -> Response {
    let not_found = |what: String| error_page(StatusCode::NOT_FOUND, Nav::Prompts, &what, None);
    let Some(d) = prompt_detail(snap, name) else {
        return not_found(format!("prompt {name:?} not found"));
    };
    let (view, diff) = match show {
        Show::Version(v) => {
            let v = v.unwrap_or(d.latest);
            match version_view(snap, name, v) {
                Some(view) => (Some(view), None),
                None => return not_found(format!("prompt {name:?} has no version {v}")),
            }
        }
        Show::Diff(a, b) => match diff_view(snap, name, a, b) {
            Some(diff) => (None, Some(diff)),
            None => return not_found(format!("prompt {name:?} has no version {a} or {b}")),
        },
    };
    layout::html(
        status,
        prompt::render(&d, view.as_ref(), diff.as_ref(), &state.csrf, error),
    )
}

/// After a failed label write: the detail page with the error banner and the R10 status,
/// or the plain error page when the prompt itself cannot be shown. An `Http` body in the
/// banner is cut to an excerpt of at most 1,024 bytes.
async fn write_failed(state: &AppState, name: &str, e: &StoreError) -> Response {
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let Ok(snap) = store.load_all().await else {
        return store_error(e);
    };
    if prompt_detail(&snap, name).is_none() {
        return store_error(e);
    }
    let error = banner(&store_text(e), None);
    detail_page(
        state,
        &snap,
        name,
        Show::Version(None),
        store_status(e),
        Some(error),
    )
}

fn written(name: &str) -> Response {
    Redirect::to(&prompt::href(name)).into_response()
}

/// The set-label form; missing fields are empty strings.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SetLabelForm {
    label: String,
    version: String,
    expected: String,
    csrf: String,
}

/// `version` (a u32) and `expected` (empty: the label must be unset; a u32: it must point
/// there), as `set_label` arguments.
fn set_args(form: &SetLabelForm) -> Result<(Label, u32, Option<Option<u32>>), StoreError> {
    let label = Label::new(&form.label)?;
    let version = form
        .version
        .parse()
        .map_err(|_| StoreError::Invalid(format!("version {:?}: not a number", form.version)))?;
    let expected = if form.expected.is_empty() {
        None
    } else {
        Some(form.expected.parse().map_err(|_| {
            StoreError::Invalid(format!("expected {:?}: not a number", form.expected))
        })?)
    };
    Ok((label, version, Some(expected)))
}

async fn set_label(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(form): Form<SetLabelForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let result = match set_args(&form) {
        Ok((label, version, expected)) => store.set_label(&name, &label, version, expected).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => written(&name),
        Err(e) => write_failed(&state, &name, &e).await,
    }
}

/// The delete form: only the token.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeleteForm {
    csrf: String,
}

async fn delete_label(
    State(state): State<AppState>,
    Path((name, label)): Path<(String, String)>,
    Form(form): Form<DeleteForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let result = match Label::new(&label) {
        Ok(label) => store.remove_label(&name, &label).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => written(&name),
        Err(e) => write_failed(&state, &name, &e).await,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use gadk_prompts::{FileStore, Label, NewVersion, PromptStore};
    use http::StatusCode;
    use http::header::LOCATION;
    use serde_json::json;

    use crate::web::AppState;
    use crate::web::testutil::{CSRF, FakeSource, get, post_form, state};

    const ORIGIN: &str = "http://127.0.0.1:8765";
    const NO_STORE: &str = "No prompt store configured (start with --store)";

    /// `greet` v1..v2 (production → 1, staging → 2), `summarize` v1 (a `<b>` template),
    /// `team/greet` v1.
    async fn seeded() -> (tempfile::TempDir, Arc<FileStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(dir.path()));
        let versions = [
            ("greet", "Hello {{name}}\n", None),
            (
                "greet",
                "Hello {{name}}!\nBye\n",
                Some(json!({"temperature": 0.2})),
            ),
            ("summarize", "Summarize <b>{{text}}</b>\n", None),
            ("team/greet", "Hi\n", None),
        ];
        for (name, template, config) in versions {
            store
                .create_version(NewVersion {
                    name: name.into(),
                    template: template.into(),
                    config,
                    author: "chris".into(),
                    message: format!("{name} change"),
                })
                .await
                .unwrap();
        }
        store
            .set_label("greet", &Label::production(), 1, None)
            .await
            .unwrap();
        store
            .set_label("greet", &Label::staging(), 2, None)
            .await
            .unwrap();
        (dir, store)
    }

    fn st(store: &Arc<FileStore>) -> AppState {
        let store: Arc<dyn PromptStore> = store.clone();
        state(Arc::new(FakeSource::new()), Some(store))
    }

    fn no_store() -> AppState {
        state(Arc::new(FakeSource::new()), None)
    }

    async fn labels(store: &FileStore, name: &str) -> BTreeMap<String, u32> {
        let snap = store.load_all().await.unwrap();
        snap.prompts[name]
            .labels
            .iter()
            .map(|(l, v)| (l.as_str().to_owned(), *v))
            .collect()
    }

    fn set_body(label: &str, version: &str, expected: &str) -> String {
        format!("label={label}&version={version}&expected={expected}&csrf={CSRF}")
    }

    #[tokio::test]
    async fn list_shows_every_prompt_with_latest_and_labels() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(r#"<a href="/prompts/greet">greet</a>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a href="/prompts/summarize">summarize</a>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a href="/prompts/team%2Fgreet">team/greet</a>"#),
            "{body}"
        );
        assert!(body.contains(r#"<td class="num">v2</td>"#), "{body}");
        assert!(
            body.contains(r#"<span class="chip">production → v1</span>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<span class="chip">staging → v2</span>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a class="active" href="/prompts">"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn without_a_store_the_list_explains_and_the_rest_is_404() {
        let (status, _, body) = get(no_store(), "/prompts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(NO_STORE), "{body}");

        let (status, _, body) = get(no_store(), "/prompts/greet").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains(NO_STORE), "{body}");

        let (status, _, _) = post_form(
            no_store(),
            "/prompts/greet/labels",
            &set_body("canary", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = post_form(
            no_store(),
            "/prompts/greet/labels/staging/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn detail_lists_versions_newest_first_with_labels() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        let v2 = body.find(r#"href="/prompts/greet?v=2""#).expect("v2 link");
        let v1 = body.find(r#"href="/prompts/greet?v=1""#).expect("v1 link");
        assert!(v2 < v1, "newest first: {body}");
        assert!(body.contains("greet change"), "{body}");
        assert!(body.contains("chris"), "{body}");
        assert!(
            body.contains(r#"<span class="chip">production</span>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<span class="chip">staging</span>"#),
            "{body}"
        );
        // No ?v= selects the latest version.
        assert!(body.contains("Hello {{name}}!"), "{body}");
    }

    #[tokio::test]
    async fn version_query_shows_template_and_config() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet?v=2").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h2>Version 2</h2>"), "{body}");
        assert!(
            body.contains("<pre class=\"template\">Hello {{name}}!\nBye\n</pre>"),
            "{body}"
        );
        assert!(
            body.contains("<pre class=\"config\">{\n  &quot;temperature&quot;: 0.2\n}</pre>"),
            "{body}"
        );

        let (status, _, body) = get(st(&store), "/prompts/greet?v=1").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("<pre class=\"template\">Hello {{name}}\n</pre>"),
            "{body}"
        );
        assert!(body.contains("No config"), "{body}");
    }

    #[tokio::test]
    async fn diff_query_renders_tagged_rows() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet?a=1&b=2").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("v1 → v2"), "{body}");
        assert!(body.contains(r#"<table class="diff">"#), "{body}");
        assert!(
            body.contains(
                r#"<tr class="diff-delete"><td class="ln">1</td><td class="ln"></td><td>-</td><td>Hello {{name}}</td></tr>"#
            ),
            "{body}"
        );
        assert!(
            body.contains(
                r#"<tr class="diff-insert"><td class="ln"></td><td class="ln">1</td><td>+</td><td>Hello {{name}}!</td></tr>"#
            ),
            "{body}"
        );
        assert!(
            body.contains(
                r#"<tr class="diff-insert"><td class="ln"></td><td class="ln">2</td><td>+</td><td>Bye</td></tr>"#
            ),
            "{body}"
        );
        // The config diff: nothing in v1, three pretty-printed lines in v2.
        assert!(
            body.contains(r#"<td>+</td><td>  &quot;temperature&quot;: 0.2</td>"#),
            "{body}"
        );
        // The picker preselects the compared versions.
        assert!(
            body.contains(r#"<select name="a"><option value="2">v2</option><option value="1" selected>v1</option></select>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn unknown_or_partial_queries_are_rejected() {
        let (_dir, store) = seeded().await;
        for (path, want) in [
            ("/prompts/nope", StatusCode::NOT_FOUND),
            ("/prompts/greet?v=9", StatusCode::NOT_FOUND),
            ("/prompts/greet?a=1&b=9", StatusCode::NOT_FOUND),
            ("/prompts/greet?a=9&b=1", StatusCode::NOT_FOUND),
            ("/prompts/greet?a=1", StatusCode::BAD_REQUEST),
            ("/prompts/greet?b=2", StatusCode::BAD_REQUEST),
            ("/prompts/greet?v=two", StatusCode::BAD_REQUEST),
            ("/prompts/greet?a=x&b=1", StatusCode::BAD_REQUEST),
        ] {
            let (status, _, body) = get(st(&store), path).await;
            assert_eq!(status, want, "{path}: {body}");
            assert!(
                body.contains(r#"<div class="banner error">"#),
                "{path}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn nested_names_use_an_encoded_slash() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/team%2Fgreet").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h1>team/greet</h1>"), "{body}");
        assert!(
            body.contains(r#"action="/prompts/team%2Fgreet/labels""#),
            "{body}"
        );

        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/team%2Fgreet/labels",
            &set_body("production", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/team%2Fgreet");
        assert_eq!(labels(&store, "team/greet").await["production"], 1);
    }

    #[tokio::test]
    async fn set_label_creates_a_new_label() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("canary", "2", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(labels(&store, "greet").await["canary"], 2);
    }

    #[tokio::test]
    async fn set_label_moves_when_expected_matches() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("production", "2", "1"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(labels(&store, "greet").await["production"], 2);
    }

    #[tokio::test]
    async fn stale_expected_is_409_on_the_detail_page() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("production", "2", "2"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        assert!(
            body.contains(
                r#"<div class="banner error"><p>conflict: expected v2, found v1</p></div>"#
            ),
            "{body}"
        );

        // An empty `expected` requires the label to be unset.
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("staging", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body.contains("conflict: expected unset, found v2"),
            "{body}"
        );
        let now = labels(&store, "greet").await;
        assert_eq!((now["production"], now["staging"]), (1, 2));
    }

    #[tokio::test]
    async fn invalid_label_or_version_is_400() {
        let (_dir, store) = seeded().await;
        for body in [
            set_body("latest", "1", ""),
            set_body("Bad+Label", "1", ""),
            set_body("canary", "two", ""),
            set_body("canary", "1", "one"),
        ] {
            let (status, _, html) =
                post_form(st(&store), "/prompts/greet/labels", &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(html.contains("<h1>greet</h1>"), "{html}");
            assert!(
                html.contains(r#"<div class="banner error"><p>invalid: "#),
                "{html}"
            );
        }
        assert_eq!(labels(&store, "greet").await.len(), 2);
    }

    #[tokio::test]
    async fn unknown_prompt_or_version_is_404() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/nope/labels",
            &set_body("canary", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("not found: nope"), "{body}");

        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("canary", "9", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        assert!(body.contains("not found: greet@v9"), "{body}");
    }

    #[tokio::test]
    async fn delete_removes_the_label() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels/staging/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        let now = labels(&store, "greet").await;
        assert!(!now.contains_key("staging"), "{now:?}");
        assert_eq!(now["production"], 1);

        // Gone already: 404 on the detail page; a bad name: 400.
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels/staging/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        let (status, _, _) = post_form(
            st(&store),
            "/prompts/greet/labels/Bad/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn wrong_or_missing_csrf_is_403_and_changes_nothing() {
        let (_dir, store) = seeded().await;
        let wrong = "b".repeat(64);
        let cases = [
            (
                "/prompts/greet/labels",
                format!("label=canary&version=1&expected=&csrf={wrong}"),
            ),
            (
                "/prompts/greet/labels",
                "label=canary&version=1&expected=".to_owned(),
            ),
            (
                "/prompts/greet/labels/staging/delete",
                format!("csrf={wrong}"),
            ),
            ("/prompts/greet/labels/staging/delete", String::new()),
        ];
        for (path, body) in cases {
            let (status, _, html) = post_form(st(&store), path, &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path} {body}");
            assert!(html.contains("csrf"), "{html}");
        }
        let now = labels(&store, "greet").await;
        assert_eq!(now.len(), 2);
        assert_eq!((now["production"], now["staging"]), (1, 2));
    }

    #[tokio::test]
    async fn template_text_is_escaped() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/summarize?v=1").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Summarize &lt;b&gt;{{text}}&lt;/b&gt;"),
            "{body}"
        );
        assert!(!body.contains("<b>{{text}}"), "{body}");
    }

    #[tokio::test]
    async fn every_post_form_carries_csrf_and_a_confirmation() {
        let (_dir, store) = seeded().await;
        let (_, _, body) = get(st(&store), "/prompts/greet").await;
        let forms: Vec<&str> = body
            .split("<form ")
            .skip(1)
            .map(|f| &f[..f.find("</form>").unwrap()])
            .collect();
        let posts: Vec<&&str> = forms
            .iter()
            .filter(|f| f.starts_with(r#"method="post""#))
            .collect();
        // production and staging each have a move and a delete form, plus the new-label form.
        assert_eq!(posts.len(), 5, "{body}");
        let hidden = format!(r#"<input type="hidden" name="csrf" value="{CSRF}">"#);
        for f in &posts {
            assert!(f.contains(&hidden), "{f}");
            assert!(f.contains("data-confirm=\""), "{f}");
        }
        assert!(
            body.contains(r#"action="/prompts/greet/labels/staging/delete" data-confirm="Remove label staging?""#),
            "{body}"
        );
        // The move form for production carries its current version as `expected`.
        assert!(
            body.contains(r#"<input type="hidden" name="label" value="production"><input type="hidden" name="expected" value="1">"#),
            "{body}"
        );
        // The new-label form carries an empty `expected`.
        assert!(
            body.contains(r#"<input type="hidden" name="expected" value="">"#),
            "{body}"
        );
        // The diff picker is the only other form, a GET without the token.
        assert_eq!(forms.len(), posts.len() + 1, "{body}");
        assert!(
            body.contains(r#"<form method="get" action="/prompts/greet""#),
            "{body}"
        );
    }

    /// Loads from a `FileStore`; every label write fails with a large `StoreError::Http`.
    struct HttpFailStore {
        inner: Arc<FileStore>,
        body: String,
    }

    #[async_trait::async_trait]
    impl PromptStore for HttpFailStore {
        async fn load_all(&self) -> Result<gadk_prompts::PromptSnapshot, gadk_prompts::StoreError> {
            self.inner.load_all().await
        }
        async fn create_version(
            &self,
            new: NewVersion,
        ) -> Result<gadk_prompts::PromptVersion, gadk_prompts::StoreError> {
            self.inner.create_version(new).await
        }
        async fn set_label(
            &self,
            _name: &str,
            _label: &Label,
            _version: u32,
            _expected: Option<Option<u32>>,
        ) -> Result<(), gadk_prompts::StoreError> {
            Err(gadk_prompts::StoreError::Http {
                status: 500,
                body: self.body.clone(),
            })
        }
        async fn remove_label(
            &self,
            _name: &str,
            _label: &Label,
        ) -> Result<(), gadk_prompts::StoreError> {
            Err(gadk_prompts::StoreError::Http {
                status: 500,
                body: self.body.clone(),
            })
        }
    }

    #[tokio::test]
    async fn store_http_error_banner_is_capped() {
        const MARKER: &str = "END-OF-UPSTREAM-BODY";
        let (_dir, inner) = seeded().await;
        let body = format!("{}{MARKER}", "x".repeat(2000));
        let store: Arc<dyn PromptStore> = Arc::new(HttpFailStore { inner, body });
        let state = state(Arc::new(FakeSource::new()), Some(store));
        for (path, form) in [
            ("/prompts/greet/labels", set_body("canary", "1", "")),
            (
                "/prompts/greet/labels/staging/delete",
                format!("csrf={CSRF}"),
            ),
        ] {
            let (status, _, html) = post_form(state.clone(), path, &form, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert!(!html.contains(MARKER), "{path}: {html}");
            assert!(html.contains("<h1>greet</h1>"), "{path}: the detail page");
            let start = html.find("<p>http 500: ").expect("http banner") + "<p>http 500: ".len();
            let len = html[start..].find("</p>").expect("banner end");
            assert!(len <= 1024, "{path}: banner body is {len} bytes");
            assert!(len > 0, "{path}");
        }
    }
}
