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

//! The `cloud.google.com/go/compute/metadata` v0.10.0 subset the detector and ADC use:
//! `OnGCE`, `Get`, `ProjectID`, `InstanceName`, `Hostname`, `Zone` and `InstanceAttributeValue`,
//! with Go's error texts.

use std::collections::hash_map::RandomState;
use std::error::Error as _;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use gadk_platform::env::EnvProvider;

/// Go `metadataHostEnv`.
pub const METADATA_HOST_ENV: &str = "GCE_METADATA_HOST";

/// Go `metadataIP`.
pub const METADATA_IP: &str = "169.254.169.254";

/// Go `userAgent`.
const USER_AGENT: &str = "gcloud-golang/0.1";

/// Go `defaultClient`'s `http.Client.Timeout`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Go `defaultClient`'s dial timeout.
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);

/// Go `maxRetryAttempts`.
const MAX_RETRY_ATTEMPTS: u32 = 5;

/// Go `newRetryer`'s initial backoff.
const BACKOFF_INITIAL: Duration = Duration::from_millis(100);

/// Go `newRetryer`'s maximum backoff.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long `OnGCE` waits for the second probe when the system suggests GCE (Go
/// `OnGCEWithContext`'s `5*time.Second`).
const TRY_HARDER_WAIT: Duration = Duration::from_secs(5);

/// A metadata lookup failure; `Display` is Go's text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// Go `NotDefinedError` (HTTP 404): `metadata: GCE metadata "<suffix>" not defined`.
    #[error("metadata: GCE metadata \"{0}\" not defined")]
    NotDefined(String),
    /// Go `*metadata.Error` (any other non-200 status): ``compute: Received <code> `<body>` ``.
    #[error("compute: Received {code} `{message}`")]
    Status {
        /// The HTTP status code.
        code: u16,
        /// The response body.
        message: String,
    },
    /// The request failed before a response (Go's transport error).
    #[error("{0}")]
    Transport(String),
}

/// Go `*metadata.Client`: `GET <base>/computeMetadata/v1/<suffix>` with `Metadata-Flavor: Google`,
/// where `<base>` is `http://$GCE_METADATA_HOST` (from the injected environment), else
/// `http://169.254.169.254`, or the injected base URL.
#[derive(Clone)]
pub struct MetadataClient {
    client: reqwest::Client,
    env: Arc<dyn EnvProvider>,
    base_url: Option<String>,
}

impl MetadataClient {
    /// Go `metadata.NewClient(nil)` reading `GCE_METADATA_HOST` from `env`.
    pub fn new(env: Arc<dyn EnvProvider>) -> Self {
        Self {
            client: default_http_client(),
            env,
            base_url: None,
        }
    }

    /// A client sending every request to `base_url` (e.g. `http://127.0.0.1:<port>`, a test
    /// server) instead of the metadata host; `env` still decides [`MetadataClient::on_gce`].
    pub fn with_base_url(env: Arc<dyn EnvProvider>, base_url: impl Into<String>) -> Self {
        Self {
            client: default_http_client(),
            env,
            base_url: Some(base_url.into().trim_end_matches('/').to_owned()),
        }
    }

    /// Go `OnGCE`: `true` when `GCE_METADATA_HOST` is set (non-empty), else the result of
    /// probing the metadata server raced with a DNS lookup of `metadata.google.internal.`.
    ///
    /// Divergence (ledger 7a): with an injected base URL only that URL is probed (no DNS
    /// lookup). Unlike Go's package-level `OnGCE`, the answer is not cached for the process.
    pub async fn on_gce(&self) -> bool {
        if self
            .env
            .var(METADATA_HOST_ENV)
            .is_some_and(|host| !host.is_empty())
        {
            return true;
        }
        let probe_url = match &self.base_url {
            Some(base) => {
                return metadata_request_strategy(self.client.clone(), base.clone()).await;
            }
            None => format!("http://{METADATA_IP}"),
        };
        let mut probes = tokio::task::JoinSet::new();
        probes.spawn(metadata_request_strategy(self.client.clone(), probe_url));
        probes.spawn(dns_request_strategy());
        let first = matches!(probes.join_next().await, Some(Ok(true)));
        if system_info_suggests_gce() {
            if first {
                return true;
            }
            return matches!(
                tokio::time::timeout(TRY_HARDER_WAIT, probes.join_next()).await,
                Ok(Some(Ok(true)))
            );
        }
        // Dropping the set aborts the slower probe (Go cancels its context).
        first
    }

