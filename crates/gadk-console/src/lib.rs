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

//! A local web console for gadk: stored traces from Cloud Trace and Cloud Logging, the prompt
//! registry, and the reports of gadk-eval-monitor monitors, experiments and rollout guards.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_eval_monitor::{BigQueryTranscriptSource, ExportSource, MonitorError, ReportStore};

use crate::datasets::Datasets;
use crate::evals::{CACHE_CAP, ReportCache};
use crate::experiments::ExperimentCache;
use crate::guards::GuardCache;
use crate::rubrics::Rubrics;
use crate::traces::{BigQueryTable, BigQueryTraceSource, CloudTraceSource, TraceSource};
use crate::usage::UsageSource;

mod cache;
mod cli;
mod datasets;
mod evals;
mod experiments;
mod guards;
mod model;
mod prices;
mod prompts;
mod review;
mod rubrics;
mod scores;
mod sessions;
pub mod traces;
mod url;
mod usage;
mod web;

pub use cli::{parse_args, parse_cli};
pub use prices::{ModelPrice, PriceTable};

/// Console settings (from the command line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleConfig {
    /// GCP project whose traces and logs are read.
    pub project: String,
    /// Prompt store URL (`file://…` or `firestore://…`); `None` disables the prompt pages.
    pub store: Option<String>,
    /// Monitor report store URL (`file://…`, a directory or `gs://…`); `None` disables the Evals,
    /// Experiments and Guards pages.
    pub reports: Option<String>,
    /// Loopback port; 0 picks a free one.
    pub port: u16,
    /// Cloud Logging log id that gadk-telemetry writes (default `gadk`).
    pub log_id: String,
}

/// The default `--bigquery-max-bytes`: 1 GiB.
pub(crate) const DEFAULT_MAX_BYTES: i64 = 1_073_741_824;

/// The parsed command line: the console settings and where traces come from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cli {
    /// The console settings.
    pub config: ConsoleConfig,
    /// The trace source; `None` reads Cloud Trace and Cloud Logging.
    pub traces: Option<TracesFrom>,
    /// The price file (`--prices`); `None` shows no costs.
    pub prices: Option<PathBuf>,
    /// The author of scores added in the console (`--author`); `None` uses `USER`, else
    /// `USERNAME`, else `console`.
    pub author: Option<String>,
    /// The directory of eval-set files that traces are added to (`--datasets`); `None` shows
    /// no "Add to dataset" form.
    pub datasets: Option<PathBuf>,
    /// The rubric file (`--rubrics`); `None` offers no rubric forms or rubric queues.
    pub rubrics: Option<PathBuf>,
}

impl Cli {
    /// `config` with the default trace source (Cloud Trace + Cloud Logging).
    pub fn new(config: ConsoleConfig) -> Self {
        Self {
            config,
            traces: None,
            prices: None,
            author: None,
            datasets: None,
            rubrics: None,
        }
    }

    /// Reads traces from `traces` instead.
    pub fn with_traces(mut self, traces: TracesFrom) -> Self {
        self.traces = Some(traces);
        self
    }

    /// Prices token counts with the price file at `path` (see [`PriceTable`]).
    pub fn with_prices(mut self, path: PathBuf) -> Self {
        self.prices = Some(path);
        self
    }

    /// Writes `author` as the author of the scores added on `/traces/{id}`.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Adds traces as eval cases to the eval-set files in `dir` (BigQuery trace source only).
    pub fn with_datasets(mut self, dir: PathBuf) -> Self {
        self.datasets = Some(dir);
        self
    }

    /// Offers the review rubrics of the YAML file at `path` on `/review` and `/traces/{id}`
    /// (BigQuery trace source only).
    pub fn with_rubrics(mut self, path: PathBuf) -> Self {
        self.rubrics = Some(path);
        self
    }
}

