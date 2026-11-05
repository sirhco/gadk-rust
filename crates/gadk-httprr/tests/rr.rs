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

//! Ports of ADK-Go `internal/httprr/rr_test.go` plus Go-fixture replay tests.

use bytes::Bytes;
use futures::StreamExt;
use gadk_httprr::*;
use gadk_model::{BodyStream, HttpClient, ModelError, collect_body};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Unreachable;
#[async_trait::async_trait]
impl HttpClient for Unreachable {
    async fn send(
        &self,
        _: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        panic!("network used in replay")
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/TestModel_Generate_ok.httprr")
}

fn go_request(body: &str) -> http::Request<Bytes> {
    http::Request::post(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent",
    )
    .header("content-type", "application/json")
    .header("x-goog-api-key", "fakekey")
    .body(Bytes::from(body.to_string()))
    .unwrap()
}

/// A fresh, empty temp directory unique to this test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gadk-httprr-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn response(status: u16, body: &str) -> http::Response<BodyStream> {
    let body = Bytes::from(body.to_string());
    let stream: BodyStream = futures::stream::iter([Ok(body)]).boxed();
    let mut resp = http::Response::new(stream);
    *resp.status_mut() = http::StatusCode::from_u16(status).unwrap();
    resp.headers_mut()
        .insert("content-type", http::HeaderValue::from_static("text/plain"));
    resp
}

/// Port of Go's `handler`.
struct Handler;
#[async_trait::async_trait]
impl HttpClient for Handler {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        if req.uri().path().ends_with("/redirect") {
            return Ok(response(304, "redirect me!\n"));
        }
        if req.method() == http::Method::GET
            && req.headers().get("secret").map(|v| v.as_bytes()) != Some(b"key")
        {
            return Ok(response(666, "missing secret\n"));
        }
        if req.method() == http::Method::POST
            && !String::from_utf8_lossy(req.body()).contains("my Secret")
        {
            return Ok(response(667, "missing body secret\n"));
        }
        Ok(response(200, ""))
    }
}

/// Port of Go's `always555`.
struct Always555;
#[async_trait::async_trait]
impl HttpClient for Always555 {
    async fn send(
        &self,
        _: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        Ok(response(555, "should not be making HTTP requests\n"))
    }
}

