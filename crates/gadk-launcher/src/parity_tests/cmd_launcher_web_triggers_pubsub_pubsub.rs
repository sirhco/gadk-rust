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

//! Port of ADK-Go `cmd/launcher/web/triggers/pubsub/pubsub_test.go` (Task 11).
//!
//! Go serves the route through a fresh `mux.NewRouter()` and `httptest`; here a fresh
//! `WebRouter::new(false)` serves the request directly. The helpers the Eventarc port shares are
//! `pub(super)`.

use std::sync::Arc;

use axum::body::Body;
use futures::StreamExt;
use gadk_agent::{AgentConfig, SingleLoader, new_agent};
use gadk_server::authn::{self, Authenticator, AuthnError, Caller};
use gadk_server::mux::{Request, Response};
use gadk_session::{InMemorySessionService, ListRequest, SessionService};
use http::{Method, StatusCode};
use pretty_assertions::assert_eq;

use crate::config::Config;
use crate::golog::{self, Captured};
use crate::test_support::strings;
use crate::web::triggers::pubsub::{self, PubSubLauncher};
use crate::web::{WebRouter, WebSublauncher};

pub(super) const TEST_SERVICE_ACCOUNT: &str = "push@project.iam.gserviceaccount.com";

const TEST_AUDIENCE: &str = "https://example.run.app/api/apps/noop/trigger/pubsub";

/// Go `authArgs` for `audience`.
pub(super) fn auth_args(audience: &str) -> Vec<String> {
    vec![
        format!("-oidc_audience={audience}"),
        format!("-oidc_service_accounts={TEST_SERVICE_ACCOUNT}"),
    ]
}

/// Go's `noop` agent (`Run` yields nothing) and in-memory sessions in a launcher config whose log
/// is captured.
pub(super) fn noop_config() -> (Config, Arc<InMemorySessionService>, Captured) {
    let agent = new_agent(AgentConfig {
        name: "noop".into(),
        run: Some(Arc::new(|_ctx| futures::stream::empty().boxed())),
        ..Default::default()
    })
    .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    let (log, out) = golog::capture();
    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    config.session_service = Some(sessions.clone());
    config.log = log;
    (config, sessions, out)
}

/// Go `sessionCount`.
pub(super) async fn session_count(sessions: &InMemorySessionService, user_id: &str) -> usize {
    sessions
        .list(ListRequest {
            app_name: "noop".into(),
            user_id: user_id.into(),
        })
        .await
        .unwrap()
        .len()
}

/// Serves `req` and returns the status and body.
pub(super) async fn serve(router: &WebRouter, req: Request) -> (StatusCode, String) {
    let resp: Response = router.serve(req).await;
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// An authenticator that verifies every request as `push-sa-subject`.
pub(super) fn verified() -> Arc<dyn Authenticator> {
    authn::custom(|_req| async {
        Ok(Some(Caller {
            user_id: "push-sa-subject".into(),
            claims: None,
        }))
    })
}

/// An authenticator that refuses every principal.
pub(super) fn refused() -> Arc<dyn Authenticator> {
    authn::custom(|_req| async { Err(AuthnError::Forbidden(None)) })
}

/// Go `newTestServer`: parses `args`, wires the route onto a fresh router and returns it with
/// the session service the trigger writes into and the captured log. A `Some` auth replaces the
/// authenticator Parse built, standing in for a token that verifies.
async fn new_test_server(
    mut l: PubSubLauncher,
    args: Vec<String>,
    auth: Option<Arc<dyn Authenticator>>,
) -> (WebRouter, Arc<InMemorySessionService>, Captured) {
    if let Err(e) = l.parse(args.clone()) {
        panic!("Parse({args:?}) failed: {e}");
    }
    if auth.is_some() {
        l.trigger.auth = auth;
    }
    let (mut config, sessions, out) = noop_config();
    let mut router = WebRouter::new(false);
    l.setup_subrouters(&mut router, &mut config)
        .unwrap_or_else(|e| panic!("SetupSubrouters() failed: {e}"));
    (router, sessions, out)
}

/// Go `postEvent`: a push delivery whose subscription field names `source`.
async fn post_event(router: &WebRouter, source: &str, authorization: &str) -> (StatusCode, String) {
    let body = format!(
        r#"{{"message":{{"data":"aGk=","messageId":"1"}},"subscription":{}}}"#,
        gadk_platform::go_quote(source)
    );
    let mut req = http::Request::builder()
        .method(Method::POST)
        .uri("/api/apps/noop/trigger/pubsub");
    if !authorization.is_empty() {
        req = req.header("Authorization", authorization);
    }
    serve(router, req.body(Body::from(body)).unwrap()).await
}

#[test]
fn test_parse() {
    for (name, args, want_prefix, want_retry, want_err) in [
        ("default values", vec![], "/api", 3, false),
        (
            "custom prefix and retries",
            vec!["-path_prefix=/custom", "-trigger_max_retries=5"],
            "/custom",
            5,
            false,
        ),
        (
            "invalid retry count",
            vec!["-trigger_max_retries=-1"],
            "/api",
            3,
            true,
        ),
    ] {
        let mut l = pubsub::new();
        let result = l.parse(strings(&args));
        assert_eq!(result.is_err(), want_err, "{name}: Parse() = {result:?}");
        if want_err {
            continue;
        }
        assert_eq!(l.trigger.path_prefix(), want_prefix, "{name}: pathPrefix");
        assert_eq!(
            l.trigger.trigger_max_retries(),
            want_retry,
            "{name}: triggerMaxRetries"
        );
    }
}

// parity: adapted: WebRouter has no Match; the request is served and must reach the route (not gorilla's 404 or 405)
#[tokio::test]
async fn test_setup_subrouters() {
    let mut l = pubsub::new();
    l.parse(strings(&["-path_prefix=/api"])).unwrap();

    let mut router = WebRouter::new(false);
    let (mut config, _, _) = noop_config();
    l.setup_subrouters(&mut router, &mut config)
        .unwrap_or_else(|e| panic!("SetupSubrouters() failed: {e}"));

    let req = http::Request::builder()
        .method(Method::POST)
        .uri("/api/apps/my-app/trigger/pubsub")
        .body(Body::empty())
        .unwrap();
    let (status, body) = serve(&router, req).await;
    assert!(
        status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED,
        "SetupSubrouters() did not register expected route: {status} {body}"
    );
}

#[tokio::test]
async fn test_unauthenticated_caller_cannot_impersonate() {
    for (name, authorization) in [
        ("no credential", ""),
        ("malformed bearer token", "Bearer not-a-jwt"),
        ("non-bearer scheme", "Basic dXNlcjpwYXNz"),
    ] {
        let (router, sessions, _) =
            new_test_server(pubsub::new(), auth_args(TEST_AUDIENCE), None).await;

        let (status, _) = post_event(&router, "victim", authorization).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}: status");
        assert_eq!(
            session_count(&sessions, "victim").await,
            0,
            "{name}: an unauthenticated request ran the agent as the user it named"
        );
    }
}

