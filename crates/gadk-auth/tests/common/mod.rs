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

//! A minimal HTTP/1.1 test server, the stand-in for Go's `httptest.Server` (plan D6, C14).
//!
//! Integration-test targets use it as `mod common;`; the crate's own `#[cfg(test)]` code reaches
//! the same file as `crate::testserver` (`src/testserver.rs` includes it by `#[path]`).
//!
//! The server binds `127.0.0.1:0`, serves one HTTP/1.1 request per connection and answers with
//! `Connection: close` and a `Content-Length`. It reads request bodies by `Content-Length` or
//! `Transfer-Encoding: chunked`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use http::{HeaderMap, HeaderName, HeaderValue};
use parking_lot::Mutex;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// One request as the server received it (Go handlers read `r.URL.Path`, `r.Header`, `r.Body`).
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    /// Go `r.URL.Path`: percent-decoded, no query.
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

    /// The body as lossy UTF-8.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body decoded as JSON; panics on bad JSON (tests only).
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "request body is not the expected JSON ({e}): {}",
                self.text()
            )
        })
    }
}

/// What the handler answers: status, headers, body (any size; >1 MiB bodies are used).
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

    /// `status` (e.g. 307) with `Location: location` and no body (Go `http.Redirect`).
    pub fn redirect(location: impl Into<String>, status: u16) -> Self {
        Self::status(status, Vec::new()).with_header("Location", location)
    }

    /// Adds a response header (`Content-Length` and `Connection` are always the server's own).
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Go `httptest.Server`.
pub struct TestServer {
    url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    accept: JoinHandle<()>,
}

impl TestServer {
    /// Go `httptest.NewServer(handler)`. The handler is async, so it can sleep, wait on a
    /// `Notify`/channel (Go's `hold` channels) or never resolve (a hang). Requests are recorded on
    /// arrival, before the handler runs.
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

    /// Go client_test.go `sequenceServer`: 200 with `bodies[i]`, repeating the last.
    pub async fn sequence(bodies: &[&str]) -> TestServer {
        assert!(!bodies.is_empty(), "sequence needs at least one body");
        let bodies: Vec<String> = bodies.iter().map(|b| (*b).to_string()).collect();
        let n = AtomicUsize::new(0);
        Self::start(move |_req| {
            let i = n.fetch_add(1, Ordering::SeqCst).min(bodies.len() - 1);
            let body = bodies[i].clone();
            async move { Reply::ok(body) }
        })
        .await
    }

    /// Go `srv.URL`: "http://127.0.0.1:<port>", no trailing slash.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Everything received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().clone()
    }

    /// `requests().len()` (Go's atomic call counters).
    pub fn calls(&self) -> usize {
        self.requests.lock().len()
    }

    /// Go `srv.Client()`: a `reqwest::Client` with `no_proxy()`.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build test client")
    }
}

impl Drop for TestServer {
    // Go srv.Close(): aborts the accept loop (and with it every open connection).
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
    let (raw_path, query) = match target.split_once('?') {
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
    let chunked = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .any(|v| v.as_bytes().eq_ignore_ascii_case(b"chunked"));
    let body = if chunked {
        read_chunked(r).await?
    } else {
        let len = headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        r.read_exact(&mut body).await.ok()?;
        body
    };
    Some(RecordedRequest {
        method,
        path: percent_decode(&raw_path),
        query,
        headers,
        body,
    })
}

async fn read_chunked<R: AsyncBufRead + Unpin>(r: &mut R) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = String::from_utf8(read_line(r).await?).ok()?;
        let size = usize::from_str_radix(line.split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            // Trailers, up to the empty line.
            while !read_line(r).await?.is_empty() {}
            return Some(body);
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..]).await.ok()?;
        read_line(r).await?; // the CRLF after the chunk
    }
}

/// Go `url.PathUnescape`-style decoding of `%XX` (a malformed escape is kept as is).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
