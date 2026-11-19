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

//! Replays every Cloud Storage upload the Go probe recorded from ADK-Go's
//! `gcsOffloader` (`tests/fixtures/gcs/*.json`, spec R11): a `FakeServer` answers the canned
//! responses in order (`{{SERVER}}` replaced by its URL), `GcsOffloader::upload_content` runs
//! the scenario's call, and every request must be Go's — method, path, raw query, headers
//! (`Authorization`, `User-Agent` and the transports' `Accept`, `Accept-Encoding`,
//! `Content-Length`, `Host` aside; the idempotency tokens, the multipart boundary and the
//! client-version tokens of `X-Goog-Api-Client` templated as the probe templates them), the
//! multipart parts (headers, metadata text, media bytes) or the body (JSON text, or the bytes
//! of the payload range the `Content-Range` names) — and the result Go's (URI, error text,
//! googleapi code). Further tests cover Go behaviour the probe does not capture: the 32 s
//! chunk retry deadline, retries until the context ends, transport errors, an ended context,
//! a 308, and the client options.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use gadk_agentanalytics::{
    ClientOptions, Context, ContextError, Error, GcsContentOffloader, GcsOffloader,
    GcsUploadRequest,
};
use gadk_model::rest::test_util::{
    FakeResponse, FakeServer, RecordedRequest, ScriptedClient, bearer_credentials,
};
use gadk_platform::env::MapEnv;
use pretty_assertions::assert_eq;
use regex::Regex;
use serde_json::Value;

const GCS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gcs");

const MIB16: usize = 16 * 1024 * 1024;

/// A canned response: status, headers, body.
type Canned = (u16, Vec<(String, String)>, String);

/// A multipart part: its headers and data.
type Part = (BTreeMap<String, Vec<String>>, Vec<u8>);

/// Headers the comparison leaves out: credentials and client identification, and those the
/// HTTP transports add themselves (Go's `net/http`: `Accept-Encoding`, `Content-Length`;
/// reqwest/hyper: `Accept`, `Host`).
const IGNORED_HEADERS: [&str; 6] = [
    "authorization",
    "user-agent",
    "accept",
    "accept-encoding",
    "content-length",
    "host",
];

fn load(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{GCS_DIR}/{name}.json")).unwrap())
        .unwrap()
}

