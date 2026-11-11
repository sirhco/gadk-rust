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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go `server/adkrest/uicontract_test.go`: every endpoint the ADK web UI calls reaches a
//! handler on a real server (half 1), and the golden list is still in step with the Angular
//! bundle gadk-launcher vendors byte for byte from ADK-Go (half 2). Half 2 reads the bundle from
//! disk (`../gadk-launcher/webui/distr`, as Go's `readUIBundle` reads a relative path), so
//! gadk-server takes no dependency on gadk-launcher.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use axum::body::Body;
use gadk_agent::{Context, SingleLoader};
use gadk_artifact::InMemoryArtifactService;
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::workflow::{NodeConfig, START, chain, function_node};
use gadk_memory::InMemoryMemoryService;
use gadk_server::rest::{DebugApiConfig, Server, ServerConfig};
use gadk_session::InMemorySessionService;
use http::{Method, StatusCode};
use regex::Regex;
use tower::ServiceExt;

/// Go `uiEndpoint`: one request the ADK web UI makes.
struct UiEndpoint {
    method: Method,
    /// The URL path as the UI builds it, with named placeholders.
    template: &'static str,
    /// Why an entry needs explaining (optional).
    note: &'static str,
    /// The one endpoint the UI opens as a WebSocket.
    web_socket: bool,
}

const fn ep(method: Method, template: &'static str) -> UiEndpoint {
    UiEndpoint {
        method,
        template,
        note: "",
        web_socket: false,
    }
}

/// Go `uiGolden`: every endpoint the embedded ADK web UI calls (39 method/template pairs).
fn ui_golden() -> Vec<UiEndpoint> {
    vec![
        // Endpoints the UI calls without the developer prefix.
        ep(Method::POST, "/run_sse"),
        ep(Method::GET, "/list-apps"),
        ep(Method::GET, "/version"),
        ep(Method::GET, "/apps/{app_name}/users/{user_id}/sessions"),
        ep(Method::POST, "/apps/{app_name}/users/{user_id}/sessions"),
        ep(
            Method::GET,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
        ),
        UiEndpoint {
            note: "the UI's updateSession(), which it uses to rename a session and to write session state",
            ..ep(
                Method::PATCH,
                "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
            )
        },
        ep(
            Method::DELETE,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
        ),
        ep(
            Method::GET,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/artifacts/{artifact_name}",
        ),
        ep(
            Method::GET,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/artifacts/{artifact_name}/versions/{version}",
        ),
        UiEndpoint {
            web_socket: true,
            note: "opened as a WebSocket; a plain GET cannot complete the handshake, so this only checks that the route exists",
            ..ep(Method::GET, "/run_live")
        },
        // Developer endpoints, under /dev/apps/{app_name} since ADK v2.
        ep(Method::GET, "/dev/apps/{app_name}/build_graph"),
        ep(Method::GET, "/dev/apps/{app_name}/build_graph_image"),
        ep(Method::GET, "/dev/apps/{app_name}/builder"),
        ep(Method::POST, "/dev/apps/{app_name}/builder/save"),
        ep(Method::POST, "/dev/apps/{app_name}/builder/cancel"),
        ep(Method::GET, "/dev/apps/{app_name}/eval_sets"),
        UiEndpoint {
            note: "the UI creates an eval set at the hyphenated spelling and reads it back at the underscored one",
            ..ep(Method::POST, "/dev/apps/{app_name}/eval-sets")
        },
        ep(Method::GET, "/dev/apps/{app_name}/eval_sets/{eval_set_id}"),
        ep(
            Method::DELETE,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}",
        ),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals",
        ),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        ep(
            Method::PUT,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        ep(
            Method::DELETE,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        ep(
            Method::POST,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/add_session",
        ),
        ep(
            Method::POST,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/run_eval",
        ),
        ep(Method::GET, "/dev/apps/{app_name}/eval_results"),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/eval_results/{eval_result_id}",
        ),
        ep(Method::GET, "/dev/apps/{app_name}/metrics-info"),
        ep(Method::GET, "/dev/apps/{app_name}/debug/trace/{event_id}"),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/debug/trace/session/{session_id}",
        ),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph",
        ),
        ep(Method::GET, "/dev/apps/{app_name}/tests"),
        ep(Method::GET, "/dev/apps/{app_name}/tests/{test_name}"),
        ep(Method::PUT, "/dev/apps/{app_name}/tests/{test_name}"),
        ep(Method::DELETE, "/dev/apps/{app_name}/tests/{test_name}"),
        ep(Method::POST, "/dev/apps/{app_name}/tests/rebuild"),
        ep(Method::POST, "/dev/apps/{app_name}/tests/run"),
    ]
}

