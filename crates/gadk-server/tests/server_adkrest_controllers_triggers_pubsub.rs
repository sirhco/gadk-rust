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

//! Ports of ADK-Go `server/adkrest/controllers/triggers/pubsub_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod triggers_support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use futures::StreamExt;
use gadk_agent::{AgentConfig, SingleLoader, new_agent};
use gadk_server::rest::testutil::FakeSessionService;
use gadk_server::rest::triggers::{ControllerConfig, PubSubController, TriggerConfig};
use gadk_session::Event;
use gadk_session::compaction::Config as CompactionConfig;
use serde_json::json;
use tokio::sync::{Semaphore, mpsc};
use tower::ServiceExt;
use triggers_support::{
    CountingSummarizer, FailingSummarizer, NoRootLoader, Runs, build_pub_sub_request, fake_config,
    mock_agent, post, pubsub_body, read,
};

struct HandlerCase {
    name: &'static str,
    mock_agent_results: Vec<Option<&'static str>>,
    expected_code: u16,
    expected_run_count: usize,
    request_attributes: Option<&'static [(&'static str, &'static str)]>,
    expected_attributes: Option<serde_json::Value>,
    request_data: &'static str,
}

#[tokio::test]
async fn test_pub_sub_trigger_handler() {
    let cases = [
        HandlerCase {
            name: "Success_Immediate",
            mock_agent_results: vec![],
            expected_code: 200,
            expected_run_count: 1,
            request_attributes: None,
            expected_attributes: None,
            request_data: "Hello agent",
        },
        HandlerCase {
            name: "ResourceExhaustedRetry",
            mock_agent_results: vec![Some("429 ResourceExhausted"), Some("429 ResourceExhausted")],
            expected_code: 200,
            expected_run_count: 3,
            request_attributes: None,
            expected_attributes: None,
            request_data: "Hello agent",
        },
        HandlerCase {
            name: "With_Attributes",
            mock_agent_results: vec![],
            expected_code: 200,
            expected_run_count: 1,
            request_attributes: Some(&[("key1", "val1"), ("key2", "val2")]),
            expected_attributes: Some(json!({"key1": "val1", "key2": "val2"})),
            request_data: "Hello agent",
        },
        HandlerCase {
            name: "Empty Data",
            mock_agent_results: vec![],
            expected_code: 400,
            expected_run_count: 0,
            request_attributes: None,
            expected_attributes: None,
            request_data: "",
        },
    ];

    for tc in cases {
        let runs = Arc::new(Runs::default());
        let agent = mock_agent(tc.mock_agent_results, runs.clone());
        let controller = PubSubController::new(fake_config(agent)).unwrap();

        let req = post(
            "/apps/test-agent/triggers/pubsub",
            &[],
            pubsub_body(tc.request_data, tc.request_attributes),
        );
        let (code, body) = read(controller.pub_sub_trigger_handler(req).await).await;

        assert_eq!(
            code, tc.expected_code,
            "{}: expected status {}, got {code}. Body: {body}",
            tc.name, tc.expected_code
        );
        assert_eq!(
            runs.count(),
            tc.expected_run_count,
            "{}: expected {} run attempts",
            tc.name,
            tc.expected_run_count
        );
        if let Some(want) = tc.expected_attributes {
            assert_eq!(runs.received_json()["attributes"], want, "{}", tc.name);
        }
    }
}

