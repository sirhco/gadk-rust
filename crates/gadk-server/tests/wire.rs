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

//! Replays the ADK-Go wire fixtures (`testdata/wire`, written by `scripts/gen-go-adkrest`)
//! against `Server::router()` on a real loopback socket: one server per configuration, the
//! requests in file-name order, the response read as raw bytes (headers as they arrive on the
//! wire), compared in each fixture's mode (`scripts/gen-go-adkrest/SCENARIO.md`).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Once};

use gadk_engine::testutil::telemetry::ENV_LOCK;
use gadk_platform::env::MapEnv;
use gadk_server::rest::testutil::serve_loopback;
use gadk_server::rest::testutil::wire_scenario::{WireConfig, wire_server};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing_subscriber::layer::SubscriberExt;

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    request: FixtureRequest,
    response: FixtureResponse,
    compare: String,
}

#[derive(Debug, Deserialize)]
struct FixtureRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Debug, Deserialize)]
struct FixtureResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

/// A response as read off the socket: status, headers (Go canonical names, sorted, without
/// `Date`, `Content-Length` and `Transfer-Encoding`) and the transfer-decoded body.
#[derive(Debug, PartialEq)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

/// The fixtures of `config`, in file-name order.
fn fixtures(config: WireConfig) -> Vec<Fixture> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/wire");
    let prefix = format!("{}_", config.name());
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".json"))
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

/// Go `textproto.CanonicalMIMEHeaderKey`: each `-`-separated word capitalised.
fn canonical(name: &str) -> String {
    name.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// The request bytes Go's `net/http` client sends: request line, `Host` (the fixture's or the
/// listener's), `User-Agent: Go-http-client/1.1`, the fixture's headers, and `Content-Length`
/// when there is a body or the method is POST, PUT or PATCH.
fn request_bytes(req: &FixtureRequest, addr: SocketAddr) -> Vec<u8> {
    let host = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map_or_else(|| addr.to_string(), |(_, v)| v.clone());
    let mut out = format!(
        "{} {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Go-http-client/1.1\r\n",
        req.method, req.target
    );
    for (k, v) in req
        .headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("host"))
    {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if !req.body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH") {
        out.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(req.body.as_bytes());
    bytes
}

/// Sends `req` on a fresh connection and reads one response.
async fn exchange(addr: SocketAddr, req: &FixtureRequest) -> Answer {
    let mut conn = TcpStream::connect(addr).await.unwrap();
    conn.write_all(&request_bytes(req, addr)).await.unwrap();
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let mut chunk = [0u8; 4096];
        let n = conn.read(&mut chunk).await.unwrap();
        assert!(n > 0, "connection closed before the response head");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8(buf[..head_end].to_vec()).unwrap();
    let mut rest = buf[head_end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut raw_headers: Vec<(String, String)> = lines
        .map(|l| {
            let (k, v) = l.split_once(':').unwrap();
            (canonical(k.trim()), v.trim().to_owned())
        })
        .collect();
    let header = |name: &str| {
        raw_headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let bodiless = req.method == "HEAD" || status == 204 || status == 304 || status < 200;
    let body = if bodiless {
        Vec::new()
    } else if header("Transfer-Encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        read_chunked(&mut conn, rest).await
    } else if let Some(len) = header("Content-Length") {
        let len: usize = len.parse().unwrap();
        while rest.len() < len {
            let mut chunk = [0u8; 4096];
            let n = conn.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed inside the body");
            rest.extend_from_slice(&chunk[..n]);
        }
        rest.truncate(len);
        rest
    } else {
        conn.read_to_end(&mut rest).await.unwrap();
        rest
    };
    raw_headers
        .retain(|(k, _)| !matches!(k.as_str(), "Date" | "Content-Length" | "Transfer-Encoding"));
    raw_headers.sort();
    Answer {
        status,
        headers: raw_headers,
        body: String::from_utf8(body).unwrap(),
    }
}

/// Decodes a chunked body whose first bytes are `buf`, reading the rest from `conn`.
async fn read_chunked(conn: &mut TcpStream, mut buf: Vec<u8>) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let line_end = loop {
            if let Some(i) = buf.windows(2).position(|w| w == b"\r\n") {
                break i;
            }
            let mut chunk = [0u8; 4096];
            let n = conn.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed inside a chunk header");
            buf.extend_from_slice(&chunk[..n]);
        };
        let size_text = String::from_utf8(buf[..line_end].to_vec()).unwrap();
        let size = usize::from_str_radix(size_text.split(';').next().unwrap().trim(), 16).unwrap();
        buf.drain(..line_end + 2);
        while buf.len() < size + 2 {
            let mut chunk = [0u8; 4096];
            let n = conn.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed inside a chunk");
            buf.extend_from_slice(&chunk[..n]);
        }
        if size == 0 {
            return body;
        }
        body.extend_from_slice(&buf[..size]);
        buf.drain(..size + 2);
    }
}

/// SCENARIO.md's `debug_trace` normalisation: every trace/span/parent-span ID value numbered in
/// order of first appearance and replaced wherever it appears as a string, and every
/// `start_time`, `end_time` and `observed_timestamp` value replaced by `"<t>"`.
fn normalise_debug_trace(mut v: Value) -> Value {
    fn collect(v: &Value, ids: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if matches!(k.as_str(), "trace_id" | "span_id" | "parent_span_id")
                        && let Value::String(s) = x
                        && !ids.contains(s)
                    {
                        ids.push(s.clone());
                    }
                    collect(x, ids);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| collect(x, ids)),
            _ => {}
        }
    }
    fn replace(v: &mut Value, ids: &BTreeMap<String, String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if matches!(k.as_str(), "start_time" | "end_time" | "observed_timestamp") {
                        *x = Value::from("<t>");
                    } else {
                        replace(x, ids);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| replace(x, ids)),
            Value::String(s) => {
                if let Some(id) = ids.get(s.as_str()) {
                    *s = id.clone();
                }
            }
            _ => {}
        }
    }
    // Document order of the parsed value; the numbering depends only on the document's shape,
    // so both sides number corresponding IDs alike.
    let mut ids = Vec::new();
    collect(&v, &mut ids);
    let ids: BTreeMap<String, String> = ids
        .into_iter()
        .enumerate()
        .map(|(i, s)| (s, format!("<id-{i}>")))
        .collect();
    replace(&mut v, &ids);
    v
}