// Auth is opt-in, so without the flags the route behaves as it always has. This also proves
// session_count can see the run the test above forbids.
#[tokio::test]
async fn test_no_auth_flags_keeps_route_open() {
    let (router, sessions, _) = new_test_server(pubsub::new(), Vec::new(), None).await;

    let (status, body) = post_event(&router, "victim", "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(session_count(&sessions, "victim").await, 1);
}

#[tokio::test]
async fn test_verified_caller_runs_as_delivery_user() {
    let (router, sessions, _) =
        new_test_server(pubsub::new(), auth_args(TEST_AUDIENCE), Some(verified())).await;

    let (status, body) = post_event(&router, "delivery-user", "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        session_count(&sessions, "delivery-user").await,
        1,
        "sessions for the delivery's user"
    );
    assert_eq!(
        session_count(&sessions, "push-sa-subject").await,
        0,
        "sessions for the authenticated service account"
    );
}

#[tokio::test]
async fn test_refused_caller_gets403() {
    let (router, sessions, _) =
        new_test_server(pubsub::new(), auth_args(TEST_AUDIENCE), Some(refused())).await;

    let (status, _) = post_event(&router, "victim", "").await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        session_count(&sessions, "victim").await,
        0,
        "a refused caller ran the agent"
    );
}

// parity: adapted: the warning goes to the config's Go log (captured) instead of the process-wide log output
#[tokio::test]
async fn test_startup_warns_only_when_unauthenticated() {
    for (name, args, want_warn) in [
        ("no auth flags", Vec::new(), true),
        ("auth flags", auth_args(TEST_AUDIENCE), false),
    ] {
        let (_, _, out) = new_test_server(pubsub::new(), args, None).await;

        let log = out.contents();
        assert_eq!(
            log.contains("unauthenticated"),
            want_warn,
            "{name}: logged an unauthenticated warning; log: {log:?}"
        );
    }
}

#[test]
fn test_parse_trims_space_around_service_accounts() {
    let args = vec![
        format!("-oidc_audience={TEST_AUDIENCE}"),
        format!(
            "-oidc_service_accounts={TEST_SERVICE_ACCOUNT}, other@project.iam.gserviceaccount.com"
        ),
    ];
    if let Err(e) = pubsub::new().parse(args.clone()) {
        panic!("Parse({args:?}) = {e}, want no error");
    }
}

#[test]
fn test_parse_rejects_incomplete_auth_flags() {
    for (name, args) in [
        (
            "audience without service accounts",
            vec![format!("-oidc_audience={TEST_AUDIENCE}")],
        ),
        (
            "service accounts without audience",
            vec![format!("-oidc_service_accounts={TEST_SERVICE_ACCOUNT}")],
        ),
        (
            "trailing comma",
            vec![
                format!("-oidc_audience={TEST_AUDIENCE}"),
                format!("-oidc_service_accounts={TEST_SERVICE_ACCOUNT},"),
            ],
        ),
        (
            "padded audience",
            vec![
                format!("-oidc_audience={TEST_AUDIENCE} "),
                format!("-oidc_service_accounts={TEST_SERVICE_ACCOUNT}"),
            ],
        ),
        // What a launch script passes when the variables it reads are unset.
        (
            "both flags empty",
            strings(&["-oidc_audience=", "-oidc_service_accounts="]),
        ),
        (
            "service accounts empty",
            strings(&["-oidc_service_accounts="]),
        ),
    ] {
        assert!(
            pubsub::new().parse(args.clone()).is_err(),
            "{name}: Parse({args:?}) succeeded, want an error"
        );
    }
}