fn scenario_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(GCS_DIR)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// The payload of `call.data`: `base64`, or `pattern` "i % 251" of `len` bytes.
fn payload(data: &Value) -> Bytes {
    let len = usize::try_from(data["len"].as_u64().unwrap()).unwrap();
    let bytes: Vec<u8> = match data["pattern"].as_str() {
        Some("i % 251") => (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect(),
        Some(other) => panic!("unknown pattern {other}"),
        None => STANDARD.decode(data["base64"].as_str().unwrap()).unwrap(),
    };
    assert_eq!(bytes.len(), len);
    Bytes::from(bytes)
}

/// Go `newGCSOffloader(ctx, "test-bucket", option.WithEndpoint(server+"/storage/v1/"),
/// option.WithoutAuthentication())` with `STORAGE_EMULATOR_HOST` empty.
fn offloader(bucket: &str, endpoint: String) -> GcsOffloader {
    let options = ClientOptions {
        endpoint: Some(endpoint),
        without_authentication: true,
        ..ClientOptions::default()
    };
    GcsOffloader::new_with_env(bucket, &options, &MapEnv(Default::default())).unwrap()
}

/// A fake Cloud Storage answering a scenario's canned responses in order; a request beyond
/// them gets a 400 (never retried), so the test fails instead of hanging.
fn replay(fixture: &Value) -> FakeServer {
    let responses: Vec<Canned> = fixture["exchanges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let r = &e["response"];
            let headers = r["headers"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                .collect();
            (
                u16::try_from(r["status"].as_u64().unwrap()).unwrap(),
                headers,
                r["body"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let url = Arc::new(Mutex::new(String::new()));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let server_url = url.clone();
    let server = FakeServer::start(move |_req: &RecordedRequest| {
        let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let Some((status, headers, body)) = responses.get(i) else {
            return FakeResponse::json_status(
                400,
                r#"{"error":{"code":400,"message":"unexpected request"}}"#,
            );
        };
        let base = server_url.lock().unwrap().clone();
        let mut resp = FakeResponse::new(*status);
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("Content-Type"))
        {
            resp = resp.header("Content-Type", "application/json; charset=UTF-8");
        }
        for (k, v) in headers {
            resp = resp.header(k.clone(), v.replace("{{SERVER}}", &base));
        }
        resp.chunk(body.replace("{{SERVER}}", &base))
    });
    *url.lock().unwrap() = server.url();
    server
}

/// The probe's templating of the Rust side: idempotency tokens numbered in order of first
/// appearance, the multipart boundary, the server URL, and gadk's tracking header for Go's
/// `gl-go/… gccl/…` / `gl-go/… gdcl/…` client-version tokens.
struct Templater {
    server: String,
    tokens: BTreeMap<String, String>,
}

impl Templater {
    fn learn(&mut self, req: &RecordedRequest) {
        let tok = req.header("x-goog-gcs-idempotency-token");
        if !tok.is_empty() && !self.tokens.contains_key(&tok) {
            let name = format!("{{{{IDEMPOTENCY_TOKEN_{}}}}}", self.tokens.len() + 1);
            self.tokens.insert(tok, name);
        }
    }

    fn apply(&self, v: &str) -> String {
        let mut v = v.replace(&self.server, "{{SERVER}}");
        v = v.replace(&gadk_platform::tracking_header(), "{{CLIENT}}");
        for (tok, name) in &self.tokens {
            v = v.replace(tok, name);
        }
        v
    }
}

/// Go's client-version tokens of `X-Goog-Api-Client` (`gl-go/<go> gccl/<storage>` on the
/// insert, `gl-go/<go> gdcl/<api>` on a chunk) as `{{CLIENT}}`.
fn template_go_client(v: &str) -> String {
    let re = Regex::new(r"gl-go/\{\{GO_VERSION\}\} (gccl|gdcl)/[0-9A-Za-z.\-]+").unwrap();
    re.replace_all(v, "{{CLIENT}}").into_owned()
}

/// The `boundary` parameter of a `multipart/related` content type.
fn boundary_of(content_type: &str) -> Option<String> {
    content_type
        .strip_prefix("multipart/related; boundary=")
        .map(str::to_owned)
}

/// The parts of a `mime/multipart` body: (headers, data).
fn parse_multipart(body: &[u8], boundary: &str) -> Vec<Part> {
    let first = format!("--{boundary}\r\n");
    let sep = format!("\r\n--{boundary}");
    assert!(body.starts_with(first.as_bytes()), "multipart body start");
    let end = format!("\r\n--{boundary}--\r\n");
    assert!(body.ends_with(end.as_bytes()), "multipart body end");
    let inner = &body[first.len()..body.len() - end.len()];
    let mut parts = Vec::new();
    let mut rest = inner;
    loop {
        let (part, next) = match find(rest, format!("{sep}\r\n").as_bytes()) {
            Some(i) => (&rest[..i], Some(&rest[i + sep.len() + 2..])),
            None => (rest, None),
        };
        let hdr_end = find(part, b"\r\n\r\n").map_or(0, |i| i + 4);
        let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if hdr_end > 0 {
            for line in std::str::from_utf8(&part[..hdr_end - 4])
                .unwrap()
                .split("\r\n")
            {
                let (k, v) = line.split_once(": ").unwrap();
                headers.entry(k.to_owned()).or_default().push(v.to_owned());
            }
        } else {
            assert!(part.starts_with(b"\r\n"), "part without headers");
        }
        let data_start = if hdr_end > 0 { hdr_end } else { 2 };
        parts.push((headers, part[data_start..].to_vec()));
        match next {
            Some(n) => rest = n,
            None => break,
        }
    }
    parts
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The payload range a `Content-Range` names (`bytes a-b/…` or the empty `bytes */n`).
fn content_range(v: &str, data: &Bytes) -> Bytes {
    let r = v.strip_prefix("bytes ").unwrap();
    let (range, _) = r.split_once('/').unwrap();
    if range == "*" {
        return Bytes::new();
    }
    let (a, b) = range.split_once('-').unwrap();
    data.slice(a.parse::<usize>().unwrap()..=b.parse::<usize>().unwrap())
}

/// Asserts request `i` equals Go's recorded one.
fn assert_request(
    at: &str,
    t: &Templater,
    got: &RecordedRequest,
    want: &Value,
    data: &Bytes,
    call: &Value,
) {
    assert_eq!(got.method, want["method"].as_str().unwrap(), "{at}: method");
    assert_eq!(got.path, want["path"].as_str().unwrap(), "{at}: path");
    assert_eq!(
        t.apply(&got.query.clone().unwrap_or_default()),
        want["raw_query"].as_str().unwrap(),
        "{at}: query"
    );
    let boundary = boundary_of(&got.header("content-type"));
    let mut got_headers: Vec<(String, Vec<String>)> = got
        .headers
        .keys()
        .map(|k| k.as_str().to_ascii_lowercase())
        .filter(|k| !IGNORED_HEADERS.contains(&k.as_str()))
        .map(|k| {
            let vals = got
                .header_values(&k)
                .iter()
                .map(|v| {
                    let v = t.apply(v);
                    match &boundary {
                        Some(b) => v.replace(b, "{{BOUNDARY}}"),
                        None => v,
                    }
                })
                .collect();
            (k, vals)
        })
        .collect();
    got_headers.sort();
    got_headers.dedup();
    let mut want_headers: Vec<(String, Vec<String>)> = want["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            let vals: Vec<String> = serde_json::from_value(v.clone()).unwrap();
            (
                k.to_ascii_lowercase(),
                vals.iter().map(|v| template_go_client(v)).collect(),
            )
        })
        .filter(|(k, _)| !IGNORED_HEADERS.contains(&k.as_str()))
        .collect();
    want_headers.sort();
    assert_eq!(got_headers, want_headers, "{at}: headers");

    let body = &want["body"];
    assert_eq!(
        got.body.len(),
        usize::try_from(body["len"].as_u64().unwrap()).unwrap(),
        "{at}: body length"
    );
    if let Some(parts) = body["multipart"].as_array() {
        let boundary = boundary.expect("multipart content type");
        assert_eq!(
            boundary.len(),
            usize::try_from(body["boundary_len"].as_u64().unwrap()).unwrap(),
            "{at}: boundary length"
        );
        assert!(
            boundary
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
        let got_parts = parse_multipart(&got.body, &boundary);
        assert_eq!(got_parts.len(), parts.len(), "{at}: multipart parts");
        for (j, ((headers, part_data), want_part)) in got_parts.iter().zip(parts).enumerate() {
            let want_headers: BTreeMap<String, Vec<String>> =
                serde_json::from_value(want_part["headers"].clone()).unwrap();
            assert_eq!(headers, &want_headers, "{at}: part {j} headers");
            assert_eq!(
                part_data.len(),
                usize::try_from(want_part["len"].as_u64().unwrap()).unwrap(),
                "{at}: part {j} length"
            );
            match want_part["text"].as_str() {
                Some(text) => {
                    assert_eq!(
                        std::str::from_utf8(part_data).unwrap(),
                        text,
                        "{at}: part {j}"
                    );
                }
                None => {
                    // The media part: the payload itself (Go recorded its SHA-256, the
                    // payload's).
                    assert_eq!(part_data.as_slice(), data.as_ref(), "{at}: part {j} media");
                    assert_eq!(
                        want_part["sha256"], call["data"]["sha256"],
                        "{at}: part {j}"
                    );
                }
            }
        }
    } else if let Some(text) = body["text"].as_str() {
        assert_eq!(t.apply(&got.body_text()), text, "{at}: body");
    } else {
        let range = got.header("content-range");
        assert_eq!(got.body, content_range(&range, data), "{at}: chunk bytes");
        if got.body.len() == data.len() {
            assert_eq!(body["sha256"], call["data"]["sha256"], "{at}: chunk digest");
        }
    }
}

/// The first `googleapi` error code in `err`'s tree (Go `errors.As(err, &apiErr)`).
fn googleapi_code(err: &Error) -> Option<u16> {
    match err.find(&mut |e| matches!(e, Error::Api(_))) {
        Some(Error::Api(api)) => Some(api.code),
        _ => None,
    }
}

#[tokio::test]
async fn every_gcs_scenario_replays_gos_wire() {
    let names = scenario_names();
    assert_eq!(names.len(), 12, "{names:?}");
    for name in &names {
        let fixture = load(name);
        let server = replay(&fixture);
        let bucket = fixture["client"]["bucket"].as_str().unwrap();
        let endpoint = fixture["client"]["endpoint"]
            .as_str()
            .unwrap()
            .replace("{{SERVER}}", &server.url());
        let o = offloader(bucket, endpoint);
        let call = &fixture["call"];
        let data = payload(&call["data"]);
        let req = GcsUploadRequest {
            path: call["path"].as_str().unwrap().to_owned(),
            data: data.clone(),
            content_type: call["content_type"].as_str().unwrap().to_owned(),
        };
        let result = o.upload_content(&Context::background(), req).await;
        let close = o.close().await;

        let want = &fixture["result"];
        match &result {
            Ok(uri) => {
                assert_eq!(uri, want["uri"].as_str().unwrap(), "{name}: uri");
                assert!(want["error"].is_null(), "{name}: Go failed");
            }
            Err(err) => {
                assert_eq!(want["uri"], "", "{name}: Go succeeded");
                assert_eq!(
                    err.to_string(),
                    want["error"].as_str().unwrap(),
                    "{name}: error"
                );
                assert_eq!(
                    googleapi_code(err).map(u64::from),
                    want["googleapi_code"].as_u64(),
                    "{name}: googleapi code"
                );
                // Go `ownedUploadError{err: fmt.Errorf("failed to write …: %w", err)}`: an
                // opaque wrapper around the wrapped error.
                assert_eq!(want["is_owned_upload_error"], true, "{name}");
                let Error::Other(owned) = err else {
                    panic!("{name}: not the owned upload error: {err:?}");
                };
                let inner = std::error::Error::source(owned.as_ref())
                    .and_then(|e| e.downcast_ref::<Error>())
                    .unwrap();
                assert!(
                    matches!(inner, Error::Wrapped { source, .. } if matches!(**source, Error::Api(_))),
                    "{name}: {inner:?}"
                );
            }
        }
        assert_eq!(want["is_precondition_failed"], false, "{name}");
        assert!(
            close.is_ok() && want["close_error"].is_null(),
            "{name}: close"
        );

        let exchanges = fixture["exchanges"].as_array().unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), exchanges.len(), "{name}: request count");
        let mut t = Templater {
            server: server.url(),
            tokens: BTreeMap::new(),
        };
        for (i, (got, exchange)) in requests.iter().zip(exchanges).enumerate() {
            t.learn(got);
            assert_request(
                &format!("{name} request {i}"),
                &t,
                got,
                &exchange["request"],
                &data,
                call,
            );
        }
    }
}

/// A resumable upload whose session URI is `http://fake/session`, answering the initiation
/// 200 and every chunk with `chunk(n)` (1-based, resends counted).
fn resumable_client(
    chunk: impl Fn(usize) -> FakeResponse + Send + Sync + 'static,
) -> Arc<ScriptedClient> {
    let chunks = std::sync::atomic::AtomicUsize::new(0);
    ScriptedClient::new(move |req: &RecordedRequest| {
        if req
            .query
            .as_deref()
            .unwrap_or_default()
            .contains("upload_id=")
        {
            let n = chunks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            chunk(n)
        } else {
            FakeResponse::new(200).header(
                "Location",
                "http://fake/upload/storage/v1/b/b/o?uploadType=resumable&upload_id=s",
            )
        }
    })
}

fn scripted_offloader(client: Arc<ScriptedClient>) -> GcsOffloader {
    let options = ClientOptions {
        endpoint: Some("http://fake/storage/v1/".into()),
        without_authentication: true,
        http_client: Some(client),
        ..ClientOptions::default()
    };
    GcsOffloader::new_with_env("b", &options, &MapEnv(Default::default())).unwrap()
}

fn storage_error(code: u16, reason: &str) -> FakeResponse {
    FakeResponse::json_status(
        code,
        format!(
            r#"{{"error":{{"code":{code},"message":"{reason}","errors":[{{"message":"{reason}","domain":"global","reason":"{reason}"}}]}}}}"#
        ),
    )
}

fn request(data: Bytes) -> GcsUploadRequest {
    GcsUploadRequest {
        path: "p/o.bin".into(),
        data,
        content_type: "application/octet-stream".into(),
    }
}

/// gensupport `uploadChunkWithRetries`: a chunk answered 503 is resent with the same token
/// and growing attempt counts until 32 s after the chunk started; the last 503 is the error.
#[tokio::test(start_paused = true)]
async fn a_failing_chunk_is_retried_until_the_32s_chunk_deadline() {
    let client = resumable_client(|_| storage_error(503, "backendError"));
    let o = scripted_offloader(client.clone());
    let start = tokio::time::Instant::now();
    let err = o
        .upload_content(
            &Context::background(),
            request(Bytes::from(vec![7u8; MIB16 + 1])),
        )
        .await
        .unwrap_err();
    let elapsed = start.elapsed();
    assert_eq!(
        err.to_string(),
        "failed to write the Cloud Storage object: googleapi: Error 503: backendError, backendError"
    );
    assert!(elapsed >= Duration::from_secs(32), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(33), "{elapsed:?}");
    let chunks: Vec<RecordedRequest> = client.requests().into_iter().skip(1).collect();
    assert!(chunks.len() >= 4, "{} chunk requests", chunks.len());
    let token = chunks[0].header("x-goog-gcs-idempotency-token");
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.header("x-goog-gcs-idempotency-token"), token);
        assert!(
            c.header("x-goog-api-client").ends_with(&format!(
                "gccl-invocation-id/{token} gccl-attempt-count/{}",
                i + 1
            )),
            "{}",
            c.header("x-goog-api-client")
        );
        assert_eq!(c.header("content-range"), "bytes 0-16777215/*");
    }
}

/// gensupport `sendAndRetry`: a multipart upload answered 503 is retried until the context
/// ends; the context's error is returned (the last attempt had no transport error).
#[tokio::test(start_paused = true)]
async fn a_multipart_upload_is_retried_until_the_context_ends() {
    let client = ScriptedClient::always(storage_error(503, "backendError"));
    let o = scripted_offloader(client.clone());
    let ctx = Context::background().with_timeout(Duration::from_secs(5));
    let err = o
        .upload_content(&ctx, request(Bytes::from_static(b"abc")))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to write the Cloud Storage object: context deadline exceeded"
    );
    assert!(err.is_context(ContextError::DeadlineExceeded));
    assert!(client.hits() >= 5, "{} attempts", client.hits());
    let tokens: Vec<String> = client
        .requests()
        .iter()
        .map(|r| r.header("x-goog-gcs-idempotency-token"))
        .collect();
    assert!(tokens.iter().all(|t| *t == tokens[0]));
}

