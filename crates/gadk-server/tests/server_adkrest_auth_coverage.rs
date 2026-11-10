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

//! Port of ADK-Go `server/adkrest/auth_coverage_test.go`: every route the server registers
//! answers 401 to a request without credentials, except the allowlisted public endpoints.

use std::collections::BTreeSet;
use std::sync::Arc;

use gadk_server::authn;
use gadk_server::rest::testutil::wire_scenario::server_routes;
use gadk_server::rest::testutil::{serve_http, single_agent_loader};
use gadk_server::rest::{DebugApiConfig, Server, ServerConfig};
use gadk_session::InMemorySessionService;
use http::{Method, StatusCode};

/// Go `publicEndpoints`: the path templates intentionally reachable without authentication.
const PUBLIC_ENDPOINTS: [&str; 2] = ["/health", "/version"];

/// Go `concretePath`: every `{var}` segment of a path template replaced by `x`.
fn concrete_path(template: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in template.chars() {
        match c {
            '{' => {
                if depth == 0 {
                    out.push('x');
                }
                depth += 1;
            }
            '}' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Go `primaryMethod`: a verb the route serves, preferring one other than HEAD and OPTIONS.
fn primary_method(methods: &[Method]) -> Method {
    methods
        .iter()
        .find(|m| **m != Method::HEAD && **m != Method::OPTIONS)
        .or(methods.first())
        .cloned()
        .unwrap_or(Method::GET)
}

#[tokio::test]
async fn test_all_endpoints_require_authentication() {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(single_agent_loader("test-app"));
    // A request without the named header is answered 401 before the handler runs.
    cfg.authenticator = Some(authn::header("non-existing"));
    // Opt into the debug API so its routes are covered too.
    cfg.debug_api_config = DebugApiConfig {
        include_debug_api: true,
    };
    let srv = Server::new(cfg).unwrap();

    let mut seen_public = BTreeSet::new();
    let mut protected = 0;
    let mut failures = Vec::new();
    for route in server_routes(&srv) {
        let method = primary_method(&route.methods);
        let path = concrete_path(&route.pattern);
        // The request deliberately carries no credentials.
        let (code, _) = serve_http(&srv, method.clone(), &path).await;
        if PUBLIC_ENDPOINTS.contains(&route.pattern.as_str()) {
            seen_public.insert(route.pattern.clone());
            if code == StatusCode::UNAUTHORIZED {
                failures.push(format!(
                    "{method} {} is allowlisted as public but returned 401",
                    route.pattern
                ));
            }
            continue;
        }
        protected += 1;
        if code != StatusCode::UNAUTHORIZED {
            failures.push(format!(
                "{method} {} returned {code} without credentials, want 401",
                route.pattern
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Guard against the walk finding nothing.
    assert!(protected > 0, "no protected routes were walked");
    // Keep the allowlist honest: every entry must still be a real route.
    for tmpl in PUBLIC_ENDPOINTS {
        assert!(
            seen_public.contains(tmpl),
            "public endpoint {tmpl:?} was not found among the server's routes"
        );
    }
}
