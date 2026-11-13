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

//! `BigQueryViewRunner::run_view_ddl` (Go `bqViewRunner.RunViewDDL`, spec R6) over every view
//! scenario `scripts/gen-go-agentanalytics` recorded from Go (`tests/fixtures/wire/view_*.json`):
//! a `FakeServer` answers Go's canned responses in order (`{{JOB_ID}}` replaced by the job ID
//! the client sent), the runner is built as the probe built Go's (`call.runner`: project,
//! dataset, a live client or a dropped one) and run once with `call.view_name` / `call.query`.
//! The requests it sends must be exactly Go's (method, path, query, headers but
//! `Authorization`, `User-Agent`, `X-Goog-Api-Client` and transport-added ones, JSON body
//! semantically, job IDs by shape and count) and its result Go's (`result.error` text or
//! success). Covers: identical view → no job (`view_identical_no_job`), job pending → done
//! (`view_job_pending_query_path`, `view_job_pending_no_config`), job error surfaced
//! (`view_job_error`, `view_job_pending_then_error`, `view_job_pending_no_config_then_error`),
//! non-view refusal (`view_non_view_table`, `view_non_view_materialized`) and a dropped client
//! (`view_dropped_client`), plus `sync_analytics_views` over the REST runner.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_agentanalytics::bigquery::fake::RecordingLogger;
use gadk_agentanalytics::{
    BigQueryClient, BigQueryViewRunner, ClientOptions, Context, ViewDefinition, ViewRunner,
    sync_analytics_views,
};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use pretty_assertions::assert_eq;
use serde_json::Value;

const WIRE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wire");

/// Headers the comparison leaves out (as `tests/rest_wire.rs`): credentials and client
/// identification, and those the HTTP transports add themselves.
const IGNORED_HEADERS: [&str; 7] = [
    "authorization",
    "user-agent",
    "x-goog-api-client",
    "accept",
    "accept-encoding",
    "content-length",
    "host",
];

fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{WIRE_DIR}/{name}.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A fake BigQuery API answering a scenario's canned responses in order (as
/// `tests/rest_wire.rs`'s `Replay`).
struct Replay {
    server: FakeServer,
    job_id: Arc<Mutex<String>>,
}

impl Replay {
    fn start(fixture: &Value) -> Replay {
        let responses: Vec<(u16, String)> = fixture["exchanges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let status = u16::try_from(e["response"]["status"].as_u64().unwrap()).unwrap();
                (status, e["response"]["body"].to_string())
            })
            .collect();
        let job_id = Arc::new(Mutex::new(String::new()));
        let next = AtomicUsize::new(0);
        let server_job_id = job_id.clone();
        let server = FakeServer::start(move |req: &RecordedRequest| {
            if req.method == "POST" && req.path.ends_with("/jobs") {
                let id = req.json()["jobReference"]["jobId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let mut current = server_job_id.lock().unwrap();
                if current.is_empty() {
                    *current = id;
                }
            }
            let i = next.fetch_add(1, Ordering::SeqCst);
            // An unexpected request gets a 400, which Go's retry policy never retries (a 5xx
            // would be retried until the context ends), so the test fails instead of hanging.
            let Some((status, body)) = responses.get(i) else {
                return FakeResponse::json_status(
                    400,
                    r#"{"error":{"code":400,"message":"unexpected request"}}"#,
                );
            };
            let body = body.replace("{{JOB_ID}}", &server_job_id.lock().unwrap());
            FakeResponse::json_status(*status, body)
        });
        Replay { server, job_id }
    }

    fn job_id(&self) -> String {
        self.job_id.lock().unwrap().clone()
    }
}

/// Go `bq.NewClient(ctx, project, option.WithEndpoint(server+"/bigquery/v2/"),
/// option.WithoutAuthentication())`.
fn client(project: &str, server: &FakeServer) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        endpoint: Some(format!("{}/bigquery/v2/", server.url())),
        without_authentication: true,
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new(project, &options).unwrap())
}

/// Replaces `{{JOB_ID}}` in every string of `v`.
fn with_job_id(v: &Value, job_id: &str) -> Value {
    serde_json::from_str(&v.to_string().replace("{{JOB_ID}}", job_id)).unwrap()
}