/// A refused / reset connection is retried (Go's `*url.Error` "connection reset" text).
#[tokio::test(start_paused = true)]
async fn a_reset_connection_is_retried() {
    let client = ScriptedClient::sequence(vec![
        FakeResponse::reset("connection reset by peer"),
        FakeResponse::json(r#"{"name":"p/o.bin"}"#),
    ]);
    let o = scripted_offloader(client.clone());
    let uri = o
        .upload_content(&Context::background(), request(Bytes::from_static(b"abc")))
        .await
        .unwrap();
    assert_eq!(uri, "gs://b/p/o.bin");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[1]
            .header("x-goog-api-client")
            .contains("gccl-attempt-count/2")
    );
}

/// A transport error Go does not retry ends the upload with Go's `*url.Error` text.
#[tokio::test(start_paused = true)]
async fn an_unretryable_transport_error_is_a_url_error() {
    let client = ScriptedClient::always(FakeResponse::reset("tls handshake failure"));
    let o = scripted_offloader(client.clone());
    let err = o
        .upload_content(&Context::background(), request(Bytes::from_static(b"abc")))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.starts_with(
            "failed to write the Cloud Storage object: Post \"http://fake/upload/storage/v1/b/b/o?alt=json&"
        ),
        "{text}"
    );
    assert!(text.ends_with("tls handshake failure"), "{text}");
    assert_eq!(client.hits(), 1);
}

