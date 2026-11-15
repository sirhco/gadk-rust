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

//! Ports of ADK-Go `server/adkrest/controllers/triggers/eventarc_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod triggers_support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use futures::StreamExt as _;
use gadk_agent::{AgentConfig, new_agent};
use gadk_server::rest::testutil::FakeSessionService;
use gadk_server::rest::triggers::{EventarcController, TriggerConfig};
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};
use tower::ServiceExt;
use triggers_support::{Runs, fake_config, mock_agent, post, read};

const STORAGE_PAYLOAD: &str = r#"{
  "bucket": "sample-bucket",
  "contentType": "text/plain",
  "generation": "1587627537231057",
  "id": "sample-bucket/folder/Test.cs/1587627537231057",
  "kind": "storage#object",
  "size": "352",
  "storageClass": "MULTI_REGIONAL",
  "timeCreated": "2020-04-23T07:38:57.230Z",
  "timeStorageClassUpdated": "2020-04-23T07:38:57.230Z",
  "updated": "2020-04-23T07:38:57.230Z"
}"#;

const PUBSUB_PAYLOAD: &str = r#"{
  "subscription": "projects/test-project/subscriptions/my-subscription",
  "message": {
    "attributes": {
      "attr1":"attr1-value"
    },
    "data": "dGVzdCBtZXNzYWdlIDM=",
    "messageId": "message-id",
    "publishTime":"2021-02-05T04:06:14.109Z",
    "orderingKey": "ordering-key"
  }
}"#;

struct Case {
    name: &'static str,
    content_type: &'static str,
    headers: &'static [(&'static str, &'static str)],
    body: Vec<u8>,
    expected_payload: Value,
}

const PUBSUB_HEADERS: &[(&str, &str)] = &[
    ("ce-id", "1234-5678"),
    ("ce-type", "google.cloud.pubsub.topic.v1.messagePublished"),
    (
        "ce-source",
        "//pubsub.googleapis.com/projects/test-project/topics/test-topic",
    ),
    ("ce-specversion", "1.0"),
];

#[tokio::test]
async fn test_eventarc_trigger_handler() {
    let pubsub_structured = serde_json::to_vec(&json!({
        "specversion": "1.0",
        "type": "google.cloud.pubsub.topic.v1.messagePublished",
        "source": "//pubsub.googleapis.com/projects/test-project/topics/test-topic",
        "id": "1234-5678",
        "data": serde_json::from_str::<Value>(PUBSUB_PAYLOAD).unwrap(),
    }))
    .unwrap();
    let pubsub_message = json!({
        "data": "test message 3",
        "attributes": {"attr1": "attr1-value"},
    });

    let cases = [
        Case {
            name: "Storage_Structured_Mode",
            content_type: "application/cloudevents+json",
            headers: &[],
            body: STORAGE_PAYLOAD.as_bytes().to_vec(),
            expected_payload: json!({
                "id": "sample-bucket/folder/Test.cs/1587627537231057",
                "source": "",
                "type": "",
                "specversion": "",
            }),
        },
        Case {
            name: "Storage_Binary_Mode",
            content_type: "application/json",
            headers: &[
                ("ce-id", "1234-5678"),
                ("ce-type", "google.storage.object.v1.finalized"),
                (
                    "ce-source",
                    "//storage.googleapis.com/projects/_/buckets/sample-bucket",
                ),
                ("ce-specversion", "1.0"),
            ],
            body: STORAGE_PAYLOAD.as_bytes().to_vec(),
            expected_payload: json!({
                "id": "1234-5678",
                "type": "google.storage.object.v1.finalized",
                "source": "//storage.googleapis.com/projects/_/buckets/sample-bucket",
                "specversion": "1.0",
                "data": serde_json::from_str::<Value>(STORAGE_PAYLOAD).unwrap(),
            }),
        },
        Case {
            name: "PubSub_Structured_Mode",
            content_type: "application/cloudevents+json",
            headers: &[],
            body: pubsub_structured,
            expected_payload: pubsub_message.clone(),
        },
        Case {
            name: "PubSub_Binary_Mode",
            content_type: "application/json",
            headers: PUBSUB_HEADERS,
            body: PUBSUB_PAYLOAD.as_bytes().to_vec(),
            expected_payload: pubsub_message,
        },
    ];

    for tc in cases {
        let runs = Arc::new(Runs::default());
        let controller =
            EventarcController::new(fake_config(mock_agent(vec![], runs.clone()))).unwrap();

        let mut headers = vec![("content-type", tc.content_type)];
        headers.extend_from_slice(tc.headers);
        let req = post("/apps/test-agent/triggers/eventarc", &headers, tc.body);
        let (code, body) = read(controller.eventarc_trigger_handler(req).await).await;

        assert_eq!(code, 200, "{}: expected status 200. Body: {body}", tc.name);
        assert_eq!(runs.count(), 1, "{}: expected 1 run attempt", tc.name);
        assert_eq!(
            runs.received_json(),
            tc.expected_payload,
            "{}: payload mismatch",
            tc.name
        );
    }
}