    /// Go `GetWithContext`: the untrimmed value at `suffix`, with Go's retries.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn get(&self, suffix: &str) -> Result<String, MetadataError> {
        let suffix = suffix.trim_start_matches('/');
        let base = match &self.base_url {
            Some(base) => base.clone(),
            None => {
                let host = self
                    .env
                    .var(METADATA_HOST_ENV)
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| METADATA_IP.to_owned());
                format!("http://{host}")
            }
        };
        let url = format!("{base}/computeMetadata/v1/{suffix}");
        let mut retryer = Retryer::new();
        loop {
            let sent = self
                .client
                .get(&url)
                .header("Metadata-Flavor", "Google")
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .send()
                .await;
            match sent {
                Ok(res) => {
                    let code = res.status().as_u16();
                    let body = res
                        .bytes()
                        .await
                        .map_err(|e| MetadataError::Transport(transport_text(&url, &e)))?;
                    if code == 200 {
                        return Ok(String::from_utf8_lossy(&body).into_owned());
                    }
                    if let Some(pause) = retryer.retry(should_retry_status(code)) {
                        tokio::time::sleep(pause).await;
                        continue;
                    }
                    if code == 404 {
                        return Err(MetadataError::NotDefined(suffix.to_owned()));
                    }
                    return Err(MetadataError::Status {
                        code,
                        message: String::from_utf8_lossy(&body).into_owned(),
                    });
                }
                Err(e) => {
                    if let Some(pause) = retryer.retry(should_retry_error(&e)) {
                        tokio::time::sleep(pause).await;
                        continue;
                    }
                    return Err(MetadataError::Transport(transport_text(&url, &e)));
                }
            }
        }
    }

    /// Go `getTrimmed`.
    async fn get_trimmed(&self, suffix: &str) -> Result<String, MetadataError> {
        self.get(suffix).await.map(|s| s.trim().to_owned())
    }

    /// Go `ProjectIDWithContext`: `project/project-id`, trimmed.
    ///
    /// Divergence (ledger 7a): Go caches the value for the process; gadk does not.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn project_id(&self) -> Result<String, MetadataError> {
        self.get_trimmed("project/project-id").await
    }

    /// Go `InstanceNameWithContext`: `instance/name`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn instance_name(&self) -> Result<String, MetadataError> {
        self.get_trimmed("instance/name").await
    }

    /// Go `HostnameWithContext`: `instance/hostname`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn hostname(&self) -> Result<String, MetadataError> {
        self.get_trimmed("instance/hostname").await
    }

    /// Go `ZoneWithContext`: the last path segment of `instance/zone`, trimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn zone(&self) -> Result<String, MetadataError> {
        let zone = self.get_trimmed("instance/zone").await?;
        Ok(match zone.rfind('/') {
            Some(i) => zone[i + 1..].to_owned(),
            None => zone,
        })
    }

    /// Go `InstanceAttributeValueWithContext`: `instance/attributes/<attr>`, untrimmed.
    ///
    /// # Errors
    ///
    /// [`MetadataError`].
    pub async fn instance_attribute_value(&self, attr: &str) -> Result<String, MetadataError> {
        self.get(&format!("instance/attributes/{attr}")).await
    }
}

impl fmt::Debug for MetadataClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

/// Go `newDefaultHTTPClient(true)`: 2 s dial timeout, 5 s request timeout and no proxy (Go's
/// transport sets no `Proxy`).
fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(DIAL_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .no_proxy()
        .build()
        .unwrap_or_default()
}

/// Go `metadataRequestStrategy`: the server answers `GET <url>` with `Metadata-Flavor: Google`.
async fn metadata_request_strategy(client: reqwest::Client, url: String) -> bool {
    match client
        .get(&url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
    {
        Ok(res) => res
            .headers()
            .get("Metadata-Flavor")
            .is_some_and(|v| v.as_bytes() == b"Google"),
        Err(_) => false,
    }
}

/// Go `dnsRequestStrategy`: `metadata.google.internal.` resolves to the metadata IP.
async fn dns_request_strategy() -> bool {
    let metadata_ip = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));
    match tokio::net::lookup_host(("metadata.google.internal.", 0)).await {
        Ok(mut addrs) => addrs.any(|a| a.ip() == metadata_ip),
        Err(_) => false,
    }
}

/// Go `systemInfoSuggestsGCE`: Linux reads the DMI product name; other systems say no.
///
/// Divergence (ledger 7a): Go reads the Windows registry's `SystemProductName`; gadk answers
/// `false` on Windows (the probes still decide).
fn system_info_suggests_gce() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let name = std::fs::read_to_string("/sys/class/dmi/id/product_name").unwrap_or_default();
    let name = name.trim();
    name == "Google" || name == "Google Compute Engine"
}

/// Go `metadataRetryer` over `defaultBackoff`.
struct Retryer {
    attempts: u32,
    cur: Duration,
}

