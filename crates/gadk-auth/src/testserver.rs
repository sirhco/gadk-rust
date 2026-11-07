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

//! The in-crate HTTP test server (plan D6, C14) for `#[cfg(test)]` code. It is the same source
//! as the integration tests' `tests/common/mod.rs`, included here so the two cannot drift.

#[path = "../tests/common/mod.rs"]
mod imp;

#[allow(unused_imports)]
pub(crate) use imp::{RecordedRequest, Reply, TestServer};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn records_requests_and_serves_replies() {
        let srv = TestServer::start(|req: RecordedRequest| async move {
            match req.path.as_str() {
                "/json" => Reply::json(r#"{"ok":true}"#).with_header("X-Seen", req.method),
                "/redirect" => Reply::redirect("http://127.0.0.1:1/elsewhere", 307),
                _ => Reply::status(404, "not found"),
            }
        })
        .await;
        assert!(srv.url().starts_with("http://127.0.0.1:"));
        assert!(!srv.url().ends_with('/'));
        let client = srv.client();

        let resp = client
            .post(format!("{}/json?a=1&b=2", srv.url()))
            .header("Authorization", "Bearer t")
            .body(r#"{"userId":"u"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "application/json");
        assert_eq!(resp.headers()["x-seen"], "POST");
        assert_eq!(resp.text().await.unwrap(), r#"{"ok":true}"#);

        let no_redirects = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = no_redirects
            .get(format!("{}/redirect", srv.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 307);
        assert_eq!(resp.headers()["location"], "http://127.0.0.1:1/elsewhere");

        let resp = client
            .get(format!("{}/projects%2Fp/x", srv.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        assert_eq!(resp.text().await.unwrap(), "not found");

        assert_eq!(srv.calls(), 3);
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/json");
        assert_eq!(reqs[0].query.as_deref(), Some("a=1&b=2"));
        assert_eq!(reqs[0].header("authorization"), Some("Bearer t"));
        assert_eq!(reqs[0].text(), r#"{"userId":"u"}"#);
        let v: serde_json::Value = reqs[0].json();
        assert_eq!(v["userId"], "u");
        assert_eq!(reqs[1].method, "GET");
        assert_eq!(reqs[1].query, None);
        assert_eq!(reqs[2].path, "/projects/p/x");
    }

    #[tokio::test]
    async fn reads_chunked_bodies_and_serves_large_ones() {
        let big = vec![b'x'; (1 << 20) + 7];
        let reply_body = big.clone();
        let srv = TestServer::start(move |_req| {
            let body = reply_body.clone();
            async move { Reply::ok(body) }
        })
        .await;
        let stream = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(bytes_of("hello, ")),
            Ok(bytes_of("chunked world")),
        ]);
        let resp = srv
            .client()
            .post(srv.url())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.bytes().await.unwrap().len(), big.len());
        let req = &srv.requests()[0];
        assert_eq!(req.header("transfer-encoding"), Some("chunked"));
        assert_eq!(req.text(), "hello, chunked world");
    }

    fn bytes_of(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[tokio::test]
    async fn sequence_repeats_the_last_body() {
        let srv = TestServer::sequence(&["one", "two"]).await;
        let client = srv.client();
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(
                client
                    .get(srv.url())
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(got, ["one", "two", "two"]);
        assert_eq!(srv.calls(), 3);
    }

    #[tokio::test]
    async fn a_hanging_handler_records_the_request_first_and_drop_closes_it() {
        let srv = TestServer::start(|_req| std::future::pending::<Reply>()).await;
        let client = srv.client();
        let url = srv.url().to_string();
        let pending = tokio::spawn(async move { client.get(url).send().await });
        // Real-clock wait, well under the plan's 100 ms ceiling per step.
        for _ in 0..50 {
            if srv.calls() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(srv.calls(), 1, "request recorded before the handler ran");
        drop(srv);
        let res = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("closing the server ends the hung request")
            .unwrap();
        assert!(res.is_err(), "{res:?}");
    }
}