/// Asserts the Rust request equals Go's recorded one.
fn assert_request(scenario: &str, i: usize, got: &RecordedRequest, want: &Value, job_id: &str) {
    let want = with_job_id(want, job_id);
    let at = format!("{scenario} request {i}");
    assert_eq!(got.method, want["method"].as_str().unwrap(), "{at}: method");
    assert_eq!(got.path, want["path"].as_str().unwrap(), "{at}: path");
    assert_eq!(
        got.query.clone().unwrap_or_default(),
        want["raw_query"].as_str().unwrap(),
        "{at}: query"
    );
    let mut got_headers: Vec<(String, Vec<String>)> = got
        .headers
        .keys()
        .map(|k| k.as_str().to_ascii_lowercase())
        .filter(|k| !IGNORED_HEADERS.contains(&k.as_str()))
        .map(|k| (k.clone(), got.header_values(&k)))
        .collect();
    got_headers.sort();
    got_headers.dedup();
    let mut want_headers: Vec<(String, Vec<String>)> = want["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            (
                k.to_ascii_lowercase(),
                serde_json::from_value(v.clone()).unwrap(),
            )
        })
        .filter(|(k, _)| !IGNORED_HEADERS.contains(&k.as_str()))
        .collect();
    want_headers.sort();
    assert_eq!(got_headers, want_headers, "{at}: headers");
    if want["body"].is_null() {
        assert_eq!(got.body_text(), "", "{at}: body");
    } else {
        let got_body: Value = serde_json::from_slice(&got.body).unwrap();
        assert_eq!(got_body, want["body"], "{at}: body");
    }
}

/// Runs `BigQueryViewRunner::run_view_ddl` as Go's probe ran `bqViewRunner.RunViewDDL` for
/// scenario `name`, and checks the requests and the result against Go's.
async fn run_scenario(name: &str) {
    let fixture = load(name);
    assert_eq!(fixture["name"], name);
    let call = &fixture["call"];
    assert!(
        call["fn"].as_str().unwrap().starts_with("bqViewRunner"),
        "{name}: not a view scenario"
    );
    let replay = Replay::start(&fixture);
    let bq = client(
        fixture["client"]["project"].as_str().unwrap(),
        &replay.server,
    );
    let runner_spec = &call["runner"];
    let runner = BigQueryViewRunner::new(
        Arc::downgrade(&bq),
        runner_spec["projectID"].as_str().unwrap(),
        runner_spec["datasetID"].as_str().unwrap(),
    );
    // `dropped`: Go's weak pointer is empty (the client was collected).
    let _keep = match runner_spec["client"].as_str().unwrap() {
        "dropped" => {
            drop(bq);
            None
        }
        live => {
            assert!(live.starts_with("live"), "{name}: runner client {live:?}");
            Some(bq)
        }
    };

    let result = runner
        .run_view_ddl(
            &Context::background(),
            call["view_name"].as_str().unwrap(),
            call["query"].as_str().unwrap(),
        )
        .await;

    let exchanges = fixture["exchanges"].as_array().unwrap();
    let requests = replay.server.requests();
    assert_eq!(requests.len(), exchanges.len(), "{name}: request count");
    let job_id = replay.job_id();
    for (i, (got, want)) in requests.iter().zip(exchanges).enumerate() {
        assert_request(name, i, got, &want["request"], &job_id);
    }

    let sent_ids: BTreeSet<String> = requests
        .iter()
        .filter(|r| r.method == "POST" && r.path.ends_with("/jobs"))
        .map(|r| {
            r.json()["jobReference"]["jobId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    match fixture["job_id"].as_object() {
        None => assert!(sent_ids.is_empty(), "{name}: unexpected job"),
        Some(spec) => {
            assert_eq!(spec["pattern"], "^[0-9a-f]{32}$");
            assert_eq!(
                sent_ids.len() as u64,
                spec["distinct_ids"].as_u64().unwrap(),
                "{name}: distinct job IDs"
            );
            for id in &sent_ids {
                assert!(
                    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                    "{name}: job ID {id:?}"
                );
            }
        }
    }

    let want = fixture["result"]["error"]
        .as_str()
        .map(|e| e.replace("{{JOB_ID}}", &job_id));
    assert_eq!(
        result.map_err(|e| e.to_string()).err(),
        want,
        "{name}: RunViewDDL() error"
    );
}

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        /// Every view scenario of `tests/fixtures/wire`.
        const VIEW_SCENARIOS: &[&str] = &[$(stringify!($name)),*];
        $(
            #[tokio::test]
            async fn $name() {
                run_scenario(stringify!($name)).await;
            }
        )*
    };
}

