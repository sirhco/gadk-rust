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

//! A local web console for gadk: stored traces from Cloud Trace and Cloud Logging, and the
//! prompt registry.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use crate::traces::CloudTraceSource;

#[allow(dead_code)] // removed in Task 8 once the web layer uses it
mod model;
#[allow(dead_code)] // removed in Task 8
mod prompts;
pub mod traces;
#[allow(dead_code)] // removed in Task 8
mod web;

/// Console settings (from the command line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleConfig {
    /// GCP project whose traces and logs are read.
    pub project: String,
    /// Prompt store URL (`file://…` or `firestore://…`); `None` disables the prompt pages.
    pub store: Option<String>,
    /// Loopback port; 0 picks a free one.
    pub port: u16,
    /// Cloud Logging log id that gadk-telemetry writes (default `gadk`).
    pub log_id: String,
}

/// Startup failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsoleError {
    #[error("prompt store: {0}")]
    Store(#[from] gadk_prompts::StoreError),
    #[error("bind 127.0.0.1:{port}: {detail}")]
    Bind { port: u16, detail: String },
    #[error("serve: {0}")]
    Serve(String),
}

/// A console bound to its port, not yet serving.
pub struct Bound {
    listener: tokio::net::TcpListener,
    addr: SocketAddr,
    router: axum::Router,
}

impl Bound {
    /// The bound address (127.0.0.1:<port>), the real port when the config said 0.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Serves until the process ends.
    pub async fn serve(self) -> Result<(), ConsoleError> {
        axum::serve(self.listener, self.router)
            .await
            .map_err(|e| ConsoleError::Serve(e.to_string()))
    }
}

/// Opens the store (if any), binds 127.0.0.1:<port>, builds the router.
///
/// The trace source builds its ADC credentials on the first GCP call, so binding needs none.
pub async fn bind(cfg: ConsoleConfig) -> Result<Bound, ConsoleError> {
    let store = cfg
        .store
        .as_deref()
        .map(gadk_prompts_cli::open_store)
        .transpose()?;
    let bind_err = |e: std::io::Error| ConsoleError::Bind {
        port: cfg.port,
        detail: e.to_string(),
    };
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, cfg.port))
        .await
        .map_err(bind_err)?;
    let addr = listener.local_addr().map_err(bind_err)?;
    let state = web::AppState {
        source: Arc::new(CloudTraceSource::new(
            cfg.project.clone(),
            cfg.log_id.clone(),
        )),
        store,
        csrf: Arc::from(csrf_token()),
        port: addr.port(),
        now: chrono::Utc::now,
    };
    Ok(Bound {
        listener,
        addr,
        router: web::router(state),
    })
}

/// `bind`, writes `http://127.0.0.1:<port>/` and a newline to `out`, then `serve`.
pub async fn run(
    cfg: ConsoleConfig,
    out: &mut (dyn std::io::Write + Send),
) -> Result<(), ConsoleError> {
    let bound = bind(cfg).await?;
    writeln!(out, "http://127.0.0.1:{}/", bound.local_addr().port())
        .and_then(|()| out.flush())
        .map_err(|e| ConsoleError::Serve(format!("print the URL: {e}")))?;
    bound.serve().await
}

/// 32 random bytes, hex-encoded (64 characters).
fn csrf_token() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{ConsoleConfig, ConsoleError, bind, run};

    fn cfg(store: Option<&str>) -> ConsoleConfig {
        ConsoleConfig {
            project: "p".into(),
            store: store.map(str::to_owned),
            port: 0,
            log_id: "gadk".into(),
        }
    }

    /// One raw HTTP/1.1 GET of `/` to 127.0.0.1:`port` with the given Host; the response head.
    async fn raw_get(port: u16, host: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn bind_port_zero_serves_on_the_real_port() {
        let bound = bind(cfg(None)).await.unwrap();
        let addr = bound.local_addr();
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0);
        let server = tokio::spawn(bound.serve());

        let resp = raw_get(addr.port(), &format!("127.0.0.1:{}", addr.port())).await;
        assert!(resp.starts_with("HTTP/1.1 303"), "{resp}");
        assert!(
            resp.to_ascii_lowercase().contains("location: /traces"),
            "{resp}"
        );

        let resp = raw_get(addr.port(), "127.0.0.1:1").await;
        assert!(resp.starts_with("HTTP/1.1 421"), "{resp}");
        server.abort();
    }

    #[tokio::test]
    async fn bad_store_url_is_a_store_error() {
        let err = bind(cfg(Some("bogus://x"))).await.err().unwrap();
        assert!(matches!(err, ConsoleError::Store(_)), "{err}");
    }

    #[tokio::test]
    async fn file_store_url_binds() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("file://{}", dir.path().display());
        assert!(bind(cfg(Some(&url))).await.is_ok());
    }

    /// A writer that forwards every write to a channel.
    struct ChanWriter(tokio::sync::mpsc::UnboundedSender<Vec<u8>>);

    impl std::io::Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_prints_the_url_then_serves() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut out = ChanWriter(tx);
            run(cfg(None), &mut out).await
        });
        let mut printed = Vec::new();
        while !printed.ends_with(b"\n") {
            printed.extend(rx.recv().await.unwrap());
        }
        let line = String::from_utf8(printed).unwrap();
        let port: u16 = line
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("/\n"))
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(port, 0);

        let resp = raw_get(port, &format!("localhost:{port}")).await;
        assert!(resp.starts_with("HTTP/1.1 303"), "{resp}");
        server.abort();
    }
}