/// Go `uiContractApp`.
const UI_CONTRACT_APP: &str = "ui_contract_agent";

/// Go `uiPathValues`. `{test_name}` avoids `rebuild` and `run`, which are literal sibling
/// routes.
fn ui_path_value(name: &str) -> Option<&'static str> {
    Some(match name {
        "app_name" => UI_CONTRACT_APP,
        "user_id" => "ui-contract-user",
        "session_id" => "ui-contract-session",
        "artifact_name" => "ui-contract-artifact",
        "version" => "0",
        "event_id" => "ui-contract-event",
        "eval_set_id" => "ui-contract-eval-set",
        "eval_case_id" => "ui-contract-eval-case",
        "eval_result_id" => "ui-contract-eval-result",
        "test_name" => "ui-contract-test",
        _ => return None,
    })
}

/// Go `concretePath`: every `{name}` placeholder replaced by its value; an unknown placeholder
/// is a test bug.
fn concrete_path(template: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}').unwrap() + open;
        let name = &rest[open + 1..close];
        let value = ui_path_value(name).unwrap_or_else(|| {
            panic!(
                "golden template {template:?} uses placeholder {{{name}}} with no value; add one"
            )
        });
        out.push_str(&rest[..open]);
        out.push_str(value);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Go `muxNotFoundBody`: the only 404 that means "no route matched".
const MUX_NOT_FOUND_BODY: &str = "404 page not found";

/// Go `isMuxNotFound`.
fn is_mux_not_found(status: StatusCode, body: &str) -> bool {
    status == StatusCode::NOT_FOUND && body.trim() == MUX_NOT_FOUND_BODY
}

/// Go `noteSuffix`.
fn note_suffix(endpoint: &UiEndpoint) -> String {
    if endpoint.note.is_empty() {
        String::new()
    } else {
        format!("\nNote: {}", endpoint.note)
    }
}

/// Go `newUIContractServer`: in-memory services, a one-node workflow agent, and the debug API
/// on (three of the UI's endpoints are debug-trace routes).
fn new_ui_contract_server() -> axum::Router {
    let echo = function_node::<String, String, _, _>(
        "echo",
        |_ctx: Context, input: String| async move { Ok(input) },
        NodeConfig::default(),
    );
    let root = workflow_agent::new(WorkflowAgentConfig {
        name: UI_CONTRACT_APP.into(),
        edges: chain(&[Arc::clone(&START), echo]),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::new()));
    cfg.memory_service = Some(Arc::new(InMemoryMemoryService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    cfg.debug_api_config = DebugApiConfig {
        include_debug_api: true,
    };
    Server::new(cfg).unwrap().router()
}

/// Half 1: every endpoint the web UI calls reaches a handler. Only 405 and the router's
/// plain-text 404 mean the request never reached one; 200, 400, 500 and 501 all come from a
/// route that exists.
#[tokio::test]
async fn test_ui_contract_every_ui_endpoint_routes() {
    let server = new_ui_contract_server();
    for endpoint in ui_golden() {
        let mut path = concrete_path(endpoint.template);
        let name = format!("{} {}", endpoint.method, endpoint.template);
        if endpoint.web_socket {
            // The UI sends these as query parameters; the handshake cannot complete here, the
            // point is only that the request reaches the handler.
            path.push_str(&format!(
                "?app_name={UI_CONTRACT_APP}&user_id=u&session_id=s"
            ));
        }
        let req = http::Request::builder()
            .method(endpoint.method.clone())
            .uri(&path)
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = server.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let allow = resp
            .headers()
            .get("Allow")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{name}: {} {path}: server answered 405 Method Not Allowed, so the path is routed but not for this method.\nThe web UI makes this exact request, so it is broken in the browser.\nAllow header: {allow:?}{}",
            endpoint.method,
            note_suffix(&endpoint)
        );
        assert!(
            !is_mux_not_found(status, &body),
            "{name}: {} {path}: server answered the router's {:?}, so no route matches this path at all.\nThe web UI makes this exact request, so it is broken in the browser.{}",
            endpoint.method,
            body.trim(),
            note_suffix(&endpoint)
        );
    }
}

/// Go `placeholderRE`: one `{name}` placeholder in a golden template.
static PLACEHOLDER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[a-z_]+\}").unwrap());