scenarios!(
    view_differs_replace,
    view_dropped_client,
    view_get_error_still_creates,
    view_identical_no_job,
    view_job_error,
    view_job_insert_400,
    view_job_insert_409,
    view_job_insert_503_retried,
    view_job_pending_no_config,
    view_job_pending_no_config_then_error,
    view_job_pending_query_path,
    view_job_pending_then_error,
    view_missing_create_done,
    view_no_dataset_skips_get,
    view_non_view_materialized,
    view_non_view_table,
    view_query_without_as_newline,
    view_runner_empty_project_uses_client,
    view_runner_project_differs_from_client,
);

#[test]
fn every_view_wire_fixture_is_run() {
    let mut names: Vec<String> = std::fs::read_dir(WIRE_DIR)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|f| f.strip_suffix(".json").map(str::to_owned))
        .filter(|n| n.starts_with("view_"))
        .collect();
    names.sort();
    assert_eq!(names, VIEW_SCENARIOS);
}

/// `sync_analytics_views` over the REST runner: a missing view is created by a job, a
/// `jobs.insert` 409 counts as created (Go `isConflictError`), an identical view runs no job,
/// and a job error is joined into the pass error and logged with Go's texts.
#[tokio::test]
async fn sync_over_rest_runner_aggregates_like_go() {
    const DDL: &str = "CREATE OR REPLACE VIEW `p.d.{v}` AS\nSELECT 1\n";
    let defs: Vec<ViewDefinition> = ["v_a", "v_b", "v_c", "v_d"]
        .iter()
        .map(|v| ViewDefinition {
            name: (*v).to_owned(),
            event_type: v.to_uppercase(),
            query: DDL.replace("{v}", v),
        })
        .collect();
    let server = FakeServer::start(|r: &RecordedRequest| {
        if r.method == "GET" {
            if r.path.ends_with("/tables/v_c") {
                return FakeResponse::json(r#"{"type":"VIEW","view":{"query":"SELECT 1"}}"#);
            }
            return FakeResponse::json_status(
                404,
                r#"{"error":{"code":404,"message":"Not found"}}"#,
            );
        }
        let query = r.json()["configuration"]["query"]["query"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let job_id = r.json()["jobReference"]["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if query.contains("v_b") {
            return FakeResponse::json_status(
                409,
                r#"{"error":{"code":409,"message":"Already Exists","errors":[{"reason":"duplicate","message":"Already Exists"}]}}"#,
            );
        }
        let error = if query.contains("v_d") {
            r#","errorResult":{"reason":"invalidQuery","location":"query","message":"bad"}"#
        } else {
            ""
        };
        FakeResponse::json(format!(
            r#"{{"jobReference":{{"projectId":"p","jobId":"{job_id}"}},"configuration":{{"query":{{}}}},"status":{{"state":"DONE"{error}}}}}"#
        ))
    });
    let bq = client("p", &server);
    let runner = BigQueryViewRunner::new(Arc::downgrade(&bq), "p", "d");
    let logger = RecordingLogger::default();
    let err = sync_analytics_views(&Context::background(), &runner, &defs, Some(&logger))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"failed to create view v_d: {Location: "query"; Message: "bad"; Reason: "invalidQuery"}"#
    );
    assert_eq!(
        logger.lines(),
        [
            r#"agentanalytics: failed to create view v_d: {Location: "query"; Message: "bad"; Reason: "invalidQuery"}"#,
            "agentanalytics: view creation finished: 3 created, 1 failed of 4; call SyncViews to retry",
        ]
    );
    let calls: Vec<String> = server
        .requests()
        .iter()
        .map(|r| {
            format!(
                "{} {}",
                r.method,
                r.path.rsplit('/').next().unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            "GET v_a",
            "POST jobs",
            "GET v_b",
            "POST jobs",
            "GET v_c",
            "GET v_d",
            "POST jobs",
        ]
    );
}