/// A trace source other than Cloud Trace + Cloud Logging.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TracesFrom {
    /// The agent-analytics BigQuery table.
    #[non_exhaustive]
    BigQuery {
        /// The table read.
        table: BigQueryTable,
        /// The byte cap of every query.
        max_bytes_billed: i64,
    },
}

impl TracesFrom {
    /// Traces from the agent-analytics table `table`, each query capped at `max_bytes_billed`.
    pub fn bigquery(table: BigQueryTable, max_bytes_billed: i64) -> Self {
        Self::BigQuery {
            table,
            max_bytes_billed,
        }
    }
}

/// Startup failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsoleError {
    #[error("prompt store: {0}")]
    /// The prompt store could not be opened.
    Store(#[from] gadk_prompts::StoreError),
    #[error("report store: {0}")]
    /// The report store could not be opened.
    Reports(String),
    /// The loopback port could not be bound.
    #[error("bind 127.0.0.1:{port}: {detail}")]
    Bind {
        /// The requested port.
        port: u16,
        /// The bind error text.
        detail: String,
    },
    #[error("serve: {0}")]
    /// The server failed while serving or printing its URL.
    Serve(String),
    /// The price file could not be read or parsed.
    #[error("price file {}: {detail}", path.display())]
    Prices {
        /// The price file.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },
    /// The `--datasets` directory does not exist, or its export source could not be built.
    #[error("datasets: {0}")]
    Datasets(String),
    /// The rubric file could not be read or parsed.
    #[error("rubric file {}: {detail}", path.display())]
    Rubrics {
        /// The rubric file.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },
}

/// A console bound to its port, not yet serving.
pub struct Bound {
    listener: tokio::net::TcpListener,
    addr: SocketAddr,
    router: axum::Router,
}

impl Bound {
    /// The bound address (`127.0.0.1:<port>`), the real port when the config said 0.
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

/// Opens the prompt store and the report store (if any, once each for the Evals, Experiments and
/// Guards pages), binds `127.0.0.1:<port>`, builds the router.
///
/// A `gs://` report store needs Application Default Credentials here; the trace source builds its
/// credentials on the first GCP call.
pub async fn bind(cfg: ConsoleConfig) -> Result<Bound, ConsoleError> {
    let source = Arc::new(CloudTraceSource::new(
        cfg.project.clone(),
        cfg.log_id.clone(),
    ));
    bind_with_source(cfg, source).await
}

/// Like [`bind`] with an explicit trace source.
pub async fn bind_with_source(
    cfg: ConsoleConfig,
    source: Arc<dyn TraceSource>,
) -> Result<Bound, ConsoleError> {
    bind_full(cfg, source, Extras::default()).await
}

/// What [`run_cli`] adds to a bound console besides the trace source.
#[derive(Default)]
pub(crate) struct Extras {
    /// The loaded price file.
    pub(crate) prices: Option<Arc<PriceTable>>,
    /// The usage queries (BigQuery trace source only).
    pub(crate) usage: Option<Arc<UsageSource>>,
    /// The author of console scores; `None` is [`default_author`] over the environment.
    pub(crate) author: Option<String>,
    /// The `--datasets` directory and its export source (BigQuery trace source only).
    pub(crate) datasets: Option<Arc<Datasets>>,
    /// The export source of the BigQuery trace source (see [`export_source`]).
    pub(crate) export: Option<Result<Arc<dyn ExportSource>, String>>,
    /// The loaded rubric file.
    pub(crate) rubrics: Option<Arc<Rubrics>>,
}

/// The export source that reads conversation text from the agent-analytics table: `None`
/// without the BigQuery trace source, else the [`BigQueryTranscriptSource`] of its table, billed
/// to `billing_project` under its byte cap, or the text of the error that kept it from being
/// built (it loads Application Default Credentials eagerly).
fn export_source(
    traces: Option<&TracesFrom>,
    billing_project: &str,
) -> Option<Result<Arc<dyn ExportSource>, String>> {
    let Some(TracesFrom::BigQuery {
        table,
        max_bytes_billed,
    }) = traces
    else {
        return None;
    };
    Some(
        BigQueryTranscriptSource::new(&table.project, &table.dataset, &table.table)
            .map(|source| {
                Arc::new(
                    source
                        .with_billing_project(billing_project)
                        .with_max_bytes_billed(*max_bytes_billed),
                ) as Arc<dyn ExportSource>
            })
            .map_err(|e| e.to_string()),
    )
}

/// The author of console scores without `--author`: the first non-empty of the `USER` and
/// `USERNAME` variables (read with `lookup`), else `console`.
pub(crate) fn default_author(lookup: impl Fn(&str) -> Option<String>) -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(lookup)
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "console".to_owned())
}

/// The author of console scores: `given` (`--author`) unless it is absent or empty after
/// trimming, else [`default_author`] over `lookup`.
pub(crate) fn resolve_author(
    given: Option<String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> String {
    given
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| default_author(lookup))
}

/// Reads an environment variable (`None` when unset or not Unicode).
fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// [`bind_with_source`] plus `extras`.
pub(crate) async fn bind_full(
    cfg: ConsoleConfig,
    source: Arc<dyn TraceSource>,
    extras: Extras,
) -> Result<Bound, ConsoleError> {
    let store = cfg
        .store
        .as_deref()
        .map(gadk_prompts_cli::open_store)
        .transpose()?;
    // `ReportStore` is not `Clone`: the Evals, Experiments and Guards caches each open the URL.
    let open_reports = |url: &str| {
        ReportStore::open(url).map_err(|e| {
            ConsoleError::Reports(match e {
                MonitorError::Store(text) => text,
                other => other.to_string(),
            })
        })
    };
    let (reports, experiments, guards) = match cfg.reports.as_deref() {
        Some(url) => (
            Some(Arc::new(ReportCache::new(open_reports(url)?, CACHE_CAP))),
            Some(Arc::new(ExperimentCache::new(
                open_reports(url)?,
                CACHE_CAP,
            ))),
            Some(Arc::new(GuardCache::new(open_reports(url)?, CACHE_CAP))),
        ),
        None => (None, None, None),
    };
    let bind_err = |e: std::io::Error| ConsoleError::Bind {
        port: cfg.port,
        detail: e.to_string(),
    };
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, cfg.port))
        .await
        .map_err(bind_err)?;
    let addr = listener.local_addr().map_err(bind_err)?;
    let state = web::AppState {
        source,
        store,
        reports,
        experiments,
        guards,
        csrf: Arc::from(csrf_token()),
        port: addr.port(),
        now: chrono::Utc::now,
        prices: extras.prices,
        usage: extras.usage,
        author: resolve_author(extras.author, env_var),
        datasets: extras.datasets,
        export: extras.export,
        rubrics: extras.rubrics,
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
    run_cli(Cli::new(cfg), out).await
}