/// The agent's text for a non-Pub/Sub event is Go's `json.Marshal(event)`: attributes in field
/// order, `time` when set, the data compacted and HTML-escaped; the source is the user.
#[tokio::test]
async fn eventarc_trigger_handler_passes_gos_event_text() {
    let runs = Arc::new(Runs::default());
    let service = Arc::new(FakeSessionService::default());
    let controller = EventarcController::new(triggers_support::controller_config(
        service.clone(),
        Arc::new(gadk_agent::SingleLoader::new(mock_agent(
            vec![],
            runs.clone(),
        ))),
    ))
    .unwrap();
    let req = post(
        "/",
        &[
            ("ce-id", "1"),
            ("ce-type", "t"),
            ("ce-source", "src"),
            ("ce-time", "2020-01-01T00:00:00Z"),
        ],
        b"{ \"k\" : \"<v>\" }".to_vec(),
    );
    let (code, _) = read(controller.eventarc_trigger_handler(req).await).await;
    assert_eq!(code, 200);
    assert_eq!(
        runs.received.lock().clone().unwrap(),
        "{\"id\":\"1\",\"source\":\"src\",\"type\":\"t\",\"specversion\":\"\",\
         \"time\":\"2020-01-01T00:00:00Z\",\"data\":{\"k\":\"\\u003cv\\u003e\"}}"
    );
    assert_eq!(
        service.sessions.lock().keys().next().unwrap().user_id,
        "src"
    );
}

/// Name, request headers, body, expected status and expected `status` text.
type ErrorCase = (
    &'static str,
    Vec<(&'static str, &'static str)>,
    &'static [u8],
    u16,
    &'static str,
);

/// Error statuses and texts of each Go branch.
#[tokio::test]
async fn eventarc_trigger_handler_error_responses() {
    let runs = Arc::new(Runs::default());
    let controller =
        EventarcController::new(fake_config(mock_agent(vec![], runs.clone()))).unwrap();
    let structured = [("content-type", "application/cloudevents+json")];

    let cases: [ErrorCase; 5] = [
        (
            "structured body is not JSON",
            structured.to_vec(),
            b"",
            400,
            "failed to unmarshal eventarc request: EOF",
        ),
        (
            "Pub/Sub event without data",
            vec![("content-type", "application/cloudevents+json")],
            br#"{"type":"google.cloud.pubsub.topic.v1.messagePublished"}"#,
            500,
            "failed to unmarshal pubsub data: unexpected end of JSON input",
        ),
        (
            "Pub/Sub event with null data",
            structured.to_vec(),
            br#"{"type":"google.cloud.pubsub.topic.v1.messagePublished","data":null}"#,
            400,
            "failed to retrieve message content: empty message data and attributes",
        ),
        (
            "binary body is not JSON",
            vec![("ce-type", "t")],
            b"not json",
            500,
            "failed to marshal agent message: json: error calling MarshalJSON for type \
             json.RawMessage: expected ident at line 1 column 2",
        ),
        (
            "content type with parameters is binary mode",
            vec![
                (
                    "content-type",
                    "application/cloudevents+json; charset=utf-8",
                ),
                ("ce-type", "google.cloud.pubsub.topic.v1.messagePublished"),
            ],
            br#"{"message":{}}"#,
            400,
            "failed to retrieve message content: empty message data and attributes",
        ),
    ];
    for (name, headers, body, want_code, want_status) in cases {
        let req = post("/", &headers, body.to_vec());
        let (code, body) = read(controller.eventarc_trigger_handler(req).await).await;
        assert_eq!(
            (code, body),
            (want_code, format!("{}\n", json!({ "status": want_status }))),
            "{name}"
        );
    }
    assert_eq!(runs.count(), 0);
}

/// Go `TestEventarcBinaryModeRejectsOversizedBody`.
// parity: adapted: the limit is axum's `DefaultBodyLimit` on a route mounting `handler()` (Go wraps the body in `http.MaxBytesReader`)
#[tokio::test]
async fn test_eventarc_binary_mode_rejects_oversized_body() {
    let runs = Arc::new(Runs::default());
    let controller =
        Arc::new(EventarcController::new(fake_config(mock_agent(vec![], runs))).unwrap());

    const LIMIT: usize = 16;
    let app = axum::Router::new()
        .route("/apps/{app_name}/trigger/eventarc", controller.handler())
        .layer(DefaultBodyLimit::max(LIMIT));
    let req = http::Request::post("/apps/test-agent/trigger/eventarc")
        .header("content-type", "application/json")
        .body(Body::from("a".repeat(LIMIT + 1)))
        .unwrap();

    let (code, body) = read(app.oneshot(req).await.unwrap()).await;
    assert_eq!(
        (code, body.as_str()),
        (
            400,
            "{\"status\":\"failed to read body: http: request body too large\"}\n"
        ),
        "oversized binary-mode body"
    );
}