/// Go `Writer.openWriter`: a context that already ended sends nothing.
#[tokio::test]
async fn an_ended_context_sends_nothing() {
    let client = ScriptedClient::always(FakeResponse::json("{}"));
    let o = scripted_offloader(client.clone());
    let ctx = Context::background().with_cancel();
    ctx.cancel();
    let err = o
        .upload_content(&ctx, request(Bytes::from_static(b"abc")))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to write the Cloud Storage object: context canceled"
    );
    assert!(err.is_context(ContextError::Canceled));
    assert_eq!(client.hits(), 0);
}

/// "X-GUploader-No-308: yes" was sent: a real 308 fails the chunk.
#[tokio::test]
async fn a_308_fails_the_chunk() {
    let client = resumable_client(|_| FakeResponse::new(308));
    let o = scripted_offloader(client);
    let err = o
        .upload_content(
            &Context::background(),
            request(Bytes::from(vec![1u8; MIB16])),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to write the Cloud Storage object: unexpected 308 response status code"
    );
}

/// A 412 on the final chunk is Go's precondition failure too: a success.
#[tokio::test]
async fn a_412_on_the_last_chunk_is_a_success() {
    let client = resumable_client(|n| {
        if n == 1 {
            FakeResponse::new(200).header("X-Http-Status-Code-Override", "308")
        } else {
            storage_error(412, "conditionNotMet")
        }
    });
    let o = scripted_offloader(client.clone());
    let uri = o
        .upload_content(
            &Context::background(),
            request(Bytes::from(vec![1u8; MIB16 + 5])),
        )
        .await
        .unwrap();
    assert_eq!(uri, "gs://b/p/o.bin");
    assert_eq!(client.hits(), 3);
}