/// Always fails with the given message (port of Go's `errTransport`).
struct ErrTransport(&'static str);
#[async_trait::async_trait]
impl HttpClient for ErrTransport {
    async fn send(
        &self,
        _: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        Err(ModelError::Transport(self.0.into()))
    }
}

/// Returns a response whose body stream errors (port of Go's `badRespTransport`).
struct BadRespTransport;
#[async_trait::async_trait]
impl HttpClient for BadRespTransport {
    async fn send(
        &self,
        _: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let stream: BodyStream =
            futures::stream::iter([Err(ModelError::Transport("TRANSPORT ERROR".into()))]).boxed();
        Ok(http::Response::new(stream))
    }
}

/// Records every request it receives and answers 200 `{"ok":true}`.
#[derive(Default)]
struct Capture(Mutex<Vec<http::Request<Bytes>>>);
#[async_trait::async_trait]
impl HttpClient for Capture {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        self.0.lock().push(req);
        Ok(response(200, r#"{"ok":true}"#))
    }
}

fn drop_port(r: &mut http::Request<Bytes>) -> Result<(), HttprrError> {
    let uri = r.uri().clone();
    if let Some(auth) = uri.authority()
        && auth.port().is_some()
    {
        let pq = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let scheme = uri.scheme_str().unwrap_or("http");
        *r.uri_mut() = format!("{scheme}://{}{pq}", auth.host())
            .parse()
            .map_err(|e: http::uri::InvalidUri| HttprrError::Wire(e.to_string()))?;
    }
    Ok(())
}

fn drop_secret_header(r: &mut http::Request<Bytes>) -> Result<(), HttprrError> {
    r.headers_mut().remove("secret");
    Ok(())
}

fn hide_secret_body(r: &mut http::Request<Bytes>) -> Result<(), HttprrError> {
    if !r.body().is_empty() {
        *r.body_mut() = Bytes::from_static(b"redacted");
    }
    Ok(())
}

fn req(method: &str, url: &str, body: &str) -> http::Request<Bytes> {
    http::Request::builder()
        .method(method)
        .uri(url)
        .body(Bytes::from(body.to_string()))
        .unwrap()
}

async fn must_do(rr: &RecordReplay, req: http::Request<Bytes>, status: u16) {
    let url = req.uri().to_string();
    let resp = rr.send(req).await.unwrap_or_else(|e| panic!("{url}: {e}"));
    let got = resp.status().as_u16();
    let body = collect_body(resp.into_body()).await.unwrap();
    assert_eq!(got, status, "{url}: {}", String::from_utf8_lossy(&body));
}

fn transport_err(err: ModelError) -> String {
    match err {
        ModelError::Transport(e) => e.to_string(),
        other => panic!("expected transport error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_record_replay() {
    let dir = temp_dir("record-replay");
    let file = dir.join("rr");

    // 4 passes:
    //   0: record (pattern matches the file)
    //   1: replay
    //   2: record with pattern "r+"
    //   3: replay (no pattern)
    for pass in 0..4u16 {
        let (real, pattern): (Arc<dyn HttpClient>, Option<&str>) = match pass {
            0 => (Arc::new(Handler), Some("rr$")),
            2 => (Arc::new(Handler), Some("r+")),
            _ => (Arc::new(Always555), None),
        };
        let mut rr = RecordReplay::open_with(&file, real, pattern).unwrap();
        assert_eq!(rr.is_recording(), pass % 2 == 0, "pass {pass}");
        rr.scrub_req(Box::new(drop_port));
        rr.scrub_req(Box::new(drop_secret_header));
        rr.scrub_req(Box::new(hide_secret_body));
        rr.scrub_resp(Box::new(|_| Ok(())));
        rr.scrub_resp(Box::new(|b| {
            let s = b.clone();
            b.clear();
            b.extend_from_slice(&s);
            Ok(())
        }));

        // A different port on each pass, as httptest.NewServer would give.
        let srv = format!("http://127.0.0.1:{}", 40000 + pass);

        let mut r = req("GET", &format!("{srv}/myrequest"), "");
        r.headers_mut()
            .insert("secret", http::HeaderValue::from_static("key"));
        must_do(&rr, r, 200).await;

        must_do(
            &rr,
            req("POST", &format!("{srv}/myrequest"), "my Secret"),
            200,
        )
        .await;

        must_do(&rr, req("GET", &format!("{srv}/redirect"), ""), 304).await;

        if !rr.is_recording() {
            let r = req("GET", &format!("{srv}/uncached"), "");
            if let Ok(resp) = rr.send(r).await {
                panic!("uncached request served: {}", resp.status());
            }
        }

        rr.close().unwrap();
    }

    let data = std::fs::read(&file).unwrap();
    assert!(
        !String::from_utf8_lossy(&data).contains("Secret"),
        "rr file contains Secret:\n{}",
        String::from_utf8_lossy(&data)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

const BAD_RESPONSE_TRACE: &str = concat!(
    "httprr trace v1\n",
    "92 75\n",
    "GET http://127.0.0.1/myrequest HTTP/1.1\r\n",
    "Host: 127.0.0.1\r\n",
    "User-Agent: Go-http-client/1.1\r\n",
    "\r\n",
    "HZZP/1.1 200 OK\r\n",
    "Date: Wed, 12 Jun 2024 13:55:02 GMT\r\n",
    "Content-Length: 0\r\n",
    "\r\n",
);

#[tokio::test]
async fn test_errors() {
    let dir = temp_dir("errors");
    let mut n = 0;
    let mut make_tmp_file = || {
        n += 1;
        let p = dir.join(format!("TestErrors{n}"));
        std::fs::write(&p, b"").unwrap();
        p
    };

    // Record regex parsing.
    let err = RecordReplay::open_with(make_tmp_file(), Arc::new(Unreachable), Some("+"))
        .err()
        .unwrap();
    assert!(
        matches!(err, HttprrError::InvalidRecordRegex(..)),
        "{err:?}"
    );
    assert!(
        err.to_string().contains("invalid GADK_HTTPRR_RECORD regex"),
        "{err}"
    );

    // Invalid httprr trace (no header).
    let err = RecordReplay::open_with(make_tmp_file(), Arc::new(Unreachable), None)
        .err()
        .unwrap();
    assert!(err.to_string().contains("not an httprr trace"), "{err}");

    // Corrupt httprr trace.
    let corrupt = make_tmp_file();
    std::fs::write(&corrupt, b"httprr trace v1\ngarbage\n").unwrap();
    let err = RecordReplay::open_with(&corrupt, Arc::new(Unreachable), None)
        .err()
        .unwrap();
    assert!(err.to_string().contains("corrupt httprr trace"), "{err}");

    // Lengths that run past the end of the file.
    let short = make_tmp_file();
    std::fs::write(&short, b"httprr trace v1\n10 10\nGET / HTTP/1.1\r\n").unwrap();
    let err = RecordReplay::open_with(&short, Arc::new(Unreachable), None)
        .err()
        .unwrap();
    assert!(matches!(err, HttprrError::Corrupt(_)), "{err:?}");

    // Create error (parent directory missing).
    let err = RecordReplay::open_with(
        dir.join("no-such-dir").join("rr"),
        Arc::new(Unreachable),
        Some(".*"),
    )
    .err()
    .unwrap();
    assert!(matches!(err, HttprrError::Io(_)), "{err:?}");

    // Read error (nonexistent trace).
    let err = RecordReplay::open_with(dir.join("nonexistent"), Arc::new(Unreachable), None)
        .err()
        .unwrap();
    assert!(matches!(err, HttprrError::Io(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("no httprr trace; record it with GADK_HTTPRR_RECORD"),
        "{err}"
    );

    // Error during request scrub.
    let mut rr =
        RecordReplay::open_with(make_tmp_file(), Arc::new(Unreachable), Some(".*")).unwrap();
    rr.scrub_req(Box::new(|_| Err(HttprrError::Wire("SCRUB ERROR".into()))));
    let err = rr
        .send(req("GET", "http://127.0.0.1/nonexist", ""))
        .await
        .err()
        .expect("expected an error");
    assert!(transport_err(err).contains("SCRUB ERROR"));
    rr.close().unwrap();

    // Error during response scrub.
    let mut rr =
        RecordReplay::open_with(make_tmp_file(), Arc::new(Capture::default()), Some(".*")).unwrap();
    rr.scrub_resp(Box::new(|_| Err(HttprrError::Wire("SCRUB ERROR".into()))));
    let err = rr
        .send(req("GET", "http://127.0.0.1/nonexist", ""))
        .await
        .err()
        .expect("expected an error");
    assert!(transport_err(err).contains("SCRUB ERROR"));
    rr.close().unwrap();

    // Error reading the real response body.
    let rr =
        RecordReplay::open_with(make_tmp_file(), Arc::new(BadRespTransport), Some(".*")).unwrap();
    let err = rr
        .send(req("GET", "http://127.0.0.1/nonexist", ""))
        .await
        .err()
        .expect("expected an error");
    assert!(transport_err(err).contains("TRANSPORT ERROR"));
    rr.close().unwrap();

    // Error during the real round trip.
    let rr = RecordReplay::open_with(
        make_tmp_file(),
        Arc::new(ErrTransport("TRANSPORT ERROR")),
        Some(".*"),
    )
    .unwrap();
    let err = rr
        .send(req("GET", "http://127.0.0.1/", ""))
        .await
        .err()
        .expect("expected an error");
    assert!(transport_err(err).contains("TRANSPORT ERROR"));
    rr.close().unwrap();

    // Writing after close fails.
    let rr =
        RecordReplay::open_with(make_tmp_file(), Arc::new(Capture::default()), Some(".*")).unwrap();
    rr.close().unwrap();
    assert!(rr.send(req("GET", "http://127.0.0.1/", "")).await.is_err());

    // Structurally valid trace with a malformed response inside.
    let bad = make_tmp_file();
    std::fs::write(&bad, BAD_RESPONSE_TRACE).unwrap();
    let rr = RecordReplay::open_with(&bad, Arc::new(Unreachable), None).unwrap();
    let err = rr
        .send(req("GET", "http://127.0.0.1/myrequest", ""))
        .await
        .err()
        .expect("expected an error");
    assert!(transport_err(err).contains("corrupt httprr trace"));
    rr.close().unwrap();

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn replays_go_fixture_http2_status() {
    let rr = RecordReplay::open_with(fixture(), Arc::new(Unreachable), None).unwrap();
    // Same JSON as the fixture body, different key order and whitespace.
    let resp = rr
        .send(go_request(
            r#"{ "generationConfig": {"temperature": 0}, "contents": [{"role":"user","parts":[{"text":"What is the capital of France? One word."}]}] }"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json; charset=UTF-8"
    );
    let body = collect_body(resp.into_body()).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("Paris"));
    assert!(
        text.ends_with("}\n"),
        "body is the remainder of the entry: {text:?}"
    );
}

#[tokio::test]
async fn replays_float_temperature_against_go_integer() {
    // Rust serializes `temperature: Some(0.0)` as `0.0`; the Go fixture has `0`.
    let rr = RecordReplay::open_with(fixture(), Arc::new(Unreachable), None).unwrap();
    let resp = rr
        .send(go_request(
            r#"{"contents":[{"parts":[{"text":"What is the capital of France? One word."}],"role":"user"}],"generationConfig":{"temperature":0.0}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn unmatched_request_diff() {
    let rr = RecordReplay::open_with(fixture(), Arc::new(Unreachable), None).unwrap();
    let err = rr
        .send(go_request(r#"{"contents":[]}"#))
        .await
        .err()
        .expect("expected an error");
    let msg = err.to_string();
    assert!(msg.contains("cached HTTP response not found"), "{msg}");
    assert!(msg.contains("contents"), "{msg}");
    assert!(msg.contains("\n-") && msg.contains("\n+"), "{msg}");
}

#[tokio::test]
async fn unmatched_url_reports_no_entry() {
    let rr = RecordReplay::open_with(fixture(), Arc::new(Unreachable), None).unwrap();
    let r = http::Request::post("https://example.com/other")
        .body(Bytes::from_static(b"{}"))
        .unwrap();
    let msg = rr
        .send(r)
        .await
        .err()
        .expect("expected an error")
        .to_string();
    assert!(msg.contains("no entry for method+URL"), "{msg}");
}

#[test]
fn canonical_body_sorts_keys() {
    assert_eq!(
        canonical_body(br#"{ "b":1, "a":{"d":2,"c":3} }"#),
        br#"{"a":{"c":3,"d":2},"b":1}"#.to_vec()
    );
    assert_eq!(canonical_body(b"not json"), b"not json".to_vec());
    assert_eq!(
        canonical_body(br#"{"t":0.0,"u":0.5,"v":1e2}"#),
        canonical_body(br#"{"t":0,"u":0.5,"v":100}"#)
    );
}

#[tokio::test]
async fn record_scrubs_keys() {
    let dir = temp_dir("scrub-keys");
    let file = dir.join("keys.httprr");
    let real = Arc::new(Capture::default());
    let rr = RecordReplay::open_with(&file, real.clone(), Some("keys")).unwrap();
    assert!(rr.is_recording());
    let r = http::Request::post("https://example.com/v1/m:stream?alt=sse&key=fakekey")
        .header("x-goog-api-key", "fakekey")
        .header("authorization", "Bearer fakekey")
        .header("content-type", "application/json")
        .body(Bytes::from_static(br#"{"a":1}"#))
        .unwrap();
    let resp = rr.send(r).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        &collect_body(resp.into_body()).await.unwrap()[..],
        br#"{"ok":true}"#
    );
    rr.close().unwrap();

    // The real client saw the unscrubbed request.
    {
        let seen = real.0.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].uri().query(), Some("alt=sse&key=fakekey"));
        assert_eq!(seen[0].headers().get("x-goog-api-key").unwrap(), "fakekey");
    }

    let data = String::from_utf8(std::fs::read(&file).unwrap()).unwrap();
    assert!(data.starts_with("httprr trace v1\n"), "{data}");
    assert!(!data.contains("fakekey"), "{data}");
    assert!(
        !data.to_ascii_lowercase().contains("authorization"),
        "{data}"
    );
    assert!(!data.contains("key="), "{data}");
    assert!(data.contains("?alt=sse HTTP/1.1\r\n"), "{data}");

    // The recorded trace replays the same (keyed) request.
    let rr = RecordReplay::open_with(&file, Arc::new(Unreachable), None).unwrap();
    let r = http::Request::post("https://example.com/v1/m:stream?key=other&alt=sse")
        .body(Bytes::from_static(br#"{ "a": 1 }"#))
        .unwrap();
    let resp = rr.send(r).await.unwrap();
    assert_eq!(
        &collect_body(resp.into_body()).await.unwrap()[..],
        br#"{"ok":true}"#
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn entry(req: &str, resp: &str) -> String {
    format!("{} {}\n{req}{resp}", req.len(), resp.len())
}

#[tokio::test]
async fn duplicate_key_last_wins() {
    let dir = temp_dir("dup");
    let file = dir.join("dup.httprr");
    let request = "POST https://example.com/x HTTP/1.1\r\nHost: example.com\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
    let trace = format!(
        "httprr trace v1\n{}{}",
        entry(request, "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfirst"),
        entry(request, "HTTP/2.0 201 Created\r\n\r\nsecond"),
    );
    std::fs::write(&file, trace).unwrap();
    let rr = RecordReplay::open_with(&file, Arc::new(Unreachable), None).unwrap();
    let resp = rr
        .send(
            http::Request::post("https://example.com/x")
                .body(Bytes::from_static(b"{\"a\":1}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert_eq!(
        &collect_body(resp.into_body()).await.unwrap()[..],
        b"second"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn replays_chunked_and_lf_only_entries() {
    let dir = temp_dir("chunked");
    let file = dir.join("c.httprr");
    let request = "GET http://example.com/c HTTP/1.1\nHost: example.com\n\n";
    let trace = format!(
        "httprr trace v1\n{}",
        entry(
            request,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
        ),
    );
    std::fs::write(&file, trace).unwrap();
    let rr = RecordReplay::open_with(&file, Arc::new(Unreachable), None).unwrap();
    let resp = rr
        .send(req("GET", "http://example.com/c", ""))
        .await
        .unwrap();
    assert_eq!(
        &collect_body(resp.into_body()).await.unwrap()[..],
        b"hello world"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn non_matching_pattern_replays() {
    let rr = RecordReplay::open_with(fixture(), Arc::new(Unreachable), Some("^no-match$")).unwrap();
    assert!(!rr.is_recording());
}
