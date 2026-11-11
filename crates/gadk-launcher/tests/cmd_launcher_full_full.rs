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

//! Port of ADK-Go `cmd/launcher/full/full_test.go`.

#![cfg(feature = "full")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gadk_agent::{AgentConfig, SingleLoader, new_agent};
use gadk_launcher::{CancellationToken, Config, Error, Launcher};
use tokio::task::JoinHandle;

mod support;

use support::free_port;

/// Go `launchedServer`: a full launcher running for the duration of one test.
struct LaunchedServer {
    base: String,
    /// Cancelled at the end of the test (Go's test context).
    cancel: CancellationToken,
    /// The result of `execute`, which returns only once the launcher has stopped.
    run: JoinHandle<Result<(), Error>>,
}

impl LaunchedServer {
    /// Go `assertStillRunning`: fails the test with the launcher's own error when `execute` has
    /// already returned (another process took the port first, for instance).
    async fn assert_still_running(&mut self) {
        if self.run.is_finished() {
            let err = (&mut self.run).await.unwrap();
            panic!(
                "the launcher on {} stopped while the test was running: {err:?}",
                self.base
            );
        }
    }

    /// Go's cleanup: cancels the launcher and checks it returned without an error.
    async fn stop(self) {
        self.cancel.cancel();
        match tokio::time::timeout(Duration::from_secs(10), self.run).await {
            Ok(result) => {
                if let Err(e) = result.unwrap() {
                    panic!("the launcher on {} failed: {e}", self.base);
                }
            }
            Err(_) => eprintln!(
                "the launcher on {} did not return within 10s of shutdown",
                self.base
            ),
        }
    }
}

/// Go `startFullLauncher`: `full::new()` on a free port with every sub-launcher enabled;
/// `api_args` follow the api keyword. It drives `full::new()` itself rather than an equivalent
/// launcher: the composition in `full.rs` is the thing being pinned.
async fn start_full_launcher(api_args: &[&str]) -> LaunchedServer {
    let root_agent = new_agent(AgentConfig {
        name: "test_agent".into(),
        description: "root agent".into(),
        ..Default::default()
    })
    .unwrap();

    let port = free_port();
    let mut args: Vec<String> = [
        "web",
        "--port",
        &port.to_string(),
        "webui",
        "a2a",
        "pubsub",
        "eventarc",
        "api",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    args.extend(api_args.iter().map(|s| (*s).to_owned()));

    let cancel = CancellationToken::new();
    // Go's log goes to the test output; here it is captured to keep the output clean.
    let mut config = Config::new(Arc::new(SingleLoader::new(root_agent)));
    config.log = gadk_launcher::golog::capture().0;
    let run = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            gadk_launcher::full::new()
                .execute(cancel, config, args)
                .await
        })
    };
    let mut s = LaunchedServer {
        base: format!("http://127.0.0.1:{port}"),
        cancel,
        run,
    };

    // Bounded per request, not only across the retry loop.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    // The web launcher registers /health before any sub-launcher, so readiness does not depend
    // on the order under test.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.assert_still_running().await;
        if client
            .get(format!("{}/health", s.base))
            .send()
            .await
            .is_ok()
        {
            // Every instance of this test answers /health the same way, so a reply proves only
            // that something holds the port. Re-check before handing the server over.
            s.assert_still_running().await;
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("GET {}/health never answered within the deadline", s.base);
}

/// The routes registered before `api` still serve their own content: with an empty path prefix
/// `api` mounts a catch-all matching every path (the web UI and the A2A paths included), and
/// `pubsub`/`eventarc` default to `/api`, the prefix `api` also defaults to; all four survive
/// only because `full::new()` passes `api` last.
#[tokio::test(flavor = "multi_thread")]
async fn test_api_does_not_shadow_earlier_routes() {
    let mut empty_api_prefix_server = start_full_launcher(&["--path_prefix", ""]).await;
    let mut default_api_prefix_server = start_full_launcher(&[]).await;

    struct Case {
        name: &'static str,
        /// Selects the server whose api prefix is empty.
        empty_api_prefix: bool,
        method: reqwest::Method,
        path: &'static str,
        headers: &'static [(&'static str, &'static str)],
        body: &'static str,
        want_status: u16,
        /// Lowercase, matched against the lowercased response body.
        want_body: &'static str,
    }
    let tests = [
        Case {
            name: "webui",
            empty_api_prefix: true,
            method: reqwest::Method::GET,
            path: "/ui/",
            headers: &[],
            body: "",
            want_status: 200,
            want_body: "<!doctype html",
        },
        Case {
            name: "a2a",
            empty_api_prefix: true,
            method: reqwest::Method::GET,
            path: "/.well-known/agent-card.json",
            headers: &[],
            body: "",
            want_status: 200,
            want_body: "test_agent",
        },
        Case {
            // POST, because the trigger routes match on method; the empty payload is rejected
            // before the agent runs.
            name: "pubsub",
            empty_api_prefix: false,
            method: reqwest::Method::POST,
            path: "/api/apps/test_agent/trigger/pubsub",
            headers: &[],
            body: "{}",
            want_status: 400,
            want_body: "failed to retrieve message content",
        },
        Case {
            // A Pub/Sub event type takes the branch that rejects an empty message.
            name: "eventarc",
            empty_api_prefix: false,
            method: reqwest::Method::POST,
            path: "/api/apps/test_agent/trigger/eventarc",
            headers: &[("ce-type", "google.cloud.pubsub.topic.v1.messagePublished")],
            body: "{}",
            want_status: 400,
            want_body: "failed to retrieve message content",
        },
    ];

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for tc in &tests {
        let server = if tc.empty_api_prefix {
            &mut empty_api_prefix_server
        } else {
            &mut default_api_prefix_server
        };
        let url = format!("{}{}", server.base, tc.path);
        let mut req = client.request(tc.method.clone(), &url).body(tc.body);
        for (name, value) in tc.headers {
            req = req.header(*name, *value);
        }
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                server.assert_still_running().await;
                panic!("{}: {} {url} failed: {e}", tc.name, tc.method);
            }
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap();
        if status != tc.want_status {
            server.assert_still_running().await;
            panic!(
                "{}: {} {} status = {status}, want {}; api is shadowing the route. First 120 bytes: {:?}",
                tc.name,
                tc.method,
                tc.path,
                tc.want_status,
                body.chars().take(120).collect::<String>()
            );
        }
        assert!(
            body.to_lowercase().contains(tc.want_body),
            "{}: {} {} body does not contain {:?}; first 120 bytes: {:?}",
            tc.name,
            tc.method,
            tc.path,
            tc.want_body,
            body.chars().take(120).collect::<String>()
        );
    }

    empty_api_prefix_server.stop().await;
    default_api_prefix_server.stop().await;
}