/// A 2xx whose body is not JSON fails as Go's decoder does.
#[tokio::test]
async fn an_empty_success_body_is_a_decode_error() {
    let client = ScriptedClient::always(FakeResponse::new(200));
    let o = scripted_offloader(client);
    let err = o
        .upload_content(&Context::background(), request(Bytes::from_static(b"abc")))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to write the Cloud Storage object: unexpected end of JSON input"
    );
}

/// Go `htransport.NewClient`: a `WithHTTPClient` client is used as is, so construction
/// needs no Application Default Credentials and requests carry no `Authorization`, even
/// with credentials given.
#[tokio::test]
async fn a_caller_http_client_skips_credentials() {
    let env = MapEnv(
        [(
            "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
            "/nonexistent/gadk-agentanalytics-adc.json".to_owned(),
        )]
        .into(),
    );
    for credentials in [None, Some(bearer_credentials("t"))] {
        let client = ScriptedClient::json(r#"{"bucket":"b","name":"p/o.bin"}"#);
        let options = ClientOptions {
            credentials,
            endpoint: Some("http://fake/storage/v1/".into()),
            http_client: Some(client.clone()),
            ..ClientOptions::default()
        };
        let o = GcsOffloader::new_with_env("b", &options, &env).unwrap();
        o.upload_content(&Context::background(), request(Bytes::from_static(b"abc")))
            .await
            .unwrap();
        assert!(
            !client.last_request().headers.contains_key("authorization"),
            "{:?}",
            client.last_request().headers
        );
    }
}

#[test]
fn client_options_follow_storage_new_client() {
    let mut env = MapEnv(Default::default());
    let conflict = ClientOptions {
        without_authentication: true,
        credentials: Some(bearer_credentials("t")),
        ..ClientOptions::default()
    };
    assert_eq!(
        GcsOffloader::new_with_env("b", &conflict, &env)
            .unwrap_err()
            .to_string(),
        "failed to create Cloud Storage client: dialing: ClientOptions.without_authentication is \
         incompatible with any option that provides credentials"
    );
    env.0
        .insert("STORAGE_EMULATOR_HOST".into(), "localhost:9023".into());
    let o = GcsOffloader::new_with_env("b", &ClientOptions::default(), &env).unwrap();
    let debug = format!("{o:?}");
    assert!(
        debug.contains(r#"upload_url: "http://localhost:9023/upload/storage/v1""#),
        "{debug}"
    );
    assert!(debug.contains("credentials: false"), "{debug}");
    let bare = ClientOptions {
        endpoint: Some("localhost:9000".into()),
        without_authentication: true,
        ..ClientOptions::default()
    };
    let o = GcsOffloader::new_with_env("b", &bare, &MapEnv(Default::default())).unwrap();
    assert!(
        format!("{o:?}").contains(r#"base_url: "https://localhost:9000/storage/v1/""#),
        "{o:?}"
    );
}