/// Like [`run`], reading traces from the source `cli.traces` names, prices from `cli.prices`
/// and rubrics from `cli.rubrics` (both read before anything is bound); console scores are
/// written by `cli.author`, else `USER`, else `USERNAME`, else `console`. `cli.datasets` must be
/// an existing directory.
pub async fn run_cli(cli: Cli, out: &mut (dyn std::io::Write + Send)) -> Result<(), ConsoleError> {
    let author = resolve_author(cli.author, env_var);
    let prices = cli
        .prices
        .as_deref()
        .map(PriceTable::load)
        .transpose()?
        .map(Arc::new);
    let rubrics = cli
        .rubrics
        .as_deref()
        .map(Rubrics::load)
        .transpose()?
        .map(Arc::new);
    if let Some(dir) = &cli.datasets {
        check_datasets_dir(dir)?;
    }
    let export = export_source(cli.traces.as_ref(), &cli.config.project);
    let datasets = match (&cli.datasets, &export) {
        (Some(dir), Some(source)) => {
            let source = source.clone().map_err(ConsoleError::Datasets)?;
            Some(Arc::new(Datasets::new(dir.clone(), source)))
        }
        _ => None,
    };
    let (source, usage): (Arc<dyn TraceSource>, Option<Arc<UsageSource>>) = match cli.traces {
        None => (
            Arc::new(CloudTraceSource::new(
                cli.config.project.clone(),
                cli.config.log_id.clone(),
            )),
            None,
        ),
        Some(TracesFrom::BigQuery {
            table,
            max_bytes_billed,
        }) => (
            Arc::new(BigQueryTraceSource::new(
                cli.config.project.clone(),
                table.clone(),
                max_bytes_billed,
            )),
            Some(Arc::new(UsageSource::new(
                cli.config.project.clone(),
                table,
                max_bytes_billed,
            ))),
        ),
    };
    let bound = bind_full(
        cli.config,
        source,
        Extras {
            prices,
            usage,
            author: Some(author),
            datasets,
            export,
            rubrics,
        },
    )
    .await?;
    writeln!(out, "http://127.0.0.1:{}/", bound.local_addr().port())
        .and_then(|()| out.flush())
        .map_err(|e| ConsoleError::Serve(format!("print the URL: {e}")))?;
    bound.serve().await
}

