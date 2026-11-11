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

//! Port of ADK-Go `cmd/launcher/web/triggers/eventarc/eventarc_test.go` (Task 11).
//!
//! The helpers Go repeats from the Pub/Sub test come from its port.

use std::sync::Arc;

use axum::body::Body;
use gadk_server::authn::Authenticator;
use gadk_session::InMemorySessionService;
use http::{Method, StatusCode};
use pretty_assertions::assert_eq;

use super::cmd_launcher_web_triggers_pubsub_pubsub::{
    TEST_SERVICE_ACCOUNT, auth_args, noop_config, refused, serve, session_count, strings, verified,
};
use crate::golog::Captured;
use crate::web::triggers::eventarc::{self, EventarcLauncher};
use crate::web::{WebRouter, WebSublauncher};

const TEST_AUDIENCE: &str = "https://example.run.app/api/apps/noop/trigger/eventarc";

/// Go `newTestServer`: parses `args`, wires the route onto a fresh router and returns it with
/// the session service the trigger writes into and the captured log. A `Some` auth replaces the
/// authenticator Parse built, standing in for a token that verifies.
async fn new_test_server(
    mut l: EventarcLauncher,
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

/// Go `postEvent`: a binary-mode CloudEvent whose `ce-source` names `source`.
async fn post_event(router: &WebRouter, source: &str, authorization: &str) -> (StatusCode, String) {
    let mut req = http::Request::builder()
        .method(Method::POST)
        .uri("/api/apps/noop/trigger/eventarc")
        .header("ce-id", "1")
        .header("ce-type", "google.cloud.storage.object.v1.finalized")
        .header("ce-source", source)
        .header("ce-specversion", "1.0");
    if !authorization.is_empty() {
        req = req.header("Authorization", authorization);
    }
    serve(router, req.body(Body::from("{}")).unwrap()).await
}

#[tokio::test]
async fn test_unauthenticated_caller_cannot_impersonate() {
    for (name, authorization) in [
        ("no credential", ""),
        ("malformed bearer token", "Bearer not-a-jwt"),
        ("non-bearer scheme", "Basic dXNlcjpwYXNz"),
    ] {
        let (router, sessions, _) =
            new_test_server(eventarc::new(), auth_args(TEST_AUDIENCE), None).await;

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
    let (router, sessions, _) = new_test_server(eventarc::new(), Vec::new(), None).await;

    let (status, body) = post_event(&router, "victim", "").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(session_count(&sessions, "victim").await, 1);
}

#[tokio::test]
async fn test_verified_caller_runs_as_delivery_user() {
    let (router, sessions, _) =
        new_test_server(eventarc::new(), auth_args(TEST_AUDIENCE), Some(verified())).await;

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
        new_test_server(eventarc::new(), auth_args(TEST_AUDIENCE), Some(refused())).await;

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
        let (_, _, out) = new_test_server(eventarc::new(), args, None).await;

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
    if let Err(e) = eventarc::new().parse(args.clone()) {
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
            eventarc::new().parse(args.clone()).is_err(),
            "{name}: Parse({args:?}) succeeded, want an error"
        );
    }
}
