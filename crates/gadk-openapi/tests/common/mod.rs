// Copyright 2026 Chris Olson
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

//! A minimal HTTP/1.1 test server, cut down from gadk-auth's `tests/common/mod.rs`.
//!
//! The server binds `127.0.0.1:0`, serves one HTTP/1.1 request per connection and answers with
//! `Connection: close` and a `Content-Length`. It reads request bodies by `Content-Length`, and
//! records each request's path exactly as sent (still percent-encoded).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::sync::Arc;

use http::{HeaderMap, HeaderName, HeaderValue};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// One request as the server received it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    /// The path as sent: percent-encoded, no query.
    pub path: String,
    /// The raw query string after `?`, if any.
    pub query: Option<String>,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The first value of header `name` (case-insensitive), if it is visible ASCII.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The body decoded as JSON; panics on bad JSON (tests only).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "request body is not JSON ({e}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

/// What the handler answers: status, headers, body.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    /// 200 with `body`.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::status(200, body)
    }

    /// `status` with `body`.
    pub fn status(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// 200, `Content-Type: application/json`.
    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        Self::ok(body).with_header("Content-Type", "application/json")
    }

    /// Adds a response header (`Content-Length` and `Connection` are always the server's own).
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A local HTTP server that records requests and answers with a handler.
pub struct TestServer {
    url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    accept: JoinHandle<()>,
}

impl TestServer {
    /// Starts the server. The handler is async, so it can sleep or never resolve (a hang).
    /// Requests are recorded on arrival, before the handler runs.
    pub async fn start<F, Fut>(handler: F) -> TestServer
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind 127.0.0.1:0");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let handler = Arc::new(handler);
        let accept = tokio::spawn(async move {
            // Dropping the set (when the accept loop is aborted) aborts every connection task.
            let mut conns = JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                while conns.try_join_next().is_some() {}
                conns.spawn(serve(stream, handler.clone(), recorded.clone()));
            }
        });
        TestServer {
            url,
            requests,
            accept,
        }
    }

    /// Answers every request with `reply`.
    pub async fn replying(reply: Reply) -> TestServer {
        Self::start(move |_req| {
            let reply = reply.clone();
            async move { reply }
        })
        .await
    }

    /// "http://127.0.0.1:<port>", no trailing slash.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Everything received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().clone()
    }

    /// `requests().len()`.
    pub fn calls(&self) -> usize {
        self.requests.lock().len()
    }
}

impl Drop for TestServer {
    // Aborts the accept loop (and with it every open connection).
    fn drop(&mut self) {
        self.accept.abort();
    }
}

async fn serve<F, Fut>(
    stream: TcpStream,
    handler: Arc<F>,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
) where
    F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send + 'static,
{
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let Some(req) = read_request(&mut reader).await else {
        return;
    };
    recorded.lock().push(req.clone());
    let reply = handler(req).await;
    let _ = write.write_all(&encode_reply(&reply)).await;
    let _ = write.shutdown().await;
}

/// One line up to and including `\n`, without the line ending; `None` at EOF or on an I/O error.
async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    if r.read_until(b'\n', &mut line).await.ok()? == 0 {
        return None;
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Some(line)
}

async fn read_request<R: AsyncBufRead + Unpin>(r: &mut R) -> Option<RecordedRequest> {
    let start = String::from_utf8(read_line(r).await?).ok()?;
    let mut parts = start.splitn(3, ' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (target, None),
    };
    let mut headers = HeaderMap::new();
    loop {
        let line = read_line(r).await?;
        if line.is_empty() {
            break;
        }
        let colon = line.iter().position(|&b| b == b':')?;
        let name = HeaderName::from_bytes(line[..colon].trim_ascii()).ok()?;
        let value = HeaderValue::from_bytes(line[colon + 1..].trim_ascii()).ok()?;
        headers.append(name, value);
    }
    let len = headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body).await.ok()?;
    Some(RecordedRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn encode_reply(reply: &Reply) -> Vec<u8> {
    let reason = http::StatusCode::from_u16(reply.status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("Status");
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", reply.status);
    for (name, value) in &reply.headers {
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("connection") {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        reply.body.len()
    ));
    let mut out = head.into_bytes();
    out.extend_from_slice(&reply.body);
    out
}
