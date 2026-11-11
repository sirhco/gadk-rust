// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Port of ADK-Go `cmd/launcher/web/webui/webui_test.go` (Task 10).
//!
//! Go's in-process `httptest.NewRecorder` requests are [`WebRouter::serve`] calls; its
//! `httptest.NewServer` is the launcher's own server on an ephemeral port, and its `http.Client`
//! is reqwest (with a redirect policy recording the hops Go's `CheckRedirect` records).

use std::sync::{Arc, Mutex};

use axum::body::Body;
use http::{HeaderMap, Method, StatusCode};
use pretty_assertions::assert_eq;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::golog;
use crate::test_support::NoAgents;
use crate::web::webui::WebUiLauncher;
use crate::web::{self, WebRouter, WebSublauncher, build_base_router, testing};

/// Go `testBackendAddress`.
const TEST_BACKEND_ADDRESS: &str = "http://localhost:8080/api";

/// Go `assetRefPattern`: the src/href of every `<script>` and `<link>` in a document.
const ASSET_REF_PATTERN: &str = r#"(?is)<(?:script|link)\s[^>]*?(?:src|href)\s*=\s*"([^"]+)""#;

/// Go `docBaseHrefPattern`: the value of the `<base href>` tag.
const DOC_BASE_HREF_PATTERN: &str = r#"(?is)<base\s[^>]*?href\s*=\s*"([^"]*)""#;

/// Go's `nil` `*launcher.Config`: the web UI reads only its logger.
fn config() -> Config {
    let (log, _) = golog::capture();
    let mut config = Config::new(Arc::new(NoAgents));
    config.log = log;
    config
}

/// Go `newTestRouter`: the UI routes on a `StrictSlash(true)` router, as the web launcher builds
/// them.
fn new_test_router(path_prefix: &str) -> WebRouter {
    let mut router = WebRouter::new(true);
    let launcher = WebUiLauncher::with_path_prefix(path_prefix);
    launcher
        .setup_subrouters(&mut router, &mut config())
        .unwrap_or_else(|e| panic!("SetupSubrouters({path_prefix:?}) failed: {e}"));
    router
}

