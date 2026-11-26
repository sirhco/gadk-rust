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

//! Guards of the example dashboard in `examples/console-dashboard/app.js`: every API path it
//! calls is a console route, and it never builds HTML from strings.
//!
//! The script lives outside this crate, so `Cargo.toml` leaves this file out of the published
//! package (`exclude`); it runs only in the repository.

use std::collections::BTreeSet;

use gadk_console::{ConsoleConfig, bind};
use serde_json::Value;

/// The example dashboard's script.
const APP_JS: &str = include_str!("../../../examples/console-dashboard/app.js");

/// The API paths the dashboard must call, as written in its `ROUTES`.
const ROUTES: [&str; 7] = [
    "/api/v1/usage/breakdown",
    "/api/v1/usage/trend",
    "/api/v1/usage/top-traces",
    "/api/v1/sessions",
    "/api/v1/sessions/{id}/turns",
    "/api/v1/users",
    "/api/v1/review/pending",
];

/// The strings `app.js` must not contain: HTML parsing sinks, `eval` and the `fetch`
/// credentials option.
const FORBIDDEN: [&str; 6] = [
    "innerHTML",
    "outerHTML",
    "insertAdjacentHTML",
    "document.write",
    "eval(",
    "credentials",
];

/// Every string literal in `src` that starts with `/api/v1/`, up to its closing quote.
fn api_paths(src: &str) -> BTreeSet<String> {
    const OPEN: &str = "\"/api/v1/";
    let mut paths = BTreeSet::new();
    let mut rest = src;
    while let Some(at) = rest.find(OPEN) {
        let literal = &rest[at + 1..];
        let Some(end) = literal.find('"') else {
            panic!("a /api/v1/ string literal without its closing quote");
        };
        paths.insert(literal[..end].to_string());
        rest = &literal[end + 1..];
    }
    paths
}

/// Each `/api/v1/` path in `src` that opens a single-quoted or a template literal, with up to
/// 20 characters of text on each side. [`api_paths`] reads only double-quoted literals, so such
/// a path would escape the route check.
fn stray_api_literals(src: &str) -> Vec<String> {
    let mut found = Vec::new();
    for open in ["'/api/v1/", "`/api/v1/"] {
        for (at, _) in src.match_indices(open) {
            let before: Vec<char> = src[..at].chars().rev().take(20).collect();
            let before: String = before.into_iter().rev().collect();
            let after: String = src[at..].chars().take(open.len() + 20).collect();
            found.push(format!("{before}{after}"));
        }
    }
    found
}

#[test]
fn stray_api_literals_finds_single_quoted_and_template_paths() {
    let src = "a('/api/v1/users'); b(`/api/v1/x/${id}`); c(`${ROUTES.sessions}?`); \
               d(\"/api/v1/review/pending\");";
    let found = stray_api_literals(src);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].contains("'/api/v1/users'"), "{found:?}");
    assert!(found[1].contains("`/api/v1/x/${id}`"), "{found:?}");
}

#[test]
fn every_api_path_of_the_dashboard_is_double_quoted() {
    let stray = stray_api_literals(APP_JS);
    assert!(
        stray.is_empty(),
        "app.js has /api/v1/ paths outside double quotes, which the route check cannot see: {stray:?}"
    );
}

/// `template` as a request path: each `{…}` placeholder becomes `x` and the query is dropped.
fn probe_path(template: &str) -> String {
    let path = template.split('?').next().unwrap_or(template);
    let mut out = String::with_capacity(path.len());
    let mut in_placeholder = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_placeholder = true;
                out.push('x');
            }
            '}' => in_placeholder = false,
            _ if in_placeholder => {}
            _ => out.push(c),
        }
    }
    out
}

#[test]
fn probe_paths_fill_placeholders_and_drop_the_query() {
    assert_eq!(
        probe_path("/api/v1/sessions/{id}/turns"),
        "/api/v1/sessions/x/turns"
    );
    assert_eq!(probe_path("/api/v1/users?range=7d"), "/api/v1/users");
    assert_eq!(
        api_paths("const a = \"/api/v1/users\"; f(\"/api/v1/x/{id}\");"),
        BTreeSet::from(["/api/v1/users".to_string(), "/api/v1/x/{id}".to_string()])
    );
}

/// Every `/api/v1/` path of `app.js`, requested from a bound console without the BigQuery
/// source, answers the API's `needs_bigquery` 404, which only a real route gives (an unknown
/// path is `not_found`). Binding reads no GCP: the Cloud Trace source loads credentials on its
/// first call, and no request here reaches it.
#[tokio::test]
async fn every_api_path_of_the_dashboard_is_a_console_route() {
    let bound = bind(ConsoleConfig {
        project: "dashboard-test".into(),
        store: None,
        reports: None,
        port: 0,
        log_id: "gadk".into(),
    })
    .await
    .expect("bind the console");
    let addr = bound.local_addr();
    let server = tokio::spawn(bound.serve());
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("an HTTP client");

    let paths = api_paths(APP_JS);
    for template in &paths {
        let probe = probe_path(template);
        let resp = client
            .get(format!("http://{addr}{probe}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {probe}: {e}"));
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or_else(|e| panic!("{probe}: {e}"));
        assert_eq!(
            (status, body["error"]["kind"].as_str()),
            (404, Some("needs_bigquery")),
            "app.js calls {template} ({probe}), which is not a console API route: {body}"
        );
    }
    server.abort();
    let expected: BTreeSet<String> = ROUTES.iter().map(|r| r.to_string()).collect();
    assert_eq!(paths, expected, "the /api/v1/ string literals of app.js");
}

#[test]
fn the_dashboard_never_builds_html_from_strings() {
    for needle in FORBIDDEN {
        assert!(!APP_JS.contains(needle), "app.js contains {needle:?}");
    }
}
