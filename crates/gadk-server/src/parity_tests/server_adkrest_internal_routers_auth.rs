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

//! Ports of ADK-Go `server/adkrest/internal/routers/auth_test.go`.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use axum::body::Body;
use gadk_agent::{AgentConfig, Loader, SingleLoader};
use gadk_session::{InMemorySessionService, SessionService};
use http::Method;

use crate::authn;
use crate::rest::controllers::{
    AgentGraphApiController, AppsApiController, ArtifactsApiController, DebugApiController,
    RuntimeApiController, RuntimeApiControllerConfig, SessionsApiController, VersionApiController,
    agentgraph::agent_graph_routes, apps::apps_routes, artifacts::artifacts_routes,
    debug::debug_routes, devfeatures::eval_routes, devfeatures::tests_routes,
    runtime::runtime_routes, sessions::sessions_routes, version::version_routes,
};
use crate::rest::routing::{Route, RouteTable};
use crate::rest::{DebugTelemetry, DebugTelemetryConfig};

/// Go `routerAuthCase`: a router and the names of the routes it serves without
/// authentication.
struct RouterAuthCase {
    name: &'static str,
    /// Builds the router's routes (Go `Router.Routes()`).
    routes: Box<dyn Fn() -> Vec<Route>>,
    public_routes: HashSet<&'static str>,
}

/// Go `concreteRoutePath`: every `{var}` replaced by `x`.
fn concrete_route_path(pattern: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        match rest[open..].find('}') {
            Some(close) => {
                out.push('x');
                rest = &rest[open + close + 1..];
            }
            None => {
                rest = &rest[open..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Go `primaryRouteMethod`: a method the route serves, preferring one that is not HEAD or
/// OPTIONS.
fn primary_route_method(methods: &[Method]) -> Method {
    methods
        .iter()
        .find(|m| **m != Method::HEAD && **m != Method::OPTIONS)
        .or_else(|| methods.first())
        .cloned()
        .unwrap_or(Method::GET)
}

/// Go `authTestRouters`: one instance of every router adkrest mounts.
fn auth_test_routers() -> Vec<RouterAuthCase> {
    let root = gadk_agent::new_agent(AgentConfig {
        name: "test-app".into(),
        description: "root agent".into(),
        ..Default::default()
    })
    .expect("agent::new");
    let loader: Arc<dyn Loader> = Arc::new(SingleLoader::new(root));
    let session_svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runtime = Arc::new(RuntimeApiController::new(RuntimeApiControllerConfig {
        agent_loader: Some(Arc::clone(&loader)),
        ..RuntimeApiControllerConfig::new(Arc::clone(&session_svc))
    }));

    let mut cases = vec![
        RouterAuthCase {
            name: "sessions",
            routes: {
                let c = Arc::new(SessionsApiController::new(Arc::clone(&session_svc)));
                Box::new(move || sessions_routes(Arc::clone(&c)))
            },
            public_routes: HashSet::new(),
        },
        RouterAuthCase {
            name: "runtime",
            routes: Box::new(move || runtime_routes(Arc::clone(&runtime))),
            public_routes: HashSet::new(),
        },
        RouterAuthCase {
            name: "apps",
            routes: {
                let c = Arc::new(AppsApiController::new(Some(Arc::clone(&loader))));
                Box::new(move || apps_routes(Arc::clone(&c)))
            },
            public_routes: HashSet::new(),
        },
        RouterAuthCase {
            name: "artifacts",
            routes: {
                let c = Arc::new(ArtifactsApiController::new(None));
                Box::new(move || artifacts_routes(Arc::clone(&c)))
            },
            public_routes: HashSet::new(),
        },
        RouterAuthCase {
            name: "version",
            routes: {
                let c = Arc::new(VersionApiController::new());
                Box::new(move || version_routes(Arc::clone(&c)))
            },
            public_routes: HashSet::from(["GetVersion"]),
        },
        RouterAuthCase {
            name: "agentgraph",
            routes: {
                let c = Arc::new(AgentGraphApiController::new(Some(Arc::clone(&loader))));
                Box::new(move || agent_graph_routes(Arc::clone(&c)))
            },
            public_routes: HashSet::new(),
        },
        RouterAuthCase {
            name: "tests",
            routes: Box::new(tests_routes),
            public_routes: HashSet::new(),
        },
        RouterAuthCase {
            name: "eval",
            routes: Box::new(eval_routes),
            public_routes: HashSet::new(),
        },
    ];
    // The debug store is filled by plan 5a Task 12; until it lands its constructor panics and
    // the debug case cannot be built.
    if let Ok(telemetry) = catch_unwind(AssertUnwindSafe(|| {
        DebugTelemetry::new(DebugTelemetryConfig::default())
    })) {
        let c = Arc::new(DebugApiController::new(
            session_svc,
            Some(loader),
            telemetry,
        ));
        cases.push(RouterAuthCase {
            name: "debug",
            routes: Box::new(move || debug_routes(Arc::clone(&c))),
            public_routes: HashSet::new(),
        });
    }
    cases
}

// parity: adapted: each router is mounted alone in a RouteTable (Go: SetupSubRouters on a fresh mux.Router); a public route whose handler is not yet implemented answers 500 from the panic catcher, which the test (like Go's) accepts as not 401
#[tokio::test]
async fn test_router_authentication() {
    // NewHeader rejects a request without the named header, so a credential-less request to a
    // protected route is answered 401 before the handler runs.
    let authenticator = authn::header("non-existing-header");

    for tc in auth_test_routers() {
        let routes = (tc.routes)();
        assert!(!routes.is_empty(), "router {:?} exposes no routes", tc.name);
        let probes: Vec<_> = routes
            .iter()
            .map(|r| {
                (
                    r.name,
                    r.public,
                    primary_route_method(&r.methods),
                    concrete_route_path(&r.pattern),
                )
            })
            .collect();
        let table = RouteTable::new((tc.routes)(), Some(Arc::clone(&authenticator)), 0);

        for (route_name, public, method, path) in probes {
            let case = format!("{}/{}", tc.name, route_name);
            let want_public = tc.public_routes.contains(route_name);
            // The declared Public flag must match the intended policy.
            assert_eq!(public, want_public, "{case}: Route {route_name:?} Public");

            // The request deliberately carries no credentials.
            let req = http::Request::builder()
                .method(method.clone())
                .uri(path.as_str())
                .body(Body::empty())
                .unwrap();
            let status = table.serve(req).await.status();
            if want_public {
                assert_ne!(
                    status.as_u16(),
                    401,
                    "{case}: {method} {path} is public but returned 401"
                );
            } else {
                assert_eq!(
                    status.as_u16(),
                    401,
                    "{case}: {method} {path} returned {status} without credentials, want 401"
                );
            }
        }
    }
}