/// `dir` must be an existing directory.
fn check_datasets_dir(dir: &Path) -> Result<(), ConsoleError> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(ConsoleError::Datasets(format!(
            "{} is not a directory",
            dir.display()
        ))),
        Err(e) => Err(ConsoleError::Datasets(format!("{}: {e}", dir.display()))),
    }
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
            reports: None,
            port: 0,
            log_id: "gadk".into(),
        }
    }

    #[test]
    fn default_author_is_user_then_username_then_console() {
        use super::default_author;
        let env = |user: Option<&'static str>, username: Option<&'static str>| {
            move |key: &str| match key {
                "USER" => user.map(str::to_owned),
                "USERNAME" => username.map(str::to_owned),
                _ => None,
            }
        };
        assert_eq!(default_author(env(Some("chris"), Some("other"))), "chris");
        assert_eq!(default_author(env(None, Some("olson"))), "olson");
        assert_eq!(default_author(env(Some(""), Some("olson"))), "olson");
        assert_eq!(default_author(env(Some(" "), Some(""))), "console");
        assert_eq!(default_author(env(None, None)), "console");
    }

    #[test]
    fn a_blank_author_falls_back_like_an_absent_one() {
        use super::resolve_author;
        let env = |key: &str| (key == "USER").then(|| "chris".to_owned());
        assert_eq!(resolve_author(None, env), "chris");
        assert_eq!(resolve_author(Some(String::new()), env), "chris");
        assert_eq!(resolve_author(Some(" \t".into()), env), "chris");
        assert_eq!(resolve_author(Some("olson".into()), env), "olson");
        assert_eq!(resolve_author(Some(" ".into()), |_: &str| None), "console");
        assert_eq!(
            super::Cli::new(cfg(None))
                .with_author(" ")
                .author
                .as_deref(),
            Some(" "),
            "with_author stores what it is given"
        );
    }

    #[tokio::test]
    async fn run_cli_fails_on_a_bad_price_file_before_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prices.yaml");
        std::fs::write(&path, "- model: m\n  input: -1\n  output: 1\n").unwrap();
        let mut out = Vec::new();
        let err = super::run_cli(
            super::Cli::new(cfg(None)).with_prices(path.clone()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConsoleError::Prices { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "price file {}: model m: prices must be finite and not negative",
                path.display()
            )
        );
        assert!(out.is_empty(), "no URL is printed");
    }

    #[tokio::test]
    async fn run_cli_fails_on_a_bad_rubric_file_before_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rubrics.yaml");
        std::fs::write(&path, "- name: support\n").unwrap();
        let mut out = Vec::new();
        let err = super::run_cli(
            super::Cli::new(cfg(None)).with_rubrics(path.clone()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConsoleError::Rubrics { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "rubric file {}: rubrics[1].criteria must be a list of 1 to 10 criteria",
                path.display()
            )
        );
        assert!(out.is_empty(), "no URL is printed");
    }

    #[tokio::test]
    async fn run_cli_fails_on_a_missing_datasets_directory_before_binding() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.yaml");
        std::fs::write(&file, "x").unwrap();
        for (path, detail) in [
            (dir.path().join("missing"), ""),
            (file, "is not a directory"),
        ] {
            let mut out = Vec::new();
            let err = super::run_cli(
                super::Cli::new(cfg(None)).with_datasets(path.clone()),
                &mut out,
            )
            .await
            .unwrap_err();
            assert!(matches!(err, ConsoleError::Datasets(_)), "{err}");
            let text = err.to_string();
            assert!(
                text.starts_with(&format!("datasets: {}", path.display())) && text.contains(detail),
                "{text}"
            );
            assert!(out.is_empty(), "no URL is printed");
        }
    }

    #[tokio::test]
    async fn the_export_source_is_none_with_cloud_trace() {
        assert!(super::export_source(None, "billing-proj").is_none());
    }

    #[tokio::test]
    async fn the_export_source_is_built_for_the_bigquery_source() {
        let traces = super::TracesFrom::bigquery(
            crate::traces::BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        );
        // Ok with Application Default Credentials, the build error without: never None.
        match super::export_source(Some(&traces), "billing-proj") {
            Some(Ok(_)) => {}
            Some(Err(text)) => assert!(!text.is_empty()),
            None => panic!("the BigQuery source has an export source"),
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

    #[tokio::test]
    async fn bad_reports_url_is_a_reports_error() {
        let mut c = cfg(None);
        c.reports = Some("s3://x".into());
        let err = bind(c).await.err().unwrap();
        assert!(matches!(err, ConsoleError::Reports(_)), "{err}");
        assert!(
            err.to_string()
                .starts_with("report store: unsupported store URL scheme \"s3\""),
            "{err}"
        );
    }

    #[tokio::test]
    async fn file_reports_url_binds() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(None);
        c.reports = Some(format!("file://{}", dir.path().display()));
        assert!(bind(c).await.is_ok());
    }

    #[tokio::test]
    async fn a_reports_url_serves_the_experiments_page() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(None);
        c.reports = Some(format!("file://{}", dir.path().display()));
        let bound = bind(c).await.unwrap();
        let port = bound.local_addr().port();
        let server = tokio::spawn(bound.serve());
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req = format!(
            "GET /experiments HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        let resp = String::from_utf8_lossy(&out);
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.contains("No experiments yet"), "{resp}");
        server.abort();
    }

    #[tokio::test]
    async fn a_reports_url_serves_the_guards_page() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(None);
        c.reports = Some(format!("file://{}", dir.path().display()));
        let bound = bind(c).await.unwrap();
        let port = bound.local_addr().port();
        let server = tokio::spawn(bound.serve());
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req =
            format!("GET /guards HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        let resp = String::from_utf8_lossy(&out);
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.contains("No guard runs yet"), "{resp}");
        server.abort();
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

    #[tokio::test]
    async fn bind_with_source_serves_traces_from_the_given_source() {
        let source = std::sync::Arc::new(crate::web::testutil::FakeSource::new());
        let bound = super::bind_with_source(cfg(None), source.clone())
            .await
            .unwrap();
        let port = bound.local_addr().port();
        tokio::spawn(bound.serve());
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req =
            format!("GET /traces HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        let head = String::from_utf8_lossy(&out);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(
            source.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the page asked the given source"
        );
    }
}