impl Retryer {
    fn new() -> Self {
        Self {
            attempts: 0,
            cur: BACKOFF_INITIAL,
        }
    }

    /// Go `Retry`: the pause before the next attempt, or `None` to stop.
    fn retry(&mut self, retryable: bool) -> Option<Duration> {
        if !retryable || self.attempts == MAX_RETRY_ATTEMPTS {
            return None;
        }
        self.attempts += 1;
        Some(self.pause())
    }

    /// Go `defaultBackoff.Pause`: uniform in `[1ns, cur]`, then `cur` doubles up to the max.
    fn pause(&mut self) -> Duration {
        let cur = u64::try_from(self.cur.as_nanos())
            .unwrap_or(u64::MAX)
            .max(1);
        let random = RandomState::new().build_hasher().finish();
        let pause = Duration::from_nanos(1 + random % cur);
        self.cur = (self.cur * 2).min(BACKOFF_MAX);
        pause
    }
}

/// Go `shouldRetry` on a status code.
fn should_retry_status(code: u16) -> bool {
    (500..=599).contains(&code) || code == 429
}

/// Go `shouldRetry` on a transport error: timeouts and temporary socket errors (Go
/// `Temporary()`: `ECONNRESET`, `ECONNABORTED`, `ETIMEDOUT`), an unexpected EOF, and on Linux
/// also `ECONNREFUSED` (`retry_linux.go`).
fn should_retry_error(err: &reqwest::Error) -> bool {
    if err.is_timeout() {
        return true;
    }
    let mut source = err.source();
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::UnexpectedEof => return true,
                std::io::ErrorKind::ConnectionRefused if cfg!(target_os = "linux") => return true,
                _ => {}
            }
        }
        source = e.source();
    }
    false
}

