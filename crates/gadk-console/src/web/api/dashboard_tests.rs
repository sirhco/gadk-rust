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

use std::collections::BTreeSet;
use std::sync::Arc;

use http::StatusCode;

use super::tests::api_get;
use crate::web::testutil::{FakeSource, state};

/// The example dashboard's script. Test-only, so publishing the crate does not need the file.
const APP_JS: &str = include_str!("../../../../../examples/console-dashboard/app.js");

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
        let end = literal
            .find('"')
            .expect("a /api/v1/ string literal without its closing quote");
        paths.insert(literal[..end].to_string());
        rest = &literal[end + 1..];
    }
    paths
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

#[tokio::test]
async fn every_api_path_of_the_dashboard_is_a_console_route() {
    let paths = api_paths(APP_JS);
    for template in &paths {
        let probe = probe_path(template);
        let (status, _, body) = api_get(state(Arc::new(FakeSource::new()), None), &probe).await;
        assert_eq!(
            (status, body["error"]["kind"].as_str()),
            (StatusCode::NOT_FOUND, Some("needs_bigquery")),
            "app.js calls {template} ({probe}), which is not a console API route: {body}"
        );
    }
    let expected: BTreeSet<String> = ROUTES.iter().map(|r| r.to_string()).collect();
    assert_eq!(paths, expected, "the /api/v1/ string literals of app.js");
}

#[test]
fn the_dashboard_never_builds_html_from_strings() {
    for needle in FORBIDDEN {
        assert!(!APP_JS.contains(needle), "app.js contains {needle:?}");
    }
}