/// Go `request`: an in-process request against the router.
async fn request(
    router: &WebRouter,
    method: Method,
    target: &str,
) -> (StatusCode, HeaderMap, String) {
    let req = http::Request::builder()
        .method(method.clone())
        .uri(target)
        .body(Body::empty())
        .unwrap();
    let resp = router.serve(req).await;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_else(|e| panic!("{method} {target}: reading body failed: {e}"));
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

/// Go `assertAssetsResolve`: resolves every script and stylesheet reference of the document
/// against its `<base href>` (itself resolved against the document URL) and fetches it.
async fn assert_assets_resolve(router: &WebRouter, doc_url: &str, body: &str) {
    let doc = url::Url::parse(doc_url)
        .unwrap_or_else(|e| panic!("cannot parse document URL {doc_url:?}: {e}"));
    let mut base = doc.clone();
    if let Some(m) = Regex::new(DOC_BASE_HREF_PATTERN).unwrap().captures(body) {
        base = doc.join(&m[1]).unwrap_or_else(|e| {
            panic!(
                "cannot resolve <base href={:?}> against {doc_url:?}: {e}",
                &m[1]
            )
        });
    }

    let mut local = 0;
    let mut errors = Vec::new();
    for m in Regex::new(ASSET_REF_PATTERN).unwrap().captures_iter(body) {
        let reference = &m[1];
        if reference.starts_with("//") || reference.starts_with("data:") {
            continue;
        }
        let resolved = match base.join(reference) {
            Ok(u) => u,
            Err(e) => {
                errors.push(format!(
                    "{doc_url}: cannot resolve asset reference {reference:?}: {e}"
                ));
                continue;
            }
        };
        if (resolved.host_str(), resolved.port()) != (doc.host_str(), doc.port()) {
            // A third-party asset (fonts.gstatic.com) is not ours to serve.
            continue;
        }
        local += 1;
        let request_uri = match resolved.query() {
            Some(q) => format!("{}?{q}", resolved.path()),
            None => resolved.path().to_owned(),
        };
        let (status, _, _) = request(router, Method::GET, &request_uri).await;
        if status != StatusCode::OK {
            errors.push(format!(
                "{doc_url}: asset {reference:?} resolved to {request_uri}, GET returned {}, want 200",
                status.as_u16()
            ));
        }
    }
    // Guard against a document whose references the pattern failed to find.
    if local < 10 {
        errors.push(format!(
            "{doc_url}: found only {local} local asset references in the served document, want at least 10"
        ));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[tokio::test]
async fn test_deep_link_serves_app_with_working_assets() {
    let router = new_test_router("/ui/");

    for target in ["/ui/", "/ui/eval", "/ui/a/b/c"] {
        let (status, headers, body) = request(&router, Method::GET, target).await;
        assert_eq!(status, StatusCode::OK, "GET {target} status");
        let ct = header(&headers, "Content-Type");
        assert!(
            ct.contains("text/html"),
            "GET {target} Content-Type = {ct:?}, want text/html"
        );
        assert_assets_resolve(&router, &format!("http://example.com{target}"), &body).await;
    }
}

#[tokio::test]
async fn test_served_index_rewrites_base_href() {
    let router = new_test_router("/ui/");

    let (status, _, body) = request(&router, Method::GET, "/ui/a/b/c").await;
    assert_eq!(status, StatusCode::OK, "GET /ui/a/b/c status");
    assert!(
        !body.contains(r#"<base href="./">"#),
        r#"served document still contains the bundled <base href="./">, which resolves assets against the request directory"#
    );
    assert!(
        body.contains(r#"<base href="/ui/">"#),
        r#"served document does not contain <base href="/ui/">"#
    );
}

#[tokio::test]
async fn test_missing_asset_still404s() {
    let router = new_test_router("/ui/");

    // A missing asset must not be answered with index.html.
    for target in [
        "/ui/does-not-exist.js",
        "/ui/missing.css",
        "/ui/missing.svg",
        "/ui/missing.map",
        "/ui/missing.json",
        "/ui/missing.wasm",
        "/ui/missing.woff2",
        "/ui/deep/link/missing.js",
    ] {
        let (status, _, body) = request(&router, Method::GET, target).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {target} status");
        assert!(
            !body.contains("<html"),
            "GET {target} returned an HTML document instead of a plain 404"
        );
    }
}

#[tokio::test]
async fn test_bare_prefix_redirects_to_prefix() {
    let router = new_test_router("/ui/");

    let (status, headers, _) = request(&router, Method::GET, "/ui").await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY, "GET /ui status");
    assert_eq!(header(&headers, "Location"), "/ui/", "GET /ui Location");
}

/// A launcher-configured server for `router` on an ephemeral loopback port: its base URL, and
/// the token that stops it.
async fn start_server(router: WebRouter) -> (String, CancellationToken) {
    let listener = testing::listen("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let (log, _) = golog::capture();
    tokio::spawn(testing::serve(
        listener,
        router,
        web::new(Vec::new()).serve_options(),
        cancel.clone(),
        log,
    ));
    (format!("http://{addr}"), cancel)
}

// parity: adapted: Go's httptest server and CheckRedirect client are the launcher's server on an ephemeral port and a reqwest client whose redirect policy records the hops
#[tokio::test]
async fn test_bare_prefix_does_not_loop() {
    let (log, _) = golog::capture();
    let mut router = build_base_router(&log);
    let launcher = WebUiLauncher::with_path_prefix("/ui/");
    launcher
        .setup_subrouters(&mut router, &mut config())
        .unwrap_or_else(|e| panic!("SetupSubrouters failed: {e}"));
    let (server_url, cancel) = start_server(router).await;

    let hops: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&hops);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            recorded
                .lock()
                .unwrap()
                .push(attempt.url().path().to_owned());
            if attempt.previous().len() >= 5 {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .unwrap();

    for target in ["/ui", "/"] {
        hops.lock().unwrap().clear();
        let res = client
            .get(format!("{server_url}{target}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {target} failed: {e}"));
        let hops = hops.lock().unwrap().clone();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "GET {target} ended at {} after hops {hops:?}",
            res.url().path()
        );
        assert_eq!(
            res.url().path(),
            "/ui/",
            "GET {target} ended after hops {hops:?}"
        );
        assert!(
            hops.len() <= 2,
            "GET {target} took {} redirects ({hops:?}), which looks like a loop",
            hops.len()
        );
    }
    cancel.cancel();
}

// parity: adapted: Go's httptest server is the launcher's server on an ephemeral port (it strips a HEAD response's body, as net/http does)
#[tokio::test]
async fn test_head_is_allowed() {
    let (server_url, cancel) = start_server(new_test_router("/ui/")).await;
    let client = reqwest::Client::new();

    for target in [
        "/ui/",
        "/ui/eval",
        "/ui/adk_favicon.svg",
        "/ui/assets/config/runtime-config.json",
    ] {
        let res = client
            .head(format!("{server_url}{target}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("HEAD {target} failed: {e}"));
        let status = res.status();
        let body = res
            .bytes()
            .await
            .unwrap_or_else(|e| panic!("HEAD {target}: reading body failed: {e}"));
        assert_eq!(status, StatusCode::OK, "HEAD {target} status");
        assert_eq!(
            body.len(),
            0,
            "HEAD {target} returned a {}-byte body, want none",
            body.len()
        );
    }
    cancel.cancel();
}

#[tokio::test]
async fn test_directory_listing_is_suppressed() {
    let router = new_test_router("/ui/");

    let (status, _, body) = request(&router, Method::GET, "/ui/assets/config/").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "GET /ui/assets/config/ status"
    );
    assert!(
        !body.contains("runtime-config.json"),
        "GET /ui/assets/config/ body links to runtime-config.json, so it is a directory listing:\n{body}"
    );
}

#[tokio::test]
async fn test_runtime_config_is_generated() {
    let router = new_test_router("/ui/");

    let (status, _, body) = request(
        &router,
        Method::GET,
        "/ui/assets/config/runtime-config.json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET runtime-config.json status");
    let got: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("runtime-config.json is not valid JSON ({e}): {body}"));
    assert_eq!(
        got["backendUrl"], TEST_BACKEND_ADDRESS,
        "runtime-config.json backendUrl"
    );
}

#[tokio::test]
async fn test_root_redirects_to_prefix() {
    let router = new_test_router("/ui/");

    let (status, headers, _) = request(&router, Method::GET, "/").await;
    assert_eq!(status, StatusCode::FOUND, "GET / status");
    assert_eq!(header(&headers, "Location"), "/ui/", "GET / Location");
}

/// Checks that nothing hardcodes "/ui/".
#[tokio::test]
async fn test_custom_path_prefix() {
    const PREFIX: &str = "/adk-console/";
    let router = new_test_router(PREFIX);

    let (status, _, body) = request(&router, Method::GET, &format!("{PREFIX}a/b/c")).await;
    assert_eq!(status, StatusCode::OK, "GET {PREFIX}a/b/c status");
    assert!(
        body.contains(&format!(r#"<base href="{PREFIX}">"#)),
        "served document does not contain <base href={PREFIX:?}>"
    );
    assert_assets_resolve(&router, &format!("http://example.com{PREFIX}a/b/c"), &body).await;

    let (status, headers, _) = request(&router, Method::GET, "/adk-console").await;
    assert_eq!(
        status,
        StatusCode::MOVED_PERMANENTLY,
        "GET /adk-console status"
    );
    assert_eq!(
        header(&headers, "Location"),
        PREFIX,
        "GET /adk-console Location"
    );

    let (_, headers, _) = request(&router, Method::GET, "/").await;
    assert_eq!(header(&headers, "Location"), PREFIX, "GET / Location");

    let (status, _, body) = request(
        &router,
        Method::GET,
        &format!("{PREFIX}assets/config/runtime-config.json"),
    )
    .await;
    assert!(
        status == StatusCode::OK && body.contains("backendUrl"),
        "GET {PREFIX}assets/config/runtime-config.json = {status} {body:?}, want 200 with backendUrl"
    );

    let (status, _, _) = request(&router, Method::GET, &format!("{PREFIX}missing.js")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "GET {PREFIX}missing.js status"
    );

    // The default prefix must not be served at all under a custom one.
    let (status, _, _) = request(&router, Method::GET, "/ui/").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "GET /ui/ status with prefix {PREFIX:?}"
    );
}