/// Go's `*url.Error` text, `Get "<url>": <cause>`, with the innermost cause's text.
///
/// Divergence (ledger 7a): the cause is reqwest's innermost error text, not Go's `net/http`
/// transport text; only the `Get "<url>": ` shape is Go's.
pub(crate) fn transport_text(url: &str, err: &(dyn std::error::Error + 'static)) -> String {
    let mut cause: &(dyn std::error::Error + 'static) = err;
    while let Some(inner) = cause.source() {
        cause = inner;
    }
    format!("Get \"{url}\": {cause}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;

    use super::*;
    use crate::test_support::env;

    /// A metadata server answering `routes` (path → (status, body)), recording each request's
    /// path and headers, and answering `/` with `Metadata-Flavor: <flavor>`.
    struct Server {
        url: String,
        seen: Arc<Mutex<Vec<(String, http::HeaderMap)>>>,
    }

    /// Each path's answers (status, body), in order.
    type Routes = HashMap<String, Mutex<Vec<(u16, String)>>>;

    async fn serve(routes: &[(&str, u16, &str)], flavor: &'static str) -> Server {
        let routes: Arc<Routes> = Arc::new({
            let mut m = Routes::new();
            for (path, code, body) in routes {
                m.entry((*path).to_owned())
                    .or_default()
                    .get_mut()
                    .unwrap()
                    .push((*code, (*body).to_owned()));
            }
            m
        });
        let seen: Arc<Mutex<Vec<(String, http::HeaderMap)>>> = Arc::default();
        let log = Arc::clone(&seen);
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let routes = Arc::clone(&routes);
            let log = Arc::clone(&log);
            async move {
                let path = req.uri().path().to_owned();
                log.lock()
                    .unwrap()
                    .push((path.clone(), req.headers().clone()));
                if path == "/" {
                    return axum::response::Response::builder()
                        .header("Metadata-Flavor", flavor)
                        .body(axum::body::Body::empty())
                        .unwrap();
                }
                // Each route answers its responses in order, repeating the last one.
                let (code, body) = match routes.get(&path) {
                    Some(answers) => {
                        let mut answers = answers.lock().unwrap();
                        if answers.len() > 1 {
                            answers.remove(0)
                        } else {
                            answers[0].clone()
                        }
                    }
                    None => (404, String::new()),
                };
                axum::response::Response::builder()
                    .status(code)
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { url, seen }
    }

    fn client(server: &Server) -> MetadataClient {
        MetadataClient::with_base_url(env(&[]), &server.url)
    }

    /// A base URL nothing listens on.
    async fn unreachable_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        url
    }

    #[tokio::test]
    async fn get_is_untrimmed_and_sends_gos_headers() {
        let server = serve(&[("/computeMetadata/v1/instance/id", 200, " 42\n")], "").await;
        assert_eq!(client(&server).get("/instance/id").await.unwrap(), " 42\n");
        let seen = server.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "/computeMetadata/v1/instance/id");
        assert_eq!(seen[0].1["metadata-flavor"], "Google");
        assert_eq!(seen[0].1["user-agent"], "gcloud-golang/0.1");
    }

    #[tokio::test]
    async fn not_found_is_gos_not_defined_error() {
        let server = serve(&[], "").await;
        let err = client(&server).get("/instance/zone").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"metadata: GCE metadata "instance/zone" not defined"#
        );
        assert_eq!(err, MetadataError::NotDefined("instance/zone".to_owned()));
    }

    #[tokio::test]
    async fn another_status_is_gos_error_text_without_retry() {
        let server = serve(&[("/computeMetadata/v1/x", 403, "forbidden")], "").await;
        let err = client(&server).get("x").await.unwrap_err();
        assert_eq!(err.to_string(), "compute: Received 403 `forbidden`");
        assert_eq!(server.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn server_errors_and_429_are_retried() {
        let server = serve(
            &[
                ("/computeMetadata/v1/x", 503, "busy"),
                ("/computeMetadata/v1/x", 429, "slow down"),
                ("/computeMetadata/v1/x", 200, "ok"),
            ],
            "",
        )
        .await;
        assert_eq!(client(&server).get("x").await.unwrap(), "ok");
        assert_eq!(server.seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn retries_stop_after_five() {
        let server = serve(&[("/computeMetadata/v1/x", 500, "down")], "").await;
        let err = client(&server).get("x").await.unwrap_err();
        assert_eq!(err.to_string(), "compute: Received 500 `down`");
        assert_eq!(server.seen.lock().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn trimmed_lookups_and_the_zones_last_segment() {
        let server = serve(
            &[
                ("/computeMetadata/v1/project/project-id", 200, " p \n"),
                ("/computeMetadata/v1/instance/name", 200, "n\n"),
                (
                    "/computeMetadata/v1/instance/hostname",
                    200,
                    "\th.example\n",
                ),
                (
                    "/computeMetadata/v1/instance/zone",
                    200,
                    "projects/1/zones/us-central1-a\n",
                ),
                ("/computeMetadata/v1/instance/attributes/a", 200, " v \n"),
            ],
            "",
        )
        .await;
        let c = client(&server);
        assert_eq!(c.project_id().await.unwrap(), "p");
        assert_eq!(c.instance_name().await.unwrap(), "n");
        assert_eq!(c.hostname().await.unwrap(), "h.example");
        assert_eq!(c.zone().await.unwrap(), "us-central1-a");
        assert_eq!(c.instance_attribute_value("a").await.unwrap(), " v \n");
    }

    #[tokio::test]
    async fn the_metadata_host_comes_from_the_environment() {
        let server = serve(&[("/computeMetadata/v1/x", 200, "v")], "").await;
        let host = server.url.trim_start_matches("http://");
        let c = MetadataClient::new(env(&[(METADATA_HOST_ENV, host)]));
        assert_eq!(c.get("x").await.unwrap(), "v");
    }

    #[tokio::test]
    async fn a_transport_error_is_gos_url_error_shape() {
        let url = unreachable_url().await;
        let err = MetadataClient::with_base_url(env(&[]), &url)
            .get("x")
            .await
            .unwrap_err();
        let want = format!("Get \"{url}/computeMetadata/v1/x\": ");
        assert!(err.to_string().starts_with(&want), "{err}");
    }

    #[tokio::test]
    async fn on_gce_is_true_when_the_metadata_host_is_set() {
        let c = MetadataClient::with_base_url(
            env(&[(METADATA_HOST_ENV, "169.254.169.254")]),
            unreachable_url().await,
        );
        assert!(c.on_gce().await);
    }

    #[tokio::test]
    async fn on_gce_probes_for_the_metadata_flavor_header() {
        assert!(client(&serve(&[], "Google").await).on_gce().await);
        assert!(!client(&serve(&[], "").await).on_gce().await);
        let c =
            MetadataClient::with_base_url(env(&[(METADATA_HOST_ENV, "")]), unreachable_url().await);
        let started = Instant::now();
        assert!(!c.on_gce().await);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_backoff_is_bounded_and_doubles() {
        let mut r = Retryer::new();
        for i in 0..MAX_RETRY_ATTEMPTS {
            let cap = (BACKOFF_INITIAL * 2u32.pow(i)).min(BACKOFF_MAX);
            let pause = r.retry(true).unwrap();
            assert!(
                pause >= Duration::from_nanos(1) && pause <= cap,
                "{pause:?}"
            );
        }
        assert_eq!(r.retry(true), None);
        assert_eq!(Retryer::new().retry(false), None);
    }
}