/// Go `canonicalTemplate`: the query string dropped and every placeholder collapsed to `{}` (the
/// bundle is minified, so only the shape of a path is stable).
fn canonical_template(tmpl: &str) -> String {
    let path = tmpl.split_once('?').map_or(tmpl, |(p, _)| p);
    PLACEHOLDER_RE.replace_all(path, "{}").into_owned()
}

/// Go `bundleDir`: the web UI bundle gadk-launcher embeds, relative to this package.
fn bundle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../gadk-launcher/webui/distr")
}

/// Go `bundleCallSite`: one family of API calls in the Angular bundle. `min_matches` is a floor
/// so a bundle refresh that changes how URLs are built fails loudly; `fixed_method` is the verb
/// of a site whose method is not written next to the URL.
struct BundleCallSite {
    name: &'static str,
    pattern: Regex,
    min_matches: usize,
    fixed_method: Option<&'static str>,
}

/// Go `urlChars`: the body of a URL inside a template literal (Go's `\w` is ASCII).
const URL_CHARS: &str = r"(?:[0-9A-Za-z_\-./?=&]|\$\{[^{}`]*\})*";

/// Go `bundleCallSites`: the three shapes the UI builds an XHR URL in, plus the WebSocket URL.
fn bundle_call_sites() -> Vec<BundleCallSite> {
    vec![
        BundleCallSite {
            // `${this.apiServerDomain}/dev/apps/${A}/tests`
            name: "template literal interpolating apiServerDomain",
            pattern: Regex::new(&format!(
                r"`\$\{{[0-9A-Za-z_$.]+\.apiServerDomain\}}({URL_CHARS})`"
            ))
            .unwrap(),
            min_matches: 5,
            fixed_method: None,
        },
        BundleCallSite {
            // this.apiServerDomain+`/dev/apps/${A}/eval_sets`
            name: "apiServerDomain concatenated with a template literal",
            pattern: Regex::new(&format!(r"\.apiServerDomain\s*\+\s*`({URL_CHARS})`")).unwrap(),
            min_matches: 28,
            fixed_method: None,
        },
        BundleCallSite {
            // this.apiServerDomain+"/run_sse"
            name: "apiServerDomain concatenated with a string literal",
            pattern: Regex::new(r#"\.apiServerDomain\s*\+\s*"([0-9A-Za-z_\-./?=&]*)""#).unwrap(),
            min_matches: 3,
            fixed_method: None,
        },
        BundleCallSite {
            // `ws://${Ur.getWSServerUrl()}/run_live?app_name=...`
            name: "WebSocket URL built from getWSServerUrl",
            pattern: Regex::new(&format!(
                r"\$\{{[0-9A-Za-z_$.]+\.getWSServerUrl\(\)\}}({URL_CHARS})`"
            ))
            .unwrap(),
            min_matches: 1,
            fixed_method: Some("GET"),
        },
    ]
}

/// Go `minBundleTemplates`: a floor on the distinct templates extracted.
const MIN_BUNDLE_TEMPLATES: usize = 10;

/// Go `verbRE`: the HTTP verb a call site uses, just after the URL literal.
static VERB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.http\.(get|post|put|patch|delete)\(|method:\s*"(GET|POST|PUT|PATCH|DELETE)""#)
        .unwrap()
});

/// Go `verbWindow`: how far after a URL literal the verb may appear.
const VERB_WINDOW: usize = 260;

/// Go `verbAfterURL`: the HTTP verb of the URL literal ending at byte `end`.
fn verb_after_url(source: &str, end: usize) -> Option<String> {
    let window = &source.as_bytes()[end..(end + VERB_WINDOW).min(source.len())];
    // The window may cut a multi-byte character; the verbs are ASCII.
    let window = String::from_utf8_lossy(window);
    let caps = VERB_RE.captures(&window)?;
    match caps.get(1) {
        Some(m) => Some(m.as_str().to_ascii_uppercase()),
        None => caps.get(2).map(|m| m.as_str().to_owned()),
    }
}