/// The top-level keys of a JSON object body.
fn key_set(body: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(body).unwrap();
    v.as_object().unwrap().keys().cloned().collect()
}

/// A JSON array body as a sorted list of its elements' JSON texts.
fn sorted_array(body: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(body).unwrap();
    let mut items: Vec<String> = v.as_array().unwrap().iter().map(Value::to_string).collect();
    items.sort();
    items
}

/// `None` when `got` matches `want` in `mode`, else a description of the difference.
fn mismatch(mode: &str, want: &FixtureResponse, got: &Answer) -> Option<String> {
    let mut want_headers = want.headers.clone();
    want_headers.sort();
    if (want.status, &want_headers) != (got.status, &got.headers) {
        return Some(format!(
            "status/headers: want {} {:?}\n                   got {} {:?}\nbody: {}",
            want.status, want_headers, got.status, got.headers, got.body
        ));
    }
    let same = match mode {
        "exact" => want.body == got.body,
        "json_key_set" => key_set(&want.body) == key_set(&got.body),
        "json_sorted_array" => sorted_array(&want.body) == sorted_array(&got.body),
        "sse" => {
            want.body.split("\n\n").collect::<Vec<_>>()
                == got.body.split("\n\n").collect::<Vec<_>>()
        }
        "debug_trace" => {
            let parse = |b: &str| normalise_debug_trace(serde_json::from_str(b).unwrap());
            parse(&want.body) == parse(&got.body)
        }
        other => panic!("unknown compare mode {other}"),
    };
    (!same).then(|| {
        if mode == "debug_trace" {
            let pretty = |b: &str| {
                serde_json::to_string_pretty(&normalise_debug_trace(
                    serde_json::from_str(b).unwrap(),
                ))
                .unwrap()
            };
            format!(
                "body ({mode}):\n--- want\n{}\n--- got\n{}",
                pretty(&want.body),
                pretty(&got.body)
            )
        } else {
            format!(
                "body ({mode}):\n--- want\n{}\n--- got\n{}",
                want.body, got.body
            )
        }
    })
}

/// Installs (once per process) a no-op global subscriber, so callsite interest never collapses
/// to "off" while the `main` replay's thread-default subscriber comes and goes.
fn pin_tracing_interest() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    });
}

/// Replays every fixture of `config` against a fresh server and fails with every mismatch.
async fn replay(config: WireConfig) {
    pin_tracing_interest();
    let server = wire_server(config).unwrap();
    // The main configuration's debug store sees the engine's spans and log records, as the
    // probe registers `SpanProcessor()`/`LogProcessor()` on the global providers.
    let subscriber = tracing_subscriber::registry().with(server.debug_layer());
    let _guard = tracing::subscriber::set_default(subscriber);

    let addr = serve_loopback(server.router()).await.unwrap();

    let mut failures = Vec::new();
    let all = fixtures(config);
    assert!(!all.is_empty(), "no {} fixtures", config.name());
    for f in &all {
        let got = exchange(addr, &f.request).await;
        if let Some(diff) = mismatch(&f.compare, &f.response, &got) {
            failures.push(format!("{}_{}: {diff}", config.name(), f.name));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} {} fixtures differ from ADK-Go:\n\n{}",
        failures.len(),
        all.len(),
        config.name(),
        failures.join("\n\n")
    );
}

/// The `main` configuration (67 fixtures): every route, the debug store, the router's own
/// answers and the origin guard.
#[tokio::test]
async fn wire_main_matches_go() {
    // Both telemetry variables unset, as in the probe. No other test of this binary changes
    // the provider, so the lock is not held across the replay.
    {
        let _env = ENV_LOCK.lock();
        gadk_engine::telemetry::set_env_provider(Arc::new(MapEnv::default()));
    }
    replay(WireConfig::Main).await;
}

/// The `strict` configuration (11 fixtures): header authentication and the strict authorizer.
#[tokio::test]
async fn wire_strict_matches_go() {
    replay(WireConfig::Strict).await;
}

/// The `custom` configuration (4 fixtures): a custom authenticator's outcomes.
#[tokio::test]
async fn wire_custom_matches_go() {
    replay(WireConfig::Custom).await;
}