/// The response bytes of each outcome (Go `respondSuccess` / `respondError` through
/// `EncodeJSONResponse`).
#[tokio::test]
async fn pub_sub_trigger_handler_response_bytes() {
    let runs = Arc::new(Runs::default());
    let controller =
        PubSubController::new(fake_config(mock_agent(vec![Some("boom")], runs.clone()))).unwrap();

    let resp = controller
        .pub_sub_trigger_handler(build_pub_sub_request("x"))
        .await;
    assert_eq!(
        resp.headers()["content-type"],
        "application/json; charset=UTF-8"
    );
    assert_eq!(
        read(resp).await,
        (
            500,
            "{\"status\":\"failed to run agent: boom\"}\n".to_owned()
        )
    );

    let resp = controller
        .pub_sub_trigger_handler(build_pub_sub_request("x"))
        .await;
    assert_eq!(
        read(resp).await,
        (200, "{\"status\":\"success\"}\n".to_owned())
    );

    let resp = controller
        .pub_sub_trigger_handler(post("/", &[], Vec::new()))
        .await;
    assert_eq!(
        read(resp).await,
        (
            400,
            "{\"status\":\"failed to decode request: EOF\"}\n".to_owned()
        )
    );

    let resp = controller
        .pub_sub_trigger_handler(post("/", &[], br#"{"message":{}}"#.to_vec()))
        .await;
    assert_eq!(
        read(resp).await,
        (
            400,
            "{\"status\":\"failed to retrieve message content: empty message data and attributes\"}\n"
                .to_owned()
        )
    );

    let mut no_app = build_pub_sub_request("x");
    no_app.extensions_mut().clear();
    let resp = controller.pub_sub_trigger_handler(no_app).await;
    assert_eq!(
        read(resp).await,
        (
            500,
            "{\"status\":\"failed to retrieve app name: no application name provided\"}\n"
                .to_owned()
        )
    );
}

/// The agent receives Go's `json.Marshal` text of the message, and the subscription is the user.
#[tokio::test]
async fn pub_sub_trigger_handler_passes_gos_message_text() {
    let runs = Arc::new(Runs::default());
    let service = Arc::new(FakeSessionService::default());
    let cfg = triggers_support::controller_config(
        service.clone(),
        Arc::new(SingleLoader::new(mock_agent(vec![], runs.clone()))),
    );
    let controller = PubSubController::new(cfg).unwrap();
    let body = br#"{"message":{"data":"PGI+","attributes":{"a":"&"}},"subscription":"s"}"#;
    let (code, _) = read(
        controller
            .pub_sub_trigger_handler(post("/", &[], body.to_vec()))
            .await,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(
        runs.received.lock().clone().unwrap(),
        r#"{"attributes":{"a":"\u0026"},"data":"\u003cb\u003e"}"#
    );
    let sessions = service.sessions.lock();
    let key = sessions.keys().next().unwrap();
    assert_eq!(
        (key.app_name.as_str(), key.user_id.as_str()),
        ("test-agent", "s")
    );
}

/// Binary Pub/Sub `data` reaches the agent as Go's `json.Marshal(string(data))`: `\ufffd` per
/// invalid byte (go1.27.2 output for `[]byte{0x61, 0xff, 0xfe, 0x62}`), not one U+FFFD per
/// invalid sequence.
#[tokio::test]
async fn pub_sub_trigger_handler_escapes_invalid_utf8_as_go() {
    let runs = Arc::new(Runs::default());
    let service = Arc::new(FakeSessionService::default());
    let cfg = triggers_support::controller_config(
        service.clone(),
        Arc::new(SingleLoader::new(mock_agent(vec![], runs.clone()))),
    );
    let controller = PubSubController::new(cfg).unwrap();
    let body = br#"{"message":{"data":"Yf/+Yg==","attributes":{"k":"v"}},"subscription":"s"}"#;
    let (code, _) = read(
        controller
            .pub_sub_trigger_handler(post("/", &[], body.to_vec()))
            .await,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(
        runs.received.lock().clone().unwrap(),
        r#"{"attributes":{"k":"v"},"data":"a\ufffd\ufffdb"}"#
    );
}

/// Go `TestPubSubTriggerSurvivesACompactionFailure`.
#[tokio::test]
async fn test_pub_sub_trigger_survives_a_compaction_failure() {
    let runs = Arc::new(Runs::default());
    let controller = PubSubController::new(ControllerConfig {
        compaction: Some(CompactionConfig {
            compaction_interval: 1,
            summarizer: Some(Arc::new(FailingSummarizer)),
            ..Default::default()
        }),
        ..fake_config(mock_agent(vec![], runs.clone()))
    })
    .unwrap();

    let (code, body) = read(
        controller
            .pub_sub_trigger_handler(build_pub_sub_request("Hello agent"))
            .await,
    )
    .await;

    assert_eq!(
        code, 200,
        "the agent answered, so the delivery succeeded. Body: {body}"
    );
    assert_eq!(
        runs.count(),
        1,
        "a NACKed delivery is retried and repeats work already done"
    );
}

/// The compaction check never asks the loader for its root agent (Go passes no agent): a loader
/// with no agents is accepted and each delivery answers with the loader's not-found error.
#[tokio::test]
async fn test_pub_sub_controller_compaction_check_with_no_root_agent() {
    let controller = PubSubController::new(ControllerConfig {
        compaction: Some(CompactionConfig {
            compaction_interval: 1,
            summarizer: Some(Arc::new(FailingSummarizer)),
            ..Default::default()
        }),
        agent_loader: Arc::new(NoRootLoader),
        ..fake_config(mock_agent(vec![], Arc::new(Runs::default())))
    })
    .unwrap();

    let (code, body) = read(
        controller
            .pub_sub_trigger_handler(build_pub_sub_request("Hello agent"))
            .await,
    )
    .await;
    assert_eq!(code, 500, "body: {body}");
    assert!(body.contains("agent test-agent not found"), "body: {body}");
}

/// Go `TestPubSubTriggerActuallyRunsCompaction`.
#[tokio::test]
async fn test_pub_sub_trigger_actually_runs_compaction() {
    let runs = Arc::new(Runs::default());
    let summarizer = Arc::new(CountingSummarizer::default());
    let controller = PubSubController::new(ControllerConfig {
        compaction: Some(CompactionConfig {
            compaction_interval: 1,
            summarizer: Some(summarizer.clone()),
            ..Default::default()
        }),
        ..fake_config(mock_agent(vec![], runs))
    })
    .unwrap();

    let (code, _) = read(
        controller
            .pub_sub_trigger_handler(build_pub_sub_request("Hello agent"))
            .await,
    )
    .await;

    assert_eq!(code, 200);
    assert!(
        summarizer.count() > 0,
        "the summarizer was never called, so this surface accepts a compaction config and does \
         nothing with it"
    );
}

/// Go `TestPubSubTriggerHandler_EnforcesMaxConcurrentRuns`: with `max_concurrent_runs` N, N+1
/// concurrent requests at a blocking agent leave exactly N runs in flight.
// parity: adapted: tokio's paused clock stands in for Go's wall-clock waits (2 s, 250 ms)
#[tokio::test(start_paused = true)]
async fn test_pub_sub_trigger_handler_enforces_max_concurrent_runs() {
    const LIMIT: usize = 2;
    let (started_tx, mut started) = mpsc::unbounded_channel::<()>();
    let release = Arc::new(Semaphore::new(0));
    let release_in_agent = release.clone();
    let blocking_agent = new_agent(AgentConfig {
        name: "test-agent".into(),
        run: Some(Arc::new(move |_ctx| {
            let started_tx = started_tx.clone();
            let release = release_in_agent.clone();
            futures::stream::once(async move {
                started_tx.send(()).unwrap();
                let _ = release.acquire().await.unwrap();
                Ok(Event {
                    id: "success-event".into(),
                    ..Default::default()
                })
            })
            .boxed()
        })),
        ..Default::default()
    })
    .unwrap();

    let mut cfg = fake_config(blocking_agent);
    cfg.trigger_config = TriggerConfig {
        max_concurrent_runs: LIMIT as i32,
        ..triggers_support::DEFAULT_TRIGGER_CONFIG
    };
    let ctrl = Arc::new(PubSubController::new(cfg).unwrap());

    let mut tasks = Vec::new();
    for _ in 0..=LIMIT {
        let ctrl = ctrl.clone();
        tasks.push(tokio::spawn(async move {
            ctrl.pub_sub_trigger_handler(build_pub_sub_request("hello"))
                .await
        }));
    }

    for i in 0..LIMIT {
        tokio::time::timeout(Duration::from_secs(2), started.recv())
            .await
            .unwrap_or_else(|_| panic!("only {i} of {LIMIT} runs started"));
    }
    // The (LIMIT+1)th request must be blocked on the semaphore, not running.
    if tokio::time::timeout(Duration::from_millis(250), started.recv())
        .await
        .is_ok()
    {
        panic!("a run beyond MaxConcurrentRuns={LIMIT} started; limit not enforced");
    }
    release.add_permits(LIMIT + 1);
    for task in tasks {
        assert_eq!(task.await.unwrap().status(), 200);
    }
}

/// Go `TestPubSubTriggerHandler_ZeroMaxConcurrentRunsIsUnlimited`.
// parity: adapted: tokio's paused clock stands in for Go's 2 s wall-clock wait
#[tokio::test(start_paused = true)]
async fn test_pub_sub_trigger_handler_zero_max_concurrent_runs_is_unlimited() {
    let runs = Arc::new(Runs::default());
    let mut cfg = fake_config(mock_agent(vec![], runs));
    cfg.trigger_config.max_concurrent_runs = 0;
    let ctrl = PubSubController::new(cfg).unwrap();

    let resp = tokio::time::timeout(
        Duration::from_secs(2),
        ctrl.pub_sub_trigger_handler(build_pub_sub_request("hello")),
    )
    .await
    .expect("handler blocked with MaxConcurrentRuns=0; must be treated as no limit");
    assert_eq!(resp.status(), 200);
}

/// A throttled run is retried after Go's jittered backoff (`base * 2^attempt`, capped, plus up
/// to half of it), on tokio's clock.
#[tokio::test(start_paused = true)]
async fn pub_sub_trigger_retries_after_gos_backoff() {
    let runs = Arc::new(Runs::default());
    let mut cfg = fake_config(mock_agent(
        vec![Some("429 ResourceExhausted"), Some("429 ResourceExhausted")],
        runs.clone(),
    ));
    cfg.trigger_config = TriggerConfig {
        max_retries: 3,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(5),
        max_concurrent_runs: 1,
    };
    let ctrl = PubSubController::new(cfg).unwrap();

    let start = tokio::time::Instant::now();
    let resp = ctrl
        .pub_sub_trigger_handler(build_pub_sub_request("hello"))
        .await;
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), 200);
    assert_eq!(runs.count(), 3);
    // 1 s + jitter [0, 0.5 s) then 2 s + jitter [0, 1 s).
    assert!(
        elapsed >= Duration::from_secs(3) && elapsed < Duration::from_millis(4500),
        "elapsed {elapsed:?}"
    );
}

/// Retries stop at `max_retries`; the last throttling error is the answer.
#[tokio::test(start_paused = true)]
async fn pub_sub_trigger_gives_up_after_max_retries() {
    let runs = Arc::new(Runs::default());
    let mut cfg = fake_config(mock_agent(
        vec![Some("429 ResourceExhausted"); 3],
        runs.clone(),
    ));
    cfg.trigger_config.max_retries = 2;
    let ctrl = PubSubController::new(cfg).unwrap();

    let (code, body) = read(
        ctrl.pub_sub_trigger_handler(build_pub_sub_request("hello"))
            .await,
    )
    .await;
    assert_eq!(
        (code, body.as_str()),
        (
            500,
            "{\"status\":\"failed to run agent: 429 ResourceExhausted\"}\n"
        )
    );
    assert_eq!(runs.count(), 3);
}

/// `handler()` mounts on an axum route: POST only, `{app_name}` from the route.
#[tokio::test]
async fn pub_sub_handler_mounts_on_an_axum_route() {
    let runs = Arc::new(Runs::default());
    let service = Arc::new(FakeSessionService::default());
    let ctrl = Arc::new(
        PubSubController::new(triggers_support::controller_config(
            service.clone(),
            Arc::new(SingleLoader::new(mock_agent(vec![], runs.clone()))),
        ))
        .unwrap(),
    );
    // The Go launcher's 10 MiB trigger body limit.
    let app = axum::Router::new()
        .route("/apps/{app_name}/trigger/pubsub", ctrl.handler())
        .layer(DefaultBodyLimit::max(10 << 20));

    let req = http::Request::post("/apps/test%2Dagent/trigger/pubsub")
        .body(Body::from(pubsub_body("hi", None)))
        .unwrap();
    let (code, _) = read(app.clone().oneshot(req).await.unwrap()).await;
    assert_eq!(code, 200);
    assert_eq!(
        service.sessions.lock().keys().next().unwrap().app_name,
        "test-agent"
    );

    let req = http::Request::get("/apps/a/trigger/pubsub")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(req).await.unwrap().status(), 405);
}

/// `handler()` mounted behind `authn::middleware`, as the handler docs (and the Go launcher)
/// prescribe: no credentials is 401 and the agent never runs; an authenticated call runs it.
#[tokio::test]
async fn pub_sub_handler_behind_the_authn_middleware() {
    use gadk_server::authn::{self, AuthnError, Caller};

    let runs = Arc::new(Runs::default());
    let ctrl = Arc::new(
        PubSubController::new(triggers_support::controller_config(
            Arc::new(FakeSessionService::default()),
            Arc::new(SingleLoader::new(mock_agent(vec![], runs.clone()))),
        ))
        .unwrap(),
    );
    // A fake authenticator: accepts exactly `Bearer good`.
    let auth = authn::custom(|parts: &http::request::Parts| {
        let ok = parts
            .headers
            .get(http::header::AUTHORIZATION)
            .is_some_and(|v| v == "Bearer good");
        async move {
            if ok {
                Ok(Some(Caller {
                    user_id: "pubsub-push".into(),
                    claims: None,
                }))
            } else {
                Err(AuthnError::Unauthenticated(None))
            }
        }
    });
    let app = axum::Router::new()
        .route(
            "/apps/{app_name}/trigger/pubsub",
            ctrl.handler().layer(authn::middleware(auth)),
        )
        .layer(DefaultBodyLimit::max(10 << 20));

    let req = http::Request::post("/apps/test-agent/trigger/pubsub")
        .body(Body::from(pubsub_body("hi", None)))
        .unwrap();
    let (code, _) = read(app.clone().oneshot(req).await.unwrap()).await;
    assert_eq!(code, 401);
    assert_eq!(
        runs.count(),
        0,
        "an unauthenticated delivery must not run the agent"
    );

    let req = http::Request::post("/apps/test-agent/trigger/pubsub")
        .header(http::header::AUTHORIZATION, "Bearer good")
        .body(Body::from(pubsub_body("hi", None)))
        .unwrap();
    let (code, _) = read(app.oneshot(req).await.unwrap()).await;
    assert_eq!(code, 200);
    assert_eq!(runs.count(), 1);
}