/// Go `readUIBundle`: the contents and path of the bundle's one `main-*.js`.
fn read_ui_bundle() -> (String, PathBuf) {
    let dir = bundle_dir();
    let matches: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {} failed: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("main-") && n.ends_with(".js"))
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "found {} files matching {}/main-*.js, want exactly 1: {matches:?}\nThe embedded ADK web UI ships one main bundle. Zero means the bundle is missing and this test cannot check anything; more than one means stale bundles are being shipped.",
        matches.len(),
        dir.display()
    );
    let content = std::fs::read_to_string(&matches[0]).unwrap_or_else(|e| {
        panic!(
            "reading the embedded UI bundle {} failed: {e}",
            matches[0].display()
        )
    });
    (content, matches[0].clone())
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Go `extractBundleTemplates`: the endpoints the bundle calls, as a set of methods per canonical
/// path template.
fn extract_bundle_templates(source: &str, path: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let interpolation_re = Regex::new(r"\$\{[^{}]*\}").unwrap();
    let mut templates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut total = 0;
    for site in bundle_call_sites() {
        let found: Vec<regex::Captures<'_>> = site.pattern.captures_iter(source).collect();
        assert!(
            found.len() >= site.min_matches,
            "the {} pattern matched {} call site(s) in {}, want at least {}.\nThe bundle has changed how it builds URLs, so this test is no longer reading it. Repair the pattern in bundleCallSites before trusting any result from this file.\nPattern: {}",
            site.name,
            found.len(),
            base_name(path),
            site.min_matches,
            site.pattern
        );
        for caps in found {
            let url = caps.get(1).unwrap().as_str();
            let raw = url.split_once('?').map_or(url, |(p, _)| p);
            let template = interpolation_re.replace_all(raw, "{}").into_owned();
            let method = match site.fixed_method {
                Some(m) => m.to_owned(),
                None => verb_after_url(source, caps.get(0).unwrap().end()).unwrap_or_else(|| {
                    panic!(
                        "could not find the HTTP verb for the {} call site building {template:?} in {}.\nThe bundle has changed how it issues requests, so this test can no longer tell a GET from a PUT and would stop catching a verb change. Repair verbRE or verbWindow.",
                        site.name,
                        base_name(path)
                    )
                }),
            };
            templates.entry(template).or_default().insert(method);
            total += 1;
        }
    }
    assert!(
        templates.len() >= MIN_BUNDLE_TEMPLATES,
        "extracted only {} distinct API path template(s) from {}, want at least {MIN_BUNDLE_TEMPLATES}.\nAn extraction this thin is not trustworthy; treat it as a broken test, not a passing one.",
        templates.len(),
        base_name(path)
    );
    eprintln!(
        "extracted {} distinct API path templates from {total} call sites in {}",
        templates.len(),
        base_name(path)
    );
    templates
}

/// Half 2: the golden list is still in step with the bundle actually shipped.
#[test]
fn test_ui_contract_golden_list_matches_bundle() {
    let (source, bundle_path) = read_ui_bundle();
    let bundle_templates = extract_bundle_templates(&source, &bundle_path);

    let mut golden: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for endpoint in ui_golden() {
        golden
            .entry(canonical_template(endpoint.template))
            .or_default()
            .insert(endpoint.method.to_string());
    }

    let mut errors = Vec::new();
    // bundle_calls_are_all_in_the_golden_list
    for (tmpl, methods) in &bundle_templates {
        let Some(known) = golden.get(tmpl) else {
            errors.push(format!(
                "the embedded web UI bundle calls {tmpl:?} (in {}), which the golden list does not know about.\nThis is the 2026-06-30 failure mode: the bundle was refreshed and now addresses an endpoint the server does not route, so that part of the UI 404s in the browser with nothing to show for it.\nFix, in order: (1) register the route in the REST server's routes, (2) add the matching entry to ui_golden in this file.",
                base_name(&bundle_path)
            ));
            continue;
        };
        for method in methods {
            if !known.contains(method) {
                // A verb change is the same failure as a new path: the request 405s.
                errors.push(format!(
                    "the bundle now calls {method} {tmpl:?}, a method the golden list does not have (it has {known:?}).\nThe UI changed the verb it uses on an endpoint that already existed, so this request 405s in the browser.\nFix, in order: (1) add {method} to that route's methods in the REST server, (2) update ui_golden here."
                ));
            }
        }
    }
    // golden_entries_are_all_still_called_by_the_bundle
    for (tmpl, methods) in &golden {
        let Some(called) = bundle_templates.get(tmpl) else {
            errors.push(format!(
                "the golden list has {tmpl:?} (methods {methods:?}) but the shipped bundle {} never calls it.\nEither the UI dropped the endpoint and the entry is now stale, or the extraction regexes no longer see how the UI builds it.\nFix: confirm against the bundle, then either drop the entry or repair bundle_call_sites.",
                base_name(&bundle_path)
            ));
            continue;
        };
        for method in methods {
            if !called.contains(method) {
                errors.push(format!(
                    "the golden list has {method} {tmpl:?} but the shipped bundle never issues that verb against it (it issues {called:?}).\nFix: confirm against the bundle, then either drop the entry or repair bundle_call_sites."
                ));
            }
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n\n"));
}
