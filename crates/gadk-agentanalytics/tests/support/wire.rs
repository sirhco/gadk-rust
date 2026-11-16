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

//! Replay of the REST wire scenarios the Go probe recorded from Go's
//! `cloud.google.com/go/bigquery` v1.79.0 client (`tests/fixtures/wire/*.json`): a `FakeServer`
//! answering a scenario's canned responses in order, the client Go's probe built, and the
//! comparison of a Rust request with Go's recorded one. Shared by `tests/rest_wire.rs`,
//! `tests/rest_schema_upgrade.rs` and `tests/rest_views.rs`, which include this file with
//! `#[path]` (each uses a subset, hence the `dead_code` allow).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_agentanalytics::{BigQueryClient, ClientOptions};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use pretty_assertions::assert_eq;
use serde_json::Value;

/// The probe's wire scenarios.
pub const WIRE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wire");

/// Headers the comparison leaves out: credentials and client identification (gadk sends its
/// own `User-Agent`, as the GCS crate does), and those the HTTP transports add themselves (Go's
/// `net/http`: `Accept-Encoding`, `Content-Length`; reqwest/hyper: `Accept`, `Host`, …).
pub const IGNORED_HEADERS: [&str; 7] = [
    "authorization",
    "user-agent",
    "x-goog-api-client",
    "accept",
    "accept-encoding",
    "content-length",
    "host",
];

/// Scenario `name`'s fixture.
pub fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{WIRE_DIR}/{name}.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The scenario names under [`WIRE_DIR`], sorted.
pub fn scenario_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(WIRE_DIR)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// A fake BigQuery API answering a scenario's canned responses in order, with `{{JOB_ID}}` in
/// a response body replaced by the job ID of the first `jobs.insert` the client sent.
pub struct Replay {
    pub server: FakeServer,
    job_id: Arc<Mutex<String>>,
}

impl Replay {
    pub fn start(fixture: &Value) -> Replay {
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

    /// The job ID of the first `jobs.insert` (empty when there was none).
    pub fn job_id(&self) -> String {
        self.job_id.lock().unwrap().clone()
    }
}

/// Go `bq.NewClient(ctx, project, option.WithEndpoint(server+"/bigquery/v2/"),
/// option.WithoutAuthentication())`.
pub fn client(project: &str, server: &FakeServer) -> Arc<BigQueryClient> {
    let options = ClientOptions {
        endpoint: Some(format!("{}/bigquery/v2/", server.url())),
        without_authentication: true,
        ..ClientOptions::default()
    };
    Arc::new(BigQueryClient::new(project, &options).unwrap())
}

/// [`client`] for the scenario's `client.project`.
pub fn client_for(fixture: &Value, server: &FakeServer) -> Arc<BigQueryClient> {
    client(fixture["client"]["project"].as_str().unwrap(), server)
}

/// Replaces `{{JOB_ID}}` in every string of `v`.
pub fn with_job_id(v: &Value, job_id: &str) -> Value {
    serde_json::from_str(&v.to_string().replace("{{JOB_ID}}", job_id)).unwrap()
}

/// Asserts request `i` of `scenario` equals Go's recorded one: method, path, raw query,
/// headers but [`IGNORED_HEADERS`], and the JSON body semantically (`{{JOB_ID}}` = `job_id`).
pub fn assert_request(scenario: &str, i: usize, got: &RecordedRequest, want: &Value, job_id: &str) {
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