/// Go `buildEventarcRequest`: a structured-mode CloudEvent carrying a Pub/Sub
/// messagePublished payload whose message data is `data`.
fn build_eventarc_request(data: &str) -> http::Request<Body> {
    use base64::Engine as _;
    let payload = format!(
        r#"{{"message": {{"data": "{}"}}}}"#,
        base64::engine::general_purpose::STANDARD.encode(data)
    );
    let body = serde_json::to_vec(&json!({
        "data": serde_json::from_str::<Value>(&payload).unwrap(),
        "id": "1234-5678",
        "source": "//pubsub.googleapis.com/projects/test-project/topics/test-topic",
        "specversion": "1.0",
        "type": "google.cloud.pubsub.topic.v1.messagePublished",
    }))
    .unwrap();
    post(
        "/apps/test-agent/triggers/eventarc",
        &[("content-type", "application/cloudevents+json")],
        body,
    )
}

/// Go `blockingEventarcAgent`: each run signals `started` and then holds until `release` gives
/// it a permit, so a test can count runs in flight.
fn blocking_eventarc_agent(
    started: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
) -> Arc<dyn gadk_agent::Agent> {
    new_agent(AgentConfig {
        name: "test-agent".into(),
        run: Some(Arc::new(move |_ctx| {
            let started = started.clone();
            let release = release.clone();
            futures::stream::once(async move {
                started.send(()).unwrap();
                let _permit = release.acquire().await.unwrap();
                Ok(Event {
                    id: "success-event".into(),
                    ..Default::default()
                })
            })
            .boxed()
        })),
        ..Default::default()
    })
    .unwrap()
}

/// Go `TestEventarcTriggerHandler_NonPositiveMaxConcurrentRunsIsUnlimited`: zero or a negative
/// limit means no limit, so three requests are in flight at once.
// parity: adapted: tokio's paused clock stands in for Go's 2 s wall-clock wait
#[tokio::test(start_paused = true)]
async fn test_eventarc_trigger_handler_non_positive_max_concurrent_runs_is_unlimited() {
    for max_runs in [0, -1] {
        const REQUESTS: usize = 3;
        let (started_tx, mut started) = mpsc::unbounded_channel::<()>();
        let release = Arc::new(Semaphore::new(0));
        let mut cfg = fake_config(blocking_eventarc_agent(started_tx, release.clone()));
        cfg.trigger_config = TriggerConfig {
            max_concurrent_runs: max_runs,
            ..triggers_support::DEFAULT_TRIGGER_CONFIG
        };
        let ctrl = Arc::new(EventarcController::new(cfg).unwrap());

        let mut tasks = Vec::new();
        for _ in 0..REQUESTS {
            let ctrl = ctrl.clone();
            tasks.push(tokio::spawn(async move {
                read(
                    ctrl.eventarc_trigger_handler(build_eventarc_request("hello"))
                        .await,
                )
                .await
            }));
        }
        for i in 0..REQUESTS {
            tokio::time::timeout(Duration::from_secs(2), started.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "only {i} of {REQUESTS} runs in flight; MaxConcurrentRuns={max_runs} must mean no limit"
                    )
                });
        }
        release.add_permits(REQUESTS);
        for (i, task) in tasks.into_iter().enumerate() {
            let (code, body) = task.await.unwrap();
            assert_eq!(code, 200, "request {i}: want 200. Body: {body}");
        }
    }
}

/// Go `TestEventarcTriggerHandler_EnforcesMaxConcurrentRuns`.
// parity: adapted: tokio's paused clock stands in for Go's wall-clock waits (2 s, 250 ms)
#[tokio::test(start_paused = true)]
async fn test_eventarc_trigger_handler_enforces_max_concurrent_runs() {
    const LIMIT: usize = 2;
    let (started_tx, mut started) = mpsc::unbounded_channel::<()>();
    let release = Arc::new(Semaphore::new(0));
    let mut cfg = fake_config(blocking_eventarc_agent(started_tx, release.clone()));
    cfg.trigger_config = TriggerConfig {
        max_concurrent_runs: LIMIT as i32,
        ..triggers_support::DEFAULT_TRIGGER_CONFIG
    };
    let ctrl = Arc::new(EventarcController::new(cfg).unwrap());

    let mut tasks = Vec::new();
    for _ in 0..=LIMIT {
        let ctrl = ctrl.clone();
        tasks.push(tokio::spawn(async move {
            ctrl.eventarc_trigger_handler(build_eventarc_request("hello"))
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
        task.await.unwrap();
    }
}
